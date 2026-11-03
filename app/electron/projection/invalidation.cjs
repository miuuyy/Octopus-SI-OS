module.exports = { invalidationProjection };

const crypto = require("node:crypto");
const fsSync = require("node:fs");
const fs = require("node:fs/promises");
const path = require("node:path");
const { projectionTruthChange } = require("./registryRows.cjs");

function invalidationProjection(store) {
  let projectionGeneration = 1;
  let projectionStructureGeneration = 1;
  let projectionConfigurationGeneration = 1;
  let projectionBuild = null;
  let projectionBuildPending = null;
  let projectionBroadcastTimer = null;
  let projectionBroadcastDeadline = 0;
  let projectionBroadcastKind = "ledger";
  const projectionWatchers = new Map();
  const projectionLedgerWatchers = new Map();
  const projectionWatchHealth = new Map();
  let projectionFallbackTimer = null;
  let projectionFallbackFingerprint = "";
  let projectionFallbackReading = false;
  let projectionFallbackError = "";

  function projectionWatchRoots() {
    return [store.octopusHome()];
  }

  function projectionLedgerFiles() {
    return [path.join(store.octopusHome(), "ledger", "movements.jsonl")];
  }

  function ensureProjectionWatchers() {
    for (const root of projectionWatchRoots()) {
      if (projectionWatchers.has(root)) continue;
      try {
        const watcher = require("node:fs").watch(root, { recursive: true, persistent: false }, (_event, filename) => {
          const change = projectionTruthChange(filename);
          if (change === "policy") invalidateProjection(true, "structure", filename);
          else if (change === "structure") invalidateProjection(false, "structure", filename);
          else if (change === "runs") invalidateProjection(false, "runs", filename);
          else if (change === "files") invalidateProjection(false, "files", filename);
          else if (change === "ledger" || change === "state") signalLedgerProjectionChange();
        });
        watcher.on("error", (error) => {
          try { watcher.close(); } catch {}
          projectionWatchers.delete(root);
          projectionWatchHealth.set(root, String(error?.message ?? error));
          invalidateProjection();
          ensureProjectionFallbackWatchers();
          ensureProjectionFallbackPoller();
        });
        projectionWatchers.set(root, watcher);
        projectionWatchHealth.delete(root);
        invalidateProjection();
      } catch (error) {
        projectionWatchHealth.set(root, String(error?.message ?? error));
      }
    }
    if (projectionWatchers.size > 0) {
      stopProjectionFallbackPoller();
      for (const [file, listener] of [...projectionLedgerWatchers]) {
        fsSync.unwatchFile(file, listener);
        projectionLedgerWatchers.delete(file);
      }
      return;
    }
    ensureProjectionFallbackWatchers();
    ensureProjectionFallbackPoller();
  }

  function ensureProjectionFallbackWatchers() {
    for (const file of projectionLedgerFiles()) {
      if (projectionLedgerWatchers.has(file)) continue;
      const listener = (current, previous) => {
        if (
          current.dev === previous.dev
          && current.ino === previous.ino
          && current.size === previous.size
          && current.mtimeMs === previous.mtimeMs
          && current.nlink === previous.nlink
        ) return;
        signalLedgerProjectionChange();
      };
      fsSync.watchFile(file, { interval: 250, persistent: false }, listener);
      projectionLedgerWatchers.set(file, listener);
    }
  }

  function projectionWatchWarnings() {
    const warnings = [...projectionWatchHealth.entries()].map(([root, detail]) => (
      `Projection filesystem watch degraded for ${root}: ${detail}; authoritative fingerprint polling is active until the watcher recovers.`
    ));
    if (projectionFallbackError) warnings.push(projectionFallbackError);
    return warnings;
  }

  function ensureProjectionFallbackPoller() {
    if (projectionFallbackTimer) return;
    const poll = async () => {
      if (projectionFallbackReading || projectionWatchers.size > 0) return;
      projectionFallbackReading = true;
      try {
        const fingerprint = await readProjectionTruthFingerprint();
        if (projectionFallbackFingerprint && fingerprint !== projectionFallbackFingerprint) invalidateProjection();
        projectionFallbackFingerprint = fingerprint;
        if (projectionFallbackError) {
          projectionFallbackError = "";
          invalidateProjection();
        }
      } catch (error) {
        const detail = `Projection fingerprint polling failed: ${String(error?.message ?? error)}`;
        if (projectionFallbackError !== detail) {
          projectionFallbackError = detail;
          invalidateProjection();
        }
      } finally {
        projectionFallbackReading = false;
      }
    };
    void poll();
    projectionFallbackTimer = setInterval(() => void poll(), 1_000);
    projectionFallbackTimer.unref?.();
  }

  function stopProjectionFallbackPoller() {
    if (projectionFallbackTimer) clearInterval(projectionFallbackTimer);
    projectionFallbackTimer = null;
    projectionFallbackFingerprint = "";
    projectionFallbackReading = false;
    projectionFallbackError = "";
  }

  async function readProjectionTruthFingerprint() {
    const root = store.octopusHome();
    const pending = [
      path.join(root, "registry"),
      path.join(root, "state", "runs"),
      path.join(root, "state", "change-journals"),
      path.join(root, "state", "ee_instances"),
      path.join(root, "interfaces"),
      path.join(root, "app-server-control"),
      path.join(root, "client", "package-state"),
      path.join(root, "client", "agents"),
      path.join(root, "client-state", "workspace"),
    ];
    const facts = [];
    const hostManifest = path.join(root, "host.json");
    try {
      const stat = await fs.stat(hostManifest);
      facts.push(`host.json\0${stat.dev}\0${stat.ino}\0${stat.size}\0${stat.mtimeMs}`);
    } catch (error) {
      if (error?.code !== "ENOENT") throw error;
    }
    while (pending.length > 0) {
      const current = pending.pop();
      let entries;
      try {
        entries = await fs.readdir(current, { withFileTypes: true });
      } catch (error) {
        if (error?.code === "ENOENT") continue;
        throw error;
      }
      for (const entry of entries) {
        if (entry.isSymbolicLink()) continue;
        const absolute = path.join(current, entry.name);
        if (entry.isDirectory()) {
          pending.push(absolute);
          continue;
        }
        if (!entry.isFile()) continue;
        const relative = path.relative(root, absolute);
        if (!projectionTruthChange(relative)) continue;
        const stat = await fs.stat(absolute);
        facts.push(`${relative}\0${stat.dev}\0${stat.ino}\0${stat.size}\0${stat.mtimeMs}`);
      }
    }
    facts.sort();
    return crypto.createHash("sha256").update(facts.join("\n")).digest("hex");
  }

  function fireProjectionBroadcast() {
    projectionBroadcastTimer = null;
    projectionBroadcastDeadline = 0;
    const kind = projectionBroadcastKind;
    projectionBroadcastKind = "ledger";
    store.onChange(projectionGeneration, kind);
  }

  function scheduleProjectionBroadcast(kind, coalesce) {
    const priority = { ledger: 0, runs: 1, files: 2, structure: 3 };
    if (priority[kind] > priority[projectionBroadcastKind]) projectionBroadcastKind = kind;
    if (!coalesce) {
      if (projectionBroadcastTimer) return;
      projectionBroadcastTimer = setTimeout(fireProjectionBroadcast, 120);
      return;
    }
    const now = Date.now();
    if (projectionBroadcastDeadline === 0) projectionBroadcastDeadline = now + 480;
    if (projectionBroadcastTimer) clearTimeout(projectionBroadcastTimer);
    const wait = Math.max(0, Math.min(120, projectionBroadcastDeadline - now));
    projectionBroadcastTimer = setTimeout(fireProjectionBroadcast, wait);
  }

  function signalLedgerProjectionChange() {
    projectionGeneration += 1;
    scheduleProjectionBroadcast("ledger", false);
  }

  function invalidateProjection(policyChanged = true, kind = "structure", filename) {
    projectionGeneration += 1;
    projectionStructureGeneration += 1;
    if (kind !== "runs") projectionConfigurationGeneration += 1;
    store.invalidateConfigurationProjection(kind !== "runs", policyChanged, filename);
    projectionBuild = null;
    scheduleProjectionBroadcast(kind, true);
  }

  function fileProjectionScope(repoRoot) {
    return JSON.stringify([repoRoot, store.octopusHome(), store.resolveSourceRegistry(), store.resolveLocalPackageRegistry(), store.resolveLocalPackageLibrary()]);
  }

  async function cachedFileProjection(repoRoot, includeLedger) {
    ensureProjectionWatchers();
    const scope = fileProjectionScope(repoRoot);
    for (;;) {
      let build;
      if (projectionBuild && projectionBuild.structureGeneration === projectionStructureGeneration && projectionBuild.scope === scope) {
        build = projectionBuild;
      } else if (projectionBuildPending?.scope === scope) {
        build = projectionBuildPending;
      } else {
        const promise = store.buildFileProjection(repoRoot);
        build = { structureGeneration: projectionStructureGeneration, configurationGeneration: projectionConfigurationGeneration, scope, promise };
        projectionBuild = build;
        projectionBuildPending = build;
        const settled = () => {
          if (projectionBuildPending === build) projectionBuildPending = null;
        };
        void promise.then(settled, settled);
        void promise.catch(() => {
          if (projectionBuild === build) projectionBuild = null;
        });
      }
      const projection = await build.promise;
      if (scope !== fileProjectionScope(repoRoot)) throw new Error("Projection Home changed during read");
      if (build.configurationGeneration !== projectionConfigurationGeneration) continue;
      if (!includeLedger) return projection;
      return refreshLedgerProjection(projection);
    }
  }

  async function refreshLedgerProjection(projection) {
    const ledger = await store.readLedgerProjection(projection.durableRoot, projection.warnings);
    if (ledger.revision !== projection.ledgerRevision) {
      projection.ledgerRows = ledger.rows;
      projection.ledgerRowsByRunSources = ledger.rowsByRunSources;
      projection.invalidLedgerRows = ledger.invalidRows;
      projection.ledgerRevision = ledger.revision;
      projection.ledgerMemo.clear();
      projection.operatorChatRunIndexCache = null;
      projection.generatedAt = new Date().toISOString();
      if (projection.generation === projectionGeneration) signalLedgerProjectionChange();
      projection.generation = projectionGeneration;
    }
    return projection;
  }

  function closeProjectionWatchers() {
    if (projectionBroadcastTimer) clearTimeout(projectionBroadcastTimer);
    projectionBroadcastTimer = null;
    projectionBroadcastKind = "ledger";
    projectionBuild = null;
    projectionBuildPending = null;
    for (const watcher of projectionWatchers.values()) {
      try { watcher.close(); } catch {}
    }
    projectionWatchers.clear();
    projectionWatchHealth.clear();
    stopProjectionFallbackPoller();
    for (const [file, listener] of projectionLedgerWatchers) {
      fsSync.unwatchFile(file, listener);
    }
    projectionLedgerWatchers.clear();
  }

  Object.defineProperty(store, "projectionGeneration", { get: () => projectionGeneration });

  return { cachedFileProjection, closeProjectionWatchers, ensureProjectionFallbackPoller, ensureProjectionFallbackWatchers, ensureProjectionWatchers, fileProjectionScope, fireProjectionBroadcast, invalidateProjection, projectionLedgerFiles, projectionWatchRoots, projectionWatchWarnings, readProjectionTruthFingerprint, refreshLedgerProjection, scheduleProjectionBroadcast, signalLedgerProjectionChange, stopProjectionFallbackPoller };
}
