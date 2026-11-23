module.exports = { jsonDocumentDigest, readJsonFile, readOptionalRegularJsonDocument, readRegularJsonDocument, requirePlainDirectory, requireUnchangedJsonDigest, stableJson };

const fs = require("node:fs/promises");
const crypto = require("node:crypto");

async function readJsonFile(file) {
  try {
    return JSON.parse(await fs.readFile(file, "utf8"));
  } catch (error) {
    if (error?.code === "ENOENT") return null;
    throw new Error(`json parse failed: ${file}: ${error.message}`);
  }
}

function stableJson(value) {
  if (Array.isArray(value)) return `[${value.map(stableJson).join(",")}]`;
  if (value && typeof value === "object") {
    return `{${Object.keys(value).sort().map((key) => `${JSON.stringify(key)}:${stableJson(value[key])}`).join(",")}}`;
  }
  return JSON.stringify(value);
}

function jsonDocumentDigest(value) {
  return crypto.createHash("sha256").update(`${JSON.stringify(value, null, 2)}\n`).digest("hex");
}

async function readRegularJsonDocument(file, label) {
  const observedBeforeReadMs = Date.now();
  const before = await fs.lstat(file);
  if (before.isSymbolicLink() || !before.isFile()) throw new Error(`${label} must be a regular file`);
  const body = await fs.readFile(file);
  const after = await fs.lstat(file);
  if (after.isSymbolicLink() || !after.isFile() || before.ino !== after.ino || before.dev !== after.dev || before.size !== after.size || before.mtimeMs !== after.mtimeMs || before.ctimeMs !== after.ctimeMs) {
    throw new Error(`${label} changed while it was read`);
  }
  let document;
  try {
    document = JSON.parse(body.toString("utf8"));
  } catch (error) {
    throw new Error(`${label} is malformed JSON: ${error.message}`);
  }
  return { document, digest: crypto.createHash("sha256").update(body).digest("hex"), body, stat: after, observedBeforeReadMs };
}

async function readOptionalRegularJsonDocument(file, label) {
  try {
    return await readRegularJsonDocument(file, label);
  } catch (error) {
    if (error?.code === "ENOENT") return null;
    throw error;
  }
}

async function requireUnchangedJsonDigest(file, expectedDigest, label) {
  const current = await readRegularJsonDocument(file, label);
  if (current.digest !== expectedDigest) throw new Error(`${label} changed before the edit could be committed`);
}

async function requirePlainDirectory(directory, label) {
  const stat = await fs.lstat(directory);
  if (stat.isSymbolicLink() || !stat.isDirectory()) throw new Error(`${label} must be a directory and not a symbolic link`);
}
