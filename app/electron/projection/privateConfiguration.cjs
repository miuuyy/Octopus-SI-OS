module.exports = { applicationLogoFileName, atomicWritePrivateEnvironment, mutateApplicationPrivateConfiguration, mutateApplicationPrivateConfigurationUnlocked, readApplicationLogoAsset, readInterfaceEnvironmentDeclarations, readInterfacePrivateConfigurationProjection, readInterfacePrivateEnvironmentDocument, resolveJsonPointer, validInterfaceEnvironmentName, validateInterfacePrivateDirectory, validateInterfacePrivateFileStat, validateOwnerOnlyStat, writeInterfacePrivateValues };

const fs = require("node:fs/promises");
const crypto = require("node:crypto");
const path = require("node:path");
const { APPLICATION_LOGO_MAX_BYTES, INTERFACE_PRIVATE_ENVIRONMENT_FILE, INTERFACE_PRIVATE_ENVIRONMENT_MAX_BYTES, INTERFACE_PRIVATE_RESERVED_ENV } = require("./constants.cjs");
const { readOptionalRegularJsonDocument, readRegularJsonDocument, requirePlainDirectory } = require("./jsonFiles.cjs");
const { normalizeRuntimeLaunchEnvironment, parseApplicationManifest, resolveAuthoredApplicationSetupFields } = require("./sidecars.cjs");
const { exactObjectKeys, isPathInside, isPlainRecord, requiredDigest, requiredSafeRegistryId, requiredString, stringValue, syncDirectory, validJsonPointer } = require("./values.cjs");

let applicationPrivateConfigurationMutationTail = Promise.resolve();

async function readInterfacePrivateConfigurationProjection({
  descriptor,
  interfaceId,
  interfaceRegistryDirectory,
  interfacePrivateDirectory,
  launchEnvironment,
}) {
  const declarations = await readInterfaceEnvironmentDeclarations(
    descriptor,
    interfaceId,
    interfaceRegistryDirectory,
  );
  const document = await readInterfacePrivateEnvironmentDocument(
    interfacePrivateDirectory,
    interfaceId,
    new Set(declarations.map((declaration) => declaration.name)),
  );
  const environmentKnown = launchEnvironment !== null;
  const fields = declarations.map((declaration) => {
    const privateValue = document.values.get(declaration.name);
    const environmentValue = environmentKnown && Object.prototype.hasOwnProperty.call(launchEnvironment, declaration.name)
      ? launchEnvironment[declaration.name]
      : undefined;
    const source = privateValue !== undefined && environmentValue !== undefined
      ? privateValue === environmentValue ? "both" : "conflict"
      : privateValue !== undefined
        ? "private"
        : environmentValue !== undefined
          ? environmentValue.length > 0 ? "environment" : "missing"
          : environmentKnown ? "missing" : "unknown";
    return {
      name: declaration.name,
      pointers: declaration.pointers,
      required: declaration.required === true,
      configured: source !== "conflict"
        && (privateValue !== undefined || (environmentValue !== undefined && environmentValue.length > 0)),
      private_saved: privateValue !== undefined,
      source,
    };
  });
  return {
    interface_id: interfaceId,
    status: fields.some((field) => field.source === "conflict") ? "conflict" : "valid",
    document_status: document.present ? "valid" : "absent",
    document_digest: document.digest,
    fields,
    error: "",
  };
}

async function readInterfaceEnvironmentDeclarations(descriptor, interfaceId, interfaceRegistryDirectory) {
  const lifePath = path.join(interfaceRegistryDirectory, "life.json");
  const life = await readOptionalRegularJsonDocument(lifePath, `interface life ${interfaceId}`);
  if (!life) return [];
  exactObjectKeys(life.document, ["processes"], ["processes"], `interface life ${interfaceId}`);
  if (!Array.isArray(life.document.processes) || life.document.processes.length === 0) {
    throw new Error(`interface life '${interfaceId}' declares no processes`);
  }
  const declarations = new Map();
  life.document.processes.forEach((processDocument, processIndex) => {
    if (!isPlainRecord(processDocument)) throw new Error(`interface life '${interfaceId}' process ${processIndex} is invalid`);
    const inheritedNames = processDocument.inherit_env === undefined ? [] : processDocument.inherit_env;
    const requiredNames = processDocument.required_env === undefined ? [] : processDocument.required_env;
    if (!Array.isArray(inheritedNames) || inheritedNames.some((name) => !validInterfaceEnvironmentName(name))) {
      throw new Error(`interface life '${interfaceId}' process ${processIndex} has invalid inherit_env`);
    }
    if (!Array.isArray(requiredNames) || requiredNames.some((name) => (
      typeof name !== "string" || !inheritedNames.includes(name)
    ))) {
      throw new Error(`interface life '${interfaceId}' process ${processIndex} has invalid required_env`);
    }
    for (const name of inheritedNames) {
      if (name === "PATH") continue;
      const current = declarations.get(name) ?? { pointers: new Set(), required: false };
      current.required ||= requiredNames.includes(name);
      declarations.set(name, current);
    }
    const pointers = processDocument.inherit_env_from_interface === undefined
      ? []
      : processDocument.inherit_env_from_interface;
    if (!Array.isArray(pointers)) {
      throw new Error(`interface life '${interfaceId}' process ${processIndex} has invalid inherit_env_from_interface`);
    }
    for (const [pointerIndex, pointer] of pointers.entries()) {
      if (!validJsonPointer(pointer)) {
        throw new Error(`interface life '${interfaceId}' process ${processIndex} env pointer ${pointerIndex} is invalid`);
      }
      const selected = resolveJsonPointer(descriptor, pointer);
      if (selected === undefined) {
        throw new Error(`interface life '${interfaceId}' env pointer '${pointer}' is absent from its descriptor`);
      }
      const names = Array.isArray(selected) ? selected : [selected];
      for (const [nameIndex, candidate] of names.entries()) {
        if (typeof candidate !== "string" || !validInterfaceEnvironmentName(candidate)) {
          throw new Error(`interface life '${interfaceId}' env pointer '${pointer}' item ${nameIndex} is invalid`);
        }
        if (candidate === "PATH") continue;
        const current = declarations.get(candidate) ?? { pointers: new Set(), required: false };
        current.pointers.add(pointer);
        current.required ||= requiredNames.includes(candidate);
        declarations.set(candidate, current);
      }
    }
  });
  return [...declarations.entries()]
    .map(([name, declaration]) => ({
      name,
      pointers: [...declaration.pointers].sort(),
      required: declaration.required,
    }))
    .sort((left, right) => left.name.localeCompare(right.name));
}

function resolveJsonPointer(value, pointer) {
  let cursor = value;
  for (const encoded of pointer.split("/").slice(1)) {
    const token = encoded.replaceAll("~1", "/").replaceAll("~0", "~");
    if (Array.isArray(cursor)) {
      if (!/^(?:0|[1-9][0-9]*)$/.test(token)) return undefined;
      cursor = cursor[Number(token)];
    } else if (isPlainRecord(cursor) && Object.prototype.hasOwnProperty.call(cursor, token)) {
      cursor = cursor[token];
    } else {
      return undefined;
    }
  }
  return cursor;
}

function validInterfaceEnvironmentName(value) {
  const name = typeof value === "string" ? value.trim() : "";
  return Boolean(name)
    && name === value
    && !name.includes("=")
    && !name.includes("\0")
    && !INTERFACE_PRIVATE_RESERVED_ENV.has(name);
}

async function readInterfacePrivateEnvironmentDocument(directory, interfaceId, declaredNames) {
  const file = path.join(directory, INTERFACE_PRIVATE_ENVIRONMENT_FILE);
  try {
    await validateInterfacePrivateDirectory(directory, false);
  } catch (error) {
    if (error?.code === "ENOENT") return { present: false, digest: "", values: new Map() };
    throw error;
  }
  let stat;
  try {
    stat = await fs.lstat(file);
  } catch (error) {
    if (error?.code === "ENOENT") return { present: false, digest: "", values: new Map() };
    throw error;
  }
  validateInterfacePrivateFileStat(file, stat);
  const body = await fs.readFile(file);
  const after = await fs.lstat(file);
  validateInterfacePrivateFileStat(file, after);
  if (stat.dev !== after.dev || stat.ino !== after.ino || stat.size !== after.size || stat.mtimeMs !== after.mtimeMs) {
    throw new Error(`interface private environment '${interfaceId}' changed while it was read`);
  }
  let document;
  try {
    document = JSON.parse(body.toString("utf8"));
  } catch (error) {
    throw new Error(`interface private environment '${interfaceId}' is malformed JSON: ${error.message}`);
  }
  exactObjectKeys(document, ["schema_version", "interface", "values"], ["schema_version", "interface", "values"], `interface private environment ${interfaceId}`);
  if (document.schema_version !== 1 || document.interface !== interfaceId || !isPlainRecord(document.values)) {
    throw new Error(`interface private environment '${interfaceId}' has invalid schema or identity`);
  }
  const values = new Map();
  for (const [name, value] of Object.entries(document.values)) {
    if (!declaredNames.has(name) || typeof value !== "string" || value.length === 0 || value.includes("\0")) {
      throw new Error(`interface private environment '${interfaceId}' contains invalid or undeclared field '${name}'`);
    }
    values.set(name, value);
  }
  return {
    present: true,
    digest: crypto.createHash("sha256").update(body).digest("hex"),
    values,
  };
}

function validateInterfacePrivateFileStat(file, stat) {
  if (stat.isSymbolicLink() || !stat.isFile()) throw new Error(`interface private environment '${file}' must be a regular file`);
  if (stat.size > INTERFACE_PRIVATE_ENVIRONMENT_MAX_BYTES) {
    throw new Error(`interface private environment '${file}' exceeds ${INTERFACE_PRIVATE_ENVIRONMENT_MAX_BYTES} bytes`);
  }
  validateOwnerOnlyStat(file, stat, false);
}

async function validateInterfacePrivateDirectory(directory, create) {
  try {
    const stat = await fs.lstat(directory);
    if (stat.isSymbolicLink() || !stat.isDirectory()) throw new Error(`interface private directory '${directory}' must be a real directory`);
    validateOwnerOnlyStat(directory, stat, true);
  } catch (error) {
    if (!create || error?.code !== "ENOENT") throw error;
    const parent = path.dirname(directory);
    await requirePlainDirectory(parent, `interface private root ${parent}`);
    await fs.mkdir(directory, { mode: 0o700 });
    const stat = await fs.lstat(directory);
    if (stat.isSymbolicLink() || !stat.isDirectory()) throw new Error(`interface private directory '${directory}' was not created safely`);
    validateOwnerOnlyStat(directory, stat, true);
  }
}

function validateOwnerOnlyStat(label, stat, directory) {
  if (process.platform === "win32") return;
  const currentUid = typeof process.geteuid === "function" ? process.geteuid() : null;
  if (currentUid !== null && Number.isSafeInteger(stat.uid) && stat.uid !== currentUid) {
    throw new Error(`${directory ? "interface private directory" : "interface private environment"} '${label}' is not owned by the current user`);
  }
  if ((stat.mode & 0o077) !== 0) {
    throw new Error(`${directory ? "interface private directory" : "interface private environment"} '${label}' is not owner-only`);
  }
}

async function mutateApplicationPrivateConfiguration(registryRootValue, octopusHomeRootValue, request, options = {}) {
  const run = applicationPrivateConfigurationMutationTail.then(() => (
    mutateApplicationPrivateConfigurationUnlocked(registryRootValue, octopusHomeRootValue, request, options)
  ));
  applicationPrivateConfigurationMutationTail = run.then(() => undefined, () => undefined);
  return run;
}

async function mutateApplicationPrivateConfigurationUnlocked(registryRootValue, octopusHomeRootValue, request, options) {
  exactObjectKeys(
    request,
    ["applicationId", "interfaceId", "expectedDigest", "updates", "clears"],
    ["applicationId", "interfaceId", "expectedDigest", "updates", "clears"],
    "application private configuration mutation",
  );
  const applicationId = requiredSafeRegistryId(request.applicationId, "application private configuration applicationId");
  const interfaceId = requiredSafeRegistryId(request.interfaceId, "application private configuration interfaceId");
  const expectedDigest = request.expectedDigest === null
    ? null
    : requiredDigest(request.expectedDigest, "application private configuration expectedDigest");
  if (!isPlainRecord(request.updates) || !Array.isArray(request.clears)) {
    throw new Error("application private configuration updates must be an object and clears must be an array");
  }
  const registryRoot = path.resolve(requiredString(registryRootValue, "registry root"));
  const octopusHomeRoot = path.resolve(requiredString(octopusHomeRootValue, "Octopus home root"));
  const applicationDirectory = path.join(registryRoot, "interface", applicationId);
  await requirePlainDirectory(applicationDirectory, `application registry owner ${applicationId}`);
  const applicationManifestPath = path.join(applicationDirectory, "application.json");
  const applicationManifest = parseApplicationManifest(
    (await readRegularJsonDocument(applicationManifestPath, `application manifest ${applicationId}`)).document,
    applicationId,
    applicationManifestPath,
  );
  if (!applicationManifest.interfaces.includes(interfaceId)) {
    throw new Error(`application '${applicationId}' does not own interface '${interfaceId}'`);
  }
  const interfaceRegistryDirectory = path.join(registryRoot, "interface", interfaceId);
  await requirePlainDirectory(interfaceRegistryDirectory, `interface registry owner ${interfaceId}`);
  const descriptor = (await readRegularJsonDocument(
    path.join(interfaceRegistryDirectory, "interface.json"),
    `interface descriptor ${interfaceId}`,
  )).document;
  if (stringValue(descriptor.id ?? descriptor.rule_id) !== interfaceId) {
    throw new Error(`interface descriptor '${interfaceId}' does not match its registry folder`);
  }
  const declarations = await readInterfaceEnvironmentDeclarations(
    descriptor,
    interfaceId,
    interfaceRegistryDirectory,
  );
  const declaredNames = new Set(declarations.map((declaration) => declaration.name));
  const presented = applicationManifest.setup.fields.filter((field) => field.interface === interfaceId);
  const editableNames = new Set((presented.length
    ? resolveAuthoredApplicationSetupFields(
        applicationManifest.setup.fields,
        interfaceId,
        declarations,
        `application '${applicationId}' setup`,
      ).map(({ declaration }) => declaration.name)
    : declarations.map((declaration) => declaration.name)));
  if (editableNames.size === 0) {
    throw new Error(`application '${applicationId}' declares no editable setup fields for interface '${interfaceId}'`);
  }
  const launchEnvironment = normalizeRuntimeLaunchEnvironment(
    typeof options.runtimeLaunchEnvironment === "function" ? options.runtimeLaunchEnvironment() : null,
  );
  const updates = new Map();
  for (const [name, value] of Object.entries(request.updates)) {
    if (!editableNames.has(name) || typeof value !== "string" || value.length === 0 || value.includes("\0") || value.length > INTERFACE_PRIVATE_ENVIRONMENT_MAX_BYTES) {
      throw new Error(`application private configuration update '${name}' is invalid or undeclared`);
    }
    if (launchEnvironment !== null
      && Object.prototype.hasOwnProperty.call(launchEnvironment, name)
      && launchEnvironment[name] !== value) {
      throw new Error(`application private configuration update '${name}' conflicts with the managed runtime environment`);
    }
    updates.set(name, value);
  }
  const clears = request.clears.map((name, index) => {
    if (typeof name !== "string" || !editableNames.has(name)) {
      throw new Error(`application private configuration clear ${index} is invalid or undeclared`);
    }
    return name;
  });
  if (new Set(clears).size !== clears.length) throw new Error("application private configuration clears contain duplicates");
  if (clears.some((name) => updates.has(name))) throw new Error("application private configuration cannot update and clear the same field");
  if (updates.size === 0 && clears.length === 0) throw new Error("application private configuration mutation contains no changes");

  const interfacePrivateRoot = path.join(octopusHomeRoot, "interfaces");
  await requirePlainDirectory(interfacePrivateRoot, `interface private root ${interfacePrivateRoot}`);
  const interfacePrivateDirectory = path.join(interfacePrivateRoot, interfaceId);
  let current;
  try {
    current = await readInterfacePrivateEnvironmentDocument(interfacePrivateDirectory, interfaceId, declaredNames);
  } catch (error) {
    if (error?.code !== "ENOENT") throw error;
    current = { present: false, digest: "", values: new Map() };
  }
  if ((current.present ? current.digest : null) !== expectedDigest) {
    throw new Error(`application private configuration '${interfaceId}' changed before the edit could be committed`);
  }
  for (const name of clears) {
    if (!current.values.has(name)) {
      throw new Error(`application private configuration clear '${name}' has no saved value`);
    }
  }
  const values = new Map(current.values);
  for (const [name, value] of updates) values.set(name, value);
  for (const name of clears) values.delete(name);
  const document = {
    schema_version: 1,
    interface: interfaceId,
    values: Object.fromEntries([...values.entries()].sort(([left], [right]) => left.localeCompare(right))),
  };
  const body = Buffer.from(`${JSON.stringify(document, null, 2)}\n`, "utf8");
  if (body.byteLength > INTERFACE_PRIVATE_ENVIRONMENT_MAX_BYTES) {
    throw new Error(`application private configuration '${interfaceId}' exceeds ${INTERFACE_PRIVATE_ENVIRONMENT_MAX_BYTES} bytes`);
  }
  if (!current.present && values.size > 0) await validateInterfacePrivateDirectory(interfacePrivateDirectory, true);
  if (current.present) await validateInterfacePrivateDirectory(interfacePrivateDirectory, false);
  const file = path.join(interfacePrivateDirectory, INTERFACE_PRIVATE_ENVIRONMENT_FILE);
  if (values.size === 0) {
    if (!current.present) throw new Error(`application private configuration '${interfaceId}' has no saved values to clear`);
    const unchanged = await readInterfacePrivateEnvironmentDocument(interfacePrivateDirectory, interfaceId, declaredNames);
    if (unchanged.digest !== current.digest) throw new Error(`application private configuration '${interfaceId}' changed before removal`);
    await fs.unlink(file);
    await syncDirectory(interfacePrivateDirectory);
    return { interfaceId, documentStatus: "absent", documentDigest: null, configuredFields: 0 };
  }
  await atomicWritePrivateEnvironment(file, body, current.present ? current.digest : null, interfaceId, declaredNames);
  const committed = await readInterfacePrivateEnvironmentDocument(interfacePrivateDirectory, interfaceId, declaredNames);
  return {
    interfaceId,
    documentStatus: "valid",
    documentDigest: committed.digest,
    configuredFields: committed.values.size,
  };
}

async function writeInterfacePrivateValues(interfacePrivateDirectory, interfaceId, declaredNames, changes) {
  const run = applicationPrivateConfigurationMutationTail.then(() => (
    writeInterfacePrivateValuesUnlocked(interfacePrivateDirectory, interfaceId, declaredNames, changes)
  ));
  applicationPrivateConfigurationMutationTail = run.then(() => undefined, () => undefined);
  return run;
}

async function writeInterfacePrivateValuesUnlocked(interfacePrivateDirectory, interfaceId, declaredNames, changes) {
  if (!isPlainRecord(changes)) throw new Error(`interface '${interfaceId}' private values must be an object of names`);
  const updates = new Map();
  for (const [name, value] of Object.entries(changes)) {
    if (!declaredNames.has(name)) throw new Error(`interface '${interfaceId}' does not declare private value '${name}'`);
    if (value !== null && (typeof value !== "string" || value.length === 0 || value.includes("\0") || Buffer.byteLength(value) > INTERFACE_PRIVATE_ENVIRONMENT_MAX_BYTES)) {
      throw new Error(`interface '${interfaceId}' private value '${name}' must be a non-empty string or null`);
    }
    updates.set(name, value);
  }
  await validateInterfacePrivateDirectory(interfacePrivateDirectory, true);
  const current = await readInterfacePrivateEnvironmentDocument(interfacePrivateDirectory, interfaceId, declaredNames);
  const values = new Map(current.values);
  for (const [name, value] of updates) {
    if (value === null) values.delete(name);
    else values.set(name, value);
  }
  const file = path.join(interfacePrivateDirectory, INTERFACE_PRIVATE_ENVIRONMENT_FILE);
  if (values.size === 0) {
    if (current.present) {
      await fs.unlink(file);
      await syncDirectory(interfacePrivateDirectory);
    }
    return new Map();
  }
  const document = {
    schema_version: 1,
    interface: interfaceId,
    values: Object.fromEntries([...values.entries()].sort(([left], [right]) => left.localeCompare(right))),
  };
  const body = Buffer.from(`${JSON.stringify(document, null, 2)}\n`, "utf8");
  if (body.byteLength > INTERFACE_PRIVATE_ENVIRONMENT_MAX_BYTES) {
    throw new Error(`interface private environment '${interfaceId}' exceeds ${INTERFACE_PRIVATE_ENVIRONMENT_MAX_BYTES} bytes`);
  }
  await atomicWritePrivateEnvironment(file, body, current.present ? current.digest : null, interfaceId, declaredNames);
  return (await readInterfacePrivateEnvironmentDocument(interfacePrivateDirectory, interfaceId, declaredNames)).values;
}

async function atomicWritePrivateEnvironment(file, body, expectedDigest, interfaceId, declaredNames) {
  const directory = path.dirname(file);
  const temporary = path.join(directory, `.${INTERFACE_PRIVATE_ENVIRONMENT_FILE}.${process.pid}.${crypto.randomUUID()}.tmp`);
  let handle;
  try {
    handle = await fs.open(temporary, "wx", 0o600);
    await handle.writeFile(body);
    await handle.sync();
    await handle.close();
    handle = null;
    const temporaryStat = await fs.lstat(temporary);
    validateInterfacePrivateFileStat(temporary, temporaryStat);
    const current = await readInterfacePrivateEnvironmentDocument(directory, interfaceId, declaredNames);
    if ((current.present ? current.digest : null) !== expectedDigest) {
      throw new Error(`application private configuration '${interfaceId}' changed before commit`);
    }
    await fs.rename(temporary, file);
    await syncDirectory(directory);
  } finally {
    await handle?.close().catch(() => {});
    await fs.rm(temporary, { force: true }).catch(() => {});
  }
}

function applicationLogoFileName(value) {
  return typeof value === "string"
    && /^[A-Za-z0-9][A-Za-z0-9._-]*\.(?:png|svg|webp)$/.test(value);
}

async function readApplicationLogoAsset(applicationRoot, relative, label) {
  if (!applicationLogoFileName(relative)) throw new Error(`${label} has an invalid package-owned file name`);
  const root = await fs.realpath(applicationRoot);
  const file = path.join(root, relative);
  const stat = await fs.lstat(file);
  if (!stat.isFile() || stat.isSymbolicLink()) throw new Error(`${label} is not a regular package-owned file: ${file}`);
  const resolved = await fs.realpath(file);
  if (!isPathInside(root, resolved)) throw new Error(`${label} escapes its application package: ${file}`);
  if (stat.size <= 0 || stat.size > APPLICATION_LOGO_MAX_BYTES) {
    throw new Error(`${label} size is outside 1..${APPLICATION_LOGO_MAX_BYTES} bytes: ${file}`);
  }
  const extension = path.extname(relative).toLowerCase();
  const mime = extension === ".svg" ? "image/svg+xml" : extension === ".png" ? "image/png" : "image/webp";
  const bytes = await fs.readFile(resolved);
  if (bytes.byteLength !== stat.size) throw new Error(`${label} changed while it was read: ${file}`);
  return `data:${mime};base64,${bytes.toString("base64")}`;
}
