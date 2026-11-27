const assert = require("node:assert/strict");
const { EventEmitter } = require("node:events");
const { readFileSync } = require("node:fs");
const { createRequire } = require("node:module");
const path = require("node:path");
const { pathToFileURL } = require("node:url");
const { test } = require("node:test");
const ts = require("typescript");
const { createClientContextMenuHost } = require("../electron/clientContextMenu.cjs");
const app = path.resolve(__dirname, "..");
const tick = () => new Promise((resolve) => setImmediate(resolve));
function load(file, mocks = {}) {
  const filename = path.join(app, file), module = { exports: {} }, localRequire = createRequire(filename);
  const compiled = ts.transpileModule(readFileSync(filename, "utf8"), { fileName: filename, reportDiagnostics: true, compilerOptions: {
    target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX,
  } });
  assert.deepEqual(compiled.diagnostics, []);
  Function("require", "module", "exports", compiled.outputText)((id) => Object.hasOwn(mocks, id) ? mocks[id] : localRequire(id), module, module.exports);
  return module.exports;
}
function fixture() {
  const entries = new Map(), handlers = new Map(), menus = [], errors = [];
  const dependencies = { createClientContextMenuHost, path, __dirname: path.join(app, "electron"), require, isDev: false,
    console: { error: (...args) => errors.push(args) },
    clientWindows: { entryForWebContents: (id) => entries.get(id) },
    ipcMain: { handle: (id, fn) => handlers.set(id, fn) },
    Menu: { buildFromTemplate(template) {
      const menu = { template, closed: 0, popup(options) { this.options = options; }, closePopup(win) { assert.equal(win, this.options.window); this.closed++; this.options.callback(); } };
      menus.push(menu); return menu;
    } },
  };
  const source = ts.createSourceFile("main.cjs", readFileSync(path.join(app, "electron/main.cjs"), "utf8"), ts.ScriptTarget.Latest, true);
  const statements = source.statements.filter((node) => ts.isVariableStatement(node)
    ? node.declarationList.declarations.some((d) => d.name.getText(source) === "clientContextMenu")
    : ts.isExpressionStatement(node) && ts.isCallExpression(node.expression) && node.expression.expression.getText(source) === "ipcMain.handle" && node.expression.arguments[0]?.text?.startsWith("octopus:client-context-menu"));
  assert.equal(statements.length, 3);
  Function(...Object.keys(dependencies), statements.map((n) => n.getText(source)).join("\n"))(...Object.values(dependencies));
  function add(id) {
    const win = Object.assign(new EventEmitter(), { id, dead: false, isDestroyed() { return this.dead; }, getContentBounds: () => ({ width: 800, height: 600 }) });
    win.webContents = Object.assign(new EventEmitter(), { id, dead: false, mainFrame: { url: pathToFileURL(path.join(app, "dist/index.html")).href }, isDestroyed() { return this.dead; }, getZoomFactor: () => 1.2 });
    const event = { sender: win.webContents, senderFrame: win.webContents.mainFrame };
    entries.set(id, { win });
    let bridge;
    Function("require", "process", readFileSync(path.join(app, "electron/preload.cjs"), "utf8"))((name) => {
      assert.equal(name, "electron");
      return { contextBridge: { exposeInMainWorld: (_name, value) => { bridge = value; } }, ipcRenderer: { invoke: async (channel, ...args) => handlers.get(channel)(event, ...args) } };
    }, { argv: [] });
    return { win, event, bridge };
  }
  return { add, menus, errors, handlers, entries };
}
const request = (id = "request-1") => ({ requestId: id, position: { x: 50, y: 70 }, items: [{ id: "action", label: "Pin & keep", accelerator: "CmdOrCtrl+P" }] });

test("window titlebar displays the active surface glyph and label across Tasks, Home, and Scheduled", () => {
  const source = ts.createSourceFile('OperatorShell.tsx', readFileSync(path.join(app, 'src/components/shell/OperatorShell.tsx'), 'utf8'), ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
  let context;
  const visit = (node) => {
    if (ts.isJsxAttribute(node) && node.name.text === 'titlebarSurfaceContext') context = node.initializer.expression.getText(source);
    ts.forEachChild(node, visit);
  };
  visit(source); assert.ok(context);
  const titlebar = Function('activeSurface', `return ${context};`);
  assert.deepEqual(titlebar({ rendererKind: 'builtin.chat', label: 'Tasks', glyph: 'tasks', chrome: {} }), { glyph: 'tasks', title: 'Tasks' });
  assert.deepEqual(titlebar({ rendererKind: 'builtin.voice', label: 'Home', glyph: 'home', chrome: {} }), { glyph: 'home', title: 'Home' });
  assert.deepEqual(titlebar({ rendererKind: 'builtin.scheduled', label: 'Scheduled', glyph: 'clock', chrome: {} }), { glyph: 'clock', title: 'Scheduled' });
  assert.deepEqual(titlebar({ rendererKind: 'builtin.dashboard', label: 'Dashboard', glyph: 'dashboardRail', chrome: { titlebarGlyph: 'topology' } }), { glyph: 'topology', title: 'Dashboard' });
});

test("actual main and preload native menu route bounds data and owns selection, cancellation and lifetime per sender", async () => {
  const f = fixture(), a = f.add(1), b = f.add(2);
  const first = a.bridge.showClientContextMenu(request());
  assert.equal(f.menus.length, 1);
  const menu = f.menus[0]; assert.equal(menu.template[0].label, "Pin && keep"); assert.equal(menu.template[0].registerAccelerator, false);
  assert.equal(menu.options.x, 60); assert.equal(menu.options.y, 84);
  menu.template[0].click(); menu.options.callback(); menu.template[0].click();
  assert.deepEqual(await first, { requestId: "request-1", id: "action" });
  const blocked = [
    { ...request(), role: "quit" }, { ...request(), items: [{ id: "a", label: "Quit", role: "quit" }] },
    { ...request(), items: [{ id: "a", label: "File", icon: "/tmp/secret" }] },
    { ...request(), items: [{ id: "a", label: "Run", click() {} }] },
    { ...request(), items: [{ id: "a", label: "x", checked: true }] },
    { ...request(), items: [{ id: "a", label: "x", accelerator: "not-a-key" }] },
    { ...request(), items: Array(257).fill({ type: "separator" }) },
    { ...request(), position: { x: NaN, y: 0 } },
    { ...request(), items: [{ id: "a", label: "x" }, { id: "a", label: "y" }] },
  ];
  let deep = [{ id: "leaf", label: "leaf" }]; for (let i = 0; i < 10; i++) deep = [{ id: `level:${i}`, label: "Nested", submenu: deep }]; blocked.push({ ...request(), items: deep });
  for (const value of blocked) await assert.rejects(a.bridge.showClientContextMenu(value));
  assert.equal(f.menus.length, 1);
  const savedFrame = a.event.senderFrame;
  a.event.senderFrame = { ...savedFrame }; await assert.rejects(a.bridge.showClientContextMenu(request()), /current main/); a.event.senderFrame = savedFrame;
  savedFrame.url = "https://untrusted.invalid/"; await assert.rejects(a.bridge.showClientContextMenu(request()), /trusted/); savedFrame.url = pathToFileURL(path.join(app, "dist/index.html")).href;
  const old = a.bridge.showClientContextMenu(request("old")), foreign = b.bridge.showClientContextMenu(request("foreign"));
  const next = a.bridge.showClientContextMenu({ ...request("next"), items: [{ id: "parent", label: "Disabled", enabled: false, submenu: [{ id: "leaf", label: "Selected", type: "checkbox", checked: true }] }] });
  assert.equal((await old).id, null); assert.equal(f.menus[1].closed, 1); assert.equal(f.menus[2].closed, 0);
  assert.deepEqual(await a.bridge.cancelClientContextMenu({ requestId: "old" }), { cancelled: false });
  assert.equal(f.menus[3].template[0].type, "submenu");
  f.menus[3].template[0].submenu[0].click();
  assert.deepEqual(await a.bridge.cancelClientContextMenu({ requestId: "next" }), { cancelled: true }); assert.equal((await next).id, null);
  b.win.webContents.emit("did-start-navigation", {}, "https://child.invalid/", false, false); assert.equal(f.menus[2].closed, 0);
  b.win.webContents.emit("did-start-navigation", {}, "#route", true, true); assert.equal((await foreign).id, null);
  for (const lifetime of ["destroyed", "render-process-gone", "closed"]) {
    const current = a.bridge.showClientContextMenu(request(lifetime));
    (lifetime === "closed" ? a.win : a.win.webContents).emit(lifetime);
    assert.equal((await current).id, null);
  }
  const stale = a.bridge.showClientContextMenu(request("stale-frame")); a.win.webContents.mainFrame = { ...savedFrame };
  f.menus.at(-1).template[0].click(); assert.equal((await stale).id, null); a.win.webContents.mainFrame = savedFrame;
  assert.equal(a.win.webContents.listenerCount("did-start-navigation"), 0);
  assert.deepEqual(f.errors, []);
});

test("long multiline labels reach the native menu without changing action identity or splitting graphemes", async () => {
  const { nativeTaskMenuLabel } = load("src/shell/taskActionsMenu.ts");
  const family = "👩‍👩‍👧‍👦", raw = `Project\n\t\u0000${family.repeat(50)} end`;
  const f = fixture(), a = f.add(3);
  await assert.rejects(a.bridge.showClientContextMenu({ ...request("raw-label"), items: [{ id: "copy-title", label: raw }] }), /Invalid native context-menu label/);
  const label = nativeTaskMenuLabel(raw);
  assert.ok(label.length <= 180); assert.doesNotMatch(label, /[\x00-\x1f\x7f]/);
  assert.ok(label.startsWith("Project ")); assert.ok(label.endsWith("…"));
  assert.equal(label.slice(8, -1).length % family.length, 0);
  const selection = a.bridge.showClientContextMenu({ ...request("formatted-label"), items: [{ id: "copy-title", label }] });
  const menu = f.menus.at(-1); assert.equal(menu.template[0].label, label); menu.template[0].click();
  assert.deepEqual(await selection, { requestId: "formatted-label", id: "copy-title" });
});

test("task controller revalidates native context menus and anchored titlebar menus against current task authority", async () => {
  const { JSDOM } = require("jsdom"), React = require("react"), { createRoot } = require("react-dom/client");
  const dom = new JSDOM('<div id="root"></div>', { url: "http://localhost", pretendToBeVisual: true }), w = dom.window;
  const previousNavigator = Object.getOwnPropertyDescriptor(globalThis, "navigator");
  Object.defineProperty(w.navigator, "platform", { configurable: true, value: "MacIntel" });
  Object.defineProperty(globalThis, "navigator", { configurable: true, value: w.navigator });
  for (const key of ["window", "document", "Element", "HTMLElement", "Node", "Event", "CustomEvent"]) globalThis[key] = key === "window" ? w : w[key];
  globalThis.IS_REACT_ACT_ENVIRONMENT = true;
  globalThis.ResizeObserver = class { observe() {} disconnect() {} };
  const events = load("src/shell/taskActionsMenu.ts"), effects = [], errors = [], pending = [], cancellations = [];
  let organization = { aliases: { task: "Alias" } };
  const transport = { showClientContextMenu(value) { return new Promise((resolve, reject) => pending.push({ value, resolve, reject })); }, cancelClientContextMenu: async (value) => { cancellations.push(value); return { cancelled: true }; } };
  const workspaces = { useClientTaskOrganization: () => organization, clientTaskTitle: (id, title, org) => org.aliases[id] || title,
    flattenClientChatProjects: (projects) => projects.map((project) => ({ project, depth: project.parentId ? 1 : 0 })),
  };
  for (const name of ["clearClientTaskAlias", "removeClientTaskPresentation", "renameClientTask", "restoreClientTaskAliasIfAbsent"]) workspaces[name] = (...args) => effects.push([name, ...args]);
  const components = {
    OcDialog: ({ open, children }) => open ? React.createElement("div", { role: "dialog" }, children) : null,
    OcDialogHeader: ({ title }) => React.createElement("h2", null, title), OcDialogFooter: ({ children }) => React.createElement("div", null, children),
    OcButton: ({ buttonRef, variant, tone, ...props }) => React.createElement("button", { ...props, ref: buttonRef }),
    OcTextField: ({ inputRef, label, ...props }) => React.createElement("input", { ...props, ref: inputRef, "aria-label": label }),
    OcMenuSurface: ({ surfaceRef, ...props }) => React.createElement("div", { ...props, ref: surfaceRef }),
    OcMenuItem: ({ label, leadingIcon, tone, description, trailing, ...props }) => React.createElement("button", props, label, trailing),
  };
  const popover = load("src/components/task/TaskActionsPopover.tsx", {
    "../../design-system/components": components, "../../design-system/modalFocus": load("src/design-system/modalFocus.ts"),
    "../../keyboardShortcuts": load("src/keyboardShortcuts.ts", { "./clientActions": load("src/clientActions.ts") }),
    "../IconGlyphs": { Glyph: () => null }, "./task-actions.css": {},
  });
  const { TaskActionsController } = load("src/components/task/TaskActionsController.tsx", {
    "./TaskActionsPopover": popover,
    "../../keyboardShortcuts": { useKeyboardShortcuts: () => ({ toggle_pin_task: ['Mod+Alt+KeyP'] }) },
    "../../shell/taskActionsMenu": events, "../../shell/clientWorkspaces": workspaces, "../../design-system/components": components,
    "../../runtimeFormat": { copyText: async (id) => { effects.push(["copy", id]); return true; } },
    "../../toasts": { appToast: { danger: (message) => errors.push(message), info() {}, success() {} } },
  });
  const root = createRoot(w.document.getElementById("root"));
  let props = { runtimeHome: "/home/a", accessProfile: "live", homeRunId: "home", activeChatRunId: "task", chatOrderIds: ["task"], pinnedChatIds: [],
    chatRuns: [{ runId: "task", title: "Task", chatInterfaceId: "private", eeId: "ee", osRootEeId: "root", createdAtMs: 1, writable: true, archived: false, turnOpen: false }],
    projectWorkspace: { projects: [{ id: "p", name: "Parent", parentId: "", chatRunIds: [] }, { id: "c", name: "Child", parentId: "p", chatRunIds: ["task"] }], rootEeId: "root", homeInterfaceId: "home-interface", pending: false,
      deleteTask: async (...args) => effects.push(["delete", ...args]), setTaskArchived: async (...args) => effects.push(["archive", ...args]) },
    onChatOrderIdsChange: (...args) => effects.push(["order", ...args]), onOpenChatInNewWindow: (...args) => effects.push(["open", ...args]), onPinnedChatIdsChange: (...args) => effects.push(["pin", ...args]), onSelectChat: (...args) => effects.push(["select", ...args]),
  };
  const render = async (changes = {}) => { props = { ...props, ...changes }; await React.act(async () => root.render(React.createElement(TaskActionsController, props))); };
  const open = async () => { await React.act(async () => events.dispatchOpenClientTaskActionsMenu("task", { right: 100, bottom: 100 }, { x: 50, y: 60 })); return pending.at(-1); };
  const finish = async (p, id) => React.act(async () => { p.resolve({ requestId: p.value.requestId, id }); await tick(); });
  const ids = (items) => items.flatMap((item) => item.type === "separator" ? [] : [item.id, ...ids(item.submenu ?? [])]);
  try {
    w.octopusTransport = transport; await render();
    const p = await open(); assert.deepEqual(p.value.position, { x: 50, y: 60 });
    assert.deepEqual(ids(p.value.items), ["rename", "pin", "projected-title", "copy-menu", "copy-title", "copy", "open", "archive", "delete"]);
    assert.equal(w.document.querySelector('[role="menu"]'), null);
    await finish(p, "copy"); assert.deepEqual(effects.pop(), ["copy", "task"]);
    const refreshed = await open(); await render({ onOpenChatInNewWindow: (id) => effects.push(["current-open", id]) });
    await finish(refreshed, "open"); assert.deepEqual(effects.pop(), ["current-open", "task"]);
    const malformed = await open(); await React.act(async () => { malformed.resolve({ requestId: "another-request", id: "archive" }); await tick(); });
    assert.match(errors.pop(), /Invalid native/); assert.deepEqual(effects, []);
    const stale = await open(); await render({ runtimeHome: "/home/b" }); await render({ runtimeHome: "/home/a" }); await finish(stale, "archive"); assert.deepEqual(effects, []);
    const profile = await open(); await render({ accessProfile: "lab" }); await finish(profile, "delete"); assert.deepEqual(effects, []); await render({ accessProfile: "live" });
    const removed = await open(), originalRuns = props.chatRuns; await render({ chatRuns: [] }); await render({ chatRuns: originalRuns }); await finish(removed, "copy"); assert.deepEqual(effects, []);
    const writable = await open(); await render({ chatRuns: [{ ...originalRuns[0], writable: false }] });
    await finish(writable, "archive"); assert.deepEqual(effects, []); const readOnly = await open();
    assert.ok(ids(readOnly.value.items).includes("readonly")); assert.ok(!ids(readOnly.value.items).includes("archive")); await finish(readOnly, null); await render({ chatRuns: originalRuns });
    const busy = await open(); await render({ projectWorkspace: { ...props.projectWorkspace, pending: true } }); await finish(busy, "delete"); assert.equal(w.document.querySelector('[role="dialog"]'), null);
    await render({ projectWorkspace: { ...props.projectWorkspace, pending: false } });
    const old = await open(), newer = await open(); await finish(old, "archive"); assert.deepEqual(effects, []); await finish(newer, "pin"); assert.deepEqual(effects.pop(), ["pin", ["task"]]);
    const failure = await open(); await React.act(async () => { failure.reject(new Error("native failed")); await tick(); }); assert.equal(errors.pop(), "native failed"); assert.equal(w.document.querySelector('[role="menu"]'), null);
    w.octopusTransport = {}; await open(); assert.match(errors.pop(), /native task menu is unavailable/); assert.equal(w.document.querySelector('[role="menu"]'), null); w.octopusTransport = transport;
    const rename = await open(); await finish(rename, "rename"); assert.ok(w.document.querySelector('[role="dialog"]'));
    await render({ projectWorkspace: { ...props.projectWorkspace, pending: true } }); const save = [...w.document.querySelectorAll('button')].find((b) => b.textContent === "Save"); assert.equal(save.disabled, true);
    await React.act(async () => w.document.querySelector('form').dispatchEvent(new w.Event('submit', { bubbles: true, cancelable: true }))); assert.deepEqual(effects, []);
    await render({ runtimeHome: "/home/b" }); assert.equal(w.document.querySelector('[role="dialog"]'), null);
    await render({ projectWorkspace: { ...props.projectWorkspace, pending: false } });
    const deletion = await open(); await finish(deletion, "delete"); assert.ok(w.document.querySelector('[role="dialog"]'));
    await render({ projectWorkspace: { ...props.projectWorkspace, pending: true } });
    const deleteButton = [...w.document.querySelectorAll('button')].find((b) => b.textContent === "Delete task"); assert.equal(deleteButton.disabled, true);
    await render({ projectWorkspace: { ...props.projectWorkspace, pending: false } });
    await React.act(async () => [...w.document.querySelectorAll('button')].find((b) => b.textContent === "Delete task").click());
    assert.deepEqual(effects.splice(0), [["delete", "task"], ["removeClientTaskPresentation", "live", "task"], ["pin", []], ["order", []], ["select", "home"]]);
    await render({ chatRuns: [{ ...originalRuns[0], archived: true, writable: false }] }); const archived = await open(); assert.deepEqual(ids(archived.value.items), ["restore", "copy-menu", "copy-title", "copy"]); await finish(archived, "restore"); assert.deepEqual(effects.pop(), ["archive", "task", false]);
    await render({ chatRuns: originalRuns }); const archive = await open(); await finish(archive, "archive"); assert.deepEqual(effects.pop(), ["archive", "task", true]);
    await render({ chatRuns: [{ ...originalRuns[0], turnOpen: true }] }); const working = await open(); assert.equal(working.value.items.find((item) => item.id === "archive").enabled, false); await finish(working, null);
    await render({ chatRuns: originalRuns });
    const trigger = w.document.createElement('button'); trigger.textContent = 'Task actions'; w.document.body.append(trigger);
    const anchored = async () => React.act(async () => events.dispatchOpenClientTaskActionsMenu("task", trigger));
    const button = (id) => w.document.querySelector(`[data-task-action="${id}"]`);
    const click = async (id) => React.act(async () => button(id).click());
    const before = pending.length; await anchored();
    assert.equal(pending.length, before); assert.equal(trigger.getAttribute('aria-expanded'), 'true');
    assert.ok(w.document.querySelector('[role="menu"]')); assert.equal(button('copy-title'), null);
    assert.equal(button('share'), null); assert.equal(button('fork'), null);
    await click('copy-menu'); await click('copy-title'); assert.deepEqual(effects.pop(), ['copy', 'Alias']);
    await anchored(); await click('copy-menu'); await click('copy'); assert.deepEqual(effects.pop(), ['copy', 'task']);
    assert.equal(trigger.getAttribute('aria-expanded'), null);
    await anchored(); await click('copy-menu');
    await React.act(async () => button('copy-title').dispatchEvent(new w.KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true })));
    assert.equal(button('copy-title'), null); assert.equal(w.document.activeElement, button('copy-menu'));
    await render({ chatRuns: [{ ...originalRuns[0], turnOpen: true }] }); assert.equal(w.document.querySelector('[role="menu"]'), null);
    await anchored(); assert.equal(button('archive').disabled, true);
    await render({ runtimeHome: '/home/c' }); assert.equal(w.document.querySelector('[role="menu"]'), null);
    await render({ chatRuns: originalRuns }); await anchored(); await anchored(); assert.equal(w.document.querySelector('[role="menu"]'), null);
    for (const platform of ['MacIntel', 'Linux x86_64']) {
      Object.defineProperty(w.navigator, "platform", { configurable: true, value: platform });
      await anchored();
      await React.act(async () => button('pin').dispatchEvent(new w.KeyboardEvent('keydown', {
        key: 'p', code: 'KeyP', ctrlKey: platform === 'MacIntel', metaKey: platform !== 'MacIntel', altKey: true, bubbles: true, cancelable: true,
      })));
      assert.deepEqual(effects, [], `The other platform's modifier must not pin on ${platform}`);
      assert.ok(w.document.querySelector('[role="menu"]'));
      await React.act(async () => button('pin').dispatchEvent(new w.KeyboardEvent('keydown', {
        key: 'p', code: 'KeyP', ctrlKey: platform !== 'MacIntel', metaKey: platform === 'MacIntel', altKey: true, bubbles: true, cancelable: true,
      })));
      assert.deepEqual(effects.pop(), ['pin', ['task']]);
      assert.equal(w.document.querySelector('[role="menu"]'), null);
    }
    delete w.octopusTransport; await render(); await open(); assert.ok(w.document.querySelector('[role="menu"]'));
    await click('copy-menu'); await click('copy'); assert.deepEqual(effects.pop(), ['copy', 'task']);
    w.octopusTransport = transport; const unmount = await open(); await React.act(async () => root.unmount()); await finish(unmount, "archive"); assert.deepEqual(effects, []); assert.ok(cancellations.length >= 5);
  } finally {
    dom.window.close();
    if (previousNavigator) Object.defineProperty(globalThis, "navigator", previousNavigator);
    else delete globalThis.navigator;
  }
});
