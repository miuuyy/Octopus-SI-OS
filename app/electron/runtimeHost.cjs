const { EventEmitter } = require("node:events");
const fs = require("node:fs/promises");
const net = require("node:net");
const path = require("node:path");
const { blockedStorageHealth, nativeStorageFailure, readHomeStorageHealth } = require("./storageHealth.cjs");

const RUNTIME_CONTROL_STATUS_TIMEOUT_MS = 10_000;

const RUNTIME_INSTANCE_FIELDS = [
  "launch_owner",
  "phase",
  "pid",
  "schema_version",
  "started_at_unix_ms",
  "updated_at_unix_ms",
];

function createRuntimeHost(options) {
  return new RuntimeHost(options);
}

class RuntimeHost extends EventEmitter {
  constructor(options) {
    super();
    this.options = options;
    this.timer = null;
    this.probePromise = null;
    this.storageProbePromise = null;
    this.storageProbeAtMs = 0;
    this.storageHome = "";
    this.storageRevision = 0;
    this.state = {
      phase: "probing",
      ready: false,
      ownership: "none",
      pid: null,
      home: options.home(),
      sourceRegistry: options.sourceRegistry(),
      runtimeBinary: options.runtimeBinary(),
      repoRoot: options.repoRoot?.() ?? "",
      launchCwd: options.launchCwd?.() ?? options.repoRoot?.() ?? "",
      launchCommand: options.launchCommand?.() ?? "",
      launchEnv: stringMap(options.launchEnv?.() ?? {}, "runtime launch environment"),
      endpoint: options.endpoint(),
      error: null,
      storageHealth: null,
      interfaceFailures: [],
      updatedAtMs: Date.now(),
    };
  }

  snapshot() {
    return { ...this.state, launchEnv: { ...this.state.launchEnv }, storageHealth: this.state.storageHealth ? { ...this.state.storageHealth } : null,
      interfaceFailures: this.state.interfaceFailures.map((failure) => ({ ...failure })) };
  }

  async refreshStorageHealth(force = false) {
    const home = absolutePath(this.options.home(), "runtime home");
    if (this.storageProbePromise) {
      await this.storageProbePromise;
      if (home !== this.options.home()) return this.refreshStorageHealth(force);
    }
    if (!force && this.storageHome === home && Date.now() < this.storageProbeAtMs) return this.state.storageHealth;
    const revision = this.storageRevision;
    this.storageProbePromise = readHomeStorageHealth(home).then((storageHealth) => {
      if (home !== this.options.home()) return null;
      if (revision !== this.storageRevision) return this.state.storageHealth;
      this.storageHome = home;
      this.storageProbeAtMs = Date.now() + 5_000;
      this.transition(this.state.phase, { home, storageHealth });
      return storageHealth;
    }).finally(() => { this.storageProbePromise = null; });
    return this.storageProbePromise;
  }

  recordStorageFailure(error) {
    if (!nativeStorageFailure(error)) return;
    this.storageRevision += 1;
    this.storageHome = this.options.home();
    this.storageProbeAtMs = Date.now() + 5_000;
    this.transition(this.state.phase, { home: this.storageHome,
      storageHealth: blockedStorageHealth(error.code, this.state.storageHealth?.availableBytes ?? null) });
  }

  async assertStorageWritable() {
    const health = await this.refreshStorageHealth(true);
    if (health?.status === "writable") return;
    throw Object.assign(new Error(health?.message ?? "Home storage has not been checked. Your message has not been sent; your draft is retained."), {
      code: "home_storage_blocked", deliveryState: "not_sent",
    });
  }

  observe(intervalMs = 350) {
    if (this.timer) return;
    void this.probe();
    this.timer = setInterval(() => void this.probe(), intervalMs);
  }

  close() {
    if (this.timer) clearInterval(this.timer);
    this.timer = null;
  }

  async waitUntilStopped(timeoutMs = 8_000) {
    const deadline = Date.now() + timeoutMs;
    while (Date.now() < deadline) {
      await this.probe();
      if (
        this.state.phase === "unmanaged"
        && !this.state.ready
        && this.state.ownership === "none"
        && this.state.pid == null
        && !this.state.error
      ) return true;
      await delay(100);
    }
    return false;
  }

  async waitUntilReady(timeoutMs = 300_000) {
    const deadline = Date.now() + timeoutMs;
    let observedRuntimePid = false;
    while (Date.now() < deadline) {
      await this.probe();
      if (this.state.ready) return this.snapshot();
      if (this.state.pid != null) observedRuntimePid = true;
      if (
        observedRuntimePid
        && this.state.phase === "unmanaged"
        && this.state.ownership === "none"
        && this.state.pid == null
        && !this.state.error
      ) {
        throw new Error("Octopus runtime exited before becoming ready");
      }
      await delay(100);
    }
    throw new Error(`Octopus runtime did not become ready within ${Math.round(timeoutMs / 1000)} seconds after terminal submission`);
  }

  probe() {
    if (!this.probePromise) {
      this.probePromise = this.probeInner()
        .catch((error) => {
          this.transition("degraded", {
            ready: false,
            ownership: "none",
            pid: null,
            error: formatError(error),
          });
          return this.snapshot();
        })
        .finally(() => {
          this.probePromise = null;
        });
    }
    return this.probePromise;
  }

  async probeInner() {
    const home = absolutePath(this.options.home(), "runtime home");
    const sourceRegistry = absolutePath(this.options.sourceRegistry(), "source registry");
    const runtimeBinary = absolutePath(this.options.runtimeBinary(), "runtime binary");
    const endpoint = absolutePath(this.options.endpoint(), "runtime endpoint");
    requireUnixSocketPath(endpoint, "runtime endpoint");
    requireUnixSocketPath(
      path.join(home, "app-server-control", `${"0".repeat(24)}.sock`),
      "per-interface artifact socket",
    );
    const base = {
      home,
      sourceRegistry,
      runtimeBinary,
      repoRoot: this.options.repoRoot?.() ?? "",
      launchCwd: absolutePath(
        this.options.launchCwd?.() ?? this.options.repoRoot?.() ?? path.dirname(runtimeBinary),
        "runtime launch directory",
      ),
      launchCommand: requiredString(this.options.launchCommand?.(), "runtime launch command"),
      launchEnv: stringMap(this.options.launchEnv?.() ?? {}, "runtime launch environment"),
      endpoint,
    };

    await this.refreshStorageHealth();

    const instance = await readRuntimeInstance(home);
    if (instance && processIsAlive(instance.pid)) {
      const status = await readRuntimeControlStatus(home);
      if (status.pid !== instance.pid || status.started_at_unix_ms !== instance.started_at_unix_ms) {
        throw new Error("runtime control status does not match the live runtime instance");
      }
      const ready = status.phase === "ready" && status.stopping === null;
      this.transition(ready ? "ready" : status.stopping ? "stopping" : normalizeRuntimePhase(status.phase), {
        ...base,
        ready,
        ownership: "external",
        pid: instance.pid,
        error: null,
        interfaceFailures: status.interface_failures.map((failure) => ({ interfaceId: failure.interface, message: failure.error })),
      });
      return this.snapshot();
    }

    if (await socketAccepts(endpoint, 200)) {
      this.transition("degraded", {
        ...base,
        ready: false,
        ownership: "external",
        pid: null,
        error: "runtime endpoint accepts connections without coherent live runtime-instance proof",
        interfaceFailures: [],
      });
      return this.snapshot();
    }

    this.transition("unmanaged", {
      ...base,
      ready: false,
      ownership: "none",
      pid: null,
      error: null,
      interfaceFailures: [],
    });
    return this.snapshot();
  }

  transition(phase, patch = {}) {
    const next = {
      ...this.state,
      ...patch,
      phase,
      updatedAtMs: Date.now(),
    };
    if (sameState(this.state, next)) return;
    this.state = next;
    this.emit("state", this.snapshot());
  }
}

function readRuntimeControlStatus(home) {
  const socketPath = path.join(home, "state", "runtime-instance", "control.sock");
  return new Promise((resolve, reject) => {
    const socket = net.createConnection(socketPath);
    let bytes = Buffer.alloc(0), settled = false;
    const finish = (error, status) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      socket.destroy();
      if (error) reject(error); else resolve(status);
    };
    const timer = setTimeout(
      () => finish(new Error(`runtime control status did not answer within ${RUNTIME_CONTROL_STATUS_TIMEOUT_MS}ms`)),
      RUNTIME_CONTROL_STATUS_TIMEOUT_MS,
    );
    socket.once("connect", () => socket.write('{"request":"status"}\n'));
    socket.once("error", (error) => finish(error));
    socket.once("end", () => finish(new Error("runtime control status ended without a complete frame")));
    socket.on("data", (chunk) => {
      bytes = Buffer.concat([bytes, chunk]);
      if (bytes.length > 1024 * 1024) { finish(new Error("runtime control status exceeds 1 MiB")); return; }
      const newline = bytes.indexOf(10);
      if (newline < 0) return;
      try {
        if (bytes.subarray(newline + 1).toString("utf8").trim()) throw new Error("runtime control status contains trailing data");
        finish(null, parseRuntimeControlStatus(JSON.parse(bytes.subarray(0, newline).toString("utf8"))));
      } catch (error) { finish(error); }
    });
  });
}

function parseRuntimeControlStatus(value) {
  if (value?.outcome === "refused") {
    if (Object.keys(value).sort().join(",") !== "error,outcome" || typeof value.error !== "string" || !value.error.trim()) {
      throw new Error("runtime control refusal is malformed");
    }
    throw new Error(`runtime refused the control status request: ${value.error}`);
  }
  const fields = ["interface_failures", "outcome", "phase", "pid", "started_at_unix_ms", "stopping"];
  const keys = value && typeof value === "object" && !Array.isArray(value) ? Object.keys(value).sort() : [];
  if (keys.length !== fields.length || keys.some((key, index) => key !== fields[index])
    || value.outcome !== "status" || !Number.isSafeInteger(value.pid) || value.pid <= 0 || value.pid > 0xffff_ffff
    || typeof value.phase !== "string" || !/^[a-z_]+$/.test(value.phase)
    || !Number.isSafeInteger(value.started_at_unix_ms) || value.started_at_unix_ms <= 0
    || ![null, "stop", "restart"].includes(value.stopping) || !Array.isArray(value.interface_failures)) {
    throw new Error("runtime control status is malformed; interface_failures is required");
  }
  const interfaces = new Set();
  for (const failure of value.interface_failures) {
    if (!failure || typeof failure !== "object" || Array.isArray(failure)
      || Object.keys(failure).sort().join(",") !== "error,interface"
      || typeof failure.interface !== "string" || !failure.interface.trim()
      || typeof failure.error !== "string" || !failure.error.trim() || interfaces.has(failure.interface)) {
      throw new Error("runtime control interface failure is malformed");
    }
    interfaces.add(failure.interface);
  }
  return value;
}

async function readRuntimeInstance(home) {
  const file = path.join(home, "state", "runtime-instance", "state.json");
  try {
    const value = JSON.parse(await fs.readFile(file, "utf8"));
    const keys = value && typeof value === "object" && !Array.isArray(value)
      ? Object.keys(value).sort()
      : [];
    if (
      keys.length !== RUNTIME_INSTANCE_FIELDS.length
      || keys.some((key, index) => key !== RUNTIME_INSTANCE_FIELDS[index])
      || value.schema_version !== 3
      || !Number.isSafeInteger(value.pid)
      || value.pid <= 0
      || value.pid > 0xffff_ffff
      || typeof value.phase !== "string"
      || !/^[a-z_]+$/.test(value.phase)
      || !Number.isSafeInteger(value.started_at_unix_ms)
      || value.started_at_unix_ms <= 0
      || !Number.isSafeInteger(value.updated_at_unix_ms)
      || value.updated_at_unix_ms < value.started_at_unix_ms
      || value.launch_owner !== null
    ) {
      throw new Error(`runtime instance file '${file}' is malformed`);
    }
    return value;
  } catch (error) {
    if (error?.code === "ENOENT") return null;
    throw error;
  }
}

function processIsAlive(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    if (error?.code === "ESRCH") return false;
    if (error?.code === "EPERM") return true;
    throw error;
  }
}

function normalizeRuntimePhase(phase) {
  const value = typeof phase === "string" ? phase : "starting_runtime";
  if (value === "installing_registry") return "registry_install";
  if (value === "registry_ready") return "registry_ready";
  if (value === "state_ready") return "state_ready";
  return "starting_runtime";
}

function socketAccepts(socketPath, timeoutMs) {
  return new Promise((resolve) => {
    let settled = false;
    const socket = net.createConnection(socketPath);
    const finish = (value) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      socket.destroy();
      resolve(value);
    };
    const timer = setTimeout(() => finish(false), timeoutMs);
    socket.once("connect", () => finish(true));
    socket.once("error", () => finish(false));
  });
}

function absolutePath(value, field) {
  if (typeof value !== "string" || !value.trim() || !path.isAbsolute(value)) {
    throw new Error(`${field} must be an absolute path`);
  }
  return path.normalize(value);
}

function requiredString(value, field) {
  if (typeof value !== "string" || !value.trim()) throw new Error(`${field} must be a non-empty string`);
  return value;
}

function stringMap(value, field) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`${field} must be an object`);
  }
  const result = {};
  for (const [key, entry] of Object.entries(value)) {
    if (!key || typeof entry !== "string") throw new Error(`${field} must contain string values`);
    result[key] = entry;
  }
  return result;
}

function requireUnixSocketPath(value, field) {
  if (process.platform === "win32") return;
  const maximum = process.platform === "darwin" ? 103 : 107;
  const bytes = Buffer.byteLength(value);
  if (bytes > maximum) {
    throw new Error(`${field} is ${bytes} bytes but ${process.platform} allows at most ${maximum}; choose a shorter OCTOPUS_HOME`);
  }
}

function formatError(error) {
  return error instanceof Error ? error.message : String(error);
}

function sameState(left, right) {
  return Object.keys(right).every((key) => {
    if (key === "updatedAtMs") return true;
    const previous = left[key];
    const next = right[key];
    if (previous && next && typeof previous === "object" && typeof next === "object") {
      return JSON.stringify(previous) === JSON.stringify(next);
    }
    return previous === next;
  });
}

function delay(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

module.exports = { createRuntimeHost };
