module.exports = { readConfig, resolveEeInstancesRoot, resolveInstallationRoot, resolvePackageExecutable, validateArtifactReadProjection };

const fs = require("node:fs/promises");
const path = require("node:path");
const { CONFIG_PATH, LIVE_STREAM_ARTIFACT_LABEL } = require("./constants.cjs");
const { absolutePath, isObject, positiveInteger, readJsonIfPresent, requiredArray, requiredObject, requiredString, requiredTrimmedString, stringValue } = require("./values.cjs");

async function readConfig() {
  const value = JSON.parse(await fs.readFile(CONFIG_PATH, "utf8"));
  if (!isObject(value) || value.schema_version !== 2) {
    throw new Error("world_operator_chat/client.json has an unsupported schema_version");
  }
  requiredString(value.interface, "client.interface");
  value.artifact_read_projection = validateArtifactReadProjection(value.artifact_read_projection);
  requiredObject(value.executables, "client.executables");
  requiredString(value.executables.topology_planner, "client.executables.topology_planner");
  requiredString(value.executables.artifact_materializer, "client.executables.artifact_materializer");
  requiredObject(value.artifact_labels, "client.artifact_labels");
  requiredString(value.artifact_labels.message, "client.artifact_labels.message");
  requiredString(value.artifact_labels.terminal_output, "client.artifact_labels.terminal_output");
  requiredString(value.artifact_labels.error_output, "client.artifact_labels.error_output");
  requiredArray(value.artifact_labels.stream_outputs, "client.artifact_labels.stream_outputs")
    .forEach((label, index) => requiredString(label, `client.artifact_labels.stream_outputs[${index}]`));
  requiredString(value.artifact_labels.tool_call, "client.artifact_labels.tool_call");
  requiredObject(value.authoring, "client.authoring");
  for (const field of ["source_interface", "room_interface", "tool"]) {
    requiredString(value.authoring[field], `client.authoring.${field}`);
  }
  requiredObject(value.run, "client.run");
  for (const field of [
    "interface_prefix",
    "run_prefix",
    "ee",
    "worker",
    "tool_call_mark",
    "tool_call_verifier",
    "listen_env",
  ]) {
    requiredString(value.run[field], `client.run.${field}`);
  }
  requiredArray(value.run.context_artifact_bindings, "client.run.context_artifact_bindings");
  return value;
}

function validateArtifactReadProjection(value) {
  const fields = ["schema_version", "json_object_labels", "excluded_root_fields", "max_source_bytes", "max_projected_bytes"];
  if (!isObject(value) || value.schema_version !== 1 || Object.keys(value).length !== fields.length
    || fields.some(field => !Object.hasOwn(value, field))) throw new Error("client.artifact_read_projection requires its exact version 1 contract");
  for (const field of ["max_source_bytes", "max_projected_bytes"]) positiveInteger(value[field], `client.artifact_read_projection.${field}`);
  const names = requiredArray(value.excluded_root_fields, "client.artifact_read_projection.excluded_root_fields");
  if (names.length > 65536 || new Set(names).size !== names.length) throw new Error("client.artifact_read_projection requires unique root field names");
  for (const name of names) {
    requiredString(name, "client.artifact_read_projection excluded root field");
    if (name.trim() !== name || Buffer.byteLength(name) > 65536) throw new Error("client.artifact_read_projection root field name exceeds its bound");
  }
  const labels = requiredArray(value.json_object_labels, "client.artifact_read_projection.json_object_labels");
  if (labels.length > 65536 || new Set(labels).size !== labels.length || labels.includes(LIVE_STREAM_ARTIFACT_LABEL)) throw new Error("client.artifact_read_projection requires unique JSON object labels excluding its byte stream");
  for (const label of labels) {
    requiredTrimmedString(label, "client.artifact_read_projection JSON object label");
    if (Buffer.byteLength(label) > 65536) throw new Error("client.artifact_read_projection JSON object label exceeds its bound");
  }
  return Object.freeze({ ...value, json_object_labels: Object.freeze([...labels]), excluded_root_fields: Object.freeze([...names]) });
}

function resolveInstallationRoot(options) {
  if (typeof options?.installationRoot !== "function") {
    throw new Error("installationRoot resolver is required");
  }
  return absolutePath(options.installationRoot(), "installationRoot");
}

function resolvePackageExecutable(config, installationRoot, registryRoot, name) {
  const declared = requiredString(config.executables?.[name], `client.executables.${name}`);
  const ownerRelative = declared.startsWith("owner://");
  const relative = ownerRelative ? declared.slice("owner://".length) : declared;
  if (path.isAbsolute(relative)) {
    throw new Error(`client.executables.${name} must be owner- or installation-relative`);
  }
  const components = relative.split(/[\\/]/);
  if (!relative || components.some((component) => !component || component === "." || component === "..")) {
    throw new Error(`client.executables.${name} is not a normalized relative path`);
  }
  const base = ownerRelative
    ? path.join(registryRoot, "interface", config.interface)
    : installationRoot;
  const executable = path.resolve(base, relative);
  const prefix = base.endsWith(path.sep) ? base : `${base}${path.sep}`;
  if (!executable.startsWith(prefix)) {
    throw new Error(`client.executables.${name} escapes its executable owner`);
  }
  return executable;
}

async function resolveEeInstancesRoot(octopusHome, options = {}) {
  if (typeof options.eeInstancesRoot === "function") {
    return absolutePath(options.eeInstancesRoot(), "ee_instances_root");
  }
  const host = await readJsonIfPresent(path.join(octopusHome, "host.json"));
  const configured = stringValue(host?.root_refs?.ee_instances_root);
  const root = configured || path.join(octopusHome, "state", "ee_instances");
  return absolutePath(root, "ee_instances_root");
}
