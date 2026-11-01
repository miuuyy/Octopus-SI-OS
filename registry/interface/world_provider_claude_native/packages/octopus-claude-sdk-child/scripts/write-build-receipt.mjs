import { createHash } from "node:crypto";
import { readFileSync, statSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const binary = resolve(root, "bin/octopus-claude-sdk-child");
const inputs = [
  "package.json",
  "package-lock.json",
  "protocol.schema.json",
  "src/claude.ts",
  "src/main.ts",
  "src/protocol.ts",
  "src/session.ts",
  "src/tools.ts",
];
const digest = (bytes) => createHash("sha256").update(bytes).digest("hex");
const files = Object.fromEntries(inputs.map((name) => [name, digest(readFileSync(resolve(root, name)))]));
const binaryBytes = readFileSync(binary);
const receipt = {
  schema_version: 1,
  executable: "bin/octopus-claude-sdk-child",
  executable_sha256: digest(binaryBytes),
  executable_bytes: statSync(binary).size,
  inputs: files,
};
writeFileSync(resolve(root, "build-receipt.json"), `${JSON.stringify(receipt, null, 2)}\n`, { mode: 0o644 });
