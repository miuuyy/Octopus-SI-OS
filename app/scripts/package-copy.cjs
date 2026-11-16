const fs = require("node:fs");
const path = require("node:path");

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

module.exports = { planTree, copyEntries, copyTree, copyFile };
