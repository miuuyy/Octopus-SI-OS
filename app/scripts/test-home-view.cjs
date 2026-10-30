const assert = require("node:assert/strict");
const path = require("node:path");
const { test, after } = require("node:test");
const { build } = require("esbuild");
const { JSDOM } = require("jsdom");

const dom = new JSDOM('<div id="root"></div>', { url: "http://fixture.invalid", pretendToBeVisual: true });
const w = dom.window;
for (const key of ["window", "document", "HTMLElement", "Element", "SVGElement", "Node", "Event", "CustomEvent", "MouseEvent", "KeyboardEvent", "PointerEvent", "StorageEvent", "navigator", "getComputedStyle", "localStorage"]) {
  if (w[key] || key === "window") Object.defineProperty(globalThis, key, { configurable: true, value: key === "getComputedStyle" ? w[key].bind(w) : key === "window" ? w : w[key] });
}
globalThis.requestAnimationFrame = w.requestAnimationFrame.bind(w);
globalThis.cancelAnimationFrame = w.cancelAnimationFrame.bind(w);
const resizeObservers = new Set();
const stageSize = { width: 1200, height: 800 };
globalThis.ResizeObserver = w.ResizeObserver = class {
  constructor(callback) { this.callback = callback; }
  observe(target) { this.target = target; resizeObservers.add(this); }
  unobserve() { resizeObservers.delete(this); }
  disconnect() { resizeObservers.delete(this); }
};
globalThis.MutationObserver = w.MutationObserver;
w.matchMedia = () => ({ matches: false, addEventListener() {}, removeEventListener() {} });
w.HTMLElement.prototype.scrollTo = function ({ top }) { this.scrollTop = top; };
w.HTMLElement.prototype.getBoundingClientRect = function () {
  if (this.classList.contains("voice-widget-canvas")) return { x: 0, y: 0, left: 0, top: 0, right: stageSize.width, bottom: stageSize.height, ...stageSize };
  return { x: 600, y: 10, left: 600, top: 10, right: 632, bottom: 42, width: 32, height: 32 };
};
for (const name of ["clientWidth", "clientHeight"]) {
  Object.defineProperty(w.HTMLElement.prototype, name, { configurable: true, get() { return this.classList.contains("voice-widget-canvas") ? (name === "clientWidth" ? stageSize.width : stageSize.height) : 0; } });
}

const React = require("react");
const { createRoot } = require("react-dom/client");
const appRoot = path.join(__dirname, "..");
const stubbed = {
  ClientSurfaceRenderer: 'export function ClientSurfaceRenderer() { return null; } export function resetVoiceWidgetLayout() { return null; } export function restoreVoiceWidgetLayout() {}',
  ChatComposer: 'export function ChatComposer() { return createElement("div", { "data-composer": "" }); }',
  MessageContent: 'export function MessageContent({ text }) { return createElement("span", { "data-content": "" }, text); }',
  QueuedMessageList: "export function QueuedMessageList() { return null; }",
  OperatorRequestSurface: "export function OperatorRequestSurface() { return null; }",
  HomeCallControls: "export function HomeCallControls() { return null; }",
  VoiceOrbScene: "export function VoiceOrbScene() { return null; }",
  OctopusOceanScene: "export function OctopusOceanScene() { return null; }",
};
const bundle = (entry, names) => build({
  stdin: { contents: entry, resolveDir: appRoot, loader: "ts" },
  bundle: true, platform: "node", format: "cjs", write: false, logLevel: "silent",
  plugins: [{
    name: "home-stubs",
    setup(builder) {
      if (!names.length) return;
      builder.onResolve({ filter: new RegExp(`/(${names.join("|")})$`) }, (args) => ({ path: path.basename(args.path), namespace: "stub" }));
      builder.onLoad({ filter: /.*/, namespace: "stub" }, (args) => ({ loader: "js", contents: `import { createElement } from "react";\n${stubbed[args.path]}` }));
    },
  }],
  define: { "import.meta.env.DEV": "false", "import.meta.env.PROD": "true" },
  loader: { ".css": "empty", ".svg": "dataurl", ".png": "dataurl", ".webm": "dataurl", ".mp4": "dataurl" },
  external: ["react", "react-dom", "react-dom/client", "react/jsx-runtime", "motion/react", "@pierre/diffs", "@pierre/diffs/*", "shiki", "@shikijs/*", "mermaid", "katex"],
}).then((result) => {
  const module = { exports: {} };
  const heavy = new Proxy({}, { get: (_, name) => name === "__esModule" ? true : name === "preloadHighlighter" ? async () => {} : () => null });
  Function("require", "module", "exports", result.outputFiles[0].text)((id) => /^(@pierre\/diffs|shiki|@shikijs|mermaid|katex)/.test(id) ? heavy : require(id), module, module.exports);
  return module.exports;
});
const homeBundle = bundle('export { VoiceView, projectTaskProgress } from "./src/components/voice/VoiceView";', Object.keys(stubbed));
const widgetBundle = bundle('export { ClientSurfaceRenderer, resetVoiceWidgetLayout, restoreVoiceWidgetLayout } from "./src/client-surfaces/ClientSurfaceRenderer"; export { placeVoiceWidget } from "./src/client-surfaces/voiceWidgetPlacement"; export { buildVoiceWidgetDocuments } from "./src/components/voice/voiceWidgetDocuments";', []);

const container = document.getElementById("root");
const root = createRoot(container);
after(() => { root.unmount(); w.close(); });
const settle = (ms = 30) => new Promise((resolve) => setTimeout(resolve, ms));
const query = (selector) => container.querySelector(selector);
const noop = () => {};
const meter = { level: 0, waveform: [] };
const session = { phase: "idle", transcript: [], context: null, startedAtMs: 0, error: "", deviceError: "", inputDevices: [], outputDevices: [], selectedInputId: "", selectedOutputId: "",
  subscribeOutputMeter: () => noop, getOutputMeterSnapshot: () => meter, subscribeMeter: () => noop, getMeterSnapshot: () => meter, providerActivity: false,
  setSpeakerEnabled: noop, selectInputDevice: noop, selectOutputDevice: noop, refreshDevices: noop, toggleMute: noop, end: async () => {}, start: async () => {}, updateContext: async () => {} };
const home = { runId: "home", title: "Home", clientSurfaceRole: "home", chatInterfaceId: "home-client", pinnedArtifactIds: [], updatedAtMs: 1, writable: true };
const homeMessage = (id, role, eventKind, text, createdAtMs, extra = {}) => ({ id, runId: "home", interfaceId: "home-client", role, direction: role === "user" ? "in" : "out", eventKind, text, createdAtMs, attachments: [], ...extra });
const prompt = homeMessage("prompt", "user", "message", "Read the quarterly report", 1_000);
const reading = homeMessage("read", "assistant", "dynamic_tool", "", 1_001, { eventData: { tool: "workspace.read_file", call_id: "read", status: "in_progress", arguments: { path: "report.md" }, _activity_origin: { interface_role: "provider" } } });
const longReply = homeMessage("reply", "assistant", "message", Array.from({ length: 8 }, (_, index) => `Finding ${index + 1} of the quarterly report.`).join("\n"), 1_002, { terminal: true });
const workspace = (overrides = {}) => ({
  runsLoaded: true, runsError: "", activeRunId: "home", activeRun: home, runs: [home], loadingMessages: false, renderedMessages: [], declaredProviderInterfaces: [],
  busy: false, submitting: false, toolPresentations: {}, text: "", queuedTurns: [], providerInterfaces: [], attachments: [], dragOver: false,
  setText: noop, submit: noop, interruptActiveTurn: noop, respondToOperatorRequest: async () => {}, onDragLeave: noop, onDragOver: noop, dropFiles: noop,
  ...overrides,
});
async function showHome(chat, homeRunId = "home") {
  const { VoiceView } = await homeBundle;
  root.render(React.createElement(VoiceView, { accessProfile: "live", activity: { entries: [], total: 0 }, builtinWidgetSources: {}, homeRunId, installedWidgetIds: [],
    installedWidgetsReady: true, session, surfaces: [], topology: { projection: { mode: "live" }, environments: [] }, workerEnvironmentSurfaces: [], widgetsOpen: false, workspace: chat, viewMode: "voice" }));
  await settle();
}
const stage = () => query('[aria-label="Home latest response viewport"]');

test("Home opens behind its loading status until its own task is selected and loaded", { timeout: 5000 }, async () => {
  await showHome(workspace({ activeRunId: "", activeRun: null }));
  assert.ok(query('.voice-view.is-loading [role="status"][aria-label="Opening Home"]'));
  assert.ok(!stage());
  const pendingRuns = { runsLoaded: false, loadingRuns: true, activeRun: null, runs: [] };
  await showHome(workspace({ ...pendingRuns, loadingMessages: true }));
  assert.ok(query('[aria-label="Opening Home"]'));
  await showHome(workspace({ ...pendingRuns, loadingMessages: false }));
  assert.ok(query('[aria-label="Opening Home"]'));
  assert.doesNotMatch(container.textContent, /The Home connection has not been projected/);
  assert.ok(!stage());
  await showHome(workspace());
  assert.ok(!query('[aria-label="Opening Home"]'));
  assert.ok(stage());
  assert.match(container.textContent, /What would you like to do/);
  await showHome(workspace({ runsLoaded: false, runsError: "Projection read refused", activeRun: null, runs: [] }));
  assert.ok(!query('[aria-label="Opening Home"]'));
  assert.match(container.textContent, /The Home connection has not been projected/);
  await showHome(workspace({ activeRunId: "", activeRun: null, runs: [] }), "");
  assert.ok(!query('[aria-label="Opening Home"]'));
  assert.match(container.textContent, /The Home connection has not been projected/);
});

test("a working Home turn shows its live activity in a compact stage, and a long reply expands it", async () => {
  await showHome(workspace({ busy: true, renderedMessages: [prompt, reading] }));
  assert.ok(stage().classList.contains("is-compact") && stage().classList.contains("is-generating"));
  const live = query(".home-response-output--live .octopus-turn-activity");
  assert.equal(live.dataset.state, "working");
  assert.match(live.textContent, /report\.md/);
  await showHome(workspace({ busy: true, renderedMessages: [prompt, reading, { ...longReply, terminal: false, eventKind: "message_delta" }] }));
  assert.match(query(".home-response-output__copy").textContent, /Finding 8/);
  await showHome(workspace({ busy: true, renderedMessages: [prompt, reading, { ...longReply, terminal: false, eventData: { phase: "final_answer", lifecycle: "completed" } }] }));
  assert.match(query(".home-response-output__copy")?.textContent ?? "", /Finding 8/);
  await showHome(workspace({ busy: true, renderedMessages: [prompt, reading, longReply] }));
  assert.match(query(".home-response-output__copy")?.textContent ?? "", /Finding 8/);
  assert.equal(query(".home-response-output--live"), null);
  await showHome(workspace({ renderedMessages: [prompt, { ...reading, eventData: { ...reading.eventData, status: "completed" } }, longReply] }));
  assert.ok(stage().classList.contains("is-expanded") && !stage().classList.contains("is-compact"));
  assert.ok(!query(".home-response-output--live"));
  assert.match(stage().textContent, /Finding 8 of the quarterly report\./);
  await showHome(workspace({ submitting: true, busy: true, renderedMessages: [prompt, longReply] }));
  assert.doesNotMatch(stage().textContent, /Finding 8/);
});

test("stopping Home keeps the activity layout and scroll position while showing the interruption", async () => {
  const commentary = homeMessage("commentary", "assistant", "commentary", "Preparing the requested video", 1001);
  await showHome(workspace({ busy: true, renderedMessages: [prompt, commentary, reading] }));
  const viewport = query(".home-response-stage__viewport");
  const activity = query(".home-response-output");
  viewport.scrollTop = 75;
  const stopped = homeMessage("stopped", "assistant", "turn_control", "", 1003, { eventData: { action: "stop", status: "acknowledged" } });
  const interrupted = homeMessage("interrupted", "assistant", "turn_completed", "", 1004, { terminal: true, eventData: { turn: { status: "interrupted" } } });
  for (const [busy, terminal] of [[true, stopped], [false, interrupted]]) {
    await showHome(workspace({ busy, renderedMessages: [prompt, commentary, reading, terminal] }));
    assert.ok(stage().classList.contains("is-compact"), "Stop must not move the activity into the top-aligned answer layout");
    assert.equal(query(".home-response-output"), activity, "the existing activity stays mounted");
    assert.ok(activity.classList.contains("home-response-output--activity"), "activity spacing must survive the end of live execution");
    assert.equal(viewport.scrollTop, 75);
    assert.equal(stage().getAttribute("aria-busy"), "false");
    assert.match(activity.textContent, /Preparing the requested video/);
    assert.match(activity.textContent, /Turn interrupted before a final reply/);
  }
});

const widgetInput = { context: [], goals: [], memory: [], scheduled: [], tools: [],
  insights: { change: "+1%", changeDetail: "vs last week", label: "Leads", series: [1, 2], value: "12" },
  taskProgress: { completion: 50, failed: 0, running: 1, scheduled: 0, unseenDone: 0, waiting: 0 } };

test("task progress counts visible tasks and renders one scale-independent arc, with honest empty and complete states", async () => {
  const { projectTaskProgress } = await homeBundle;
  const { ClientSurfaceRenderer, buildVoiceWidgetDocuments } = await widgetBundle;
  const task = (id, overrides = {}) => ({ ...home, runId: id, eeId: "root", clientSurfaceRole: "task", archived: false,
    turnOpen: false, turnState: "idle", nextScheduledAtMs: 0, lastTerminalMessage: { text: "Finished" }, ...overrides });
  const topology = { environments: [{ id: "root", runs: [{ id: "writer", status: "active" }, { id: "memory", status: "settled" }] }] };
  const target = { environment: { id: "root" } };
  const counts = projectTaskProgress(topology, target, "home", [task("done"), task("working", { turnOpen: true, turnState: "working" }),
    task("waiting", { turnOpen: true, turnState: "waiting" }), task("failed", { turnState: "failed" }),
    task("scheduled", { nextScheduledAtMs: Date.now() + 60000 }), task("archived", { archived: true }),
    task("internal", { writable: false, lastTerminalMessage: null }), task("other", { eeId: "elsewhere" }), home]);
  assert.deepEqual(counts, { completion: 25, failed: 1, running: 1, scheduled: 1, unseenDone: 1, waiting: 1 });
  const show = async (taskProgress) => {
    const admitted = buildVoiceWidgetDocuments({ ...widgetInput, taskProgress }).find(row => row.document.id === "builtin.voice.task-progress");
    root.render(React.createElement(ClientSurfaceRenderer, { accessProfile: "live", admitted })); await settle();
  };
  await show(counts);
  assert.equal(query(".client-surface-donut__ring").getAttribute("pathLength"), "100");
  assert.equal(query(".client-surface-donut__ring").getAttribute("stroke-dasharray"), "25 100");
  await show({ ...counts, completion: 0 }); assert.equal(query(".client-surface-donut__ring"), null);
  await show({ ...counts, completion: 100 }); assert.equal(query(".client-surface-donut__ring").hasAttribute("stroke-dasharray"), false);
  await show(projectTaskProgress(topology, target, "home", [home]));
  assert.equal(query(".client-surface-donut"), null); assert.match(container.textContent, /No tasks yet/);
  root.render(null); await settle();
});

test("built-in Home widgets keep their declared slot classification", async () => {
  const { buildVoiceWidgetDocuments } = await widgetBundle;
  const slots = Object.fromEntries(buildVoiceWidgetDocuments(widgetInput).map(({ document }) => [document.id, document.target.slot]));
  assert.equal(slots["builtin.voice.live-context"], "right");
  assert.equal(slots["builtin.voice.insights"], "right");
  assert.ok(Object.values(slots).every((slot) => slot === "right"), JSON.stringify(slots));
});

test("built-in Home widgets stay admitted however many rows a run projects", async () => {
  const { buildVoiceWidgetDocuments } = await widgetBundle;
  const rows = Array.from({ length: 80 }, (_, index) => ({ label: `row ${index}` }));
  const documents = Object.fromEntries(buildVoiceWidgetDocuments({ ...widgetInput, context: rows, memory: rows, tools: rows })
    .map(({ document }) => [document.id, document]));
  const children = documents["builtin.voice.live-context"].render.root.children;
  assert.equal(children.length, 24);
  assert.deepEqual(children.at(-1), { type: "key_value", label: "More", tone: "muted", value: "57" });
});

test("saved widget positions survive slot changes and older layouts without a slot", async () => {
  const { ClientSurfaceRenderer, buildVoiceWidgetDocuments } = await widgetBundle;
  const documents = Object.fromEntries(buildVoiceWidgetDocuments(widgetInput).map((admitted) => [admitted.document.id, admitted]));
  const legacy = { x: 20, y: 20, width: 300, height: 240, stageWidth: 1200, stageHeight: 800, z: 1 };
  for (const id of ["builtin.voice.live-context", "builtin.voice.insights", "builtin.voice.task-progress"]) {
    w.localStorage.setItem("octopus.voice.widget-layout.v3", JSON.stringify({ [id]: { ...legacy, ...(id.endsWith("task-progress") ? { slot: "left" } : {}) } }));
    root.render(null);
    await settle();
    root.render(React.createElement("div", { className: "voice-widget-canvas" },
      React.createElement(ClientSurfaceRenderer, { accessProfile: "live", admitted: documents[id], voicePlacement: { slot: "right", index: 0, count: 1 } })));
    await settle(60);
    const widget = query(`[data-surface-document="${id}"]`);
    assert.equal(widget.style.left, "20px", id);
    assert.equal(widget.style.top, "20px", id);
    assert.equal(JSON.parse(w.localStorage.getItem("octopus.voice.widget-layout.v3"))[id].slot, "right", id);
  }
  root.render(null);
  await settle();
});

test("automatic placement uses free space on both sides, protects the conversation and stays inside small canvases", async () => {
  const { placeVoiceWidget } = await widgetBundle;
  let seed = 91;
  const random = () => ((seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0) / 4294967296);
  const overlap = (a, b) => a.x < b.x + b.width && a.x + a.width > b.x && a.y < b.y + b.height && a.y + a.height > b.y;
  const stage = { width: 1200, height: 800 }, size = { width: 180, height: 120 };
  const center = { x: 420, y: 240, width: 360, height: 320 };
  const composer = { x: 300, y: 650, width: 600, height: 140 };
  const occupied = [];
  for (let index = 0; index < 6; index++) {
    const rect = { ...size, ...placeVoiceWidget(stage, size, occupied, [composer], random) };
    assert.ok(rect.x >= 8 && rect.x + rect.width <= 1192 && rect.y >= 8 && rect.y + rect.height <= 792);
    assert.ok(!overlap(rect, center) && !overlap(rect, composer));
    assert.ok(occupied.every(peer => !overlap(rect, peer)), "new widgets must use remaining free space");
    occupied.push(rect);
  }
  assert.ok(occupied.some(rect => rect.x < 420) && occupied.some(rect => rect.x > 780));
  assert.equal(new Set(occupied.map(rect => rect.y)).size, occupied.length);
  const different = Array.from({ length: 8 }, () => placeVoiceWidget(stage, size, [], [], random));
  assert.ok(new Set(different.map(rect => `${rect.x},${rect.y}`)).size > 1);
  const narrow = placeVoiceWidget({ width: 240, height: 160 }, { width: 224, height: 144 }, [], [], random);
  assert.deepEqual(narrow, { x: 8, y: 8 });
});

test("widgets opened together reserve their positions, keep them on remount, and restore a reset", async () => {
  const { ClientSurfaceRenderer, buildVoiceWidgetDocuments, resetVoiceWidgetLayout, restoreVoiceWidgetLayout } = await widgetBundle;
  root.render(null); await settle();
  w.localStorage.removeItem("octopus.voice.widget-layout.v3");
  const template = buildVoiceWidgetDocuments(widgetInput)[0];
  const widgets = ["builtin.voice.first", "builtin.voice.second", "agent.widget"].map((id, index) => ({
    ...template,
    ...(index === 2 ? { sourceArtifactId: "published-widget", sourceRunId: "home", sourceInterfaceId: "home-client" } : {}),
    document: { ...template.document, id, target: { ...template.document.target, layout: {
      ...template.document.target.layout, min_width: 180, preferred_width: 180, max_width: 180,
      min_height: 120, preferred_height: 120, max_height: 120,
    } } },
  }));
  const render = async () => {
    root.render(React.createElement("div", { className: "voice-widget-canvas" }, widgets.map((admitted, index) =>
      React.createElement(ClientSurfaceRenderer, { key: admitted.document.id, accessProfile: "live", admitted, voicePlacement: { slot: "right", index, count: widgets.length } }))));
    await settle(60);
  };
  const positions = () => [...container.querySelectorAll("[data-voice-slot]")].map(node => ({ x: Number.parseFloat(node.style.left), y: Number.parseFloat(node.style.top) }));
  Object.assign(stageSize, { width: 0, height: 0 });
  await render();
  assert.ok([...container.querySelectorAll("[data-voice-slot]")].every(node => node.style.visibility === "hidden"));
  Object.assign(stageSize, { width: 1200, height: 800 });
  for (const observer of resizeObservers) if (observer.target.classList.contains("voice-widget-canvas")) observer.callback();
  await settle(60);
  assert.ok([...container.querySelectorAll("[data-voice-slot]")].every(node => node.style.visibility === "visible"));
  const first = positions();
  for (let i = 0; i < first.length; i++) for (let j = 0; j < i; j++) {
    assert.ok(Math.abs(first[i].x - first[j].x) >= 188 || Math.abs(first[i].y - first[j].y) >= 128, "simultaneous mounts must not choose overlapping rectangles");
  }
  root.render(null); await settle(); await render();
  assert.deepEqual(positions(), first);
  const snapshot = resetVoiceWidgetLayout("live"); await settle();
  restoreVoiceWidgetLayout("live", snapshot); await settle();
  assert.deepEqual(positions(), first, "Undo layout reset must restore the saved positions");
  root.render(null); await settle();
});
