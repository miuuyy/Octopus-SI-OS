const fs = require("node:fs/promises");
const path = require("node:path");
const { INTERFACE_PRIVATE_ENVIRONMENT_MAX_BYTES } = require("./projection/constants.cjs");
const { readOptionalRegularJsonDocument, readRegularJsonDocument, requirePlainDirectory } = require("./projection/jsonFiles.cjs");
const { readInterfaceEnvironmentDeclarations, readInterfacePrivateEnvironmentDocument, validateInterfacePrivateDirectory, writeInterfacePrivateValues } = require("./projection/privateConfiguration.cjs");
const { parseApplicationManifest, resolveAuthoredApplicationSetupFields } = require("./projection/sidecars.cjs");
const { exactObjectKeys, isPlainRecord, requiredSafeRegistryId, stringValue } = require("./projection/values.cjs");

const CONNECTION_STATES = new Set(["connected", "not_connected", "expired", "error"]);
const PROBE_TIMEOUT_MS = 30_000;
const DISCONNECT_TIMEOUT_MS = 60_000;
const CONNECT_TIMEOUT_MS = 15 * 60_000;
const OPERATION_ID = /^[A-Za-z0-9][A-Za-z0-9._:-]{7,127}$/;
const SERVER_TOOL_PREFIX = /^[a-z][a-z0-9_]{0,31}$/;
const SERVER_TOOL_NAME = /^[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}$/;
const SERVER_TOOL_LIMIT = 512;
const SERVER_TOOL_SCHEMA_BYTES = 64 * 1024;
const SERVER_TOOL_DESCRIPTION_CHARS = 4096;
const SERVER_TOOL_HINTS = ["readOnlyHint", "destructiveHint", "idempotentHint", "openWorldHint"];

class ApplicationConnectionStopped extends Error {
  constructor(message, code) {
    super(message);
    this.code = code;
  }
}

function createApplicationConnectionHost({ resolveTargets, scope = () => "" }) {
  const operations = new Map();
  const busy = new Set();

  async function probe(request) {
    exactObjectKeys(request, ["applicationId"], ["applicationId"], "application connection check");
    const applicationId = requiredSafeRegistryId(request.applicationId, "application connection check application");
    const startScope = scope();
    const targets = await resolveTargets(applicationId, null);
    const interfaces = await Promise.all(targets.map(async (target) => {
      if (target.error) return { interfaceId: target.interfaceId, state: connectionFailure(target.error) };
      const controller = new AbortController();
      const timer = setTimeout(() => controller.abort(new ApplicationConnectionStopped(`${target.source} did not answer within ${PROBE_TIMEOUT_MS / 1000} seconds.`, "failed")), PROBE_TIMEOUT_MS);
      try {
        const ctx = await target.createContext({ kind: "probe", input: {}, signal: controller.signal, progress: discardProgress });
        return { interfaceId: target.interfaceId, state: validateConnectionState(await untilAborted(target.module.probe(ctx), controller.signal), target.source) };
      } catch (error) {
        return { interfaceId: target.interfaceId, state: connectionFailure(controller.signal.aborted ? controller.signal.reason : error) };
      } finally {
        clearTimeout(timer);
      }
    }));
    requireScope(startScope);
    return { applicationId, interfaces };
  }

  async function connect(request, { onProgress } = {}) {
    exactObjectKeys(request, ["operationId", "applicationId", "interfaceId", "input"], ["operationId", "applicationId", "interfaceId", "input"], "application connection request");
    const operationId = typeof request.operationId === "string" && OPERATION_ID.test(request.operationId) ? request.operationId : "";
    if (!operationId) throw new Error("Application connection request has no valid operation id.");
    const applicationId = requiredSafeRegistryId(request.applicationId, "application connection application");
    const interfaceId = requiredSafeRegistryId(request.interfaceId, "application connection interface");
    if (operations.has(operationId)) throw new Error("This connection attempt is already running.");
    const key = `${applicationId}\0${interfaceId}`;
    if (busy.has(key)) throw new Error("A connection attempt for this application is already in progress.");
    const controller = new AbortController();
    operations.set(operationId, controller);
    busy.add(key);
    const timer = setTimeout(() => controller.abort(new ApplicationConnectionStopped("The connection was not finished in time. Try again.", "failed")), CONNECT_TIMEOUT_MS);
    const startScope = scope();
    try {
      const [target] = await resolveTargets(applicationId, interfaceId);
      const progress = (update) => {
        const valid = validateConnectionProgress(update);
        if (!controller.signal.aborted) onProgress?.({ operationId, applicationId, interfaceId, ...valid });
      };
      const ctx = await target.createContext({ kind: "connect", input: request.input, signal: controller.signal, progress });
      const state = validateConnectionState(await untilAborted(target.module.connect(ctx), controller.signal), target.source);
      requireScope(startScope);
      if (state.state === "connected" && target.catalog) await writeOperationCatalog(target.catalog);
      return state;
    } catch (error) {
      throw controller.signal.aborted ? controller.signal.reason : error;
    } finally {
      clearTimeout(timer);
      operations.delete(operationId);
      busy.delete(key);
    }
  }

  function cancel(request) {
    exactObjectKeys(request, ["operationId"], ["operationId"], "application connection cancellation");
    const controller = operations.get(request.operationId);
    if (!controller || controller.signal.aborted) return { cancelled: false };
    controller.abort(new ApplicationConnectionStopped("Connection was cancelled.", "cancelled"));
    return { cancelled: true };
  }

  async function disconnect(request) {
    exactObjectKeys(request, ["applicationId", "interfaceId"], ["applicationId", "interfaceId"], "application disconnection request");
    const applicationId = requiredSafeRegistryId(request.applicationId, "application disconnection application");
    const interfaceId = requiredSafeRegistryId(request.interfaceId, "application disconnection interface");
    const key = `${applicationId}\0${interfaceId}`;
    if (busy.has(key)) throw new Error("Finish or cancel the connection attempt before disconnecting.");
    busy.add(key);
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(new ApplicationConnectionStopped(`Disconnecting did not finish within ${DISCONNECT_TIMEOUT_MS / 1000} seconds.`, "failed")), DISCONNECT_TIMEOUT_MS);
    const startScope = scope();
    try {
      const [target] = await resolveTargets(applicationId, interfaceId);
      const ctx = await target.createContext({ kind: "disconnect", input: {}, signal: controller.signal, progress: discardProgress });
      let state;
      if (typeof target.module.disconnect === "function") {
        state = validateConnectionState(await untilAborted(target.module.disconnect(ctx), controller.signal), target.source);
      } else {
        await ctx.saveValues(Object.fromEntries(Object.keys(ctx.values).map((name) => [name, null])));
        state = validateConnectionState(await untilAborted(target.module.probe(ctx), controller.signal), target.source);
      }
      requireScope(startScope);
      return state;
    } catch (error) {
      throw controller.signal.aborted ? controller.signal.reason : error;
    } finally {
      clearTimeout(timer);
      busy.delete(key);
    }
  }

  function dispose() {
    for (const controller of operations.values()) {
      controller.abort(new ApplicationConnectionStopped("Connection was cancelled because Octopus is closing.", "cancelled"));
    }
  }

  function requireScope(startScope) {
    if (scope() !== startScope) throw new Error("The selected Home changed during the connection. Try again.");
  }

  return { probe, connect, cancel, disconnect, dispose };
}

function createRegistryConnectionTargets({ registryRoot, octopusHome, launchEnvironment, openExternal }) {
  return async function resolveTargets(applicationId, interfaceId) {
    const registry = path.resolve(registryRoot());
    const home = path.resolve(octopusHome());
    const applicationDirectory = path.join(registry, "interface", applicationId);
    await requirePlainDirectory(applicationDirectory, `Application '${applicationId}'`);
    const manifestFile = path.join(applicationDirectory, "application.json");
    const manifest = parseApplicationManifest(
      (await readRegularJsonDocument(manifestFile, `application manifest ${applicationId}`)).document,
      applicationId,
      manifestFile,
    );
    if (interfaceId !== null && !manifest.interfaces.includes(interfaceId)) {
      throw new Error(`Application '${applicationId}' does not include interface '${interfaceId}'.`);
    }
    const targets = [];
    for (const id of interfaceId === null ? manifest.interfaces : [interfaceId]) {
      try {
        const directory = path.join(registry, "interface", id);
        const descriptor = (await readRegularJsonDocument(path.join(directory, "interface.json"), `interface descriptor ${id}`)).document;
        if (stringValue(descriptor?.id) !== id) throw new Error(`Interface descriptor '${id}' does not match its registry folder.`);
        if (!declaresConnectionModule(descriptor)) {
          if (interfaceId === null) continue;
          throw new Error(`Interface '${id}' declares no client connection module.`);
        }
        const { module, reference } = await loadConnectionModule(registry, descriptor);
        const catalog = operationCatalogTarget(descriptor, directory, id, registry);
        targets.push({
          interfaceId: id,
          module,
          source: `Connection module ${reference}`,
          catalog,
          createContext: (request) => registryConnectionContext({
            ...request,
            descriptor,
            directory,
            catalog,
            interfaceId: id,
            manifest,
            privateDirectory: path.join(home, "interfaces", id),
            launchEnvironment,
            openExternal,
          }),
        });
      } catch (error) {
        if (interfaceId !== null) throw error;
        targets.push({ interfaceId: id, error });
      }
    }
    return targets;
  };
}

async function registryConnectionContext({ kind, input, signal, progress, descriptor, directory, catalog, interfaceId, manifest, privateDirectory, launchEnvironment, openExternal }) {
  const declarations = await readInterfaceEnvironmentDeclarations(descriptor, interfaceId, directory);
  const declaredNames = new Set(declarations.map((declaration) => declaration.name));
  const launchNames = await readInterfaceLaunchEnvironmentNames(interfaceId, directory, declaredNames);
  const inputNames = new Set(resolveAuthoredApplicationSetupFields(
    manifest.setup.fields,
    interfaceId,
    declarations,
    `application '${manifest.id}' setup`,
  ).map(({ declaration }) => declaration.name));
  await validateInterfacePrivateDirectory(privateDirectory, true);
  const document = await readInterfacePrivateEnvironmentDocument(privateDirectory, interfaceId, declaredNames);
  const environment = launchEnvironment() ?? {};
  const serverTools = kind === "connect" ? serverToolOperationTarget(descriptor, directory, interfaceId) : null;
  return connectionContext({
    descriptor,
    interfaceId,
    values: Object.fromEntries(document.values),
    input: kind === "connect" ? connectionInput(input, inputNames, interfaceId) : {},
    privateDirectory,
    signal,
    progress,
    saveValues: (changes) => writeInterfacePrivateValues(privateDirectory, interfaceId, declaredNames, changes),
    launchEnvironment: (name) => {
      if (typeof name !== "string" || !launchNames.has(name)) {
        throw new Error(`Interface '${interfaceId}' does not declare '${String(name)}' in its life.json.`);
      }
      return typeof environment[name] === "string" ? environment[name] : undefined;
    },
    openExternal,
    publishOperations: serverTools ? async (tools) => {
      const published = await writeServerToolOperations(serverTools, tools);
      await writeOperationCatalog(catalog);
      return published;
    } : null,
  });
}

function operationCatalogTarget(descriptor, directory, interfaceId, registry) {
  const config = descriptor?.interface_driver_config?.private_config;
  if (!isPlainRecord(config) || config.operation_catalog_dir === undefined) return null;
  const catalogDir = stringValue(config.operation_catalog_dir);
  const catalogLabel = stringValue(config.operation_catalog_label);
  const requestLabel = stringValue(config.operation_request_label);
  const inputLabels = config.task_input_labels ?? [];
  if (!catalogDir || path.isAbsolute(catalogDir) || catalogDir.split(/[\\/]/).some((part) => !part || part === "." || part === "..")
    || !catalogLabel || !requestLabel || !Array.isArray(inputLabels)
    || inputLabels.some((label) => typeof label !== "string" || !label || label.trim() !== label)
    || new Set(inputLabels).size !== inputLabels.length) {
    throw new Error(`Interface '${interfaceId}' declares an operation catalog without a valid directory, catalog label, request label and task input labels.`);
  }
  const artifactId = `artifact-operation-catalog-${interfaceId}`;
  return { interfaceId, root: path.join(directory, catalogDir), catalogLabel, requestLabel, inputLabels, artifactId,
    file: path.join(registry, "artifact", artifactId, "artifact.json") };
}

async function writeOperationCatalog(target) {
  await requirePlainDirectory(target.root, `Operation catalog of '${target.interfaceId}'`);
  const files = (await fs.readdir(target.root, { withFileTypes: true }))
    .filter((entry) => entry.isFile() && entry.name.endsWith(".json")).map((entry) => entry.name).sort();
  const operations = [];
  for (const file of files) {
    const { document } = await readRegularJsonDocument(path.join(target.root, file), `operation ${file}`);
    const entry = {
      name: stringValue(document?.id),
      description: stringValue(document?.description),
      input_schema: document?.input_schema,
      request_label: stringValue(document?.request_label),
      output_label: stringValue(document?.output_label),
    };
    if (`${entry.name}.json` !== file || !entry.description || !isPlainRecord(entry.input_schema)
      || entry.request_label !== target.requestLabel || !entry.output_label || target.inputLabels.includes(entry.output_label)) {
      throw new Error(`Operation '${file}' of '${target.interfaceId}' is not a complete operation of its catalog.`);
    }
    operations.push(entry);
  }
  if (!operations.length) throw new Error(`Interface '${target.interfaceId}' declares an empty operation catalog.`);
  const content = `${JSON.stringify({
    id: target.artifactId,
    label: target.catalogLabel,
    interface: target.interfaceId,
    request_label: target.requestLabel,
    ...(target.inputLabels.length ? { task_input_labels: target.inputLabels } : {}),
    operations,
  }, null, 2)}\n`;
  await fs.mkdir(path.dirname(target.file), { recursive: true });
  const temporary = path.join(path.dirname(target.file), `.artifact.json.${process.pid}.${Date.now()}.tmp`);
  await fs.writeFile(temporary, content, { flag: "wx", mode: 0o644 });
  await fs.rename(temporary, target.file);
}

function serverToolOperationTarget(descriptor, directory, interfaceId) {
  const config = descriptor?.interface_driver_config?.private_config;
  if (!isPlainRecord(config) || config.server_tool_operations === undefined) return null;
  const declaration = config.server_tool_operations;
  exactObjectKeys(declaration, ["prefix"], ["prefix"], `interface '${interfaceId}' server_tool_operations`);
  const catalogDir = stringValue(config.operation_catalog_dir);
  const requestLabel = stringValue(config.operation_request_label);
  if (!SERVER_TOOL_PREFIX.test(stringValue(declaration.prefix))
    || !catalogDir || path.isAbsolute(catalogDir) || catalogDir.split(/[\\/]/).some((part) => !part || part === "." || part === "..")
    || !requestLabel) {
    throw new Error(`Interface '${interfaceId}' declares server_tool_operations without a valid prefix, operation_catalog_dir and operation_request_label.`);
  }
  return { interfaceId, prefix: declaration.prefix, root: path.join(directory, catalogDir), requestLabel };
}

function serverToolDescription(tool) {
  const text = [tool.description, tool.title, tool.annotations?.title]
    .find((value) => typeof value === "string" && value.trim())?.trim() ?? `Call the server tool ${tool.name}.`;
  return text.length > SERVER_TOOL_DESCRIPTION_CHARS ? `${text.slice(0, SERVER_TOOL_DESCRIPTION_CHARS - 1)}…` : text;
}

function serverToolAnnotations(tool) {
  const source = isPlainRecord(tool?.annotations) ? tool.annotations : {};
  const annotations = {};
  if (typeof source.title === "string" && source.title.trim()) annotations.title = source.title.trim().slice(0, 256);
  for (const hint of SERVER_TOOL_HINTS) {
    if (typeof source[hint] === "boolean") annotations[hint] = source[hint];
  }
  return Object.keys(annotations).length ? annotations : null;
}

function generatedServerTool(document) {
  return document?.handler_config?.operation === "call_tool" && typeof document.handler_config.mcp_tool_name === "string";
}

async function writeServerToolOperations(target, tools) {
  if (!Array.isArray(tools) || tools.length > SERVER_TOOL_LIMIT) {
    throw new Error(`Server tools for '${target.interfaceId}' must be a list of at most ${SERVER_TOOL_LIMIT} tools.`);
  }
  await requirePlainDirectory(target.root, `Operation catalog of '${target.interfaceId}'`);
  const authored = new Set();
  const generated = new Set();
  for (const entry of await fs.readdir(target.root, { withFileTypes: true })) {
    if (!entry.isFile() || !entry.name.endsWith(".json")) continue;
    const { document } = await readRegularJsonDocument(path.join(target.root, entry.name), `operation ${entry.name}`);
    (generatedServerTool(document) ? generated : authored).add(entry.name);
  }
  const skipped = [];
  const approvalRequired = [];
  const files = new Map();
  for (const tool of tools) {
    const name = typeof tool?.name === "string" ? tool.name : "";
    const id = name.startsWith(`${target.prefix}.`) ? name : `${target.prefix}.${name}`;
    const schemaBytes = isPlainRecord(tool?.inputSchema) ? JSON.stringify(tool.inputSchema).length : Infinity;
    const encodedLength = id.length + (id.match(/[.-]/g) ?? []).length;
    const reason = !SERVER_TOOL_NAME.test(name) ? "its name cannot be an operation id"
      : encodedLength > 64 ? "its name is too long for a model tool"
      : authored.has(`${id}.json`) ? "it would replace an operation of the application"
        : files.has(`${id}.json`) ? "the server lists it twice"
          : schemaBytes > SERVER_TOOL_SCHEMA_BYTES ? "its input schema is missing or too large"
            : null;
    if (reason) {
      skipped.push(`${name || "(unnamed)"}: ${reason}`);
      continue;
    }
    const annotations = serverToolAnnotations(tool);
    files.set(`${id}.json`, `${JSON.stringify({
      id,
      request_label: target.requestLabel,
      output_label: "mcp.tool.result",
      handler_config: { operation: "call_tool", mcp_tool_name: name, ...(annotations ? { annotations } : {}) },
      description: serverToolDescription(tool),
      input_schema: tool.inputSchema,
    }, null, 2)}\n`);
    if (annotations?.readOnlyHint !== true) approvalRequired.push(id);
  }
  for (const [file, content] of files) {
    const destination = path.join(target.root, file);
    const temporary = path.join(target.root, `.${file}.${process.pid}.${Date.now()}.tmp`);
    await fs.writeFile(temporary, content, { flag: "wx", mode: 0o644 });
    await fs.rename(temporary, destination);
  }
  for (const file of generated) {
    if (!files.has(file)) await fs.rm(path.join(target.root, file));
  }
  return { published: files.size, approvalRequired, skipped };
}

function connectionContext({ descriptor, interfaceId, values, input, privateDirectory, signal, progress, saveValues, launchEnvironment, openExternal, publishOperations = null }) {
  let current = Object.freeze({ ...values });
  return Object.freeze({
    descriptor: deepFreeze(structuredClone(descriptor)),
    interfaceId,
    get values() { return current; },
    input: Object.freeze({ ...input }),
    async saveValues(changes) {
      if (signal.aborted) throw signal.reason;
      current = Object.freeze(Object.fromEntries(await saveValues(changes)));
    },
    privateDirectory,
    launchEnvironment,
    async openExternal(url) {
      const href = connectionHttpsUrl(url, "Connection link");
      if (signal.aborted) throw signal.reason;
      await openExternal(href);
    },
    progress,
    signal,
    fetch: globalThis.fetch,
    ...(publishOperations ? {
      async publishOperations(tools) {
        if (signal.aborted) throw signal.reason;
        return publishOperations(structuredClone(tools));
      },
    } : {}),
  });
}

function declaresConnectionModule(descriptor) {
  const config = descriptor?.interface_driver_config?.private_config;
  return isPlainRecord(config) && Object.prototype.hasOwnProperty.call(config, "client_connection");
}

async function loadConnectionModule(registryRoot, descriptor) {
  const declaration = descriptor.interface_driver_config.private_config.client_connection;
  const parts = isPlainRecord(declaration) && typeof declaration.module === "string"
    && declaration.module.match(/^interface:\/\/([A-Za-z0-9_-]+)\/(packages\/.+\.cjs)$/);
  if (!parts || declaration.schema_version !== 1 || Object.keys(declaration).some((key) => !["schema_version", "module"].includes(key))
    || parts[2].split("/").some((part) => !part || part === "." || part === ".." || /[\\\x00]/.test(part))) {
    throw new Error(`Interface '${descriptor.id}' has an invalid client connection declaration.`);
  }
  const owner = (await readRegularJsonDocument(
    path.join(registryRoot, "interface", parts[1], "interface.json"),
    `interface descriptor ${parts[1]}`,
  )).document;
  if (stringValue(owner?.id) !== parts[1] || owner.interface_driver_config?.private_config?.client_connection?.module !== declaration.module) {
    throw new Error(`Connection module ${declaration.module} is not declared by its installed owner.`);
  }
  const root = await fs.realpath(registryRoot);
  const file = path.join(root, "interface", parts[1], parts[2]);
  if (await fs.realpath(file) !== file || !(await fs.stat(file)).isFile()) {
    throw new Error(`Connection module ${declaration.module} must be a regular registry file.`);
  }
  delete require.cache[file];
  const module = require(file);
  if (module?.schemaVersion !== 1 || typeof module.probe !== "function" || typeof module.connect !== "function"
    || (module.disconnect !== undefined && typeof module.disconnect !== "function")) {
    throw new Error(`Connection module ${declaration.module} does not implement the client connection contract.`);
  }
  return { module, reference: declaration.module };
}

async function readInterfaceLaunchEnvironmentNames(interfaceId, directory, declaredNames) {
  const names = new Set(declaredNames);
  const life = await readOptionalRegularJsonDocument(path.join(directory, "life.json"), `interface life ${interfaceId}`);
  for (const processDocument of life?.document?.processes ?? []) {
    const lists = [processDocument?.inherit_env ?? [], ...["read_env_paths", "write_env_paths", "executable_env_paths"].map((field) => processDocument?.authority?.[field] ?? [])];
    for (const list of lists) {
      if (!Array.isArray(list) || list.some((name) => typeof name !== "string" || !name || name.includes("=") || name.includes("\0"))) {
        throw new Error(`interface life '${interfaceId}' declares an invalid environment name`);
      }
      for (const name of list) names.add(name);
    }
  }
  return names;
}

function connectionInput(value, inputNames, interfaceId) {
  if (!isPlainRecord(value)) throw new Error("Connection details must be an object of setup fields.");
  return Object.fromEntries(Object.entries(value).map(([name, text]) => {
    if (!inputNames.has(name)) throw new Error(`'${name}' is not a setup field of interface '${interfaceId}'.`);
    if (typeof text !== "string" || !text || text.includes("\0") || Buffer.byteLength(text) > INTERFACE_PRIVATE_ENVIRONMENT_MAX_BYTES) {
      throw new Error(`Setup field '${name}' must contain a value.`);
    }
    return [name, text];
  }));
}

function validateConnectionState(value, source) {
  const fail = (reason) => { throw new Error(`${source} returned an invalid connection state: ${reason}.`); };
  if (!isPlainRecord(value)) fail("expected an object");
  const unknown = Object.keys(value).filter((key) => !["state", "account", "message"].includes(key));
  if (unknown.length) fail(`unknown field ${unknown.join(", ")}`);
  if (!CONNECTION_STATES.has(value.state)) fail("state must be connected, not_connected, expired or error");
  const state = { state: value.state };
  if (value.account !== undefined) {
    if (!isPlainRecord(value.account) || Object.keys(value.account).some((key) => !["label", "detail"].includes(key))) {
      fail("account must contain only label and detail");
    }
    state.account = { label: connectionText(value.account.label, 96, "account.label", fail) };
    if (value.account.detail !== undefined) state.account.detail = connectionText(value.account.detail, 160, "account.detail", fail);
  }
  if (value.message !== undefined) state.message = connectionText(value.message, 300, "message", fail, true);
  return state;
}

function validateConnectionProgress(update) {
  const fail = (reason) => { throw new Error(`Connection progress is invalid: ${reason}.`); };
  if (!isPlainRecord(update) || Object.keys(update).some((key) => !["message", "url", "code"].includes(key))) {
    fail("expected message with an optional url and code");
  }
  const progress = { message: connectionText(update.message, 300, "message", fail, true) };
  if (update.url !== undefined) progress.url = connectionHttpsUrl(update.url, "Connection progress link");
  if (update.code !== undefined) {
    if (typeof update.code !== "string" || !/^[\x21-\x7e](?:[\x20-\x7e]{0,62}[\x21-\x7e])?$/.test(update.code)) fail("code must be 1..64 printable characters");
    progress.code = update.code;
  }
  return progress;
}

function connectionText(value, limit, field, fail, multiline = false) {
  const invalid = multiline ? /[\u0000-\u0009\u000b-\u001f\u007f]/u : /[\u0000-\u001f\u007f]/u;
  if (typeof value !== "string" || !value.trim() || value.length > limit || invalid.test(value)) {
    fail(`${field} must be text of 1..${limit} characters`);
  }
  return value.trim();
}

function connectionHttpsUrl(value, label) {
  let url = null;
  try {
    url = typeof value === "string" && value.length <= 2048 ? new URL(value) : null;
  } catch {
    url = null;
  }
  if (!url || url.protocol !== "https:" || url.username || url.password) {
    throw new Error(`${label} must be an https address without credentials.`);
  }
  return url.href;
}

function connectionFailure(error) {
  const text = error instanceof Error && error.message ? error.message : String(error || "The connection could not be checked.");
  const message = text.replace(/[\u0000-\u0009\u000b-\u001f\u007f]/gu, " ").trim() || "The connection could not be checked.";
  return { state: "error", message: message.length > 300 ? `${message.slice(0, 299)}…` : message };
}

function untilAborted(promise, signal) {
  if (signal.aborted) return Promise.reject(signal.reason);
  return new Promise((resolve, reject) => {
    const stop = () => reject(signal.reason);
    signal.addEventListener("abort", stop, { once: true });
    Promise.resolve(promise).then(resolve, reject).finally(() => signal.removeEventListener("abort", stop));
  });
}

function discardProgress(update) {
  validateConnectionProgress(update);
}

function deepFreeze(value) {
  if (value && typeof value === "object" && !Object.isFrozen(value)) {
    Object.freeze(value);
    for (const child of Object.values(value)) deepFreeze(child);
  }
  return value;
}

module.exports = {
  ApplicationConnectionStopped,
  connectionContext,
  connectionInput,
  createApplicationConnectionHost,
  createRegistryConnectionTargets,
  validateConnectionProgress,
  validateConnectionState,
};
