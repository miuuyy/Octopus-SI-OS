const fs = require("node:fs");
const path = require("node:path");
const { spawnSync } = require("node:child_process");
const { verifyNodeHeaders } = require("./runtime.cjs");

function run(command, args) {
  const result = spawnSync(command, args, { encoding: "utf8" });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`${command} failed: ${result.stderr.trim() || result.signal || result.status}`);
  return result.stdout.trim();
}

function buildClientFilesystem({ headers = process.env.OCTOPUS_NATIVE_NODE_HEADERS } = {}) {
  if (process.platform !== "darwin") throw new Error("Client package cloning requires macOS");
  headers = verifyNodeHeaders(headers);
  if (!["arm64", "x64"].includes(process.arch)) throw new Error(`Unsupported client filesystem architecture: ${process.arch}`);
  const compiler = run("/usr/bin/xcrun", ["--find", "clang"]);
  const sdk = run("/usr/bin/xcrun", ["--sdk", "macosx", "--show-sdk-path"]);
  const outputRoot = path.join(__dirname, "build");
  fs.mkdirSync(outputRoot, { recursive: true });
  const stage = fs.mkdtempSync(path.join(outputRoot, ".compile-"));
  const name = "octopus-filesystem.node";
  try {
    const output = path.join(stage, name);
    run(compiler, ["-std=c11", "-O2", "-Wall", "-Wextra", "-Werror", "-fvisibility=hidden", "-DNAPI_VERSION=8",
      "-mmacosx-version-min=14.0", "-arch", process.arch === "x64" ? "x86_64" : "arm64", "-isysroot", sdk,
      "-I", headers, "-bundle", "-undefined", "dynamic_lookup", path.join(__dirname, "filesystem.c"), "-o", output]);
    run("/usr/bin/codesign", ["--force", "--sign", "-", output]);
    const destination = path.join(outputRoot, name);
    fs.renameSync(output, destination);
    return destination;
  } finally { fs.rmSync(stage, { recursive: true, force: true }); }
}

if (require.main === module) {
  try { process.stdout.write(`${buildClientFilesystem()}\n`); }
  catch (error) { process.stderr.write(`${error.message}\n`); process.exitCode = 1; }
}

module.exports = { buildClientFilesystem };
