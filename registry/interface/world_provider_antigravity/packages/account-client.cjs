const fs = require("node:fs/promises");
const { constants } = require("node:fs");
const path = require("node:path");
const { spawn } = require("node:child_process");

const TIMEOUTS = { status: 20_000, login: 600_000, logout: 20_000 };
const ID = /^[A-Za-z0-9_-]+$/;

function record(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function text(value, limit) {
  return typeof value === "string" && value.length > 0 && value.length <= limit
    && value.trim() === value && !/[\u0000-\u001f\u007f]/u.test(value);
}

function exact(value, keys) {
  return record(value) && Object.keys(value).length === keys.length && keys.every(key => Object.hasOwn(value, key));
}

function absolute(value, label) {
  if (!text(value, 4096) || !path.isAbsolute(value)) throw new Error(`Antigravity requires an explicit absolute ${label}.`);
  return path.resolve(value);
}

function checkCancelled(signal) {
  if (signal?.aborted) throw new Error("Antigravity account operation was cancelled.");
}

async function target(ctx) {
  checkCancelled(ctx.signal);
  const descriptor = ctx.descriptor;
  const config = descriptor?.interface_driver_config?.private_config;
  const account = config?.opencodex;
  if (typeof descriptor?.id !== "string" || !ID.test(descriptor.id) || descriptor.interface_type !== "world" || descriptor.role !== "provider"
    || account?.schema_version !== 1 || account.oauth_provider !== "google-antigravity"
    || typeof account.credential_owner_interface !== "string" || !ID.test(account.credential_owner_interface)) {
    throw new Error("Antigravity requires its declared OAuth provider and credential owner.");
  }
  const parts = typeof config.account_bridge === "string"
    && config.account_bridge.match(/^interface:\/\/([A-Za-z0-9_-]+)\/(packages\/.+)$/);
  if (!parts || parts[2].split("/").some(part => !/^[A-Za-z0-9_.-]+$/.test(part) || part === "." || part === "..")) {
    throw new Error("Antigravity account bridge must be a declared interface package executable.");
  }
  const registry = await fs.realpath(path.resolve(__dirname, "../../.."));
  const binary = path.join(registry, "interface", parts[1], parts[2]);
  if (await fs.realpath(binary) !== binary || !(await fs.stat(binary)).isFile()) {
    throw new Error("Antigravity account bridge must be a regular executable inside the selected registry.");
  }
  await fs.access(binary, constants.X_OK);
  const privateDirectory = absolute(ctx.privateDirectory, "interface private directory");
  if (ctx.interfaceId !== descriptor.id || path.basename(privateDirectory) !== descriptor.id) {
    throw new Error("Antigravity connection does not name its interface private directory.");
  }
  const ownerDirectory = path.join(path.dirname(privateDirectory), account.credential_owner_interface);
  const directory = path.join(ownerDirectory, "opencodex-account");
  for (const candidate of [ownerDirectory, directory]) {
    const stat = await fs.lstat(candidate).catch(error => {
      if (error.code === "ENOENT") return null;
      throw error;
    });
    if (stat && !stat.isDirectory()) throw new Error("Antigravity account storage must use owned directories without symlinks.");
  }
  checkCancelled(ctx.signal);
  return { binary, account: { provider: account.oauth_provider, directory } };
}

function authorizationUrl(value) {
  let url;
  try { url = text(value, 2048) ? new URL(value) : null; } catch { url = null; }
  if (!url || url.protocol !== "https:" || url.hostname !== "accounts.google.com"
    || url.port || url.username || url.password || url.hash) {
    throw new Error("Antigravity returned an invalid Google authorization URL.");
  }
  return url.href;
}

function invoke(target, operation, ctx, authorize) {
  checkCancelled(ctx.signal);
  return new Promise((resolve, reject) => {
    const child = spawn(target.binary, ["__account"], {
      cwd: path.dirname(target.binary), env: {}, stdio: ["pipe", "pipe", "pipe"], windowsHide: true,
    });
    const decoder = new TextDecoder("utf-8", { fatal: true });
    let buffer = "", bytes = 0, stderrBytes = 0, result, received = false, authReceived = false;
    let exited = false, closed = false, settled = false, failure = null, pending = 0, callbacks = Promise.resolve();
    let killTimer, cleanupTimer;
    const timer = setTimeout(() => fail(new Error(`Antigravity account ${operation} timed out.`)), TIMEOUTS[operation]);
    const abort = () => fail(new Error("Antigravity account operation was cancelled."));

    function settle(error) {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      clearTimeout(killTimer);
      clearTimeout(cleanupTimer);
      ctx.signal?.removeEventListener("abort", abort);
      if (error) reject(error); else resolve(result);
    }

    function finish() {
      if (!closed || settled) return;
      if (failure) return settle(failure);
      if (pending) return;
      settle(exited && received ? null : new Error("Antigravity account bridge closed without a result and successful exit."));
    }

    function fail(error) {
      if (failure || settled) return;
      failure = error;
      clearTimeout(timer);
      if (!closed) {
        if (!exited) child.kill("SIGTERM");
        child.stdin.destroy();
        killTimer = setTimeout(() => { if (!exited) child.kill("SIGKILL"); }, 1000);
        cleanupTimer = setTimeout(() => {
          child.stdout.destroy();
          child.stderr.destroy();
          settle(new Error(`${failure.message} Antigravity account bridge cleanup did not observe process and stream closure.`));
        }, 4000);
      }
      finish();
    }

    function notify(action, errorMessage) {
      pending++;
      callbacks = callbacks.then(async () => {
        if (failure || settled) return;
        checkCancelled(ctx.signal);
        await action();
      }).catch(() => fail(new Error(errorMessage))).finally(() => { pending--; finish(); });
    }

    function event(line) {
      const value = JSON.parse(line);
      if (received) throw new Error("Antigravity account bridge emitted data after its result.");
      if (exact(value, ["event", "result"]) && value.event === "result") {
        received = true;
        result = value.result;
      } else if (exact(value, ["event", "url"]) && value.event === "auth" && operation === "login" && authorize && !authReceived) {
        const url = authorizationUrl(value.url);
        authReceived = true;
        notify(() => authorize(url), "Antigravity sign-in could not open the authorization page.");
      } else if (exact(value, ["event", "message"]) && value.event === "progress" && operation === "login" && text(value.message, 300)) {
        notify(() => ctx.progress({ message: value.message }), "Antigravity sign-in could not report progress.");
      } else if (exact(value, ["event", "code", "message"]) && value.event === "error"
        && typeof value.code === "string" && /^[a-z_]+$/.test(value.code) && text(value.message, 600)) {
        fail(new Error(value.message));
      } else {
        throw new Error("Antigravity account bridge emitted an invalid event.");
      }
    }

    child.stdout.on("data", chunk => {
      if (failure || settled) return;
      bytes += chunk.length;
      if (bytes > 8 * 1024 * 1024) return fail(new Error("Antigravity account response exceeded its size limit."));
      try {
        buffer += decoder.decode(chunk, { stream: true });
        let newline;
        while ((newline = buffer.indexOf("\n")) !== -1) {
          const line = buffer.slice(0, newline);
          buffer = buffer.slice(newline + 1);
          event(line);
        }
      } catch {
        fail(new Error("Antigravity account bridge returned invalid NDJSON events."));
      }
    });
    child.stdout.on("end", () => {
      if (failure || settled) return;
      try {
        buffer += decoder.decode();
        if (buffer) throw new Error("Incomplete event");
      } catch { fail(new Error("Antigravity account bridge returned an incomplete response.")); }
    });
    child.stderr.on("data", chunk => {
      stderrBytes += chunk.length;
      if (stderrBytes > 64 * 1024) fail(new Error("Antigravity account bridge exceeded its diagnostic limit."));
    });
    child.once("error", () => fail(new Error("Antigravity account bridge could not run.")));
    child.stdin.on("error", () => fail(new Error("Antigravity account bridge could not receive its request.")));
    child.stdout.on("error", () => fail(new Error("Antigravity account bridge response could not be read.")));
    child.stderr.on("error", () => fail(new Error("Antigravity account bridge diagnostics could not be read.")));
    child.once("exit", (code, signal) => {
      exited = true;
      if (code !== 0 || signal) fail(new Error(`Antigravity account ${operation} failed in its bridge.`));
    });
    child.once("close", () => { closed = true; finish(); });
    ctx.signal?.addEventListener("abort", abort, { once: true });
    if (ctx.signal?.aborted) abort();
    if (!failure) child.stdin.end(JSON.stringify({ operation, account: target.account }) + "\n");
  });
}

function accountStatus(value) {
  if (!exact(value, ["signedIn", "accountLabel"]) || typeof value.signedIn !== "boolean"
    || (value.signedIn ? !text(value.accountLabel, 320) : value.accountLabel !== null)) {
    throw new Error("Antigravity account bridge returned an invalid account status.");
  }
  return value;
}

function connectionState(account) {
  return account.signedIn
    ? { state: "connected", account: { label: account.accountLabel.length > 96 ? `${account.accountLabel.slice(0, 95)}…` : account.accountLabel } }
    : { state: "not_connected" };
}

module.exports = {
  schemaVersion: 1,
  async probe(ctx) {
    return connectionState(accountStatus(await invoke(await target(ctx), "status", ctx)));
  },
  async connect(ctx) {
    if (typeof ctx.openExternal !== "function" || typeof ctx.progress !== "function") {
      throw new Error("Antigravity sign-in requires the client connection flow.");
    }
    const account = accountStatus(await invoke(await target(ctx), "login", ctx, url => ctx.openExternal(url)));
    if (!account.signedIn) throw new Error("Antigravity sign-in did not establish an account.");
    return connectionState(account);
  },
  async disconnect(ctx) {
    const account = accountStatus(await invoke(await target(ctx), "logout", ctx));
    if (account.signedIn) throw new Error("Antigravity account remains signed in after disconnecting.");
    return connectionState(account);
  },
};
