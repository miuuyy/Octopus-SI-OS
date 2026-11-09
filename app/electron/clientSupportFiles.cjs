const fs = require("node:fs/promises");
const { constants } = require("node:fs");
const path = require("node:path");

async function supportPath(root, relative) {
  if (!relative || path.isAbsolute(relative) || relative.split(path.sep).some(part => part === ".." || !part)) throw new Error("outside_report_source");
  const canonicalRoot = await fs.realpath(root);
  let cursor = canonicalRoot;
  for (const part of relative.split(path.sep)) {
    cursor = path.join(cursor, part);
    if ((await fs.lstat(cursor)).isSymbolicLink()) throw new Error("symlink_refused");
  }
  const real = await fs.realpath(cursor);
  if (!real.startsWith(canonicalRoot + path.sep)) throw new Error("outside_report_source");
  return real;
}

async function readBounded(root, relative, limit, tail = false) {
  const destination = await supportPath(root, relative);
  const handle = await fs.open(destination, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK);
  try {
    const stat = await handle.stat();
    if (!stat.isFile()) throw new Error("not_a_regular_file");
    if (!tail && stat.size > limit) throw new Error("source_too_large");
    const bytes = Buffer.alloc(Math.min(stat.size, limit));
    const start = tail ? Math.max(0, stat.size - limit) : 0;
    let offset = 0;
    while (offset < bytes.length) {
      const { bytesRead } = await handle.read(bytes, offset, bytes.length - offset, start + offset);
      if (!bytesRead) throw new Error("source_changed");
      offset += bytesRead;
    }
    const after = await handle.stat();
    const changed = stat.size !== after.size || stat.mtimeMs !== after.mtimeMs || stat.ctimeMs !== after.ctimeMs;
    if (changed && (!tail || after.size < stat.size)) throw new Error("source_changed");
    let text = bytes.toString("utf8");
    const truncated = stat.size > limit;
    if (tail && truncated) text = text.includes("\n") ? text.slice(text.indexOf("\n") + 1) : "";
    return { text, bytes, truncated, observed_bytes: stat.size, changed_during_read: changed };
  } finally { await handle.close(); }
}

async function supportEntries(root, relative, limit) {
  const directory = await fs.opendir(await supportPath(root, relative));
  const entries = [];
  let truncated = false;
  for await (const entry of directory) {
    if (entries.length === limit) { truncated = true; break; }
    entries.push(entry);
  }
  return { entries, truncated };
}

function sourceFailure(error) {
  if (error?.code === "ENOENT") return "not_found";
  if (error instanceof SyntaxError) return "invalid_json";
  return new Set(["symlink_refused", "outside_report_source", "not_a_regular_file", "source_too_large", "source_changed", "digest_mismatch", "invalid_json", "invalid_identity", "read_budget_exhausted"]).has(error?.message) ? error.message : "unreadable";
}

module.exports = { readBounded, supportEntries, sourceFailure };
