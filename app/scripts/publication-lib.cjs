const crypto = require("node:crypto");
const fs = require("node:fs/promises");
const os = require("node:os");
const path = require("node:path");

const FORBIDDEN_PRIVATE_BASENAMES = new Set([
  ".octopus-source-manifest.json",
  ".env",
  ".netrc",
  ".npmrc",
  ".pypirc",
  "auth.json",
  "client-access.json",
  "client-lab-access.json",
  "cookies.json",
  "credentials.json",
  "environment.json",
  "host.json",
  "movements.jsonl",
  "octopus_os.json",
  "token.json",
]);
const FORBIDDEN_PRIVATE_KEY_NAMES = new Set([
  "id_dsa",
  "id_ecdsa",
  "id_ed25519",
  "id_rsa",
]);
const PRIVATE_KEY_BLOCK = /-----BEGIN (?:ENCRYPTED |RSA |EC |OPENSSH )?PRIVATE KEY-----\r?\n[A-Za-z0-9+/=\r\n]{128,}-----END (?:ENCRYPTED |RSA |EC |OPENSSH )?PRIVATE KEY-----/g;
const SAFE_SPDX = /^[A-Za-z0-9][A-Za-z0-9.+-]{0,63}$/;

function parseOptions(argv, allowed) {
  const result = {};
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (!argument.startsWith("--")) throw new Error(`unexpected argument '${argument}'`);
    const name = argument.slice(2);
    if (!allowed.has(name)) throw new Error(`unknown option '--${name}'`);
    if (Object.prototype.hasOwnProperty.call(result, name)) throw new Error(`option '--${name}' is repeated`);
    const value = argv[index + 1];
    if (!value || value.startsWith("--")) throw new Error(`option '--${name}' requires a value`);
    result[name] = value;
    index += 1;
  }
  return result;
}

function requiredOption(options, name) {
  const value = typeof options[name] === "string" ? options[name].trim() : "";
  if (!value) throw new Error(`--${name} is required`);
  return value;
}

function requiredAbsoluteOption(options, name) {
  const value = requiredOption(options, name);
  if (!path.isAbsolute(value)) throw new Error(`--${name} must be absolute`);
  return path.resolve(value);
}

function publicationPrivateRoots() {
  const configured = typeof process.env.OCTOPUS_HOME === "string" ? process.env.OCTOPUS_HOME.trim() : "";
  if (configured && !path.isAbsolute(configured)) throw new Error("OCTOPUS_HOME must be absolute when publishing");
  return [...new Set([
    path.resolve(configured || path.join(os.homedir(), ".octopus")),
  ])];
}

function pathContains(root, candidate) {
  const relative = path.relative(root, candidate);
  return relative === "" || !relative.startsWith(`..${path.sep}`) && relative !== ".." && !path.isAbsolute(relative);
}

function assertOutsidePrivateRoots(candidate, label, roots = publicationPrivateRoots()) {
  const resolved = path.resolve(candidate);
  for (const root of roots) {
    if (pathContains(root, resolved)) {
      throw new Error(`${label} is inside OCTOPUS_HOME; copy reviewed source into a clean source directory before publication`);
    }
  }
  return resolved;
}

async function assertDisjointPaths(left, leftLabel, right, rightLabel) {
  const [resolvedLeft, resolvedRight] = await Promise.all([
    canonicalCandidatePath(left),
    canonicalCandidatePath(right),
  ]);
  if (pathContains(resolvedLeft, resolvedRight) || pathContains(resolvedRight, resolvedLeft)) {
    throw new Error(`${leftLabel} and ${rightLabel} must be separate directories`);
  }
}

async function canonicalCandidatePath(candidate) {
  let current = path.resolve(candidate);
  const missing = [];
  for (;;) {
    try {
      const existing = await fs.realpath(current);
      return path.join(existing, ...missing.reverse());
    } catch (error) {
      if (error?.code !== "ENOENT") throw error;
    }
    const parent = path.dirname(current);
    if (parent === current) throw new Error(`publication path cannot be resolved: ${candidate}`);
    missing.push(path.basename(current));
    current = parent;
  }
}

async function requirePlainDirectory(directory, label) {
  const stat = await fs.lstat(directory);
  if (!stat.isDirectory() || stat.isSymbolicLink()) throw new Error(`${label} is not a plain directory: ${directory}`);
  return fs.realpath(directory);
}

async function requireRegularFile(file, label, maximumBytes = 512 * 1024 * 1024) {
  const stat = await fs.lstat(file);
  if (!stat.isFile() || stat.isSymbolicLink()) throw new Error(`${label} is not a regular file: ${file}`);
  if (stat.size < 1 || stat.size > maximumBytes) throw new Error(`${label} has an invalid size: ${file}`);
  const body = await fs.readFile(file);
  if (body.byteLength !== stat.size) throw new Error(`${label} changed while reading: ${file}`);
  return { body, stat };
}

async function readJsonRegular(file, label, maximumBytes) {
  const { body, stat } = await requireRegularFile(file, label, maximumBytes);
  let value;
  try {
    value = JSON.parse(body.toString("utf8"));
  } catch (error) {
    throw new Error(`${label} is malformed JSON: ${errorMessage(error)}`);
  }
  if (!isRecord(value)) throw new Error(`${label} must be a JSON object`);
  return { body, stat, value };
}

async function readSourceRegistryIdentity(sourceRegistryRoot) {
  const activeMarker = path.join(sourceRegistryRoot, ".octopus-source-manifest.json");
  try {
    await fs.lstat(activeMarker);
    throw new Error(`source registry is an Active Registry, not a publication source: ${sourceRegistryRoot}`);
  } catch (error) {
    if (error?.code !== "ENOENT") throw error;
  }
  const file = path.join(sourceRegistryRoot, ".octopus-source.json");
  const { value } = await readJsonRegular(file, "source registry identity", 64 * 1024);
  if (
    value.schema_version !== 1
    || typeof value.id !== "string"
    || !/^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/.test(value.id)
  ) throw new Error(`source registry identity is invalid: ${file}`);
  return value.id;
}

async function readPublicationLicense(options, privateRoots) {
  const spdx = requiredOption(options, "license");
  if (!SAFE_SPDX.test(spdx)) throw new Error("--license must be one SPDX identifier without an expression");
  const source = assertOutsidePrivateRoots(requiredAbsoluteOption(options, "license-file"), "license file", privateRoots);
  await assertOutsideDetectedOctopusHome(source, "license file");
  const { body } = await requireRegularFile(source, "license file", 1024 * 1024);
  if (body.byteLength === 0) throw new Error("license file must not be empty");
  const publicationPath = path.basename(source);
  if (["packages", "publication.json", "registry"].includes(publicationPath)) {
    throw new Error(`license filename is reserved by the publication layout: ${publicationPath}`);
  }
  return {
    spdx,
    source,
    body,
    path: publicationPath,
    sha256: sha256(body),
  };
}

function validatePublicPayload(relativePath, body, options = {}) {
  const normalized = relativePath.split(path.sep).join("/");
  const basename = path.posix.basename(normalized);
  const lower = basename.toLowerCase();
  if (
    FORBIDDEN_PRIVATE_BASENAMES.has(lower)
    || lower.startsWith(".env.") && !lower.endsWith(".example")
    || FORBIDDEN_PRIVATE_KEY_NAMES.has(lower)
    || lower.endsWith(".p12")
    || lower.endsWith(".pfx")
    || lower.endsWith(".key")
  ) throw new Error(`publication payload contains private-state path '${normalized}'`);
  const text = body.toString("utf8");
  if (containsUnapprovedPrivateKey(text, normalized)) {
    throw new Error(`publication payload contains private-key material at '${normalized}'`);
  }
  const resolvedHome = path.resolve(options.userHome ?? os.homedir());
  const normalizedHome = resolvedHome.split(path.sep).join("/");
  if (normalizedHome !== "/" && normalizedHome !== "/root") {
    const userHome = resolvedHome.endsWith(path.sep) ? resolvedHome : `${resolvedHome}${path.sep}`;
    const posixUserHome = normalizedHome.endsWith("/") ? normalizedHome : `${normalizedHome}/`;
    if (text.includes(userHome) || text.includes(posixUserHome)) {
      throw new Error(`publication payload contains an absolute user-home path at '${normalized}'`);
    }
  }
  for (const privateRoot of options.privateRoots ?? publicationPrivateRoots()) {
    if (text.includes(privateRoot) || text.includes(privateRoot.split(path.sep).join("/"))) {
      throw new Error(`publication payload contains an OCTOPUS_HOME path at '${normalized}'`);
    }
  }
  if (options.rejectDevelopmentExecutables && lower.endsWith(".json")) {
    let value;
    try {
      value = JSON.parse(text);
    } catch (error) {
      throw new Error(`publication JSON is malformed at '${normalized}': ${errorMessage(error)}`);
    }
    walkJson(value, (candidate) => {
      if (typeof candidate === "string" && /^(?:\.\/)?target\/(?:debug|release)\//.test(candidate)) {
        throw new Error(`publication retains a development executable path at '${normalized}'`);
      }
    });
  }
}

function containsUnapprovedPrivateKey(text, relativePath) {
  PRIVATE_KEY_BLOCK.lastIndex = 0;
  let match;
  while ((match = PRIVATE_KEY_BLOCK.exec(text)) !== null) {
    const sourceFixture = /\.(?:c|cc|cpp|go|java|js|jsx|mjs|py|rs|swift|ts|tsx)$/.test(relativePath);
    const prefix = text.slice(Math.max(0, match.index - 512), match.index);
    if (sourceFixture && /\btest(?:_|\b)/i.test(prefix)) continue;
    return true;
  }
  return false;
}

async function createAtomicPublication(outRootValue, bundleName, writer, privateRoots) {
  const outRoot = assertOutsidePrivateRoots(path.resolve(outRootValue), "publication output", privateRoots);
  await assertOutsideDetectedOctopusHome(outRoot, "publication output");
  await fs.mkdir(outRoot, { recursive: true, mode: 0o755 });
  const realOutRoot = await requirePlainDirectory(outRoot, "publication output");
  const finalRoot = path.join(realOutRoot, bundleName);
  try {
    await fs.lstat(finalRoot);
    throw new Error(`publication output already exists: ${finalRoot}`);
  } catch (error) {
    if (error?.code !== "ENOENT") throw error;
  }
  const stageRoot = path.join(realOutRoot, `.${bundleName}.stage-${crypto.randomUUID()}`);
  await fs.mkdir(stageRoot, { mode: 0o700 });
  try {
    const result = await writer(stageRoot);
    await syncDirectory(stageRoot);
    await fs.rename(stageRoot, finalRoot);
    await fs.chmod(finalRoot, 0o755);
    await syncDirectory(realOutRoot);
    return { ...result, output: finalRoot };
  } catch (error) {
    await fs.rm(stageRoot, { recursive: true, force: true }).catch(() => undefined);
    throw error;
  }
}

async function assertOutsideDetectedOctopusHome(candidate, label) {
  let current = path.resolve(candidate);
  try {
    const stat = await fs.lstat(current);
    if (stat.isFile()) current = path.dirname(current);
  } catch (error) {
    if (error?.code !== "ENOENT") throw error;
  }
  for (;;) {
    if (await looksLikeOctopusHome(current)) {
      throw new Error(`${label} is inside a detected Octopus Home; copy reviewed source into a clean source directory before publication`);
    }
    const parent = path.dirname(current);
    if (parent === current) break;
    current = parent;
  }
  return path.resolve(candidate);
}

async function looksLikeOctopusHome(directory) {
  const checks = await Promise.all([
    pathType(path.join(directory, "host.json"), "file"),
    pathType(path.join(directory, "registry"), "directory"),
    pathType(path.join(directory, "state"), "directory"),
    pathType(path.join(directory, "client"), "directory"),
  ]);
  return checks[0] && checks[1] && (checks[2] || checks[3]);
}

async function pathType(candidate, expected) {
  try {
    const stat = await fs.lstat(candidate);
    if (stat.isSymbolicLink()) return false;
    return expected === "file" ? stat.isFile() : stat.isDirectory();
  } catch (error) {
    if (error?.code === "ENOENT" || error?.code === "ENOTDIR") return false;
    throw error;
  }
}

async function writePublicationFile(root, relativePath, body, mode = 0o644) {
  const normalized = safeRelativePath(relativePath);
  const target = path.join(root, ...normalized.split("/"));
  if (!pathContains(root, target)) throw new Error(`publication path escapes its root: ${relativePath}`);
  await fs.mkdir(path.dirname(target), { recursive: true, mode: 0o755 });
  const handle = await fs.open(target, "wx", mode & 0o777);
  try {
    await handle.writeFile(body);
    await handle.sync();
  } finally {
    await handle.close();
  }
  await fs.chmod(target, mode & 0o777);
  await syncDirectory(path.dirname(target));
  return fileEvidence(normalized, body, mode);
}

async function copyPublicationFile(root, relativePath, source, expected) {
  const { body, stat } = await requireRegularFile(source, `publication source '${relativePath}'`);
  const actual = {
    size: stat.size,
    mode: stat.mode & 0o777,
    sha256: sha256(body),
  };
  if (expected && (
    actual.size !== expected.size
    || actual.mode !== expected.mode
    || actual.sha256 !== expected.sha256
  )) throw new Error(`publication source changed after snapshot: ${source}`);
  return writePublicationFile(root, relativePath, body, actual.mode);
}

function fileEvidence(relativePath, body, mode) {
  return {
    path: safeRelativePath(relativePath),
    size: body.byteLength,
    mode: mode & 0o777,
    sha256: sha256(body),
  };
}

function publicationDigest(value) {
  return sha256(Buffer.from(stableJson(value), "utf8"));
}

function prettyJson(value) {
  return Buffer.from(`${JSON.stringify(value, null, 2)}\n`, "utf8");
}

function stableJson(value) {
  if (Array.isArray(value)) return `[${value.map(stableJson).join(",")}]`;
  if (value && typeof value === "object") {
    return `{${Object.keys(value).sort().map((key) => `${JSON.stringify(key)}:${stableJson(value[key])}`).join(",")}}`;
  }
  return JSON.stringify(value);
}

function sha256(body) {
  return crypto.createHash("sha256").update(body).digest("hex");
}

function safeRelativePath(value) {
  if (typeof value !== "string" || !value || path.isAbsolute(value)) throw new Error(`publication path is invalid: ${value}`);
  const normalized = value.replaceAll("\\", "/").split(path.sep).join("/");
  const parts = normalized.split("/");
  if (parts.some((part) => !part || part === "." || part === "..")) throw new Error(`publication path is invalid: ${value}`);
  return normalized;
}

async function syncDirectory(directory) {
  const handle = await fs.open(directory, "r");
  try {
    await handle.sync();
  } finally {
    await handle.close();
  }
}

function walkJson(value, visit) {
  visit(value);
  if (Array.isArray(value)) {
    for (const entry of value) walkJson(entry, visit);
    return;
  }
  if (value && typeof value === "object") {
    for (const entry of Object.values(value)) walkJson(entry, visit);
  }
}

function isRecord(value) {
  return Boolean(value) && typeof value === "object" && !Array.isArray(value);
}

function errorMessage(error) {
  return error instanceof Error ? error.message : String(error);
}

module.exports = {
  assertDisjointPaths,
  assertOutsideDetectedOctopusHome,
  assertOutsidePrivateRoots,
  copyPublicationFile,
  createAtomicPublication,
  errorMessage,
  parseOptions,
  prettyJson,
  publicationDigest,
  publicationPrivateRoots,
  readJsonRegular,
  readPublicationLicense,
  readSourceRegistryIdentity,
  requiredAbsoluteOption,
  requiredOption,
  requirePlainDirectory,
  requireRegularFile,
  sha256,
  stableJson,
  validatePublicPayload,
  writePublicationFile,
};
