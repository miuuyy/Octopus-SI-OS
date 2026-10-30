import fs from "node:fs";
import path from "node:path";
import process from "node:process";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const manifest = JSON.parse(fs.readFileSync(path.join(root, "package.json"), "utf8"));
const expectedVersion = manifest?.devDependencies?.bun;
if (!/^\d+\.\d+\.\d+$/.test(expectedVersion || "")) {
  throw new Error("package.json must pin one exact Bun toolchain version");
}
const platform = process.platform;
const architecture = process.arch === "arm64" ? "aarch64" : process.arch;
if (!["darwin", "freebsd", "linux", "win32"].includes(platform)) {
  throw new Error(`unsupported Bun platform '${platform}'`);
}
if (!["aarch64", "x64"].includes(architecture)) {
  throw new Error(`unsupported Bun architecture '${process.arch}'`);
}
const packagePlatform = platform === "win32" ? "windows" : platform;
const executableName = platform === "win32" ? "bun.exe" : "bun";
const candidates = [
  path.join(root, "node_modules", "@oven", `bun-${packagePlatform}-${architecture}`, "bin", executableName),
  path.join(root, "node_modules", "bun", "bin", "bun.exe"),
];
const executable = candidates.find((candidate) => {
  try {
    const stat = fs.statSync(candidate);
    return stat.isFile() && (platform === "win32" || (stat.mode & 0o111) !== 0);
  } catch {
    return false;
  }
});
if (!executable) {
  throw new Error(`pinned Bun executable is not installed: ${candidates.join(", ")}`);
}
const version = spawnSync(executable, ["--version"], { encoding: "utf8" });
if (version.status !== 0 || version.stdout.trim() !== expectedVersion) {
  throw new Error(`pinned Bun version mismatch: expected ${expectedVersion}`);
}
const result = spawnSync(executable, process.argv.slice(2), { stdio: "inherit" });
if (result.error) throw result.error;
if (result.status !== 0) process.exit(result.status ?? 1);
