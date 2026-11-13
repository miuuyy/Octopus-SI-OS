const assert = require("node:assert/strict"), fs = require("node:fs"), path = require("node:path"), vm = require("node:vm");
const { test } = require("node:test"), ts = require("typescript");
const app = path.resolve(__dirname, "..");
const compile = (source) => ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText;
function load(relative) {
  const filename = path.join(app, "src", relative), module = { exports: {} };
  Function("module", "exports", "require", compile(fs.readFileSync(filename, "utf8")))(module, module.exports, (id) => load(path.relative(path.join(app, "src"), path.resolve(path.dirname(filename), `${id}.ts`))));
  return module.exports;
}
const policies = load("shell/projectPolicyOptions.ts"), capabilities = load("components/chat/chatWorkspaceCapabilities.ts");
test("Project presets come from admitted descriptors and inheritance never hides a restriction", () => {
  const project = JSON.parse(fs.readFileSync(path.join(app, "../registry/ee_template/project/ee_template.json")));
  assert.equal(policies.projectPolicyOption(project).label, "Same policy as parent");
  for (const restriction of [{ allowed_tools: [] }, { allowed_root_slots: [] }, { hard_blocked_tools: ["write"] }, { interface_policy_overrides: { provider: { models: [] } } }]) {
    assert.equal(policies.projectPolicyOption({ ...project, ...restriction }).inheritsParentPolicy, false);
  }
  const custom = policies.projectPolicyOption({ id: "custom_policy", goal_class: "Installed custom policy" });
  const available = policies.allowedProjectPolicies([custom, policies.projectPolicyOption(project)], ["project", "missing"]);
  assert.deepEqual(available.map((option) => option.id), ["project"]);
  assert.match(custom.source, /custom_policy\/ee_template.json$/);
  assert.equal(policies.projectPolicyOption({ id: "existence", bootstrap_root: true }), null);
});

const source = ts.createSourceFile("workspace.ts", fs.readFileSync(path.join(app, "src/components/chat/useChatWorkspace.ts"), "utf8"), ts.ScriptTarget.Latest, true);
let readiness, chooseAccess;
(function visit(node) {
  if (ts.isVariableDeclaration(node) && node.name.getText(source) === "capabilitySelectionReady") readiness = node.initializer.getText(source);
  if (ts.isFunctionDeclaration(node) && node.name?.text === "chooseTaskAccessMode") chooseAccess = node.getText(source);
  ts.forEachChild(node, visit);
})(source);
function selection(patch = {}) {
  const worker = { id: "worker", applications: [], drivers: [{ name: "files", rootSlots: ["host_filesystem_root"], doorInterfaces: ["door"], tools: [], hostSlots: {} }], externalInputs: [], externalRelays: [], modelOutputs: [], selectedTools: [], contextArtifactBindings: [], payloadArtifactBindings: [], doorHostSlots: {} };
  const context = { eeId: "project", fullAccessRoot: "/", hostRootRefs: { host_filesystem_root: "/" }, workers: [worker] };
  const state = { ...capabilities, executionContextsLoaded: true, providerInterfacesLoaded: true,
    pendingNewTaskTargetRef: { current: null }, activeRunIdRef: { current: "" }, newTaskTargetResolutionBlocked: false,
    selectedClientAgent: { id: "agent", projectEeId: "project", workerInterfaceId: "worker" }, selectedWorker: worker,
    capabilitySelectionOwnerRef: { current: "project\0worker\0agent" }, executionContexts: [context], eeId: "project", workerInterfaceId: "worker",
    applicationIds: [], driverNames: ["files"], mode: "project_access", error: "", ...patch };
  state.setError = (value) => { state.error = value; }; state.setAccessMode = (value) => { state.mode = value; };
  vm.createContext(state);
  vm.runInContext(compile(`const capabilitySelectionReady = ${readiness}; ${chooseAccess}`), state);
  return state;
}
test("Full access waits for matching loaded selections and still refuses unsupported authority", () => {
  for (const patch of [{ executionContextsLoaded: false }, { providerInterfacesLoaded: false }, { pendingNewTaskTargetRef: { current: {} } }, { capabilitySelectionOwnerRef: { current: "other" } }, { selectedClientAgent: { id: "agent", projectEeId: "other", workerInterfaceId: "worker" } }]) {
    const state = selection(patch); state.chooseTaskAccessMode("full_access");
    assert.equal(state.mode, "project_access"); assert.equal(state.error, "");
  }
  const valid = selection(); valid.chooseTaskAccessMode("full_access"); assert.equal(valid.mode, "full_access");
  for (const mutate of [s => { s.executionContexts[0].fullAccessRoot = ""; }, s => { s.selectedWorker.drivers[0].hostSlots = { host_filesystem_root: "/limited" }; }]) {
    const state = selection(); mutate(state); state.chooseTaskAccessMode("full_access");
    assert.equal(state.mode, "project_access"); assert.ok(state.error);
    assert.ok(capabilities.taskFullAccessSelectionFailure(state.executionContexts[0], state.selectedWorker, [], state.driverNames));
  }
  for (const mutate of [s => { s.driverNames = []; }, s => { s.selectedWorker.drivers[0].rootSlots = []; }]) {
    const state = selection(); mutate(state); state.chooseTaskAccessMode("full_access");
    assert.deepEqual([state.mode, state.error], ["full_access", ""], "full access without a host-root Worker Environment adds nothing and blocks nothing");
    assert.equal(capabilities.taskFullAccessSelectionFailure(state.executionContexts[0], state.selectedWorker, [], state.driverNames), "");
  }
});

test("tasks require an explicit working folder without blocking access-mode selection", () => {
  const state = selection();
  const context = state.executionContexts[0], worker = state.selectedWorker;
  worker.drivers[0].rootSlots.push("workspace_root");
  context.fullAccessRoot = "/";
  const prepare = mode => capabilities.executionContextTaskCapabilities([context], context.eeId, worker.id, [], ["files"], mode);
  assert.match(prepare("project_access").workspaceSelectionError, /no working folder/);
  assert.equal(prepare("full_access").workspaceSelectionError, "");
  assert.equal(prepare("full_access").doorHostSlots.door.workspace_root, "/");
  context.sourceFolders = ["/project"];
  assert.equal(prepare("project_access").workspaceSelectionError, "");
  assert.equal(prepare("project_access").doorHostSlots.door.workspace_root, "/project");
  context.sourceFolders = [];
  context.hostRootRefs.workspace_root = "/configured";
  assert.equal(prepare("project_access").workspaceSelectionError, "");
  delete context.hostRootRefs.workspace_root;
  worker.doorHostSlots = { door: { workspace_root: "/agent" } };
  assert.equal(prepare("project_access").workspaceSelectionError, "");
});
