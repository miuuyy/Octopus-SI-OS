const fs = require("node:fs");
const path = require("node:path");
const { spawnSync } = require("node:child_process");

const repoRoot = path.resolve(__dirname, "..", "..");
const BUILD_OUTPUTS = ["target", "app/build"];
const gigabytes = (bytes) => `${(bytes / 1024 ** 3).toFixed(1)} GB`;

function directoryBytes(directory) {
  if (!fs.statSync(directory, { throwIfNoEntry: false })?.isDirectory()) return null;
  const result = spawnSync("du", ["-sk", directory], { encoding: "utf8", timeout: 30_000 });
  const kilobytes = result.status === 0 ? Number.parseInt(result.stdout, 10) : NaN;
  return Number.isSafeInteger(kilobytes) ? kilobytes * 1024 : null;
}

function buildSpaceNotice(root = repoRoot) {
  const sizes = BUILD_OUTPUTS.map((relative) => [relative, directoryBytes(path.join(root, relative))]).filter(([, bytes]) => bytes !== null);
  const volume = fs.statfsSync(root);
  const total = sizes.reduce((sum, [, bytes]) => sum + bytes, 0);
  const measured = sizes.length
    ? `Build output takes ${gigabytes(total)} (${sizes.map(([relative, bytes]) => `${relative}/ ${gigabytes(bytes)}`).join(", ")}); `
    : "";
  return `${measured}${gigabytes(Number(volume.bavail) * Number(volume.bsize))} is free on this volume.\n`
    + "A build takes a lot of disk space: keep an eye on free space. target/ and the staged or previous bundles in app/build/ are build output and can be deleted when you need the space back.";
}

function printBuildSpaceNotice(root = repoRoot) {
  process.stderr.write(`${buildSpaceNotice(root)}\n`);
}

module.exports = { buildSpaceNotice, printBuildSpaceNotice };
