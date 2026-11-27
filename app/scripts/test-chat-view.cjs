const assert = require("node:assert/strict");
const path = require("node:path");
const { test, after } = require("node:test");
const { build } = require("esbuild");
const { JSDOM } = require("jsdom");

const dom = new JSDOM('<div id="host"></div><div id="root"></div>', { url: "http://fixture.invalid", pretendToBeVisual: true });
const w = dom.window;
for (const key of ["window", "document", "HTMLElement", "Element", "SVGElement", "Node", "Event", "CustomEvent", "MouseEvent", "KeyboardEvent", "WheelEvent", "MutationObserver", "navigator", "getComputedStyle", "localStorage"]) {
  Object.defineProperty(globalThis, key, { configurable: true, value: key === "getComputedStyle" ? w[key].bind(w) : key === "window" ? w : w[key] });
}
globalThis.requestAnimationFrame = w.requestAnimationFrame.bind(w);
globalThis.cancelAnimationFrame = w.cancelAnimationFrame.bind(w);
globalThis.IntersectionObserver = class { observe() {} unobserve() {} disconnect() {} };
w.matchMedia = () => ({ matches: false, addEventListener() {}, removeEventListener() {} });
w.scrollTo = () => {};

const geometry = new WeakMap();
const box = (element) => geometry.get(element) ?? {};
const setBox = (element, values) => geometry.set(element, { ...box(element), ...values });
Object.defineProperty(w.HTMLElement.prototype, "scrollTop", { configurable: true, get() { return box(this).scrollTop ?? 0; }, set(value) { setBox(this, { scrollTop: value }); } });
for (const name of ["scrollHeight", "clientHeight"]) {
  Object.defineProperty(w.HTMLElement.prototype, name, { configurable: true, get() { return box(this)[name] ?? 0; } });
}
w.HTMLElement.prototype.scrollTo = function (options) { this.scrollTop = options.top; };
w.HTMLElement.prototype.scrollIntoView = function () {};
let layoutWidth = 800;
w.HTMLElement.prototype.getBoundingClientRect = function () {
  if (this.classList.contains("chat-run-layout")) return { x: 0, y: 0, left: 0, top: 0, right: layoutWidth, bottom: 900, width: layoutWidth, height: 900 };
  return { x: 600, y: 10, left: 600, top: 10, right: 632, bottom: 42, width: 32, height: 32 };
};
globalThis.ResizeObserver = w.ResizeObserver = class { observe() {} unobserve() {} disconnect() {} };

const React = require("react");
const { createRoot } = require("react-dom/client");
const stubs = {
  name: "chat-view-stubs",
  setup(bundle) {
    bundle.onResolve({ filter: /\/(ChatComposer|MessageContent|OctopusSwarm)$/ }, (args) => ({ path: path.basename(args.path), namespace: "stub" }));
    bundle.onLoad({ filter: /.*/, namespace: "stub" }, (args) => ({
      loader: "js",
      contents: {
        ChatComposer: `import { createElement } from "react";
export function ChatComposer({ turnOpen, submissionMode }) { return createElement("div", { "data-composer": "", "data-turn-open": String(turnOpen), "data-submission-mode": submissionMode }); }`,
        MessageContent: `import { createElement } from "react";
export function MessageContent({ text, streaming }) { return createElement("span", { "data-content": "", "data-streaming": streaming ? "true" : undefined }, text); }`,
        OctopusSwarm: "export function OctopusSwarm() { return null; }",
      }[args.path],
    }));
  },
};
const compiled = build({
  stdin: {
    contents: [
      'export { ChatView } from "./src/components/ChatView";',
      'export { HomeAgentActivityView } from "./src/components/voice/HomeAgentActivityView";',
      'export { CollapsibleMessageBody } from "./src/components/chat/CollapsibleMessageBody";',
      'export { Glyph } from "./src/components/IconGlyphs";',
      'export { WorkspacePaneHeaderActionsProvider } from "./src/components/shell/WorkspacePaneHeaderActions";',
      'export { WorkspaceCompactTabProvider } from "./src/components/shell/WorkspaceCompactTabContext";',
      'export { ClientPreferencesProvider } from "./src/clientPreferences";',
      'import "./src/components/icon-glyphs/shell";',
    ].join("\n"),
    resolveDir: path.join(__dirname, ".."), loader: "ts",
  },
  bundle: true, platform: "node", format: "cjs", write: false, logLevel: "silent", plugins: [stubs],
  define: { "import.meta.env.DEV": "false", "import.meta.env.PROD": "true" },
  loader: { ".css": "empty", ".svg": "dataurl", ".png": "dataurl" },
  external: ["react", "react-dom", "react-dom/client", "react-dom/server", "react/jsx-runtime", "motion/react", "@pierre/diffs", "@pierre/diffs/*", "shiki", "@shikijs/*", "mermaid", "katex"],
}).then((result) => {
  const module = { exports: {} };
  const heavy = new Proxy({}, { get: (_, name) => name === "__esModule" ? true : name === "preloadHighlighter" ? async () => {} : () => null });
  Function("require", "module", "exports", result.outputFiles[0].text)((id) => /^(@pierre\/diffs|shiki|@shikijs|mermaid|katex)/.test(id) ? heavy : require(id), module, module.exports);
  return module.exports;
});

const host = document.getElementById("host");
const container = document.getElementById("root");
const root = createRoot(container);
after(() => { root.unmount(); w.close(); });
const settle = (ms = 20) => new Promise((resolve) => setTimeout(resolve, ms));
const query = (selector) => container.querySelector(selector);
const noop = () => {};
const run = (runId, title = `Task ${runId}`) => ({ runId, title, clientSurfaceRole: "task", pinnedArtifactIds: [], updatedAtMs: 1, archived: false, writable: true });
const user = (id, text, createdAtMs = 1) => ({ id, role: "user", text, createdAtMs, runId: "run-1" });
const answer = (id, text, createdAtMs = 2) => ({ id, role: "assistant", text, createdAtMs, eventKind: "message", terminal: true, runId: "run-1" });
const turns = (count, prefix) => Array.from({ length: count }, (_, index) => [user(`${prefix}u${index}`, `${prefix} question ${index}`, index * 10 + 1), answer(`${prefix}a${index}`, `${prefix} answer ${index}`, index * 10 + 2)]).flat();
const workspace = (overrides = {}) => {
  const activeRun = overrides.activeRun ?? run("run-1", "Quarterly report");
  return {
    accessProfile: "lab", accessMode: "project_access", activeRun, activeRunId: activeRun?.runId ?? "", runs: activeRun ? [activeRun] : [],
    renderedMessages: [user("u1", "Summarize the report"), answer("a1", "The report balances.")], activeMessageCount: 2,
    queuedTurns: [], pinnedMessages: [], pinnedContextItems: [], pinningArtifactIds: [], continuation: null, loadingMessages: false,
    busy: false, turnOpen: false, submitting: false, toolPresentations: {}, messageQuery: "", dragOver: false, attachments: [], applicationIds: [], driverNames: [],
    executionContexts: [], providerInterfaces: [], declaredProviderInterfaces: [], workerEnvironmentRooms: [], composerAgentPresets: [], artifactWriterAvailable: true,
    text: "", submitBlockReason: "", fileInputRef: { current: null }, error: "", runsError: "", executionContextError: "", newTaskAuthoringError: "", providerInterfaceError: "",
    setMessageQuery: noop, togglePinnedMessage: noop, unpinContextItem: noop, respondToOperatorRequest: async () => {}, onDragLeave: noop, onDragOver: noop, dropFiles: noop,
    ...overrides,
  };
};
const topology = { environments: [] };
async function show(chat, props = {}, wrap = (element) => element) {
  const { ChatView, ClientPreferencesProvider } = await compiled;
  root.render(React.createElement(ClientPreferencesProvider, { profile: "lab" },
    wrap(React.createElement(ChatView, { workspace: chat, searchFocusTick: 0, surfaces: [], topology, onOpenFiles: noop, ...props }))));
  await settle();
}
const press = async (target, type) => {
  target.dispatchEvent(new w.MouseEvent(type, { bubbles: true, cancelable: true }));
  await settle();
};

test("history loading hides stale messages while preserving the composer", async () => {
  await show(workspace({ loadingMessages: true }));
  const status = query('[role="status"][aria-label="Loading task"]');
  assert.ok(status);
  assert.equal(status.getAttribute("aria-busy"), "true");
  const composer = query("[data-composer]");
  assert.ok(composer && query(".thread-scroll-container"));
  assert.doesNotMatch(query(".thread-scroll-container").textContent, /The report balances/);
  await show(workspace());
  assert.ok(!query('[aria-label="Loading task"]'));
  assert.equal(query("[data-composer]"), composer);
  assert.match(query(".thread-scroll-container").textContent, /The report balances\./);
});

test("the thread and composer follow the task's turn: a busy task streams its reply, an open turn queues", async () => {
  const delta = { id: "d1", role: "assistant", text: "Totals are", createdAtMs: 3, eventKind: "message_delta", runId: "run-1", eventData: { phase: "final_answer" } };
  await show(workspace({ busy: true, turnOpen: true, renderedMessages: [user("u1", "Add totals"), delta] }));
  assert.equal(query('[data-message-id="d1"] [data-content]').dataset.streaming, "true");
  assert.deepEqual([query("[data-composer]").dataset.turnOpen, query("[data-composer]").dataset.submissionMode], ["true", "queue"]);
  await show(workspace({ busy: false, turnOpen: false, renderedMessages: [user("u1", "Add totals"), { ...delta, eventKind: "message", terminal: true }] }));
  assert.equal(query('[data-message-id="d1"] [data-content]').dataset.streaming, undefined);
  assert.deepEqual([query("[data-composer]").dataset.turnOpen, query("[data-composer]").dataset.submissionMode], ["false", "send"]);
});

test("switching tasks keeps the thread mounted and restores each task's reading place", async () => {
  const taskA = workspace({ activeRun: run("task-a"), renderedMessages: turns(4, "a"), activeMessageCount: 8 });
  const taskB = workspace({ activeRun: run("task-b"), renderedMessages: turns(2, "b"), activeMessageCount: 4 });
  await show(taskA);
  const list = query(".thread-scroll-container");
  setBox(list, { clientHeight: 500, scrollHeight: 2000 });
  list.dispatchEvent(new w.WheelEvent("wheel", { deltaY: -120, bubbles: true }));
  list.scrollTop = -400;
  list.dispatchEvent(new w.Event("scroll"));
  await settle(45);
  const composer = query("[data-composer]");
  await show({ ...taskB, loadingMessages: true });
  assert.equal(query("[data-composer]"), composer);
  assert.doesNotMatch(query(".thread-scroll-container").textContent, /[ab] answer/);
  await show(taskB);
  assert.ok(query(".thread-scroll-container") === list, "a task switch must not remount the thread");
  assert.equal(list.scrollTop, 0);
  await show({ ...taskA, loadingMessages: true });
  list.scrollTop = 0;
  list.dispatchEvent(new w.Event("scroll"));
  await show(taskA);
  assert.equal(query("[data-composer]"), composer);
  assert.equal(list.scrollTop, -400);
});

test("a pane-hosted task drags from its title, and a window-hosted task keeps its title and controls inside the chat area", async () => {
  const { WorkspacePaneHeaderActionsProvider, WorkspaceCompactTabProvider } = await compiled;
  const drags = [];
  await show(workspace(), { headerMode: "pane" }, (element) => React.createElement(WorkspaceCompactTabProvider, { value: { onPointerDown: (event) => drags.push(event.type) } }, element));
  const title = query("[data-thread-title]");
  assert.match(title.textContent, /Quarterly report/);
  await press(title, "pointerdown");
  assert.deepEqual(drags, ["pointerdown"], "a pane header title is the pane's drag handle");
  const actions = [...container.querySelectorAll('button[aria-label="Task actions"], [data-task-summary-trigger]')];
  assert.equal(actions.length, 2);
  assert.ok(actions.every((button) => button.closest(".no-drag")), "header actions stay clickable inside a draggable header");
  await show(workspace(), { headerMode: "window" }, (element) => React.createElement(WorkspacePaneHeaderActionsProvider, { host }, element));
  assert.match(query("[data-thread-title]").textContent, /Quarterly report/);
  assert.ok(query("[data-task-summary-trigger]"), "the summary toggle stays inside the chat header");
  assert.ok(query('button[aria-label="Task actions"]'), "task actions stay inside the chat header");
  assert.ok(query('button[aria-label="Toggle secondary pane"][aria-pressed="false"]'), "split toggle stays inside the chat header");
  assert.ok(!host.querySelector("[data-task-summary-trigger]"), "window-hosted chat actions are not portaled out of the chat");
  root.render(null);
  await settle();
});

test("opening the summary in a mid-width thread shifts the conversation aside; a narrow thread overlays it", async () => {
  const { WorkspacePaneHeaderActionsProvider } = await compiled;
  const transform = () => query(".thread-scroll-container").firstElementChild.style.transform || "none";
  for (const [width, shifted] of [[1200, true], [800, false]]) {
    layoutWidth = width;
    root.render(null);
    await settle();
    await show(workspace({ activeRun: run(`summary-${width}`) }), { headerMode: "window" }, (element) => React.createElement(WorkspacePaneHeaderActionsProvider, { host }, element));
    await press(query("[data-task-summary-trigger]"), "click");
    await settle(450);
    assert.equal(transform(), shifted ? "translateX(-158px)" : "none", `${width}px`);
    assert.equal(query(".task-summary-surface") !== null, shifted);
    await press(query("[data-task-summary-trigger]"), "click");
    await settle(450);
    assert.match(transform(), /^(none|translateX\(0px\))$/);
  }
  layoutWidth = 800;
});

test("Home activity shows the Home task read-only with its own empty state", async () => {
  const { HomeAgentActivityView, ClientPreferencesProvider } = await compiled;
  const showHome = async (chat) => {
    root.render(React.createElement(ClientPreferencesProvider, { profile: "lab" },
      React.createElement(HomeAgentActivityView, { workspace: chat, searchFocusTick: 0, surfaces: [], topology, onOpenFiles: noop })));
    await settle();
  };
  await showHome(workspace());
  const section = query('section[aria-label="Home activity"]');
  assert.match(section.textContent, /The report balances\./);
  assert.ok(!query("[data-composer]"), "Home activity has no composer");
  await showHome(workspace({ activeRun: null, renderedMessages: [], activeMessageCount: 0 }));
  assert.doesNotMatch(query('section[aria-label="Home activity"]').textContent, /No activity yet/);
  assert.ok(!query(".octopus-chat-home"));
});

test("a long message body clamps to its lines and expands to its full height without a measured animation", async () => {
  const { CollapsibleMessageBody } = await compiled;
  const tall = Object.getOwnPropertyDescriptor(w.HTMLElement.prototype, "scrollHeight");
  Object.defineProperty(w.HTMLElement.prototype, "scrollHeight", { configurable: true, get() { return 2000; } });
  try {
    root.render(React.createElement(CollapsibleMessageBody, { maxLines: 20, text: "Long body" },
      React.createElement("p", null, "Long body ", React.createElement("a", { href: "https://example.invalid" }, "Clipped link"))));
    await settle();
    const content = query(".collapsible-message-content");
    const toggle = query(".collapsible-message-toggle");
    const link = query('a[href="https://example.invalid"]');
    assert.equal(content.style.maxHeight, "19lh");
    assert.equal(query(".collapsible-message-ellipsis").textContent, "…");
    assert.deepEqual([toggle.textContent, toggle.getAttribute("aria-expanded")], ["Show more", "false"]);
    assert.deepEqual([link.hasAttribute("inert"), link.getAttribute("aria-hidden")], [true, "true"]);
    await press(toggle, "click");
    assert.deepEqual([content.style.maxHeight, toggle.textContent, toggle.getAttribute("aria-expanded")], ["", "Show less", "true"]);
    assert.deepEqual([link.hasAttribute("inert"), link.getAttribute("aria-hidden")], [false, null]);
    assert.ok(!query(".collapsible-message-fade"));
    await show(workspace({ renderedMessages: [user("long-user", "Long user message")], activeMessageCount: 1 }));
    const bubble = query('[data-user-message-bubble="true"]');
    assert.ok(bubble.querySelector(".collapsible-message-fade"));
    assert.equal(bubble.querySelector(".collapsible-message-content").style.maxHeight, "20lh");
    assert.equal(bubble.querySelector(".collapsible-message-ellipsis"), null);
  } finally {
    Object.defineProperty(w.HTMLElement.prototype, "scrollHeight", tall);
  }
});

test("shell glyphs render on their declared grids", async () => {
  const { Glyph } = await compiled;
  const { renderToStaticMarkup } = require("react-dom/server");
  for (const [name, viewBox] of [["taskSummary", "0 0 16 16"], ["taskPlan", "0 0 16 16"], ["activity", "0 0 16 16"], ["activityUnread", "0 0 16 16"], ["users", "0 0 24 24"]]) {
    const svg = new w.DOMParser().parseFromString(renderToStaticMarkup(React.createElement(Glyph, { name })), "image/svg+xml").documentElement;
    assert.equal(svg.getAttribute("viewBox"), viewBox, name);
    assert.ok(svg.querySelector("path, circle"), `${name} draws a shape`);
  }
});
