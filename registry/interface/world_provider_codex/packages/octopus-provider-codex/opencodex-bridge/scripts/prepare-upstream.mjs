import { spawnSync } from "node:child_process";
import { existsSync, lstatSync, readFileSync, realpathSync } from "node:fs";
import { dirname, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const upstream = JSON.parse(readFileSync(resolve(root, "upstream.json"), "utf8"));
const manifest = JSON.parse(readFileSync(resolve(root, "package.json"), "utf8"));
const name = upstream.npm_package.slice(0, upstream.npm_package.lastIndexOf("@"));
const packageRoot = resolve(root, "node_modules", name);
const installed = JSON.parse(readFileSync(resolve(packageRoot, "package.json"), "utf8"));
if (upstream.npm_package !== `${name}@${installed.version}` || manifest.dependencies[name] !== installed.version) {
  throw new Error("OpenCodex embedding patches require the exact declared upstream version");
}

for (const entry of upstream.patches ?? []) {
  if (typeof entry !== "string" || !/^upstream\/patches\/[a-z0-9-]+\.patch$/.test(entry)) {
    throw new Error("OpenCodex embedding patch must be a declared owner file");
  }
  const file = resolve(root, entry);
  if (!lstatSync(file).isFile() || realpathSync(file) !== file) throw new Error(`Invalid patch file: ${entry}`);
  const content = readFileSync(file, "utf8");
  const targets = [...content.matchAll(/^\+\+\+ b\/(src\/[^\r\n]+)$/gm)].map(match => match[1]);
  if (!targets.length) throw new Error(`Patch has no upstream source targets: ${entry}`);
  for (const target of targets) {
    const path = resolve(packageRoot, target);
    if (!path.startsWith(packageRoot + sep) || target.split("/").some(part => !part || part === "." || part === "..")
      || (existsSync(path) && (!lstatSync(path).isFile() || realpathSync(path) !== path))) {
      throw new Error(`Patch target is not an upstream source file: ${target}`);
    }
  }
  const env = { ...process.env, GIT_CONFIG_NOSYSTEM: "1", GIT_CEILING_DIRECTORIES: dirname(packageRoot) };
  delete env.GIT_DIR;
  delete env.GIT_WORK_TREE;
  delete env.GIT_INDEX_FILE;
  const git = (...args) => spawnSync("git", ["apply", ...args, file], {
    cwd: packageRoot, encoding: "utf8", env,
  });
  if (git("--reverse", "--check").status === 0) continue;
  const check = git("--check");
  if (check.status !== 0) throw new Error(`OpenCodex embedding patch no longer applies: ${entry}\n${check.stderr}`);
  const applied = git();
  if (applied.status !== 0) throw new Error(`OpenCodex embedding patch failed: ${entry}\n${applied.stderr}`);
  if (git("--reverse", "--check").status !== 0) throw new Error(`OpenCodex embedding patch could not be verified: ${entry}`);
}
