const crypto = require("node:crypto");
const fsSync = require("node:fs");
const fs = require("node:fs/promises");
const os = require("node:os");
const { Worker } = require("node:worker_threads");

const DEFAULT_FS_OPEN = fs.open;
const HASH_CHUNK_BYTES = 1024 * 1024;
const WORKER_HASH_MIN_BYTES = 64 * 1024;
const VERIFIED_INODE_DIGEST_CACHE_MAX = 16384;
const WORKER_IDLE_TIMEOUT_MS = 1000;
const WORKER_POOL_MAX = Math.min(
  8,
  Math.max(
    1,
    typeof os.availableParallelism === "function"
      ? os.availableParallelism()
      : (os.cpus?.().length || 2),
  ),
);

const HASH_WORKER_SOURCE = `
const crypto = require("node:crypto");
const fs = require("node:fs");
const { parentPort } = require("node:worker_threads");
const BUFFER = Buffer.allocUnsafe(1024 * 1024);

parentPort.on("message", (task) => {
  let fd = -1;
  try {
    fd = fs.openSync(task.path, fs.constants.O_RDONLY | fs.constants.O_NOFOLLOW);
    const before = fs.fstatSync(fd, { bigint: true });
    if (!before.isFile()) {
      parentPort.postMessage({ id: task.id, error: "Local package hashing requires a regular file" });
      return;
    }
    if (
      before.dev !== task.dev
      || before.ino !== task.ino
      || before.size !== task.size
      || before.mode !== task.mode
      || before.mtimeNs !== task.mtimeNs
      || before.ctimeNs !== task.ctimeNs
    ) {
      parentPort.postMessage({ id: task.id, changed: true });
      return;
    }
    const hash = crypto.createHash("sha256");
    let size = 0;
    for (;;) {
      const bytesRead = fs.readSync(fd, BUFFER, 0, BUFFER.length, size);
      if (bytesRead === 0) break;
      hash.update(BUFFER.subarray(0, bytesRead));
      size += bytesRead;
    }
    const after = fs.fstatSync(fd, { bigint: true });
    if (
      !after.isFile()
      || size !== Number(before.size)
      || after.dev !== before.dev
      || after.ino !== before.ino
      || after.size !== before.size
      || after.mode !== before.mode
      || after.mtimeNs !== before.mtimeNs
      || after.ctimeNs !== before.ctimeNs
    ) {
      parentPort.postMessage({ id: task.id, changed: true });
      return;
    }
    parentPort.postMessage({ id: task.id, size, sha256: hash.digest("hex") });
  } catch (error) {
    parentPort.postMessage({
      id: task.id,
      error: error instanceof Error ? error.message : String(error),
      code: error && typeof error === "object" && typeof error.code === "string" ? error.code : "",
    });
  } finally {
    if (fd !== -1) {
      try { fs.closeSync(fd); } catch {}
    }
  }
});
`;

const workers = [];
const queue = [];
let nextTaskId = 1;
let idleTimer = null;
const verifiedInodeDigestCache = new Map();

function verifiedInodeCacheKey(stat) {
  if (
    !stat
    || typeof stat.dev !== "bigint"
    || typeof stat.ino !== "bigint"
    || typeof stat.size !== "bigint"
    || typeof stat.mode !== "bigint"
    || typeof stat.mtimeNs !== "bigint"
    || typeof stat.ctimeNs !== "bigint"
  ) {
    return "";
  }
  return `${stat.dev}:${stat.ino}:${stat.size}:${stat.mode}:${stat.mtimeNs}:${stat.ctimeNs}`;
}

function recordVerifiedFileDigest(stat, sha256) {
  if (typeof sha256 !== "string" || sha256.length !== 64) return;
  const key = verifiedInodeCacheKey(stat);
  if (!key) return;
  if (verifiedInodeDigestCache.has(key)) verifiedInodeDigestCache.delete(key);
  else if (verifiedInodeDigestCache.size >= VERIFIED_INODE_DIGEST_CACHE_MAX) {
    verifiedInodeDigestCache.delete(verifiedInodeDigestCache.keys().next().value);
  }
  verifiedInodeDigestCache.set(key, { size: Number(stat.size), sha256 });
}

function getVerifiedFileDigest(stat) {
  if (fs.open !== DEFAULT_FS_OPEN) return null;
  const key = verifiedInodeCacheKey(stat);
  if (!key) return null;
  const entry = verifiedInodeDigestCache.get(key);
  if (!entry) return null;
  verifiedInodeDigestCache.delete(key);
  verifiedInodeDigestCache.set(key, entry);
  return { size: entry.size, sha256: entry.sha256 };
}

function clearIdleTimer() {
  if (idleTimer) {
    clearTimeout(idleTimer);
    idleTimer = null;
  }
}

function scheduleIdleShutdown() {
  clearIdleTimer();
  if (queue.length > 0 || workers.some((slot) => slot.busy)) return;
  if (workers.length === 0) return;
  idleTimer = setTimeout(() => {
    idleTimer = null;
    if (queue.length > 0 || workers.some((slot) => slot.busy)) return;
    while (workers.length > 0) {
      const slot = workers.pop();
      slot.closing = true;
      slot.worker.terminate().catch(() => {});
    }
  }, WORKER_IDLE_TIMEOUT_MS);
  idleTimer.unref();
}

function removeWorkerSlot(slot, failure) {
  const index = workers.indexOf(slot);
  if (index >= 0) workers.splice(index, 1);
  if (slot.current) {
    const task = slot.current;
    slot.current = null;
    slot.busy = false;
    task.reject(failure ?? new Error("Parallel file hash worker exited unexpectedly"));
  }
  dispatchQueue();
  scheduleIdleShutdown();
}

function spawnWorkerSlot() {
  const worker = new Worker(HASH_WORKER_SOURCE, { eval: true });
  worker.unref();
  const slot = { worker, busy: false, current: null, closing: false };
  worker.on("message", (message) => {
    const task = slot.current;
    slot.current = null;
    slot.busy = false;
    worker.unref();
    if (task && message && message.id === task.id) {
      if (message.changed) {
        task.reject(new Error(`${task.changedLabel}: ${task.payload.path}`));
      } else if (message.error) {
        const err = new Error(message.error);
        if (message.code) err.code = message.code;
        task.reject(err);
      } else {
        task.resolve({ size: message.size, sha256: message.sha256 });
      }
    }
    dispatchQueue();
    scheduleIdleShutdown();
  });
  worker.on("error", (error) => {
    removeWorkerSlot(slot, error);
  });
  worker.on("exit", (code) => {
    if (!slot.closing) {
      removeWorkerSlot(slot, new Error(`Parallel file hash worker exited with code ${code}`));
    }
  });
  workers.push(slot);
  return slot;
}

function dispatchQueue() {
  while (queue.length > 0) {
    let slot = workers.find((candidate) => !candidate.busy);
    if (!slot) {
      if (workers.length >= WORKER_POOL_MAX) return;
      slot = spawnWorkerSlot();
    }
    clearIdleTimer();
    const task = queue.shift();
    slot.busy = true;
    slot.current = task;
    slot.worker.ref();
    slot.worker.postMessage(task.payload);
  }
}

async function hashFileInline(filePath, expectedStat, changedLabel) {
  const hasExpected = expectedStat
    && typeof expectedStat.dev === "bigint"
    && typeof expectedStat.ino === "bigint"
    && typeof expectedStat.size === "bigint"
    && typeof expectedStat.mode === "bigint"
    && typeof expectedStat.mtimeNs === "bigint"
    && typeof expectedStat.ctimeNs === "bigint";
  const handle = await fs.open(filePath, fsSync.constants.O_RDONLY | fsSync.constants.O_NOFOLLOW);
  try {
    const before = await handle.stat({ bigint: true });
    if (!before.isFile()) throw new Error("Local package hashing requires a regular file");
    if (
      hasExpected
      && (
        before.dev !== expectedStat.dev
        || before.ino !== expectedStat.ino
        || before.size !== expectedStat.size
        || before.mode !== expectedStat.mode
        || before.mtimeNs !== expectedStat.mtimeNs
        || before.ctimeNs !== expectedStat.ctimeNs
      )
    ) {
      throw new Error(`${changedLabel}: ${filePath}`);
    }
    const hash = crypto.createHash("sha256");
    const buffer = Buffer.allocUnsafe(Math.max(1, Math.min(Number(before.size), HASH_CHUNK_BYTES)));
    let size = 0;
    for (;;) {
      const { bytesRead } = await handle.read(buffer, 0, buffer.length, size);
      if (bytesRead === 0) break;
      hash.update(buffer.subarray(0, bytesRead));
      size += bytesRead;
    }
    if (hasExpected) {
      const after = await handle.stat({ bigint: true });
      if (
        !after.isFile()
        || size !== Number(before.size)
        || after.dev !== before.dev
        || after.ino !== before.ino
        || after.size !== before.size
        || after.mode !== before.mode
        || after.mtimeNs !== before.mtimeNs
        || after.ctimeNs !== before.ctimeNs
      ) {
        throw new Error(`${changedLabel}: ${filePath}`);
      }
    }
    return { size, sha256: hash.digest("hex") };
  } finally {
    await handle.close();
  }
}

async function hashVerifiedRegularFile(filePath, stat, options = {}) {
  const changedLabel = options.changedLabel || "local package file changed while reading";
  if (options.useVerifiedCache === true) {
    const cached = getVerifiedFileDigest(stat);
    if (cached) return cached;
  }
  const sizeNumber = Number(stat.size);
  const hasExactStat = typeof stat.dev === "bigint"
    && typeof stat.ino === "bigint"
    && typeof stat.size === "bigint"
    && typeof stat.mode === "bigint"
    && typeof stat.mtimeNs === "bigint"
    && typeof stat.ctimeNs === "bigint";
  const canUseWorkers = WORKER_POOL_MAX > 1
    && fs.open === DEFAULT_FS_OPEN
    && sizeNumber >= WORKER_HASH_MIN_BYTES
    && hasExactStat;
  if (!canUseWorkers) {
    const result = await hashFileInline(filePath, stat, changedLabel);
    if (hasExactStat && result.size === sizeNumber) {
      recordVerifiedFileDigest(stat, result.sha256);
    }
    return result;
  }
  const result = await new Promise((resolve, reject) => {
    const id = nextTaskId++;
    queue.push({
      id,
      changedLabel,
      payload: {
        id,
        path: filePath,
        dev: stat.dev,
        ino: stat.ino,
        size: stat.size,
        mode: stat.mode,
        mtimeNs: stat.mtimeNs,
        ctimeNs: stat.ctimeNs,
      },
      resolve,
      reject,
    });
    dispatchQueue();
  });
  if (result.size === sizeNumber) {
    recordVerifiedFileDigest(stat, result.sha256);
  }
  return result;
}

module.exports = {
  WORKER_HASH_MIN_BYTES,
  WORKER_POOL_MAX,
  getVerifiedFileDigest,
  hashVerifiedRegularFile,
  recordVerifiedFileDigest,
};
