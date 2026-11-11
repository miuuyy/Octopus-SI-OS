import { createHash } from "node:crypto";
import { existsSync, readdirSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { dirname, join, posix, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const self = fileURLToPath(import.meta.url);
const root = resolve(dirname(self), "..");
const binary = resolve(root, "bin/octopus-opencodex-bridge");
const metafile = resolve(root, "node_modules/.cache/octopus-opencodex-bridge/metafile.json");
const entryPoint = "src/main.ts";
const readJson = (name) => JSON.parse(readFileSync(resolve(root, name), "utf8"));
const upstream = readJson("upstream.json");
const separator = upstream.npm_package?.lastIndexOf("@") ?? -1;
const packageName = separator > 0 ? upstream.npm_package.slice(0, separator) : "";
const packageVersion = separator > 0 ? upstream.npm_package.slice(separator + 1) : "";
const packageRoot = resolve(root, "node_modules", packageName);
const installed = JSON.parse(readFileSync(join(packageRoot, "package.json"), "utf8"));
const locked = readJson("package-lock.json").packages?.[`node_modules/${packageName}`];
if (
  !packageName
  || upstream.tag !== `v${packageVersion}`
  || readJson("package.json").dependencies?.[packageName] !== packageVersion
  || installed.name !== packageName
  || installed.version !== packageVersion
  || locked?.version !== packageVersion
  || locked.integrity !== upstream.npm_integrity
) {
  throw new Error(`upstream.json does not attest the installed ${packageName || "OpenCodex"} package`);
}

const meta = JSON.parse(readFileSync(metafile, "utf8"));
rmSync(metafile);
if (!Object.values(meta.outputs ?? {}).some((output) => output?.entryPoint === entryPoint)) {
  throw new Error(`bridge build metafile does not describe the ${entryPoint} executable`);
}
const inside = (base, file) => file.startsWith(base + sep);
const packagePrefix = `node_modules/${packageName}/`;
const bridgeInputs = Object.keys(meta.inputs ?? {}).filter((name) => !name.startsWith("node_modules/")).sort();
for (const name of bridgeInputs) {
  if (!inside(root, resolve(root, name)) || !existsSync(resolve(root, name))) {
    throw new Error(`bridge build input '${name}' is not a file of the bridge owner folder`);
  }
}
if (!bridgeInputs.includes(entryPoint)) throw new Error(`bridge build metafile does not list ${entryPoint}`);

const codeFile = /\.(?:[cm]?[jt]s|[jt]sx)$/;
const walk = (folder) => readdirSync(resolve(root, folder), { withFileTypes: true }).flatMap((item) => {
  const name = posix.join(folder, item.name);
  if (item.isDirectory()) return walk(name);
  if (!item.isFile()) throw new Error(`bridge source '${name}' is neither a file nor a folder`);
  return codeFile.test(item.name) ? [name] : [];
});
const scanned = [...new Set([...walk("src"), ...walk("scripts"), ...bridgeInputs])]
  .filter((name) => resolve(root, name) !== self)
  .sort();

const names = (list) => list.split(",").map((name) => name.trim().replace(/^type\s+/, "").split(/\s+as\s+/)[0]).filter(Boolean);
const referenced = new Map();
const reference = (surface, imported = []) => {
  const symbols = referenced.get(surface) ?? new Set();
  for (const name of imported) symbols.add(name);
  referenced.set(surface, symbols);
};
const escape = (text) => text.replace(/[.*+?^${}()|[\]\\/]/g, "\\$&");
const pkg = escape(packageName);
const scope = escape(packageName.startsWith("@") ? packageName.split("/")[0] : packageName);
const statementPattern = new RegExp(String.raw`(?:import|export)\s+(?:type\s+)?\{([^}]*)\}\s*from\s*(["'])((?:\.\./)+node_modules/${pkg}/[^"'$\\\s]+)\2`, "g");
const stringPattern = new RegExp(String.raw`(["'])((?:\.\./)+node_modules/${pkg}/[^"'$\\\s]+)\1`, "g");
const importContext = /(?:\bfrom|\bimport|\bexport|\brequire)\s*\(?\s*$/;
const declarationPattern = /const\s+opencodexRoot\s*=[^;]*;/g;
const rootStringPattern = new RegExp(String.raw`(["'])(?:\.\./)*node_modules/${pkg}\1`, "g");
const joinPattern = /path\.join\(\s*opencodexRoot\s*,\s*"([^"$\\]+)"\s*\)/g;
const rootImportPattern = /const\s*\{([^}]*)\}\s*=\s*await\s+import\(\s*path\.join\(\s*opencodexRoot\s*,\s*"([^"$\\]+)"\s*\)\s*\)/g;
const plainSurface = (surface) => surface === posix.normalize(surface) && !surface.startsWith("..") && !surface.startsWith("/");
for (const file of scanned) {
  const source = readFileSync(resolve(root, file), "utf8");
  const covered = [];
  const cover = (start, length) => covered.push([start, start + length]);
  const isCovered = (index) => covered.some(([start, end]) => index >= start && index < end);
  const surfaceOf = (specifier) => {
    const target = resolve(root, dirname(file), specifier);
    if (!inside(packageRoot, target)) throw new Error(`${file}: '${specifier}' does not resolve into ${packageName}`);
    return relative(packageRoot, target).split(sep).join("/");
  };
  const statements = new Set();
  for (const match of source.matchAll(statementPattern)) {
    cover(match.index, match[0].length);
    statements.add(match[3]);
    reference(surfaceOf(match[3]), names(match[1]));
  }
  for (const match of source.matchAll(stringPattern)) {
    if (isCovered(match.index)) continue;
    if (importContext.test(source.slice(Math.max(0, match.index - 16), match.index))) {
      throw new Error(`${file}: '${match[2]}' is imported in a form the used_surfaces check does not read`);
    }
    cover(match.index, match[0].length);
    reference(surfaceOf(match[2]));
  }
  const declarations = [...source.matchAll(declarationPattern)];
  for (const declaration of declarations) {
    for (const match of declaration[0].matchAll(rootStringPattern)) cover(declaration.index + match.index, match[0].length);
  }
  const joins = [...source.matchAll(joinPattern)];
  const rootImports = [...source.matchAll(rootImportPattern)];
  if (
    declarations.length > 1
    || (joins.length > 0 && declarations.length === 0)
    || [...source.matchAll(/\bopencodexRoot\b/g)].length !== declarations.length + joins.length
    || [...source.matchAll(/\bimport\(\s*path\.join\(\s*opencodexRoot\b/g)].length !== rootImports.length
    || !joins.every((match) => plainSurface(match[1]))
  ) {
    throw new Error(`${file}: opencodexRoot is used outside one declaration and plain path.join(opencodexRoot, "<surface>") calls`);
  }
  for (const match of joins) reference(match[1]);
  for (const match of rootImports) reference(match[2], names(match[1]));
  for (const match of source.matchAll(new RegExp(scope, "g"))) {
    if (!isCovered(match.index)) throw new Error(`${file}: references ${packageName} in a form the used_surfaces check does not read`);
  }
  for (const record of meta.inputs?.[file]?.imports ?? []) {
    if (record.external || typeof record.path !== "string" || !record.path.startsWith(packagePrefix)) continue;
    if (!statements.has(record.original) || surfaceOf(record.original) !== record.path.slice(packagePrefix.length)) {
      throw new Error(`${file}: bundled import '${record.original}' is not a named import the used_surfaces check reads`);
    }
  }
}

const declared = new Map();
for (const entry of upstream.used_surfaces ?? []) {
  const [surface, symbols, extra] = entry.split("#");
  if (extra !== undefined || declared.has(surface)) throw new Error(`upstream.json used surface '${entry}' is malformed or repeated`);
  declared.set(surface, new Set(symbols === undefined ? [] : names(symbols)));
}
const sameSet = (left, right) => left.size === right.size && [...left].every((value) => right.has(value));
const drift = [
  ...[...referenced.keys()].filter((surface) => !declared.has(surface)).map((surface) => `undeclared ${surface}`),
  ...[...declared.keys()].filter((surface) => !referenced.has(surface)).map((surface) => `unused ${surface}`),
  ...[...referenced].filter(([surface, symbols]) => declared.has(surface) && !sameSet(symbols, declared.get(surface)))
    .map(([surface, symbols]) => `${surface} imports ${[...symbols].sort().join(",") || "no symbols"} but declares ${[...declared.get(surface)].sort().join(",") || "no symbols"}`),
  ...[...declared.keys(), ...(upstream.excluded_surfaces ?? [])].filter((surface) => !existsSync(join(packageRoot, surface)))
    .map((surface) => `missing ${surface}`),
  ...[...declared.keys()].filter((surface) => (upstream.excluded_surfaces ?? []).some((excluded) => surface === excluded || surface.startsWith(`${excluded}/`)))
    .map((surface) => `excluded ${surface}`),
];
if (drift.length) throw new Error(`upstream.json used_surfaces drifted from the bridge sources:\n${drift.join("\n")}`);

const digest = (bytes) => createHash("sha256").update(bytes).digest("hex");
const inputs = ["package.json", "package-lock.json", "upstream.json", "scripts/prepare-upstream.mjs", ...(upstream.patches ?? []), ...bridgeInputs];
const files = Object.fromEntries(inputs.map((name) => [name, digest(readFileSync(resolve(root, name)))]));
const binaryBytes = readFileSync(binary);
const receipt = {
  schema_version: 1,
  executable: "bin/octopus-opencodex-bridge",
  executable_sha256: digest(binaryBytes),
  executable_bytes: statSync(binary).size,
  inputs: files,
  upstream
};
writeFileSync(resolve(root, "build-receipt.json"), `${JSON.stringify(receipt, null, 2)}\n`, { mode: 0o644 });
