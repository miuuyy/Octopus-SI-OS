const assert = require("node:assert/strict");
const { EventEmitter } = require("node:events");
const { readFileSync } = require("node:fs");
const path = require("node:path");
const { test } = require("node:test");
const ts = require("typescript");
const { createClientWindowCloseHost } = require("../electron/clientWindowClose.cjs");
const main = ts.createSourceFile("main.cjs", readFileSync(path.join(__dirname, "../electron/main.cjs"), "utf8"), ts.ScriptTarget.Latest, true, ts.ScriptKind.JS);
const tick = () => new Promise((resolve) => setImmediate(resolve));
const event = () => ({ defaultPrevented: false, preventDefault() { this.defaultPrevented = true; } });

class Window extends EventEmitter {
  constructor(id) {
    super(); this.id = id; this.dead = false; this.veto = false;
    this.renderer = new EventEmitter(); this.sent = [];
    this.webContents = Object.assign(new EventEmitter(), { id, mainFrame: {}, isDestroyed: () => this.dead, send: (channel, payload) => { this.sent.push({ channel, payload }); this.renderer.emit(channel, {}, payload); } });
  }
  isDestroyed() { return this.dead; }
  close() {
    const attempt = event(); this.emit("close", attempt);
    if (!attempt.defaultPrevented) {
      if (this.veto) this.webContents.emit("will-prevent-unload", event());
      else this.destroy();
    }
    return attempt;
  }
  destroy() { this.dead = true; this.webContents.emit("destroyed"); this.emit("closed"); }
  requests() { return this.sent.filter((e) => e.channel.endsWith("-requested")).map((e) => e.payload); }
  cancellations() { return this.sent.filter((e) => e.channel.endsWith("-cancelled")).map((e) => e.payload); }
}

function fixture() {
  const handlers = new Map(), entries = new Map(), effects = [], revoked = new Set();
  const subscriptions = new Map(), status = (token) => ({ configured: true, authenticated: Boolean(token) && !revoked.has(token), profile: token && !revoked.has(token) ? "live" : null });
  const controller = { requireSession: (token) => { if (!token || revoked.has(token)) throw new Error("unauthorized"); }, status, lock: (token) => { effects.push(`access.lock:${token}`); revoked.add(token); return status(""); } };
  const aux = { quick: new Window(10), ownedEnvironment: new Window(11), foreignEnvironment: new Window(12), ownedMonitor: new Window(13), foreignMonitor: new Window(14) };
  const app = Object.assign(new EventEmitter(), { quit: () => { effects.push("app.quit"); app.emit("before-quit", event()); }, relaunch: () => effects.push("app.relaunch") });
  const dependencies = {
    createClientWindowCloseHost, app, console: { error() {} }, ipcMain: { handle: (name, handler) => handlers.set(name, handler) },
    clientWindows: { entryForWebContents: (id) => entries.get(id), entries: () => [...entries.values()].filter((entry) => !entry.win.dead), clearAccessToken: (token) => { for (const entry of entries.values()) if (entry.accessToken === token) entry.accessToken = ""; } },
    getClientAccessController: () => controller,
    clientSupportHost: { closeOwner: (token) => effects.push(`support.close:${token}`) },
    runtimeEventSubscriptions: subscriptions, sendClientWindowEvent: (win, channel, payload) => win.webContents.send(channel, payload),
    globalDictationAccessToken: "foreign", clearGlobalDictationShortcut: () => effects.push("dictation.clear"),
    cdpViewerManager: { closeOwner: (token) => effects.push(`cdp.close:${token}`) }, processViewerManager: { closeOwner: (token) => effects.push(`process.close:${token}`) },
    desktopSurfaceHost: { pauseOwner: async (senderId) => effects.push(`desktop.pause:${senderId}`) },
    nativeBrowserSurface: null,
    nativeDesktopPermissions: { closeOwner: (id) => effects.push(`desktop.permissions.close:${id}`) },
    nativeDesktopSettings: { closeOwner: (id) => effects.push(`desktop.settings.close:${id}`) },
    nativeBrowserPresentations: null, nativeBrowserDownloadHistory: null, nativeDesktopPresentations: null,
    scheduleNativeDesktopPresentations: () => effects.push("desktop.refresh"),
    clientArtifactStreamStore: { abortOwner: async (token) => effects.push(`artifact.abort:${token}`) }, clientAttachmentStagingStore: { abortOwner: async (token) => effects.push(`attachment.abort:${token}`) },
    quickTaskAccessToken: "shared", quickTaskWindow: aux.quick,
    workerEnvironmentWindows: new Map([[1, { accessToken: "shared", win: aux.ownedEnvironment }], [2, { accessToken: "foreign", win: aux.foreignEnvironment }]]),
    taskMonitorWindows: new Map([[1, { accessToken: "shared", win: aux.ownedMonitor }], [2, { accessToken: "foreign", win: aux.foreignMonitor }]]),
    runtimeInitializationPromise: null, appendClientLog: async () => {}, closeClientResources: () => effects.push("resources.close"),
    terminalHost: { closeOwner: (token) => effects.push(`terminal.close:${token}`), interruptLaunch: () => { effects.push("interrupt"); return 1; }, terminate: async () => { effects.push("terminate"); }, closeAll: () => effects.push("terminals.close") },
    runtimeHost: { waitUntilStopped: async () => true, close: () => effects.push("runtime.close") },
  };
  const statements = main.statements.filter((node) => {
    if (ts.isVariableStatement(node)) return node.declarationList.declarations.some((d) => d.name.getText(main) === "clientWindowClose");
    if (ts.isFunctionDeclaration(node)) return ["startClientQuitCleanup", "currentClientWindowCloseSender", "requireClientLockWindow", "nativeDesktopAccessEntries"].includes(node.name?.text);
    if (!ts.isExpressionStatement(node) || !ts.isCallExpression(node.expression)) return false;
    const call = node.expression, owner = call.expression.getText(main), channel = call.arguments[0]?.text;
    return owner === "app.on" && channel === "before-quit" || owner === "ipcMain.handle" && (channel?.startsWith("octopus:client-window-close-") || channel === "octopus:access-lock" || channel === "octopus:client-relaunch");
  });
  assert.equal(statements.length, 11);
  const integration = Function(...Object.keys(dependencies), `let quitCleanupStarted = false, quitReady = false, relaunchRequested = false;\n${statements.map((n) => n.getText(main)).join("\n")}\nreturn { host: clientWindowClose, state: () => ({quitCleanupStarted,quitReady}) };`)(...Object.values(dependencies));
  function add(id, guarded = true, token = `token-${id}`) {
    const win = new Window(id);
    entries.set(id, { win, webContentsId: id, accessToken: token, primary: id === 1 }); integration.host.trackWindow(win, `window-${id}`); subscriptions.set(id, { token });
    const ipc = win.renderer;
    ipc.sendSync = () => token;
    win.invocations = [];
    ipc.invoke = async (channel, passedToken, ...args) => {
      win.invocations.push({ channel, token: passedToken });
      if (channel === "octopus:runtime-events-unsubscribe") { subscriptions.delete(id); return; }
      if (channel === "octopus:access-status") return status(passedToken);
      return handlers.get(channel)({ sender: win.webContents, senderFrame: win.webContents.mainFrame }, passedToken, ...args);
    };
    let bridge;
    Function("require", "process", readFileSync(path.join(__dirname, "../electron/preload.cjs"), "utf8"))(() => ({ ipcRenderer: ipc, contextBridge: { exposeInMainWorld: (_name, value) => { bridge = value; } } }), { argv: ["--octopus-access-claim=test"] });
    win.bridge = bridge; win.token = token;
    if (guarded) win.guard = integration.host.register(win.webContents, token);
    bridge.onClientAccessLocked((value) => { win.lockedStatus = value; });
    return win;
  }
  const quit = () => { const attempt = event(); app.emit("before-quit", attempt); assert.equal(attempt.defaultPrevented, true); };
  return { ...integration, add, effects, handlers, revoked, subscriptions, entries, aux, quit };
}
const reply = (win, allowClose, payload = win.requests().at(-1)) => win.bridge.resolveClientWindowClose({ ...payload, allowClose });

test("a pending quit keeps account reads alive until approved resource cleanup", () => {
  const app = Object.assign(new EventEmitter(), { getVersion: () => "test" });
  app.on("before-quit", (attempt) => attempt.preventDefault());
  let disposed = 0;
  const account = { dispose: () => { disposed += 1; } };
  const declarations = main.statements.filter((node) => ts.isFunctionDeclaration(node)
    && ["getChatgptSignIn", "closeClientResources"].includes(node.name?.text));
  assert.equal(declarations.length, 2);
  const lifecycle = Function("app", "createChatgptSignIn", `
    let chatgptSignIn = null;
    const resolveOperatorCodexHome = () => "/fixture/codex", resolveOctopusHome = () => "/fixture/home";
    const clearGlobalDictationShortcut = () => {};
    const cdpViewerManager = null, processViewerManager = null, clientSurfaceDocumentStore = null,
      clientExtensionStore = null, clientArtifactStreamStore = null, clientAttachmentStagingStore = null,
      applicationConnectionHost = null;
    const projectionStores = new Map();
    ${declarations.map((node) => node.getText(main)).join("\n")}
    return { getChatgptSignIn, closeClientResources };
  `)(app, () => account);
  assert.equal(lifecycle.getChatgptSignIn(), account);
  const pending = event();
  app.emit("before-quit", pending);
  assert.equal(pending.defaultPrevented, true);
  assert.equal(disposed, 0);
  assert.equal(lifecycle.getChatgptSignIn(), account);
  lifecycle.closeClientResources();
  assert.equal(disposed, 1);
});

test("actual preload and main handlers bind single-use close to exact sender, token and generation", async () => {
  const f = fixture(), a = f.add(1, false), b = f.add(2);
  a.guard = await a.bridge.registerClientWindowCloseGuard(); assert.equal(a.guard.generation, 1);
  const seen = []; const unsubscribe = a.bridge.onClientWindowCloseRequested((request) => seen.push(request));
  assert.equal(a.close().defaultPrevented, true); a.close(); assert.equal(seen.length, 1);
  const request = seen[0], value = { ...request, allowClose: true }, handler = f.handlers.get("octopus:client-window-close-resolve");
  assert.deepEqual(await b.bridge.resolveClientWindowClose(value), { accepted: false });
  for (const [sender, senderFrame, token, input] of [[a.webContents, {}, a.token, value], [{ ...a.webContents }, a.webContents.mainFrame, a.token, value], [a.webContents, a.webContents.mainFrame, "wrong", value], [a.webContents, a.webContents.mainFrame, a.token, { ...value, generation: 99 }], [a.webContents, a.webContents.mainFrame, a.token, { ...value, requestId: "wrong" }]]) assert.deepEqual(handler({ sender, senderFrame }, token, input), { accepted: false });
  assert.deepEqual(await reply(a, false), { accepted: true }); assert.equal(a.dead, false); assert.equal(a.cancellations().at(-1).reason, "cancelled");
  a.close(); assert.notEqual(a.requests().at(-1).requestId, request.requestId);
  assert.deepEqual(await reply(a, true), { accepted: true }); assert.deepEqual(await reply(a, true), { accepted: false });
  await tick(); assert.equal(a.dead, true); assert.deepEqual(f.effects, []); unsubscribe();
});

test("quit veto invalidates approved peers before cleanup; unanimous native closes alone permit teardown", async () => {
  const f = fixture(), a = f.add(1), b = f.add(2), boot = f.add(3, false);
  f.quit(); f.quit(); assert.equal(a.requests().length, 1); assert.equal(boot.requests().length, 0);
  const old = a.requests()[0]; await reply(a, true); await tick();
  assert.equal(a.dead, false); assert.equal(f.state().quitCleanupStarted, false); assert.deepEqual(f.effects, []);
  await reply(b, false); assert.equal(a.cancellations().at(-1).reason, "cancelled"); assert.equal(b.cancellations().length, 1);
  assert.deepEqual(await reply(a, true, old), { accepted: false }); assert.deepEqual(f.effects, []);
  f.quit(); await reply(a, true); await reply(b, true); a.veto = true; await tick();
  assert.equal(a.dead, false); assert.equal(b.dead, false); assert.equal(f.state().quitCleanupStarted, false);
  a.veto = false; f.quit(); await reply(a, true); await reply(b, true); await tick(); await tick();
  assert.ok([a, b, boot].every((win) => win.dead)); assert.equal(f.state().quitReady, true);
  assert.equal(f.effects.filter((e) => e === "interrupt").length, 1); assert.ok(f.effects.includes("runtime.close")); assert.ok(f.effects.includes("app.quit"));
  assert.throws(() => f.host.assertCanCreateWindow(), /closing/);
});

test("a relaunch leaves the launched runtime running; a vetoed relaunch restores an ordinary quit", async () => {
  const vetoed = fixture(), first = vetoed.add(1);
  assert.deepEqual(await first.bridge.relaunchClient(), { relaunching: true });
  await new Promise((resolve) => setImmediate(resolve));
  await reply(first, false); await tick();
  vetoed.quit(); await reply(first, true); await tick(); await tick();
  assert.equal(vetoed.effects.includes("app.relaunch"), false); assert.equal(vetoed.effects.filter((e) => e === "interrupt").length, 1);
  const f = fixture(), a = f.add(1);
  assert.deepEqual(await a.bridge.relaunchClient(), { relaunching: true });
  await new Promise((resolve) => setImmediate(resolve));
  await reply(a, true); await tick(); await tick();
  assert.equal(a.dead, true); assert.equal(f.state().quitReady, true);
  assert.ok(f.effects.includes("app.relaunch")); assert.equal(f.effects.includes("interrupt"), false);
  assert.ok(f.effects.includes("runtime.close"));
});

test("navigation, unregister, new windows and destruction cancel stale decisions without phantom approval", async () => {
  const f = fixture(), a = f.add(1), b = f.add(2);
  f.quit(); await reply(a, true); const old = a.guard;
  b.webContents.emit("did-start-navigation", event(), "file:///next", false, true);
  assert.equal(a.cancellations().at(-1).reason, "invalidated"); assert.deepEqual(f.effects, []);
  assert.equal(b.close().defaultPrevented, true); await assert.rejects(b.bridge.registerClientWindowCloseGuard(), /navigation/);
  b.webContents.emit("did-navigate"); b.guard = await b.bridge.registerClientWindowCloseGuard();
  f.quit(); const added = f.add(3); assert.equal(a.cancellations().at(-1).reason, "invalidated"); await tick(); assert.deepEqual(f.effects, []);
  a.close(); await a.bridge.unregisterClientWindowCloseGuard(old); assert.equal(a.close().defaultPrevented, true);
  a.guard = await a.bridge.registerClientWindowCloseGuard(); await a.bridge.unregisterClientWindowCloseGuard(old);
  a.close(); assert.equal(a.requests().at(-1).generation, a.guard.generation);
  f.quit(); await reply(a, true); added.destroy(); assert.equal(a.cancellations().at(-1).reason, "invalidated"); assert.deepEqual(f.effects, []);
  f.quit(); f.revoked.add(b.token); assert.deepEqual(await reply(b, true), { accepted: true }); assert.deepEqual(f.effects, []);
  await assert.rejects(b.bridge.registerClientWindowCloseGuard(), /unauthorized/); await reply(a, false);
  b.close(); assert.deepEqual(await reply(b, true), { accepted: true }); await tick(); assert.equal(b.dead, true);
  const initial = fixture(), unready = initial.add(4, false);
  unready.webContents.emit("did-start-navigation", event(), "file:///initial", false, true);
  initial.quit(); await tick(); await tick(); assert.equal(unready.dead, true); assert.equal(initial.state().quitReady, true);
  const crashed = fixture(), gone = crashed.add(5); crashed.quit(); gone.webContents.emit("render-process-gone");
  assert.deepEqual(crashed.effects, []); assert.equal(gone.cancellations().at(-1).reason, "invalidated");
  assert.equal(gone.close().defaultPrevented, false); assert.equal(gone.dead, true);
});

test("actual logout IPC settles token peers before revoke, preserves cancellation subscriptions, and leaves other owners alone", async () => {
  const f = fixture(), a = f.add(1, true, "shared"), b = f.add(2, true, "shared"), foreign = f.add(3, true, "foreign");
  let pending = a.bridge.lockAccess(); assert.equal(a.requests().at(-1).reason, "logout"); assert.equal(b.requests().at(-1).reason, "logout"); assert.equal(foreign.requests().length, 0);
  await reply(a, true); await reply(b, false); assert.equal((await pending).authenticated, true);
  assert.deepEqual(f.effects, []); assert.equal(f.subscriptions.size, 3); assert.equal((await a.bridge.accessStatus()).authenticated, true);
  pending = a.bridge.lockAccess(); const duplicate = b.bridge.lockAccess(); await reply(a, true);
  const otherBoot = f.add(4, false, "independent"); otherBoot.close(); assert.equal(otherBoot.dead, true);
  assert.equal(f.revoked.has("shared"), false); await reply(b, true);
  assert.equal((await pending).authenticated, false); assert.equal((await duplicate).authenticated, false);
  assert.equal(f.effects.filter((effect) => effect === "access.lock:shared").length, 1);
  assert.deepEqual(f.effects.filter((effect) => effect.startsWith("desktop.pause:")), ["desktop.pause:1", "desktop.pause:2", "desktop.pause:11", "desktop.pause:13", "desktop.pause:10"]);
  for (const kind of ["permissions", "settings"]) assert.deepEqual(f.effects.filter((effect) => effect.startsWith(`desktop.${kind}.close:`)), [1, 2, 11, 13, 10].map((id) => `desktop.${kind}.close:${id}`));
  assert.equal(a.lockedStatus.authenticated, false); assert.equal(b.lockedStatus.authenticated, false); assert.equal(foreign.lockedStatus, undefined);
  assert.equal(b.dead, true); assert.equal(a.dead, false); assert.equal(foreign.dead, false); assert.equal(f.subscriptions.has(3), true);
  assert.equal((await a.bridge.accessStatus()).authenticated, false); assert.equal(a.invocations.at(-1).token, ""); assert.equal((await foreign.bridge.accessStatus()).authenticated, true);
  assert.ok(f.aux.quick.dead && f.aux.ownedEnvironment.dead && f.aux.ownedMonitor.dead); assert.ok(!f.aux.foreignEnvironment.dead && !f.aux.foreignMonitor.dead);
  assert.deepEqual(f.effects.filter((effect) => effect.startsWith("support.close:")), ["support.close:shared"]);
  assert.ok(!f.effects.includes("dictation.clear")); assert.equal(f.state().quitCleanupStarted, false);
  await a.bridge.unregisterClientWindowCloseGuard(a.guard); assert.equal(a.close().defaultPrevented, false); assert.equal(a.dead, true);
  const drift = fixture(), first = drift.add(1, true, "shared"), second = drift.add(2, true, "shared"), changed = drift.add(3, true, "foreign");
  const cancelled = first.bridge.lockAccess(); await reply(first, true); drift.entries.get(changed.id).accessToken = "shared"; await reply(second, true);
  assert.equal((await cancelled).authenticated, true); assert.deepEqual(drift.effects, []); assert.equal(first.cancellations().at(-1).reason, "invalidated");
});

test("every desktop IPC sender detaches on document loss and closes once on destruction", async () => {
  const declaration = main.statements.find((node) => ts.isFunctionDeclaration(node) && node.name?.text === "getDesktopSurfaceHost");
  const effects = [], host = { closeOwner: async (id) => effects.push(`close:${id}`), pauseOwner: async (id) => effects.push(`pause:${id}`) };
  const getHost = Function("desktopSurfaceHost", "desktopSurfaceSenders", "nativeDesktopPermissions", "nativeDesktopPresentations", "nativeDesktopSettings", `${declaration.getText(main)}; return getDesktopSurfaceHost;`)(
    host, new WeakSet(),
    { closeOwner: (id) => effects.push(`permissions:${id}`) },
    { closeViewsOwner: (id) => effects.push(`views:${id}`) },
    { closeOwner: (id) => effects.push(`settings:${id}`) },
  );
  const full = new Window(1).webContents, detached = new Window(11).webContents;
  for (const sender of [full, detached]) { assert.equal(getHost(sender), host); assert.equal(getHost(sender), host); assert.equal(sender.listenerCount("destroyed"), 1); }
  detached.emit("did-start-navigation", {}, "file:///next", false, false);
  detached.emit("did-start-navigation", {}, "file:///next#hash", true, true); assert.deepEqual(effects, []);
  detached.emit("did-start-navigation", {}, "file:///next", false, true); full.emit("render-process-gone");
  detached.emit("destroyed"); detached.emit("destroyed"); full.emit("destroyed"); await tick();
  assert.deepEqual(effects, [[11, "pause"], [1, "pause"], [11, "close"], [1, "close"]]
    .flatMap(([id, action]) => [`views:${id}`, `permissions:${id}`, `settings:${id}`, `${action}:${id}`]));
});
