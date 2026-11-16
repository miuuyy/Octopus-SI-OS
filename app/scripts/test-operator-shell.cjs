const assert = require("node:assert/strict");
const path = require("node:path");
const { test, after } = require("node:test");
const { build } = require("esbuild");
const { JSDOM } = require("jsdom");

const dom = new JSDOM('<div id="root"></div>', { url: "http://fixture.invalid", pretendToBeVisual: true });
const w = dom.window;
for (const key of ["window", "document", "HTMLElement", "HTMLInputElement", "Element", "SVGElement", "Node", "Event", "CustomEvent", "MouseEvent", "KeyboardEvent", "StorageEvent", "localStorage", "sessionStorage", "navigator", "getComputedStyle", "MutationObserver", "AbortController", "customElements", "HTMLTemplateElement", "ShadowRoot", "DocumentFragment"]) {
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
  if (this.dataset.paneTabId) return rect([...this.parentElement.children].indexOf(this) * 100, 0, 100, 30);
  if (this.classList.contains("workspace-pane-manager") && globalThis.paneManagerWidth) return rect(0, 0, globalThis.paneManagerWidth, 900);
  return rect(0, 0, 1440, 900);
};
globalThis.BroadcastChannel = class extends BroadcastChannel { constructor(name) { super(name); this.unref(); } };
const heldLocks = new Set();
Object.defineProperty(w.navigator, "locks", { configurable: true, value: {
  request: async (name, callback) => { heldLocks.add(name); try { return await callback(); } finally { heldLocks.delete(name); } },
  query: async () => ({ held: [...heldLocks].map((name) => ({ name })) }),
} });
function memoryIndexedDb() {
  const stores = new Map();
  const later = (callback) => setTimeout(callback, 0);
  let settled = Promise.resolve();
  const database = {
    createObjectStore: (name) => { stores.set(name, new Map()); }, close() {},
    transaction(name) {
      const store = stores.get(name), writes = [];
      const transaction = { aborted: false, error: null, abort() { transaction.aborted = true; later(() => transaction.onabort?.()); },
        objectStore: () => ({
          get(key) {
            const request = {};
            settled = settled.then(() => new Promise((finish) => later(() => {
              request.result = store.has(key) ? structuredClone(store.get(key)) : undefined;
              request.onsuccess?.();
              later(() => { if (!transaction.aborted) { for (const [value, writeKey] of writes) store.set(writeKey, structuredClone(value)); transaction.oncomplete?.(); } finish(); });
            })));
            return request;
          },
          put(value, key) { writes.push([value, key]); },
        }) };
      return transaction;
    },
  };
  return { open() { const request = { result: database }; later(() => { if (!stores.has("queues")) request.onupgradeneeded?.(); request.onsuccess?.(); }); return request; } };
}
globalThis.indexedDB = memoryIndexedDb();
const errors = [];
w.addEventListener("error", (event) => errors.push(event.error ?? event.message));
const consoleError = console.error;
console.error = (...args) => {
  if (String(args[0]).includes("Cannot update a component")) errors.push(new Error(args.map(String).join(" ")));
  consoleError(...args);
};

const stubView = `import { createElement } from "react";
export function view(kind, props, attributes = {}) {
  return createElement("div", { "data-stub": kind, ref: (element) => { if (element) element.props = props; }, ...attributes });
}`;
const stubs = [
  [/components\/ChatView\.tsx$/, `import { createElement, Fragment } from "react"; import { view } from "stub-view"; import { WorkspacePaneHeaderActions } from "./shell/WorkspacePaneHeaderActions";
    export function ChatView(props) {
      if (globalThis.chatViewGate) throw globalThis.chatViewGate;
      return createElement(Fragment, null, view("chat", props, { "data-header-mode": props.headerMode, "data-commands-active": String(props.commandsActive), "data-run": props.workspace.activeRunId }),
        createElement(WorkspacePaneHeaderActions, null, createElement("button", { "aria-label": "Share " + (props.workspace.activeRunId || "new task"), type: "button" })));
    }`],
  [/components\/shell\/HomeSurface\.tsx$/, `import { view } from "stub-view"; export function HomeSurface(props) { if (globalThis.homeViewGate) throw globalThis.homeViewGate; return view("home", props, { "data-commands-active": String(props.commandsActive) }); }`],
  [/components\/activity\/InboxSurface\.tsx$/, `import { view } from "stub-view"; export function InboxSurface(props) { return view("inbox", props); }`],
  [/components\/EventLogView\.tsx$/, `import { view } from "stub-view"; export function EventLogView(props) { return view("event-log", props); }`],
  [/components\/(TopologyStage|Inspector|RegistryView|ApplicationsView|DriversView|WorkerEnvironmentsView)\.tsx$/, (file) => `import { view } from "stub-view"; export function ${path.basename(file, ".tsx")}(props) { return view("${path.basename(file, ".tsx")}", props); }`],
  [/components\/CustomProjectionSurface\.tsx$/, `import { view } from "stub-view"; export function CustomProjectionSurface(props) { return view("custom", props); } export function StaticSurface(props) { return view("static", props); }`],
  [/client-surfaces\/ClientSurfaceRenderer\.tsx$/, `import { view } from "stub-view"; export function ClientSurfaceRenderer(props) { return view("client-surface", props); }`],
  [/components\/scheduled\/LiveScheduledView\.tsx$/, `import { view } from "stub-view"; export function LiveScheduledView(props) { return view("scheduled", props); }`],
  [/components\/settings\/SettingsPage\.tsx$/, `import { view } from "stub-view"; export function SettingsPage(props) { return view("settings", props); }`],
  [/src\/shell\/surfaceRegistry\.ts$/, `import { useClientSurfaces as useRegistry } from "real-surface-registry"; export { builtinSurfaceDescriptors } from "real-surface-registry";
    export function useClientSurfaces(profile) { const state = useRegistry(profile); return globalThis.surfaceRegistryView ? globalThis.surfaceRegistryView(state) : state; }`],
  [/components\/CommandMenu\.tsx$/, `export function CommandMenu() { return null; }`],
  [/components\/voice\/VoiceCallPuck\.tsx$/, `export function VoiceCallPuck() { return null; }`],
  [/components\/shell\/RetainedTerminalTabs\.tsx$/, `export function RetainedTerminalTabs() { return null; }`],
  [/components\/files\/RetainedFilesTabs\.tsx$/, `export function RetainedFilesTabs() { return null; }`],
  [/components\/files\/FilesPaneDialogs\.tsx$/, `export function FilesPaneDialogs() { return null; }`],
  [/components\/worker-environment\/ChangesWorkspace\.tsx$/, `import { view } from "stub-view"; export { surfaceHasProjectedChanges } from "./changesProjection"; export function ChangesWorkspace(props) { return view("changes", props); }`],
  [/components\/voice\/useVoiceSession\.ts$/, `const session = { phase: "idle", error: "", deviceError: "", context: null, updateContext: async () => {} }; export function useVoiceSession() { return session; }`],
];
const heavy = /^(@pierre\/diffs|shiki|@shikijs|mermaid|katex)/;
const compiled = build({
  stdin: {
    contents: [
      'import "./src/components/icon-glyphs/shell"; import "./src/components/icon-glyphs/chat";',
      'export { OperatorShell } from "./src/components/shell/OperatorShell"; export { ClientPreferencesProvider } from "./src/clientPreferences";',
      'export { dispatchClientCommand } from "./src/clientActions"; export { dispatchOpenTaskSourcesPane, OPEN_TASK_SOURCES_PANE_EVENT } from "./src/components/chat/taskSourcesPane";',
      'export { dispatchOpenClientTaskActionsMenu } from "./src/shell/taskActionsMenu"; export { CLIENT_PANE_DRAG_MIME, clientWindowStateKey, clientWindowStateStorage } from "./src/shell/clientWindowModel";',
      'export { activeDestinationStorageKey } from "./src/shell/navigation";',
      'export { readSecondaryPaneState, writeSecondaryPaneState } from "./src/components/shell/operatorShellState"; export { SurfaceHost } from "./src/components/shell/SurfaceHost";',
      'export { OPEN_WORKER_ENVIRONMENT_PANE_EVENT } from "./src/components/worker-environment/workerEnvironmentPane";',
      'export { WorkspaceSurfacePane } from "./src/components/shell/WorkspaceSurfacePane"; export { WorkspacePaneManager } from "./src/components/shell/WorkspacePaneManager";',
      'export { WindowPaneControlsProvider } from "./src/components/shell/WindowPaneControls"; export { useWorkspaceCompactTab } from "./src/components/shell/WorkspaceCompactTabContext";',
    ].join("\n"),
    resolveDir: path.join(__dirname, ".."), loader: "tsx",
  },
  bundle: true, platform: "node", format: "cjs", write: false, logLevel: "silent",
  define: { "import.meta.env.DEV": "false", "import.meta.env.PROD": "true" },
  loader: { ".css": "empty", ".svg": "dataurl", ".png": "dataurl", ".woff2": "empty", ".wasm": "empty" },
  external: ["react", "react-dom", "react-dom/client", "react/jsx-runtime", "motion/react", "@pierre/diffs", "@pierre/diffs/*", "shiki", "@shikijs/*", "mermaid", "katex"],
  plugins: [{ name: "operator-shell-stubs", setup(builder) {
    builder.onResolve({ filter: /^stub-view$/ }, () => ({ path: "stub-view", namespace: "stub" }));
    builder.onLoad({ filter: /.*/, namespace: "stub" }, () => ({ contents: stubView, loader: "js" }));
    builder.onResolve({ filter: /^real-surface-registry$/ }, () => ({ path: path.join(__dirname, "../src/shell/surfaceRegistry.ts"), suffix: "?real" }));
    builder.onLoad({ filter: /\.(tsx?)$/ }, ({ path: file, suffix }) => {
      if (suffix === "?real") return undefined;
      const stub = stubs.find(([pattern]) => pattern.test(file));
      if (!stub) return undefined;
      return { contents: typeof stub[1] === "function" ? stub[1](file) : stub[1], loader: "js", resolveDir: path.dirname(file) };
    });
  } }],
}).then((result) => {
  const module = { exports: {} };
  const heavyStub = new Proxy({}, { get: (_, name) => name === "__esModule" ? true : name === "preloadHighlighter" ? async () => {} : () => null });
  Function("require", "module", "exports", result.outputFiles[0].text)((id) => heavy.test(id) ? heavyStub : require(id), module, module.exports);
  return module.exports;
});

const React = require("react");
const { createRoot } = require("react-dom/client");

const provider = {
  id: "provider.test", label: "Test provider", descriptorPath: "registry/interface/provider.test", sourceNames: [], supportedReasoningEfforts: ["medium"],
  models: [{ model: "model-a", displayName: "Model A", description: "", defaultReasoningEffort: "medium", supportedReasoningEfforts: ["medium"], showInPicker: true, isDefault: true }],
};
const runRow = (runId, extra = {}) => ({
  run_id: runId, title: `Task ${runId}`, chat_interface_id: `chat.${runId}`, client_surface_role: "task", writable: true, ee_id: "ee.root",
  provider_interface_id: provider.id, provider_bound_interface_id: provider.id, provider_model: "model-a", reasoning_effort: "medium", worker_interface_id: "worker.test",
  turn_open: false, turn_state: "idle", last_movement_seq: 2, updated_at_ms: 1_000, agent_id: null, agent_name: null, agent_icon: null, agent_color: null, ...extra,
});
const messageRow = (runId, seq, role, text, extra = {}) => ({
  id: `${runId}:${seq}`, run_id: runId, role, text, created_at_ms: seq * 100, movement_seq: seq, artifact_id: `${runId}.artifact.${seq}`,
  direction: role === "user" ? "in" : "out", interface_id: `chat.${runId}`, event_kind: "message", terminal: true, ...extra,
});
const movementArtifact = (row) => ({
  source: "movement", recorded_at_unix_ms: row.created_at_ms, run_id: row.run_id, interface_id: row.interface_id,
  from_interface_id: row.role === "user" ? row.interface_id : provider.id, from_interface_role: row.role === "user" ? "operator" : "provider",
  label: row.role === "user" ? "operator.message" : "provider.message", event_kind: row.event_kind, event_data: row.event_data ?? {}, text: row.text,
  error: false, terminal: true, will_retry: false, movement_seq: row.movement_seq, checkpoint_id: `checkpoint.${row.movement_seq}`, artifact_id: row.artifact_id, content_hash: "",
});

const writerAgent = {
  client_agent_id: "agent-writer", client_agent_name: "Writer", client_agent_icon: "default", client_agent_created_at_ms: 1, client_agent_updated_at_ms: 1,
  client_agent_deleted_at_ms: 0, agent_source_template: true, client_template_digest: "template-1", client_template_provider_interface_id: provider.id,
  client_template_provider_model: "model-a", client_template_reasoning_effort: "medium", client_template_application_ids: [], client_template_worker_environment_kinds: [],
};
const gate = () => { let open; const promise = new Promise((resolve) => { open = resolve; }); return { promise, open }; };

const subscribers = new Map();
let currentHost = null;
const emit = (name, ...args) => { for (const listener of [...subscribers.get(name) ?? []]) listener(...args); };
w.octopusTransport = new Proxy({}, { get: (_, name) => {
  if (typeof name !== "string" || name === "then") return undefined;
  if (currentHost?.transport[name]) return (...args) => { currentHost.calls.push([name, ...args]); return currentHost.transport[name](...args); };
  if (name.startsWith("on")) return (listener) => {
    if (!subscribers.has(name)) subscribers.set(name, new Set());
    subscribers.get(name).add(listener);
    return () => subscribers.get(name).delete(listener);
  };
  return (...args) => { currentHost?.calls.push([name, ...args]); return Promise.resolve(undefined); };
} });

function fakeHost() {
  const host = {
    seq: 2, calls: [], reads: [], turns: [], waiters: new Set(), accepting: new Map(), nativeMenuChoice: null, activity: [],
    runs: [runRow("run-1"), runRow("run-2"), runRow("home", { client_surface_role: "home", title: "Home", os_root_ee_id: "ee.root" })],
    messages: { "run-1": [messageRow("run-1", 1, "user", "Draft the report"), messageRow("run-1", 2, "assistant", "Here is the report.")], "run-2": [], home: [] },
    topology: { generation: 1, snapshotRaw: { projection: { mode: "lab" } } },
    surfaceDocuments: { generation: 1, root: "", documents: [], errors: [] },
    run: (runId) => host.runs.find((run) => run.run_id === runId),
    storedAtHandoff: [],
    called: (name) => host.calls.filter(([method]) => method === name).map(([, ...args]) => args),
    changed(kind) { if (currentHost === host) emit("onProjectionChanged", { kind }); },
    append(runId, role, text, extra = {}) {
      const seq = ++host.seq;
      host.messages[runId] = [...(host.messages[runId] ?? []), messageRow(runId, seq, role, text, extra)];
      host.run(runId).last_movement_seq = seq;
      setTimeout(() => { host.changed("ledger"); for (const waiter of [...host.waiters]) if (waiter()) host.waiters.delete(waiter); }, 0);
      return seq;
    },
  };
  host.transport = {
    projectionRead: async (request) => {
      host.reads.push(request.method);
      switch (request.method) {
        case "projection.operator.chats.list": await host.holdRuns?.promise; return { chats: structuredClone(host.runs), ...(request.params?.include_activity ? { activity: structuredClone(host.activity) } : {}) };
        case "projection.operator.chats.status": return { chats: host.runs.filter((run) => request.params.run_ids.includes(run.run_id)).map((run) => ({ ...run })) };
        case "projection.operator.chat.messages": return { messages: structuredClone(host.messages[request.params.run_id] ?? []) };
        case "projection.operator.chat.contexts": return { contexts: [{ ee_id: "ee.root", is_os_root: true, display_name: "Root", agent_catalog_digest: "catalog-1",
          workers: [{ id: "worker.test", label: "worker.test", provider_interface_ids: [provider.id], interface_policy: { [provider.id]: { models: ["model-a"] } }, ...writerAgent }] }] };
        case "projection.operator.os.scope": await host.holdScope?.promise; return { status: "ready", root_ee_id: "ee.root", home_run_id: "home", home_interface_id: "chat.home", root_environment: { ee_id: "ee.root" } };
        case "projection.client_surfaces.list": await host.holdSurfaces?.promise; return host.surfaceDocuments;
        case "projection.runtime.topology": return structuredClone(host.topology);
        case "projection.worker_environments.list": return { worker_environments: [] };
        case "projection.tools.list": return { tools: [] };
        case "projection.operator.providers.status": return { providers: [] };
        case "projection.marks.list": return { marks: [] };
        case "projection.descriptors.mark_kinds.list": return { descriptors: [] };
        default: return {};
      }
    },
    providerInterfaces: async () => ({ repoRoot: "/repo", providers: [structuredClone(provider)] }),
    defaults: async () => ({ endpoint: "http://runtime.invalid" }),
    clientExtensions: async () => ({ generation: 1, root: "", manifests: [], state: {}, stateStatus: "valid", lifecycle: {}, credentials: {}, actionStatuses: [], errors: [] }),
    readOperatorChatArtifacts: (request) => {
      const after = Math.max(request.afterSeq, request.cursor?.seq ?? -1);
      const batch = () => {
        const rows = (host.messages[request.runId] ?? []).filter((row) => row.movement_seq > after);
        return rows.length ? { artifacts: rows.map(movementArtifact), cursor: { offset: 0, seq: rows.at(-1).movement_seq } } : null;
      };
      const ready = batch();
      if (ready) return Promise.resolve(ready);
      return new Promise((resolve) => host.waiters.add(() => { const next = batch(); if (next) resolve(next); return Boolean(next); }));
    },
    submitChatTurn: async (request) => {
      host.turns.push(structuredClone(request));
      const runId = request.runId || "run-new";
      if (!request.runId) { host.runs.push(runRow(runId, { title: request.text })); host.messages[runId] = []; host.changed("runs"); await settle(2); }
      const seq = host.append(runId, "user", request.text, { id: request.messageId });
      return { runId, chatInterfaceId: `chat.${runId}`, messageVerdict: { admitted: { seq } }, outboundCursor: null, messageRecovered: false };
    },
    windowState: async () => ({ fullScreen: false, nativeControlInset: 0 }),
    registerClientWindowCloseGuard: async () => ({ windowId: host.windowId, generation: 1 }),
    pendingClientPaneTransfers: async () => [],
    pendingClientPaneSourceFinalizations: async () => [],
    pendingClientPaneDestinationTransfers: async () => host.pendingDestinations ?? [],
    beginClientPaneTransfer: () => ({ schema: "octopus.client-pane-transfer", schemaVersion: 1, sourceWindowId: host.windowId, transferId: `transfer-${host.calls.filter(([name]) => name === "beginClientPaneTransfer").length}` }),
    acceptClientPaneTransfer: async ({ transferId }) => host.accepting.get(transferId),
    showClientContextMenu: async ({ requestId }) => ({ requestId, id: host.nativeMenuChoice }),
    finalizeClientPaneTransfer: async ({ transferId }) => {
      host.storedAtHandoff.push(host.stored().tabs.map((tab) => tab.id));
      host.pendingDestinations = host.pendingDestinations?.filter((transfer) => transfer.transferId !== transferId);
    },
    acknowledgeClientPaneSourceFinalization: async () => { host.storedAtHandoff.push(host.stored().tabs.map((tab) => tab.id)); },
  };
  return host;
}

const container = document.getElementById("root");
let root = null;
after(() => { root?.unmount(); w.close(); console.error = consoleError; assert.deepEqual(errors, []); });
const settle = async (rounds = 8) => { for (let round = 0; round < rounds; round += 1) await new Promise((resolve) => setTimeout(resolve, 10)); };
const until = async (predicate, message) => {
  for (let attempt = 0; attempt < 300; attempt += 1) {
    if (predicate()) return;
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
  assert.fail(typeof message === "function" ? message() : message);
};

async function mount(host, { clientWindow = {}, seed, keepStorage = false, home } = {}) {
  const bundle = await compiled;
  const { OperatorShell, ClientPreferencesProvider } = bundle;
  root?.unmount();
  host.home = home ?? (keepStorage ? currentHost?.home : undefined) ?? `/home/${Math.random().toString(16).slice(2)}`;
  if (!keepStorage) { window.localStorage.clear(); window.sessionStorage.clear(); }
  currentHost = host;
  host.windowId = clientWindow.windowId ?? "main";
  host.windowScope = clientWindow.scope ?? "main";
  host.stored = () => {
    const surfaceId = bundle.clientWindowStateStorage(host.windowScope).getItem(bundle.clientWindowStateKey(host.windowScope, bundle.activeDestinationStorageKey("live"))) ?? "voice";
    const runId = surfaceId === "chat" ? stubIn("primary", "chat")?.dataset.run ?? "" : "";
    return bundle.readSecondaryPaneState("live", "home", host.windowScope, JSON.stringify([host.home, surfaceId, runId]));
  };
  seed?.(bundle, host);
  root = createRoot(container);
  const props = {
    accessProfile: "live", onLogOut() {},
    clientWindow: { schema: "octopus.client-window", schemaVersion: 1, primary: true, scope: "main", windowId: "main", ...clientWindow },
    runtimeHost: { phase: "ready", ready: true, available: true, ownership: "fixture", pid: null, home: host.home, sourceRegistry: "", runtimeBinary: "", repoRoot: "", launchCwd: "", launchCommand: "", launchEnv: {}, endpoint: "", error: null, updatedAtMs: 1 },
  };
  root.render(React.createElement(ClientPreferencesProvider, { profile: "live" }, React.createElement(OperatorShell, props)));
  await settle(12);
  return host;
}

const pane = (role) => document.querySelector(role === "primary" ? ".workspace-pane-primary" : ".workspace-pane-secondary");
const stubIn = (role, kind) => pane(role)?.querySelector(`[data-stub="${kind}"]`) ?? null;
const sidebarRow = (title) => [...document.querySelectorAll(".octopus-task-row")].find((row) => row.getAttribute("aria-label").startsWith(title)) ?? null;
const secondaryTabs = () => [...document.querySelectorAll('[aria-label="Secondary workspace tabs"] [role="tab"]')];
const titlebarContext = () => document.querySelector(".app-titlebar-surface-context")?.textContent ?? "";
const button = (label, scope = document) => scope.querySelector(`button[aria-label="${label}"]`);
const pointer = (target, type, x = 10, y = 10) => {
  const event = new w.MouseEvent(type, { button: 0, clientX: x, clientY: y, bubbles: true, cancelable: true });
  Object.defineProperties(event, { pointerId: { value: 1 }, isPrimary: { value: true } });
  target.dispatchEvent(event);
};
async function command(id) { (await compiled).dispatchClientCommand(id, "keyboard"); await settle(); }
async function openTask(runId) {
  emit("onOpenRunRequested", runId);
  await until(() => stubIn("primary", "chat")?.dataset.run === runId, () => `the primary pane never showed ${runId}: ${pane("primary")?.textContent}`);
}

const chatTab = (runId) => ({ id: `chat:octopus-os:${runId}`, target: { kind: "surface", runId, surfaceId: "chat", workspaceKey: "octopus-os" } });
const seedSecondary = (tabs, activeTabId = tabs[0]?.id ?? "", destination = { surfaceId: "voice" }, open = true) => ({ writeSecondaryPaneState }, host) => writeSecondaryPaneState("live", "home", host.windowScope, { activeTabId, open, tabs }, JSON.stringify([host.home, destination.surfaceId, destination.runId ?? ""]));
const storedSecondary = async () => currentHost.stored();
const commandsActive = (role) => stubIn(role, "chat")?.dataset.commandsActive ?? "";

test("the primary task owns the sidebar highlight and titlebar while commands follow the active or focused pane", { timeout: 20_000 }, async () => {
  await mount(fakeHost(), { seed: seedSecondary([chatTab("run-2")], chatTab("run-2").id, { surfaceId: "chat", runId: "run-1" }, false) });
  await openTask("run-1");
  assert.ok(!pane("secondary") || pane("secondary").getAttribute("aria-hidden") === "true", "the task restores its own closed side pane");
  await command("toggle_split");
  await until(() => stubIn("secondary", "chat")?.dataset.run === "run-2", "the restored side task never opened");
  assert.ok(document.querySelector(".app-titlebar-secondary-pane .workspace-surface-pane-header"), "the side pane's tabs live in the window header");
  assert.ok(document.querySelector(".workspace-pane-secondary .workspace-surface-pane-header") === null);
  assert.deepEqual([stubIn("primary", "chat").dataset.headerMode, stubIn("secondary", "chat").dataset.headerMode], ["window", "none"]);
  assert.equal(sidebarRow("Task run-1").getAttribute("aria-current"), "page");
  assert.equal(sidebarRow("Task run-2").getAttribute("aria-current"), null);
  assert.equal(titlebarContext(), "Task run-1");
  assert.deepEqual([commandsActive("primary"), commandsActive("secondary")], ["false", "true"]);
  assert.ok(document.querySelector('.app-titlebar-pane-actions button[aria-label="Share run-1"]'), "the primary surface's header actions live in the window titlebar");
  assert.ok(document.querySelector('.workspace-surface-pane-header button[aria-label="Share run-2"]'), "the side surface's header actions live in its pane header");
  pointer(stubIn("primary", "chat"), "pointerdown"); await settle();
  assert.deepEqual([commandsActive("primary"), commandsActive("secondary")], ["true", "false"]);
  pointer(stubIn("secondary", "chat"), "pointerdown"); await settle();
  assert.deepEqual([commandsActive("primary"), commandsActive("secondary")], ["false", "true"]);
  assert.equal(titlebarContext(), "Task run-1", "the titlebar keeps naming the primary surface while the side pane is active");
  button("Focus this workspace").click(); await settle();
  assert.ok(document.querySelector(".workspace-pane-manager.is-secondary-focused"));
  assert.ok(document.querySelector(".app-titlebar-surface-context") === null, "a blocked primary pane gives up the titlebar context");
  assert.ok(document.querySelector(".app-titlebar-pane-actions").hidden, "and its titlebar actions");
  pointer(pane("primary"), "pointerdown"); await settle();
  assert.deepEqual([commandsActive("primary"), commandsActive("secondary")], ["false", "true"], "commands stay with the focused side pane while the primary pane is blocked");
  button("Restore split workspace").click(); await settle();
  assert.equal(titlebarContext(), "Task run-1");
  pointer(stubIn("primary", "chat"), "pointerdown"); await settle();
  assert.deepEqual([commandsActive("primary"), commandsActive("secondary")], ["true", "false"]);
  pointer(stubIn("secondary", "chat"), "pointerdown"); await settle();
  await command("open_settings");
  assert.deepEqual([commandsActive("primary"), commandsActive("secondary")], ["false", "false"], "open Settings own every command");
});

test("dictation recovery opens its recording in existing Voice settings and Back restores the task", { timeout: 20_000 }, async () => {
  await mount(fakeHost());
  await openTask("run-1");
  w.dispatchEvent(new w.CustomEvent("octopus:client-command", { detail: { id: "open_settings", source: "surface",
    settingsDestination: { sectionId: "voice-audio", dictationRecordingId: "recording-123" } } }));
  await until(() => document.querySelector('[data-stub="settings"]')?.props.recordingId === "recording-123", "recording destination did not reach Settings");
  assert.equal(document.querySelector('[data-stub="settings"]').props.sectionId, "voice-audio");
  await command("navigate_back");
  await until(() => !document.querySelector('[data-stub="settings"]'), "Back did not close Settings");
  assert.equal(stubIn("primary", "chat").dataset.run, "run-1");
});

test("a pane shows a task only while its workspace holds that exact run", { timeout: 20_000 }, async () => {
  const host = await mount(fakeHost());
  await openTask("run-1");
  await command("open_side_task");
  await until(() => stubIn("secondary", "chat")?.dataset.run === "run-1", "the side task never opened");
  assert.deepEqual(secondaryTabs().map((tab) => tab.textContent), ["Task run-1"]);
  assert.ok(secondaryTabs()[0].querySelector(".glyph-chat"), "a lone task tab still shows its strip with the task glyph");
  stubIn("secondary", "chat").props.workspace.selectRun("run-2");
  await until(() => !stubIn("secondary", "chat"), "the side pane kept rendering a task its tab does not name");
  assert.equal(pane("secondary").querySelector('[role="status"]').getAttribute("aria-label"), "Opening task…");
  assert.deepEqual(secondaryTabs().map((tab) => tab.textContent), ["Opening task"]);
  secondaryTabs()[0].dispatchEvent(new w.MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: 5, clientY: 5 })); await settle();
  assert.deepEqual([...document.querySelectorAll('[aria-label="Tab actions"] [role="menuitem"]')].map((item) => item.textContent), ["Close", "Close other tabs", "Close tabs to the right"], "a tab whose run is not ready cannot move");
  document.dispatchEvent(new w.KeyboardEvent("keydown", { key: "Escape", bubbles: true })); await settle();
  secondaryTabs()[0].dispatchEvent(new w.MouseEvent("mousedown", { button: 0, bubbles: true })); await settle();
  await until(() => stubIn("secondary", "chat")?.dataset.run === "run-1", "selecting the tab never restored its run");
  stubIn("primary", "chat").props.workspace.selectRun("run-2");
  await until(() => !stubIn("primary", "chat"), "the primary pane kept rendering a task its destination does not name");
  assert.equal(pane("primary").querySelector('[role="status"]').getAttribute("aria-label"), "Opening task…");
  sidebarRow("Task run-2").click();
  await until(() => stubIn("primary", "chat")?.dataset.run === "run-2", "the sidebar never opened run-2");
  assert.deepEqual(host.called("beginClientPaneTransfer"), []);
});

test("right-pane navigation opens the current task without changing the left pane, including from Triggers", { timeout: 20_000 }, async () => {
  await mount(fakeHost());
  await openTask("run-1");
  await command("toggle_split");
  await until(() => button("Side task", pane("secondary")), "the launcher did not offer the current task");
  button("Side task", pane("secondary")).click();
  await until(() => stubIn("secondary", "chat")?.dataset.run === "run-1", "the current left task could not open on the right");
  assert.equal(stubIn("primary", "chat").dataset.run, "run-1");

  button("New tab").click(); await settle();
  pane("secondary").querySelector('.workspace-pane-launcher-item[aria-haspopup="menu"]').click(); await settle();
  document.querySelector('[data-action-id="surface:octopus-os:scheduled"]').click();
  await until(() => stubIn("secondary", "scheduled"), "Triggers did not open on the right");
  assert.equal(typeof stubIn("secondary", "scheduled").props.onSelectChat, "function");
  stubIn("secondary", "scheduled").props.onSelectChat("run-1");
  await until(() => stubIn("secondary", "chat")?.dataset.run === "run-1", "the trigger's existing task did not open on the right");
  assert.equal(stubIn("primary", "chat").dataset.run, "run-1");
  await until(() => secondaryTabs().map((tab) => tab.textContent).join() === "Task run-1,Triggers", () => `the launcher was not replaced by the surface it opened: ${secondaryTabs().map((tab) => tab.textContent)}`);

  stubIn("primary", "chat").props.workspace.selectRun("run-2");
  await until(() => !stubIn("primary", "chat"), "the primary task did not enter its loading state");
  pointer(pane("primary"), "pointerdown"); await settle();
  button("New tab").click();
  await until(() => pane("secondary").querySelector(".workspace-pane-launcher"), "the left task's loading state blocked opening a right tab");
});

test("a new side task tab becomes the run its first message creates", { timeout: 20_000 }, async () => {
  const host = await mount(fakeHost());
  await openTask("run-1");
  await command("toggle_split");
  await until(() => document.querySelector('.workspace-pane-launcher-item[aria-label="New task"]'), "the empty side pane offers no launcher");
  document.querySelector('.workspace-pane-launcher-item[aria-label="New task"]').click();
  await until(() => stubIn("secondary", "chat"), "the new task tab never rendered its task");
  assert.deepEqual(secondaryTabs().map((tab) => tab.textContent), ["New task"]);
  const workspace = () => stubIn("secondary", "chat").props.workspace;
  assert.equal(workspace().activeRunId, "");
  workspace().setText("Plan the launch");
  await until(() => workspace().canSubmit, () => `the side task never became sendable: ${workspace().submitBlockReason}`);
  void workspace().submit();
  await until(() => secondaryTabs().map((tab) => tab.textContent).join() === "Plan the launch", () => `the tab never adopted the created run: ${secondaryTabs().map((tab) => tab.textContent)}`);
  assert.equal(stubIn("secondary", "chat").dataset.run, "run-new");
  assert.deepEqual(host.turns.map((turn) => [turn.runId, turn.text]), [[undefined, "Plan the launch"]]);
  assert.deepEqual((await storedSecondary()).tabs.map((tab) => tab.target), [chatTab("run-new").target]);
  assert.equal(stubIn("primary", "chat").dataset.run, "run-1", "the primary task is untouched");
});

test("restored side tabs are dropped only after the registry and task list that would resolve them have loaded", { timeout: 20_000 }, async () => {
  const host = fakeHost();
  host.holdRuns = gate();
  host.holdSurfaces = gate();
  const notes = { id: "surface:octopus-os:notes", target: { kind: "surface", runId: "", surfaceId: "notes", workspaceKey: "octopus-os" } };
  await mount(host, { seed: seedSecondary([chatTab("run-gone"), chatTab("run-1"), notes], chatTab("run-1").id) });
  await until(() => secondaryTabs().length === 3, () => `restored tabs were not shown while loading: ${secondaryTabs().map((tab) => tab.textContent)}`);
  host.holdSurfaces.open(); host.holdSurfaces = null;
  await until(() => secondaryTabs().length === 2, "a tab for a surface the loaded registry lacks was kept");
  await settle();
  assert.equal(secondaryTabs().length, 2, "chat tabs wait for the task list");
  host.holdRuns.open(); host.holdRuns = null;
  await until(() => secondaryTabs().length === 1, () => `a tab for a run the loaded task list lacks was kept: ${secondaryTabs().map((tab) => tab.textContent)}`);
  assert.deepEqual(secondaryTabs().map((tab) => tab.textContent), ["Task run-1"]);
  assert.deepEqual((await storedSecondary()).tabs.map((tab) => tab.id), [chatTab("run-1").id]);
});

test("task sources open as one Sources tab per task, prefer the loaded history, and survive the shell losing its surface", { timeout: 20_000 }, async () => {
  const host = fakeHost();
  host.messages["run-1"].push(messageRow("run-1", 3, "assistant", "octopus pricing", { event_kind: "web_search" }));
  host.run("run-1").last_movement_seq = 3; host.seq = 3;
  await mount(host);
  await openTask("run-1");
  const { dispatchOpenTaskSourcesPane, OPEN_TASK_SOURCES_PANE_EVENT } = await compiled;
  const row = (label) => ({ activity: "Attached to the conversation", glyph: "file", id: `attachment:${label}`, label, meta: "text/markdown" });
  const sources = () => [...document.querySelectorAll('.workspace-pane-secondary ul[aria-label="Sources"] > li strong')].map((label) => label.textContent);
  dispatchOpenTaskSourcesPane("run-1", [row("stale.md")]);
  await until(() => sources().length, "the Sources tab never listed anything");
  assert.deepEqual(sources(), ["Web search"], "a task whose history is loaded lists its live sources");
  assert.deepEqual(secondaryTabs().map((tab) => tab.textContent), ["Sources"]);
  assert.ok(secondaryTabs()[0].querySelector(".glyph-viewAll"));
  dispatchOpenTaskSourcesPane("run-2", [row("notes.md")]);
  await until(() => sources().join() === "notes.md", () => `a task without loaded history did not list its snapshot: ${sources()}`);
  assert.deepEqual(secondaryTabs().map((tab) => tab.textContent), ["Sources", "Sources"]);
  dispatchOpenTaskSourcesPane("run-2", [row("again.md")]);
  window.dispatchEvent(new w.CustomEvent(OPEN_TASK_SOURCES_PANE_EVENT, { detail: { schemaVersion: 2, runId: "run-1", sources: [] } }));
  await settle();
  assert.equal(secondaryTabs().length, 2, "a task reopens its own Sources tab and malformed requests are ignored");
  const { layout, ...storedTabs } = await storedSecondary();
  assert.deepEqual(storedTabs, { version: 3, open: false, activeTabId: "", tabs: [] }, "Sources tabs are never persisted");
  assert.equal(layout.activePane, "secondary", "the layout still records the user's active pane");
  globalThis.surfaceRegistryView = (state) => ({ ...state, surfaces: [] });
  dispatchOpenTaskSourcesPane("run-1", [row("stale.md")]);
  await until(() => !document.querySelector(".app-shell"), "the shell kept rendering without a surface");
  globalThis.surfaceRegistryView = null;
  dispatchOpenTaskSourcesPane("run-2", [row("notes.md")]);
  await until(() => document.querySelector(".app-shell") && sources().join() === "notes.md", "the shell never came back with its Sources tab");
  assert.deepEqual(errors, []);
});

const transfer = (transferId, target, extra = {}) => ({ schema: "octopus.client-pane-transfer", schemaVersion: 1, sourceWindowId: "window-b", transferId, sourcePane: "secondary", sourceTabId: "tab-b", target, ...extra });
const toasts = (host) => host.called("logClientEvent").map(([event]) => event.message);
const contextMenu = async (tab) => {
  tab.dispatchEvent(new w.MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: 5, clientY: 5 }));
  await settle(2);
  return [...document.querySelectorAll('[aria-label="Tab actions"] [role="menuitem"]')];
};
const menuItem = (items, label) => items.find((item) => item.textContent === label) ?? null;
const drag = async (target, dx = 40) => { pointer(target, "pointerdown", 10, 10); pointer(window, "pointermove", 10 + dx, 10); await settle(2); pointer(window, "pointerup", 10 + dx, 10); await settle(2); };
w.HTMLElement.prototype.setPointerCapture = function () {};
w.HTMLElement.prototype.hasPointerCapture = function () { return false; };
w.HTMLElement.prototype.releasePointerCapture = function () {};

test("a side tab moves to a new window only when it is transferable and holds no unsent work", { timeout: 20_000 }, async () => {
  const host = await mount(fakeHost());
  await openTask("run-1");
  await command("open_side_task");
  await until(() => stubIn("secondary", "chat")?.dataset.run === "run-1", "the side task never opened");
  const workspace = () => stubIn("secondary", "chat").props.workspace;
  workspace().setText("Unsent thought");
  await until(() => workspace().text === "Unsent thought", "the draft never landed");
  menuItem(await contextMenu(secondaryTabs()[0]), "Move to New Window").click(); await settle();
  assert.deepEqual(host.called("beginClientPaneTransfer"), [], "a drafted task stays in its window");
  assert.ok(toasts(host).includes("Send or clear the current draft before moving this task."));
  await drag(secondaryTabs()[0].closest(".pane-tab"));
  assert.deepEqual(host.called("beginClientPaneTransfer"), [], "dragging a drafted task out never starts a transfer");
  workspace().setText("");
  await until(() => workspace().text === "", "the draft never cleared");
  menuItem(await contextMenu(secondaryTabs()[0]), "Move to New Window").click(); await settle();
  assert.deepEqual(host.called("beginClientPaneTransfer"), [[{ sourcePane: "secondary", sourceTabId: chatTab("run-1").id, target: chatTab("run-1").target }]]);
  assert.deepEqual(host.called("detachClientPane"), [[{ transferId: "transfer-1" }]]);
  await drag(secondaryTabs()[0].closest(".pane-tab"));
  assert.deepEqual(host.called("beginClientPaneTransfer")[1], [{ sourcePane: "secondary", sourceTabId: chatTab("run-1").id, target: chatTab("run-1").target }], "dragging the tab out of the strip begins the same transfer");
  const { dispatchOpenTaskSourcesPane } = await compiled;
  dispatchOpenTaskSourcesPane("run-1", []);
  await until(() => secondaryTabs().length === 2, "the Sources tab never opened");
  assert.ok(!menuItem(await contextMenu(secondaryTabs()[1]), "Move to New Window"), "Sources tabs belong to their window");
});

test("the source window releases a moved tab only while it still holds that target and removes it once the destination finalizes", { timeout: 20_000 }, async () => {
  const host = await mount(fakeHost());
  await openTask("run-1");
  await command("open_side_task");
  await until(() => stubIn("secondary", "chat")?.dataset.run === "run-1", "the side task never opened");
  const sourceLayout = (await storedSecondary()).layout;
  const completion = (transferId, runId) => ({ ...transfer(transferId, chatTab(runId).target), sourceWindowId: "main", sourceTabId: chatTab("run-1").id });
  menuItem(await contextMenu(secondaryTabs()[0]), "Move to New Window").click(); await settle();
  emit("onClientPaneTransferCompleted", completion("transfer-1", "run-2")); await settle();
  menuItem(await contextMenu(secondaryTabs()[0]), "Move to New Window").click(); await settle();
  assert.deepEqual(host.called("detachClientPane"), [[{ transferId: "transfer-1" }], [{ transferId: "transfer-2" }]]);
  emit("onClientPaneTransferCompleted", completion("transfer-2", "run-1"));
  await settle();
  assert.deepEqual(host.called("acknowledgeClientPaneTransfer"), [[{ released: false, transferId: "transfer-1" }], [{ released: true, transferId: "transfer-2" }]]);
  assert.ok(toasts(host).includes("The source changed during handoff. It was retained and the destination move was cancelled."));
  assert.deepEqual(secondaryTabs().map((tab) => tab.textContent), ["Task run-1"], "a released tab stays until the destination finalizes");
  assert.deepEqual((await storedSecondary()).tabs.map((tab) => tab.id), [chatTab("run-1").id]);
  emit("onClientPaneTransferFinalized", completion("transfer-2", "run-1"));
  await until(() => host.called("acknowledgeClientPaneSourceFinalization").length === 1, "the source never acknowledged finalization");
  assert.deepEqual(host.called("acknowledgeClientPaneSourceFinalization"), [[{ transferId: "transfer-2" }]]);
  assert.deepEqual(host.storedAtHandoff, [[]], "the source forgets the tab durably before acknowledging the handoff");
  await until(() => secondaryTabs().length === 0, "the finalized tab stayed in the source window");
  assert.deepEqual(await storedSecondary(), { version: 3, open: false, activeTabId: "", tabs: [], layout: { ...sourceLayout, focused: false, activePane: "primary" } });
  assert.deepEqual(host.called("windowControl"), [], "the primary window never closes itself");
});

test("an independent window hands over its whole surface from the titlebar and closes once the destination finalizes", { timeout: 20_000 }, async () => {
  const host = await mount(fakeHost(), { clientWindow: { primary: false, scope: "window-a", windowId: "window-a" } });
  await until(() => titlebarContext() === "Home", () => `unexpected titlebar: ${titlebarContext()}`);
  const voice = { kind: "surface", runId: "", surfaceId: "voice", workspaceKey: "octopus-os" };
  await drag(document.querySelector(".app-titlebar-surface-context"));
  assert.deepEqual(host.called("beginClientPaneTransfer"), [[{ sourcePane: "primary", sourceTabId: "client-window:window-a", target: voice }]]);
  const whole = (transferId, target) => ({ ...transfer(transferId, target), sourceWindowId: "window-a", sourcePane: "primary", sourceTabId: "client-window:window-a" });
  emit("onClientPaneTransferCompleted", whole("transfer-1", voice));
  emit("onClientPaneTransferCompleted", whole("t-2", { ...voice, surfaceId: "dashboard", workspaceKey: "dashboard" }));
  await settle();
  assert.deepEqual(host.called("acknowledgeClientPaneTransfer"), [[{ released: true, transferId: "transfer-1" }], [{ released: false, transferId: "t-2" }]]);
  await command("toggle_split");
  await until(() => document.querySelector(".workspace-pane-launcher"), "the side pane never opened");
  pointer(stubIn("primary", "home"), "pointerdown"); await settle();
  await drag(document.querySelector(".app-titlebar-surface-context"));
  assert.equal(host.called("beginClientPaneTransfer").length, 1, "a window with an open side pane is not one surface");
  emit("onClientPaneTransferCompleted", whole("t-3", voice));
  await settle();
  assert.deepEqual(host.called("acknowledgeClientPaneTransfer")[2], [{ released: false, transferId: "t-3" }]);
  await command("toggle_split");
  emit("onClientPaneTransferFinalized", whole("transfer-1", voice));
  await until(() => host.called("windowControl").length === 1, "the emptied window never closed");
  assert.deepEqual(host.called("windowControl"), [["close"]]);
  const primary = await mount(fakeHost());
  await drag(document.querySelector(".app-titlebar-surface-context"));
  assert.deepEqual(primary.called("beginClientPaneTransfer"), [], "the main window never hands itself over");
});

test("a destination window stages an accepted tab, commits or resumes it, and rolls back what the host refuses", { timeout: 20_000 }, async () => {
  const resumed = fakeHost();
  resumed.holdScope = gate();
  resumed.pendingDestinations = [{ ...transfer("t-pending", chatTab("run-2").target), committed: true }];
  await mount(resumed);
  assert.deepEqual(resumed.called("resumeClientPaneTransfer"), [], "a pending destination waits for its exact Home owner");
  resumed.holdScope.open(); resumed.holdScope = null;
  await until(() => resumed.called("finalizeClientPaneTransfer").length === 1, () => `the pending committed transfer never finalized: ${JSON.stringify({ resume: resumed.called("resumeClientPaneTransfer"), commit: resumed.called("commitClientPaneTransfer"), reject: resumed.called("rejectClientPaneTransfer"), toasts: toasts(resumed) })}`);
  assert.deepEqual([resumed.called("resumeClientPaneTransfer"), resumed.called("commitClientPaneTransfer")], [[[{ transferId: "t-pending" }]], []]);
  await until(() => secondaryTabs().map((tab) => tab.textContent).join() === "Task run-2", "the resumed tab never resolved its task title");

  const host = await mount(fakeHost());
  await openTask("run-1");
  emit("onClientPaneTransferAccepted", transfer("t-1", chatTab("run-2").target));
  await until(() => host.called("finalizeClientPaneTransfer").length === 1, "the accepted transfer never finalized");
  assert.deepEqual(host.called("commitClientPaneTransfer"), [[{ transferId: "t-1" }]]);
  assert.deepEqual(host.storedAtHandoff, [[chatTab("run-2").id]], "the destination persists the tab before it finalizes");
  assert.equal(stubIn("secondary", "chat").dataset.run, "run-2");

  host.transport.commitClientPaneTransfer = async () => { throw new Error("source vanished"); };
  emit("onClientPaneTransferAccepted", transfer("t-2", chatTab("run-1").target));
  await until(() => host.called("rejectClientPaneTransfer").length === 1, "the refused transfer was never rejected");
  assert.deepEqual(host.called("rejectClientPaneTransfer"), [[{ transferId: "t-2" }]]);
  await until(() => secondaryTabs().map((tab) => tab.textContent).join() === "Task run-2", () => `the refused tab was not rolled back: ${secondaryTabs().map((tab) => tab.textContent)}`);
  assert.ok(toasts(host).includes("source vanished"));
  assert.deepEqual((await storedSecondary()).tabs.map((tab) => tab.id), [chatTab("run-2").id]);
  emit("onClientPaneTransferAccepted", transfer("t-3", chatTab("run-2").target));
  await until(() => host.called("rejectClientPaneTransfer").length === 2, "the refused transfer of an open tab was never rejected");
  await settle();
  assert.deepEqual(secondaryTabs().map((tab) => tab.textContent), ["Task run-2"], "rolling back a transfer onto an open tab keeps that tab");
  assert.deepEqual((await storedSecondary()).tabs.map((tab) => tab.id), [chatTab("run-2").id]);
});

test("dropping a pane dragged from another window accepts exactly that transfer", { timeout: 20_000 }, async () => {
  const host = await mount(fakeHost());
  const { CLIENT_PANE_DRAG_MIME } = await compiled;
  const drop = (payload, type = "drop") => {
    const event = new w.Event(type, { bubbles: true, cancelable: true });
    const dataTransfer = { types: [CLIENT_PANE_DRAG_MIME], dropEffect: "none", getData: (mime) => mime === CLIENT_PANE_DRAG_MIME ? JSON.stringify(payload) : "" };
    Object.defineProperty(event, "dataTransfer", { value: dataTransfer });
    document.querySelector(".app-shell").dispatchEvent(event);
    return { event, dataTransfer };
  };
  const payload = (sourceWindowId, transferId) => ({ schema: "octopus.client-pane-transfer", schemaVersion: 1, sourceWindowId, transferId });
  const over = drop(payload("window-b", "t-1"), "dragover");
  assert.deepEqual([over.event.defaultPrevented, over.dataTransfer.dropEffect], [true, "move"]);
  drop(payload("main", "t-own")); drop({ schema: "other" }); await settle();
  assert.deepEqual(host.called("acceptClientPaneTransfer"), [], "own-window and foreign payloads are never accepted");
  host.accepting.set("t-2", transfer("t-1", chatTab("run-1").target));
  drop(payload("window-b", "t-2")); await settle();
  assert.ok(toasts(host).includes("Client pane transfer acceptance does not match the dragged source."));
  assert.equal(secondaryTabs().length, 0);
  host.accepting.set("t-1", transfer("t-1", chatTab("run-1").target));
  drop(payload("window-b", "t-1"));
  await until(() => host.called("finalizeClientPaneTransfer").length === 1, "the dropped transfer never finalized");
  assert.deepEqual(host.called("acceptClientPaneTransfer"), [[{ transferId: "t-2" }], [{ transferId: "t-1" }]]);
  await until(() => secondaryTabs().map((tab) => tab.textContent).join() === "Task run-1", "the dropped tab never showed its task");
});

test("a window opened on a target starts focused on it with the sidebar collapsed and never reopens the consumed target", { timeout: 20_000 }, async () => {
  const peer = { primary: false, scope: "window-a", windowId: "window-a", initialTarget: chatTab("run-1").target };
  await mount(fakeHost(), { clientWindow: peer });
  await until(() => stubIn("secondary", "chat")?.dataset.run === "run-1", "the initial target never opened");
  assert.equal(commandsActive("secondary"), "true");
  assert.ok(document.querySelector(".workspace-pane-manager.is-secondary-focused"));
  assert.ok(document.querySelector(".app-shell.is-sidebar-closed"));
  button("Restore split workspace").click(); await settle();
  assert.equal(commandsActive("secondary"), "true", "the target's pane stays active once the split is restored");
  button("Close Task run-1 tab").click();
  await until(() => secondaryTabs().length === 0, "the initial tab never closed");
  button("Show sidebar").click(); await settle();
  assert.ok(document.querySelector(".app-shell.is-sidebar-open"));
  await mount(fakeHost(), { clientWindow: peer, keepStorage: true });
  await settle();
  assert.equal(secondaryTabs().length, 0, "a reloaded window does not reopen its consumed initial target");
  await openTask("run-1");
  assert.equal(secondaryTabs().length, 0, "returning to the exact task does not recreate the consumed target");
  assert.ok(document.querySelector(".app-shell.is-sidebar-open"), "a stored sidebar choice outlives the window default");

  await mount(fakeHost());
  await openTask("run-1");
  assert.ok(document.querySelector(".app-shell.is-sidebar-open"));
  button("Hide sidebar").click(); await settle();
  await mount(fakeHost(), { keepStorage: true });
  await openTask("run-1");
  assert.ok(document.querySelector(".app-shell.is-sidebar-closed"));
});

test("task actions open a task in a new window from the popover and from the native menu", { timeout: 20_000 }, async () => {
  const host = await mount(fakeHost());
  await openTask("run-1");
  const target = { initialTarget: { kind: "surface", runId: "run-2", surfaceId: "chat", workspaceKey: "octopus-os" } };
  button("Task actions for Task run-2").click();
  await until(() => document.querySelector('[role="menu"].task-actions-menu'), "the task actions menu never opened");
  [...document.querySelectorAll('.task-actions-menu [role="menuitem"]')].find((item) => item.textContent.includes("Open in new window")).click();
  await until(() => host.called("openClientWindow").length === 1, "the popover never opened a window");
  assert.deepEqual(host.called("openClientWindow"), [[target]]);
  host.nativeMenuChoice = "open";
  sidebarRow("Task run-2").dispatchEvent(new w.MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: 30, clientY: 40 }));
  await until(() => host.called("openClientWindow").length === 2, "the native menu never opened a window");
  assert.deepEqual(host.called("showClientContextMenu")[0][0].position, { x: 30, y: 40 });
  assert.ok(host.called("showClientContextMenu")[0][0].items.some((item) => item.id === "open" && item.label === "Open in new window"));
  assert.deepEqual(host.called("openClientWindow")[1], [target]);
});

test("pin and side-task commands act only on the active pane's ready task", { timeout: 20_000 }, async () => {
  await mount(fakeHost());
  const pinned = () => [...document.querySelectorAll(".app-shell-sidebar-slot button[aria-expanded]")].some((toggle) => toggle.textContent === "Pinned");
  await command("open_side_task");
  await command("toggle_pin_task");
  assert.ok(!pane("secondary") || !stubIn("secondary", "chat"), "Home has no task to open beside it");
  await openTask("run-1");
  assert.equal(pinned(), false, "Home has no task to pin");
  await command("toggle_pin_task");
  await until(() => pinned(), "the active task was never pinned");
  await command("toggle_pin_task");
  await until(() => !pinned(), "the active task was never unpinned");
  await command("open_side_task");
  await until(() => stubIn("secondary", "chat")?.dataset.run === "run-1", "the side task never opened");
  stubIn("secondary", "chat").props.workspace.selectRun("run-2");
  await until(() => !stubIn("secondary", "chat"), "the side pane kept an unready task");
  await command("toggle_pin_task");
  assert.equal(pinned(), false, "a task whose run is not ready cannot be pinned");
});

test("pins and preferences written by another window reach this window's sidebar", { timeout: 20_000 }, async () => {
  const host = fakeHost();
  host.runs.push(runRow("run-internal", { title: "Internal run", writable: false }));
  await mount(host);
  await openTask("run-1");
  const titles = () => [...document.querySelectorAll(".octopus-task-row")].map((row) => row.getAttribute("aria-label").split(",")[0]);
  const store = (key, value) => {
    window.localStorage.setItem(key, JSON.stringify(value));
    window.dispatchEvent(new w.StorageEvent("storage", { key, newValue: JSON.stringify(value), storageArea: window.localStorage }));
  };
  assert.deepEqual(titles(), ["Task run-1", "Task run-2"]);
  store("octopus.shell.pinnedTasks.v2", ["run-2"]);
  await until(() => [...document.querySelectorAll(".app-shell-sidebar-slot button[aria-expanded]")].some((toggle) => toggle.textContent === "Pinned"), "another window's pin never reached the sidebar");
  assert.deepEqual(titles(), ["Task run-2", "Task run-1"]);
  store("octopus.client-preferences.v1", { showInternalRuns: true });
  await until(() => titles().includes("Internal run"), () => `another window's preference never reached the sidebar: ${titles()}`);
});

test("the Inbox lists projected activity in the sidebar, selects it in place and opens a task mark beside the inbox", { timeout: 20_000 }, async () => {
  const host = fakeHost();
  host.activity = [{ id: "attention:7", kind: "message", title: "Message — Task run-2", name: "message", run_id: "run-2", client_surface_role: "task", worker: "worker.test",
    agent_id: null, agent_name: null, agent_icon: null, agent_color: null, artifact_id: "artifact:7", movement_id: "movements:7", request_id: null, seq: 7, created_at_ms: Date.now(), excerpt: "Task run-2 finished." }];
  await mount(host);
  await until(() => button("Inbox, 1 unread"), "the rail never counted the unread mark");
  button("Inbox, 1 unread").click();
  await until(() => stubIn("primary", "inbox"), "the Inbox surface never opened");
  const row = () => document.querySelector('.app-shell-sidebar-slot [aria-label="Inbox"] [role="button"]');
  await until(() => row(), "the sidebar never listed the mark");
  row().click(); await settle();
  assert.equal(stubIn("primary", "inbox").props.selected?.id, "attention:7");
  await until(() => button("Inbox"), "selecting the mark never marked it read");
  stubIn("primary", "inbox").props.onOpen(stubIn("primary", "inbox").props.selected);
  await until(() => stubIn("secondary", "chat")?.dataset.run === "run-2", "opening the mark never showed its task beside the inbox");
  assert.ok(stubIn("primary", "inbox"));
});

test("a worker environment tab shows the code comments the loaded task's assistant wrote for that room's run", { timeout: 20_000 }, async () => {
  const host = fakeHost();
  const room = (id, runId) => ({ worker_environment_id: id, run_id: runId, ee_id: "ee.root", worker_environment_kind: "worktree", status: "active", lifecycle: "active",
    surfaces: [{ id: "changes", kind: "changes", label: "Changes", mode: "direct", status: "ready", protocol: "git.worktree", root: "/repo" }] });
  host.topology = { generation: 1, snapshotRaw: { projection: { mode: "lab" } }, eeRaw: { ee_instances: [{ ee_id: "ee.root", template_name: "root" }] },
    runsRaw: { runs: [{ run_id: "run-1", ee_id: "ee.root", worker: "worker.test", status: "idle" }, { run_id: "run-2", ee_id: "ee.root", worker: "worker.test", status: "idle" }] },
    runCheckpointsRaw: { run_checkpoints: [] }, interfacesRaw: { interfaces: [] }, bindingsRaw: { bindings: [] }, artifactsRaw: { artifacts: [] }, marksRaw: { marks: [] },
    workerEnvironmentsRaw: { worker_environments: [room("room-1", "run-1"), room("room-2", "run-2")] }, movementsRaw: { movements: [] }, runDetailsRaw: {} };
  host.messages["run-1"].push(messageRow("run-1", 3, "assistant", 'Reviewed.\n::code-comment{title="Bound path" body="Keep the exact root." file="src/a.ts" start=4}'));
  host.run("run-1").last_movement_seq = 3; host.seq = 3;
  await mount(host);
  await openTask("run-1");
  const { OPEN_WORKER_ENVIRONMENT_PANE_EVENT } = await compiled;
  const open = (workerEnvironmentId) => window.dispatchEvent(new w.CustomEvent(OPEN_WORKER_ENVIRONMENT_PANE_EVENT, { detail: { schemaVersion: 1, workerEnvironmentId, surfaceId: "changes", panelKind: "changes" } }));
  const comments = () => stubIn("secondary", "changes")?.props.assistantCodeComments.map((comment) => [comment.title, comment.path, comment.startLine]);
  open("room-1");
  await until(() => comments()?.length, () => `the changes panel never received the task's code comments: ${JSON.stringify(comments())} ${pane("secondary")?.textContent}`);
  assert.deepEqual(comments(), [["Bound path", "src/a.ts", 4]]);
  open("room-2");
  await until(() => stubIn("secondary", "changes")?.props.workerEnvironmentId === "room-2", "the second room never opened");
  assert.deepEqual(comments(), [], "a room whose run is not loaded gets no comments from another task");
});

test("chat and Home surfaces show a content loading state while their renderer loads, and a side pane without tabs keeps the chat header", { timeout: 20_000 }, async () => {
  const chatGate = gate(), homeGate = gate();
  globalThis.homeViewGate = homeGate.promise;
  await mount(fakeHost());
  await until(() => pane("primary")?.querySelector('.surface-content-loading-state[aria-label="Opening Home"]'), "Home never showed its loading state");
  globalThis.homeViewGate = null; homeGate.open();
  await until(() => stubIn("primary", "home"), "Home never rendered");
  globalThis.chatViewGate = chatGate.promise;
  emit("onOpenRunRequested", "run-1");
  await until(() => pane("primary")?.querySelector('.surface-content-loading-state[aria-label="Loading task"]'), "the task never showed its loading state");
  globalThis.chatViewGate = null; chatGate.open();
  await until(() => stubIn("primary", "chat")?.dataset.run === "run-1", "the task never rendered");
  const { SurfaceHost } = await compiled;
  const surface = { id: "chat", label: "Tasks", glyph: "tasks", rendererKind: "builtin.chat", order: 50, source: "builtin", placements: ["home"], chrome: { topbar: "none", workspace: "immersive" } };
  const host = document.createElement("div");
  document.body.append(host);
  const paneRoot = createRoot(host);
  const props = { accessProfile: "live", surface, topology: { environments: [], interfaceIndex: {}, projection: { mode: "offline" } }, query: "", onQueryChange() {}, searchPlaceholder: "", searchFocusTick: 0,
    dashboardScope: "System", dashboardTopologyMode: "drilldown", dashboardFilters: {}, selected: null, chatWorkspace: { activeRunId: "run-9", activeRun: null }, homeRunId: "home", voiceSession: {},
    homeViewMode: "voice", paneRole: "secondary", commandsActive: true, inspectorOpen: false, onSelect() {}, onCloseInspector() {}, onOpenFiles() {}, voiceSurfaces: [], installedVoiceWidgetIds: [],
    installedVoiceWidgetsReady: true, builtinVoiceWidgetSources: {}, workerEnvironmentSurfaces: [] };
  paneRoot.render(React.createElement(SurfaceHost, props));
  await until(() => host.querySelector('[data-stub="chat"]'), "the standalone side surface never rendered");
  assert.equal(host.querySelector('[data-stub="chat"]').dataset.headerMode, "pane");
  paneRoot.render(React.createElement(SurfaceHost, { ...props, paneTabStripVisible: true })); await settle(2);
  assert.equal(host.querySelector('[data-stub="chat"]').dataset.headerMode, "none");
  paneRoot.render(React.createElement(SurfaceHost, { ...props, paneRole: "primary" })); await settle(2);
  assert.equal(host.querySelector('[data-stub="chat"]').dataset.headerMode, "window");
  paneRoot.unmount(); host.remove();
});

test("a side pane shows its tab strip only when tabs need it, lends a lone tab's drag to its surface and flips its focus control", { timeout: 20_000 }, async () => {
  const { WorkspaceSurfacePane, WorkspacePaneManager, WindowPaneControlsProvider, useWorkspaceCompactTab } = await compiled;
  const { motionValue } = require("motion/react");
  const host = document.createElement("div");
  document.body.append(host);
  let paneRoot = createRoot(host);
  const dragged = [], focusChanges = [];
  function Probe() {
    const drag = useWorkspaceCompactTab();
    return React.createElement("div", { id: "probe", "data-tab": drag?.tabId ?? "", onPointerDown: drag?.onPointerDown });
  }
  const render = async (tabs, { focused = false, canNativeDrag = () => true } = {}) => {
    paneRoot.render(React.createElement(WindowPaneControlsProvider, { host: null }, React.createElement(WorkspacePaneManager, {
      primary: "Primary", secondaryOpen: true, secondaryFocused: focused, secondaryWidthPercent: 40, resolvedWidthMotion: motionValue(0),
      onOpenChange() {}, onSecondaryFocusedChange() {}, onSecondaryWidthPercentChange() {},
      secondary: React.createElement(WorkspaceSurfacePane, {
        activeTabId: tabs[0].id, addActions: [], tabs, focused, canNativeDrag, onFocusedChange: (next) => focusChanges.push(next),
        onAddAction() {}, onClose() {}, onCloseOtherTabs() {}, onCloseTabsToRight() {}, onTabClose() {}, onTabReorder() {}, onTabSelect() {},
        onTabWindowPointerDown: (tab) => dragged.push(tab.id),
      }, React.createElement(Probe)),
    })));
    await settle(3);
  };
  const files = { id: "files", title: "Files", glyph: "folder" };
  await render([files]);
  assert.ok(host.querySelector('[role="tablist"]') === null, "a lone tab that does not ask for a strip gets none");
  assert.equal(host.querySelector("#probe").dataset.tab, "files");
  pointer(host.querySelector("#probe"), "pointerdown");
  assert.deepEqual(dragged, ["files"], "the surface can drag its lone tab out of the window");
  await render([files], { canNativeDrag: () => false });
  pointer(host.querySelector("#probe"), "pointerdown");
  assert.deepEqual(dragged, ["files"], "a lone tab that cannot leave lends no drag");
  await render([{ ...files, showWhenSingle: true }]);
  assert.deepEqual([...host.querySelectorAll('[role="tab"]')].map((tab) => tab.textContent), ["Files"]);
  assert.equal(host.querySelector("#probe").dataset.tab, "");
  await render([files, { id: "terminal", title: "Terminal", glyph: "terminal" }]);
  assert.deepEqual([...host.querySelectorAll('[role="tab"]')].map((tab) => tab.textContent), ["Files", "Terminal"]);
  assert.equal(host.querySelector("#probe").dataset.tab, "");
  const focus = () => host.querySelector(".workspace-surface-focus");
  assert.equal(focus().getAttribute("aria-label"), "Focus this workspace");
  const unfocusedIcon = focus().innerHTML;
  focus().click();
  assert.deepEqual(focusChanges, [true]);
  await render([files, { id: "terminal", title: "Terminal", glyph: "terminal" }], { focused: true });
  assert.equal(focus().getAttribute("aria-label"), "Restore split workspace");
  assert.notEqual(focus().innerHTML, unfocusedIcon, "a focused pane shows the restore icon instead of the expand icon");
  globalThis.paneManagerWidth = 600;
  paneRoot.unmount(); paneRoot = createRoot(host);
  await render([files, { id: "terminal", title: "Terminal", glyph: "terminal" }]);
  globalThis.paneManagerWidth = null;
  assert.ok(focus() === null, "a compact overlay pane has nothing to focus");
  assert.ok(button("Close secondary pane", host));
  paneRoot.unmount(); host.remove();
});

test("closing the last side tab immediately removes the pane and its content", { timeout: 20_000 }, async () => {
  const { WorkspacePaneManager } = await compiled;
  const { motionValue } = require("motion/react");
  const host = document.createElement("div");
  document.body.append(host);
  const paneRoot = createRoot(host);
  const width = motionValue(0);
  const render = async (open, content) => {
    paneRoot.render(React.createElement(WorkspacePaneManager, {
      primary: "Primary", secondaryOpen: open, secondaryFocused: false, secondaryWidthPercent: 40, resolvedWidthMotion: width,
      onOpenChange() {}, onSecondaryFocusedChange() {}, onSecondaryWidthPercentChange() {},
      secondary: content == null ? null : React.createElement("div", { "data-last-tab": true }, content),
    }));
    await settle(2);
  };
  try {
    await render(true, "Last tab");
    const openWidth = width.get();
    assert.ok(openWidth > 0);
    await render(false, null);
    assert.equal(host.querySelector(".workspace-pane-secondary"), null);
    assert.equal(width.get(), 0);
    assert.equal(host.querySelector("[data-last-tab]"), null);
    await render(true, "New tab");
    assert.equal(host.querySelector("[data-last-tab]")?.textContent, "New tab", "reopening must show the new tab");
  } finally {
    paneRoot.unmount(); host.remove();
  }
});

test("pane tabs reorder by dragging, scroll a newly opened tab into view once it has grown, and turn inert while they close", { timeout: 20_000 }, async () => {
  const { WorkspaceSurfacePane, WorkspacePaneManager, WindowPaneControlsProvider } = await compiled;
  const { motionValue } = require("motion/react");
  const host = document.createElement("div");
  document.body.append(host);
  const paneRoot = createRoot(host);
  const reorders = [], scrolled = [];
  const scroll = w.HTMLElement.prototype.scrollIntoView;
  w.HTMLElement.prototype.scrollIntoView = function (options) { scrolled.push([this.dataset.paneTabId, options?.behavior]); };
  const tab = (id) => ({ id, title: id, glyph: "terminal", showWhenSingle: true });
  const render = async (tabs, activeTabId) => {
    paneRoot.render(React.createElement(WindowPaneControlsProvider, { host: null }, React.createElement(WorkspacePaneManager, {
      primary: "Primary", secondaryOpen: true, secondaryFocused: false, secondaryWidthPercent: 40, resolvedWidthMotion: motionValue(0),
      onOpenChange() {}, onSecondaryFocusedChange() {}, onSecondaryWidthPercentChange() {},
      secondary: React.createElement(WorkspaceSurfacePane, {
        activeTabId, addActions: [], tabs, focused: false, onFocusedChange() {}, onAddAction() {}, onClose() {}, onCloseOtherTabs() {}, onCloseTabsToRight() {},
        onTabClose() {}, onTabReorder: (...ids) => reorders.push(ids), onTabSelect() {},
      }, "content"),
    })));
    await settle(2);
  };
  try {
    await render([tab("a"), tab("b")], "a");
    const chrome = (id) => host.querySelector(`[data-pane-tab-id="${id}"] .pane-tab`);
    pointer(chrome("a"), "pointerdown", 50, 10);
    pointer(document, "pointermove", 60, 10); await settle(2);
    pointer(document, "pointermove", 150, 10); await settle(2);
    pointer(document, "pointerup", 150, 10); await settle(2);
    assert.deepEqual(reorders, [["a", "b"]]);
    scrolled.length = 0;
    await render([tab("a"), tab("b"), tab("c")], "c");
    await until(() => scrolled.some(([id, behavior]) => id === "c" && behavior === "smooth"), () => `the grown tab was never scrolled into view: ${JSON.stringify(scrolled)}`);
    await render([tab("a"), tab("b")], "a");
    assert.ok(host.querySelector('[data-pane-tab-id="c"]')?.hasAttribute("inert"), "a closing tab cannot be focused or clicked");
    await until(() => !host.querySelector('[data-pane-tab-id="c"]'), "the closing tab never left");
  } finally {
    w.HTMLElement.prototype.scrollIntoView = scroll;
    paneRoot.unmount(); host.remove();
  }
});

