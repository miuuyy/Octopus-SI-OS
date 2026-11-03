const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const vm = require("node:vm");
const { createRequire } = require("node:module");
const { EventEmitter } = require("node:events");
const { test, after } = require("node:test");

const appRoot = path.resolve(__dirname, "..");
const mainFile = path.join(appRoot, "electron/main.cjs");
const realRequire = createRequire(mainFile);
const roots = [];
after(() => { for (const root of roots) fs.rmSync(root, { recursive: true, force: true }); });
const settle = (ms = 0) => new Promise((resolve) => setTimeout(resolve, ms));
async function until(condition, message) {
  for (const deadline = Date.now() + 5_000; Date.now() < deadline; await settle(5)) if (condition()) return;
  assert.fail(message);
}
const PASSWORD = "fixture-administrator";
const WINDOW_SETTERS = ["setBackgroundColor", "setVibrancy", "setWindowButtonVisibility", "setWindowButtonPosition", "setAlwaysOnTop",
  "setIgnoreMouseEvents", "setVisibleOnAllWorkspaces", "setContentSize", "showInactive", "hide", "maximize", "minimize", "center"];

function fakeElectron(world) {
  const windows = [];
  let focused = null;
  let nextWebContentsId = 100;
  class WebContents extends EventEmitter {
    constructor() { super(); this.id = ++nextWebContentsId; this.sent = []; this.destroyed = false; this.mainFrame = { url: "file:///index.html" }; }
    send(channel, ...args) { this.sent.push([channel, ...args]); }
    isDestroyed() { return this.destroyed; }
    setZoomFactor(value) { this.zoom = value; }
    getZoomFactor() { return this.zoom ?? 1; }
    setWindowOpenHandler(handler) { this.windowOpenHandler = handler; }
    isLoadingMainFrame() { return false; }
    getURL() { return "file:///index.html"; }
    sentOn(channel) { return this.sent.filter(([candidate]) => candidate === channel).map(([, ...args]) => args); }
  }
  class BrowserWindow extends EventEmitter {
    constructor(options) {
      super();
      this.options = options;
      this.webContents = new WebContents();
      this.calls = [];
      this.loads = [];
      this.destroyed = false;
      this.visible = false;
      this.minimized = false;
      this.bounds = { x: options.x ?? 0, y: options.y ?? 0, width: options.width ?? 800, height: options.height ?? 600 };
      windows.push(this);
      world.events.push(["window"]);
    }
    loadFile(...args) { this.loads.push(["file", ...args]); return Promise.resolve(); }
    loadURL(...args) { this.loads.push(["url", ...args]); return Promise.resolve(); }
    show() { this.visible = true; this.calls.push(["show"]); }
    focus() { focused = this; this.calls.push(["focus"]); this.emit("focus"); }
    restore() { this.minimized = false; this.calls.push(["restore"]); }
    isFocused() { return focused === this; }
    isVisible() { return this.visible; }
    isMinimized() { return this.minimized; }
    isMaximized() { return false; }
    isFullScreen() { return false; }
    setFullScreen(value) { this.calls.push(["setFullScreen", value]); }
    isDestroyed() { return this.destroyed; }
    getBounds() { return { ...this.bounds }; }
    getNormalBounds() { return { ...this.bounds }; }
    getContentBounds() { return { ...this.bounds }; }
    getContentSize() { return [this.bounds.width, this.bounds.height]; }
    setPosition(x, y) { Object.assign(this.bounds, { x, y }); }
    getNativeWindowHandle() { return Buffer.alloc(8); }
    close() {
      if (this.destroyed) return;
      const event = { defaultPrevented: false, preventDefault() { this.defaultPrevented = true; } };
      this.emit("close", event);
      if (!event.defaultPrevented) this.destroy();
    }
    destroy() {
      if (this.destroyed) return;
      this.destroyed = true;
      this.webContents.destroyed = true;
      if (focused === this) focused = null;
      this.emit("closed");
      this.webContents.emit("destroyed");
    }
    static getAllWindows() { return windows.filter((win) => !win.destroyed); }
    static getFocusedWindow() { return focused && !focused.destroyed ? focused : null; }
    static fromWebContents(webContents) { return windows.find((win) => win.webContents === webContents) ?? null; }
  }
  for (const name of WINDOW_SETTERS) BrowserWindow.prototype[name] = function record(...args) { this.calls.push([name, ...args]); };
  const handlers = new Map();
  const listeners = new Map();
  const shortcuts = new Map();
  const app = new EventEmitter();
  const paths = { appData: path.join(world.root, "app-data") };
  Object.assign(app, {
    commandLine: { appendSwitch: () => {} },
    dock: { setIcon: () => {} },
    isPackaged: false,
    getAppPath: () => path.join(world.root, "app"),
    getName: () => "Octopus",
    getVersion: () => "0.0.0",
    getPath: (name) => paths[name] ?? path.join(world.root, name),
    setPath: (name, value) => { paths[name] = value; },
    setAppLogsPath: (value) => { paths.logs = value; },
    setName: () => {},
    setAboutPanelOptions: () => {},
    requestSingleInstanceLock: () => true,
    whenReady: () => world.ready,
    relaunch: () => world.events.push(["relaunch"]),
    quit() {
      world.events.push(["quit"]);
      const event = { defaultPrevented: false, preventDefault() { this.defaultPrevented = true; } };
      app.emit("before-quit", event);
      if (!event.defaultPrevented) world.events.push(["exit"]);
    },
  });
  const safeStorage = {
    isAsyncEncryptionAvailable: async () => { world.events.push(["safe-storage"]); return true; },
    encryptStringAsync: async (value) => { world.events.push(["safe-storage"]); return Buffer.from(`sealed:${world.sealKey ?? 1}:${value}`); },
    decryptStringAsync: async (value) => {
      world.events.push(["safe-storage"]);
      const sealed = /^sealed:(\d+):(.*)$/s.exec(value.toString());
      if (!sealed) throw new Error("not sealed here");
      return { result: sealed[2], shouldReEncrypt: Number(sealed[1]) !== (world.sealKey ?? 1) };
    },
  };
  return {
    windows,
    handlers,
    listeners,
    shortcuts,
    module: {
      app,
      BrowserWindow,
      clipboard: { writeText: (text) => { world.clipboard = text; } },
      dialog: { showOpenDialog: async () => ({ canceled: true, filePaths: [] }), showSaveDialog: async () => world.saveResult ?? ({ canceled: true }), showMessageBox: async () => ({ response: 0 }) },
      globalShortcut: {
        register: (accelerator, callback) => { shortcuts.set(accelerator, callback); return true; },
        unregister: (accelerator) => { shortcuts.delete(accelerator); },
      },
      ipcMain: {
        handle: (channel, handler) => { handlers.set(channel, handler); },
        on: (channel, handler) => { listeners.set(channel, handler); },
      },
      Menu: {
        buildFromTemplate: (template) => ({ template, popup: () => {} }),
        setApplicationMenu: (menu) => { world.menu = menu; },
      },
      nativeTheme: { themeSource: "system", shouldUseDarkColors: true },
      net: { fetch: async () => { throw new Error("offline fixture"); } },
      powerSaveBlocker: { start: () => 1, stop: () => {}, isStarted: () => false },
      safeStorage,
      screen: {
        getAllDisplays: () => [{ workArea: { x: 0, y: 0, width: 1440, height: 900 }, scaleFactor: world.scaleFactor }],
        getDisplayMatching: () => ({ workArea: { x: 0, y: 0, width: 1440, height: 900 }, scaleFactor: world.scaleFactor }),
        getDisplayNearestPoint: () => ({ workArea: { x: 0, y: 0, width: 1440, height: 900 }, scaleFactor: world.scaleFactor }),
        getCursorScreenPoint: () => ({ x: 0, y: 0 }),
      },
      session: { defaultSession: { setPermissionRequestHandler: () => {}, setPermissionCheckHandler: () => {} } },
      shell: { openExternal: async (url) => { world.events.push(["external", url]); }, openPath: async () => "", showItemInFolder: () => {} },
      systemPreferences: world.touchId === false ? {} : {
        promptTouchID: async (reason) => {
          world.events.push(["touch-id", reason]);
          if (world.touchIdRefused) throw new Error("Touch ID was cancelled.");
        },
      },
    },
  };
}

function fakeRuntimeHost(world) {
  return (options) => {
    const host = new EventEmitter();
    world.runtimeHosts.push(host);
    return Object.assign(host, {
      options,
      probe: async () => { world.events.push(["probe"]); return { ...world.runtimeState }; },
      snapshot: () => ({ ...world.runtimeState }),
      assertStorageWritable: async () => { if (world.storageError) throw world.storageError; },
      recordStorageFailure: (error) => world.events.push(["storage-failure", error]),
      observe: () => world.events.push(["observe"]),
      start: () => world.events.push(["runtime-start"]),
      waitUntilStopped: async () => { world.events.push(["wait-stopped"]); return true; },
      close: () => world.events.push(["runtime-close"]),
    });
  };
}

function fakeTerminalHost(world) {
  return () => {
    const host = new EventEmitter();
    world.terminalHosts.push(host);
    return Object.assign(host, {
      opened: [],
      openExisting: (owner, request) => { world.events.push(["terminal-open-existing", owner, request]); return world.existingLaunch ?? null; },
      open: (owner, request) => {
        world.events.push(["terminal-open", owner, request]);
        return { terminalId: request.terminalId, sessionId: "launch-session", running: true, transcript: "" };
      },
      write: (...args) => world.events.push(["terminal-write", ...args]),
      resize: (...args) => world.events.push(["terminal-resize", ...args]),
      close: () => true,
      closeOwner: (...args) => world.events.push(["terminal-close-owner", ...args]),
      closeSender: (...args) => world.events.push(["terminal-close-sender", ...args]),
      closeAll: () => world.events.push(["terminal-close-all"]),
      interruptLaunch: () => { world.events.push(["interrupt-launch"]); return world.interruptedLaunches ?? 1; },
      terminate: async () => world.events.push(["terminal-terminate"]),
    });
  };
}

function fakeProjectionStore(world) {
  return (profile) => ({
    read: async (request) => { world.events.push(["projection-read", profile, request?.method ?? request]); return {}; },
    invalidate: (...args) => world.events.push(["projection-invalidate", profile, ...args]),
    close: () => world.events.push(["projection-close", profile]),
  });
}

function overrides(world) {
  return {
    "./clientDictationStream.cjs": () => ({ createClientDictationStream: (_request, options) => ({
      ready: Promise.resolve(),
      append: (bytes) => world.events.push(["dictation-audio", bytes.length]),
      finish: async () => { options.onEvent({ type: "transcript", text: "Final words" }); return { text: "Final words" }; },
      close: () => world.events.push(["dictation-close"]),
    }) }),
    "./runtimeHost.cjs": (real) => ({ ...real, createRuntimeHost: fakeRuntimeHost(world) }),
    "./terminalHost.cjs": () => ({ createTerminalHost: fakeTerminalHost(world) }),
    "./clientDataSource.cjs": (real) => ({ ...real, createClientProjectionStore: fakeProjectionStore(world) }),
    "./clientWindows.cjs": (real) => ({ ...real, createClientWindowRegistry: (options) => real.createClientWindowRegistry({ ...options, readyTimeoutMs: 150, transferTimeoutMs: 150 }) }),
    "./clientAccess.cjs": (real) => ({
      ...real,
      createClientAccessController: (options) => {
        const controller = real.createClientAccessController(options);
        return { ...controller, requireSession: (token, profiles) => {
          if (world.expiredTokens.has(token)) throw new Error("Client access session is locked or expired");
          return controller.requireSession(token, profiles);
        } };
      },
    }),
    "./workspaceSetup.cjs": (real) => ({ ...real, recoverWorkspaceTransactions: async () => { world.events.push(["recover-workspace"]); } }),
    "./localPackages.cjs": (real) => ({
      ...real,
      recoverLocalPackageTransactions: async () => { world.events.push(["recover-packages"]); await world.recoveryGate; },
      registerLocalPackageMutationGuard: () => () => {},
      withLocalPackageInstallation: async (roots) => { throw new Error(`${roots.packageId} is not installed in this fixture`); },
    }),
    "./operatorChatClientContract.cjs": (real) => ({
      ...real,
      createOperatorChatClientLoader: (options) => {
        world.chatLoaderOptions.push(options);
        return { load: async () => { world.events.push(["chat-client-load"]); return world.chatClient; } };
      },
    }),
    "./contentAddressedArtifact.cjs": (real) => ({
      ...real,
      readContentAddressedArtifactPayload: async (input) => { world.events.push(["read-payload", input]); return { payload: input.label }; },
      readContentAddressedArtifactProjection: async (input) => { world.events.push(["read-projection", input]); return { projection: input.label }; },
    }),
    "./clientArtifacts.cjs": (real) => ({
      ...real,
      submitClientInterfaceArtifact: async (deps, request) => { await deps.authorizeSubmission(request); world.events.push(["artifact-submitted", request]); return { submitted: true }; },
    }),
    "./clientExtensions.cjs": (real) => ({
      ...real,
      createClientExtensionStore: () => ({
        withVerifiedLifecycleSnapshot: (operation) => operation({ installedExtensionIds: ["fixture.extension"] }),
        generation: () => 7,
        close: () => {},
      }),
    }),
    "./clientExtensionActions.cjs": (real) => ({
      ...real,
      executeClientExtensionAction: async (context, request) => { world.events.push(["extension-action", context, request]); return { actionId: request.actionId, extensionId: request.extensionId }; },
    }),
  };
}

function lazyModule(id) {
  const load = () => realRequire(id);
  return new Proxy({}, { get: (_, key) => load()[key], ownKeys: () => Reflect.ownKeys(load()), getOwnPropertyDescriptor: (_, key) => Object.getOwnPropertyDescriptor(load(), key) });
}

function writeFixture(file, value, mode = 0o600) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, typeof value === "string" ? value : JSON.stringify(value), { mode });
  fs.chmodSync(file, mode);
}

async function launchMain(options = {}) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "octopus-main-"));
  roots.push(root);
  const home = path.join(root, "home");
  const codexHome = path.join(root, "codex");
  const library = path.join(root, "library");
  for (const dir of [path.join(library, "packages"), path.join(library, "packs"), path.join(root, "repo", "registry", "interface")]) fs.mkdirSync(dir, { recursive: true });
  writeFixture(path.join(root, "repo", "registry", ".octopus-source.json"), {});
  let releaseReady;
  const world = {
    root, home, codexHome,
    userData: path.join(home, "client"),
    events: [],
    runtimeHosts: [],
    terminalHosts: [],
    chatLoaderOptions: [],
    expiredTokens: new Set(),
    recoveryGate: null,
    scaleFactor: 2,
    runtimeState: { phase: "unmanaged", ready: false, ownership: "none", pid: null, error: null, launchCwd: root, launchEnv: { OCTOPUS_HOME: home }, launchCommand: "octopus up" },
    chatClient: null,
    ready: options.deferReady ? new Promise((resolve) => { releaseReady = resolve; }) : Promise.resolve(),
    ...options.world,
  };
  for (const [relative, value] of Object.entries(options.files ?? {})) writeFixture(path.join(home, "client", relative), value);
  const electron = fakeElectron(world);
  const replaced = overrides(world);
  const env = {
    PATH: process.env.PATH,
    OCTOPUS_HOME: home,
    CODEX_HOME: codexHome,
    OCTOPUS_REPO_ROOT: path.join(root, "repo"),
    OCTOPUS_RUNTIME_BIN: path.join(root, "bin", "octopus"),
    OCTOPUS_SOURCE_REGISTRY: path.join(root, "repo", "registry"),
    OCTOPUS_LOCAL_PACKAGE_REGISTRY: path.join(root, "repo", "registry"),
    OCTOPUS_LOCAL_PACKAGE_LIBRARY: library,
    OCTOPUS_INSTALLATION_ROOT: path.join(root, "install"),
  };
  const fakeProcess = Object.create(process, {
    env: { value: env, enumerable: true },
    platform: { value: options.platform ?? "darwin", enumerable: true },
    on: { value: () => fakeProcess },
    once: { value: () => fakeProcess },
  });
  const localRequire = (id) => id === "electron" ? electron.module : replaced[id] ? replaced[id](lazyModule(id)) : realRequire(id);
  const compiled = vm.runInThisContext(`(function (exports, require, module, __filename, __dirname, process) {${fs.readFileSync(mainFile, "utf8")}\n})`, { filename: mainFile });
  const module = { exports: {} };
  compiled.call(module.exports, module.exports, localRequire, module, mainFile, path.dirname(mainFile), fakeProcess);
  await settle(5);
  const event = (win, extra = {}) => ({ sender: win.webContents, senderFrame: win.webContents.mainFrame, ...extra });
  const harness = {
    world,
    electron,
    releaseReady: async () => { releaseReady(); await settle(5); },
    windows: () => electron.module.BrowserWindow.getAllWindows(),
    invoke: (channel, win, ...args) => {
      const handler = electron.handlers.get(channel);
      assert.equal(typeof handler, "function", `${channel} is handled`);
      return Promise.resolve().then(() => handler(event(win), ...args));
    },
    sendSync: (channel, win, ...args) => {
      const handler = electron.listeners.get(channel);
      assert.equal(typeof handler, "function", `${channel} is listened`);
      const syncEvent = event(win, { returnValue: undefined });
      handler(syncEvent, ...args);
      return syncEvent.returnValue;
    },
    emitApp: (name, ...args) => electron.module.app.emit(name, ...args),
    log: () => {
      const file = path.join(home, "client", "logs", "octopus-client.jsonl");
      return fs.existsSync(file) ? fs.readFileSync(file, "utf8").trim().split("\n").filter(Boolean).map((line) => JSON.parse(line)) : [];
    },
    eventsOf: (name) => world.events.filter(([candidate]) => candidate === name),
  };
  harness.unlock = async (win) => {
    const configured = fs.existsSync(path.join(home, "client-access.json"));
    const result = await harness.invoke(configured ? "octopus:access-unlock" : "octopus:access-configure", win, PASSWORD);
    return result.token;
  };
  return harness;
}

const surface = { kind: "surface", runId: "run-1", surfaceId: "chat", workspaceKey: "octopus-os" };

test("streaming dictation binds audio, events and finalization to the unlocked owner window and closes on lock", async () => {
  const main = await launchMain();
  const [primary] = main.windows();
  const request = { attemptId: require("node:crypto").randomUUID(), sampleRateHz: 24_000 };
  assert.equal((await main.invoke("octopus:dictation-stream-start", primary, null, request)).ok, false);
  const token = await main.unlock(primary);
  assert.equal((await main.invoke("octopus:dictation-stream-start", primary, token, request)).ok, true);
  const other = new main.electron.module.BrowserWindow({});
  main.sendSync("octopus:dictation-stream-audio", other, token, request.attemptId, Buffer.from([0, 1]));
  assert.equal(main.eventsOf("dictation-audio").length, 0);
  assert.equal((await main.invoke("octopus:dictation-stream-finish", other, token, request.attemptId)).ok, false);
  main.sendSync("octopus:dictation-stream-audio", primary, token, request.attemptId, Buffer.from([0, 1]));
  assert.equal(main.eventsOf("dictation-audio").length, 1);
  const final = await main.invoke("octopus:dictation-stream-finish", primary, token, request.attemptId);
  assert.equal(final.value.text, "Final words");
  assert.equal(primary.webContents.sentOn("octopus:dictation-stream-event")[0][0].text, "Final words");
  assert.deepEqual(other.webContents.sentOn("octopus:dictation-stream-event"), []);
  assert.equal((await main.invoke("octopus:dictation-stream-start", primary, token, request)).ok, true);
  main.world.expiredTokens.add(token);
  main.sendSync("octopus:dictation-stream-audio", primary, token, request.attemptId, Buffer.from([0, 1]));
  assert.equal(main.eventsOf("dictation-audio").length, 1);
  assert.equal(main.eventsOf("dictation-close").length, 2);
});

test("dictation recording IPC requires access and returns saved audio or a safe failure", async () => {
  const main = await launchMain();
  const [primary] = main.windows();
  const denied = await main.invoke("octopus:dictation-recordings-read", primary, null);
  assert.equal(denied.ok, false);
  assert.equal(denied.error.code, "recording-storage");
  const token = await main.unlock(primary);
  const initial = await main.invoke("octopus:dictation-recordings-read", primary, token);
  assert.equal(initial.ok, true);
  assert.equal(initial.value.enabled, true);
  assert.deepEqual(initial.value.recordings, []);
  const bytes = Buffer.alloc(44 + 24_000 * 0.3 * 2);
  bytes.write("RIFF"); bytes.write("WAVE", 8);
  const saved = await main.invoke("octopus:dictation-recording-save", primary, token, {
    bytes, durationMs: 300, mediaType: "audio/wav", provider: "codex", sampleRateHz: 24_000,
  });
  assert.equal(saved.ok, true);
  const recording = await main.invoke("octopus:dictation-recording-read", primary, token, saved.value.id);
  assert.equal(recording.ok, true);
  assert.deepEqual(recording.value.bytes, bytes);
  const refused = await main.invoke("octopus:dictation-recording-read", primary, token, "../../outside");
  assert.equal(refused.ok, false);
  assert.equal(refused.error.code, "recording-storage");
  const deleted = await main.invoke("octopus:dictation-recording-delete", primary, token, saved.value.id);
  assert.deepEqual(deleted, { ok: true, value: null });
  assert.deepEqual((await main.invoke("octopus:dictation-recordings-read", primary, token)).value.recordings, []);
});

test("Help reports work independently of the main renderer and export the reviewed snapshot", async () => {
  const main = await launchMain();
  const [primary] = main.windows();
  const help = main.world.menu.template.find((item) => item.role === "help");
  const reportItem = help.submenu.find((item) => item.label === "Report a Problem…");
  primary.focus();
  reportItem.click();
  const locked = main.windows().at(-1);
  assert.match(locked.options.webPreferences.preload, /supportPreload\.cjs$/);
  assert.equal(locked.options.webPreferences.sandbox, true);
  const basic = await main.invoke("octopus:support-collect", locked, { includeDetails: false });
  assert.equal(basic.access, "locked");
  assert.equal(basic.client_logs, undefined);
  await assert.rejects(main.invoke("octopus:support-collect", locked, { includeDetails: true }), /Unlock/);
  await assert.rejects(main.invoke("octopus:support-collect", primary, { includeDetails: false }), /report window/);
  locked.destroy();
  await main.unlock(primary);
  primary.focus();
  reportItem.click();
  const support = main.windows().at(-1);
  primary.webContents.emit("render-process-gone", {}, { reason: "crashed", exitCode: 1 });
  await until(() => main.log().some((item) => item.event === "client.renderer.gone"), "renderer crash was recorded");
  const report = await main.invoke("octopus:support-collect", support, { includeDetails: true });
  assert.equal(report.access, "live");
  assert.ok(report.client_logs.events.some((item) => item.message?.includes("Renderer exited: crashed")));
  const request = { reportId: report.report_id, description: "Setup is waiting" };
  await main.invoke("octopus:support-copy", support, request);
  assert.match(main.world.clipboard, /Setup is waiting/);
  assert.deepEqual(await main.invoke("octopus:support-save", support, request), { saved: false });
  const destination = path.join(main.world.root, "report.txt");
  main.world.saveResult = { canceled: false, filePath: destination };
  assert.deepEqual(await main.invoke("octopus:support-save", support, request), { saved: true });
  assert.equal(fs.readFileSync(destination, "utf8"), main.world.clipboard);
  assert.equal(fs.statSync(destination).mode & 0o777, 0o600);
  await main.invoke("octopus:support-issue", support, request);
  assert.match(main.eventsOf("external").at(-1)[1], /^https:\/\/github.com\/miuuyy\/Octopus-SI-OS\/issues\/new\?/);
  await main.invoke("octopus:support-collect", support, { includeDetails: false });
  await assert.rejects(main.invoke("octopus:support-copy", support, request), /Refresh/);
  assert.equal(main.eventsOf("runtime-start").length, 0);
});
const names = (main, wanted) => main.world.events.map(([name]) => name).filter((name) => wanted.includes(name));
const claimOf = (win) => win.options.webPreferences.additionalArguments?.[0]?.slice("--octopus-access-claim=".length);

async function openPeer(main, source, token, request = {}) {
  const opening = main.invoke("octopus:client-window-open", source, token, request);
  await settle();
  const peer = main.windows().at(-1);
  const inherited = main.sendSync("octopus:access-inherit", peer, claimOf(peer));
  const bootstrap = main.sendSync("octopus:client-window-bootstrap", peer);
  await main.invoke("octopus:client-window-ready", peer, inherited, { windowId: bootstrap.windowId });
  return { peer, bootstrap, opened: await opening };
}

test("startup paints the primary window before Home recovery, and a second instance or activation reveals a window", async () => {
  const main = await launchMain({ deferReady: true });
  assert.equal(main.windows().length, 0);
  await main.releaseReady();
  assert.deepEqual(names(main, ["window", "recover-packages"]), ["window", "recover-packages"]);
  assert.equal(main.world.runtimeHosts.length, 1);
  assert.equal(main.eventsOf("runtime-start").length, 0, "the client observes the runtime and never starts it");
  await until(() => main.log().some((entry) => entry.event === "browser.native_host.unavailable"), "startup initialization completes");
  assert.equal(main.log().some((entry) => entry.event === "client.startup_failed"), false);
  const [primary] = main.windows();
  primary.minimized = true;
  main.emitApp("second-instance");
  await settle(5);
  assert.deepEqual(primary.calls.filter(([name]) => ["restore", "show", "focus"].includes(name)), [["restore"], ["show"], ["focus"]]);
  assert.equal(main.windows().length, 1);
  primary.calls.length = 0;
  main.emitApp("activate");
  assert.deepEqual(primary.calls.filter(([name]) => name === "focus"), [["focus"]]);
  primary.close();
  main.emitApp("second-instance");
  await settle(5);
  const [replacement] = main.windows();
  assert.ok(replacement && replacement !== primary);
  assert.equal(replacement.isFocused(), true);
  assert.equal(main.world.runtimeHosts.length, 1, "a second instance reuses the one runtime observer");
  const channels = [...main.electron.handlers.keys(), ...main.electron.listeners.keys()];
  assert.equal(channels.some((channel) => channel.includes("runtime-terminal")), false);
});

test("the primary window restores its saved bounds and persists them on close, with the macOS translucent chrome", async () => {
  const main = await launchMain({ files: { "window-state.json": { bounds: { x: 40, y: 50, width: 900, height: 700 } } } });
  const [primary] = main.windows();
  assert.deepEqual([primary.options.x, primary.options.y, primary.options.width, primary.options.height], [40, 50, 900, 700]);
  assert.equal(primary.options.backgroundColor, "#00000000");
  assert.equal(primary.options.transparent, true);
  assert.equal(primary.options.vibrancy, "under-window");
  assert.equal(primary.options.visualEffectState, "active");
  assert.equal(primary.options.webPreferences.additionalArguments, undefined);
  assert.deepEqual(primary.loads, [["file", path.join(appRoot, "dist/index.html"), undefined]]);
  const token = await main.unlock(primary);
  const { peer } = await openPeer(main, primary, token);
  peer.setPosition(300, 300);
  const savedBounds = () => JSON.parse(fs.readFileSync(path.join(main.world.userData, "window-state.json"), "utf8")).bounds;
  peer.close();
  await settle(50);
  assert.deepEqual(savedBounds(), { x: 40, y: 50, width: 900, height: 700 });
  primary.setPosition(60, 70);
  primary.close();
  await until(() => { try { return savedBounds().x === 60; } catch { return false; } }, "the primary window saves its bounds on close");
  assert.deepEqual(savedBounds(), { x: 60, y: 70, width: 900, height: 700 });

  const linux = await launchMain({ platform: "linux" });
  const [plain] = linux.windows();
  assert.equal(plain.options.backgroundColor, "#181818");
  assert.equal(plain.options.transparent, false);
  assert.equal(plain.options.vibrancy, undefined);
  assert.equal(linux.world.menu, undefined);
});

test("window appearance switches the macOS backdrop between translucent vibrancy and an opaque surface", async () => {
  const main = await launchMain();
  const [win] = main.windows();
  const backdrop = () => {
    const calls = win.calls.filter(([name]) => name === "setBackgroundColor" || name === "setVibrancy");
    win.calls.length = 0;
    const states = win.webContents.sentOn("octopus:window-state-changed");
    return [calls, states.at(-1)?.[0].opaqueSurface];
  };
  backdrop();
  main.sendSync("octopus:window-appearance", win, { theme: "dark", translucent: true, preference: "dark" });
  assert.deepEqual(backdrop(), [[["setBackgroundColor", "#00000000"], ["setVibrancy", "under-window"]], false]);
  assert.equal(main.electron.module.nativeTheme.themeSource, "dark");
  main.sendSync("octopus:window-appearance", win, { theme: "dark", translucent: false });
  assert.deepEqual(backdrop(), [[["setBackgroundColor", "#000000"], ["setVibrancy", null]], true]);
  assert.equal(main.electron.module.nativeTheme.themeSource, "system");
  win.bounds = { x: 0, y: 0, width: 1920, height: 1080 };
  main.sendSync("octopus:window-appearance", win, { theme: "dark", translucent: true });
  assert.deepEqual(backdrop(), [[["setBackgroundColor", "#000000"], ["setVibrancy", null]], true], "a 4K-class backing store stays opaque");
  assert.equal((await main.invoke("octopus:window-state", win)).opaqueSurface, true);
});

test("a peer window inherits its opener's access through a one-window claim and opens only once its renderer is ready", async () => {
  const main = await launchMain({ files: { "window-state.json": { bounds: { x: 40, y: 50, width: 900, height: 700 } } } });
  const [primary] = main.windows();
  await assert.rejects(main.invoke("octopus:client-window-open", primary, "stale-token", {}), /locked or expired/);
  const token = await main.unlock(primary);
  await assert.rejects(main.invoke("octopus:client-window-open", primary, token, { unexpected: true }), /unsupported field/);
  assert.equal(main.windows().length, 1);
  const opening = main.invoke("octopus:client-window-open", primary, token, { initialTarget: surface });
  let settled = false;
  void opening.then(() => { settled = true; });
  await settle();
  const peer = main.windows().at(-1);
  assert.deepEqual([peer.options.x, peer.options.y, peer.options.width, peer.options.height], [68, 78, 900, 700]);
  const claim = claimOf(peer);
  assert.deepEqual(peer.options.webPreferences.additionalArguments, [`--octopus-access-claim=${claim}`]);
  assert.deepEqual(peer.loads, [["file", path.join(appRoot, "dist/index.html"), undefined]]);
  assert.equal(main.sendSync("octopus:access-inherit", primary, claim), "");
  assert.equal(main.sendSync("octopus:access-inherit", peer, claim), token);
  const bootstrap = main.sendSync("octopus:client-window-bootstrap", peer);
  assert.equal(bootstrap.primary, false);
  assert.deepEqual(bootstrap.initialTarget, surface);
  assert.equal(main.sendSync("octopus:client-window-bootstrap", primary).windowId, "main");
  assert.equal(settled, false);
  await assert.rejects(main.invoke("octopus:client-window-ready", peer, "other-token", { windowId: bootstrap.windowId }), /locked or expired/);
  await main.invoke("octopus:quick-task-open", primary, token);
  const { token: unboundToken } = await main.invoke("octopus:access-unlock", main.windows().at(-1), PASSWORD);
  await assert.rejects(main.invoke("octopus:client-window-ready", peer, unboundToken, { windowId: bootstrap.windowId }), /authenticated peer bootstrap/);
  await assert.rejects(main.invoke("octopus:client-window-ready", primary, token, { windowId: "main" }), /authenticated peer bootstrap/);
  await main.invoke("octopus:client-window-ready", peer, token, { windowId: bootstrap.windowId });
  assert.deepEqual(await opening, { opened: true, windowId: bootstrap.windowId });

  const stalled = main.invoke("octopus:client-window-open", primary, token, {});
  await settle();
  const unready = main.windows().at(-1);
  await assert.rejects(stalled, /did not become ready/);
  assert.equal(unready.isDestroyed(), true, "a peer that never becomes ready is closed");
});

test("a pane moves between windows through begin, accept, commit, release and finalize", async () => {
  const main = await launchMain();
  const [primary] = main.windows();
  const token = await main.unlock(primary);
  const { peer } = await openPeer(main, primary, token);
  assert.deepEqual(main.sendSync("octopus:client-pane-transfer-begin", peer, "stale", { sourcePane: "secondary", sourceTabId: "chat:run-1", target: surface }),
    { ok: false, error: { message: "Client access session is locked or expired" } });
  const begun = main.sendSync("octopus:client-pane-transfer-begin", peer, token, { sourcePane: "secondary", sourceTabId: "chat:run-1", target: surface });
  assert.equal(begun.ok, true);
  const transferId = begun.value.transferId;
  const accepted = await main.invoke("octopus:client-pane-transfer-accept", primary, token, { transferId });
  assert.deepEqual(accepted.target, surface);
  const committed = main.invoke("octopus:client-pane-transfer-commit", primary, token, { transferId });
  await settle();
  const completion = { ...begun.value, sourcePane: "secondary", sourceTabId: "chat:run-1", target: surface };
  assert.deepEqual(peer.webContents.sentOn("octopus:client-pane-transfer-completed"), [[completion]]);
  assert.deepEqual(await main.invoke("octopus:client-pane-transfer-pending", peer, token), [completion]);
  assert.deepEqual(main.sendSync("octopus:client-pane-transfer-acknowledge", peer, token, { released: true, transferId }), { ok: true, value: { released: true, transferId } });
  assert.deepEqual(await committed, completion);
  assert.deepEqual(await main.invoke("octopus:client-pane-transfer-destination-pending", primary, token), [{ ...completion, committed: true }]);
  assert.deepEqual(await main.invoke("octopus:client-pane-transfer-resume", primary, token, { transferId }), completion);
  assert.deepEqual(await main.invoke("octopus:client-pane-transfer-finalize", primary, token, { transferId }), { finalized: true, transferId });
  assert.deepEqual(peer.webContents.sentOn("octopus:client-pane-transfer-finalized"), [[completion]]);

  for (const breakSource of [(win) => { win.webContents.destroyed = true; }, (win) => { win.webContents.send = () => { throw new Error("renderer went away"); }; }]) {
    const { peer: source } = await openPeer(main, primary, token);
    const next = main.sendSync("octopus:client-pane-transfer-begin", source, token, { sourcePane: "secondary", sourceTabId: "surface:drivers", target: { ...surface, runId: "", surfaceId: "drivers" } });
    await main.invoke("octopus:client-pane-transfer-accept", primary, token, { transferId: next.value.transferId });
    breakSource(source);
    await assert.rejects(main.invoke("octopus:client-pane-transfer-commit", primary, token, { transferId: next.value.transferId }), /closed before commit delivery|renderer went away/);
    assert.deepEqual(await main.invoke("octopus:client-pane-transfer-destination-pending", primary, token), []);
    source.destroy();
  }
});

test("logging out locks the session, tells every window of that owner, closes its peers and keeps the primary", async () => {
  const main = await launchMain();
  const [primary] = main.windows();
  const token = await main.unlock(primary);
  const { peer } = await openPeer(main, primary, token);
  await main.invoke("octopus:quick-task-open", primary, token);
  const quickTask = main.windows().at(-1);
  await main.invoke("octopus:support-open", primary, token);
  const support = main.windows().at(-1);
  await main.invoke("octopus:global-dictation-shortcut", primary, token, { enabled: true, shortcut: "Mod+Shift+KeyD" });
  assert.equal(main.electron.shortcuts.size, 1);
  await assert.rejects(main.invoke("octopus:access-lock", peer, "another-token"), /belongs to its authenticated full client window/);
  main.world.expiredTokens.add(token);
  const status = await main.invoke("octopus:access-lock", primary, token);
  assert.equal(status.authenticated, false);
  assert.equal(primary.webContents.sentOn("octopus:client-access-locked").length, 1);
  assert.equal(peer.webContents.sentOn("octopus:client-access-locked").length, 1);
  assert.equal(peer.isDestroyed(), true);
  assert.equal(quickTask.isDestroyed(), true);
  assert.equal(support.isDestroyed(), true);
  assert.equal(primary.isDestroyed(), false);
  assert.equal(main.electron.shortcuts.size, 0, "the owner's global dictation shortcut is released");
  main.world.expiredTokens.delete(token);
  assert.equal((await main.invoke("octopus:access-status", primary, token)).authenticated, false);
  await assert.rejects(main.invoke("octopus:access-lock", primary, token), /belongs to its authenticated full client window/);
});

test("File > New Window opens a peer for the focused authenticated window, or reveals the primary when there is none", async () => {
  const main = await launchMain();
  const [primary] = main.windows();
  const newWindow = () => main.world.menu.template.find((menu) => menu.label === "File").submenu.find((item) => item.label === "New Window");
  newWindow().click();
  assert.equal(main.windows().length, 1);
  assert.equal(primary.isVisible() && primary.isFocused(), true);
  const token = await main.unlock(primary);
  newWindow().click();
  const peer = main.windows().at(-1);
  assert.notEqual(peer, primary);
  assert.equal(main.sendSync("octopus:client-window-bootstrap", peer).primary, false);
  assert.equal(main.sendSync("octopus:access-inherit", peer, claimOf(peer)), token);
});

test("quick task, Worker Environment popout and global dictation hand their request to the owner's client window", async () => {
  const main = await launchMain();
  const [primary] = main.windows();
  const token = await main.unlock(primary);
  await main.invoke("octopus:quick-task-open", primary, token);
  const quickTask = main.windows().at(-1);
  await assert.rejects(main.invoke("octopus:quick-task-open-main", quickTask, token, "bad\u0000run"), /bounded run id/);
  assert.deepEqual(await main.invoke("octopus:quick-task-open-main", quickTask, token, "run-7"), { opened: true, runId: "run-7" });
  assert.deepEqual(primary.webContents.sentOn("octopus:open-run-requested"), [["run-7"]]);
  assert.equal(primary.isFocused(), true);
  assert.equal(quickTask.isDestroyed(), true);
  const target = { workerEnvironmentId: "we-1", runId: "run-7", panelKind: "changes" };
  await main.invoke("octopus:worker-environment-window-open-main", primary, token, target);
  assert.deepEqual(primary.webContents.sentOn("octopus:open-worker-environment-requested"), [[target]]);
  assert.deepEqual(await main.invoke("octopus:global-dictation-shortcut", primary, token, { enabled: true, shortcut: "Mod+Shift+KeyD" }),
    { enabled: true, shortcut: "CommandOrControl+Shift+D", registered: true });
  main.electron.shortcuts.get("CommandOrControl+Shift+D")();
  assert.equal(primary.webContents.sentOn("octopus:global-dictation-requested").length, 1);

  await main.invoke("octopus:quick-task-open", primary, token);
  const secondQuickTask = main.windows().at(-1);
  primary.close();
  await main.invoke("octopus:quick-task-open-main", secondQuickTask, token, "run-8");
  const replacement = main.windows().at(-1);
  assert.notEqual(replacement, primary);
  assert.deepEqual(replacement.webContents.sentOn("octopus:open-run-requested"), [["run-8"]]);
  assert.equal(main.sendSync("octopus:access-inherit", replacement, claimOf(replacement)), token);
});

test("the launch terminal belongs to the primary window, reuses its shell and opens the runtime's prepared command", async () => {
  const main = await launchMain();
  const [primary] = main.windows();
  const token = await main.unlock(primary);
  const { peer } = await openPeer(main, primary, token);
  await assert.rejects(main.invoke("octopus:launch-terminal-open", peer, "", { cols: 80, rows: 24 }), /primary Octopus window/);
  await main.invoke("octopus:quick-task-open", primary, token);
  await assert.rejects(main.invoke("octopus:launch-terminal-open", main.windows().at(-1), token, { cols: 80, rows: 24 }), /full client window/);
  const opened = await main.invoke("octopus:launch-terminal-open", primary, "", { cols: 120, rows: 40 });
  assert.equal(opened.sessionId, "launch-session");
  const [, owner, request] = main.eventsOf("terminal-open").at(-1);
  assert.deepEqual(owner, { scope: "launch", senderId: primary.webContents.id, token: "" });
  assert.deepEqual(request, { terminalId: "launch", cwd: main.world.root, env: { OCTOPUS_HOME: main.world.home }, preparedCommand: "octopus up", cols: 120, rows: 40 });
  main.world.existingLaunch = { terminalId: "launch", sessionId: "existing", running: true, transcript: "ready" };
  main.world.runtimeState = { ...main.world.runtimeState, phase: "ready", ready: true };
  const probes = main.eventsOf("probe").length;
  assert.equal(await main.invoke("octopus:launch-terminal-open", primary, "", { cols: 100, rows: 30 }), main.world.existingLaunch);
  assert.deepEqual(main.eventsOf("terminal-open-existing").at(-1)[2], { terminalId: "launch", cols: 100, rows: 30 });
  assert.equal(main.eventsOf("probe").length, probes, "an existing launch shell wins before any readiness probe");
  main.world.existingLaunch = null;
  await assert.rejects(main.invoke("octopus:launch-terminal-open", primary, "", { cols: 100, rows: 30 }), /already ready for this Home/);
  const [terminal] = main.world.terminalHosts;
  terminal.emit("data", { owner: { scope: "launch", senderId: primary.webContents.id, token: "" }, terminalId: "launch", sessionId: "launch-session", data: "ready in 4.2s" });
  assert.deepEqual(primary.webContents.sentOn("octopus:terminal-data"), [[{ terminalId: "launch", sessionId: "launch-session", data: "ready in 4.2s" }]]);
  terminal.emit("data", { owner: { scope: "launch", senderId: 9999, token: "" }, data: "orphan" });
  assert.deepEqual(main.eventsOf("terminal-close-sender").at(-1), ["terminal-close-sender", 9999]);
  terminal.emit("exit", { owner: { scope: "access", senderId: primary.webContents.id, token: "stale" }, exitCode: 0 });
  assert.deepEqual(main.eventsOf("terminal-close-owner").at(-1), ["terminal-close-owner", "stale"]);
  assert.deepEqual(main.world.runtimeHosts[0].options.launchEnv(), {
    OCTOPUS_HOME: main.world.home,
    OCTOPUS_SOURCE_REGISTRY: path.join(main.world.root, "repo", "registry"),
    OCTOPUS_MACHINE_CODEX_AUTH_FILE: path.join(main.world.codexHome, "auth.json"),
    OCTOPUS_MACHINE_CODEX_AUTH_LOCK_FILE: path.join(main.world.codexHome, "auth.lock"),
  });
});

test("quit waits for startup recovery, interrupts the launch shell, waits for the runtime to stop, then exits", async () => {
  let releaseRecovery;
  const main = await launchMain({ world: { recoveryGate: new Promise((resolve) => { releaseRecovery = resolve; }) } });
  const [primary] = main.windows();
  const token = await main.unlock(primary);
  await main.invoke("octopus:terminal-open", primary, token, { terminalId: "shell" });
  await main.invoke("octopus:quick-task-open", primary, token);
  const quickTask = main.windows().at(-1);
  assert.deepEqual(await main.invoke("octopus:runtime-events-subscribe", quickTask, token), { subscribed: true });
  assert.equal(quickTask.webContents.sentOn("octopus:runtime-host-state").length, 1);
  main.world.events.length = 0;
  main.electron.module.app.quit();
  await settle(10);
  assert.deepEqual(names(main, ["quit", "exit"]), ["quit"], "the first quit is held for the close protocol");
  assert.equal(primary.isDestroyed(), true);
  main.world.runtimeHosts[0].emit("state", { phase: "unmanaged", ready: false });
  assert.equal(quickTask.webContents.sentOn("octopus:runtime-host-state").length, 1, "no runtime state is published once cleanup started");
  assert.equal(main.eventsOf("interrupt-launch").length, 0, "cleanup waits for the startup recovery in flight");
  main.world.events.push(["recovery-released"]);
  releaseRecovery();
  await until(() => main.eventsOf("exit").length, "the approved quit exits");
  assert.deepEqual(names(main, ["quit", "recovery-released", "interrupt-launch", "wait-stopped", "terminal-terminate", "runtime-close", "terminal-close-all", "exit"]),
    ["quit", "recovery-released", "interrupt-launch", "wait-stopped", "terminal-terminate", "runtime-close", "terminal-close-all", "quit", "exit"]);
});

test("the live projection is invalidated once, when the runtime first becomes ready, and every runtime state reaches subscribers", async () => {
  const main = await launchMain();
  const [primary] = main.windows();
  const token = await main.unlock(primary);
  await main.invoke("octopus:projection-read", primary, token, { method: "projection.operator.os.scope" });
  await main.invoke("octopus:runtime-events-subscribe", primary, token);
  const host = main.world.runtimeHosts[0];
  for (const phase of ["ready", "ready", "unmanaged", "ready"]) host.emit("state", { phase, ready: phase === "ready" });
  assert.equal(main.world.events.filter(([name, profile, value]) => name === "projection-invalidate" && profile === "live" && value === false).length, 1);
  assert.deepEqual(primary.webContents.sentOn("octopus:runtime-host-state").slice(1).map(([state]) => state.phase), ["ready", "ready", "unmanaged", "ready"]);
});

test("Touch ID status reads only the private preference and enrollment, and enabling, declining and unlocking go through the prompt", async () => {
  const unavailable = await launchMain({ world: { touchId: false } });
  assert.deepEqual(await unavailable.invoke("octopus:touch-id-status", unavailable.windows()[0]), { available: false, enrolled: false, prompted: false });
  for (const [mode, value, error] of [[0o644, { version: 1, prompted: true }, /permissions are too broad/], [0o600, { version: 1, prompted: false }, /unsupported shape/], [0o600, "not json", /not valid JSON/]]) {
    const broken = await launchMain();
    writeFixture(path.join(broken.world.userData, "touch-id-preference.json"), value, mode);
    await assert.rejects(broken.invoke("octopus:touch-id-status", broken.windows()[0]), error);
  }
  const linked = await launchMain();
  writeFixture(path.join(linked.world.root, "elsewhere.json"), { version: 1, prompted: true });
  fs.symlinkSync(path.join(linked.world.root, "elsewhere.json"), path.join(linked.world.userData, "touch-id-preference.json"));
  await assert.rejects(linked.invoke("octopus:touch-id-status", linked.windows()[0]), /not a valid regular file/);

  const main = await launchMain();
  const [primary] = main.windows();
  assert.deepEqual(await main.invoke("octopus:touch-id-status", primary), { available: true, enrolled: false, prompted: false });
  const token = await main.unlock(primary);
  await assert.rejects(main.invoke("octopus:touch-id-set-enabled", primary, token, "yes"), /explicit enabled state/);
  assert.deepEqual(await main.invoke("octopus:touch-id-set-enabled", primary, token, false), { available: true, enrolled: false, prompted: true });
  assert.equal(fs.statSync(path.join(main.world.userData, "touch-id-preference.json")).mode & 0o777, 0o600);
  assert.equal(main.eventsOf("touch-id").length, 0);
  assert.deepEqual(await main.invoke("octopus:touch-id-set-enabled", primary, token, true), { available: true, enrolled: true, prompted: true });
  assert.deepEqual(main.eventsOf("touch-id"), [["touch-id", "Enable Touch ID to unlock Octopus"]]);
  const sealedReads = main.eventsOf("safe-storage").length;
  assert.deepEqual(await main.invoke("octopus:touch-id-status", primary), { available: true, enrolled: true, prompted: true });
  assert.equal(main.eventsOf("safe-storage").length, sealedReads, "status never opens the sealed verifier");
  await main.invoke("octopus:access-lock", primary, token);
  const unlocked = await main.invoke("octopus:touch-id-unlock", primary);
  assert.deepEqual(main.eventsOf("touch-id").at(-1), ["touch-id", "Unlock Octopus"]);
  assert.ok(main.eventsOf("safe-storage").length > sealedReads);
  assert.equal((await main.invoke("octopus:transport-defaults", primary, unlocked.token)).octopusHome, main.world.home);
  assert.equal((await main.invoke("octopus:access-lock", primary, unlocked.token)).authenticated, false, "the Touch ID session belongs to the window that unlocked");
  const enrollmentFile = path.join(main.world.userData, "touch-id-access.json");
  const enrollment = JSON.parse(fs.readFileSync(enrollmentFile, "utf8"));
  writeFixture(enrollmentFile, { ...enrollment, sealedVerifier: Buffer.from("tampered").toString("base64") });
  await assert.rejects(main.invoke("octopus:touch-id-unlock", primary), /could not be verified/);
  writeFixture(enrollmentFile, enrollment);
  main.world.sealKey = 2;
  const rotated = await main.invoke("octopus:touch-id-unlock", primary);
  const resealed = JSON.parse(fs.readFileSync(enrollmentFile, "utf8"));
  assert.equal(resealed.credentialFingerprint, enrollment.credentialFingerprint);
  assert.match(Buffer.from(resealed.sealedVerifier, "base64").toString(), /^sealed:2:[a-f0-9]{64}$/, "a verifier the OS asks to re-encrypt is sealed again under the current key");
  await main.invoke("octopus:access-lock", primary, rotated.token);
  main.world.touchIdRefused = true;
  await assert.rejects(main.invoke("octopus:touch-id-unlock", primary), /cancelled/);
  const again = await main.unlock(primary);
  assert.deepEqual(await main.invoke("octopus:touch-id-set-enabled", primary, again, false), { available: true, enrolled: false, prompted: true });
  await assert.rejects(main.invoke("octopus:touch-id-set-enabled", primary, again, true), /cancelled/);
  assert.equal(fs.existsSync(enrollmentFile), false, "declining clears the enrollment and a refused prompt writes none");
});

test("a client extension action runs with the Home, the Codex home and the verified installed extensions only", async () => {
  const main = await launchMain();
  const [primary] = main.windows();
  const token = await main.unlock(primary);
  const request = { actionId: "repair", extensionId: "fixture.extension", incident: { id: "incident-1", interfaceId: "world_provider_codex" } };
  const result = await main.invoke("octopus:client-extension-action", primary, token, request);
  assert.equal(result.ok, true);
  assert.deepEqual(main.eventsOf("extension-action"), [["extension-action", { octopusHome: main.world.home, codexHome: main.world.codexHome, installedExtensionIds: ["fixture.extension"] }, request]]);
  await main.invoke("octopus:quick-task-open", primary, token);
  await assert.rejects(main.invoke("octopus:client-extension-action", main.windows().at(-1), token, request), /full client window/);
});

function chatClient(world) {
  return {
    submitOperatorTurn: async (deps, request) => {
      world.events.push(["submit-operator-turn", request]);
      await deps.readArtifactPayload({ artifactId: "artifact-1", label: "operator.message" });
      return { chatInterfaceId: "world_operator_chat_home", runId: "run-1", label: "operator.message", messageId: request.messageId,
        attachmentCount: 0, contentHash: "hash-1", messageRecovered: false, messageVerdict: { admitted: { seq: 4 } } };
    },
    submitClientOperatorMessageArtifact: async () => world.events.push(["message-artifact"]),
    submitOperatorTurnControl: async (deps, request) => {
      world.events.push(["turn-control", request]);
      return deps.handInArtifact({ interfaceId: request.interfaceId, label: "operator.control", endpoint: "unix:///missing/socket" });
    },
    readOperatorChatArtifacts: async (deps, request) => {
      await deps.readArtifactPayload({ artifactId: "a", label: "turn.stream" });
      await deps.readArtifactPayload({ artifactId: "b", label: "turn.message" });
      await deps.readArtifactPayload({ artifactId: "c", label: "turn.stream", projection: { kind: "tail" } });
      return world.chatBatch(request);
    },
    assertGenericOperatorChatArtifactAllowed: async (request) => {
      world.events.push(["chat-artifact-check", request.interfaceId]);
      if (request.label === "forbidden") throw new Error("operator chat refuses this artifact");
    },
  };
}

test("chat turns, steering, turn control and artifact reads use the verified chat client against the Home ledger", async () => {
  const main = await launchMain();
  main.world.chatClient = chatClient(main.world);
  main.world.chatBatch = () => ({ artifacts: [{ source: "movement", from_interface_id: "world_provider_codex", from_interface_role: "provider", movement_seq: 1, recorded_at_unix_ms: 2, artifact_id: "a", content_hash: "h", checkpoint_id: "c" }], cursor: { offset: 0, seq: 1 } });
  const [primary] = main.windows();
  const token = await main.unlock(primary);
  await main.invoke("octopus:projection-read", primary, token, { method: "projection.operator.os.scope" });
  const blobs = path.join(main.world.home, "ledger", "blobs");
  main.world.events.length = 0;
  const turn = await main.invoke("octopus:submit-chat-turn", primary, token, { runId: "run-1", messageId: "m-1", text: "hello" });
  assert.equal(turn.ok, true);
  assert.equal(turn.value.runId, "run-1");
  const [loader] = main.world.chatLoaderOptions;
  assert.equal(main.world.chatLoaderOptions.length, 1);
  assert.equal(loader.clientPath(), path.join(main.world.home, "registry", "interface", "world_operator_chat", "client.cjs"));
  assert.equal(loader.packageRoots().activeRegistryRoot, path.join(main.world.home, "registry"));
  assert.equal(loader.packageRoots().stateRoot, path.join(main.world.home, "client", "package-state"));
  assert.deepEqual(names(main, ["recover-workspace", "chat-client-load", "submit-operator-turn", "read-payload", "projection-invalidate"]),
    ["chat-client-load", "submit-operator-turn", "read-payload"], "a submitted turn leaves recovery to its transaction owner and never invalidates the live projection itself");
  assert.equal(main.eventsOf("read-payload")[0][1].blobsRoot, blobs);

  main.world.events.length = 0;
  const steered = await main.invoke("octopus:submit-chat-steer", primary, token, { runId: "run-1", messageId: "m-2", text: "steer" });
  assert.equal(steered.ok, true);
  assert.deepEqual(steered.value, {
    interfaceId: "world_operator_chat_home", runId: "run-1", label: "operator.message", messageId: "m-2", attachmentCount: 0,
    contentHash: "hash-1", messageRecovered: false, verdict: { admitted: { seq: 4 } },
  });
  assert.deepEqual(names(main, ["submit-operator-turn", "message-artifact"]), ["submit-operator-turn"]);
  assert.equal(main.eventsOf("read-payload")[0][1].blobsRoot, blobs);

  main.world.storageError = Object.assign(new Error("Disk full. Your draft is retained."), { code: "home_storage_blocked", deliveryState: "not_sent" });
  main.world.events.length = 0;
  for (const channel of ["octopus:submit-chat-turn", "octopus:submit-chat-steer"]) {
    const refused = await main.invoke(channel, primary, token, { runId: "run-1", text: "Keep this draft" });
    assert.equal(refused.ok, false);
    assert.equal(refused.error.code, "home_storage_blocked");
    assert.equal(refused.error.deliveryState, "not_sent");
  }
  assert.equal(main.eventsOf("submit-operator-turn").length, 0);
  main.world.storageError = null;

  main.world.events.length = 0;
  const control = await main.invoke("octopus:submit-operator-turn-control", primary, token, { interfaceId: "world_operator_chat_home", runId: "run-1", action: "pause", controlId: "control-1" });
  assert.deepEqual(main.eventsOf("turn-control"), [["turn-control", { interfaceId: "world_operator_chat_home", runId: "run-1", action: "pause", controlId: "control-1" }]]);
  assert.equal(control.deliveryError.stage, "not-sent");
  assert.deepEqual(names(main, ["recover-workspace", "projection-read"]), [], "turn control neither recovers the workspace nor reads the projection");

  main.world.events.length = 0;
  assert.deepEqual(await main.invoke("octopus:operator-chat-artifacts-read", primary, token, { runId: "run-1" }), main.world.chatBatch());
  assert.deepEqual(main.eventsOf("read-payload").map(([, input]) => [input.artifactId, input.blobsRoot, input.maxPayloadBytes]),
    [["a", blobs, 256 * 1024 * 1024], ["b", blobs, undefined]]);
  assert.deepEqual(main.eventsOf("read-projection").map(([, input]) => [input.artifactId, input.blobsRoot]), [["c", blobs]]);
  main.world.chatBatch = () => ({ artifacts: [{ from_interface_id: "world_provider_codex", from_interface_role: "" }], cursor: { offset: 0, seq: null } });
  await assert.rejects(main.invoke("octopus:operator-chat-artifacts-read", primary, token, { runId: "run-1" }), (error) => /from_interface_role/.test(error.message));
});

test("a client interface artifact asks the chat client only when it targets a chat interface", async () => {
  const main = await launchMain();
  main.world.chatClient = chatClient(main.world);
  const [primary] = main.windows();
  const token = await main.unlock(primary);
  main.world.events.length = 0;
  await main.invoke("octopus:submit-interface-artifact", primary, token, { interfaceId: "world_telegram", label: "note" });
  assert.deepEqual(names(main, ["chat-client-load", "chat-artifact-check", "artifact-submitted"]), ["artifact-submitted"]);
  main.world.events.length = 0;
  await main.invoke("octopus:submit-interface-artifact", primary, token, { interfaceId: "world_operator_chat_home", label: "note" });
  assert.deepEqual(names(main, ["chat-client-load", "chat-artifact-check", "artifact-submitted"]), ["chat-client-load", "chat-artifact-check", "artifact-submitted"]);
  main.world.events.length = 0;
  await assert.rejects(main.invoke("octopus:submit-interface-artifact", primary, token, { interfaceId: "world_operator_chat_home", label: "forbidden" }), /refuses this artifact/);
  assert.deepEqual(names(main, ["artifact-submitted"]), []);
});

test("the renderer's transport declaration types the client window and pane transfer calls exactly", () => {
  const ts = require("typescript");
  const parsed = ts.parseJsonConfigFileContent(ts.readConfigFile(path.join(appRoot, "tsconfig.json"), ts.sys.readFile).config, ts.sys, appRoot);
  const fixture = path.join(appRoot, "src", "clientWindowTransport.contract.ts");
  const text = `import type { ClientPaneDragPayload, ClientPaneTransferAcknowledgementRequest, ClientPaneTransferBeginRequest, ClientPaneTransferCompleted,
  ClientPaneTransferRequest, ClientWindowBootstrap, ClientWindowOpenOutcome, ClientWindowOpenRequest, ClientWindowReadyRequest } from "./shell/clientWindowModel";
type Transport = NonNullable<Window["octopusTransport"]>;
type Exact<A, B> = (<T>() => T extends A ? 1 : 2) extends (<T>() => T extends B ? 1 : 2) ? true : false;
type Expected = {
  clientWindowBootstrap: () => ClientWindowBootstrap | null;
  openClientWindow: (request?: ClientWindowOpenRequest) => Promise<ClientWindowOpenOutcome>;
  clientWindowReady: (request: ClientWindowReadyRequest) => Promise<ClientWindowOpenOutcome>;
  beginClientPaneTransfer: (request: ClientPaneTransferBeginRequest) => ClientPaneDragPayload;
  acknowledgeClientPaneTransfer: (request: ClientPaneTransferAcknowledgementRequest) => { released: boolean; transferId: string };
  pendingClientPaneTransfers: () => Promise<ClientPaneTransferCompleted[]>;
  pendingClientPaneDestinationTransfers: () => Promise<(ClientPaneTransferCompleted & { committed: boolean })[]>;
  resumeClientPaneTransfer: (request: ClientPaneTransferRequest) => Promise<ClientPaneTransferCompleted>;
  finalizeClientPaneTransfer: (request: ClientPaneTransferRequest) => Promise<{ finalized: true; transferId: string }>;
  onClientPaneTransferCompleted: (listener: (completion: ClientPaneTransferCompleted) => void) => () => void;
};
export const contract: { [K in keyof Expected]: Exact<Transport[K], Expected[K]> } = {
  clientWindowBootstrap: true, openClientWindow: true, clientWindowReady: true, beginClientPaneTransfer: true, acknowledgeClientPaneTransfer: true,
  pendingClientPaneTransfers: true, pendingClientPaneDestinationTransfers: true, resumeClientPaneTransfer: true, finalizeClientPaneTransfer: true,
  onClientPaneTransferCompleted: true,
};
export const outcome: Exact<ClientWindowOpenOutcome, { opened: true; windowId: string }> = true;
`;
  const host = ts.createCompilerHost(parsed.options);
  const getSourceFile = host.getSourceFile.bind(host);
  host.getSourceFile = (name, ...rest) => name === fixture ? ts.createSourceFile(name, text, ts.ScriptTarget.ES2020, true) : getSourceFile(name, ...rest);
  const fileExists = host.fileExists.bind(host);
  host.fileExists = (name) => name === fixture || fileExists(name);
  const program = ts.createProgram([path.join(appRoot, "src/electron.d.ts"), fixture], parsed.options, host);
  const diagnostics = ts.getPreEmitDiagnostics(program, program.getSourceFile(fixture)).map((diagnostic) => ts.flattenDiagnosticMessageText(diagnostic.messageText, "\n"));
  assert.deepEqual(diagnostics, []);
});

