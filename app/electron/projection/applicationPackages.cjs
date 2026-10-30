module.exports = { applicationDirectoryDigest, applicationHasAnyAuthoredBinding, applicationIsConnectedToAuthoredRun, applicationTaskCapabilityReferences, assertApplicationPackageUnreferenced, installOctopusOsApplicationPackage, optionalApplicationPackageSource, projectCapabilityChanges, readConnectedApplicationIdsByRun, removeOctopusOsApplicationPackage, reportApplicationProgress, stageApplicationPackage, taskBaseAuthoringProfile, taskCheckpointCarriesProviderContext, taskProviderContextArtifactIds };

const fs = require("node:fs/promises");
const crypto = require("node:crypto");
const path = require("node:path");
const { readApplicationPackageSource, scanApplicationPackageRoot, snapshotApplicationPackageDirectory } = require("./applicationCatalog.cjs");
const { readOptionalRegularJsonDocument, readRegularJsonDocument, stableJson } = require("./jsonFiles.cjs");
const { projectedRunInterfaceIds } = require("./registryRows.cjs");
const { exactStringReferencePaths, registryControlJsonFiles } = require("./taskDeletion.cjs");
const { arrayValue, createJsonFile, interfaceBindingId, isPlainRecord, objectOrEmpty, requiredAbsolutePath, requiredApplicationOperationId, requiredArtifactBindingId, requiredBoolean, requiredDigest, requiredSafeRegistryId, stringValue, syncDirectory } = require("./values.cjs");

async function readConnectedApplicationIdsByRun(projection, scope) {
  const scopeEeIds = new Set(arrayValue(scope.environments).map(row => stringValue(row.ee_id)).filter(Boolean));
  const profiles = new Map(arrayValue(scope.client_runs).map(row => [stringValue(row.run_id), objectOrEmpty(row.authoring_profile)]));
  const runs = arrayValue(projection.runRecords).filter(row => scopeEeIds.has(stringValue(row.ee_id))).map(record => ({
    id: stringValue(record.run_id),
    interfaces: projectedRunInterfaceIds(record),
    artifacts: new Set(arrayValue(record.checkpoints).filter(checkpoint => checkpoint.status === "projected")
      .flatMap(checkpoint => arrayValue(checkpoint.artifact_binding_ids).map(stringValue).filter(Boolean))),
    profile: profiles.get(stringValue(record.run_id)) ?? {},
  }));
  const manifests = new Map(arrayValue(projection.descriptors?.interface)
    .filter(row => isPlainRecord(row.application_manifest))
    .map(row => [stringValue(row.application_manifest.id), row.application_manifest]));
  const sourceOwners = new Set(runs.flatMap(run => [...run.interfaces]).filter(id => !manifests.has(id)));
  const sources = await scanApplicationPackageRoot(projection.localPackageRegistryRoot, "local", [], false, sourceOwners);
  for (const source of sources) manifests.set(stringValue(source.manifest.id), source.manifest);
  const connected = new Map();
  for (const [applicationId, manifest] of manifests) {
    const interfaces = arrayValue(manifest.interfaces).map(stringValue).filter(Boolean);
    const artifacts = arrayValue(manifest.artifact_bindings).map(binding => stringValue(binding.artifact_id)).filter(Boolean);
    for (const run of runs) {
      if (!run.id || !interfaces.every(id => run.interfaces.has(id)) || !artifacts.every(id => run.artifacts.has(id))
        || !applicationTaskCapabilityReferences(run.profile, manifest).complete) continue;
      const ids = connected.get(run.id) ?? [];
      ids.push(applicationId);
      connected.set(run.id, ids);
    }
  }
  for (const ids of connected.values()) ids.sort();
  return connected;
}

async function installOctopusOsApplicationPackage(
  registryRoot,
  octopusHomeRoot,
  sourceRegistryRootValue,
  request,
  onProgress,
) {
  const applicationId = requiredSafeRegistryId(request?.applicationId, "applicationId");
  const operationId = requiredApplicationOperationId(request?.operationId);
  const expectedSourceDigest = requiredDigest(request?.expectedSourceDigest, "expectedSourceDigest");
  reportApplicationProgress(onProgress, operationId, applicationId, "install", "preflight", 0);
  const activeInterfaceRoot = await fs.realpath(path.join(registryRoot, "interface"));
  const sourceRegistryRoot = requiredAbsolutePath(sourceRegistryRootValue, "application source registry");
  const archiveRegistryRoot = path.join(octopusHomeRoot, "client", "application-packages");
  const bundled = await optionalApplicationPackageSource(sourceRegistryRoot, applicationId, "bundled");
  const archived = await optionalApplicationPackageSource(archiveRegistryRoot, applicationId, "archived");
  if (bundled && archived) {
    throw new Error(`application '${applicationId}' has conflicting bundled and archived package sources`);
  }
  const source = bundled ?? archived;
  if (!source) throw new Error(`application '${applicationId}' has no verified package source`);
  if (source.manifest.interfaces.length !== 1) {
    throw new Error(`application '${applicationId}' spans multiple interface folders and has no atomic install contract`);
  }
  if (source.package_digest !== expectedSourceDigest) {
    throw new Error(`application '${applicationId}' source changed before installation`);
  }
  const targetDirectories = source.manifest.interfaces.map((interfaceId) => ({
    interfaceId,
    path: path.join(activeInterfaceRoot, interfaceId),
  }));
  const existing = [];
  for (const target of targetDirectories) {
    const stat = await fs.lstat(target.path).catch((error) => error?.code === "ENOENT" ? null : Promise.reject(error));
    if (stat) existing.push(target.interfaceId);
  }
  if (existing.length) {
    throw new Error(`application '${applicationId}' cannot install over existing interface folders: ${existing.join(", ")}`);
  }
  const transactionParent = path.join(octopusHomeRoot, ".client-application-transactions");
  const transactionRoot = path.join(transactionParent, operationId);
  await fs.mkdir(transactionParent, { recursive: true, mode: 0o700 });
  await fs.mkdir(transactionRoot, { mode: 0o700 });
  await createJsonFile(path.join(transactionRoot, "manifest.json"), {
    schema_version: 1,
    operation: "install",
    operation_id: operationId,
    application_id: applicationId,
    source_kind: source.source_kind,
    source_digest: source.package_digest,
    interfaces: source.manifest.interfaces,
  });
  const installed = [];
  let preserveTransaction = false;
  const stagedInterfaceRoot = path.join(transactionRoot, "staged", "interface");
  try {
    let commitSource = source;
    if (source.source_kind === "bundled") {
      await fs.mkdir(stagedInterfaceRoot, { recursive: true, mode: 0o700 });
      await stageApplicationPackage(
        source,
        stagedInterfaceRoot,
        (copied, total) => reportApplicationProgress(
          onProgress,
          operationId,
          applicationId,
          "install",
          "copying",
          total ? Math.round(10 + copied / total * 70) : 80,
        ),
      );
      commitSource = await readApplicationPackageSource(
        stagedInterfaceRoot,
        applicationId,
        "staged",
      );
      if (commitSource.package_digest !== source.package_digest) {
        throw new Error(`application '${applicationId}' staged package does not match its verified source`);
      }
      const refreshed = await readApplicationPackageSource(
        source.interface_root,
        applicationId,
        source.source_kind,
      );
      if (refreshed.package_digest !== source.package_digest) {
        throw new Error(`application '${applicationId}' source changed while installation was staged`);
      }
    }
    for (const [index, interfaceId] of commitSource.manifest.interfaces.entries()) {
      const sourceDirectory = path.join(commitSource.interface_root, interfaceId);
      const targetDirectory = path.join(activeInterfaceRoot, interfaceId);
      if (await fs.lstat(targetDirectory).catch((error) => error?.code === "ENOENT" ? null : Promise.reject(error))) {
        throw new Error(`application '${applicationId}' target '${interfaceId}' appeared during installation`);
      }
      await fs.rename(sourceDirectory, targetDirectory);
      const entry = { source: sourceDirectory, target: targetDirectory };
      installed.push(entry);
      reportApplicationProgress(
        onProgress,
        operationId,
        applicationId,
        "install",
        "installing",
        Math.round(82 + (index + 1) / commitSource.manifest.interfaces.length * 16),
      );
    }
    const installedPackage = await readApplicationPackageSource(
      activeInterfaceRoot,
      applicationId,
      "installed",
    );
    if (installedPackage.package_digest !== source.package_digest) {
      throw new Error(`application '${applicationId}' installed package does not match its verified source`);
    }
    await syncDirectory(activeInterfaceRoot);
    if (source.source_kind === "archived") await syncDirectory(source.interface_root);
  } catch (error) {
    const rollbackFailures = [];
    for (const entry of installed.reverse()) {
      if (source.source_kind === "archived") {
        await fs.rename(entry.target, entry.source).catch((failure) => rollbackFailures.push(failure));
      } else {
        await fs.rm(entry.target, { recursive: true, force: true }).catch((failure) => rollbackFailures.push(failure));
      }
    }
    await syncDirectory(activeInterfaceRoot).catch((failure) => rollbackFailures.push(failure));
    if (source.source_kind === "archived") {
      await syncDirectory(source.interface_root).catch((failure) => rollbackFailures.push(failure));
    }
    if (rollbackFailures.length) {
      preserveTransaction = true;
      throw new Error(`${error.message}; application install rollback remains in '${transactionRoot}': ${rollbackFailures[0].message}`);
    }
    throw error;
  } finally {
    if (!preserveTransaction) {
      await fs.rm(transactionRoot, { recursive: true, force: true }).catch(() => {});
      await fs.rmdir(transactionParent).catch(() => {});
    }
  }
  reportApplicationProgress(onProgress, operationId, applicationId, "install", "complete", 100);
  return {
    action: "install_application",
    capabilityId: applicationId,
    installed: true,
    changed: true,
    sourceKind: source.source_kind,
  };
}

async function removeOctopusOsApplicationPackage(
  registryRoot,
  octopusHomeRoot,
  sourceRegistryRootValue,
  _scope,
  request,
  onProgress,
) {
  const applicationId = requiredSafeRegistryId(request?.applicationId, "applicationId");
  const operationId = requiredApplicationOperationId(request?.operationId);
  const expectedInstalledDigest = requiredDigest(request?.expectedInstalledDigest, "expectedInstalledDigest");
  reportApplicationProgress(onProgress, operationId, applicationId, "remove", "preflight", 0);
  const activeInterfaceRoot = await fs.realpath(path.join(registryRoot, "interface"));
  const active = await readApplicationPackageSource(activeInterfaceRoot, applicationId, "active");
  if (active.manifest.interfaces.length !== 1) {
    throw new Error(`application '${applicationId}' spans multiple interface folders and has no atomic removal contract`);
  }
  if (active.package_digest !== expectedInstalledDigest) {
    throw new Error(`application '${applicationId}' changed before removal`);
  }
  const sourceRegistryRoot = requiredAbsolutePath(sourceRegistryRootValue, "application source registry");
  if (await optionalApplicationPackageSource(sourceRegistryRoot, applicationId, "bundled")) {
    throw new Error(`application '${applicationId}' is supplied by the Octopus installation and cannot be removed independently`);
  }
  const exactInterfaces = new Set(active.manifest.interfaces);
  const excludedDirectories = new Set(active.manifest.interfaces.map((interfaceId) => (
    path.join(activeInterfaceRoot, interfaceId)
  )));
  await assertApplicationPackageUnreferenced(
    await fs.realpath(registryRoot),
    octopusHomeRoot,
    excludedDirectories,
    exactInterfaces,
    applicationId,
  );
  const archiveRegistryRoot = path.join(octopusHomeRoot, "client", "application-packages");
  const archiveInterfaceRoot = path.join(archiveRegistryRoot, "interface");
  await fs.mkdir(archiveInterfaceRoot, { recursive: true, mode: 0o700 });
  for (const interfaceId of active.manifest.interfaces) {
    const archivedPath = path.join(archiveInterfaceRoot, interfaceId);
    if (await fs.lstat(archivedPath).catch((error) => error?.code === "ENOENT" ? null : Promise.reject(error))) {
      throw new Error(`application '${applicationId}' already has an archived '${interfaceId}' package folder`);
    }
  }
  const transactionParent = path.join(octopusHomeRoot, ".client-application-transactions");
  const transactionRoot = path.join(transactionParent, operationId);
  await fs.mkdir(transactionParent, { recursive: true, mode: 0o700 });
  await fs.mkdir(transactionRoot, { mode: 0o700 });
  await createJsonFile(path.join(transactionRoot, "manifest.json"), {
    schema_version: 1,
    operation: "remove",
    operation_id: operationId,
    application_id: applicationId,
    installed_digest: active.package_digest,
    interfaces: active.manifest.interfaces,
  });
  const moved = [];
  let preserveTransaction = false;
  try {
    for (const [index, interfaceId] of active.manifest.interfaces.entries()) {
      const source = path.join(activeInterfaceRoot, interfaceId);
      const target = path.join(archiveInterfaceRoot, interfaceId);
      const refreshed = await snapshotApplicationPackageDirectory(activeInterfaceRoot, interfaceId, "active");
      const expected = active.directories.find((directory) => directory.interface_id === interfaceId);
      if (!expected || applicationDirectoryDigest(refreshed) !== applicationDirectoryDigest(expected)) {
        throw new Error(`application '${applicationId}' interface '${interfaceId}' changed during removal preflight`);
      }
      await fs.rename(source, target);
      moved.push({ source, target });
      reportApplicationProgress(
        onProgress,
        operationId,
        applicationId,
        "remove",
        "removing",
        Math.round(20 + (index + 1) / active.manifest.interfaces.length * 78),
      );
    }
    const archivedPackage = await readApplicationPackageSource(
      archiveInterfaceRoot,
      applicationId,
      "archived",
    );
    if (archivedPackage.package_digest !== active.package_digest) {
      throw new Error(`application '${applicationId}' archived package does not match the installed package`);
    }
    await syncDirectory(activeInterfaceRoot);
    await syncDirectory(archiveInterfaceRoot);
  } catch (error) {
    const rollbackFailures = [];
    for (const move of moved.reverse()) {
      await fs.rename(move.target, move.source).catch((failure) => rollbackFailures.push(failure));
    }
    await syncDirectory(activeInterfaceRoot).catch((failure) => rollbackFailures.push(failure));
    await syncDirectory(archiveInterfaceRoot).catch((failure) => rollbackFailures.push(failure));
    if (rollbackFailures.length) {
      preserveTransaction = true;
      throw new Error(`${error.message}; application removal rollback remains in '${transactionRoot}': ${rollbackFailures[0].message}`);
    }
    throw error;
  } finally {
    if (!preserveTransaction) {
      await fs.rm(transactionRoot, { recursive: true, force: true }).catch(() => {});
      await fs.rmdir(transactionParent).catch(() => {});
    }
  }
  reportApplicationProgress(onProgress, operationId, applicationId, "remove", "complete", 100);
  return {
    action: "remove_application",
    capabilityId: applicationId,
    installed: false,
    changed: true,
    archivedPath: archiveRegistryRoot,
  };
}

async function optionalApplicationPackageSource(registryRoot, applicationId, sourceKind) {
  const interfaceRoot = path.join(registryRoot, "interface");
  const manifestPath = path.join(interfaceRoot, applicationId, "application.json");
  const manifest = await readOptionalRegularJsonDocument(
    manifestPath,
    `${sourceKind} application manifest ${applicationId}`,
  );
  if (!manifest) return null;
  return readApplicationPackageSource(interfaceRoot, applicationId, sourceKind, manifest);
}

async function stageApplicationPackage(source, stagedInterfaceRoot, onCopy) {
  const total = source.directories.reduce((count, directory) => count + directory.files.length, 0);
  let copied = 0;
  for (const directory of source.directories) {
    const targetRoot = path.join(stagedInterfaceRoot, directory.interface_id);
    await fs.mkdir(targetRoot, { recursive: true, mode: 0o755 });
    for (const relativeDirectory of directory.directories) {
      if (!relativeDirectory) continue;
      await fs.mkdir(path.join(targetRoot, relativeDirectory), { recursive: true, mode: 0o755 });
    }
    for (const file of directory.files) {
      const body = await fs.readFile(file.path);
      const digest = crypto.createHash("sha256").update(body).digest("hex");
      const stat = await fs.lstat(file.path);
      if (
        stat.isSymbolicLink()
        || !stat.isFile()
        || digest !== file.sha256
        || stat.size !== file.size
        || (stat.mode & 0o777) !== file.mode
      ) {
        throw new Error(`application package source changed while copying '${file.path}'`);
      }
      const target = path.join(targetRoot, file.relative_path);
      const handle = await fs.open(target, "wx", file.mode);
      try {
        await handle.writeFile(body);
        await handle.chmod(file.mode);
        await handle.sync();
      } finally {
        await handle.close();
      }
      copied += 1;
      onCopy(copied, total);
    }
    for (const relativeDirectory of [...directory.directories].sort((left, right) => right.length - left.length)) {
      await syncDirectory(path.join(targetRoot, relativeDirectory));
    }
  }
  await syncDirectory(stagedInterfaceRoot);
}

async function assertApplicationPackageUnreferenced(
  registryRoot,
  octopusHomeRoot,
  excludedDirectories,
  exactInterfaces,
  applicationId,
) {
  const registryFiles = await registryControlJsonFiles(registryRoot, excludedDirectories);
  for (const file of registryFiles) {
    const document = await readRegularJsonDocument(file, `registry control document ${file}`);
    const references = exactStringReferencePaths(document.document, exactInterfaces);
    if (references.length) {
      throw new Error(`application '${applicationId}' is still referenced by '${file}' at ${references.slice(0, 4).join(", ")}`);
    }
  }
  for (const relativeRoot of [path.join("state", "runs"), path.join("state", "ee_instances")]) {
    const root = path.join(octopusHomeRoot, relativeRoot);
    let entries;
    try {
      entries = await fs.readdir(root, { withFileTypes: true });
    } catch (error) {
      if (error?.code === "ENOENT") continue;
      throw error;
    }
    for (const entry of entries) {
      if (!entry.isFile() || !entry.name.endsWith(".json")) continue;
      const file = path.join(root, entry.name);
      const document = await readRegularJsonDocument(file, `application removal reference ${file}`);
      const references = exactStringReferencePaths(document.document, exactInterfaces);
      if (references.length) {
        throw new Error(`application '${applicationId}' is still referenced by '${file}' at ${references.slice(0, 4).join(", ")}`);
      }
    }
  }
}

function applicationDirectoryDigest(directory) {
  return crypto.createHash("sha256").update(stableJson(directory.files.map((file) => ({
    path: file.relative_path.split(path.sep).join("/"),
    sha256: file.sha256,
    mode: file.mode,
  })))).digest("hex");
}

function reportApplicationProgress(onProgress, operationId, applicationId, operation, phase, percent) {
  if (typeof onProgress !== "function") return;
  onProgress({
    operationId,
    applicationId,
    operation,
    phase,
    percent: Math.max(0, Math.min(100, Math.round(percent))),
  });
}

function taskCheckpointCarriesProviderContext(checkpoint, providerInterface) {
  return interfaceBindingId(checkpoint?.interface_binding) !== providerInterface
    && arrayValue(checkpoint?.outbound_interface_bindings).length === 1
    && interfaceBindingId(checkpoint.outbound_interface_bindings[0]) === providerInterface;
}

function taskProviderContextArtifactIds(run, activeIds, providerInterface) {
  const ids = new Set();
  let projectedInputs = 0;
  for (const checkpoint of run.checkpoints) {
    if (!activeIds.has(stringValue(checkpoint?.id))) continue;
    if (!taskCheckpointCarriesProviderContext(checkpoint, providerInterface)) continue;
    projectedInputs += 1;
    for (const binding of arrayValue(checkpoint.artifact_bindings)) {
      ids.add(requiredArtifactBindingId(binding?.id, "task context artifact binding"));
    }
  }
  if (projectedInputs === 0) throw new Error(`task '${stringValue(run.id)}' has no projected provider input`);
  return ids;
}

function applicationTaskCapabilityReferences(profile, manifest) {
  const selected = new Set(arrayValue(profile?.selected_tools).flatMap((capability) => {
    const tool = stringValue(capability?.tool);
    const interfaceId = stringValue(capability?.interface);
    const kind = stringValue(capability?.worker_environment_kind);
    return tool && interfaceId && kind ? [`${tool}\0${interfaceId}\0${kind}`] : [];
  }));
  const required = manifest.task_capabilities.tools.map((capability) => (
    `${capability.tool}\0${capability.interface}\0${capability.worker_environment_kind}`
  ));
  return { complete: required.every((capability) => selected.has(capability)) };
}

function taskBaseAuthoringProfile(scope, runProjection, taskProfile) {
  const eeId = requiredSafeRegistryId(runProjection?.ee_id, "task base authoring EE");
  const worker = requiredSafeRegistryId(
    taskProfile?.worker_choice_interface,
    "task base authoring worker",
  );
  const candidates = [...new Map([
    ...scope.taskAuthoringProfiles.filter((profile) => (
      stringValue(profile?.ee_id) === eeId
      && stringValue(profile?.worker_choice_interface) === worker
    )),
    ...scope.clientRuns.flatMap((candidate) => {
        const profile = candidate?.authoring_profile;
        return stringValue(candidate?.surface_role) === "home"
          && stringValue(profile?.ee_id) === eeId
          && stringValue(profile?.worker_choice_interface) === worker
          ? [profile]
          : [];
      }),
  ].map((profile) => [stableJson(profile), profile])).values()];
  if (candidates.length !== 1) {
    throw new Error(
      `task '${stringValue(runProjection?.run_id)}' has ${candidates.length} exact base authoring profiles for '${eeId}' and '${worker}'`,
    );
  }
  return candidates[0];
}

function applicationIsConnectedToAuthoredRun(run, manifest, profile) {
  const active = new Set(arrayValue(run?.active_checkpoints).map(stringValue).filter(Boolean));
  const projectedInterfaces = new Set();
  const projectedArtifacts = new Set();
  for (const checkpoint of arrayValue(run?.checkpoints)) {
    if (!active.has(stringValue(checkpoint?.id))) continue;
    for (const id of authoredCheckpointInterfaceIds(checkpoint)) projectedInterfaces.add(id);
    for (const binding of arrayValue(checkpoint?.artifact_bindings)) {
      const artifactId = stringValue(binding?.id);
      if (artifactId) projectedArtifacts.add(artifactId);
    }
  }
  const capabilities = applicationTaskCapabilityReferences(profile, manifest);
  return manifest.interfaces.every((interfaceId) => projectedInterfaces.has(interfaceId))
    && manifest.artifact_bindings.every((binding) => projectedArtifacts.has(binding.artifact_id))
    && capabilities.complete;
}

function authoredCheckpointInterfaceIds(checkpoint) {
  return [checkpoint?.interface_binding, ...arrayValue(checkpoint?.outbound_interface_bindings)]
    .map(interfaceBindingId)
    .filter(Boolean);
}

function applicationHasAnyAuthoredBinding(run, manifest) {
  const active = new Set(arrayValue(run?.active_checkpoints).map(stringValue).filter(Boolean));
  const interfaceIds = new Set(manifest.interfaces);
  const artifactIds = new Set(manifest.artifact_bindings.map((binding) => binding.artifact_id));
  return arrayValue(run?.checkpoints).some((checkpoint) => {
    if (!active.has(stringValue(checkpoint?.id))) return false;
    if (authoredCheckpointInterfaceIds(checkpoint).some((id) => interfaceIds.has(id))) return true;
    return arrayValue(checkpoint?.artifact_bindings).some((binding) => artifactIds.has(stringValue(binding?.id)));
  });
}

function projectCapabilityChanges(request, includeHostSlots) {
  if (!Array.isArray(request?.projects) || request.projects.length === 0) {
    throw new Error("project capability setup requires at least one changed project");
  }
  const seen = new Set();
  return request.projects.map((row, index) => {
    if (!isPlainRecord(row)) throw new Error(`projects[${index}] must be an object`);
    const eeId = requiredSafeRegistryId(row.eeId, `projects[${index}].eeId`);
    if (seen.has(eeId)) throw new Error(`project capability setup repeats EE '${eeId}'`);
    seen.add(eeId);
    const available = requiredBoolean(row.available, `projects[${index}].available`);
    const selectedByDefault = requiredBoolean(
      row.selectedByDefault,
      `projects[${index}].selectedByDefault`,
    );
    if (selectedByDefault && !available) {
      throw new Error("an unavailable capability cannot be selected by default");
    }
    return {
      eeId,
      available,
      selectedByDefault,
      expectedDigest: requiredDigest(row.expectedDigest, `projects[${index}].expectedDigest`),
      hostSlots: includeHostSlots ? row.hostSlots : undefined,
    };
  }).sort((left, right) => left.eeId.localeCompare(right.eeId));
}
