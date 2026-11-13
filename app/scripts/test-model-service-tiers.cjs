const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { test } = require("node:test");
const ts = require("typescript");
const { modelServiceTiers, validateModelServiceTier } = require("../electron/providerModelTiers.cjs");

function taskAccessExports() {
  const source = fs.readFileSync(path.join(__dirname, "..", "src/components/chat/taskAccess.ts"), "utf8");
  const module = { exports: {} };
  Function("module", "exports", ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText)(module, module.exports);
  return module.exports;
}

function functions(file, names, bindings = {}) {
  const scope = { DEFAULT_TASK_ACCESS_MODE: taskAccessExports().DEFAULT_TASK_ACCESS_MODE, ...bindings };
  const source = fs.readFileSync(path.join(__dirname, "..", file), "utf8");
  const tree = ts.createSourceFile(file, source, ts.ScriptTarget.Latest, true);
  const selected = [];
  const visit = (node) => {
    if (ts.isFunctionDeclaration(node) && names.includes(node.name.text)) selected.push(node);
    ts.forEachChild(node, visit);
  };
  visit(tree);
  assert.equal(selected.length, names.length);
  const code = ts.transpileModule(selected.map((node) => node.getText(tree)).join("\n"), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
  }).outputText;
  return Function("exports", ...Object.keys(scope), code + ";return {" + names.join(",") + "};")({}, ...Object.values(scope));
}

const tiers = [
  { id: "priority", name: "Fast", description: "Declared Fast" },
  { id: "ultrafast", name: "Ultra Fast", description: "Declared Ultra Fast" },
  { id: "custom-express", name: "Express", description: "Provider-defined speed" },
];

test("native metadata keeps exact tier IDs, names and defaults and rejects invented eligibility", () => {
  const metadata = modelServiceTiers(tiers, "ultrafast", "Fixture");
  assert.deepEqual(metadata, { serviceTiers: tiers, defaultServiceTier: "ultrafast" });
  for (const tier of ["default", ...tiers.map((tier) => tier.id)]) {
    validateModelServiceTier({ model: "declared-model", ...metadata }, tier);
  }
  const fastOnly = { model: "declared-model", ...modelServiceTiers(tiers.slice(0, 1), null, "Fixture") };
  for (const tier of ["ultrafast", "ultra_fast", "turbo", " priority", false, null]) {
    assert.throws(() => validateModelServiceTier(fastOnly, tier), /does not advertise service tier/);
  }
  assert.throws(() => modelServiceTiers(tiers, "missing", "Fixture"), /not advertised/);
  assert.throws(() => modelServiceTiers([...tiers, tiers[0]], null, "Fixture"), /repeated/);
});

test("frontend speed choices follow the selected model and preserve unavailable configured defaults", () => {
  const api = functions("src/components/chat/chatWorkspaceCapabilities.ts", ["providerModels", "providerDefaultModel", "providerSelectedModel", "providerServiceTiers", "providerSelectedServiceTier", "providerServiceTierAvailable"]);
  const model = { model: "declared-model", showInPicker: true, isDefault: true, serviceTiers: tiers };
  const provider = { model: "declared-model", models: [model, { ...model, model: "standard-only", serviceTiers: [] }] };
  assert.deepEqual(api.providerServiceTiers(provider, model.model).map((tier) => tier.id), ["default", "priority", "ultrafast", "custom-express"]);
  assert.equal(api.providerServiceTierAvailable(provider, "standard-only", "ultrafast"), false);
  assert.equal(api.providerServiceTierAvailable(provider, model.model, "ultrafast"), true);
  assert.equal(api.providerSelectedServiceTier({ ...provider, serviceTier: "priority" }, model.model), "priority");
  assert.equal(api.providerSelectedServiceTier({ ...provider, serviceTier: "priority" }, model.model, "default"), "default");
  assert.equal(api.providerServiceTierAvailable({ ...provider, serviceTier: "ultrafast" }, "standard-only", ""), false);
  assert.equal(api.providerDefaultModel({ ...provider, model: "unavailable-configured-model" }), null);
});

test("provider projection keeps catalogue tiers without attaching its default model to every label", () => {
  const api = require("../electron/projection/providerDescriptors.cjs");
  const descriptor = { source_names: ["chatgpt"], interface_driver_config: { private_config: { model: "old-default-model" } } };
  assert.equal(api.providerLabel(descriptor, "provider"), "chatgpt");
  const [model] = api.providerModelCatalogRows({}, { model_catalog: [{ model: "chosen-model", service_tiers: tiers, default_service_tier: "ultrafast" }] }, "chosen-model");
  assert.deepEqual(model.serviceTiers, tiers);
  assert.equal(model.defaultServiceTier, "ultrafast");
});

test("picker distinguishes exact provider identities when native model and source labels overlap", () => {
  const { displayName } = functions("src/components/chat/ProviderModelPicker.tsx", ["displayName"]);
  const option = (id) => ({ provider: { id, label: "chatgpt" }, model: { model: "gpt-chosen", displayName: "GPT-Chosen" } });
  const options = [option("native-owner"), option("another-native-owner")];
  assert.equal(displayName(options[0], options), "Chosen · native-owner");
  assert.equal(displayName(options[1], options), "Chosen · another-native-owner");
});

test("existing runs wait for their bound model and tier while saved run drafts remain exact", () => {
  const file = "src/components/chat/useChatDraftState.ts";
  const tree = ts.createSourceFile(file, fs.readFileSync(path.join(__dirname, "..", file), "utf8"), ts.ScriptTarget.Latest, true);
  const initializers = {};
  let scopeRestore;
  const visit = (node) => {
    if (ts.isVariableDeclaration(node) && ts.isArrayBindingPattern(node.name)
      && ts.isCallExpression(node.initializer) && node.initializer.expression.getText(tree) === "useState") {
      const key = node.name.elements[0].name?.getText(tree);
      if (["providerInterfaceId", "providerModel", "reasoningEffort", "serviceTier"].includes(key)) {
        initializers[key] = Function("initialDraftRef", "storedSettingsRef", "input", "return " + node.initializer.arguments[0].getText(tree));
      }
    }
    if (ts.isIfStatement(node) && node.expression.getText(tree) === "loadedScope !== scopeIdentity") {
      const findRestore = (child) => {
        if (ts.isCallExpression(child) && child.expression.getText(tree) === "applyDraft") {
          scopeRestore = Function("DEFAULT_TASK_ACCESS_MODE", "initialDraftRef", "input", "storedSettingsRef", "return " + child.arguments[0].getText(tree))
            .bind(null, taskAccessExports().DEFAULT_TASK_ACCESS_MODE);
        }
        ts.forEachChild(child, findRestore);
      };
      findRestore(node.thenStatement);
    }
    ts.forEachChild(node, visit);
  };
  visit(tree);
  const settings = { providerInterfaceId: "new-task-provider", providerModel: "new-task-model", reasoningEffort: "high", serviceTier: "priority" };
  const draft = { providerInterfaceId: "bound-provider", providerModel: "chosen-model", reasoningEffort: "", serviceTier: "default" };
  for (const key of Object.keys(settings)) {
    assert.equal(initializers[key]({ current: undefined }, { current: settings }, { activeRunId: "existing-run" }), "");
    assert.equal(initializers[key]({ current: undefined }, { current: settings }, { activeRunId: "" }), settings[key]);
    assert.equal(initializers[key]({ current: draft }, { current: settings }, { activeRunId: "existing-run" }), draft[key]);
  }
  const restore = (activeRunId, saved) => scopeRestore({ current: saved }, { activeRunId }, { current: settings });
  for (const key of Object.keys(settings)) {
    assert.equal(restore("existing-run")[key], "");
    assert.equal(restore("")[key], settings[key]);
    assert.equal(restore("existing-run", draft)[key], draft[key]);
  }
  assert.equal(restore("").accessMode, "full_access");
});

test("durable queue claims and submission restoration preserve the selected tier", () => {
  const { queueHead, claimQueueTurn } = functions("src/components/chat/clientTurnQueue.ts", ["queueHead", "claimQueueTurn"]);
  const { queuedSubmission } = functions("src/components/chat/useChatWorkspace.ts", ["queuedSubmission"], { chatDraftKey: (id) => id || "new-task" });
  const item = { id: "queued-id", messageId: "message-id", runKey: "bound-run", status: "queued", text: "Work", attachments: [],
    providerInterfaceId: "chosen-provider", providerModel: "chosen-model", reasoningEffort: "xhigh", serviceTier: "ultrafast", clientSurfaceRole: "task" };
  const claimed = claimQueueTurn({ turns: [item], pausedRunKeys: [] }, item.id, "owner", false).turns[0];
  assert.equal(claimed.serviceTier, "ultrafast");
  const restored = queuedSubmission(claimed);
  assert.equal(restored.providerModel, "chosen-model");
  assert.equal(restored.serviceTier, "ultrafast");
  assert.equal(item.status, "queued");
});

test("new Home messages read current JSON settings while queued messages and task choices stay exact", async () => {
  let run = { runId: "home", clientSurfaceRole: "home", writable: true, providerInterfaceId: "provider", providerModel: "model", reasoningEffort: "xhigh", serviceTier: "ultrafast" };
  let stored = [], submitted;
  const api = functions("src/components/chat/useChatWorkspace.ts", ["beginSubmission", "currentHomeProviderSelection"], {
    loadChatRuns: async () => [run], deliveryScopeCurrent: () => true, activeRunIdRef: { current: "home" }, queueOwner: { id: "owner", active: true },
    replaceQueuedTurns: async update => { stored = update(stored); }, consumeDraft: () => {}, resetInputHistory: () => {}, executeTurnSequence: async value => { submitted = value; },
  });
  const draft = { runId: "home", clientSurfaceRole: "home", messageId: "first", text: "Draft", attachments: [], providerModel: "old-model", reasoningEffort: "xhigh", serviceTier: "priority" };
  run = { ...run, reasoningEffort: "low" };
  await api.beginSubmission(draft);
  assert.deepEqual([stored[0].providerModel, stored[0].reasoningEffort, stored[0].serviceTier], ["model", "low", "ultrafast"]);
  await api.beginSubmission({ ...draft, queueId: "already-queued" });
  assert.equal(submitted.reasoningEffort, "xhigh");
  await api.beginSubmission({ ...draft, clientSurfaceRole: "task", messageId: "task-choice" });
  assert.equal(submitted.providerModel, "old-model");
  run = { ...run, archived: true };
  await assert.rejects(api.beginSubmission({ ...draft, messageId: "archived" }), /configuration is unavailable/);
});

test("the picker lists only declared models and never invents the pinned one", () => {
  const { providerModels, providerDefaultModel, providerSelectedModel } = functions("src/components/chat/chatWorkspaceCapabilities.ts", ["providerModels", "providerDefaultModel", "providerSelectedModel"]);
  const model = (id, extra = {}) => ({ model: id, displayName: id, description: "", defaultReasoningEffort: "", supportedReasoningEfforts: [], showInPicker: true, isDefault: false, ...extra });
  const catalog = [model("alpha"), model("beta", { isDefault: true }), model("hidden", { showInPicker: false })];
  const provider = { id: "provider.fixture", models: catalog };
  assert.deepEqual(providerModels(provider).map((entry) => entry.model), ["alpha", "beta"]);
  assert.equal(providerDefaultModel(provider).model, "beta");
  assert.deepEqual(providerModels({ ...provider, model: "hidden" }).map((entry) => entry.model), ["alpha", "beta", "hidden"], "a pinned model stays selectable even when the picker hides it");
  assert.equal(providerDefaultModel({ ...provider, model: "hidden" }).model, "hidden");
  assert.equal(providerDefaultModel({ ...provider, model: "retired" }), null, "a pinned model missing from the catalog is not replaced");
  assert.deepEqual(providerModels({ ...provider, model: "retired" }).map((entry) => entry.model), ["alpha", "beta"]);
  assert.equal(providerSelectedModel(provider, "").model, "beta");
  assert.equal(providerSelectedModel(provider, " alpha ").model, "alpha");
  assert.equal(providerSelectedModel(provider, "hidden"), null);
  assert.equal(providerSelectedModel(provider, "retired"), null, "an unknown selection is not silently replaced by the default");
});
