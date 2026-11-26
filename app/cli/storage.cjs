const fs = require("node:fs/promises");
const constants = require("node:fs").constants;
const os = require("node:os");
const path = require("node:path");

const empty = () => ({ logical_bytes: 0n, allocated_bytes: 0n, files: 0, directories: 0, symlinks: 0, special: 0 });
function add(a, b) { for (const key of Object.keys(a)) a[key] += b[key]; }
function encoded(value) { return Object.fromEntries(Object.entries(value).map(([k, v]) => [k, typeof v === "bigint" ? v.toString() : v])); }
function resolveStorageHome(value, env = process.env, cwd = process.cwd(), userHome = os.homedir()) {
  const input = value ?? env.OCTOPUS_HOME ?? path.join(userHome, ".octopus");
  if (typeof input !== "string" || !input.trim() || input.includes("\0")) throw new Error("--home requires a path.");
  return path.resolve(cwd, input === "~" ? userHome : input.startsWith("~/") ? path.join(userHome, input.slice(2)) : input);
}

// Offline read-only Home inventory using lstat; symlink targets within the selected Home are not followed.
async function inspectStorage(home) {
  home = path.resolve(home);
  const errors = [], nodes = new Map(), runFiles = [], interfaceIds = new Set(), children = new Map();
  const fail = (file, operation, error) => errors.push({ path: path.relative(home, file) || ".", operation, code: error.code || "ERROR", message: error.message });
  let exists = true;
  let ancestor = path.parse(home).root;
  for (const part of home.slice(ancestor.length).split(path.sep).filter(Boolean)) {
    ancestor = path.join(ancestor, part);
    try {
      let stat = await fs.lstat(ancestor);
      if (stat.isSymbolicLink() && ancestor !== home) stat = await fs.stat(ancestor);
      if (stat.isSymbolicLink()) { const error = new Error("Home path contains a symlink; use a physical path."); error.code = "ELOOP"; fail(ancestor, "lstat", error); break; }
      if (!stat.isDirectory()) { const error = new Error("Home path is not a directory."); error.code = "ENOTDIR"; fail(ancestor, "lstat", error); break; }
    } catch (error) {
      if (error.code === "ENOENT") exists = false;
      else fail(ancestor, "lstat", error);
      break;
    }
  }
  async function walk(file, relative) {
    const total = empty();
    let stat;
    try { stat = await fs.lstat(file, { bigint: true }); }
    catch (error) { fail(file, "lstat", error); return total; }
    total.logical_bytes = stat.size;
    if (typeof stat.blocks !== "bigint") { const error = new Error("POSIX st_blocks is unavailable."); error.code = "ENOTSUP"; fail(file, "lstat", error); }
    else total.allocated_bytes = stat.blocks * 512n;
    if (stat.isSymbolicLink()) total.symlinks++;
    else if (stat.isDirectory()) {
      total.directories++;
      let names;
      try { names = await fs.readdir(file); }
      catch (error) { fail(file, "readdir", error); nodes.set(relative, { type: "directory", total }); return total; }
      for (const name of names.sort()) add(total, await walk(path.join(file, name), relative ? relative + "/" + name : name));
    } else if (stat.isFile()) {
      total.files++;
      if (/^state\/runs\/[^/]+\.json$/.test(relative)) runFiles.push({ path: relative, size: stat.size, stat });
    } else total.special++;
    if (stat.isDirectory()) {
      const parent = relative.includes("/") ? relative.slice(0, relative.lastIndexOf("/")) : "";
      if (relative) { const list = children.get(parent) ?? []; list.push(relative); children.set(parent, list); }
    }
    if (stat.isDirectory() || stat.isSymbolicLink() || /^ledger\/[^/]+$/.test(relative) || /^state\/runs\/[^/]+\.json$/.test(relative))
      nodes.set(relative, { type: stat.isDirectory() ? "directory" : stat.isFile() ? "file" : stat.isSymbolicLink() ? "symlink" : "special", total });
    const match = /^interfaces\/([^/]+)$/.exec(relative);
    if (match) interfaceIds.add(match[1]);
    return total;
  }
  const total = exists && errors.length === 0 ? await walk(home, "") : empty();
  const measure = relative => ({ path: relative || ".", present: nodes.has(relative), ...encoded(nodes.get(relative)?.total ?? empty()) });
  const top = (prefix, limit = 10) => (children.get(prefix.replace(/\/$/, "")) ?? [])
    .sort((a, b) => nodes.get(a).total.logical_bytes === nodes.get(b).total.logical_bytes ? (a < b ? -1 : a > b ? 1 : 0) : nodes.get(a).total.logical_bytes > nodes.get(b).total.logical_bytes ? -1 : 1)
    .slice(0, limit).map(measure);
  const interfaces = [...interfaceIds].map(id => {
    const base = "interfaces/" + id;
    return { id, total: measure(base), top_directories: top(base + "/") };
  }).sort((a, b) => BigInt(a.total.logical_bytes) === BigInt(b.total.logical_bytes) ? (a.id < b.id ? -1 : a.id > b.id ? 1 : 0) : BigInt(a.total.logical_bytes) > BigInt(b.total.logical_bytes) ? -1 : 1);
  const largest = runFiles.sort((a, b) => a.size === b.size ? (a.path < b.path ? -1 : a.path > b.path ? 1 : 0) : a.size > b.size ? -1 : 1).slice(0, 5);
  const topRuns = [];
  for (const file of largest) {
    let handle;
    const entry = { run_id: path.basename(file.path, ".json"), ...measure(file.path), inline_amendment_bytes: null, inline_amendment_payload_bytes: null };
    try {
      handle = await fs.open(path.join(home, file.path), constants.O_RDONLY | constants.O_NOFOLLOW);
      const current = await handle.stat({ bigint: true });
      if (!current.isFile() || current.ino !== file.stat.ino || current.dev !== file.stat.dev || current.size !== file.stat.size || current.mtimeNs !== file.stat.mtimeNs) throw Object.assign(new Error("Run changed during scan; retry for a current measurement."), { code: "ESTALE" });
      if (current.size > 256n * 1024n * 1024n) throw Object.assign(new Error("Run exceeds 256 MiB JSON inspection limit."), { code: "EFBIG" });
      const buffer = Buffer.alloc(Number(current.size));
      let offset = 0;
      while (offset < buffer.length) {
        const { bytesRead } = await handle.read(buffer, offset, buffer.length - offset, offset);
        if (!bytesRead) break;
        offset += bytesRead;
      }
      const after = await handle.stat({ bigint: true });
      if (offset !== buffer.length || after.size !== current.size || after.mtimeNs !== current.mtimeNs) throw Object.assign(new Error("Run changed while reading; retry."), { code: "ESTALE" });
      const record = JSON.parse(buffer.toString("utf8"));
      if (!record || typeof record !== "object" || Array.isArray(record)) throw new Error("Run JSON must be an object.");
      let amendmentBytes = 0n, payloadBytes = 0n;
      for (const owner of Object.values(record.interface_state ?? {})) {
        const amendment = owner?.capability_amendment;
        if (amendment != null) {
          amendmentBytes += BigInt(Buffer.byteLength(JSON.stringify(amendment)));
          if (amendment.effect?.payload !== undefined) payloadBytes += BigInt(Buffer.byteLength(JSON.stringify(amendment.effect.payload)));
        }
      }
      entry.inline_amendment_bytes = amendmentBytes.toString();
      entry.inline_amendment_payload_bytes = payloadBytes.toString();
    } catch (error) { fail(path.join(home, file.path), "read_run", error); }
    finally { try { await handle?.close(); } catch (error) { fail(path.join(home, file.path), "close_run", error); } }
    topRuns.push(entry);
  }
  return {
    schema_version: 1, home, exists, ok: errors.length === 0,
    measurement: { byte_encoding: "decimal_string", allocated: "POSIX st_blocks * 512", counting: "per_path_including_directories_and_symlinks", symlink_targets: "not_followed_within_selected_home",
      consistency: "best_effort_non_atomic_live_scan", inline_amendment: "compact_JSON_UTF8_bytes_of_interface_state_owner_capability_amendment_and_effect_payload; top_5_runs_only" },
    total: encoded(total), top_directories: top(""),
    ledger: { total: measure("ledger"), movements: measure("ledger/movements.jsonl"), index: measure("ledger/ledger-index.sqlite"),
      wal: measure("ledger/ledger-index.sqlite-wal"), shm: measure("ledger/ledger-index.sqlite-shm"),
      blobs: measure("ledger/blobs"), metadata: measure("ledger/metadata") },
    interfaces: { total: measure("interfaces"), count: interfaceIds.size, entries: interfaces },
    runs: { total: measure("state/runs"), count: runFiles.length, top: topRuns }, errors,
  };
}

function formatStorage(report) {
  const lines = ["Storage: " + report.home + (report.exists ? "" : " (not present)"),
    "Read-only, non-atomic scan; bytes are logical / POSIX allocated; path counts include hard-link aliases."];
  const row = (label, value) => lines.push(label + ": " + value.logical_bytes + " / " + value.allocated_bytes + " bytes; " + value.files + " files, " + value.directories + " directories, " + value.symlinks + " symlinks");
  row("Total", report.total);
  for (const [label, value] of Object.entries(report.ledger)) row("Ledger " + label, value);
  for (const dir of report.top_directories) row(dir.path, dir);
  lines.push("Interfaces: " + report.interfaces.count);
  for (const entry of report.interfaces.entries) {
    row(entry.id, entry.total);
    for (const dir of entry.top_directories) row("  " + dir.path, dir);
  }
  lines.push("Runs: " + report.runs.count);
  for (const run of report.runs.top) { row(run.path, run); lines.push("  inline amendment (compact JSON UTF-8): " + (run.inline_amendment_bytes ?? "unknown") + " bytes; effect payload: " + (run.inline_amendment_payload_bytes ?? "unknown") + " bytes"); }
  for (const error of report.errors) lines.push("ERROR " + error.path + " [" + error.operation + "/" + error.code + "]: " + error.message);
  return lines.join("\n") + "\n";
}
async function storageCommand(home, json, stdout) {
  const report = await inspectStorage(resolveStorageHome(home));
  stdout.write(json ? JSON.stringify(report, null, 2) + "\n" : require("./presentation.cjs").safeText(formatStorage(report)));
  return report.ok ? 0 : 1;
}
module.exports = { resolveStorageHome, inspectStorage, formatStorage, storageCommand };
