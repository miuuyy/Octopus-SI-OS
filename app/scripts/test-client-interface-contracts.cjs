const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { createRequire } = require("node:module");
const { test, after, afterEach } = require("node:test");
const ts = require("typescript");
const { JSDOM } = require("jsdom");
const React = require("react");
const { act } = React;
const dom = new JSDOM("<!doctype html><html><body></body></html>", { pretendToBeVisual: true, url: "http://localhost" });
const originals = new Map();
for (const name of ["window", "document", "navigator", "HTMLElement", "HTMLInputElement", "HTMLTextAreaElement", "Element", "Node", "KeyboardEvent", "Event", "getComputedStyle", "ResizeObserver", "IS_REACT_ACT_ENVIRONMENT"]) originals.set(name, global[name]);
for (const name of ["window", "document", "navigator", "HTMLElement", "HTMLInputElement", "HTMLTextAreaElement", "Element", "Node", "KeyboardEvent", "Event"]) global[name] = name === "window" ? dom.window : dom.window[name];
global.getComputedStyle = dom.window.getComputedStyle;
global.IS_REACT_ACT_ENVIRONMENT = true;
const { createRoot } = require("react-dom/client");
global.ResizeObserver = class { constructor(callback) { this.callback = callback; } observe(element) { this.callback([{ target: element, contentRect: element.getBoundingClientRect() }]); } disconnect() {} };
Object.defineProperties(HTMLElement.prototype, {
  clientWidth: { get() { return Number(this.closest("[data-contract-width]")?.dataset.contractWidth ?? 800); } },
  clientHeight: { get() { return 200; } },
});
HTMLElement.prototype.getBoundingClientRect = function () { return { x: 0, y: 0, left: 0, top: 0, right: this.clientWidth, bottom: 200, width: this.clientWidth, height: 200 }; };
HTMLElement.prototype.getClientRects = function () { return this.closest("[hidden]") ? [] : [this.getBoundingClientRect()]; };
const roots = [], releases = [], rendererErrors = [];
dom.window.addEventListener("error", (event) => rendererErrors.push(String(event.error ?? event.message)));
const appRoot = path.resolve(__dirname, "..");
function load(relative, mocks = {}) {
  const filename = path.join(appRoot, relative), localRequire = createRequire(filename);
  const code = ts.transpileModule(fs.readFileSync(filename, "utf8"), { fileName: filename, compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX } }).outputText;
  const module = { exports: {} };
  Function("module", "exports", "require", code)(module, module.exports, (id) => id in mocks ? mocks[id] : id.endsWith(".css") ? {} : localRequire(id));
  return module.exports;
}
function sourceFunction(relative, name, environment) {
  const source = ts.createSourceFile(relative, fs.readFileSync(path.join(appRoot, relative), "utf8"), ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
  let target;
  const visit = (node) => {
    if (ts.isFunctionDeclaration(node) && node.name?.text === name) target = node;
    if (ts.isVariableDeclaration(node) && node.name.getText(source) === name) target = node.initializer;
    ts.forEachChild(node, visit);
  };
  visit(source);
  assert.ok(target, `Missing ${relative}:${name}`);
  const code = ts.transpileModule(`return (${target.getText(source).replace(/^export\s+/, "")});`, { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText;
  return Function(...Object.keys(environment), code)(...Object.values(environment));
}
async function mount(element) {
  const host = document.createElement("div"); document.body.append(host);
  const root = createRoot(host); roots.push(root);
  await act(async () => root.render(element));
  return { host, root };
}
async function frame() { await act(async () => new Promise((resolve) => window.requestAnimationFrame(resolve))); }
function key(element, key, options = {}) { const event = new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true, ...options }); element.dispatchEvent(event); return event; }
afterEach(async () => { await act(async () => { for (const root of roots.splice(0)) root.unmount(); }); for (const release of releases.splice(0)) release(); document.body.replaceChildren(); assert.deepEqual(rendererErrors.splice(0), [], "Unhandled renderer error"); });
after(() => { dom.window.close(); for (const [name, value] of originals) global[name] = value; });

const modal = load("src/design-system/modalFocus.ts");
function dialog(id) { const element = document.createElement("div"); element.id = id; element.role = "dialog"; element.setAttribute("aria-modal", "true"); element.tabIndex = -1; document.body.append(element); return element; }
function isolate(element) { const release = modal.isolateModalLayer(element); releases.push(release); return release; }

test("overlapping modal layers stay reachable regardless of effect order and close order", () => {
  const background = document.createElement("button"); document.body.append(background);
  const lower = dialog("lower"), upper = dialog("upper");
  const closeUpper = isolate(upper), closeLower = isolate(lower);
  assert.equal(upper.hasAttribute("inert"), false);
  assert.equal(lower.hasAttribute("inert"), true);
  assert.equal(background.hasAttribute("inert"), true);
  assert.equal(modal.isTopmostModal(upper), true);
  closeLower(); assert.equal(background.hasAttribute("inert"), true);
  closeUpper(); assert.equal(background.hasAttribute("inert"), false);
  closeUpper(); assert.equal(document.body.style.overflow, "");
});

test("nested isolation restores existing inert state and body scrolling", () => {
  document.body.style.overflow = "clip";
  const locked = document.createElement("div"); locked.setAttribute("inert", ""); document.body.append(locked);
  const outer = dialog("outer"); outer.innerHTML = '<button id="parent-control">Parent</button><div id="nested" role="dialog" aria-modal="true" tabindex="-1"><input></div>';
  const nested = outer.querySelector("#nested");
  const closeOuter = isolate(outer), closeNested = isolate(nested);
  assert.equal(outer.hasAttribute("inert"), false);
  assert.equal(outer.querySelector("button").hasAttribute("inert"), true);
  closeNested(); assert.equal(outer.querySelector("button").hasAttribute("inert"), false);
  closeOuter(); assert.equal(locked.hasAttribute("inert"), true);
  assert.equal(document.body.style.overflow, "clip");
  document.body.style.overflow = "";
});

test("modal Tab wrapping skips hidden branches and ignores a covered dialog", () => {
  const lower = dialog("lower"); lower.innerHTML = '<button id="first">First</button><div hidden><button>Hidden</button></div><button id="last">Last</button>';
  const last = lower.querySelector("#last"); last.focus();
  const tab = new KeyboardEvent("keydown", { key: "Tab", cancelable: true }); modal.containModalFocus(tab, lower);
  assert.equal(tab.defaultPrevented, true); assert.equal(document.activeElement.id, "first");
  lower.focus(); modal.containModalFocus(new KeyboardEvent("keydown", { key: "Tab", shiftKey: true, cancelable: true }), lower);
  assert.equal(document.activeElement, last);
  const upper = dialog("upper"); isolate(upper);
  const covered = new KeyboardEvent("keydown", { key: "Tab", cancelable: true }); modal.containModalFocus(covered, lower);
  assert.equal(covered.defaultPrevented, false);
});

test("focus restoration cannot enter a hidden, inert or disconnected surface", () => {
  const anchor = document.createElement("button"); document.body.append(anchor); anchor.focus();
  const layer = dialog("layer"); const release = isolate(layer); layer.focus();
  modal.restoreModalFocus(anchor); assert.equal(document.activeElement, layer);
  release(); modal.restoreModalFocus(anchor); assert.equal(document.activeElement, anchor);
  anchor.hidden = true; layer.focus(); modal.restoreModalFocus(anchor); assert.equal(document.activeElement, layer);
  anchor.remove(); modal.restoreModalFocus(anchor); assert.equal(document.activeElement, layer);
});

const virtual = load("src/components/useFixedVirtualRange.ts");
test("virtual ranges remain valid after a scrolled list shrinks or becomes empty", () => {
  for (const count of [0, 1, 12, 1000]) for (const top of [0, 400, 50000]) {
    const range = virtual.fixedVirtualRange(count, 42, 6, top, 200);
    assert.ok(range.start >= 0 && range.start <= range.end && range.end <= count);
    assert.ok(count === 0 || range.end > range.start);
  }
});

test("the virtualizer attaches to a viewport that appears after an empty projection", async () => {
  let current;
  function List({ count }) { current = virtual.useFixedVirtualRange({ count, itemSize: 42 }); return count ? React.createElement("div", { onScroll: current.onScroll, ref: current.setScrollRef }) : null; }
  const { root, host } = await mount(React.createElement(List, { count: 0 }));
  await act(async () => root.render(React.createElement(List, { count: 1000 })));
  assert.equal(current.viewportWidth, 800); assert.equal(current.viewportHeight, 200);
  const viewport = host.firstChild;
  await act(async () => { viewport.scrollTop = 39000; viewport.dispatchEvent(new Event("scroll", { bubbles: true })); });
  assert.ok(current.start > 800);
  await act(async () => root.render(React.createElement(List, { count: 12 })));
  assert.ok(current.end <= 12 && current.start < current.end);
  assert.equal(viewport.scrollTop, 12 * 42 - 200);
});

test("artifact grid virtualization counts rows and survives filtering at the bottom", async () => {
  const { ArtifactLibrary } = load("src/components/artifacts/ArtifactLibrary.tsx", { "../IconGlyphs": { Glyph: () => null }, "./rendererRegistry": { artifactKindHint: () => "text" }, "../useFixedVirtualRange": virtual });
  const rows = Array.from({ length: 1000 }, (_, index) => ({ artifact_id: `artifact-${index}`, label: `File ${index}`, created_at: Date.now() + index }));
  const { host, root } = await mount(React.createElement(ArtifactLibrary, { rows, onSelect: () => {} }));
  const viewport = host.querySelector(".artifact-library-viewport");
  await act(async () => { viewport.scrollTop = 250 * 134 - 200; viewport.dispatchEvent(new Event("scroll", { bubbles: true })); });
  assert.ok(host.querySelector('[data-artifact-index="999"]'));
  assert.ok(host.querySelectorAll(".artifact-library-virtual-grid-row").length <= 10);
  await act(async () => root.render(React.createElement(ArtifactLibrary, { rows: rows.slice(0, 12), onSelect: () => {} })));
  assert.equal(host.querySelectorAll("[data-artifact-index]").length, 12);
});

test("Registry intersects its global and column filters without inventing a combined phrase", () => {
  const matches = sourceFunction("src/components/RegistryView.tsx", "registryFilterMatches", {});
  assert.equal(matches("world provider codex · ready", "provider", "codex"), true);
  assert.equal(matches("world provider codex · ready", "provider", "gemini"), false);
  assert.equal(matches("Activation.JSON", "  activation  ", "JSON"), true);
});

test("Save belongs to the visible Files pane, including its header, and respects modal ownership", () => {
  document.body.innerHTML = '<section id="primary"><input></section><section class="workspace-surface-pane"><button id="header">Tab</button><section id="files"><textarea></textarea></section></section>';
  let saves = 0, dialogOpen = false;
  const save = sourceFunction("src/components/files/FilesView.tsx", "onKeyDown", { active: true, locked: false, paneState: { closeRequested: false }, clientDialogOwnsCommands: () => dialogOpen, viewRef: { current: document.querySelector("#files") }, dirty: true, saveFile: async () => { saves++; } });
  const press = (target, prevented = false) => { const event = new KeyboardEvent("keydown", { key: "s", metaKey: true, cancelable: true }); Object.defineProperty(event, "target", { value: target }); if (prevented) event.preventDefault(); save(event); };
  press(document.querySelector("#primary input")); assert.equal(saves, 0);
  press(document.querySelector("textarea")); press(document.querySelector("#header")); assert.equal(saves, 2);
  dialogOpen = true; press(document.querySelector("textarea")); assert.equal(saves, 2);
  dialogOpen = false; press(document.querySelector("textarea"), true); assert.equal(saves, 2);
  document.querySelector("#files").setAttribute("inert", ""); press(document.querySelector("textarea")); assert.equal(saves, 2);
});

const { useFloatingMenu } = load("src/design-system/useFloatingMenu.ts", { "./modalFocus": modal });
test("Scheduled and Archived are labelled icon controls with independent selection", async () => {
  const events = [];
  const { TasksViewSwitcher } = load("src/components/task/TasksViewSwitcher.tsx", {
    "../IconGlyphs": { Glyph: () => React.createElement("svg", { "aria-hidden": true }) },
    "../chat/MessageTooltip": { MessageTooltip: ({ children }) => children },
  });
  const { host, root } = await mount(React.createElement(TasksViewSwitcher, { value: "tasks", onChange: (view) => events.push(view) }));
  const [all, scheduled, archived] = host.querySelectorAll("button");
  assert.equal(all.textContent, "All tasks"); assert.equal(all.getAttribute("aria-pressed"), "true");
  assert.equal(scheduled.textContent, ""); assert.equal(scheduled.getAttribute("aria-label"), "Triggers");
  assert.equal(archived.textContent, ""); assert.equal(archived.getAttribute("aria-label"), "Archived tasks");
  await act(async () => { scheduled.click(); archived.click(); all.click(); });
  assert.deepEqual(events, ["scheduled", "archived", "tasks"]);
  await act(async () => root.render(React.createElement(TasksViewSwitcher, { value: "archived", onChange() {} })));
  assert.deepEqual([...host.querySelectorAll("button")].map((button) => button.getAttribute("aria-pressed")), ["false", "false", "true"]);
});

test("task statuses have compact accessible indicators without replacing the title or breaking row actions", async () => {
  const appearance = { useShouldReduceMotion: () => true };
  const glyphs = { Glyph: () => React.createElement("svg", { "aria-hidden": true }) };
  const { SidebarTaskStatus } = load("src/components/navigation/SidebarTaskStatus.tsx", { "../../appearance": appearance, "../IconGlyphs": glyphs });
  const { SidebarWorkingIndicator } = load("src/components/navigation/SidebarWorkingIndicator.tsx", { "./SidebarTaskStatus": { SidebarTaskStatus } });
  const { OctopusTaskRow } = load("src/components/navigation/OctopusNav.tsx", {
    "../../appearance": appearance,
    "motion/react": { AnimatePresence: ({ children }) => children, motion: {} },
    "./OctopusMarquee": { OctopusMarquee: ({ animateOnGroupHover, ...props }) => React.createElement("span", props) },
    "./SidebarWorkingIndicator": { SidebarWorkingIndicator },
    "./SidebarTaskStatus": { SidebarTaskStatus },
    "./TaskHoverCard": { TaskHoverCard: ({ children }) => children },
    "../IconGlyphs": { Glyph: () => React.createElement("svg", { "aria-hidden": true }) },
    "../chat/MessageTooltip": { MessageTooltip: ({ children }) => children },
  });
  let opened = 0, restored = 0;
  const props = { title: "Actual task", active: false, onClick: () => opened++, meta: "3d", action: { glyph: "retry", label: "Restore task", onClick: () => restored++ } };
  const { host, root } = await mount(React.createElement(OctopusTaskRow, { ...props, failed: true, unread: true }));
  const row = host.querySelector('[role="button"]');
  assert.equal(host.querySelector("[data-thread-title]").textContent, "Actual task");
  assert.equal(host.textContent.includes("Attention"), false); assert.equal(row.classList.contains("is-task-failed"), false);
  assert.equal(host.querySelector('[role="img"]').getAttribute("aria-label"), "Failed");
  assert.equal(host.querySelector('[role="img"]').getAttribute("title"), "Task failed · open to view the error");
  assert.equal(host.querySelector('[role="img"]').textContent, "");
  assert.equal(host.querySelectorAll('[role="img"]').length, 1);
  assert.match(row.getAttribute("aria-label"), /Failed.*unread/);
  await act(async () => host.querySelector("button").click()); assert.equal(restored, 1); assert.equal(opened, 0);
  await act(async () => key(row, "Enter")); assert.equal(opened, 1);
  await act(async () => root.render(React.createElement(OctopusTaskRow, { ...props, working: true })));
  assert.equal(host.querySelector('[role="button"]').getAttribute("aria-busy"), "true");
  assert.equal(host.querySelector('[role="img"]').getAttribute("aria-label"), "Running");
  assert.equal(host.querySelector('[role="img"]').dataset.reduceMotion, "true");
  await act(async () => root.render(React.createElement(OctopusTaskRow, { ...props, waiting: true })));
  assert.equal(host.querySelector('[role="img"]').getAttribute("aria-label"), "Waiting");
  await act(async () => root.render(React.createElement(OctopusTaskRow, { ...props, unread: true })));
  assert.equal(host.querySelector('[role="img"]').getAttribute("aria-label"), "Unread reply");
  assert.equal(host.querySelector('[role="img"]').textContent, "");
  assert.equal(host.querySelector("[data-thread-title]").dataset.unread, "true");
  await act(async () => root.render(React.createElement(OctopusTaskRow, { ...props, action: { ...props.action, disabled: true } })));
  assert.equal(host.querySelector("button").disabled, true);
});

test("Settings keeps window history without a sidebar toggle, and native history yields to a modal", async () => {
  const calls = [], handlers = {};
  window.octopusTransport = {
    windowState: async () => ({ fullScreen: false, nativeControlInset: 76 }),
    onWindowStateChanged: (handler) => { handlers.state = handler; return () => {}; },
    setHistoryNavigationState: (state) => calls.push(state),
    onNavigateBack: (handler) => { handlers.back = handler; return () => {}; },
    onNavigateForward: (handler) => { handlers.forward = handler; return () => {}; },
  };
  releases.push(() => { delete window.octopusTransport; });
  const rect = HTMLElement.prototype.getBoundingClientRect;
  HTMLElement.prototype.getBoundingClientRect = function () {
    return this.role === "tooltip" ? { x: 50, y: 202, left: 50, top: 202, right: 250, bottom: 232, width: 200, height: 30 } : rect.call(this);
  };
  releases.push(() => { HTMLElement.prototype.getBoundingClientRect = rect; });
  const tooltip = load("src/components/shell/ShellTooltip.tsx", {
    "../chat/MessageTooltip": load("src/components/chat/MessageTooltip.tsx"),
  });
  const { DesktopTitleBar } = load("src/components/shell/DesktopTitleBar.tsx", {
    "../IconGlyphs": { Glyph: () => null },
    "./ShellTooltip": tooltip,
    "../../keyboardShortcuts": {
      clientDialogOwnsCommands: sourceFunction("src/keyboardShortcuts.ts", "clientDialogOwnsCommands", {}),
      useKeyboardShortcuts: () => ({ navigate_back: ["⌘⌥←"] }),
      formatKeyboardShortcuts: (shortcuts) => shortcuts.join("  "),
    },
    "motion/react": { AnimatePresence: ({ children }) => children, motion: { div: React.forwardRef(({ children, animate, initial, exit, transition, inert, ...props }, ref) => React.createElement("div", { ...props, ref }, children)) }, useMotionValueEvent() {}, useTransform: () => 8 },
  });
  let back = 0, forward = 0, toggled = 0;
  const { host } = await mount(React.createElement(DesktopTitleBar, {
    accessProfile: "lab", settingsOpen: true, canNavigateBack: true, canNavigateForward: false,
    sidebarOpen: true, secondaryPaneOpen: false, secondaryPaneWidth: { get: () => 0 },
    onNavigateBack: () => back++, onNavigateForward: () => forward++, onToggleSidebar: () => toggled++, onSlotWidthsChange() {},
  }));
  assert.deepEqual([...host.querySelector(".app-titlebar-controls").querySelectorAll("button")].map((button) => button.getAttribute("aria-label")), ["Back", "Forward"]);
  assert.deepEqual(calls.at(-1), { canNavigateBack: true, canNavigateForward: false });
  assert.equal(host.querySelector("header").style.getPropertyValue("--app-titlebar-safe-left"), "82px");
  const backButton = host.querySelector('[aria-label="Back"]');
  const hover = () => backButton.dispatchEvent(new window.MouseEvent("pointermove", { bubbles: true }));
  await act(async () => hover());
  assert.ok(document.querySelector('[role="tooltip"]') === null);
  await act(async () => new Promise((resolve) => setTimeout(resolve, 220)));
  assert.equal(document.querySelector('[role="tooltip"] kbd').textContent, "⌘⌥←");
  assert.equal(document.querySelector('[role="tooltip"]').dataset.side, "bottom");
  assert.equal(backButton.hasAttribute("title"), false);
  await act(async () => backButton.dispatchEvent(new window.MouseEvent("pointerdown", { bubbles: true })));
  assert.ok(document.querySelector('[role="tooltip"]') === null, "Tooltip closes even when the titlebar button stops bubbling");
  await act(async () => backButton.click());
  host.querySelector('[aria-label="Forward"]').click();
  assert.deepEqual([back, forward, toggled], [1, 0, 0]);
  const menu = dialog("blocking-menu");
  handlers.back(); assert.equal(back, 1);
  menu.remove(); handlers.back(); assert.equal(back, 2);
  await act(async () => handlers.state({ fullScreen: true, nativeControlInset: 76 }));
  assert.equal(host.querySelector("header").style.getPropertyValue("--app-titlebar-safe-left"), "6px");
  await act(async () => handlers.state({ fullScreen: false, nativeControlInset: 76 / 1.4 }));
  assert.equal(parseFloat(host.querySelector("header").style.getPropertyValue("--app-titlebar-safe-left")), 76 / 1.4 + 6);
});

test("Dashboard title menu selects views, keeps extensions and pins, and restores focus on Escape", async () => {
  const hasFocus = document.hasFocus;
  document.hasFocus = () => true;
  releases.push(() => { document.hasFocus = hasFocus; });
  const { OcDropdown } = load("src/design-system/OcDropdown.tsx", {
    "./modalFocus": modal,
    "./components": { OcMenuSurface: ({ surfaceRef, surface, ...props }) => React.createElement("div", { ...props, ref: surfaceRef }) },
  });
  const workspaces = load("src/shell/clientWorkspaces.ts");
  const { DashboardViewMenu } = load("src/components/shell/DashboardViewMenu.tsx", {
    "../../shell/clientWorkspaces": workspaces,
    "../../shell/navigation": load("src/shell/navigation.ts", { "./clientWorkspaces": workspaces }),
    "../../design-system/OcDropdown": { OcDropdown },
    "../../design-system/components": { OcMenuItem: ({ label, leadingIcon, selected, ...props }) => React.createElement("button", props, label) },
    "../IconGlyphs": { Glyph: () => null },
    "./ShellTooltip": { ShellTooltip: ({ children }) => children },
  });
  const selected = [], pins = [];
  const surfaces = [
    { id: "dashboard", label: "Topology", order: 0, placements: ["fullscreen"] },
    { id: "registry", label: "Registry", order: 1, placements: ["fullscreen"] },
    { id: "extension", label: "Custom view", order: 2, placements: ["fullscreen"] },
    { id: "chat", label: "Tasks", order: 3, placements: ["home"] },
  ];
  const props = { activeSurfaceId: "dashboard", surfaces, pinnedSurfaceIds: [], onSelect: (id) => selected.push(id), onPinnedSurfaceIdsChange: (ids) => pins.push(ids) };
  const { host, root } = await mount(React.createElement(DashboardViewMenu, props));
  const trigger = host.querySelector("button"); trigger.focus();
  await act(async () => key(trigger, "ArrowDown"));
  assert.equal(trigger.getAttribute("aria-expanded"), "true");
  assert.deepEqual([...document.querySelectorAll('[role="menuitemradio"]')].map((item) => item.textContent), ["Topology", "Registry", "Custom view"]);
  assert.equal(document.querySelector('[aria-checked="true"]').textContent, "Topology");
  await act(async () => key(document.activeElement, "ArrowDown"));
  assert.equal(document.activeElement.textContent, "Registry");
  await act(async () => document.activeElement.click());
  assert.deepEqual(selected, ["registry"]); assert.ok(document.querySelector('[role="menu"]') === null);
  await act(async () => root.render(React.createElement(DashboardViewMenu, { ...props, activeSurfaceId: "extension" })));
  await act(async () => trigger.click());
  await act(async () => document.querySelector('[role="menuitem"]').click());
  assert.deepEqual(pins, [["extension"]]);
  await act(async () => trigger.click());
  await act(async () => key(document.activeElement, "Escape"));
  assert.ok(document.querySelector('[role="menu"]') === null); assert.equal(document.activeElement, trigger);
});

test("provider validation waits for a successful registry read and leaves repair to an explicit selection", async () => {
  let providerState = { loading: true, error: "", providers: [] };
  const toasts = [], writes = [];
  const { GeneralSection } = load("src/components/settings/GeneralSettingsSection.tsx", {
    "./settingsCatalog": { settingsTargetProps: () => ({}) },
    "../../clientPreferences": { useClientPreferences: () => ({ startupSurfaceId: "voice" }), useSetClientPreferences: () => () => {} },
    "../../design-system/components": {
      OcGroup: ({ children }) => children, OcCard: ({ children }) => children,
      OcSettingsRow: ({ control }) => control, OcStatus: () => null, OcSwitch: () => null, OcSlider: () => null, OcButton: () => null,
      OcSelect: ({ label, value, options, error, disabled, onChange }) => React.createElement("select", { "aria-label": label, "data-error": error, disabled, value, onChange: (event) => onChange(event.target.value) }, options.map((option) => React.createElement("option", { key: option.value, value: option.value, disabled: option.disabled }, option.label))),
    },
    "../../toasts": { useAppErrorToast: (error) => { if (error) toasts.push(error); } },
    "../chat/storage": { loadChatSettings: () => ({ providerInterfaceId: "provider-a" }), saveChatSettings: (value) => writes.push(value) },
    "../chat/chatWorkspaceCapabilities": { providerDefaultModel: () => null, providerDefaultReasoningEffort: () => "" },
    "../../shell/navigation": { clientNavigationDestinations: [] },
    "./SettingsHeading": { SettingsHeading: () => null },
    "./NotificationSettingsSection": { MarkActivitySettingsRow: () => null, NotificationSoundSettingsRow: () => null },
    "./SettingsProviderState": { ProviderSummary: () => null, useDeclaredProviders: () => providerState },
    "../../projectionClient": { readProjection: async () => null, saveSetupFile: async () => ({ saved: false }) },
    "../../onboarding/setupFile": { serializeSetupFile: () => "", setupFileFromHome: () => null, setupFileName: () => "" },
    "../../extensions/useClientExtensions": { useClientExtensions: () => ({ loading: false, extensions: [] }) },
  });
  const props = { accessProfile: "lab", surfaces: [] };
  const { host, root } = await mount(React.createElement(GeneralSection, props));
  const select = () => host.querySelector('[aria-label="Default provider interface"]');
  assert.equal(select().hasAttribute("data-error"), false); assert.equal(select().disabled, true);
  providerState = { loading: false, error: "", providers: [{ id: "provider-a", label: "Provider A" }] };
  await act(async () => root.render(React.createElement(GeneralSection, props)));
  assert.equal(select().hasAttribute("data-error"), false);
  providerState = { loading: false, error: "Registry unavailable", providers: [] };
  await act(async () => root.render(React.createElement(GeneralSection, props)));
  assert.equal(select().dataset.error, "Registry unavailable");
  providerState = { loading: false, error: "", providers: [] };
  await act(async () => root.render(React.createElement(GeneralSection, props)));
  assert.match(select().dataset.error, /saved interface is unavailable/);
  assert.equal(select().value, "provider-a"); assert.deepEqual(writes, []);
  assert.deepEqual(toasts, ["Registry unavailable"]);
  await act(async () => { select().value = ""; select().dispatchEvent(new Event("change", { bubbles: true })); });
  assert.equal(writes[0].providerInterfaceId, ""); assert.equal(select().hasAttribute("data-error"), false);
});

test("opening Settings from Account preserves the destination's initial focus", async () => {
  let live = false;
  const { OcDropdown } = load("src/design-system/OcDropdown.tsx", {
    "./modalFocus": modal,
    "./components": { OcMenuSurface: ({ surfaceRef, surface, ...props }) => React.createElement("div", { ...props, ref: surfaceRef }) },
  });
  const { AccountRailButton } = load("src/components/shell/AccountRailButton.tsx", {
    "../../account/useChatgptAccount": { useChatgptAccount: (enabled) => { assert.equal(enabled, live); return { account: null, action: "idle", loading: false }; } },
    "../../account/useChatgptUsage": { useChatgptUsage: (_account, enabled) => { assert.equal(enabled, false); return { value: null, loading: false, error: "" }; } },
    "../../account/chatgptUsageModel": load("src/account/chatgptUsageModel.ts"),
    "../../account/chatgptWorkspaces": load("src/account/chatgptWorkspaces.ts"),
    "../account/AccountLogoutDialog": { AccountLogoutDialog: ({ open }) => { assert.equal(open, false); return null; } },
    "../account/AccountUsage": { AccountUsageCompact: () => { throw new Error("Lab focus navigation must not render native usage"); } },
    "../../design-system/OcDropdown": { OcDropdown },
    "../../design-system/components": { OcMenuItem: ({ label, leadingIcon, trailing, ...props }) => React.createElement("button", props, label) },
    "../../keyboardShortcuts": { formatKeyboardShortcuts: () => "", useKeyboardShortcuts: () => ({}) },
    "../chat/MessageTooltip": load("src/components/chat/MessageTooltip.tsx"),
  });
  function Shell() {
    const [settings, setSettings] = React.useState(false);
    return React.createElement(React.Fragment, null,
      React.createElement(AccountRailButton, { accessProfile: "lab", runtimeHost: {}, onOpenSettings: () => setSettings(true), onLogOut() {} }),
      settings && React.createElement("input", { autoFocus: true, id: "settings-search" }),
    );
  }
  const { host, root } = await mount(React.createElement(Shell));
  await act(async () => host.querySelector("button").click());
  await act(async () => document.querySelector('[role="menuitem"]').click());
  assert.equal(document.activeElement, host.querySelector("input"));
  assert.ok(document.querySelector('[role="menu"]') === null);
  await act(async () => host.querySelector("button").click());
  await act(async () => document.querySelector('[role="menuitem"]').click());
  assert.equal(document.activeElement, host.querySelector("input"));
  live = true;
  await act(async () => root.render(React.createElement(AccountRailButton, { accessProfile: "live", runtimeHost: { available: true, ready: true, phase: "ready" }, onOpenSettings() {}, onLogOut() {} })));
  const dot = host.querySelector(".navigation-rail-account-status");
  assert.equal(dot.parentElement.className, "navigation-rail-account"); assert.equal(dot.hasAttribute("title"), false);
  await act(async () => { dot.dispatchEvent(new window.MouseEvent("pointermove", { bubbles: true })); await new Promise(resolve => setTimeout(resolve, 330)); });
  assert.equal(document.querySelectorAll('[role="tooltip"]').length, 1); assert.equal(document.querySelector('[role="tooltip"]').textContent, "Runtime connected");
});

test("context menus retain keyboard position across projection updates and restore focus on Escape", async () => {
  const origin = document.createElement("button"); document.body.append(origin); origin.focus();
  function Menu({ revision }) {
    const [open, setOpen] = React.useState(true), ref = React.useRef(null);
    useFloatingMenu(open, ref, () => setOpen(false), { left: 1000, top: 1000 });
    return open ? React.createElement("div", { ref, role: "menu" }, React.createElement("button", { role: "menuitem" }, "First"), React.createElement("button", { role: "menuitem", disabled: true }, "Unavailable"), React.createElement("button", { role: "menuitem" }, `Last ${revision}`)) : null;
  }
  const { host, root } = await mount(React.createElement(Menu, { revision: 1 })); await frame();
  await act(async () => key(document.activeElement, "ArrowDown")); assert.equal(document.activeElement.textContent, "Last 1");
  await act(async () => root.render(React.createElement(Menu, { revision: 2 }))); await frame();
  assert.equal(document.activeElement.textContent, "Last 2");
  const menu = host.firstChild; assert.ok(parseFloat(menu.style.top) + menu.getBoundingClientRect().height <= window.innerHeight);
  await act(async () => key(document.activeElement, "Escape")); assert.equal(host.firstChild, null); assert.equal(document.activeElement, origin);
});

test("dismissal by an outside pointer preserves the newly focused control", async () => {
  const outside = document.createElement("button"); document.body.append(outside);
  function Menu() { const [open, setOpen] = React.useState(true), ref = React.useRef(null); useFloatingMenu(open, ref, () => setOpen(false), { left: 0, top: 0 }); return open ? React.createElement("div", { ref }, React.createElement("button", { role: "menuitem" }, "Action")) : null; }
  await mount(React.createElement(Menu)); await frame();
  await act(async () => { outside.dispatchEvent(new dom.window.MouseEvent("pointerdown", { bubbles: true })); outside.focus(); });
  assert.equal(document.activeElement, outside);
});

const { WorkspacePaneManager } = load("src/components/shell/WorkspacePaneManager.tsx", {
  "../../design-system/modalFocus": modal,
  "../../keyboardShortcuts": { clientDialogOwnsCommands: sourceFunction("src/keyboardShortcuts.ts", "clientDialogOwnsCommands", {}) },
  "./PanelResizeHandle": { PanelResizeHandle: () => null, useAnimatedPanelSize: (open, width) => ({ mounted: open, animatedSize: width, opacity: 1 }) },
  "motion/react": { motion: { aside: "aside" } },
});
test("a narrow secondary overlay blocks the covered pane and returns focus when closed", async () => {
  const blocked = [];
  function Panes() { const [open, setOpen] = React.useState(false); return React.createElement("div", { "data-contract-width": "700" }, React.createElement(WorkspacePaneManager, { primary: React.createElement("button", { onClick: () => setOpen(true) }, "Open"), secondary: React.createElement("input", { "aria-label": "Secondary" }), secondaryOpen: open, secondaryFocused: false, secondaryWidthPercent: 34, onOpenChange: setOpen, onPrimaryBlockedChange: (value) => blocked.push(value), onSecondaryFocusedChange: () => {}, onSecondaryWidthPercentChange: () => {}, resolvedWidthMotion: { set: () => {} } })); }
  const { host } = await mount(React.createElement(Panes)); const button = host.querySelector("button"); button.focus();
  await act(async () => button.click());
  assert.equal(host.querySelector(".workspace-pane-primary").hasAttribute("inert"), true);
  assert.equal(document.activeElement, host.querySelector("input")); assert.equal(blocked.at(-1), true);
  await act(async () => key(document.activeElement, "Escape"));
  assert.equal(host.querySelector(".workspace-pane-primary").hasAttribute("inert"), false); assert.equal(document.activeElement, button);
});

test("a wide secondary pane leaves its primary pane available", async () => {
  const { host } = await mount(React.createElement("div", { "data-contract-width": "1000" }, React.createElement(WorkspacePaneManager, { primary: "Primary", secondary: "Secondary", secondaryOpen: true, secondaryFocused: false, secondaryWidthPercent: 34, onOpenChange: () => {}, onSecondaryFocusedChange: () => {}, onSecondaryWidthPercentChange: () => {}, resolvedWidthMotion: { set: () => {} } })));
  assert.equal(host.querySelector(".workspace-pane-primary").hasAttribute("inert"), false);
});

test("portaled pane controls dismiss their own manager and yield Escape to an open menu", async () => {
  const closed = [];
  const panes = (name) => React.createElement("div", { "data-contract-width": "700" }, React.createElement(WorkspacePaneManager, {
    primary: name, secondary: React.createElement("input"), secondaryOpen: true, secondaryFocused: false, secondaryWidthPercent: 34,
    onOpenChange: () => closed.push(name), onSecondaryFocusedChange: () => {}, onSecondaryWidthPercentChange: () => {}, resolvedWidthMotion: { set: () => {} },
  }));
  const { host } = await mount(React.createElement(React.Fragment, null, panes("first"), panes("second")));
  const controls = document.createElement("div"), button = document.createElement("button");
  controls.dataset.paneManagerControls = "true";
  controls.dataset.paneManagerId = host.querySelectorAll(".workspace-pane-manager")[1].dataset.paneManagerId;
  controls.append(button); document.body.append(controls); button.focus();
  const menu = document.createElement("div"); menu.role = "menu"; menu.dataset.settingsLayer = "true"; document.body.append(menu);
  await act(async () => key(button, "Escape")); assert.deepEqual(closed, []);
  menu.remove();
  await act(async () => key(button, "Escape")); assert.deepEqual(closed, ["second"]);
});
