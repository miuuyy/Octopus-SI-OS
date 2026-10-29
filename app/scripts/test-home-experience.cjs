const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { createRequire } = require("node:module");
const { test, after } = require("node:test");
const ts = require("typescript");
const React = require("react");
const { act } = React;
const { JSDOM } = require("jsdom");
const { stylesheetCascade } = require("./support/stylesheet-cascade.cjs");
const appRoot = path.resolve(__dirname, "..");
const dom = new JSDOM("<!doctype html><html><body></body></html>", { url: "https://octopus.test", pretendToBeVisual: true });
for (const key of ["window", "document", "HTMLElement", "Element", "Node", "CustomEvent"]) globalThis[key] = dom.window[key];
Object.defineProperty(globalThis, "navigator", { configurable: true, value: dom.window.navigator });
const { createRoot } = require("react-dom/client");
globalThis.IS_REACT_ACT_ENVIRONMENT = true;
globalThis.ResizeObserver = class { observe() {} disconnect() {} };
dom.window.HTMLElement.prototype.scrollTo = function ({ top }) { this.scrollTop = top; };
after(() => dom.window.close());
const noop = () => {};
const meter = { level: 0, waveform: [] };
const motionState = new WeakMap();
const motion = new Proxy({}, { get: (_, tag) => {
  if (!motionNodes.has(tag)) motionNodes.set(tag, React.forwardRef(({ animate, exit, initial, transition, layout, onAnimationComplete, children, ...props }, ref) => {
    const node = React.useRef(null);
    React.useImperativeHandle(ref, () => node.current);
    React.useLayoutEffect(() => { motionState.set(node.current, { initial, animate, transition, onAnimationComplete }); });
    return React.createElement(tag, { ...props, ref: node }, children);
  }));
  return motionNodes.get(tag);
} });
const motionNodes = new Map();
const controls = {
  cx: (...values) => values.filter(Boolean).join(" "),
  OcEmptyState: ({ title, detail }) => React.createElement("div", null, title, detail),
  OcSearchField: ({ inputRef, label, onChange, ...props }) => React.createElement("input", { ...props, ref: inputRef, "aria-label": label, onChange: e => onChange(e.target.value) }),
  OcButton: ({ leadingIcon, trailingIcon, variant, size, selected, children, ...props }) => React.createElement("button", props, children),
  OcAnimatedActionSlot: ({ children }) => React.createElement("div", null, children),
  OcStepProgress: ({ label }) => React.createElement("span", null, label),
  OcInlineProgress: ({ label }) => React.createElement("span", { role: "status" }, label),
  OcIconButton: ({ glyph, label, variant, size, selected, buttonRef, ...props }) => React.createElement("button", { ...props, ref: buttonRef, "aria-label": label }),
  OcMenuSurface: ({ children, ...props }) => React.createElement("div", props, children),
  OcMenuSectionLabel: ({ children }) => React.createElement("span", null, children),
  OcMenuItem: ({ label, leadingIcon, description, selected, ...props }) => React.createElement("button", props, label),
  OcSelect: ({ value, onChange, label, options, ...props }) => React.createElement("select", { ...props, value, "aria-label": label, onChange: e => onChange(e.target.value) }, options.map(o => React.createElement("option", { value: o.value, key: o.value }, o.label))),
  OcSegmented: ({ label, options, onChange, value }) => React.createElement("div", { "aria-label": label }, options.map(o => React.createElement("button", { key: o.value, "aria-pressed": value === o.value, onClick: () => onChange(o.value) }, o.label))),
};
function loader(mocks = {}) {
  const cache = new Map();
  function load(filename) {
    if (filename.endsWith(".json")) return JSON.parse(fs.readFileSync(filename, "utf8"));
    if (cache.has(filename)) return cache.get(filename).exports;
    const module = { exports: {} };
    cache.set(filename, module);
    const source = fs.readFileSync(filename, "utf8");
    const compiled = ts.transpileModule(source, { fileName: filename, compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX, esModuleInterop: true } }).outputText;
    const localRequire = createRequire(filename);
    Function("module", "exports", "require", compiled)(module, module.exports, id => {
      if (id in mocks) return mocks[id];
      if (id.endsWith(".css") || id.includes("icon-glyphs")) return {};
      if (id.endsWith("design-system/components")) return controls;
      if (id.endsWith("IconGlyphs")) return { Glyph: () => null };
      if (id === "motion/react") return { motion, AnimatePresence: ({ children }) => children, useIsPresent: () => true, useReducedMotion: () => false };
      if (!id.startsWith(".")) return localRequire(id);
      const target = path.resolve(path.dirname(filename), id);
      const candidate = [target, `${target}.ts`, `${target}.tsx`].find(file => fs.existsSync(file) && fs.statSync(file).isFile());
      assert.ok(candidate, `Missing test import ${id}`);
      return load(candidate);
    });
    return module.exports;
  }
  return relative => load(path.join(appRoot, relative));
}
function session(phase, overrides = {}) {
  return { phase, transcript: [], context: null, startedAtMs: 0, error: "", deviceError: "", inputDevices: [{ deviceId: "mic", label: "Microphone" }], outputDevices: [{ deviceId: "speaker", label: "Speaker" }], selectedInputId: "mic", selectedOutputId: "speaker", subscribeOutputMeter: () => noop, getOutputMeterSnapshot: () => meter, subscribeMeter: () => noop, getMeterSnapshot: () => meter, setSpeakerEnabled: noop, selectInputDevice: noop, selectOutputDevice: noop, refreshDevices: noop, toggleMute: noop, end: async () => {}, start: async () => {}, updateContext: async () => {}, ...overrides };
}
async function mount(component, props) {
  const container = document.createElement("div");
  document.body.append(container);
  const root = createRoot(container);
  const render = async next => { await act(async () => { root.render(React.createElement(component, next)); }); };
  await render(props);
  return { container, render, close: async () => { await act(async () => root.unmount()); container.remove(); } };
}
const button = (container, label) => [...container.querySelectorAll("button")].find(node => (node.getAttribute("aria-label") || node.textContent) === label);
const click = async node => { assert.ok(node); await act(async () => node.click()); };

test("onboarding requires the explicit ChatGPT sign-in action and prevents duplicate submission while connecting", async () => {
  const { WorkspaceSetupOverlay, createDefaultWorkspaceSetupDraft } = loader()("src/components/onboarding/WorkspaceSetupOverlay.tsx");
  let signIns = 0;
  const props = {
    provider: null, agentPresets: [], catalog: [], catalogError: "", catalogLoading: false,
    draft: { ...createDefaultWorkspaceSetupDraft(), step: "applying" },
    applyState: { state: "awaiting_sign_in", label: "Connect ChatGPT", detail: "Continue with your account.", percent: null, error: "" },
    onApply: noop, onChange: noop, onRetryCatalog: noop, onSignIn: async () => { signIns++; },
  };
  const view = await mount(WorkspaceSetupOverlay, props);
  try {
    assert.equal(signIns, 0);
    assert.equal(view.container.querySelector('[role="status"]'), null);
    await click(button(view.container, "Sign in with ChatGPT"));
    assert.equal(signIns, 1);
    await view.render({ ...props, applyState: { ...props.applyState, state: "running", label: "Connecting ChatGPT" } });
    assert.equal(button(view.container, "Sign in with ChatGPT"), undefined);
    assert.equal(button(view.container, "Working…").disabled, true);
  } finally { await view.close(); }
});

test("page loaders show only the centered logo until the catalog settles", async () => {
  const { SurfaceLoadingState } = loader()("src/components/shell/SurfaceBoundary.tsx");
  const fallback = await mount(SurfaceLoadingState, { label: "Loading catalog" });
  const expectLogo = (container, className = "surface-content-loading-state") => {
    const status = container.querySelector('[role="status"]');
    assert.ok(status.classList.contains(className));
    assert.equal(status.getAttribute("aria-busy"), "true");
    assert.equal(container.textContent, "");
    assert.equal(status.querySelector(".octopus-loading-logo").style.width, "56px");
    assert.ok(container.querySelector("header, input, h1") === null);
  };
  try { expectLogo(fallback.container, "surface-loading-state"); } finally { await fallback.close(); }
  const topology = loader({
    "./ArtifactTransferAnimation": { useArtifactTransferFeed: () => ({ feed: {}, error: "" }) },
    "../toasts": { useAppErrorToast: noop },
  });
  const { TopologyStage } = topology("src/components/TopologyStage.tsx");
  const snapshot = topology("src/data/topology.ts").emptySnapshot({ mode: "connecting", label: "Loading topology", detail: "" });
  const props = { snapshot, query: "", scope: "World", viewMode: "zones", filters: { statuses: {} }, selected: null, onSelect: noop };
  const stage = await mount(TopologyStage, props);
  try {
    expectLogo(stage.container);
    await stage.render({ ...props, scope: "System" });
    expectLogo(stage.container);
    await stage.render({ ...props, snapshot: { ...snapshot, projection: { mode: "error", label: "Read failed", detail: "Unavailable" } } });
    assert.equal(stage.container.querySelector(".octopus-loading-logo"), null);
    assert.match(stage.container.textContent, /Read failed/);
  } finally { await stage.close(); }
  const incidents = [];
  for (const name of ["ApplicationsView", "DriversView"]) {
    const pending = new Map();
    const Component = loader({
      "../projectionClient": { canConnectApplications: () => false, hasProjectionBridge: () => true, onApplicationLifecycleProgress: () => noop, formatProjectionError: String, readProjection: key => new Promise((resolve, reject) => pending.set(key, { resolve, reject })) },
      "../projectionLive": { useProjectionStructureGeneration: () => 0 },
      "../extensions/useClientExtensions": { useClientExtensions: () => ({ extensions: [], actionStatuses: [] }) },
      "../extensions/incidents": { subscribeClientExtensionIncidents: () => noop, clientExtensionIncidentSnapshot: () => incidents },
      "../toasts": { useAppErrorToast: noop },
      "./catalog": { CatalogRouteTransition: ({ children }) => children, CatalogSection: () => null },
      "./SetupDialogShell": { SetupDialogShell: () => null },
      "./applications/ApplicationConnectDialog": { ApplicationConnectDialog: () => null },
    })(`src/components/${name}.tsx`)[name];
    const view = await mount(Component, { accessProfile: "live", searchFocusTick: 0 });
    try {
      expectLogo(view.container);
      if (name === "ApplicationsView") {
        await act(async () => pending.get("projection.operator.os.applications").resolve({ applications: [], projects: [], errors: [] }));
        expectLogo(view.container);
        await act(async () => pending.get("projection.operator.os.local_packages").resolve({ packs: [], errors: [] }));
        assert.match(view.container.textContent, /No application packages found/);
      } else {
        await act(async () => pending.get("projection.operator.os.drivers").reject(new Error("Catalog unavailable")));
        assert.match(view.container.textContent, /Drivers could not be loaded/);
      }
      assert.equal(view.container.querySelector(".octopus-loading-logo"), null);
      assert.ok(view.container.querySelector("h1"));
    } finally { await view.close(); }
  }
});

test("voice activity waits for completed user turns and ignores partial captions and audio samples", () => {
  const { voiceTranscriptActivity, voicePresentation } = loader()("src/components/voice/voicePresentation.ts");
  const partial = { id: "user", role: "user", text: "Explain", final: false };
  const final = { ...partial, final: true };
  assert.equal(voiceTranscriptActivity(undefined, partial), null);
  assert.equal(voiceTranscriptActivity(partial, { ...partial, text: "Explain this" }), null);
  assert.equal(voiceTranscriptActivity(partial, final), true);
  assert.equal(voiceTranscriptActivity(final, final), null);
  assert.equal(voiceTranscriptActivity(partial, { ...final, provider_turn_complete: false }), null);
  assert.equal(voiceTranscriptActivity(undefined, { ...final, text: " " }), null);
  const reply = { id: "reply", role: "assistant", text: "Here", final: false };
  assert.equal(voiceTranscriptActivity(undefined, reply), false);
  assert.equal(voiceTranscriptActivity(reply, { ...reply, final: true }), null);
  assert.equal(voicePresentation("listening").title, "Connected");
});

test("call controls enforce every lifecycle state and ending voice never invokes task control", async () => {
  const { HomeCallControls } = loader()("src/components/voice/HomeCallControls.tsx");
  let ended = 0, muted = 0;
  const props = phase => ({ contextSelection: { omittedMessageCount: 0, truncated: false, disclosure: "" }, session: session(phase, { end: async () => { ended++; }, toggleMute: () => { muted++; } }), onStart: noop, startDisabledReason: "", speakerEnabled: true, onSpeakerChange: noop });
  const view = await mount(HomeCallControls, props("requesting"));
  try {
    for (const phase of ["requesting", "connecting", "recovering", "listening", "muted"]) {
      await view.render(props(phase));
      const mic = button(view.container, phase === "muted" ? "Unmute microphone" : "Mute microphone");
      assert.equal(mic.disabled, !["listening", "muted"].includes(phase), phase);
      const end = button(view.container, "End call");
      assert.ok(button(view.container, "More audio controls"));
      assert.equal(end.disabled, false, phase);
      if (["listening", "muted"].includes(phase)) assert.doesNotMatch(view.container.querySelector('[role="status"]').textContent, /Connecting|Reconnecting/);
      if (phase === "connecting") await click(end);
      if (phase === "listening") await click(mic);
    }
    assert.equal(ended, 1); assert.equal(muted, 1);
    for (const level of [0, 0.08, 0.7, 0]) {
      await view.render({ ...props("listening"), session: session("listening", { getOutputMeterSnapshot: () => ({ level, waveform: [] }) }) });
      assert.equal(view.container.querySelector('[role="status"]').textContent, "Connected");
    }
    await view.render(props("ending"));
    assert.equal(view.container.querySelector('[role="status"]').textContent, "Closing connection…");
    assert.equal(button(view.container, "End call").disabled, true);
    assert.equal(button(view.container, "Mute microphone"), undefined);
    assert.equal(button(view.container, "Call"), undefined);
    await view.render({ ...props("closure_unconfirmed"), session: session("closure_unconfirmed", { error: "Provider did not confirm closure", end: async () => { ended++; } }) });
    assert.equal(view.container.querySelector('[role="status"]').textContent, "Closure unconfirmed");
    assert.equal(view.container.querySelector('[role="alert"]').textContent, "Call closure is unconfirmed.");
    assert.equal(button(view.container, "Retry call"), undefined);
    await click(button(view.container, "Retry closure")); assert.equal(ended, 2);
    await view.render(props("ended"));
    assert.equal(view.container.querySelector(".home-call"), null);
    const diagnostic = 'IPC error: {"reason":"Voice connection rejected"}';
    await view.render({ ...props("error"), session: session("error", { error: diagnostic, deviceError: diagnostic }) });
    assert.equal(view.container.querySelectorAll('[role="alert"]').length, 1);
    assert.equal(view.container.querySelector('[role="alert"]').textContent, "Voice call failed.");
    assert.equal(view.container.querySelector("details").open, false);
    assert.equal(view.container.querySelector("details pre").textContent, diagnostic);
    assert.ok(button(view.container, "Retry call"));
    assert.doesNotMatch(view.container.textContent, /Voice unavailable/);
    let dismissed = 0;
    await view.render({ ...props("error"), session: session("error", { error: diagnostic, dismissError: () => { dismissed++; } }) });
    await click(button(view.container, "Dismiss"));
    assert.equal(dismissed, 1);
    assert.equal(view.container.querySelector(".home-call"), null);
    await view.render(props("idle"));
    assert.equal(view.container.querySelector(".home-call"), null);
  } finally { await view.close(); }
});

test("call controls show provider captions, selectable transcript history, and audio device choices", async () => {
  const { HomeCallControls } = loader()("src/components/voice/HomeCallControls.tsx");
  let input = "", refreshed = 0, speaker = true;
  const contextSelection = { omittedMessageCount: 0, truncated: false, disclosure: "Attachments and unloaded history are not included." };
  const props = { contextSelection, session: session("listening", { transcript: [{ id: "a", role: "user", text: "Check the task", final: true }, { id: "b", role: "assistant", text: "Checking", final: false }], selectInputDevice: id => { input = id; }, refreshDevices: () => { refreshed++; } }), onStart: noop, startDisabledReason: "", speakerEnabled: true, onSpeakerChange: enabled => { speaker = enabled; } };
  const view = await mount(HomeCallControls, props);
  try {
    assert.equal(view.container.querySelector(".home-call__caption").textContent, "Home: Checking");
    assert.equal(view.container.querySelector('[aria-label="Call transcript"]'), null);
    assert.doesNotMatch(view.container.textContent, /Check the task|In progress/);
    assert.equal(props.session.transcript.length, 2);
    await click(button(view.container, "Show call transcript"));
    const transcript = view.container.querySelector('[aria-label="Call transcript"]');
    assert.deepEqual([...transcript.querySelectorAll("article")].map(entry => [entry.querySelector("strong").textContent, entry.querySelector("p").textContent]), [["You", "Check the task"], ["Home", "Checking"]]);
    const range = document.createRange();
    range.selectNodeContents(transcript.querySelector("p"));
    window.getSelection().removeAllRanges();
    window.getSelection().addRange(range);
    assert.equal(window.getSelection().toString(), "Check the task");
    window.getSelection().removeAllRanges();
    await act(async () => transcript.dispatchEvent(new window.KeyboardEvent("keydown", { key: "Escape", bubbles: true })));
    assert.equal(view.container.querySelector('[aria-label="Call transcript"]'), null);
    assert.equal(document.activeElement, button(view.container, "Show call transcript"));
    await click(button(view.container, "More audio controls"));
    assert.equal(refreshed, 1);
    await click(button(view.container, "Mute speaker"));
    assert.equal(speaker, false);
    const microphone = view.container.querySelector('select[aria-label="Microphone"]');
    await act(async () => microphone.dispatchEvent(new window.Event("change", { bubbles: true })));
    assert.equal(input, "mic");
    await view.render({ ...props, session: { ...props.session, phase: "recovering" } });
    assert.equal(view.container.querySelector('[role="log"]'), null);
    assert.equal(view.container.querySelector('select[aria-label="Microphone"]').disabled, true);
    assert.equal(view.container.querySelector(".home-call__context"), null);
    assert.doesNotMatch(view.container.textContent, /Voice context|Some Home text|Attachments and unloaded history/);
    await click(button(view.container, "Collapse audio controls"));
    assert.equal(view.container.querySelector(".home-call__devices"), null);
    await view.render({ ...props, session: { ...props.session, transcript: [{ id: "c", role: "user", text: "One more thing", final: false }] } });
    assert.equal(view.container.querySelector(".home-call__caption").textContent, "You: One more thing");
    const longReply = `${"A long spoken explanation. ".repeat(30)}Последние слова 🐙`;
    await view.render({ ...props, session: { ...props.session, transcript: [{ id: "d", role: "assistant", text: longReply, final: false }] } });
    const caption = view.container.querySelector(".home-call__caption");
    assert.equal(caption.textContent, `Home: ${longReply}`);
    const styles = stylesheetCascade(["src/foundation.css", "src/styles.css", "src/shell.css", "src/components/voice/voice.css"]);
    assert.equal(styles.value(caption, "justify-content"), "flex-end");
    assert.equal(styles.value(caption, "max-height"), "44px");
    assert.equal(styles.value(caption, "-webkit-line-clamp"), undefined);
    assert.equal(styles.value(caption.firstElementChild, "flex-shrink"), "0");
    await view.render({ ...props, session: session("connecting") });
    assert.equal(view.container.querySelector(".home-call__caption"), null);
    assert.equal(button(view.container, "Show call transcript"), undefined);
    for (const limit of [{ omittedMessageCount: 1 }, { truncated: true }]) {
      await view.render({ ...props, contextSelection: { ...contextSelection, ...limit } });
      assert.equal(view.container.querySelector(".home-call__context"), null);
      assert.doesNotMatch(view.container.textContent, /Voice context|Some Home text|Attachments and unloaded history/);
    }
  } finally { await view.close(); }
});

test("call duration survives remounts and never hides interrupted connection states", async t => {
  t.mock.method(performance, "now", () => 125_000);
  const { HomeCallControls } = loader()("src/components/voice/HomeCallControls.tsx");
  const props = { session: session("listening", { startedAtMs: 5_000 }), onStart: noop, startDisabledReason: "", speakerEnabled: true, onSpeakerChange: noop };
  for (let attempt = 0; attempt < 2; attempt++) {
    const view = await mount(HomeCallControls, props);
    try {
      assert.equal(view.container.querySelector('[aria-label="Call duration"]').textContent, "2:00");
      for (const [phase, title] of [["recovering", "Reconnecting call"], ["ending", "Closing connection…"], ["closure_unconfirmed", "Closure unconfirmed"]]) {
        await view.render({ ...props, session: { ...props.session, phase } });
        assert.equal(view.container.querySelector(".home-call__status").textContent, title);
      }
    } finally { await view.close(); }
  }
});

const task = (runId, overrides = {}) => ({ runId, title: runId.toUpperCase(), clientSurfaceRole: "task", osRootEeId: "root", eeId: "project", writable: true, archived: false, turnOpen: false, turnState: "idle", nextScheduledAtMs: 0, updatedAtMs: 1000, lastTerminalMessage: null, ...overrides });
const home = task("home", { clientSurfaceRole: "home", eeId: "root" });
const topology = { projection: { mode: "live" }, environments: [{ id: "root", name: "Existence", runs: [] }, { id: "project", name: "Research", parentId: "root", runs: [] }] };

test("Activity renders the unchanged Home chat and provider trace without a task dashboard", async () => {
  let chatProps;
  const { HomeAgentActivityView } = loader({
    "../ChatView": { ChatView: props => { chatProps = props; return React.createElement("div", { "data-home-chat": true }, props.workspace.messages.map(message => React.createElement("p", { key: message.id }, message.text))); } },
  })("src/components/voice/HomeAgentActivityView.tsx");
  const messages = [{ id: "history-error", text: "Historical instruction error", error: true }, { id: "subagent", eventKind: "subagent_activity", text: "Subagent started" }];
  const workspace = { runs: [home, task("working", { turnState: "working" })], runsLoaded: true, runsError: "", messages };
  const props = { commandsActive: true, workspace, topology, surfaces: [], searchFocusTick: 1, onOpenFiles: noop };
  const view = await mount(HomeAgentActivityView, props);
  try {
    assert.equal(chatProps.workspace, workspace);
    assert.deepEqual([chatProps.headerMode, chatProps.composerVisible, chatProps.searchFocusTick], ["none", false, 1]);
    assert.match(view.container.querySelector("[data-home-chat]").textContent, /Historical instruction errorSubagent started/);
    assert.equal(chatProps.commandsActive, true);
    assert.equal(view.container.querySelector("aside, summary, .home-task-activity, .home-task-card"), null);
    assert.doesNotMatch(view.container.textContent, /Tasks in Existence/);
    await view.render({ ...props, workspace: { ...workspace, runsError: "Projection unavailable" } });
    assert.equal(chatProps.workspace.runsError, "Projection unavailable");
    assert.equal(chatProps.workspace.messages, messages);
    assert.ok(view.container.querySelector("[data-home-chat]"));
  } finally { await view.close(); }
});

test("opening Home activity retains the Home component and unsent draft", async () => {
  let mounts = 0, homeProps;
  const Home = props => { homeProps = props; React.useEffect(() => { mounts++; }, []); return React.createElement("textarea", { defaultValue: "Unsent Home message" }); };
  const load = loader({
    "../voice/VoiceView": { VoiceView: Home },
    "../voice/HomeAgentActivityView": { HomeAgentActivityView: () => React.createElement("section", { "data-home-activity": true }) },
    "../voice/useHomeActivityWorkspace": { useHomeRunActivity: () => ({ entries: [] }) },
  });
  const { HomeSurface } = load("src/components/shell/HomeSurface.tsx");
  const { WorkspacePaneHeaderActionsProvider } = load("src/components/shell/WorkspacePaneHeaderActions.tsx");
  const toolbar = document.createElement("div");
  document.body.append(toolbar);
  function Pane(props) {
    const [mode, setMode] = React.useState("voice");
    return React.createElement(WorkspacePaneHeaderActionsProvider, { host: toolbar },
      React.createElement(HomeSurface, { ...props, homeViewMode: mode, onHomeViewModeChange: setMode }));
  }
  const props = { voiceSession: session("listening"), homeViewMode: "voice", homeRunId: "home", onHomeViewModeChange: noop };
  const view = await mount(Pane, props);
  try {
    const draft = view.container.querySelector("textarea");
    draft.value = "Draft kept during call";
    assert.deepEqual([...toolbar.querySelectorAll("button")].map(node => node.getAttribute("aria-label")), ["Activity", "Home widgets"]);
    assert.ok(view.container.querySelector("[data-home-activity]") === null, "the Home chat mounts only in activity mode");
    await click(button(toolbar, "Activity"));
    assert.equal(button(toolbar, "Activity").getAttribute("aria-expanded"), "true");
    assert.equal(view.container.querySelector("textarea"), draft);
    assert.equal(draft.value, "Draft kept during call");
    const homeLayer = draft.closest(".home-view-transition__surface");
    assert.equal(homeLayer.hidden, false);
    assert.equal(homeLayer.hasAttribute("inert"), true);
    assert.equal(motionState.get(homeLayer).animate.opacity, 0);
    assert.equal(homeProps.commandsActive, false);
    assert.equal(homeProps.session, props.voiceSession);
    assert.equal(view.container.querySelector("[data-home-activity]").closest(".home-view-transition__surface").hidden, false);
    assert.equal(view.container.querySelector("aside, .home-view-transition__activity, .home-view-transition__layout"), null);
    assert.equal(button(toolbar, "Home widgets"), undefined);
    await click(button(toolbar, "Activity"));
    assert.ok(view.container.querySelector("[data-home-activity]") === null);
    await click(button(toolbar, "Home widgets"));
    assert.equal(homeProps.widgetsOpen, true);
    assert.equal(button(toolbar, "Activity").getAttribute("aria-expanded"), "false");
    assert.equal(mounts, 1);
    assert.equal(draft.closest(".home-view-transition__surface").hidden, false);
    assert.equal(homeLayer.hasAttribute("inert"), false);
    assert.equal(motionState.get(homeLayer).animate.opacity, 1);
    assert.equal(homeProps.commandsActive, true);
  } finally { await view.close(); toolbar.remove(); }
});

test("Home sequences reply fade before the call scene and preserves drafts, replies and widget provenance", async () => {
  const calls = [];
  let orbProps, startContext, composerProps;
  const composer = props => { composerProps = props; return React.createElement("div", { "data-composer": true }, React.createElement("textarea", { value: props.text, onChange: e => props.onTextChange(e.target.value) }), React.createElement("button", { onClick: () => props.onSubmit() }, "Send Home"), React.createElement("button", { disabled: props.voiceStartDisabled, onClick: props.onVoiceStart }, "Start call")); };
  const { VoiceView } = loader({
    "../../clientActions": { subscribeClientCommands: () => noop },
    "../../projectionClient": {},
    "../../projectionLive": {},
    "../../client-surfaces/ClientSurfaceRenderer": { ClientSurfaceRenderer: ({ admitted, onDismiss }) => React.createElement("section", { "data-widget": admitted.document.title }, admitted.document.title, onDismiss ? React.createElement("button", { onClick: onDismiss }, `Hide ${admitted.document.title}`) : null) },
    "../../shell/clientWorkspaces": { readClientChatProjects: () => [] },
    "../../toasts": { appToast: { warning: noop, info: () => assert.fail("Routine voice context must not toast") }, useAppErrorToast: error => assert.doesNotMatch(error, /Exact (call|device) diagnostic/) },
    "../chat/OperatorRequestSurface": { OperatorRequestSurface: () => null },
    "../chat/MessageContent": { MessageContent: ({ text }) => React.createElement("span", null, text) },
    "../../appearance": { useShouldReduceMotion: () => true, useAppearance: () => ({ octopuses: false }) },
    "../IconGlyphs": { Glyph: ({ name }) => React.createElement("svg", { "data-glyph": name }) },
    "../CadencedShimmerText": { CadencedShimmerText: ({ children, active }) => React.createElement("span", { "data-active": active }, children), ShimmerActivityScope: ({ children }) => children },
    "../chat/ChatComposer": { ChatComposer: composer },
    "../chat/QueuedMessageList": { QueuedMessageList: () => null },
    "./VoiceOrbScene": { VoiceOrbScene: props => { orbProps = props; return null; } },
    "./voiceWidgetDocuments": { buildVoiceWidgetDocuments: () => [] },
    "../chat/chatProductError": { chatProductError: () => "" },
    "../OctopusLoadingLogo": { OctopusLoadingState: () => null },
  })("src/components/voice/VoiceView.tsx");
  const message = (id, interfaceId, text) => ({ id, runId: "home", interfaceId, direction: "out", role: "assistant", eventKind: "message", terminal: true, createdAtMs: 1000, text, attachments: [] });
  const run = { ...home, chatInterfaceId: "home-client", voiceContext: { runId: "home-voice", providerId: "voice-interface", controlInterfaceId: "voice-control", delegationInterfaceId: "home-delegation" } };
  const workspace = { runsLoaded: true, runsError: "", activeRunId: "home", activeRun: run, queuedTurns: [], runs: [run], loadingMessages: false, renderedMessages: [message("delivered", "home-client", "A result delivered to Home"), message("internal", "private-task", "Internal work must stay private")], declaredProviderInterfaces: [{ id: "voice-interface", label: "Voice", runtimeHandlerRef: "realtime_webrtc" }], busy: false, submitting: false, toolPresentations: {}, text: "Follow-up while calling", setText: noop, submit: () => calls.push("send"), interruptActiveTurn: () => calls.push("interrupt"), providerInterfaces: [] };
  const document = require("../src/client-surfaces/home-widget-authoring.json").example;
  const deliveredWidget = (title, patch = {}) => ({ document: { ...document, title, data: {} }, sourceRunId: "home", sourceInterfaceId: "home-client", sourceArtifactId: title, sourceSequence: 1, sourceDigest: title, sourceDeliveredAt: new Date().toISOString(), ...patch });
  const artifactWidget = deliveredWidget("Delivered widget");
  const extensionWidget = deliveredWidget("Installed extension", { sourceExtensionId: "approved-extension", sourceRunId: "", sourceInterfaceId: "" });
  const surfaces = [artifactWidget, extensionWidget, deliveredWidget("Other interface", { sourceInterfaceId: "private-task" }), deliveredWidget("Other Home", { sourceRunId: "other-home" })];
  const props = { accessProfile: "live", activity: { entries: [], total: 0 }, builtinWidgetSources: {}, homeRunId: "home", installedWidgetIds: [extensionWidget.document.id], installedWidgetsReady: true, session: session("idle", { start: async context => { startContext = context; calls.push("start"); }, end: async () => calls.push("end") }), surfaces, topology, workerEnvironmentSurfaces: [], widgetsOpen: true, workspace, viewMode: "voice" };
  const visibilityKey = "octopus.voice.hidden-surfaces.v1.live";
  window.localStorage.removeItem(visibilityKey);
  const view = await mount(VoiceView, { ...props, installedWidgetIds: [], installedWidgetsReady: false });
  try {
    assert.equal(window.localStorage.getItem(visibilityKey), null);
    await view.render(props);
    const homeRoot = view.container.querySelector(".voice-view");
    const scene = homeRoot.querySelector(":scope > .voice-home-scene");
    assert.ok(scene);
    assert.equal(orbProps.sceneSurface.current, homeRoot.querySelector(".voice-session-stage"));
    assert.equal(orbProps.rippleSurface.current, homeRoot);
    const homeStyles = stylesheetCascade(["src/foundation.css", "src/styles.css", "src/shell.css", "src/components/voice/voice.css"]);
    assert.notEqual(homeStyles.value(homeRoot, "position") ?? "static", "static");
    assert.equal(homeStyles.value(scene, "position"), "absolute");
    assert.deepEqual(["top", "right", "bottom", "left"].map(side => homeStyles.value(scene, side)), ["0", "0", "0", "0"]);
    assert.equal(homeStyles.value(scene, "pointer-events"), "none", "the scene behind Home never takes its clicks");
    assert.deepEqual([...view.container.querySelectorAll("[data-widget]")].map(node => node.dataset.widget), ["Delivered widget"]);
    assert.equal(button(view.container, "Installed extension").getAttribute("aria-checked"), "false");
    await click(button(view.container, "Installed extension"));
    const input = view.container.querySelector("textarea");
    assert.equal(input.value, "Follow-up while calling");
    assert.match(view.container.textContent, /A result delivered to Home/);
    assert.doesNotMatch(view.container.textContent, /Internal work must stay private/);
    assert.deepEqual([...view.container.querySelectorAll("[data-widget]")].map(node => node.dataset.widget), ["Delivered widget", "Installed extension"]);
    assert.equal(button(view.container, "Hide Installed extension"), undefined);
    await click(button(view.container, "Hide Delivered widget"));
    assert.equal(view.container.querySelector('[data-widget="Delivered widget"]'), null);
    assert.ok(view.container.querySelector('[data-widget="Installed extension"]'));
    await click(button(view.container, "Restore all"));
    assert.ok(view.container.querySelector('[data-widget="Delivered widget"]'));
    await click(button(view.container, "Start call"));
    assert.equal(startContext.runId, "home-voice");
    assert.equal(startContext.homeRunId, "home");
    assert.equal(startContext.providerId, "voice-interface");
    assert.deepEqual(calls, ["start"]);
    await view.render({ ...props, session: { ...props.session, phase: "requesting" } });
    const body = view.container.querySelector(".home-response-stage__body");
    const fade = motionState.get(body);
    assert.equal(fade.transition.duration, 0.5);
    assert.equal(fade.animate.opacity, 0);
    assert.equal(orbProps.ambient, true);
    await act(async () => fade.onAnimationComplete({ opacity: 0 }));
    assert.equal(orbProps.ambient, false);
    assert.equal(body.getAttribute("aria-hidden"), "true");
    assert.equal(view.container.querySelector("textarea"), input);
    assert.ok(input.closest("[hidden][inert]"));
    assert.equal(composerProps.listenToGlobalCommands, false);
    await click(button(view.container, "End call"));
    assert.deepEqual(calls, ["start", "end"]);
    const liveInput = { level: 0.7, waveform: [] };
    for (const [overrides, expected] of [[{}, 0], [{ providerActivity: true }, 1], [{ getMeterSnapshot: () => liveInput }, 0], [{ providerActivity: true, getMeterSnapshot: () => liveInput }, 1]]) {
      await view.render({ ...props, session: session("listening", overrides) });
      assert.equal(orbProps.activity.thinkingLevel, expected, "Provider activity drives inner waves independently of microphone input");
      assert.equal(orbProps.activity.speakingLevel, 0, "Input does not impersonate model speech");
    }
    await view.render({ ...props, workspace: { ...workspace, busy: true }, session: session("listening", { getMeterSnapshot: () => liveInput }) });
    assert.equal(orbProps.activity.thinkingLevel, 1, "Home work drives inner waves");
    for (const phase of ["connecting", "listening", "muted", "recovering", "ending", "ended", "error"]) {
      await view.render({ ...props, session: session(phase, phase === "error" ? { error: "Exact call diagnostic", deviceError: "Exact device diagnostic" } : {}) });
      assert.equal(view.container.querySelector("textarea"), input);
      assert.match(view.container.textContent, /A result delivered to Home/);
      assert.equal(Boolean(input.closest("[hidden][inert]")), !["ending", "ended", "error"].includes(phase));
      assert.equal(composerProps.listenToGlobalCommands, ["ending", "ended", "error"].includes(phase));
      assert.equal(composerProps.voiceStartDisabled, !["ended", "error"].includes(phase));
      if (phase === "ending") {
        const before = calls.length;
        await act(async () => composerProps.onVoiceStart());
        assert.equal(calls.length, before);
        assert.equal(view.container.querySelector(".home-call__status").textContent, "Closing connection…");
      }
      assert.equal(input.value, "Follow-up while calling");
      if (["ending", "ended", "error"].includes(phase)) {
        assert.equal(orbProps.ambient, true);
        await act(async () => orbProps.onDispersed());
        assert.equal(motionState.get(body).animate.opacity, 1);
      } else {
        assert.equal(motionState.get(body).animate.opacity, 0);
      }
    }
    await view.render({ ...props, session: { ...props.session, phase: "requesting" } });
    const cancelledFade = motionState.get(body);
    await view.render({ ...props, session: { ...props.session, phase: "error" } });
    await act(async () => cancelledFade.onAnimationComplete({ opacity: 0 }));
    assert.equal(orbProps.ambient, true);
    assert.equal(motionState.get(body).animate.opacity, 1);
    await view.render({ ...props, session: session("listening", { context: { runId: "other-voice", homeRunId: "other-home" } }) });
    assert.ok(view.container.querySelector('[data-widget="Delivered widget"]'));
    await click(button(view.container, "Hide Delivered widget"));
    await view.render({ ...props, surfaces: [deliveredWidget("Delivered widget", { sourceDigest: "revision-2" }), extensionWidget] });
    assert.ok(view.container.querySelector('[data-widget="Delivered widget"]'));
    const expired = deliveredWidget("Expired widget", { document: { ...document, title: "Expired widget", data: { home_presentation: { expires_at: "2020-01-01T00:00:00Z" } } } });
    const invalid = deliveredWidget("Invalid widget", { sourceArtifactId: "" });
    await view.render({ ...props, surfaces: [expired, invalid, extensionWidget] });
    assert.equal(view.container.querySelector('[data-widget="Expired widget"]'), null);
    assert.equal(view.container.querySelector('[data-widget="Invalid widget"]'), null);
    assert.match(view.container.querySelector('[role="alert"]').textContent, /exact artifact delivery provenance/);
    assert.ok(view.container.querySelector('[data-widget="Installed extension"]'));
    const nextPrompt = { ...message("next-prompt", "home-client", "Continue"), role: "user", direction: "in", createdAtMs: 2000 };
    await view.render({ ...props, workspace: { ...workspace, busy: true, renderedMessages: [...workspace.renderedMessages, nextPrompt] } });
    assert.equal(body.textContent, "Octopus is working");
    assert.equal(body.querySelector(".home-response-stage__identity"), null);
    const streaming = { ...message("stream", "home-client", "A streamed reply"), eventKind: "message_delta", terminal: false };
    await view.render({ ...props, workspace: { ...workspace, busy: true, renderedMessages: [streaming] } });
    assert.equal(body.querySelector(".home-response-output__copy").textContent, "A streamed reply");
    assert.equal(body.querySelector("time").dateTime, new Date(streaming.createdAtMs).toISOString());
    const prompt = { ...message("prompt", "home-client", "Hi"), direction: "in", role: "user", createdAtMs: 500 };
    const commentary = { ...message("progress", "home-client", "Checking the current bindings"), eventKind: "commentary_delta", terminal: false };
    const unrelated = [
      { ...commentary, id: "old", createdAtMs: 400, text: "Previous turn" },
      { ...commentary, id: "private", interfaceId: "private-task", text: "Private work" },
      { ...commentary, id: "other-run", runId: "other-home", text: "Other Home" },
    ];
    await view.render({ ...props, workspace: { ...workspace, busy: true, renderedMessages: [prompt, ...unrelated, commentary] } });
    assert.equal(body.querySelector('[aria-label="Current run activity"]').textContent, commentary.text);
    assert.equal(body.querySelector('.octopus-turn-activity').dataset.state, "working");
    assert.doesNotMatch(body.textContent, /Octopus is working|Previous turn|Private work|Other Home/);
    const tool = { ...commentary, id: "write", eventKind: "file_change", text: "", eventData: { changes: [{ path: "notes.txt", kind: "update" }], status: "completed" } };
    const terminal = { ...commentary, id: "terminal", eventKind: "turn_completed", text: "", terminal: true };
    for (const [status, notice] of [["interrupted", "Turn interrupted before a final reply."], ["completed", "Turn ended without a final reply."]]) {
      await view.render({ ...props, workspace: { ...workspace, busy: status === "interrupted", renderedMessages: [prompt, ...unrelated, commentary, tool, { ...terminal, eventData: { turn: { status } } }] } });
      assert.match(body.textContent, new RegExp(commentary.text));
      assert.match(body.textContent, new RegExp(notice));
      assert.doesNotMatch(body.textContent, /What would you like to do|Octopus is working|Previous turn|Private work|Other Home/);
      assert.equal(body.closest('.home-response-stage').getAttribute('aria-busy'), "false");
      assert.equal(body.querySelector('[data-active="true"], .home-response-output--live'), null);
      const disclosure = body.querySelector('button[aria-expanded]');
      assert.ok(disclosure);
      if (disclosure.getAttribute('aria-expanded') === "false") await click(disclosure);
      await act(async () => new Promise(resolve => window.requestAnimationFrame(resolve)));
      assert.equal(body.querySelector('.octopus-agent-activity-body').getAttribute('aria-hidden'), "false");
      assert.match(body.textContent, /Updated notes.txt/);
    }
    const answered = { ...terminal, eventData: { turn: { status: "completed", items: [{ type: "agentMessage", phase: "final_answer", text: "Done." }] } } };
    await view.render({ ...props, workspace: { ...workspace, busy: false, renderedMessages: [prompt, commentary, tool, answered] } });
    assert.doesNotMatch(body.textContent, /Turn ended without a final reply|What would you like to do/);
    assert.equal(body.closest('.home-response-stage').getAttribute('aria-busy'), "true");
    const failure = { ...terminal, id: "error", eventKind: "error", error: true, text: "Exact provider error" };
    await view.render({ ...props, workspace: { ...workspace, renderedMessages: [prompt, commentary, tool, failure] } });
    assert.match(body.querySelector('[role="alert"]').textContent, /Exact provider error/);
    assert.equal(body.querySelector('.home-live-activity'), null);
    for (const messages of [[], [prompt, commentary, tool, terminal, { ...prompt, id: "new", createdAtMs: 2000 }]]) {
      await view.render({ ...props, workspace: { ...workspace, renderedMessages: messages } });
      assert.match(body.textContent, /What would you like to do/);
      assert.equal(body.querySelector('.home-live-activity'), null);
    }
    const warning = { ...commentary, id: "warning", eventKind: "warning", text: "Provider diagnostic. ".repeat(20), eventData: { detail: "Diagnostic detail" } };
    await view.render({ ...props, workspace: { ...workspace, busy: true, renderedMessages: [prompt, warning] } });
    const notice = body.querySelector(".conversation-notice.is-danger");
    assert.equal(notice.querySelector("svg").dataset.glyph, "warning");
    assert.equal(notice.textContent, `${warning.text}Diagnostic detail`);
    await view.render({ ...props, workspace: { ...workspace, busy: true, renderedMessages: [prompt, { ...streaming, text: "Long streamed response. ".repeat(30) }] } });
    assert.ok(body.closest(".is-expanded"));
    const final = message("final", "home-client", "Hey! What's up?");
    await view.render({ ...props, workspace: { ...workspace, renderedMessages: [prompt, final] } });
    assert.ok(body.closest(".is-compact"));
    assert.equal(body.querySelector(".home-response-output__copy").textContent, final.text);
    const date = new Date(final.createdAtMs);
    assert.equal(body.querySelector("time").textContent, `${date.toLocaleDateString([], { day: "numeric", month: "short", year: "numeric" })} at ${date.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" })}`);
    for (const [daysAgo, label] of [[0, "Today"], [1, "Yesterday"]]) {
      const at = new Date();
      at.setDate(at.getDate() - daysAgo);
      await view.render({ ...props, workspace: { ...workspace, renderedMessages: [{ ...final, createdAtMs: at.getTime() }] } });
      assert.equal(body.querySelector("time").textContent, `${label} at ${at.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" })}`);
    }
    for (const createdAtMs of [0, NaN, Infinity, 9e15]) {
      await view.render({ ...props, workspace: { ...workspace, renderedMessages: [{ ...final, createdAtMs }] } });
      assert.equal(body.querySelector("header"), null);
      assert.doesNotMatch(body.textContent, /Octopus|Invalid Date/);
    }
    await view.render({ ...props, workspace: { ...workspace, renderedMessages: [prompt] } });
    await view.render({ ...props, workspace: { ...workspace, renderedMessages: [prompt, final] } });
    assert.ok(body.closest(".is-compact"));
  } finally { await view.close(); }
  const resumed = await mount(VoiceView, { ...props, session: session("listening") });
  const addedWidget = deliveredWidget("Newly installed extension", { sourceExtensionId: "new-extension", sourceRunId: "", sourceInterfaceId: "", document: { ...extensionWidget.document, id: "home.new-extension", title: "Newly installed extension" } });
  const installedProps = { ...props, installedWidgetIds: [...props.installedWidgetIds, addedWidget.document.id], surfaces: [...surfaces, addedWidget] };
  try {
    assert.ok(resumed.container.querySelector('[data-widget="Installed extension"]'));
    assert.equal(orbProps.ambient, false);
    const fade = motionState.get(resumed.container.querySelector(".home-response-stage__body"));
    assert.deepEqual([fade.initial.opacity, fade.animate.opacity], [0, 0]);
    await resumed.render(installedProps);
    assert.ok(resumed.container.querySelector('[data-widget="Newly installed extension"]'));
    assert.equal(button(resumed.container, "Newly installed extension").getAttribute("aria-checked"), "true");
    assert.ok(resumed.container.querySelector('[data-widget="Installed extension"]'));
    await click(button(resumed.container, "Installed extension"));
    await resumed.render({ ...installedProps, installedWidgetIds: [], surfaces: [] });
    await resumed.render(installedProps);
    assert.equal(resumed.container.querySelector('[data-widget="Installed extension"]'), null);
    assert.ok(resumed.container.querySelector('[data-widget="Newly installed extension"]'));
  } finally { await resumed.close(); }
  const customized = await mount(VoiceView, installedProps);
  try {
    assert.equal(customized.container.querySelector('[data-widget="Installed extension"]'), null);
    assert.ok(customized.container.querySelector('[data-widget="Newly installed extension"]'));
  } finally { await customized.close(); window.localStorage.removeItem(visibilityKey); }
});
