const assert = require("node:assert/strict");
const fs = require("node:fs/promises");
const os = require("node:os");
const path = require("node:path");
const vm = require("node:vm");
const { test } = require("node:test");
const { createChatgptSignIn } = require("../electron/chatgptSignIn.cjs");
const { createClientProviderMetadata, validateProviderModelSelection } = require("../electron/clientProviderMetadata.cjs");

test("desktop setup and paginated model discovery work without a Home, socket, or ledger", async () => {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "octopus-client-provider-"));
  const binary = path.join(root, "codex-fixture.cjs");
  const methods = path.join(root, "methods.jsonl");
  let opened = 0, accountHome = root;
  await fs.writeFile(binary, `#!${process.execPath}
const fs = require('node:fs');
const readline = require('node:readline');
let signedIn = false;
const row = model => ({model,displayName:model,description:'',defaultReasoningEffort:'high',supportedReasoningEfforts:[{reasoningEffort:'high'}],serviceTiers:[{id:'priority',name:'Fast',description:'Fast'},{id:'ultrafast',name:'Ultra Fast',description:'Ultra Fast'}],defaultServiceTier:null});
readline.createInterface({input:process.stdin}).on('line', line => {
  const {id, method, params} = JSON.parse(line);
  fs.appendFileSync(${JSON.stringify(methods)}, JSON.stringify(method)+'\\n');
  if (id === undefined) return;
  const result = method === 'account/read' ? {account:signedIn ? {type:'chatgpt'} : null}
    : method === 'account/login/start' ? {type:'chatgpt',loginId:'login',authUrl:'https://example.test/login'}
    : method === 'model/list' ? {data:params.cursor ? [row('second')] : [row('first'), {...row('no-reasoning'), defaultReasoningEffort:'none', supportedReasoningEfforts:[]}], nextCursor:params.cursor ? null : 'next'} : {};
  process.stdout.write(JSON.stringify({id,result})+'\\n');
  if (method === 'account/login/start') {
    signedIn = true;
    setTimeout(() => process.stdout.write(JSON.stringify({method:'account/login/completed',params:{loginId:'login',success:true}})+'\\n'), 10);
  }
});
`, { mode: 0o700 });
  const client = createChatgptSignIn({ codexBinary: () => binary, codexHome: () => accountHome,
    version: () => "test", openExternal: async () => { opened += 1; } });
  try {
    const folder = path.join(root, "registry", "interface", "provider");
    await fs.mkdir(folder, { recursive: true });
    await installNativeMetadata(path.join(root, "registry"));
    await fs.writeFile(path.join(folder, "interface.json"), JSON.stringify({ id: "provider", interface_type: "world",
      role: "provider", interface_driver_ref: "codex_app_server", interface_driver_config: nativeConfig(), interface_policy: { variables: { models: { default: ["second", "no-reasoning"] } } } }));
    const service = createClientProviderMetadata({ registryRoot: () => path.join(root, "registry"), sourceRegistryRoot: () => path.join(root, "registry"), accountClient: () => client,
      reference: (kind, id, evidence) => ({ kind, id, evidence }) });
    const source = await fs.readFile(path.join(__dirname, "../electron/main.cjs"), "utf8");
    const ownerFactory = source.slice(source.indexOf("function createClientSetupOwners("), source.indexOf("function clientSetupPackageRoots("));
    const owner = vm.runInNewContext(`${ownerFactory}; createClientSetupOwners().provider`, { getClientProviderMetadata: () => service });
    const plan = { provider: { interface_id: "provider" } };
    assert.equal(await owner.probe(plan), null);
    await owner.apply(plan);
    assert.equal((await owner.probe(plan))[0].evidence.signed_in, true);
    await owner.apply(plan);
    assert.equal(opened, 1, "an existing sign-in must not reopen the browser");
    const catalog = await service.models({ providerInterfaceId: "provider" });
    assert.deepEqual(catalog.models.map((row) => row.model), ["no-reasoning", "second"]);
    assert.deepEqual(catalog.models[0].supportedReasoningEfforts, []);
    assert.equal(catalog.models[0].defaultReasoningEffort, "");
    assert.deepEqual(catalog.models[1].supportedReasoningEfforts, ["high"]);
    validateProviderModelSelection([{ id: "provider", models: catalog.models }], {
      providerInterfaceId: "provider", providerModel: "no-reasoning", reasoningEffort: "",
    });
    assert.deepEqual(catalog.models[0].serviceTiers.map((tier) => tier.id), ["priority", "ultrafast"]);
    await assert.rejects(service.models({ providerInterfaceId: "../provider" }), /Invalid provider/);
    const calls = (await fs.readFile(methods, "utf8")).trim().split("\n").map(JSON.parse);
    assert.equal(calls.filter((method) => method === "model/list").length, 2);
    assert.ok(calls.every((method) => ["initialize", "initialized", "account/read", "account/login/start", "model/list"].includes(method)));
    accountHome = path.join(root, "another-account");
    assert.equal((await client.account()).signedIn, false, "a changed account Home must not reuse the previous app-server");
  } finally {
    client.dispose();
    await fs.rm(root, { recursive: true, force: true });
  }
});

test("account failures remain explicit and unsupported providers never reach Codex", async () => {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "octopus-client-provider-failure-"));
  try {
    const folder = path.join(root, "interface", "provider");
    await fs.mkdir(folder, { recursive: true });
    const descriptor = { id: "provider", interface_type: "world", role: "provider", interface_driver_ref: "other" };
    const write = () => fs.writeFile(path.join(folder, "interface.json"), JSON.stringify(descriptor));
    await write();
    const service = createClientProviderMetadata({ registryRoot: () => root, sourceRegistryRoot: () => root, accountClient: () => ({
      account: async () => { throw new Error("account transport failed"); },
    }), reference: () => assert.fail("must not manufacture signed-in evidence") });
    const plan = { provider: { interface_id: "provider" } };
    await assert.rejects(service.setupOwner.probe(plan), /does not support/);
    descriptor.interface_driver_ref = "codex_app_server";
    descriptor.interface_driver_config = nativeConfig();
    await installNativeMetadata(root);
    await write();
    await assert.rejects(service.setupOwner.probe(plan), /account transport failed/);
    await assert.rejects(service.setupOwner.apply(plan), /account transport failed/);
  } finally {
    await fs.rm(root, { recursive: true, force: true });
  }
});

function nativeConfig() {
  return { residual_ambient: { private_auth_store: { auth_file_env: "OCTOPUS_MACHINE_CODEX_AUTH_FILE" } }, private_config: {
    client_metadata: { schema_version: 1, module: "interface://native-metadata/packages/client.cjs" },
  } };
}

async function installNativeMetadata(registry) {
  const folder = path.join(registry, "interface/native-metadata/packages");
  await fs.mkdir(folder, { recursive: true });
  await fs.writeFile(path.join(folder, "../interface.json"), JSON.stringify({ id: "native-metadata", interface_type: "world", role: "provider", interface_driver_config: nativeConfig() }));
  await fs.copyFile(path.resolve(__dirname, "../../registry/interface/world_provider_codex/packages/octopus-provider-codex/client-metadata.cjs"), path.join(folder, "client.cjs"));
}

test("Claude Code exposes its declared model catalogue without consulting the Codex account", async () => {
  const registry = path.resolve(__dirname, "../../registry");
  const descriptor = JSON.parse(await fs.readFile(path.join(registry, "interface/world_provider_claude_cli/interface.json")));
  const service = createClientProviderMetadata({ registryRoot: () => registry,
    accountClient: () => assert.fail("CLI model discovery must not ask the Codex account") });
  const [provider] = await service.catalog([{ id: descriptor.id, runtimeHandlerRef: descriptor.interface_driver_ref, model: descriptor.interface_driver_config.private_config.model }]);
  assert.equal(provider.modelCatalogError, "");
  assert.deepEqual(provider.models.map(model => model.model), descriptor.interface_driver_config.private_config.model_catalog.map(model => model.model));
  assert.ok(provider.models.some(model => model.showInPicker));
});

test("catalogue uses declared data or owner modules independently of provider and adapter names", async () => {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "octopus-catalogue-"));
  let selectedRoot = root, calls = 0;
  const row = model => ({ model, displayName: model, supportedReasoningEfforts: ["high"], defaultReasoningEffort: "high", showInPicker: true });
  const declared = model => ({ model, display_name: model, supported_reasoning_efforts: ["high"], default_reasoning_effort: "high", show_in_picker: true });
  const nativeModels = [row("live-model"), row("outside-policy")];
  nativeModels[0].serviceTiers = [{ id: "priority", name: "Fast", description: "Fast" }, { id: "ultrafast", name: "Ultra Fast", description: "Ultra Fast" }];
  const native = { id: "native", runtimeHandlerRef: "codex_app_server", models: [row("stale-model")] };
  const custom = { id: "custom", runtimeHandlerRef: "codex_app_server", models: [row("stale-custom-model")] };
  const other = { id: "other", runtimeHandlerRef: "other", models: [row("other-model")] };
  const write = async (id, config, allowed, registry = root, driver = "codex_app_server") => {
    const folder = path.join(registry, "interface", id);
    await fs.mkdir(folder, { recursive: true });
    await fs.writeFile(path.join(folder, "interface.json"), JSON.stringify({ id, interface_type: "world", role: "provider",
      interface_driver_ref: driver, interface_driver_config: config, interface_policy: { variables: { models: { default: allowed } } } }));
  };
  const customConfig = { private_config: { model_catalog: [declared("custom-model")], opencodex: { adapter: "an-unfamiliar-adapter" } } };
  try {
    await installNativeMetadata(root);
    await write("native", nativeConfig(), ["live-model"]);
    await write("custom", customConfig, ["custom-model"]);
    await write("other", { private_config: { model_catalog: [declared("other-model")] } }, ["other-model"], root, "another-driver");
    const service = createClientProviderMetadata({ registryRoot: () => selectedRoot, sourceRegistryRoot: () => root,
      accountClient: () => ({ models: async () => { calls++; return nativeModels; } }) });
    const providers = await service.catalog([native, custom, other]);
    assert.equal(calls, 1);
    assert.deepEqual(providers.map(provider => provider.models.map(model => model.model)), [["live-model"], ["custom-model"], ["other-model"]]);
    await service.catalog([custom]); assert.equal(calls, 1);
    const installedRoot = path.join(root, "installed");
    const setupMetadata = createClientProviderMetadata({ registryRoot: () => installedRoot, sourceRegistryRoot: () => root,
      accountClient: () => ({ models: async () => nativeModels }) });
    const setup = { packs: [{ id: "setup", workspace_setup: { home: { provider_interface_id: "native" } },
      components: [{ product_id: "native", materialized: false, partially_materialized: false }], setup_provider: native }] };
    const sourceSetup = await setupMetadata.setupCatalog(setup);
    assert.deepEqual(sourceSetup.packs[0].setup_provider.models[0].serviceTiers, nativeModels[0].serviceTiers);
    assert.deepEqual(setup.packs[0].setup_provider.models, native.models, "discovery must not mutate the static pack projection");
    await assert.rejects(fs.access(installedRoot), { code: "ENOENT" });
    setup.packs[0].components[0].partially_materialized = true;
    const unavailableSetup = (await setupMetadata.setupCatalog(setup)).packs[0].setup_provider;
    assert.deepEqual(unavailableSetup.models, []);
    assert.match(unavailableSetup.modelCatalogError, /ENOENT/, "an installed owner must not silently reuse source catalogue evidence");
    await write("native", nativeConfig(), ["outside-policy"], installedRoot);
    await installNativeMetadata(installedRoot);
    assert.deepEqual((await setupMetadata.setupCatalog(setup)).packs[0].setup_provider.models.map((model) => model.model), ["outside-policy"]);
    const installedModule = path.join(installedRoot, "interface/native-metadata/packages/client.cjs");
    await fs.appendFile(installedModule, '\n// Locally authored provider implementation.\n');
    assert.deepEqual((await setupMetadata.setupCatalog(setup)).packs[0].setup_provider.models.map(model => model.model), ["outside-policy"]);
    await fs.appendFile(installedModule, '\nmodule.exports.schemaVersion = 99;\n');
    assert.match((await setupMetadata.setupCatalog(setup)).packs[0].setup_provider.modelCatalogError, /unsupported client metadata contract/);
    await installNativeMetadata(installedRoot);
    setup.packs[0].setup_provider = { ...native, id: "unbound" };
    await assert.rejects(setupMetadata.setupCatalog(setup), /no exact provider catalogue owner/);
    await assert.rejects(service.setupOwner.probe({ provider: { interface_id: "custom" } }), /does not support desktop account/);
    validateProviderModelSelection(providers, { providerInterfaceId: "custom", providerModel: "custom-model", reasoningEffort: "high" });
    for (const selection of [{ providerInterfaceId: "native", providerModel: "custom-model", reasoningEffort: "high" },
      { providerInterfaceId: "native", providerModel: "stale-model", reasoningEffort: "high" },
      { providerInterfaceId: "native", providerModel: "live-model", reasoningEffort: "ultra" }]) {
      assert.throws(() => validateProviderModelSelection(providers, selection));
    }
    nativeModels.splice(0, 1);
    assert.match((await service.catalog([native]))[0].modelCatalogError, /no available declared models/);
    const scoped = createClientProviderMetadata({ registryRoot: () => selectedRoot, sourceRegistryRoot: () => root, accountClient: () => ({ models: async () => { selectedRoot = "another-home"; return [row("live-model")]; } }) });
    await assert.rejects(scoped.catalog([native]), /scope changed/); selectedRoot = root;
    delete customConfig.private_config.model_catalog;
    await write("custom", customConfig, ["custom-model"]);
    const unavailableCustom = await service.catalog([custom, other]);
    assert.match(unavailableCustom[0].modelCatalogError, /no declared model catalogue/);
    assert.equal(unavailableCustom[0].models.length, 0);
    assert.deepEqual(unavailableCustom[1].models.map(model => model.model), ["other-model"]);
    customConfig.private_config.model_catalog = [declared("custom-model")];
    customConfig.private_config.client_metadata = { schema_version: 1, module: "interface://custom/packages/../outside.cjs" };
    await write("custom", customConfig, ["custom-model"]);
    assert.match((await service.catalog([custom]))[0].modelCatalogError, /invalid client metadata/);
    customConfig.private_config.client_metadata.module = "interface://custom/packages/missing.cjs";
    await write("custom", customConfig, ["custom-model"]);
    assert.match((await service.catalog([custom]))[0].modelCatalogError, /ENOENT/, "failed discovery must not fall back to a static catalogue");
    const failing = createClientProviderMetadata({ registryRoot: () => root, sourceRegistryRoot: () => root, accountClient: () => ({ models: async () => { throw new Error("offline"); } }) });
    assert.match((await failing.catalog([native]))[0].modelCatalogError, /offline/);
  } finally { await fs.rm(root, { recursive: true, force: true }); }
});

test("credential_configured providers probe and store private environment keys without Codex sign-in", async () => {
  const { providerSetupRequiredState } = require("../electron/clientProviderMetadata.cjs");
  const root = await fs.realpath(await fs.mkdtemp(path.join(os.tmpdir(), "octopus-provider-cred-")));
  try {
    const reg = path.join(root, "registry"), home = path.join(root, "home");
    const ifaceDir = path.join(reg, "interface/world_provider_keyed");
    await fs.mkdir(ifaceDir, { recursive: true });
    const descriptor = {
      id: "world_provider_keyed", interface_type: "world", role: "provider",
      interface_driver_config: { transport: { auth_env_key: "KEYED_API_KEY" }, private_config: { model: "m" } },
    };
    await fs.writeFile(path.join(ifaceDir, "interface.json"), JSON.stringify(descriptor));
    assert.equal(providerSetupRequiredState(descriptor), "credential_configured");
    const service = createClientProviderMetadata({
      registryRoot: () => reg, sourceRegistryRoot: () => reg, octopusHome: () => home,
      accountClient: () => assert.fail("must not touch Codex account"),
      reference: (kind, id, data) => ({ kind, id, data }),
    });
    const plan = { provider: { interface_id: "world_provider_keyed", required_state: "credential_configured" } };
    assert.equal(await service.setupOwner.probe(plan), null);
    await service.setupOwner.apply(plan, { resolveValue: async () => "secret-token-value" });
    const probed = await service.setupOwner.probe(plan);
    assert.equal(probed[0].data.configured, true);
    assert.equal(probed[0].data.auth_env_key, "KEYED_API_KEY");
  } finally { await fs.rm(root, { recursive: true, force: true }); }
});
