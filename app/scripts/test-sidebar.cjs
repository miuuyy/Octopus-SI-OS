const assert = require("node:assert/strict");
const path = require("node:path");
const { test, after } = require("node:test");
const { build } = require("esbuild");
const { JSDOM } = require("jsdom");

const dom = new JSDOM('<div id="root"></div>', { url: "http://fixture.invalid", pretendToBeVisual: true });
const w = dom.window;
for (const key of ["window", "document", "HTMLElement", "Element", "SVGElement", "Node", "Event", "CustomEvent", "MouseEvent", "KeyboardEvent", "localStorage", "navigator", "getComputedStyle", "MutationObserver"]) {
  Object.defineProperty(globalThis, key, { configurable: true, value: key === "window" ? w : key === "getComputedStyle" ? w[key].bind(w) : w[key] });
}
Object.defineProperty(w, "innerWidth", { configurable: true, value: 1440 });
Object.defineProperty(w, "innerHeight", { configurable: true, value: 900 });
globalThis.requestAnimationFrame = w.requestAnimationFrame.bind(w);
globalThis.cancelAnimationFrame = w.cancelAnimationFrame.bind(w);
globalThis.ResizeObserver = w.ResizeObserver = class { observe() {} unobserve() {} disconnect() {} };
globalThis.IntersectionObserver = w.IntersectionObserver = class { observe() {} unobserve() {} disconnect() {} };
w.matchMedia = () => ({ matches: false, addEventListener() {}, removeEventListener() {}, addListener() {}, removeListener() {} });
w.HTMLElement.prototype.scrollIntoView = function () {};
const rect = (left, top, width, height) => ({ x: left, y: top, left, top, right: left + width, bottom: top + height, width, height });
w.HTMLElement.prototype.getBoundingClientRect = function () {
  if (this.classList.contains("chat-tooltip")) return rect(Number.parseFloat(this.style.left) - 50 || 0, Number.parseFloat(this.style.top) || 0, 100, 20);
  if (this.style.position === "fixed" && this.style.top) {
    const [x = 0, y = 0] = (/translate3d\(([-\d.]+)px, ([-\d.]+)px/.exec(this.style.transform) ?? []).slice(1).map(Number);
    return rect(Number.parseFloat(this.style.left) + x, Number.parseFloat(this.style.top) + y, Number.parseFloat(this.style.width), Number.parseFloat(this.style.height));
  }
  if (this.classList.contains("sidebar-sortable-item")) return rect(0, [...document.querySelectorAll(".sidebar-sortable-item")].indexOf(this) * 32, 240, 30);
  if (this.querySelector?.(".sidebar-sortable-item") && !this.classList.contains("octopus-sidebar")) return rect(0, 5000, 0, 0);
  return rect(0, 0, 240, 900);
};
const errors = [];
w.addEventListener("error", (event) => errors.push(event.error ?? event.message));

const compiled = build({
  stdin: {
    contents: [
      'import "./src/components/icon-glyphs/shell"; export { Sidebar } from "./src/components/Sidebar";',
      'export { ProjectSourceFolders } from "./src/components/project/ProjectSourceFolders";',
      'export { projectChatRuns, projectChatExecutionContexts } from "./src/chatRuns"; export { resolveClientAgents } from "./src/agentPresets";',
      'export { recordClientTaskRead, recordClientDestination, updateClientTaskOrganization } from "./src/shell/clientWorkspaces";',
    ].join("\n"),
    resolveDir: path.join(__dirname, ".."), loader: "tsx",
  },
  bundle: true, platform: "node", format: "cjs", write: false, logLevel: "silent",
  define: { "import.meta.env.DEV": "false", "import.meta.env.PROD": "true" },
  loader: { ".css": "empty", ".svg": "dataurl", ".png": "dataurl" },
  external: ["react", "react-dom", "react-dom/client", "react/jsx-runtime", "motion/react", "@pierre/diffs", "@pierre/diffs/*", "shiki", "@shikijs/*", "mermaid", "katex"],
}).then((result) => {
  const module = { exports: {} };
  const heavy = new Proxy({}, { get: (_, name) => name === "__esModule" ? true : () => null });
  Function("require", "module", "exports", result.outputFiles[0].text)((id) => /^(@pierre\/diffs|shiki|@shikijs|mermaid|katex)/.test(id) ? heavy : require(id), module, module.exports);
  return module.exports;
});

const React = require("react");
const { createRoot } = require("react-dom/client");
const root = createRoot(document.getElementById("root"));
after(() => { root.unmount(); w.close(); assert.deepEqual(errors, []); });
const settle = async (rounds = 4) => { for (let round = 0; round < rounds; round += 1) await new Promise((resolve) => setTimeout(resolve, 10)); };
const until = async (predicate, message) => {
  for (let attempt = 0; attempt < 200; attempt += 1) {
    if (predicate()) return;
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
  assert.fail(typeof message === "function" ? message() : message);
};

const provider = { id: "provider.test", label: "Test provider", descriptorPath: "registry/interface/provider.test", sourceNames: [], supportedReasoningEfforts: ["medium"],
  models: [{ model: "model-a", displayName: "Model A", description: "", defaultReasoningEffort: "medium", supportedReasoningEfforts: ["medium"], showInPicker: true, isDefault: true }] };
const agent = (id, name, worker) => ({ id: worker, label: worker, provider_interface_ids: [provider.id], interface_policy: { [provider.id]: { models: ["model-a"] } },
  client_agent_id: id, client_agent_name: name, client_agent_icon: "default", client_agent_created_at_ms: 1, client_agent_updated_at_ms: 1, client_agent_deleted_at_ms: 0,
  agent_source_template: true, client_template_digest: `${id}-digest`, client_template_provider_interface_id: provider.id, client_template_provider_model: "model-a",
  client_template_reasoning_effort: "medium", client_template_application_ids: [], client_template_worker_environment_kinds: [] });
const context = (eeId, workers, extra = {}) => ({ ee_id: eeId, display_name: eeId, agent_catalog_digest: "catalog-1", workers, ...extra });
const run = (runId, eeId, worker, extra = {}) => ({ run_id: runId, title: `Task ${runId}`, chat_interface_id: `chat.${runId}`, client_surface_role: "task", writable: true, ee_id: eeId,
  provider_interface_id: provider.id, provider_model: "model-a", worker_interface_id: worker, turn_open: false, turn_state: "idle", last_movement_seq: 1, updated_at_ms: 1_000, ...extra });
const surfaces = [
  { id: "chat", label: "Tasks", glyph: "tasks", rendererKind: "builtin.chat", order: 50, source: "builtin", placements: ["home"], chrome: { topbar: "none", workspace: "immersive" } },
  { id: "scheduled", label: "Triggers", glyph: "scheduled", rendererKind: "builtin.scheduled", order: 60, source: "builtin", placements: ["home"], chrome: { topbar: "none", workspace: "immersive" } },
];

async function show(patch = {}, organization = {}) {
  const { Sidebar, projectChatRuns, projectChatExecutionContexts, resolveClientAgents, updateClientTaskOrganization } = await compiled;
  const contexts = projectChatExecutionContexts({ contexts: [
    context("ee.root", [agent("agent-writer", "Writer", "worker.writer")], { is_os_root: true }),
    context("ee.project", [agent("agent-analyst", "Analyst", "worker.analyst")]),
    context("ee.ghost", [agent("agent-ghost", "Ghost", "worker.ghost")]),
  ] });
  const calls = { newChat: [], order: [], pinned: [], projects: [] };
  const state = {
    accessProfile: "live", surfaces, navigationSection: "tasks", activeSurfaceId: "chat", activeChatRunId: "run-a",
    chatRuns: projectChatRuns({ chats: [
      run("run-a", "ee.project", "worker.analyst", { updated_at_ms: 3_000 }), run("run-b", "ee.project", "worker.other", { worker_choice_interface_id: "worker.analyst", updated_at_ms: 2_000 }),
      run("run-c", "ee.root", "worker.writer", { updated_at_ms: 1_000 }),
    ] }),
    chatRunsPhase: "ready", chatRunListError: "", onRetryChatRuns: async () => {}, homeRunId: "home", activityContent: null, agentPinnedChatIds: [], agentChatOrderIds: [], agentProjectFilterEeId: "",
    agentProjectWorkspace: {
      projects: [{ id: "ee.project", name: "Atlas", icon: "folder", color: "blue", parentId: "", chatRunIds: ["run-a", "run-b"] }], rootEeId: "ee.root", homeRunId: "home", homeInterfaceId: "chat.home",
      rootContext: { eeId: "ee.root", displayName: "Root", templateName: "" }, rootAllowedChildTemplateIds: [], templates: [], status: "ready", phase: "ready", pending: false, error: "",
      updatePresentation: (projects) => calls.projects.push(projects), createProject: async () => {}, createAgentTemplate: async () => ({ agentId: "", workerInterfaceId: "" }),
      configureAgentTemplate: async () => {}, deleteAgentTemplate: async () => {}, editProject: async () => {}, deleteProject: async () => null, restoreProject: async () => {},
      deleteTask: async () => {}, setTaskArchived: async () => {},
    },
    agentExecutionContexts: contexts, agentDefinitions: resolveClientAgents(contexts), agentProviderInterfaces: [provider], agentModelsLoading: false, agentModelError: "",
    onRefreshAgentModels: async () => {}, surfaceErrors: [], runtimeHost: { home: "/home/fixture" },
    onSurfaceChange() {}, onSelectChat: (runId) => rerender({ activeChatRunId: runId }), onNewChat: (projectId) => calls.newChat.push(projectId), onSearch() {}, onAgentProjectFilterEeIdChange() {},
    onAgentPinnedChatIdsChange: (ids) => { calls.pinned.push(ids); rerender({ agentPinnedChatIds: ids }); },
    onAgentChatOrderIdsChange: (ids) => { calls.order.push(ids); rerender({ agentChatOrderIds: ids }); },
  };
  const rerender = (patch) => { Object.assign(state, patch); root.render(React.createElement(Sidebar, { ...state })); };
  window.localStorage.clear();
  updateClientTaskOrganization("live", () => organization);
  rerender(patch);
  await settle();
  return { ...calls, state, rerender };
}
const sectionToggle = (label) => [...document.querySelectorAll("button[aria-expanded]")].find((toggle) => toggle.textContent === label) ?? null;
const agentRows = () => [...document.querySelectorAll("button[aria-label]")].map((row) => row.getAttribute("aria-label")).filter((label) => /, \d+ tasks?$/.test(label));
const taskTitles = () => [...document.querySelectorAll(".octopus-task-row")].map((row) => row.getAttribute("aria-label").split(",")[0]);

test("chat order stays fixed across clicks, reads and status changes; only data updates change recency", async () => {
  const { recordClientTaskRead, recordClientDestination } = await compiled;
  const { state, rerender } = await show({}, { readBaselineAtMs: 100 });
  const initial = ["Task run-a", "Task run-b", "Task run-c"];
  assert.deepEqual(taskTitles(), initial);
  document.querySelector('.octopus-task-row[aria-label^="Task run-b"]').click();
  await settle();
  assert.equal(state.activeChatRunId, "run-b");
  assert.deepEqual(taskTitles(), initial);
  const read = recordClientTaskRead("live", "run-b", 2_000);
  const visited = recordClientDestination("live", { workspaceKey: "octopus-os", surfaceId: "chat", runId: "run-b" });
  await settle();
  assert.equal(read.readMarks["run-b"], 2_000);
  assert.equal(visited.recentRunIds[0], "run-b");
  assert.deepEqual(taskTitles(), initial);
  rerender({ chatRuns: state.chatRuns.map((chat, index) => ({ ...chat, turnState: ["working", "failed", "waiting"][index], turnOpen: index === 0 })) });
  await settle();
  assert.deepEqual(taskTitles(), initial);
  rerender({ chatRuns: state.chatRuns.map((chat) => chat.runId === "run-c" ? { ...chat, updatedAtMs: 4_000 } : chat) });
  await settle();
  assert.deepEqual(taskTitles(), ["Task run-c", "Task run-a", "Task run-b"]);
  rerender({ chatRuns: state.chatRuns.map((chat) => chat.runId === "run-c" ? { ...chat, updatedAtMs: 3_000 } : chat) });
  await settle();
  assert.deepEqual(taskTitles(), ["Task run-a", "Task run-c", "Task run-b"], "equal timestamps preserve source order");
});

test("manual order, pins and project groups survive clicks, reads and task updates", async () => {
  const { recordClientTaskRead } = await compiled;
  const calls = await show({ agentPinnedChatIds: ["run-b"], agentChatOrderIds: ["run-c", "run-a"] }, { sortMode: "manual", readBaselineAtMs: 100 });
  const initial = ["Task run-b", "Task run-c", "Task run-a"];
  assert.deepEqual(taskTitles(), initial);
  document.querySelector('.octopus-task-row[aria-label^="Task run-c"]').click();
  recordClientTaskRead("live", "run-c", 1_000);
  calls.rerender({ chatRuns: calls.state.chatRuns.map((chat) => ({ ...chat, updatedAtMs: chat.runId === "run-a" ? 9_000 : 4_000 })) });
  await settle();
  assert.deepEqual(taskTitles(), initial);
  calls.rerender({ agentProjectFilterEeId: "ee.project" });
  await settle();
  assert.deepEqual(taskTitles(), ["Task run-b", "Task run-a"]);
  calls.rerender({ agentProjectFilterEeId: "" });
  await settle();
  assert.deepEqual(taskTitles(), initial);
  assert.deepEqual([calls.order, calls.pinned, calls.projects], [[], [], []]);
});

test("saved attention sorting uses Last updated and the list offers only chronological or manual order", async () => {
  await show({}, { sortMode: "attention", readBaselineAtMs: 100, readMarks: { "run-a": 3_000 } });
  assert.deepEqual(taskTitles(), ["Task run-a", "Task run-b", "Task run-c"]);
  sectionToggle("Tasks").dispatchEvent(new w.MouseEvent("contextmenu", { bubbles: true, cancelable: true }));
  await settle();
  const options = [...document.querySelectorAll('[role="menuitemradio"]')];
  assert.deepEqual(options.map((option) => option.textContent), ["Last updated", "Manual order"]);
  assert.equal(options[0].getAttribute("aria-checked"), "true");
  options[0].click();
  await settle();
});

test("the Agents section lists agents of known projects and the OS root with their task counts and opens each one for editing", { timeout: 20_000 }, async () => {
  const calls = await show();
  assert.deepEqual(agentRows(), ["Analyst, 2 tasks", "Writer, 1 task"], "an agent outside every known project is hidden; a task counts by its chosen worker");
  document.querySelector('button[aria-label="Analyst, 2 tasks"]').click();
  await until(() => document.getElementById("octopus-agent-preset-title"), "the agent never opened");
  assert.equal(document.getElementById("octopus-agent-preset-title").textContent, "Edit Analyst");
  assert.deepEqual(calls.newChat, [], "opening an agent never starts a task");
  document.querySelector('[role="dialog"] button[aria-label="Close"]')?.click();
  await settle();
  const create = document.querySelector('button[aria-label="Create agent"]');
  assert.ok(create && create.closest("button[aria-expanded]") === null, "Create agent sits beside the Agents toggle, not inside it");
  assert.ok(create.closest(".opacity-0") === null, "section actions are visible without hovering");
  create.click();
  await until(() => document.getElementById("octopus-agent-preset-title")?.textContent === "Create agent", "Create agent never opened the creation dialog");
});

test("section titles are toggles that collapse and restore their rows", { timeout: 20_000 }, async () => {
  await show();
  const toggle = sectionToggle("Agents");
  assert.equal(toggle.tagName, "BUTTON");
  assert.equal(toggle.getAttribute("aria-expanded"), "true");
  toggle.click();
  await until(() => sectionToggle("Agents").getAttribute("aria-expanded") === "false" && agentRows().length === 0, "the Agents section never collapsed");
  sectionToggle("Agents").click();
  await until(() => agentRows().length === 2, "the Agents section never expanded");
});

test("dragging a task within the task list reorders it manually without touching projects, over a body-level overlay", { timeout: 20_000 }, async () => {
  const calls = await show();
  assert.deepEqual(taskTitles(), ["Task run-a", "Task run-b", "Task run-c"]);
  const item = (title) => [...document.querySelectorAll(".sidebar-sortable-item")].find((element) => element.querySelector(".octopus-task-row")?.getAttribute("aria-label").startsWith(title));
  const pointer = (target, type, x, y) => {
    const event = new w.MouseEvent(type, { button: 0, clientX: x, clientY: y, bubbles: true, cancelable: true });
    Object.defineProperties(event, { pointerId: { value: 1 }, isPrimary: { value: true } });
    target.dispatchEvent(event);
  };
  const source = item("Task run-c");
  pointer(source, "pointerdown", 100, 111);
  pointer(document, "pointermove", 100, 100); await settle();
  const overlay = document.querySelector(".sidebar-drag-overlay");
  assert.ok(overlay && !document.getElementById("root").contains(overlay), "the drag preview renders at document level");
  assert.ok(overlay.textContent.includes("Task run-c"));
  pointer(document, "pointermove", 100, 45); await settle();
  const dragging = document.querySelector("[data-sidebar-dragging]");
  assert.ok(dragging, "the source row is marked while it is dragged");
  assert.ok(item("Task run-a").style.transform, "rows make room for the dragged task");
  assert.deepEqual([dragging.style.transform, dragging.style.opacity], ["", ""], "the source row stays put and fully visible while the overlay moves");
  pointer(document, "pointerup", 100, 45); await settle();
  assert.deepEqual(calls.order.at(-1), ["run-c", "run-a", "run-b"]);
  assert.deepEqual(calls.pinned.at(-1), []);
  assert.deepEqual(calls.projects, [], "reordering a task never reassigns projects");
  await until(() => taskTitles().join() === "Task run-c,Task run-a,Task run-b", () => `the manual order was not applied: ${taskTitles()}`);
});

test("an empty project keeps its empty text during background reads, loads only before the first list and names a failed list read", async () => {
  const calls = await show();
  const workspace = calls.state.agentProjectWorkspace;
  const skeleton = () => document.querySelector('[role="status"][aria-label="Loading tasks…"]');
  const emptyText = () => [...document.querySelectorAll("div")].some((element) => element.textContent === "No tasks in this project");
  const alert = () => [...document.querySelectorAll('[role="alert"]')].find((element) => element.textContent.includes("Projection read failed: busy")) ?? null;
  calls.rerender({ agentProjectFilterEeId: "ee.empty", chatRunsPhase: "refreshing",
    agentProjectWorkspace: { ...workspace, projects: [...workspace.projects, { id: "ee.empty", name: "Empty", icon: "folder", color: "blue", parentId: "", chatRunIds: [] }] } });
  await settle();
  assert.ok(emptyText(), "a background read keeps the empty list on screen");
  assert.equal(skeleton(), null, "a background read never swaps the list for a loader");
  calls.rerender({ chatRunsPhase: "initial-loading" });
  await settle();
  assert.ok(skeleton(), "only the first read shows the loader");
  assert.equal(emptyText(), false);
  calls.rerender({ chatRunsPhase: "failed", chatRunListError: "Projection read failed: busy" });
  await settle();
  assert.ok(alert(), "a failed first read names its error");
  assert.deepEqual([emptyText(), skeleton()], [false, null], "a failed list is neither empty nor loading");
  calls.rerender({ agentProjectFilterEeId: "", chatRunsPhase: "degraded" });
  await settle();
  assert.deepEqual(taskTitles(), ["Task run-a", "Task run-b", "Task run-c"], "a failed background read keeps the last good rows");
  assert.ok(alert(), "and shows its error next to them");
});

test("source folders use the native picker, deduplicate additions, and preserve explicit Primary ordering", async () => {
  const { ProjectSourceFolders } = await compiled;
  let folders = [];
  const requests = [];
  let pickerResult = { canceled: false, paths: ["/workspace/one", "/workspace/two"] };
  window.octopusTransport = { selectClientDirectory: async (request) => { requests.push(request); return pickerResult; } };
  const render = () => root.render(React.createElement(ProjectSourceFolders, { folders, disabled: false,
    onChange: (next) => { folders = next; render(); }, onError: (error) => assert.fail(error) }));
  const click = async (label) => {
    const button = [...document.querySelectorAll("button")].find((item) => item.textContent === label || item.getAttribute("aria-label") === label);
    assert.ok(button, label);
    button.click();
    await settle();
  };
  render();
  await settle();
  await click("Add folders Octopus can read and edit");
  assert.equal(requests[0].allowMultiple, true);
  assert.deepEqual(folders, ["/workspace/one", "/workspace/two"]);
  await click("Make primary");
  assert.deepEqual(folders, ["/workspace/two", "/workspace/one"]);
  pickerResult = { canceled: false, paths: ["/workspace/one", "/workspace/three"] };
  await click("Add folder");
  assert.deepEqual(folders, ["/workspace/two", "/workspace/one", "/workspace/three"]);
  await click("Remove /workspace/two");
  assert.deepEqual(folders, ["/workspace/one", "/workspace/three"]);
  pickerResult = { canceled: true, paths: [] };
  await click("Add folder");
  assert.deepEqual(folders, ["/workspace/one", "/workspace/three"]);
  delete window.octopusTransport;
});
