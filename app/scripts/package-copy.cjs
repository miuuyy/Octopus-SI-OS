const fs = require("node:fs");
const path = require("node:path");
const crypto = require("node:crypto");

function planTree(source, destination, options = {}) {
  const entries = [];
  function visit(from, to) {
    if (options.filter && !options.filter(from, to)) return;
    const stat = fs.lstatSync(from);
    if (stat.isSymbolicLink()) {
      const target = fs.readlinkSync(from);
      entries.push({ type: "symlink", destination: to, target: options.verbatimSymlinks ? target : path.resolve(path.dirname(from), target) });
    } else if (stat.isDirectory()) {
      if (!options.recursive) throw new Error(`Recursive copy is required for ${from}`);
      entries.push({ type: "directory", destination: to, mode: stat.mode & 0o777 });
      for (const name of fs.readdirSync(from)) visit(path.join(from, name), path.join(to, name));
    } else if (stat.isFile()) {
      entries.push({ type: "file", source: from, destination: to });
    } else {
      throw new Error(`Unsupported package entry: ${from}`);
    }
  }
  visit(path.resolve(source), path.resolve(destination));
  return entries;
}

function copyEntries(entries) {
  for (const entry of entries) {
    const destination = entry.destination;
    if (entry.type === "directory") {
      fs.mkdirSync(destination, { recursive: true, mode: entry.mode });
      if (!fs.lstatSync(destination).isDirectory()) throw new Error(`Package directory is not plain: ${destination}`);
      fs.chmodSync(destination, entry.mode);
      continue;
    }
    if (entry.type !== "file" && entry.type !== "symlink") throw new Error("Unknown package copy entry");
    const existing = fs.lstatSync(destination, { throwIfNoEntry: false });
    if (existing) {
      if (existing.isDirectory()) throw new Error(`Package file destination is a directory: ${destination}`);
      if (entry.type === "file") {
        const from = fs.statSync(entry.source), to = fs.statSync(destination, { throwIfNoEntry: false });
        if (to && from.dev === to.dev && from.ino === to.ino) throw new Error(`Package copy source and destination are identical: ${destination}`);
      }
      fs.unlinkSync(destination);
    }
    if (entry.type === "symlink") { fs.symlinkSync(entry.target, destination); continue; }
    const stat = fs.lstatSync(entry.source);
    if (!stat.isFile()) throw new Error(`Package source is not a regular file: ${entry.source}`);
    fs.copyFileSync(entry.source, destination, fs.constants.COPYFILE_EXCL | fs.constants.COPYFILE_FICLONE);
    fs.chmodSync(destination, stat.mode & 0o7777);
    fs.utimesSync(destination, stat.atime, stat.mtime);
  }
}

function applyEntries(entries, platform) {
  if (platform === "darwin") require("./copy-macos.cjs").cloneEntries(entries);
  else copyEntries(entries);
}

function copyTree(source, destination, options = {}, platform = process.platform) {
  applyEntries(planTree(source, destination, options), platform);
}

function copyFile(source, destination, platform = process.platform) {
  applyEntries([{ type: "file", source: path.resolve(source), destination: path.resolve(destination) }], platform);
}

function hashRegularFileNoFollow(file) {
  const hash = crypto.createHash("sha256");
  const descriptor = fs.openSync(file, fs.constants.O_RDONLY | fs.constants.O_NOFOLLOW);
  const buffer = Buffer.allocUnsafe(1024 * 1024);
  try {
    for (;;) {
      const count = fs.readSync(descriptor, buffer, 0, buffer.length, null);
      if (!count) break;
      hash.update(buffer.subarray(0, count));
    }
  } finally {
    fs.closeSync(descriptor);
  }
  return hash.digest("hex");
}

// Deduplicate identical regular files inside a staged distribution tree using
// POSIX hardlinks. Every path stays a plain regular file (never a symlink) at
// its exact expected path, preserving Linux AppArmor path matching, O_NOFOLLOW
// checks, and tar hardlink archiving across both GNU tar and bsdtar.
function deduplicateRegularFiles(root, { minBytes = 4096 } = {}) {
  const resolvedRoot = path.resolve(root);
  const candidates = [];
  const visit = (dir) => {
    const names = fs.readdirSync(dir).sort((a, b) => (a < b ? -1 : a > b ? 1 : 0));
    for (const name of names) {
      const full = path.join(dir, name);
      const stat = fs.lstatSync(full);
      if (stat.isSymbolicLink()) continue;
      if (stat.isDirectory()) {
        visit(full);
        continue;
      }
      if (!stat.isFile() || stat.size < minBytes) continue;
      const rel = path.relative(resolvedRoot, full).split(path.sep).join("/");
      candidates.push({ file: full, rel, size: stat.size, mode: stat.mode & 0o777, dev: stat.dev, ino: stat.ino });
    }
  };
  visit(resolvedRoot);

  const bySizeAndMode = new Map();
  for (const entry of candidates) {
    const bucketKey = `${entry.size}:${entry.mode}`;
    if (!bySizeAndMode.has(bucketKey)) bySizeAndMode.set(bucketKey, []);
    bySizeAndMode.get(bucketKey).push(entry);
  }

  let linkedFiles = 0;
  let alreadyLinkedFiles = 0;
  let deduplicatedFiles = 0;
  let savedBytes = 0;
  let deduplicatedGroups = 0;
  let linkSeq = 0;

  for (const bucket of bySizeAndMode.values()) {
    if (bucket.length < 2) continue;
    const byDigest = new Map();
    for (const entry of bucket) {
      const digest = hashRegularFileNoFollow(entry.file);
      if (!byDigest.has(digest)) byDigest.set(digest, []);
      byDigest.get(digest).push(entry);
    }
    for (const group of byDigest.values()) {
      if (group.length < 2) continue;
      group.sort((left, right) => (left.rel < right.rel ? -1 : left.rel > right.rel ? 1 : 0));
      const canonical = group[0];
      let groupDeduped = false;
      for (const duplicate of group.slice(1)) {
        if (duplicate.dev === canonical.dev && duplicate.ino === canonical.ino) {
          alreadyLinkedFiles += 1;
          deduplicatedFiles += 1;
          savedBytes += duplicate.size;
          groupDeduped = true;
          continue;
        }
        if (duplicate.dev !== canonical.dev) continue;
        const tempLink = path.join(path.dirname(duplicate.file), `.octopus-dedup-${process.pid}-${linkSeq++}`);
        try {
          fs.linkSync(canonical.file, tempLink);
          fs.renameSync(tempLink, duplicate.file);
        } catch (error) {
          fs.rmSync(tempLink, { force: true });
          throw error;
        }
        linkedFiles += 1;
        deduplicatedFiles += 1;
        savedBytes += duplicate.size;
        groupDeduped = true;
      }
      if (groupDeduped) deduplicatedGroups += 1;
    }
  }

  return {
    scannedFiles: candidates.length,
    linkedFiles,
    alreadyLinkedFiles,
    deduplicatedFiles,
    savedBytes,
    deduplicatedGroups,
  };
}

module.exports = { planTree, copyEntries, copyTree, copyFile, deduplicateRegularFiles };
