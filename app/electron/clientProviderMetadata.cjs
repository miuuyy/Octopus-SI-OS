const crypto = require("node:crypto");
const fs = require("node:fs/promises");
const path = require("node:path");
const { modelServiceTiers, validateModelServiceTier } = require("./providerModelTiers.cjs");
const {
  readInterfaceEnvironmentDeclarations,
  readInterfacePrivateEnvironmentDocument,
  validInterfaceEnvironmentName,
  writeInterfacePrivateValues,
} = require("./projection/privateConfiguration.cjs");

// Desktop account/catalog operations do not invoke a model or require a Home run.
async function readProvider(registryRoot, id) {
  if (typeof id !== "string" || !/^[A-Za-z0-9_-]+$/.test(id)) {
    throw new Error("Invalid provider interface id.");
  }
  const descriptor = JSON.parse(await fs.readFile(path.join(registryRoot, "interface", id, "interface.json"), "utf8"));
  if (descriptor.id !== id || descriptor.interface_type !== "world" || descriptor.role !== "provider") {
    throw new Error(`Interface '${id}' is not a provider.`);
  }
  return descriptor;
}

function providerSetupRequiredState(descriptor) {
  const privateConfig = descriptor?.interface_driver_config?.private_config;
  return privateConfig?.client_metadata !== undefined || privateConfig?.client_connection !== undefined
    ? "account_readable"
    : "credential_configured";
}

function providerAuthEnvKey(descriptor) {
  const key = descriptor?.interface_driver_config?.transport?.auth_env_key;
  if (key === undefined || key === null || key === "") return null;
  if (typeof key !== "string" || !validInterfaceEnvironmentName(key)) {
    throw new Error(`Provider '${descriptor?.id ?? "unknown"}' declares an invalid auth_env_key.`);
  }
  return key;
}

async function loadClientMetadata(registryRoot, descriptor) {
  const metadata = descriptor.interface_driver_config?.private_config?.client_metadata;
  if (metadata === undefined) return null;
  const parts = typeof metadata?.module === "string" && metadata.module.match(/^interface:\/\/([A-Za-z0-9_-]+)\/(packages\/.+\.cjs)$/);
  if (metadata?.schema_version !== 1 || !parts || parts[2].split("/").some(part => !part || part === "." || part === ".." || /[\\\x00]/.test(part))) {
    throw new Error(`Provider '${descriptor.id}' has an invalid client metadata module.`);
  }
  const owner = await readProvider(registryRoot, parts[1]);
  if (owner.interface_driver_config?.private_config?.client_metadata?.module !== metadata.module) {
    throw new Error(`Provider '${descriptor.id}' metadata is not declared by its installed owner.`);
  }
  const root = await fs.realpath(registryRoot);
  const file = path.join(root, "interface", parts[1], parts[2]);
  if (await fs.realpath(file) !== file || !(await fs.stat(file)).isFile()) {
    throw new Error(`Provider '${descriptor.id}' metadata must be a regular registry file.`);
  }
  delete require.cache[file];
  const client = require(file);
  if (client?.schemaVersion !== 1 || typeof client.modelCatalog !== "function") {
    throw new Error(`Provider '${descriptor.id}' has an unsupported client metadata contract.`);
  }
  return client;
}

function createClientProviderMetadata({ registryRoot, sourceRegistryRoot, octopusHome, accountClient, connectionHost, reference }) {
  async function catalog(providers, selectedRegistryRoot = registryRoot) {
    const root = selectedRegistryRoot();
    const cache = new Map();
    const result = await Promise.all(providers.map(async (provider) => {
      try {
        const descriptor = await readProvider(root, provider.id);
        const client = await loadClientMetadata(root, descriptor);
        const discovered = validateModelCatalog(client
          ? await client.modelCatalog({ descriptor, accountClient, cache })
          : descriptorModelCatalog(descriptor), descriptor.id);
        if (JSON.stringify(await readProvider(root, descriptor.id)) !== JSON.stringify(descriptor)) {
          throw new Error(`Provider '${descriptor.id}' changed during model discovery. Refresh the catalogue.`);
        }
        const allowed = descriptor.interface_policy?.variables?.models?.default;
        if (!Array.isArray(allowed) || !allowed.every((value) => typeof value === "string" && value)) {
          throw new Error(`Provider '${provider.id}' has no declared model list.`);
        }
        const models = discovered.filter((model) => allowed.includes("*") || allowed.includes(model.model));
        if (!models.length) throw new Error(`Provider '${provider.id}' returned no available declared models.`);
        return { ...provider, models, modelCatalogError: "" };
      } catch (error) {
        return { ...provider, models: [], modelCatalogError: error instanceof Error ? error.message : String(error) };
      }
    }));
    if (selectedRegistryRoot() !== root) throw new Error("Provider catalogue scope changed during discovery.");
    return result;
  }
  async function connectionOperation(descriptor, root, operation, options = {}) {
    const host = typeof connectionHost === "function" ? connectionHost() : connectionHost;
    if (!host || typeof host.probe !== "function" || typeof host.connect !== "function") {
      throw new Error(`Provider '${descriptor.id}' requires the application connection host during setup.`);
    }
    if (operation === "probeAccount") {
      const probed = await host.probe({ applicationId: descriptor.id });
      if (registryRoot() !== root) throw new Error("Provider account scope changed during setup.");
      const match = probed?.interfaces?.find((entry) => entry.interfaceId === descriptor.id);
      if (!match) throw new Error(`Provider '${descriptor.id}' connection status could not be checked.`);
      if (match.state?.state === "connected") {
        return [reference("provider_observation", descriptor.id, {
          source: "interface.client_connection",
          state: "connected",
          ...(match.state.account ? { account: match.state.account } : {}),
        })];
      }
      if (match.state?.state === "not_connected" || match.state?.state === "expired") return null;
      throw new Error(match.state?.error?.message || `Provider '${descriptor.id}' connection check failed.`);
    }
    if (operation === "signIn") {
      const connected = await host.connect(
        { operationId: `setup_${crypto.randomUUID()}`, applicationId: descriptor.id, interfaceId: descriptor.id, input: {} },
        {
          onProgress: (update) => {
            if (typeof options?.onProgress === "function" && typeof update?.message === "string" && update.message.trim()) {
              options.onProgress(update.message.trim());
            }
          },
        },
      );
      if (registryRoot() !== root) throw new Error("Provider account scope changed during setup.");
      if (connected?.state !== "connected") {
        throw new Error(`Provider '${descriptor.id}' did not establish a connected account.`);
      }
      return connected;
    }
    throw new Error(`Provider '${descriptor.id}' does not support setup operation '${operation}'.`);
  }
  async function accountOperation(plan, operation, options = {}) {
    const root = registryRoot();
    const descriptor = await readProvider(root, plan.provider.interface_id);
    if (descriptor.interface_driver_config?.private_config?.client_metadata !== undefined) {
      const client = await loadClientMetadata(root, descriptor);
      if (typeof client?.[operation] !== "function") {
        throw new Error(`Provider '${descriptor.id}' does not support desktop account setup.`);
      }
      const result = await client[operation]({ descriptor, accountClient, reference });
      if (registryRoot() !== root) throw new Error("Provider account scope changed during setup.");
      return result;
    }
    if (descriptor.interface_driver_config?.private_config?.client_connection !== undefined) {
      return connectionOperation(descriptor, root, operation, options);
    }
    throw new Error(`Provider '${descriptor.id}' does not support desktop account setup.`);
  }
  async function credentialScope(plan) {
    const root = registryRoot();
    const descriptor = await readProvider(root, plan.provider.interface_id);
    if (providerSetupRequiredState(descriptor) !== "credential_configured") {
      throw new Error(`Provider '${descriptor.id}' requires desktop account sign-in, not credential configuration.`);
    }
    const authEnvKey = providerAuthEnvKey(descriptor);
    if (!authEnvKey) return { root, descriptor, authEnvKey: null, home: "", interfaceDir: "", declaredNames: new Set() };
    const home = typeof octopusHome === "function" ? octopusHome() : "";
    if (typeof home !== "string" || !home.trim() || !path.isAbsolute(home)) {
      throw new Error(`Provider '${descriptor.id}' credential setup requires an absolute Octopus Home path.`);
    }
    const declarations = await readInterfaceEnvironmentDeclarations(
      descriptor,
      descriptor.id,
      path.join(root, "interface", descriptor.id),
    );
    const declaredNames = new Set([...declarations.map((entry) => entry.name), authEnvKey]);
    return { root, descriptor, authEnvKey, home, interfaceDir: path.join(home, "interfaces", descriptor.id), declaredNames };
  }
  async function probeCredential(plan) {
    const { root, descriptor, authEnvKey, home, interfaceDir, declaredNames } = await credentialScope(plan);
    if (!authEnvKey) {
      if (registryRoot() !== root) throw new Error("Provider credential scope changed during setup.");
      return [reference("provider_observation", descriptor.id, {
        source: "descriptor.no_auth_required",
        auth_required: false,
        configured: true,
      })];
    }
    const document = await readInterfacePrivateEnvironmentDocument(interfaceDir, descriptor.id, declaredNames);
    if (registryRoot() !== root || octopusHome() !== home) {
      throw new Error("Provider credential scope changed during setup.");
    }
    if (!document.values.has(authEnvKey)) return null;
    return [reference("provider_observation", descriptor.id, {
      source: "interface.private_environment",
      auth_env_key: authEnvKey,
      configured: true,
      document_digest: document.digest,
    })];
  }
  async function applyCredential(plan, options = {}) {
    const { root, descriptor, authEnvKey, home, interfaceDir, declaredNames } = await credentialScope(plan);
    if (!authEnvKey) {
      if (registryRoot() !== root) throw new Error("Provider credential scope changed during setup.");
      return;
    }
    const existing = await readInterfacePrivateEnvironmentDocument(interfaceDir, descriptor.id, declaredNames);
    if (existing.values.has(authEnvKey)) {
      if (registryRoot() !== root || octopusHome() !== home) {
        throw new Error("Provider credential scope changed during setup.");
      }
      return;
    }
    const targetFile = path.join(interfaceDir, "environment.json");
    const resolveValue = options?.resolveValue;
    if (typeof resolveValue !== "function") {
      throw new Error(
        `Provider '${descriptor.id}' requires '${authEnvKey}'. Set ${authEnvKey} in the environment or configure ${targetFile} (mode 0600) and rerun setup.`,
      );
    }
    const value = await resolveValue({ providerId: descriptor.id, authEnvKey, descriptor, targetFile });
    if (typeof value !== "string" || !value.trim() || value.includes("\0")) {
      throw new Error(`Provider '${descriptor.id}' credential '${authEnvKey}' must be a non-empty value.`);
    }
    await fs.mkdir(path.join(home, "interfaces"), { recursive: true, mode: 0o700 });
    await writeInterfacePrivateValues(interfaceDir, descriptor.id, declaredNames, { [authEnvKey]: value });
    if (registryRoot() !== root || octopusHome() !== home) {
      throw new Error("Provider credential scope changed during setup.");
    }
  }
  return {
    catalog,
    async setupCatalog(localPackages) {
      const activeRoot = registryRoot();
      const sourceRoot = sourceRegistryRoot();
      async function discoverPackProvider(pack, providerId, providerEntry) {
        const owners = pack.components.filter((component) => component.product_id === providerId);
        if (owners.length !== 1 || providerEntry?.id !== providerId) {
          throw new Error(`Setup pack '${pack.id}' has no exact provider catalogue owner.`);
        }
        const selectedRoot = owners[0].materialized === true || owners[0].partially_materialized === true
          ? registryRoot : sourceRegistryRoot;
        const [provider] = await catalog([providerEntry], selectedRoot);
        return provider;
      }
      const packs = await Promise.all(localPackages.packs.map(async (pack) => {
        if (pack.workspace_setup === undefined) return pack;
        const home = pack.workspace_setup.home;
        const defaultId = home.provider_interface_id;
        const providerIds = Array.isArray(home.provider_interface_ids) && home.provider_interface_ids.length
          ? home.provider_interface_ids
          : [defaultId];
        const rawProviders = Array.isArray(pack.setup_providers)
          ? pack.setup_providers
          : (pack.setup_provider ? [pack.setup_provider] : []);
        const discoveredProviders = await Promise.all(providerIds.map((id) => (
          discoverPackProvider(pack, id, rawProviders.find((entry) => entry?.id === id))
        )));
        const provider = discoveredProviders.find((entry) => entry.id === defaultId) ?? null;
        const voiceProviderId = home.voice?.provider_interface_id;
        const voiceProvider = voiceProviderId
          ? await discoverPackProvider(pack, voiceProviderId, pack.setup_voice_provider)
          : undefined;
        return {
          ...pack,
          setup_provider: provider,
          ...(Array.isArray(pack.setup_providers) ? { setup_providers: discoveredProviders } : {}),
          ...(voiceProviderId ? { setup_voice_provider: voiceProvider } : {}),
        };
      }));
      if (registryRoot() !== activeRoot || sourceRegistryRoot() !== sourceRoot) {
        throw new Error("Setup provider catalogue scope changed during discovery.");
      }
      return { ...localPackages, packs };
    },
    setupOwner: {
      probe: (plan, options) => (plan?.provider?.required_state === "credential_configured"
        ? probeCredential(plan)
        : accountOperation(plan, "probeAccount", options)),
      apply: (plan, options) => (plan?.provider?.required_state === "credential_configured"
        ? applyCredential(plan, options)
        : accountOperation(plan, "signIn", options)),
    },
    async models(request) {
      const id = request?.providerInterfaceId;
      const [{ models, modelCatalogError }] = await catalog([{ id }]);
      if (modelCatalogError) throw new Error(modelCatalogError);
      return { providerInterfaceId: id, models, observedAtUnixMs: Date.now() };
    },
  };
}

function descriptorModelCatalog(descriptor) {
  const config = descriptor.interface_driver_config?.private_config;
  if (!Array.isArray(config?.model_catalog)) {
    throw new Error(`Provider '${descriptor.id}' has no declared model catalogue.`);
  }
  return config.model_catalog.map(entry => ({
    model: entry?.model, displayName: entry?.display_name, description: entry?.description ?? "",
    defaultReasoningEffort: entry?.default_reasoning_effort ?? "",
    supportedReasoningEfforts: Array.isArray(entry?.supported_reasoning_efforts)
      ? entry.supported_reasoning_efforts.map(effort => typeof effort === "string" ? effort : effort?.effort) : undefined,
    showInPicker: entry?.show_in_picker, isDefault: entry?.model === config.model,
    ...modelServiceTiers(entry?.service_tiers, entry?.default_service_tier, `Provider model '${entry?.model}'`),
  }));
}

function validateModelCatalog(entries, providerId) {
  if (!Array.isArray(entries) || !entries.length) throw new Error(`Provider '${providerId}' has no model catalogue.`);
  const seen = new Set();
  return entries.map((entry) => {
    const efforts = entry?.supportedReasoningEfforts;
    if (typeof entry?.model !== "string" || !entry.model || entry.model.trim() !== entry.model || seen.has(entry.model)
      || typeof entry.displayName !== "string" || !entry.displayName.trim() || !Array.isArray(efforts)
      || efforts.some((effort) => typeof effort !== "string" || !effort) || new Set(efforts).size !== efforts.length
      || typeof entry.defaultReasoningEffort !== "string"
      || (efforts.length ? !efforts.includes(entry.defaultReasoningEffort) : entry.defaultReasoningEffort !== "")
      || typeof entry.showInPicker !== "boolean") {
      throw new Error(`Provider '${providerId}' has an invalid or repeated model.`);
    }
    seen.add(entry.model);
    return { ...entry, ...modelServiceTiers(entry.serviceTiers, entry.defaultServiceTier, `Provider model '${entry.model}'`) };
  });
}

function validateProviderModelSelection(providers, request) {
  const provider = providers.find((row) => row.id === request.providerInterfaceId);
  if (provider?.modelCatalogError) throw new Error(provider.modelCatalogError);
  const model = provider?.models.find((row) => row.model === request.providerModel);
  if (!model || (!model.showInPicker && model.model !== provider.model)) {
    throw new Error("Choose an available model from the selected provider catalogue.");
  }
  if (typeof request.reasoningEffort !== "string"
    || (model.supportedReasoningEfforts.length
      ? !model.supportedReasoningEfforts.includes(request.reasoningEffort)
      : request.reasoningEffort !== "")) {
    throw new Error("The selected reasoning level is not supported by this model.");
  }
  const requestedTier = request.serviceTier === undefined || request.serviceTier === ""
    ? provider.serviceTier || model.defaultServiceTier || "default"
    : request.serviceTier;
  validateModelServiceTier(model, requestedTier);
}

module.exports = {
  createClientProviderMetadata,
  validateProviderModelSelection,
  providerSetupRequiredState,
  providerAuthEnvKey,
  readProviderDescriptor: readProvider,
};
