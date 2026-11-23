module.exports = { configurationProjection };

const path = require("node:path");
const { readClientAgentCatalog } = require("../clientAgentCatalog.cjs");
const { readLocalPackageCatalog } = require("../localPackages.cjs");
const { inspectWorkspaceSetupState } = require("../workspaceSetup.cjs");
const { readApplicationPackageCatalog } = require("./applicationCatalog.cjs");
const { DESCRIPTOR_REGISTRIES } = require("./constants.cjs");
const { readOperatorChatProtocol, readProviderInterfaceDescriptors } = require("./providerDescriptors.cjs");
const { readEeInstanceRecords, readHostRootRefs, readRunRecords, validateEePolicyProjectionSnapshot } = require("./registryRows.cjs");
const { normalizeRuntimeLaunchEnvironment } = require("./sidecars.cjs");
const { arrayValue, stringValue } = require("./values.cjs");

function configurationProjection(store) {
  let configurationProjectionGeneration = 1;
  let configurationProjectionBuild = null;
  let policyProjectionBuild = null;
  let policyProjectionGeneration = 1;
  let packageCatalogBuild = null;
  let packageCatalogGeneration = 1;
  let packageOwnerScope = null;
  let packageOwners = null;
  let applicationCatalogBuild = null;
  let applicationCatalogGeneration = 1;
  let applicationOwnerScope = null;
  let applicationOwners = null;
  const runRecordCache = new Map();
  const packageProjectionStates = new WeakMap();

  async function cachedPackageCatalog(repoRoot) {
    const scope = store.fileProjectionScope(repoRoot);
    for (;;) {
      if (scope !== store.fileProjectionScope(repoRoot)) throw new Error("Projection Home changed during package read");
      let build = packageCatalogBuild;
      if (!build || build.scope !== scope) {
        if (packageOwnerScope !== scope) packageOwners = null;
        build = { scope, generation: packageCatalogGeneration, promise: readLocalPackageCatalog({
          sourceRegistryRoot: store.resolveLocalPackageRegistry(),
          activeRegistryRoot: path.join(store.octopusHome(), "registry"),
          libraryRoot: store.resolveLocalPackageLibrary(),
          stateRoot: path.join(store.octopusHome(), "client", "package-state"),
          publication: store.localPackagePublication(),
        }) };
        packageCatalogBuild = build;
      }
      try {
        const catalog = await build.promise;
        if (scope !== store.fileProjectionScope(repoRoot)) throw new Error("Projection Home changed during package read");
        packageOwners = new Set(arrayValue(catalog.packages).flatMap((entry) => (
          arrayValue(entry.members)
            .map((member) => `${member.registry_kind}/${member.owner}`)
        )));
        packageOwnerScope = scope;
        if (build.generation === packageCatalogGeneration) return structuredClone(catalog);
      } catch (error) {
        if (build.generation === packageCatalogGeneration) {
          if (packageCatalogBuild === build) packageCatalogBuild = null;
          throw error;
        }
      }
      if (packageCatalogBuild === build) packageCatalogBuild = null;
    }
  }

  function packageInputsChanged(filename) {
    if (filename === null || filename === undefined) return true;
    const segments = String(filename).split(/[\\/]/).filter(Boolean);
    if (!segments.length) return true;
    if (segments[0] === "client" && segments[1] === "package-state") {
      if (segments.length === 3 && ["maintenance-lock.sqlite", "maintenance-lock.sqlite-journal", "maintenance-lock.sqlite-wal", "maintenance-lock.sqlite-shm"].includes(segments[2])) {
        return false;
      }
      return true;
    }
    if (segments[0] !== "registry") return false;
    if (segments.length < 3 || !packageOwners
      || packageOwnerScope !== store.fileProjectionScope(store.resolveRepoRoot())
      || path.resolve(store.resolveLocalPackageRegistry()) === path.join(store.octopusHome(), "registry")) return true;
    return packageOwners.has(`${segments[1]}/${segments[2]}`);
  }

  function applicationInputsChanged(filename) {
    if (packageInputsChanged(filename)) return true;
    const segments = String(filename).split(/[\\/]/).filter(Boolean);
    if (segments[0] !== "registry" || segments[1] !== "interface") return false;
    if (segments.length < 3 || !applicationOwners
      || applicationOwnerScope !== store.fileProjectionScope(store.resolveRepoRoot())) return true;
    return segments[3] === "application.json" || applicationOwners.has(segments[2]);
  }

  async function cachedApplicationPackageCatalog(configuration) {
    const scope = store.fileProjectionScope(configuration.repoRoot);
    for (;;) {
      if (scope !== store.fileProjectionScope(configuration.repoRoot)) {
        throw new Error("Projection Home changed during application package read");
      }
      let build = applicationCatalogBuild;
      if (!build || build.scope !== scope) {
        if (applicationOwnerScope !== scope) applicationOwners = null;
        build = {
          scope,
          generation: applicationCatalogGeneration,
          promise: (async () => {
            const warnings = [];
            const catalog = await readApplicationPackageCatalog(
              configuration.registryRoot,
              configuration.localPackageRegistryRoot,
              warnings,
            );
            return { catalog, warnings };
          })(),
        };
        applicationCatalogBuild = build;
      }
      try {
        const result = await build.promise;
        if (scope !== store.fileProjectionScope(configuration.repoRoot)) {
          throw new Error("Projection Home changed during application package read");
        }
        applicationOwners = new Set([
          ...arrayValue(result.catalog.active),
          ...arrayValue(result.catalog.sources),
        ].flatMap((entry) => arrayValue(entry.manifest?.interfaces)));
        applicationOwnerScope = scope;
        if (build.generation === applicationCatalogGeneration) {
          return {
            catalog: structuredClone(result.catalog),
            warnings: [...result.warnings],
          };
        }
      } catch (error) {
        if (build.generation === applicationCatalogGeneration) {
          if (applicationCatalogBuild === build) applicationCatalogBuild = null;
          throw error;
        }
      }
      if (applicationCatalogBuild === build) applicationCatalogBuild = null;
    }
  }

  async function cachedConfigurationProjection(repoRoot) {
    const scope = store.fileProjectionScope(repoRoot);
    for (;;) {
      if (scope !== store.fileProjectionScope(repoRoot)) throw new Error("Projection Home changed during configuration read");
      let build = configurationProjectionBuild;
      if (!build || build.scope !== scope) {
        build = { scope, generation: configurationProjectionGeneration, promise: buildConfigurationProjection(repoRoot) };
        configurationProjectionBuild = build;
      }
      try {
        const projection = await build.promise;
        if (scope !== store.fileProjectionScope(repoRoot)) throw new Error("Projection Home changed during configuration read");
        if (build.generation === configurationProjectionGeneration) return projection;
      } catch (error) {
        if (build.generation === configurationProjectionGeneration) {
          if (configurationProjectionBuild === build) configurationProjectionBuild = null;
          throw error;
        }
      }
      if (configurationProjectionBuild === build) configurationProjectionBuild = null;
    }
  }

  async function buildConfigurationProjection(repoRoot) {
    const generation = configurationProjectionGeneration;
    const scope = store.fileProjectionScope(repoRoot);
    const warnings = store.projectionWatchWarnings();
    const homeRoot = store.octopusHome();
    const durableRoot = store.resolveDurableRoot();
    const registryRoot = path.join(homeRoot, "registry");
    const stateRoot = path.join(homeRoot, "state");
    const runRecordsRoot = path.join(stateRoot, "runs");
    let hostRootRefs = {};
    let hostManifestError = "";
    try {
      hostRootRefs = await readHostRootRefs(homeRoot);
    } catch (error) {
      hostManifestError = String(error?.message ?? error);
      warnings.push(hostManifestError);
    }
    let launchEnvironment = null;
    try {
      launchEnvironment = normalizeRuntimeLaunchEnvironment(store.runtimeLaunchEnvironment());
    } catch (error) {
      warnings.push(`Managed runtime launch environment could not be verified: ${String(error?.message ?? error)}`);
    }
    const sourceRegistryRoot = store.resolveSourceRegistry();
    const localPackageRegistryRoot = store.resolveLocalPackageRegistry();
    const localPackageLibraryRoot = store.resolveLocalPackageLibrary();
    const descriptorWarnings = [];
    const [descriptorRead, policyRead] = await Promise.allSettled([
      Promise.all(DESCRIPTOR_REGISTRIES.map(async (registry) => [
        registry.kind,
        await store.readDescriptorRows(registryRoot, registry, descriptorWarnings, launchEnvironment),
      ])),
      cachedEePolicyProjection(),
    ]);
    if (descriptorRead.status === "rejected") throw descriptorRead.reason;
    const descriptors = Object.fromEntries(descriptorRead.value);
    warnings.push(...descriptorWarnings);
    const packageWarningOffset = warnings.length;
    let clientAgentCatalog = null;
    let clientAgentCatalogError = "";
    try {
      clientAgentCatalog = await readClientAgentCatalog(homeRoot);
      if (!clientAgentCatalog.exists) {
        clientAgentCatalogError = `Client Agent catalog is absent: ${clientAgentCatalog.file}`;
        warnings.push(clientAgentCatalogError);
      }
    } catch (error) {
      clientAgentCatalogError = String(error?.message ?? error);
      warnings.push(clientAgentCatalogError);
    }
    const applicationWarningOffset = warnings.length;
    const descriptorIndexes = Object.fromEntries(Object.entries(descriptors).map(([kind, rows]) => [
      kind,
      new Map(rows.flatMap((row) => {
        const identities = new Set([
          stringValue(row.id),
          stringValue(row.rule_id),
          stringValue(row.name),
          stringValue(row.registry_dir),
        ].filter(Boolean));
        return [...identities].map((identity) => [identity, row]);
      })),
    ]));
    const eeInstances = await readEeInstanceRecords(path.join(stateRoot, "ee_instances"), warnings);
    let eePolicyProjection = { environments: [], issues: [] };
    let eePolicyProjectionError = "";
    try {
      if (policyRead.status === "rejected") throw policyRead.reason;
      eePolicyProjection = policyRead.value;
      warnings.push(...eePolicyProjection.issues);
    } catch (error) {
      eePolicyProjectionError = String(error?.message ?? error);
      warnings.push(eePolicyProjectionError);
    }
    const operatorChatProtocol = await readOperatorChatProtocol(registryRoot);
    const configuration = {
      repoRoot,
      octopusHome: homeRoot,
      registryRoot,
      sourceRegistryRoot,
      localPackageRegistryRoot,
      localPackageLibraryRoot,
      stateRoot,
      runRecordsRoot,
      durableRoot,
      descriptors,
      clientAgentCatalog,
      clientAgentCatalogError,
      descriptorIndexes,
      operatorChatProtocol,
      eeInstances,
      eePolicyProjection,
      eePolicyProjectionError,
      hostRootRefs,
      fullAccessRoot: path.parse(homeRoot).root,
      hostManifestError,
      warnings,
    };
    packageProjectionStates.set(configuration, {
      repoRoot,
      scope,
      generation,
      packageWarningOffset,
      applicationWarningOffset,
      promise: null,
    });
    return configuration;
  }

  async function readPackageInventory(configuration) {
    const [packageRead, applicationRead] = await Promise.allSettled([
      cachedPackageCatalog(configuration.repoRoot),
      cachedApplicationPackageCatalog(configuration),
    ]);
    if (packageRead.status === "rejected") throw packageRead.reason;
    if (applicationRead.status === "rejected") throw applicationRead.reason;
    const localPackages = packageRead.value;
    const applicationPackages = applicationRead.value.catalog;
    const setupSourceProviders = await readProviderInterfaceDescriptors(configuration.localPackageRegistryRoot);
    const setupInstalledProviders = await readProviderInterfaceDescriptors(configuration.registryRoot);
    const applicationMetadataForProvider = (providerId, installed) => {
      const primary = installed ? applicationPackages.active : applicationPackages.sources;
      const fallback = installed ? applicationPackages.sources : applicationPackages.active;
      const match = (Array.isArray(primary) ? primary.find((entry) => entry?.manifest?.id === providerId) : null)
        ?? (Array.isArray(fallback) ? fallback.find((entry) => entry?.manifest?.id === providerId) : null);
      if (!match) return {};
      const disclosure = Array.isArray(match.manifest?.connection?.disclosure)
        ? match.manifest.connection.disclosure.find((entry) => typeof entry === "string" && entry.trim())?.trim() ?? ""
        : "";
      return {
        ...(match.logo_data_url ? { logoDataUrl: match.logo_data_url } : {}),
        ...(match.logo_dark_data_url ? { logoDarkDataUrl: match.logo_dark_data_url } : {}),
        ...(disclosure ? { connectionSummary: disclosure } : {}),
      };
    };
    for (const pack of localPackages.packs) {
      const home = pack.workspace_setup?.home;
      const defaultId = home?.provider_interface_id;
      if (!defaultId) continue;
      const resolvePackProvider = (providerId) => {
        const owner = pack.components.find((component) => component.product_id === providerId);
        const installed = owner?.materialized === true || owner?.partially_materialized === true;
        const providers = installed ? setupInstalledProviders : setupSourceProviders;
        const provider = providers.find((candidate) => candidate.id === providerId);
        return provider ? { ...provider, ...applicationMetadataForProvider(providerId, installed) } : null;
      };
      const providerIds = Array.isArray(home.provider_interface_ids) && home.provider_interface_ids.length
        ? home.provider_interface_ids
        : [defaultId];
      pack.setup_providers = providerIds.map(resolvePackProvider).filter(Boolean);
      pack.setup_provider = pack.setup_providers.find((provider) => provider.id === defaultId) ?? null;
      const voiceProviderId = home.voice?.provider_interface_id;
      if (voiceProviderId) {
        pack.setup_voice_provider = resolvePackProvider(voiceProviderId);
      }
    }
    return {
      localPackages,
      applicationPackages,
      applicationWarnings: applicationRead.value.warnings,
    };
  }

  async function withFileProjectionPackages(projection, includeLedger = false) {
    for (;;) {
      const state = packageProjectionStates.get(projection);
      if (!state) throw new Error("File projection has no configuration package source");
      if (state.scope !== store.fileProjectionScope(state.repoRoot)) throw new Error("Projection Home changed during package read");
      if (state.generation !== configurationProjectionGeneration) {
        projection = await store.cachedFileProjection(state.repoRoot, includeLedger);
        continue;
      }
      if (projection.localPackages && projection.applicationPackages) return projection;
      const promise = state.promise ??= readPackageInventory(projection);
      let inventory;
      try {
        inventory = await promise;
      } catch (error) {
        if (state.promise === promise) state.promise = null;
        if (state.generation === configurationProjectionGeneration) throw error;
      }
      if (state.scope !== store.fileProjectionScope(state.repoRoot)) throw new Error("Projection Home changed during package read");
      if (state.generation !== configurationProjectionGeneration) {
        projection = await store.cachedFileProjection(state.repoRoot, includeLedger);
        continue;
      }
      if (!projection.localPackages) {
        projection.localPackages = inventory.localPackages;
        projection.applicationPackages = inventory.applicationPackages;
        projection.warnings.splice(state.applicationWarningOffset, 0, ...inventory.applicationWarnings);
        projection.warnings.splice(state.packageWarningOffset, 0, ...arrayValue(inventory.localPackages.errors));
        projection.structureMemo.clear();
      }
      return projection;
    }
  }

  async function buildFileProjection(repoRoot) {
    const configuration = await cachedConfigurationProjection(repoRoot);
    const warnings = [...configuration.warnings];
    const runRecordProjection = await readRunRecords(configuration.runRecordsRoot, warnings, runRecordCache);
    let workspaceSetup;
    try {
      workspaceSetup = await (store.options.inspectWorkspaceSetupState ?? inspectWorkspaceSetupState)({ octopusHomeRoot: configuration.octopusHome, registryRoot: configuration.registryRoot });
      warnings.push(...arrayValue(workspaceSetup.errors));
    } catch (error) {
      const message = String(error?.message ?? error);
      workspaceSetup = { status: "degraded", receipt: null, errors: [message] };
      warnings.push(message);
    }
    const workerEnvironments = await store.readWorkerEnvironmentRecords(configuration.octopusHome, warnings, configuration.descriptorIndexes.worker_environment_kind);
    const cachedLedger = store.ledgerProjectionSnapshot?.durableRoot === configuration.durableRoot
      ? store.ledgerProjectionSnapshot
      : null;
    const projection = {
      ...configuration,
      workspaceSetup,
      workerEnvironments,
      generatedAt: new Date().toISOString(),
      generation: store.projectionGeneration,
      ledgerRows: cachedLedger?.rows ?? [],
      ledgerRowsByRunSources: cachedLedger?.rowsByRunSources ?? [],
      invalidLedgerRows: cachedLedger?.invalidRows ?? [],
      ledgerRevision: cachedLedger?.revision ?? "",
      structureMemo: new Map(),
      ledgerMemo: new Map(),
      operatorChatsListCache: null,
      operatorActivityCache: null,
      topologySnapshotCache: null,
      operatorChatMessagesCache: new Map(),
      operatorChatRunIndexCache: null,
      operatorChatStatusCursor: -1,
      operatorChatStatusInitialized: false,
      operatorChatStatusRefreshTail: Promise.resolve(),
      operatorChatTurnStateCache: new Map(),
      runRecords: runRecordProjection.records,
      invalidRunRecords: runRecordProjection.invalidRecords,
      warnings,
    };
    packageProjectionStates.set(projection, packageProjectionStates.get(configuration));
    return projection;
  }

  async function cachedEePolicyProjection() {
    if (typeof store.options.projectEePolicies !== "function") {
      throw new Error("canonical EE policy projector is unavailable");
    }
    while (true) {
      let build = policyProjectionBuild;
      if (!build) {
        const generation = policyProjectionGeneration;
        const promise = store.options.projectEePolicies().then(validateEePolicyProjectionSnapshot);
        build = { generation, promise };
        policyProjectionBuild = build;
      }
      try {
        const snapshot = await build.promise;
        if (build.generation === policyProjectionGeneration) return snapshot;
      } catch (error) {
        if (build.generation === policyProjectionGeneration) {
          if (policyProjectionBuild === build) policyProjectionBuild = null;
          throw error;
        }
      }
      if (policyProjectionBuild === build) policyProjectionBuild = null;
    }
  }

  function invalidateConfigurationProjection(configurationChanged, policyChanged, filename) {
    if (configurationChanged) configurationProjectionGeneration += 1;
    if (configurationChanged && packageInputsChanged(filename)) packageCatalogGeneration += 1;
    if (configurationChanged && applicationInputsChanged(filename)) applicationCatalogGeneration += 1;
    if (policyChanged) policyProjectionGeneration += 1;
  }

  function closeConfigurationProjection() {
    configurationProjectionBuild = null;
    packageCatalogBuild = null;
    packageOwners = null;
    packageOwnerScope = null;
    applicationCatalogBuild = null;
    applicationOwners = null;
    applicationOwnerScope = null;
    runRecordCache.clear();
  }

  return { buildConfigurationProjection, buildFileProjection, cachedConfigurationProjection, cachedEePolicyProjection, closeConfigurationProjection, invalidateConfigurationProjection, withFileProjectionPackages };
}
