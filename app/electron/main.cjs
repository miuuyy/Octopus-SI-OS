const { app, BrowserWindow, clipboard, dialog, globalShortcut, ipcMain, Menu, nativeTheme, net: electronNet, powerSaveBlocker, safeStorage, screen, session, shell, systemPreferences } = require("electron");
const net = require("node:net");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { spawn } = require("node:child_process");
const { createHash, randomBytes, randomUUID } = require("node:crypto");
const { nativeTrafficLightPosition, nativeControlInset } = require("./windowChrome.cjs");
const { resolveClientLaunchSelection, saveClientLaunchSelection } = require("./clientLaunchSelection.cjs");

const clientLaunchSelectionPath = path.join(app.getPath("appData"), "Octopus", "launch.json");
const clientLaunchSelection = resolveClientLaunchSelection({ selectionPath: clientLaunchSelectionPath, userRoot: os.homedir(), env: process.env });
process.env.OCTOPUS_HOME = clientLaunchSelection.home;
if (clientLaunchSelection.codexHome) process.env.CODEX_HOME = clientLaunchSelection.codexHome;
const clientEnvironment = loadClientEnvironment();
const clientProfileRoot = path.join(resolveOctopusHome(), "client");
fs.mkdirSync(clientProfileRoot, { recursive: true, mode: 0o700 });
app.setPath("userData", clientProfileRoot);
const clientLogsRoot = path.join(clientProfileRoot, "logs");
fs.mkdirSync(clientLogsRoot, { recursive: true, mode: 0o700 });
app.setAppLogsPath(clientLogsRoot);
const clientZoomPath = path.join(clientProfileRoot, "window-zoom.json");
const touchIdEnrollmentPath = path.join(clientProfileRoot, "touch-id-access.json");
const touchIdPreferencePath = path.join(clientProfileRoot, "touch-id-preference.json");
const isDev = Boolean(process.env.VITE_DEV_SERVER_URL);
const {
  createClientProjectionStore,
} = require("./clientDataSource.cjs");
const {
  mutateApplicationPrivateConfiguration,
  mutateOctopusOsWorkspace,
  mutateScheduledInterface,
  setupTriggerEntries,
  setupTriggerProof,
  armSetupTriggers,
} = require("./projection.cjs");
const { projectionReadRequest } = require("./projectionReadContract.cjs");
const { createClientAccessController } = require("./clientAccess.cjs");
const {
  cascadeClientWindowBounds,
  createClientWindowRegistry,
  validateClientWindowOpenRequest,
} = require("./clientWindows.cjs");
const { createPaneWindowDragHost } = require("./paneWindowDrag.cjs");
const { createClientWindowCloseHost } = require("./clientWindowClose.cjs");
const { createClientContextMenuHost } = require("./clientContextMenu.cjs");
const { revealGestureTrace, saveGestureTrace } = require("./gestureTraces.cjs");
const { createClientAttachmentStagingStore, selectClientAttachments, stageClientAttachmentPaths } = require("./clientAttachments.cjs");
const {
  downloadClientFileCopy,
  listClientFileOpenTargets,
  materializeClientFile,
  openClientFile,
  openClientFileWithApplication,
  saveClientFile,
} = require("./clientFiles.cjs");
const {
  createClientArtifactStreamStore,
  submitClientInterfaceArtifact,
} = require("./clientArtifacts.cjs");
const { createCdpViewerManager } = require("./cdpViewer.cjs");
const { createDesktopSurfaceHost, createDesktopPresentationHost, createDesktopPermissionHost, presentationSource } = require("./desktopSurface.cjs");
const { createClientPresentationHost } = require("./nativePresentation.cjs");
const { createNativeDesktopSettingsHost } = require("./nativeDesktopSettings.cjs");
const { createProcessViewerManager } = require("./processViewer.cjs");
const { createWorkerEnvironmentOperatorActions } = require("./workerEnvironmentActions.cjs");
const { createWorkerEnvironmentFileReader } = require("./workerEnvironmentFiles.cjs");
const { createOsFileStore } = require("./osFiles.cjs");
const { createLabFileStore } = require("./labFiles.cjs");
const {
  captureArtifactDelivery,
  handInArtifact,
  normalizedEndpoint,
  validateArtifactEndpoint,
} = require("./interfaceTransport.cjs");
const { readContentAddressedArtifactPayload, readContentAddressedArtifactProjection, prepareContentAddressedPayloadBindings, payloadArtifactBindings } = require("./contentAddressedArtifact.cjs");
const { createClientSurfaceDocumentStore, filterClientSurfaceSnapshot } = require("./surfaceDocuments.cjs");
const { createClientExtensionStore, readClientExtensionFilesystemSource } = require("./clientExtensions.cjs");
const { createClientSetupStore } = require("./clientSetup.cjs");
const { createChatgptSignIn } = require("./chatgptSignIn.cjs");
const { createClientProviderMetadata, validateProviderModelSelection } = require("./clientProviderMetadata.cjs");
const { createApplicationConnectionHost, createRegistryConnectionTargets } = require("./applicationConnection.cjs");
const { runClientDoctor } = require("./clientDoctor.cjs");
const { createClientSupportHost } = require("./clientSupport.cjs");
const {
  clientExtensionActionFailure,
  clientExtensionActionSuccess,
  executeClientExtensionAction,
  readClientExtensionActionStatuses,
} = require("./clientExtensionActions.cjs");
const { createCodexUsageReader } = require("./codexUsage.cjs");
const { createTerminalHost } = require("./terminalHost.cjs");
const { createRuntimeHost } = require("./runtimeHost.cjs");
const { transcribeClientDictation, dictationFailure } = require("./clientDictation.cjs");
const { createClientDictationStream } = require("./clientDictationStream.cjs");
const { createClientDictationRecordings } = require("./clientDictationRecordings.cjs");
const clientDictationRecordings = createClientDictationRecordings(path.join(app.getPath("userData"), "dictation-recordings"));
const clientDictationRequests = new Map();
const clientDictationStreams = new Map();
const {
  operatorChatSubmitDiagnostic,
  operatorChatSubmitFailure,
  operatorChatSubmitSuccess,
} = require("./operatorChatSubmitResult.cjs");
const {
  assertOperatorChatArtifactBatch,
  createOperatorChatClientLoader,
  isOperatorChatClientInterfaceCandidate,
} = require("./operatorChatClientContract.cjs");
const {
  applyLocalApplicationPack,
  readLocalPackageCatalog,
  recoverLocalPackageTransactions,
  registerLocalPackageMutationGuard,
  withLocalPackageInstallation,
  withInstalledAgentPreset,
} = require("./localPackages.cjs");
const { planClientAgentTemplate } = require("./clientAgentCatalog.cjs");
const { createNativeBrowserSurfaceAdapter, createBrowserPresentationHost, createBrowserDownloadHistory } = require("./browserSurface.cjs");
const {
  initializeWorkspace,
  inspectWorkspaceSetupState,
  mutateWorkspaceProject,
  readWorkspaceSetupState: readWorkspaceSetupStateTransaction,
  recoverWorkspaceTransactions,
} = require("./workspaceSetup.cjs");

const { commitProjectionClientAuthoring: commitProjectionClientAuthoringTransaction } = require("./clientWorkspaceAuthoring.cjs");

const OPERATOR_CHAT_INTERFACE = "world_operator_chat";

const projectionStores = new Map();
let clientSurfaceDocumentStore = null;
let clientExtensionStore = null;
let clientSetupStore = null;
let codexUsageReader = null;
let clientAccessController = null;
let clientArtifactStreamStore = null;
let clientAttachmentStagingStore = null;
let cdpViewerManager = null;
let applicationConnectionHost = null;
let nativeBrowserHost = null;
let nativeBrowserStartup = null;
let nativeBrowserStartupError = null;
let nativeBrowserHome = "";
let nativeBrowserModuleLoaded = false;
let nativeBrowserMutationGuard = null;
let nativeBrowserSurface = null;
let desktopSurfaceHost = null;
let nativeDesktopPermissions = null;
let nativeDesktopSettings = null;
const desktopSurfaceSenders = new WeakSet();
let nativeDesktopPresentations = null;
let nativeBrowserPresentations = null;
let nativeBrowserDownloadHistory = null;
let nativeClientPresentations = null;
let nativeDesktopRefreshScheduled = false;
let nativeDesktopRefreshRunning = false;
let nativeDesktopRefreshAgain = false;
const nativeDesktopWindowHosts = new Map();
let processViewerManager = null;
let workerEnvironmentOperatorActions = null;
let workerEnvironmentFileReader = null;
let osFileStore = null;
let labFileStore = null;
let runtimeHost = null;
let runtimeInitializationPromise = null;
let runtimeInitializationComplete = false;
let operatorChatClientLoader = null;
const runtimeEventSubscriptions = new Map();
let terminalHost = null;
let primaryWindow = null;
const paneDestinationPresentations = new WeakMap();
const clientWindows = createClientWindowRegistry({
  onTransferEnd({ completion, destinationWebContentsId, finalized, sourceWebContentsId }) {
    const destination = clientWindows.entryForWebContents(destinationWebContentsId);
    if (finalized && destination) presentPaneDestination(destination.win);
    if (!finalized && destination && !destination.win.webContents.isDestroyed()) destination.win.webContents.send("octopus:client-pane-transfer-cancelled", completion);
    const source = clientWindows.entryForWebContents(sourceWebContentsId);
    if (source && !source.win.webContents.isDestroyed()) {
      source.win.webContents.send(finalized ? "octopus:client-pane-transfer-finalized" : "octopus:client-pane-transfer-cancelled", completion);
    }
  },
});
const paneWindowDrag = createPaneWindowDragHost({
  registry: clientWindows,
  cursor: () => screen.getCursorScreenPoint(),
  capturePreview: capturePaneDragPreview,
  openPreview: openPaneDragPreview,
  createDestination(source, point) {
    const win = createWindow(source.accessToken, { primary: false, deferredShow: true, reattachWindowId: source.bootstrap.windowId });
    paneDestinationPresentations.set(win, { fullscreen: source.win.isFullScreen(), maximized: source.win.isMaximized() });
    const bounds = win.getBounds();
    const area = screen.getDisplayNearestPoint(point).workArea;
    win.setPosition(Math.round(Math.max(area.x, Math.min(point.x, area.x + area.width - bounds.width))), Math.round(Math.max(area.y, Math.min(point.y, area.y + area.height - bounds.height))));
    return clientWindows.entryForWindow(win);
  },
  positionDestination(entry, contentOrigin, point) {
    const win = entry.win;
    const bounds = win.getBounds();
    const content = win.getContentBounds();
    const area = screen.getDisplayNearestPoint({ x: Math.round(point.x), y: Math.round(point.y) }).workArea;
    const x = contentOrigin.x - (content.x - bounds.x);
    const y = contentOrigin.y - (content.y - bounds.y);
    win.setPosition(
      Math.round(Math.max(area.x - bounds.width + 160, Math.min(x, area.x + area.width - 160))),
      Math.round(Math.max(area.y, Math.min(y, area.y + area.height - 120))),
    );
  },
  discardDestination(entry) {
    if (!entry.win.isDestroyed()) entry.win.destroy();
  },
  notifyDestination(ownerId, accepted) {
    clientWindows.entryForWebContents(ownerId)?.win.webContents.send("octopus:client-pane-transfer-accepted", accepted);
  },
  notifySource(ownerId, state) {
    const entry = clientWindows.entryForWebContents(ownerId);
    if (entry && !entry.win.webContents.isDestroyed()) entry.win.webContents.send("octopus:client-pane-drag-state", state);
  },
  reportError(error) { console.error("Pane preview failed:", error.message); },
});
let quickTaskWindow = null;
let quickTaskAccessToken = "";
const workerEnvironmentWindows = new Map();
const taskMonitorWindows = new Map();
let quitCleanupStarted = false;
let quitReady = false;
let relaunchRequested = false;
const clientWindowClose = createClientWindowCloseHost({
  accessTokenForSender(sender) { return clientWindows.entryForWebContents(sender.id)?.accessToken ?? ""; },
  authorize(sender, token) {
    getClientAccessController().requireSession(token);
    const entry = clientWindows.entryForWebContents(sender.id);
    if (!entry || entry.win.webContents !== sender || entry.accessToken !== token) {
      throw new Error("Window close belongs to its authenticated client renderer.");
    }
  },
  onQuitApproved: startClientQuitCleanup,
  onQuitCancelled() { relaunchRequested = false; },
  reportError(error) { console.error("Client window close failed:", error); },
});
const clientContextMenu = createClientContextMenuHost({
  Menu,
  resolveOwner(event) {
    const entry = clientWindows.entryForWebContents(event.sender.id);
    if (!entry || entry.win.webContents !== event.sender || event.senderFrame !== event.sender.mainFrame) return null;
    const expected = isDev
      ? new URL(process.env.VITE_DEV_SERVER_URL)
      : require("node:url").pathToFileURL(path.join(__dirname, "../dist/index.html"));
    const actual = new URL(event.senderFrame.url);
    if (actual.protocol !== expected.protocol || actual.host !== expected.host || actual.pathname !== expected.pathname
      || actual.username || actual.password) return null;
    return entry.win;
  },
  reportError(error) { console.error("Native context-menu cleanup failed:", error); },
});
const powerSaveBlockers = new Map();
let globalDictationShortcut = "";
let globalDictationAccessToken = "";
let nativeCommandShortcuts = {};
const accessInheritanceClaims = new Map();
const terminalSenderIds = new Set();
const MAX_CLIENT_LOG_BYTES = 4 * 1024 * 1024;
let latestSetupProgress = null;
let clientSupportHost = null;

function clientApplicationInfo() {
  let build = null;
  try { build = JSON.parse(fs.readFileSync(path.join(__dirname, "..", "build-info.json"), "utf8")); }
  catch (error) { if (error.code !== "ENOENT") build = { invalid: true }; }
  return {
    name: app.getName(), version: app.getVersion(), platform: process.platform, arch: process.arch,
    osVersion: os.release(), electron: process.versions.electron, node: process.versions.node,
    isPackaged: app.isPackaged,
    revision: typeof build?.revision === "string" && /^[a-f0-9]{40}$/.test(build.revision) ? build.revision : null,
    builtAt: typeof build?.builtAt === "string" ? build.builtAt : null,
    modifiedSource: typeof build?.modifiedSource === "boolean" ? build.modifiedSource : null,
    buildMetadata: build?.invalid ? "invalid" : build ? "recorded" : "not_recorded",
  };
}

function getClientSupportHost() {
  if (!clientSupportHost) clientSupportHost = createClientSupportHost({
    app, BrowserWindow, ipcMain, dialog, clipboard, shell,
    clientRoot: clientProfileRoot, application: clientApplicationInfo,
    accessStatus: (token) => getClientAccessController().status(token),
    runtime: () => runtimeHost?.snapshot() ?? null,
    progress: () => latestSetupProgress,
    devUrl: process.env.VITE_DEV_SERVER_URL,
  });
  return clientSupportHost;
}
const MAX_POLICY_PROJECTION_BYTES = 16 * 1024 * 1024;
const WORKER_ENVIRONMENT_PANEL_KINDS = new Set([
  "browser",
  "connection",
  "container",
  "runtime",
  "worktree",
  "changes",
  "desktop",
  "custom",
  "overview",
]);

function clientLogPath() {
  return path.join(app.getPath("logs"), "octopus-client.jsonl");
}

async function appendClientLog(level, event, detail = {}) {
  const logPath = clientLogPath();
  await fs.promises.mkdir(path.dirname(logPath), { recursive: true });
  const stat = await fs.promises.stat(logPath).catch(() => null);
  if (stat && stat.size >= MAX_CLIENT_LOG_BYTES) {
    await fs.promises.rm(`${logPath}.1`, { force: true });
    await fs.promises.rename(logPath, `${logPath}.1`);
  }
  const record = {
    at: new Date().toISOString(),
    level,
    event,
    detail: detail && typeof detail === "object" && !Array.isArray(detail) ? detail : {},
  };
  await fs.promises.appendFile(logPath, `${JSON.stringify(record)}\n`, "utf8");
  return logPath;
}
function getClientAccessController() {
  if (!clientAccessController) {
    clientAccessController = createClientAccessController({
      administratorSecret: clientEnvironment.administratorSecret,
      credentialPath: stringValue(process.env.OCTOPUS_CLIENT_ACCESS_PATH)
        || path.join(resolveOctopusHome(), "client-access.json"),
      labSecret: clientEnvironment.labSecret,
    });
  }
  return clientAccessController;
}

function loadClientEnvironment() {
  const file = path.join(resolveOctopusHome(), ".env");
  let stat;
  try {
    stat = fs.lstatSync(file);
  } catch (error) {
    if (error?.code !== "ENOENT") throw new Error(`Client environment cannot be inspected: ${formatError(error)}`);
  }
  if (stat) {
    if (!stat.isFile() || stat.isSymbolicLink()) throw new Error("Client environment path is not a regular file");
    if (stat.size <= 0 || stat.size > 64 * 1024) throw new Error("Client environment file has an invalid size");
    if (process.platform !== "win32" && (stat.mode & 0o077) !== 0) {
      throw new Error("Client environment file permissions are too broad; expected mode 0600");
    }
    process.loadEnvFile(file);
  }
  const administratorSecret = String(process.env.OCTOPUS_CLIENT_ADMIN_SECRET ?? "");
  const labSecret = String(process.env.OCTOPUS_CLIENT_LAB_SECRET ?? "");
  delete process.env.OCTOPUS_CLIENT_ADMIN_SECRET;
  delete process.env.OCTOPUS_CLIENT_LAB_SECRET;
  return { administratorSecret, labSecret };
}

function touchIdStatus() {
  const prompted = readTouchIdPreference();
  if (!touchIdCapabilityAvailable()) {
    return { available: false, enrolled: false, prompted };
  }
  const enrollment = readTouchIdEnrollmentRecord();
  if (!enrollment) return { available: true, enrolled: false, prompted };
  let fingerprint;
  try {
    fingerprint = getClientAccessController().credentialFingerprint();
  } catch {
    return { available: true, enrolled: false, prompted };
  }
  return {
    available: true,
    enrolled: Boolean(enrollment && fingerprint && enrollment.credentialFingerprint === fingerprint),
    prompted,
  };
}

function touchIdCapabilityAvailable() {
  return process.platform === "darwin" && typeof systemPreferences.promptTouchID === "function";
}

function readTouchIdPreference() {
  let stat;
  try {
    stat = fs.lstatSync(touchIdPreferencePath);
  } catch (error) {
    if (error && error.code === "ENOENT") return false;
    throw new Error(`Touch ID preference cannot be inspected: ${formatError(error)}`);
  }
  if (!stat.isFile() || stat.isSymbolicLink() || stat.size <= 0 || stat.size > 4 * 1024) {
    throw new Error("Touch ID preference is not a valid regular file.");
  }
  if (process.platform !== "win32" && (stat.mode & 0o077) !== 0) {
    throw new Error("Touch ID preference permissions are too broad; expected mode 0600.");
  }
  let value;
  try {
    value = JSON.parse(fs.readFileSync(touchIdPreferencePath, "utf8"));
  } catch (error) {
    throw new Error(`Touch ID preference is not valid JSON: ${formatError(error)}`);
  }
  if (value?.version !== 1 || value.prompted !== true) {
    throw new Error("Touch ID preference has an unsupported shape.");
  }
  return true;
}

function writeTouchIdPreference() {
  const temporary = `${touchIdPreferencePath}.${process.pid}.${randomBytes(6).toString("hex")}.tmp`;
  fs.writeFileSync(temporary, `${JSON.stringify({ version: 1, prompted: true })}\n`, { mode: 0o600, flag: "wx" });
  fs.renameSync(temporary, touchIdPreferencePath);
  fs.chmodSync(touchIdPreferencePath, 0o600);
}

function readTouchIdEnrollmentRecord() {
  let stat;
  try {
    stat = fs.lstatSync(touchIdEnrollmentPath);
  } catch (error) {
    if (error && error.code === "ENOENT") return null;
    return null;
  }
  if (!stat.isFile() || stat.isSymbolicLink() || stat.size <= 0 || stat.size > 16 * 1024) return null;
  if (process.platform !== "win32" && (stat.mode & 0o077) !== 0) return null;
  try {
    const value = JSON.parse(fs.readFileSync(touchIdEnrollmentPath, "utf8"));
    if (value?.version !== 1
      || typeof value.credentialFingerprint !== "string"
      || !/^[a-f0-9]{64}$/.test(value.credentialFingerprint)
      || typeof value.sealedVerifier !== "string") return null;
    const sealed = Buffer.from(value.sealedVerifier, "base64");
    if (!sealed.length || sealed.toString("base64") !== value.sealedVerifier) return null;
    return {
      credentialFingerprint: value.credentialFingerprint,
      sealedVerifier: value.sealedVerifier,
    };
  } catch {
    return null;
  }
}

function verifyTouchIdEnrollment(record) {
  if (!record || !safeStorage.isEncryptionAvailable()) {
    throw new Error("Touch ID secure storage is unavailable on this device.");
  }
  let verifier;
  try {
    verifier = safeStorage.decryptString(Buffer.from(record.sealedVerifier, "base64"));
  } catch {
    throw new Error("Touch ID enrollment could not be verified.");
  }
  if (!/^[a-f0-9]{64}$/.test(verifier)) {
    throw new Error("Touch ID enrollment could not be verified.");
  }
}

function writeTouchIdEnrollment() {
  if (process.platform !== "darwin" || !safeStorage.isEncryptionAvailable()) {
    throw new Error("Touch ID is unavailable on this device.");
  }
  const fingerprint = getClientAccessController().credentialFingerprint();
  if (!fingerprint) throw new Error("Configure the Administrator password before enabling Touch ID.");
  const sealedVerifier = safeStorage.encryptString(randomBytes(32).toString("hex")).toString("base64");
  const temporary = `${touchIdEnrollmentPath}.${process.pid}.${randomBytes(6).toString("hex")}.tmp`;
  fs.writeFileSync(temporary, `${JSON.stringify({ version: 1, credentialFingerprint: fingerprint, sealedVerifier })}\n`, { mode: 0o600, flag: "wx" });
  fs.renameSync(temporary, touchIdEnrollmentPath);
  fs.chmodSync(touchIdEnrollmentPath, 0o600);
}

function clearTouchIdEnrollment() {
  try {
    const stat = fs.lstatSync(touchIdEnrollmentPath);
    if (stat.isFile() && !stat.isSymbolicLink()) fs.unlinkSync(touchIdEnrollmentPath);
  } catch (error) {
    if (!error || error.code !== "ENOENT") throw error;
  }
}

async function promptTouchId(reason) {
  if (!touchIdCapabilityAvailable()) {
    throw new Error("Touch ID is unavailable on this device.");
  }
  await systemPreferences.promptTouchID(reason);
}

function getProjectionStore(profile) {
  if (!projectionStores.has(profile)) {
    projectionStores.set(profile, createClientProjectionStore(profile, {
      resolveRepoRoot,
      sourceRegistry: resolveSourceRegistry,
      localPackageRegistry: resolveLocalPackageRegistry,
      localPackageLibrary: resolveLocalPackageLibrary,
      octopusHome: resolveOctopusHome,
      beforeRead: profile === "live" ? prepareLiveProjectionRead : undefined,
      localPackagePublication: profile === "live" ? () => workspaceStorage(resolveOctopusHome()) : undefined,
      inspectWorkspaceSetupState: profile === "live" ? inspectWorkspaceSetupState : undefined,
      nativeBrowserTargetsForProjection: profile === "live" ? nativeBrowserTargetsForProjection : undefined,
      nativeBrowserDownloadsForProjection: profile === "live" ? nativeBrowserDownloadsForProjection : undefined,
      nativeDesktopPresentationsForProjection: profile === "live" ? id => (nativeDesktopPresentations?.snapshot() ?? []).filter(value => value.worker_environment_id === id) : undefined,
      nativeBrowserPresentationsForProjection: profile === "live" ? id => (nativeBrowserPresentations?.snapshot() ?? []).filter(value => value.worker_environment_id === id) : undefined,
      projectEePolicies: profile === "live" ? projectEePolicies : undefined,
      onChange: (generation, kind) => {
        for (const win of BrowserWindow.getAllWindows()) {
          if (!win.isDestroyed()) {
            win.webContents.send("octopus:projection-changed", {
              generation,
              kind: ["ledger", "runs", "files"].includes(kind) ? kind : "structure",
            });
          }
        }
        if (profile === "live") scheduleNativeDesktopPresentations();
        if (profile === "live" && kind !== "ledger") {
          void cdpViewerManager?.revalidate();
          void nativeBrowserSurface?.revalidate();
          void processViewerManager?.revalidate();
        }
      },
    }));
  }
  return projectionStores.get(profile);
}

async function prepareLiveProjectionRead() {
  await initializeRuntimeHost();
}

async function taskRetirementBarrier(octopusHome) {
  if (resolveOctopusHome() !== octopusHome) throw new Error("Home changed during task retirement");
  const client = await getOperatorChatClientLoader().load();
  return (await client.loadOperatorTaskLifecycle({
    registryRoot: () => path.join(octopusHome, "registry"), installationRoot: resolveInstallationRoot,
  })).retirementBarrier;
}

function readWorkspaceSetupState(options) {
  const octopusHome = options.octopusHomeRoot;
  return readWorkspaceSetupStateTransaction({ ...options,
    publication: workspaceStorage(octopusHome), retirementBarrier: {
    verifyBefore: (context) => taskRetirementBarrier(octopusHome).then((barrier) => barrier.verifyBefore(context)),
    withHeldProof: (context, operation) => taskRetirementBarrier(octopusHome).then((barrier) => barrier.withHeldProof(context, operation)),
  } });
}

function commitProjectionClientAuthoring(octopusHome, registryRoot, request) {
  return commitProjectionClientAuthoringTransaction(octopusHome, registryRoot, request, {
    publication: workspaceStorage(octopusHome),
    retirementBarrier: {
      verifyBefore: (context) => taskRetirementBarrier(octopusHome).then((barrier) => barrier.verifyBefore(context)),
      withHeldProof: (context, operation) => taskRetirementBarrier(octopusHome).then((barrier) => barrier.withHeldProof(context, operation)),
    },
  });
}

function workspaceStorage(octopusHome) {
  const publisher = require("./packageStorage.cjs").createPackageStoragePublisher(resolveLocalPackageRegistry(), resolveInstallationRoot());
  return Object.freeze(Object.fromEntries(["publishFiles", "snapshotDirectory", "publishDirectoryMoves", "reclaimDirectory"].map(method =>
    [method, async (...args) => {
      if (resolveOctopusHome() !== octopusHome) throw new Error("Home changed during workspace publication");
      return publisher[method](...args);
    }])));
}

function getCdpViewerManager() {
  if (!cdpViewerManager) {
    cdpViewerManager = createCdpViewerManager({ resolveSurface: resolveProjectedCdpSurface });
  }
  return cdpViewerManager;
}

function requireNativeBrowserHost() {
  if (nativeBrowserHome && nativeBrowserHome !== resolveOctopusHome()) throw new Error("The native browser host belongs to another Octopus Home. Restart the client.");
  if (!nativeBrowserHost) throw nativeBrowserStartupError ?? new Error("The native browser host is not available. Complete browser package setup and restart the client.");
  return nativeBrowserHost;
}

function requireNativeBrowserHome(home) {
  if (typeof home !== "string" || home !== nativeBrowserHome || home !== resolveOctopusHome()) throw new Error("Native browser page belongs to another Octopus Home.");
}

function nativeBrowserTargetsForProjection(projection) {
  if (projection?.octopusHome !== nativeBrowserHome || projection.octopusHome !== resolveOctopusHome()) throw new Error("Native browser observation belongs to another Octopus Home.");
  return requireNativeBrowserHost().listTargetsForProjection({
    protocol: "browser.native.v1",
    worker_environment_id: projection.worker_environment_id,
    formation_pin: projection.formation_pin,
    engine_handle: projection.engine_handle,
    host_generation: projection.host_generation,
    browser_context_id: projection.browser_context_id,
  });
}

async function nativeBrowserDownloadsForProjection(projection) {
  if (projection?.octopusHome !== nativeBrowserHome || projection.octopusHome !== resolveOctopusHome()) throw new Error("Native browser history belongs to another Octopus Home.");
  const host = requireNativeBrowserHost();
  const history = await host.listDownloadHistoryForProjection({ worker_environment_id: projection.worker_environment_id, formation_pin: projection.formation_pin });
  return { ...history, engine: host.getEngineInfo() };
}

function getNativeBrowserSurface() {
  if (!nativeBrowserSurface) nativeBrowserSurface = createNativeBrowserSurfaceAdapter({
    getHost: requireNativeBrowserHost,
    getHome: resolveOctopusHome,
    requireHome: requireNativeBrowserHome,
    resolveSurface: resolveProjectedWorkerEnvironmentSurface,
    resolveEnvironments: async () => {
      const result = await getProjectionStore("live").read(projectionReadRequest("projection.worker_environments.list", { all_scopes: true, include_archived: true }));
      if (!Array.isArray(result?.worker_environments)) throw new Error("Native browser formation projection is incomplete.");
      return result.worker_environments;
    },
    windowForSender: (sender) => BrowserWindow.fromWebContents(sender),
    chooseDownloadDirectory: async (window) => {
      const result = await dialog.showOpenDialog(window, { properties: ["openDirectory", "createDirectory"], title: "Choose Downloads Folder" });
      return result.canceled ? null : result.filePaths[0] ?? null;
    },
    requireAccess: (token) => getClientAccessController().requireSession(token, ["live"]),
    reportError: (error) => { console.error("Native browser presentation failed:", error.message); },
  });
  return nativeBrowserSurface;
}

function guardNativeBrowserPackage() {
  if (!nativeBrowserMutationGuard) nativeBrowserMutationGuard = registerLocalPackageMutationGuard({ stateRoot: clientSetupPackageRoots().stateRoot, guard: (ids) => {
    if (nativeBrowserModuleLoaded && ids.includes("driver.browser_session")) throw new Error("Restart the client before changing the loaded native browser package.");
  } });
}

function requireNativeBrowserPackageOwner(installation) {
  const owners = installation.member_states?.filter((member) => member.registry_kind === "worker_environment_kind" && member.owner === "browser_session") ?? [];
  const [owner] = owners;
  if (!installation.available || owners.length !== 1 || !owner.materialized) {
    throw new Error("The installed browser owner is missing. Restore the browser package before native browser use.");
  }
}

async function startNativeBrowserEngine() {
  if (nativeBrowserHome && nativeBrowserHome !== resolveOctopusHome()) throw new Error("Restart the client before changing the native browser Home.");
  if (nativeBrowserHost) return;
  if (nativeBrowserStartup) return nativeBrowserStartup;
  nativeBrowserHome = resolveOctopusHome();
  const roots = clientSetupPackageRoots();
  guardNativeBrowserPackage();
  nativeBrowserStartup = (async () => {
    await app.whenReady();
    await withLocalPackageInstallation({ ...roots, packageId: "driver.browser_session" }, async (installation) => {
      requireNativeBrowserPackageOwner(installation);
      const readConfig = async (file) => {
        const stat = await fs.promises.lstat(file);
        if (!stat.isFile() || stat.isSymbolicLink() || stat.size > 2 * 1024 * 1024) throw new Error("Native browser setup requires a regular bounded configuration file.");
        return JSON.parse(await fs.promises.readFile(file, "utf8"));
      };
      const [machine, door] = await Promise.all([
        readConfig(path.join(nativeBrowserHome, "host.json")),
        readConfig(path.join(roots.activeRegistryRoot, "interface", "system_browser", "interface.json")),
      ]);
      const config = door.interface_driver_config;
      if (door.id !== "system_browser" || config?.private_config?.browser_room?.transport?.wire !== "browser.native.v1"
        || config.private_config.browser_room.transport.host_root_slot !== "browser_native_host_root") throw new Error("The installed browser door does not declare the native host transport.");
      const overrides = config.host_slots;
      const hasOverride = overrides && Object.prototype.hasOwnProperty.call(overrides, "browser_native_host_root");
      const hostRoot = hasOverride ? overrides.browser_native_host_root?.path : machine.root_refs?.browser_native_host_root;
      if (typeof hostRoot !== "string" || !path.isAbsolute(hostRoot) || path.resolve(hostRoot) !== hostRoot) throw new Error("Browser setup requires an explicit canonical absolute browser_native_host_root in host.json or the browser door's host_slots.");
      const modulePath = path.join(roots.activeRegistryRoot, "worker_environment_kind", "browser_session", "packages", "octopus-browser-session-env", "native", "electron-host.cjs");
      if (await fs.promises.realpath(modulePath) !== modulePath) throw new Error("The installed native browser module path must not contain symbolic links.");
      const { verifyBrowserEngine } = require(path.join(path.dirname(modulePath), "engine-mode.cjs"));
      const browserEngine = verifyBrowserEngine({ resourcesRoot: process.resourcesPath, executablePath: process.execPath,
        isPackaged: app.isPackaged, architecture: process.arch, nodeVersion: process.versions.node, electronVersion: process.versions.electron });
      if (nativeBrowserHome !== resolveOctopusHome()) throw new Error("Browser Home changed during packaged engine verification.");
      const { createBrowserEngineHost } = require(modulePath);
      if (typeof createBrowserEngineHost !== "function") throw new Error("The installed browser kind has no native engine host export.");
      nativeBrowserModuleLoaded = true;
      const host = createBrowserEngineHost({ electron: require("electron"), hostRoot, engineMode: browserEngine.mode, engineProof: browserEngine.engineProof, descriptorHash: (bytes) => Buffer.from(require("@noble/hashes/blake3").blake3(bytes)).toString("hex"), onEvent: (event) => {
        if (["targets", "downloads", "human-download", "download-history-error"].includes(event.type)) projectionStores.get("live")?.invalidate();
        else getNativeBrowserSurface().onEvent(event);
      } });
      try { await host.start(); } catch (error) { await host.stop().catch(() => {}); throw error; }
      nativeBrowserHost = host;
      nativeBrowserStartupError = null;
    });
  })().catch((error) => { nativeBrowserStartupError = error; throw error; }).finally(() => {
    if (!nativeBrowserHost) nativeBrowserStartup = null;
  });
  return nativeBrowserStartup;
}

async function planBrowserNativeHost(request) {
  await initializeRuntimeHost();
  const roots = clientSetupPackageRoots();
  guardNativeBrowserPackage();
  return withLocalPackageInstallation({ ...roots, packageId: "driver.browser_session" }, async (installation) => {
    requireNativeBrowserPackageOwner(installation);
    const modulePath = path.join(roots.activeRegistryRoot, "worker_environment_kind", "browser_session", "packages", "octopus-browser-session-env", "native", "setup-host.cjs");
    if (await fs.promises.realpath(modulePath) !== modulePath) throw new Error("The installed browser setup module path must not contain symbolic links.");
    const helper = require(modulePath);
    if (typeof helper.planBrowserNativeHost !== "function") throw new Error("The installed browser kind has no native host setup planner.");
    nativeBrowserModuleLoaded = true;
    return helper.planBrowserNativeHost(request);
  });
}

let desktopPreferencesSetupGuard = null;
let desktopPreferencesSetupLoaded = false;
async function planDesktopApplicationPreferences(request) {
  await initializeRuntimeHost();
  const roots = clientSetupPackageRoots(), home = resolveOctopusHome();
  if (request.octopusHomeRoot !== home) throw new Error("Computer Use setup belongs to another Home.");
  if (!desktopPreferencesSetupGuard) desktopPreferencesSetupGuard = registerLocalPackageMutationGuard({ stateRoot: roots.stateRoot,
    guard: (ids) => { if (desktopPreferencesSetupLoaded && ids.includes("driver.desktop_session")) throw new Error("Restart the client before changing the loaded Computer Use setup package."); } });
  return withLocalPackageInstallation({ ...roots, packageId: "driver.desktop_session" }, async (installation) => {
    const owner = installation.member_states?.find((member) => member.registry_kind === "worker_environment_kind" && member.owner === "desktop_session");
    if (!installation.available || !installation.installed || installation.receipt_state !== "valid" || !/^[a-f0-9]{64}$/.test(installation.receipt_digest)
      || !owner?.materialized) throw new Error("Computer Use setup requires its current installed package and valid receipt.");
    const modulePath = path.join(roots.activeRegistryRoot, "worker_environment_kind", "desktop_session", "packages", "octopus-system-gui-env", "native", "setup-preferences.cjs");
    if (await fs.promises.realpath(modulePath) !== modulePath) throw new Error("The installed Computer Use setup module must not contain symbolic links.");
    const helper = require(modulePath);
    desktopPreferencesSetupLoaded = true;
    if (typeof helper.planDesktopApplicationPreferences !== "function") throw new Error("The installed Computer Use package has no preference setup planner.");
    if (resolveOctopusHome() !== home) throw new Error("Home changed during Computer Use setup verification.");
    const plan = await helper.planDesktopApplicationPreferences(request);
    if (resolveOctopusHome() !== home) { await plan.rollback(); throw new Error("Home changed during Computer Use preference setup."); }
    return plan;
  });
}

async function retryNativeBrowserAfterSetup() {
  try {
    await startNativeBrowserEngine();
    if (nativeBrowserHost) void getNativeBrowserSurface().revalidate();
  } catch (error) {
    await appendClientLog("error", "browser.native_host.unavailable", { message: error.message });
  }
}

function getDesktopSurfaceHost(sender) {
  if (!desktopSurfaceHost) {
    desktopSurfaceHost = createDesktopSurfaceHost({
      resolveSurface: resolveProjectedWorkerEnvironmentSurface,
      resolvePreferenceEnvironments: async () => {
        const result = await getProjectionStore("live").read(projectionReadRequest("projection.worker_environments.list", { all_scopes: true, include_archived: false }));
        if (!Array.isArray(result?.worker_environments)) throw new Error("Saved application host projection is unavailable.");
        return result.worker_environments;
      },
      withDefaultPreferenceDriver: async operation => {
        await initializeRuntimeHost();
        return withLocalPackageInstallation({ ...clientSetupPackageRoots(), packageId: "driver.desktop_session" }, async installation => {
          if (!installation.available || !installation.installed || installation.receipt_state !== "valid" || !/^[a-f0-9]{64}$/.test(installation.receipt_digest)) throw new Error("Saved application permissions require the verified installed Computer Use driver.");
          for (const [kind, id] of [["interface", "system_gui"], ["worker_environment_kind", "desktop_session"]]) {
            const member = installation.member_states?.find(value => value.registry_kind === kind && value.owner === id);
            if (!member?.materialized) throw new Error("The default Computer Use driver owner is missing.");
          }
          return operation("system_gui");
        });
      },
      homePath: resolveOctopusHome,
      environment: () => process.env,
    });
  }
  if (sender && !desktopSurfaceSenders.has(sender)) {
    desktopSurfaceSenders.add(sender);
    const ownerId = sender.id;
    const host = desktopSurfaceHost;
    const report = (error) => console.error("Desktop viewer cleanup failed:", error);
    sender.once("destroyed", () => { nativeDesktopPresentations?.closeViewsOwner(ownerId); nativeDesktopPermissions?.closeOwner(ownerId); nativeDesktopSettings?.closeOwner(ownerId); void host.closeOwner(ownerId).catch(report); });
    sender.on("render-process-gone", () => { nativeDesktopPresentations?.closeViewsOwner(ownerId); nativeDesktopPermissions?.closeOwner(ownerId); nativeDesktopSettings?.closeOwner(ownerId); void host.pauseOwner(ownerId).catch(report); });
    sender.on("did-start-navigation", (_event, _url, isInPlace, isMainFrame) => {
      if (isMainFrame && !isInPlace) { nativeDesktopPresentations?.closeViewsOwner(ownerId); nativeDesktopPermissions?.closeOwner(ownerId); nativeDesktopSettings?.closeOwner(ownerId); void host.pauseOwner(ownerId).catch(report); }
    });
  }
  return desktopSurfaceHost;
}

function getNativeDesktopPermissions() {
  if (!nativeDesktopPermissions) nativeDesktopPermissions = createDesktopPermissionHost({
    prepareSource: (input, owner) => input.setup === true ? getNativeDesktopSettings().source(owner) : getDesktopSurfaceHost().preparePermissionSource(input),
    automationSources: (input, helper) => getNativeDesktopPresentations().permissionSources(input, helper),
    getNativeHost: () => getNativeClientPresentations().get(), homePath: resolveOctopusHome,
  });
  return nativeDesktopPermissions;
}
function getNativeDesktopSettings() {
  if (!desktopPreferencesSetupGuard) desktopPreferencesSetupGuard = registerLocalPackageMutationGuard({ stateRoot: clientSetupPackageRoots().stateRoot,
    guard: ids => { if (desktopPreferencesSetupLoaded && ids.includes("driver.desktop_session")) throw new Error("Restart the client before changing the loaded Computer Use Settings package."); } });
  if (!nativeDesktopSettings) nativeDesktopSettings = createNativeDesktopSettingsHost({
    homePath: resolveOctopusHome,
    prepareHelper: async () => {
      await initializeRuntimeHost();
      const home = resolveOctopusHome(), roots = clientSetupPackageRoots();
      const read = operation => withLocalPackageInstallation({ ...roots, packageId: "driver.desktop_session" }, async installation => {
        const member = installation.member_states?.find(value => value.registry_kind === "worker_environment_kind" && value.owner === "desktop_session");
        const descriptor = installation.member_states?.find(value => value.registry_kind === "interface" && value.owner === "system_gui");
        if (resolveOctopusHome() !== home || !installation.available || !installation.installed || installation.receipt_state !== "valid"
          || !/^[a-f0-9]{64}$/.test(installation.receipt_digest) || !member?.materialized || !descriptor?.materialized) throw new Error("Computer Use setup requires its current verified installed driver.");
        return operation(installation.receipt_digest, member.installed_digest);
      });
      const preferences = await getDesktopSurfaceHost().defaultPreferenceStore(() => {
        if (home !== resolveOctopusHome()) throw new Error("Computer Use Settings Home changed.");
      });
      if (typeof preferences.identity !== "string" || !preferences.identity || typeof preferences.generation !== "string" || !preferences.generation) throw new Error("Computer Use preference store has no captured identity.");
      return read(async (receipt, digest) => {
        const configPath = path.join(roots.activeRegistryRoot, "interface", "system_gui", "desktop.json");
        if (await fs.promises.realpath(configPath) !== configPath) throw new Error("Computer Use configuration must be canonical.");
        const config = JSON.parse(await fs.promises.readFile(configPath, "utf8"));
        const relative = config.host?.native_program;
        if (config.schema_version !== 1 || typeof relative !== "string" || relative.split("/").some(part => !part || part === "." || part === "..")
          || !relative.startsWith("desktop_session/packages/") || !Number.isSafeInteger(config.request_timeout_ms)
          || config.request_timeout_ms <= 0 || config.request_timeout_ms > 600000) throw new Error("Computer Use setup has no exact installed native helper.");
        const program = path.join(roots.activeRegistryRoot, "worker_environment_kind", relative);
        const stat = await fs.promises.lstat(program);
        if (!stat.isFile() || stat.isSymbolicLink() || stat.size < 1 || stat.size > 64 * 1024 * 1024 || !(stat.mode & 0o111) || await fs.promises.realpath(program) !== program) throw new Error("Computer Use setup helper is not the installed executable.");
        const modulePath = path.join(roots.activeRegistryRoot, "worker_environment_kind", "desktop_session", "packages", "octopus-system-gui-env", "native", "setup-preferences.cjs");
        if (await fs.promises.realpath(modulePath) !== modulePath) throw new Error("Computer Use preference module must be canonical.");
        const lifePath = path.join(roots.activeRegistryRoot, "interface", "system_gui", "life.json");
        const lifeStat = await fs.promises.lstat(lifePath);
        if (!lifeStat.isFile() || lifeStat.isSymbolicLink() || lifeStat.size > 1024 * 1024 || await fs.promises.realpath(lifePath) !== lifePath) throw new Error("Computer Use Settings has no canonical installed lifecycle.");
        const lifeBytes = await fs.promises.readFile(lifePath), life = JSON.parse(lifeBytes.toString("utf8"));
        if (lifeBytes.length > 1024 * 1024 || !Array.isArray(life.processes)) throw new Error("Computer Use Settings lifecycle is malformed.");
        const hosts = life.processes.filter(value => typeof value.bin === "string" && path.basename(value.bin) === "octopus-desktop-host");
        const host = hosts.length === 1 ? hosts[0] : null;
        const hostPath = host?.bin?.startsWith("owner://") ? host.bin.slice("owner://".length) : null;
        if (!hostPath || hostPath.split("/").some(value => !value || value === "." || value === "..") || !Array.isArray(host.args) || host.args.length
          || !Array.isArray(host.inherit_env) || host.inherit_env.length) throw new Error("Computer Use Settings requires its unique installed host executable.");
        const settingsProgram = path.join(roots.activeRegistryRoot, "interface", "system_gui", hostPath);
        const settingsStat = await fs.promises.lstat(settingsProgram);
        if (!settingsStat.isFile() || settingsStat.isSymbolicLink() || settingsStat.size < 1 || settingsStat.size > 64 * 1024 * 1024
          || !(settingsStat.mode & 0o111) || await fs.promises.realpath(settingsProgram) !== settingsProgram) throw new Error("Computer Use Settings reader is not its installed executable.");
        const helper = require(modulePath); desktopPreferencesSetupLoaded = true;
        if (typeof helper.readComputerUsePreference !== "function" || typeof helper.writeComputerUsePreference !== "function") throw new Error("The installed Computer Use preference module is incomplete.");
        return { home, program, settingsProgram, preferenceRoot: preferences.root,
          applicationIdentity: JSON.stringify([receipt,digest,preferences.identity,preferences.generation,settingsProgram]),
          directory: path.dirname(program), timeout: config.request_timeout_ms,
          preference: async enabled => {
            await preferences.current();
            const value = enabled === undefined ? await helper.readComputerUsePreference(preferences.root) : await helper.writeComputerUsePreference(preferences.root, enabled);
            await preferences.current(); return value;
          },
          current: async () => {
            await preferences.current();
            return read((nextReceipt, nextDigest) => {
              if (receipt !== nextReceipt || digest !== nextDigest) throw new Error("Computer Use installed helper changed during setup.");
            });
          } };
      });
    },
  });
  return nativeDesktopSettings;
}
function nativeDesktopAccessEntries() {
  return clientWindows.entries().filter((entry) => {
    if (!entry.ready || entry.win.isDestroyed() || entry.win.webContents.isDestroyed()) return false;
    try { return getClientAccessController().requireSession(entry.accessToken, ["live"]).profile === "live"; }
    catch { return false; }
  });
}
function requireNativeDesktopAccess() {
  if (!nativeDesktopAccessEntries().length) throw new Error("Native presentation requires a current authenticated live client window.");
}
function getNativeClientPresentations() {
  if (!nativeClientPresentations) nativeClientPresentations = createClientPresentationHost({
    artifactOptions: () => ({ resourcesRoot: process.resourcesPath, appRoot: app.getAppPath(), isPackaged: app.isPackaged,
      architecture: process.arch, nodeVersion: process.versions.node, electronVersion: process.versions.electron }),
    homePath: resolveOctopusHome, requireAccess: requireNativeDesktopAccess, syncHosts: syncNativeDesktopHosts,
    onEvent: event => {
      nativeDesktopPresentations?.onEvent(event);
      void nativeDesktopPermissions?.onEvent(event).catch(error => appendClientLog("error", "client.native_permissions.failed", { message: error.message }));
      void nativeBrowserPresentations?.onEvent(event).catch(error => appendClientLog("error", "browser.native_preview.focus_failed", { message: error.message }));
      if (event.type === "return-to-main") scheduleNativeDesktopPresentations();
    },
  });
  return nativeClientPresentations;
}
function syncNativeDesktopHosts(host) {
  const entries = nativeDesktopAccessEntries(), current = new Set(entries.map(entry => entry.webContentsId));
  for (const [id, value] of nativeDesktopWindowHosts) if (!current.has(id)) {
    host.removeHost({ hostId: value.hostId, generation: value.generation }); nativeDesktopWindowHosts.delete(id);
  }
  for (const entry of entries) {
    let identity = nativeDesktopWindowHosts.get(entry.webContentsId);
    if (!identity) {
      identity = { hostId: entry.bootstrap.windowId, generation: randomUUID() }; nativeDesktopWindowHosts.set(entry.webContentsId, identity);
      const refresh = () => { if (!quitCleanupStarted) scheduleNativeDesktopPresentations(); };
      for (const event of ["resize", "move", "show", "hide", "focus", "closed"]) entry.win.on(event, refresh);
    }
    const [width, height] = entry.win.getContentSize();
    if (width <= 0 || height <= 0) continue;
    host.registerHost({ ...identity, windowHandle: entry.win.getNativeWindowHandle(), anchorContentRect: { x: 0, y: 0, width, height },
      passthroughRect: { x: 0, y: 0, width: 0, height: 0 }, anchors: [], scope: "all", role: "main", homeAvailable: true, animated: true });
  }
}
function getNativeDesktopPresentations() {
  if (!nativeDesktopPresentations) nativeDesktopPresentations = createDesktopPresentationHost({
    getNativeHost: () => getNativeClientPresentations().get(), homePath: resolveOctopusHome, requireAccess: requireNativeDesktopAccess,
    prepareSource: input => getDesktopSurfaceHost().preparePresentationSource(input), syncHosts: syncNativeDesktopHosts,
    onChange: () => projectionStores.get("live")?.invalidate(),
  });
  return nativeDesktopPresentations;
}
async function readNativeBrowserToolDescriptor(id) {
  const home = resolveOctopusHome(); requireNativeBrowserHome(home); requireNativeDesktopAccess();
  if (typeof id !== "string" || !id || id.trim() !== id || id === "." || id === ".." || /[\x00-\x1f/\\]/.test(id)) throw new Error("Invalid exact browser tool descriptor identity.");
  const filename = path.join(home, "registry", "tool", id, "tool.json");
  if (await fs.promises.realpath(filename) !== filename) throw new Error("Browser tool descriptor must not contain symbolic links.");
  const handle = await fs.promises.open(filename, fs.constants.O_RDONLY | fs.constants.O_NOFOLLOW);
  try {
    const stat = await handle.stat();
    if (!stat.isFile() || stat.size > 1024 * 1024) throw new Error("Browser tool descriptor is not a bounded regular file.");
    const bytes = Buffer.alloc(1024 * 1024 + 1); let length = 0;
    while (length < bytes.length) {
      const read = await handle.read(bytes, length, bytes.length - length, length);
      if (read.bytesRead === 0) break;
      length += read.bytesRead;
    }
    if (length > 1024 * 1024) throw new Error("Browser tool descriptor exceeds its byte bound.");
    requireNativeBrowserHome(home); requireNativeDesktopAccess(); return JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(bytes.subarray(0, length)));
  } finally { await handle.close(); }
}
async function currentNativeBrowserFormedRoom(expected) {
  const home = resolveOctopusHome(); requireNativeBrowserHome(home); requireNativeDesktopAccess();
  const result = await getProjectionStore("live").read(projectionReadRequest("projection.worker_environments.list", { all_scopes: true, include_archived: false }));
  requireNativeBrowserHome(home); requireNativeDesktopAccess();
  const room = result.worker_environments?.find(value => value.worker_environment_id === expected.worker_environment_id);
  if (!room || room.worker_environment_kind !== "browser_session" || room.run_id !== expected.run_id || room.door !== expected.door || room.formation_pin !== expected.formation_pin) throw new Error("Browser result formed owner changed.");
  return room;
}
async function currentNativeBrowserPreviewTarget(record) {
  requireNativeDesktopAccess(); requireNativeBrowserHome(resolveOctopusHome());
  const home = resolveOctopusHome();
  const surface = await resolveProjectedWorkerEnvironmentSurface(record.input.workerEnvironmentId, record.input.surfaceId);
  requireNativeDesktopAccess(); requireNativeBrowserHome(home);
  if (surface.environmentKind !== "browser_session" || surface.protocol !== "browser.native.v1" || surface.mode !== "direct"
    || surface.runId !== record.input.runId || surface.formationPin !== record.projection.formation_pin
    || ["engine_handle", "host_generation", "browser_context_id", "target_id"].some(key => surface.data[key] !== record.projection[key])) throw new Error("Browser preview no longer identifies its exact current target.");
  requireNativeBrowserHost().getTarget(record.projection);
  const status = await getProjectionStore("live").read(projectionReadRequest("projection.operator.chats.status", { run_ids: [record.input.runId] }));
  requireNativeDesktopAccess(); requireNativeBrowserHome(home);
  const turn = status.chats?.find(value => value.run_id === record.input.runId);
  if (!turn?.turn_open || turn.turn_opened_movement_seq !== record.batch || turn.last_movement_seq < record.sequence) throw new Error("Browser preview belongs to a completed or superseded turn.");
  requireNativeBrowserHost().getTarget(record.projection);
}
function getNativeBrowserPresentations() {
  if (!nativeBrowserPresentations) nativeBrowserPresentations = createBrowserPresentationHost({
    homePath: resolveOctopusHome, requireAccess: requireNativeDesktopAccess,
    getNativeHost: () => getNativeClientPresentations().get(), currentTarget: currentNativeBrowserPreviewTarget,
    readToolDescriptor: readNativeBrowserToolDescriptor, currentRoom: owner => currentNativeBrowserFormedRoom(owner.room),
    readActivity: (run, after) => getProjectionStore("live").read(projectionReadRequest("projection.operator.run.activity", {
      run_id: run, after_seq: after, from_end: false, limit: 1000,
    })),
    readArtifact: async row => {
      const home = resolveOctopusHome(); requireNativeBrowserHome(home); requireNativeDesktopAccess();
      const artifact = await readContentAddressedArtifactPayload({ blobsRoot: path.join(home, "ledger", "blobs"),
        artifactId: row.artifact_id, manifestHash: row.content_hash, label: row.label, maxPayloadBytes: 96 * 1024 * 1024 });
      requireNativeBrowserHome(home); requireNativeDesktopAccess(); return artifact;
    },
    focusTarget: async record => {
      await currentNativeBrowserPreviewTarget(record);
      const entry = nativeDesktopAccessEntries().find(value => value.win.isFocused()) ?? nativeDesktopAccessEntries()[0];
      if (!entry) throw new Error("Browser preview has no current live client window.");
      entry.win.show(); entry.win.focus();
      requireNativeBrowserHost().requestPageFocus(record.projection);
    },
    onChange: () => projectionStores.get("live")?.invalidate(),
  });
  return nativeBrowserPresentations;
}
function getNativeBrowserDownloadHistory() {
  if (!nativeBrowserDownloadHistory) nativeBrowserDownloadHistory = createBrowserDownloadHistory({
    homePath: resolveOctopusHome, requireAccess: requireNativeDesktopAccess,
    readToolDescriptor: readNativeBrowserToolDescriptor,
    readActivity: (run, after) => getProjectionStore("live").read(projectionReadRequest("projection.operator.run.activity", { run_id: run, after_seq: after, from_end: false, limit: 1000 })),
    readArtifact: async row => {
      const home = resolveOctopusHome(); requireNativeBrowserHome(home); requireNativeDesktopAccess();
      const artifact = await readContentAddressedArtifactPayload({ blobsRoot: path.join(home, "ledger", "blobs"), artifactId: row.artifact_id,
        manifestHash: row.content_hash, label: row.label, maxPayloadBytes: 96 * 1024 * 1024 });
      requireNativeBrowserHome(home); requireNativeDesktopAccess(); return artifact;
    },
    currentRoom: async (owner, projection) => {
      const home = resolveOctopusHome(); requireNativeBrowserHome(home); requireNativeDesktopAccess();
      const surface = await resolveProjectedWorkerEnvironmentSurface(owner.room.worker_environment_id, owner.surface.id);
      requireNativeBrowserHome(home); requireNativeDesktopAccess();
      if (surface.environmentKind !== "browser_session" || surface.mode !== "direct" || surface.protocol !== "browser.native.downloads.v1"
        || surface.runId !== owner.room.run_id || surface.formationPin !== projection.formation_pin
        || surface.data.host_generation !== projection.host_generation) throw new Error("Download history source no longer belongs to this browser room.");
      return currentNativeBrowserFormedRoom(owner.room);
    },
    acceptArtifact: (projection, body, validateCurrent) => requireNativeBrowserHost().acceptDownloadArtifact(projection, body, validateCurrent),
  });
  return nativeBrowserDownloadHistory;
}
function scheduleNativeDesktopPresentations() {
  if (process.platform !== "darwin" || quitCleanupStarted || nativeDesktopRefreshScheduled) return;
  nativeDesktopRefreshScheduled = true;
  queueMicrotask(() => {
    nativeDesktopRefreshScheduled = false;
    if (nativeDesktopRefreshRunning) { nativeDesktopRefreshAgain = true; return; }
    nativeDesktopRefreshRunning = true;
    void (async () => {
      if (!nativeDesktopAccessEntries().length) { nativeBrowserPresentations?.revokeAll(); nativeBrowserDownloadHistory?.revokeAll(); nativeDesktopPresentations?.revokeAll(); return; }
      const home = resolveOctopusHome(), store = getProjectionStore("live");
      const result = await store.read(projectionReadRequest("projection.worker_environments.list", { all_scopes: true, include_archived: false }));
      if (home !== resolveOctopusHome()) throw new Error("Native desktop Home changed during source projection.");
      const candidates = [], browserRooms = [], historyRooms = [], runIds = new Set();
      for (const room of result.worker_environments ?? []) {
        if (room.worker_environment_kind === "browser_session" && nativeBrowserHost && home === nativeBrowserHome) {
          const history = room.client_surfaces?.find(value => value.protocol === "browser.native.downloads.v1" && value.mode === "direct");
          if (history && nativeBrowserHost.getEngineInfo().capabilities.persistentDownloadHistory) { historyRooms.push({ room, surface: history }); runIds.add(room.run_id); }
          const surface = room.client_surfaces?.find(value => ["browser.native.v1", "browser.native.downloads.v1"].includes(value.protocol)
            && value.mode === "direct" && value.data?.engine_handle && value.data?.browser_context_id);
          if (room.lifecycle === "active" && surface) { browserRooms.push({ room, surface }); runIds.add(room.run_id); }
        }
        if (room.lifecycle !== "active" || room.worker_environment_kind !== "desktop_session") continue;
        for (const surface of room.client_surfaces ?? []) {
          if (surface.protocol !== "desktop_session" || surface.mode !== "direct") continue;
          for (const source of surface.data?.desktop?.applications ?? []) for (const window of source.windows ?? []) {
            if (!window.source_image) continue;
            const input = { workerEnvironmentId: room.worker_environment_id, surfaceId: surface.id, runId: room.run_id,
              appInstanceId: source.application?.app?.app_instance_id, sourceWindowId: window.window_id };
            const descriptor = { runId: room.run_id, environmentKind: room.worker_environment_kind, protocol: surface.protocol, mode: surface.mode,
              lifecycle: room.lifecycle, formationPin: room.formation_pin, data: surface.data };
            try {
              const current = presentationSource(descriptor, input);
              candidates.push({ input, source: current.source, identity: current.identity }); runIds.add(room.run_id);
            } catch (error) {
              void appendClientLog("error", "desktop.native_presentation.source_unavailable", { workerEnvironmentId: room.worker_environment_id, sourceWindowId: window.window_id, message: error.message });
            }
          }
        }
      }
      for (const record of nativeDesktopPresentations?.snapshot() ?? []) runIds.add(record.run_id);
      for (const record of nativeBrowserPresentations?.snapshot() ?? []) runIds.add(record.run_id);
      if (!candidates.length && !nativeDesktopPresentations && !browserRooms.length && !historyRooms.length && !nativeBrowserPresentations && !nativeBrowserDownloadHistory) return;
      const status = await store.read(projectionReadRequest("projection.operator.chats.status", { run_ids: [...runIds] }));
      requireNativeDesktopAccess();
      if (home !== resolveOctopusHome()) throw new Error("Native desktop Home changed before source admission.");
      const currentBrowserRooms = [];
      for (const owner of browserRooms) {
        const { room, surface } = owner;
        const turn = status.chats.find(value => value.run_id === room.run_id);
        if (!turn || !Number.isSafeInteger(turn.turn_opened_movement_seq)) continue;
        try {
          requireNativeBrowserHome(home);
          await requireNativeBrowserHost().syncTurn({ protocol: "browser.native.v1", worker_environment_id: room.worker_environment_id,
            formation_pin: room.formation_pin, engine_handle: surface.data.engine_handle, host_generation: surface.data.host_generation,
            browser_context_id: surface.data.browser_context_id },
            { open: turn.turn_open, openingMovementSeq: turn.turn_opened_movement_seq, observedMovementSeq: turn.last_movement_seq });
          currentBrowserRooms.push(owner);
        } catch (error) {
          void appendClientLog("error", "browser.native_presentation.room_unavailable", { workerEnvironmentId: room.worker_environment_id, message: error.message });
        }
      }
      requireNativeDesktopAccess();
      if (home !== resolveOctopusHome()) throw new Error("Native presentation Home changed during room revalidation.");
      if (historyRooms.length || nativeBrowserDownloadHistory) {
        try { await getNativeBrowserDownloadHistory().reconcile(historyRooms, status.chats); }
        catch (error) { void appendClientLog("error", "browser.download_history.unavailable", { message: error.message }); }
      }
      if (currentBrowserRooms.length || nativeBrowserPresentations) {
        try { await getNativeBrowserPresentations().reconcile(currentBrowserRooms, status.chats); }
        catch (error) { void appendClientLog("error", "browser.native_presentation.unavailable", { message: error.message }); }
      }
      if (candidates.length || nativeDesktopPresentations) {
        try { await getNativeDesktopPresentations().reconcile(candidates, status.chats); }
        catch (error) { void appendClientLog("error", "desktop.native_presentation.unavailable", { message: error.message }); }
      }
      nativeClientPresentations?.sync(status.chats);
    })().catch(error => {
      if (!nativeDesktopAccessEntries().length || nativeClientPresentations?.home() && nativeClientPresentations.home() !== resolveOctopusHome()) { nativeBrowserPresentations?.revokeAll(); nativeBrowserDownloadHistory?.revokeAll(); nativeDesktopPresentations?.revokeAll(); }
      void appendClientLog("error", "desktop.native_presentation.unavailable", { message: error.message });
    }).finally(() => {
      nativeDesktopRefreshRunning = false;
      if (nativeDesktopRefreshAgain) { nativeDesktopRefreshAgain = false; scheduleNativeDesktopPresentations(); }
    });
  });
}

function getProcessViewerManager() {
  if (!processViewerManager) {
    processViewerManager = createProcessViewerManager({ resolveSurface: resolveProjectedWorkerEnvironmentSurface });
  }
  return processViewerManager;
}

function getWorkerEnvironmentFileReader() {
  if (!workerEnvironmentFileReader) {
    workerEnvironmentFileReader = createWorkerEnvironmentFileReader({ resolveSurface: resolveProjectedWorkerEnvironmentSurface });
  }
  return workerEnvironmentFileReader;
}

function getLabFileStore() {
  labFileStore ??= createLabFileStore({
    registryRoot: resolveLocalPackageRegistry,
    workspaceFiles: () => getProjectionStore("lab").workspaceFiles(),
    interfaceDescriptor: (interfaceId) => getProjectionStore("lab").interfaceDescriptor(interfaceId),
  });
  return labFileStore;
}

function osFileStoreFor(token) {
  return getClientAccessController().requireSession(token).profile === "lab" ? getLabFileStore() : getOsFileStore();
}

function getOsFileStore() {
  if (!osFileStore) {
    osFileStore = createOsFileStore({
      resolveRoots: () => {
        const source = resolveRepoRoot();
        return [
          { id: "octopus-home", label: "Octopus Home", root: resolveOctopusHome() },
          ...(source ? [{ id: "source", label: "Source", root: source }] : []),
        ];
      },
    });
  }
  return osFileStore;
}

function getWorkerEnvironmentOperatorActions() {
  if (!workerEnvironmentOperatorActions) {
    workerEnvironmentOperatorActions = createWorkerEnvironmentOperatorActions({ resolveSurface: resolveProjectedWorkerEnvironmentSurface });
  }
  return workerEnvironmentOperatorActions;
}

async function resolveProjectedWorkerEnvironmentSurface(workerEnvironmentId, surfaceId) {
  const result = await getProjectionStore("live").read(projectionReadRequest(
    "projection.worker_environments.list",
    { all_scopes: true, include_archived: false },
  ));
  const environments = Array.isArray(result?.worker_environments) ? result.worker_environments : [];
  const environment = environments.find((candidate) => candidate?.worker_environment_id === workerEnvironmentId);
  if (!environment) throw new Error(`Worker Environment is no longer projected: ${workerEnvironmentId}`);
  const surfaces = Array.isArray(environment.client_surfaces) ? environment.client_surfaces : [];
  const surface = surfaces.find((candidate) => candidate?.id === surfaceId);
  if (!surface) throw new Error(`Worker Environment surface is no longer projected: ${surfaceId}`);
  const desktopRecovery = environment.lifecycle === "failed"
    && environment.worker_environment_kind === "desktop_session"
    && surface.kind === "desktop" && surface.protocol === "desktop_session" && surface.mode === "direct";
  if (environment.lifecycle !== "active" && !desktopRecovery) throw new Error(`Worker Environment is not active: ${workerEnvironmentId}`);
  return {
    id: surface.id,
    kind: surface.kind,
    mode: surface.mode,
    protocol: surface.protocol,
    viewerUrl: surface.viewer_url,
    root: surface.root,
    readOnly: surface.read_only === true,
    data: surface.data && typeof surface.data === "object" && !Array.isArray(surface.data) ? surface.data : {},
    environmentKind: environment.worker_environment_kind,
    environmentRoot: environment.root,
    formationRevision: Number.isSafeInteger(environment.opened_at_movement_seq) ? String(environment.opened_at_movement_seq) : "",
    formationPin: environment.formation_pin,
    lifecycle: environment.lifecycle,
    revision: Number.isSafeInteger(environment.last_activity_movement_seq) ? String(environment.last_activity_movement_seq) : "",
    runId: environment.run_id,
    doorInterfaceId: environment.door,
  };
}

async function resolveProjectedCdpSurface(workerEnvironmentId, surfaceId) {
  return resolveProjectedWorkerEnvironmentSurface(workerEnvironmentId, surfaceId);
}

function getClientSurfaceDocumentStore() {
  if (!clientSurfaceDocumentStore) {
    clientSurfaceDocumentStore = createClientSurfaceDocumentStore({
      root: resolveClientSurfaceRoot(),
      onChange: (generation) => {
        for (const win of BrowserWindow.getAllWindows()) {
          if (!win.isDestroyed()) {
            win.webContents.send("octopus:client-surface-documents-changed", { generation });
          }
        }
      },
    });
  }
  return clientSurfaceDocumentStore;
}

function getClientExtensionStore() {
  if (!clientExtensionStore) {
    clientExtensionStore = createClientExtensionStore({
      root: path.join(app.getPath("userData"), "extensions"),
      onChange: notifyClientExtensionsChanged,
    });
  }
  return clientExtensionStore;
}

function notifyClientExtensionsChanged(generation) {
  for (const win of BrowserWindow.getAllWindows()) {
    if (!win.isDestroyed()) {
      win.webContents.send("octopus:client-extensions-changed", { generation });
    }
  }
}

async function clientExtensionSnapshot() {
  const snapshot = await getClientExtensionStore().snapshot();
  const installedExtensionIds = verifiedInstalledExtensionIds(snapshot);
  const actionStatuses = await readClientExtensionActionStatuses({
    octopusHome: resolveOctopusHome(),
    codexHome: resolveOperatorCodexHome(),
    installedExtensionIds,
  });
  return { ...snapshot, actionStatuses };
}

function verifiedInstalledExtensionIds(snapshot) {
  if (snapshot?.stateStatus !== "valid") return [];
  return Object.entries(snapshot?.lifecycle ?? {}).flatMap(([id, evidence]) => (
    evidence?.receiptState === "valid" && evidence.desiredState === "installed" ? [id] : []
  ));
}

function getClientSetupStore() {
  if (!clientSetupStore) {
    clientSetupStore = createClientSetupStore({
      root: path.join(app.getPath("userData"), "setup-state"),
      owners: createClientSetupOwners(),
    });
  }
  return clientSetupStore;
}

function createClientSetupOwners() {
  return {
    packages: {
      probe: async (plan) => clientSetupPackageProof(plan),
      apply: async (plan, context) => {
        const roots = clientSetupPackageRoots();
        const catalog = await readLocalPackageCatalog(roots);
        requireClientSetupPack(catalog, plan);
        await applyLocalApplicationPack({
          ...roots,
          operationId: context.operationId,
          packId: plan.pack.id,
          selectedRecommended: plan.pack.selected_recommended,
          expectedSourceDigests: Object.fromEntries(plan.packages.map((entry) => [entry.id, entry.source_digest])),
          assertMutable: (packageIds) => getOperatorChatClientLoader().assertMutationAllowed(packageIds),
        });
        getProjectionStore("live").invalidate();
      },
    },
    extensions: {
      probe: async (plan) => clientSetupExtensionProof(plan),
      apply: async (plan, context) => {
        const store = getClientExtensionStore();
        for (const extension of plan.extensions) {
          const snapshot = await store.snapshot();
          await store.applyLifecycle({
            action: extension.desired_state === "installed" ? "install" : "remove",
            id: extension.id,
            operationId: context.operationId,
            expectedGeneration: snapshot.generation,
            expectedRevision: { digest: extension.source_digest, revision: extension.version },
          });
        }
      },
    },
    provider: getClientProviderMetadata().setupOwner,
    workspace: {
      probe: async (plan, context) => {
        const octopusHome = resolveOctopusHome();
        const { withOperatorWorkspaceTopologyLock } = await loadOperatorChatClient();
        return withOperatorWorkspaceTopologyLock(
          octopusHome,
          () => clientSetupWorkspaceProof(plan, context.operationId),
        );
      },
      apply: async (plan, context) => {
        const octopusHome = resolveOctopusHome();
        const registryRoot = path.join(octopusHome, "registry");
        const { withOperatorWorkspaceTopologyLock } = await loadOperatorChatClient();
        await withOperatorWorkspaceTopologyLock(octopusHome, async () => {
          const state = await readWorkspaceSetupState({ octopusHomeRoot: octopusHome, registryRoot });
          if (state.status === "configured") {
            if (
              state.receipt?.operation_id !== context.operationId
              || state.receipt.request_digest !== plan.workspace.request_digest
            ) throw new Error("Another workspace setup receipt already owns the configured Home.");
          } else if (state.status === "not_configured") {
            await initializeWorkspace({
              octopusHomeRoot: octopusHome,
              registryRoot,
              publication: workspaceStorage(octopusHome),
              retirementBarrier: {
                verifyBefore: (context) => taskRetirementBarrier(octopusHome).then((barrier) => barrier.verifyBefore(context)),
                withHeldProof: (context, operation) => taskRetirementBarrier(octopusHome).then((barrier) => barrier.withHeldProof(context, operation)),
              },
              request: plan.workspace.request,
              planOperatorSurface: planWorkspaceHomeSurface,
              planBrowserNativeHost,
              planDesktopApplicationPreferences,
              planAgentPreset: planWorkspaceAgentPreset,
            });
            await retryNativeBrowserAfterSetup();
          } else {
            throw new Error("Workspace setup evidence is degraded.");
          }
          getProjectionStore("live").invalidate();
          await applyClientSetupProjectDefaults(plan, context.operationId);
          await applyClientSetupTriggers(plan, context.operationId);
        });
        getProjectionStore("live").invalidate();
      },
    },
  };
}

function clientSetupPackageRoots() {
  const octopusHome = resolveOctopusHome();
  const sourceRegistryRoot = resolveLocalPackageRegistry();
  return {
    sourceRegistryRoot,
    publication: require("./packageStorage.cjs").createPackageStoragePublisher(sourceRegistryRoot, resolveInstallationRoot()),
    activeRegistryRoot: path.join(octopusHome, "registry"),
    libraryRoot: resolveLocalPackageLibrary(),
    stateRoot: path.join(octopusHome, "client", "package-state"),
  };
}

async function clientSetupPackageProof(plan) {
  const catalog = await readLocalPackageCatalog(clientSetupPackageRoots());
  requireClientSetupPack(catalog, plan);
  const packages = new Map(catalog.packages.map((entry) => [entry.id, entry]));
  const references = [];
  for (const expected of plan.packages) {
    const actual = packages.get(expected.id);
    if (
      !actual
      || actual.installed !== true
      || actual.receipt_state !== "valid"
      || actual.source_digest !== expected.source_digest
    ) return null;
    references.push(clientSetupReference("package_install_evidence", expected.id, {
      installed_digest: actual.installed_digest,
      receipt_operation_id: actual.receipt_operation_id,
      source_digest: actual.source_digest,
    }));
  }
  return references;
}

async function validateClientSetupPlan(plan) {
  if (!plan || typeof plan !== "object" || Array.isArray(plan)) throw new Error("Client setup plan must be an object.");
  const catalog = await readLocalPackageCatalog(clientSetupPackageRoots());
  requireClientSetupPack(catalog, plan);
  const extensionStore = getClientExtensionStore();
  const snapshot = await extensionStore.snapshot();
  if (snapshot.stateStatus !== "valid") throw new Error("Client Extension state is degraded and cannot begin setup.");
  for (const expected of plan.extensions) {
    const current = await extensionStore.currentRevision(expected.id);
    if (current.digest !== expected.source_digest || current.revision !== expected.version) {
      throw new Error(`Client Extension '${expected.id}' no longer matches the immutable setup revision.`);
    }
  }
}

function requireClientSetupPack(catalog, plan) {
  const pack = catalog.packs.find((entry) => entry.id === plan.pack.id);
  if (!pack || pack.manifest_digest !== plan.pack.manifest_digest) {
    throw new Error(`Client setup Application Pack '${plan.pack.id}' no longer matches its immutable manifest digest.`);
  }
  const recommended = new Set(pack.components
    .filter((component) => component.selection === "recommended")
    .map((component) => component.package_id));
  if (plan.pack.selected_recommended.some((id) => !recommended.has(id))) {
    throw new Error(`Client setup Application Pack '${plan.pack.id}' contains an invalid recommended selection.`);
  }
  const expectedPackages = pack.components.flatMap((component) => (
    component.selection !== "recommended" || plan.pack.selected_recommended.includes(component.package_id)
      ? [component.package_id]
      : []
  )).sort();
  if (stableClientSetupJson(expectedPackages) !== stableClientSetupJson(plan.packages.map((entry) => entry.id).sort())) {
    throw new Error(`Client setup Application Pack '${plan.pack.id}' package closure does not match its immutable plan.`);
  }
  const packExtensions = [...(pack.extensions ?? [])].sort((left, right) => left.extension_id.localeCompare(right.extension_id));
  const plannedExtensions = [...plan.extensions].sort((left, right) => left.id.localeCompare(right.id));
  if (
    packExtensions.length !== plannedExtensions.length
    || packExtensions.some((extension, index) => (
      extension.extension_id !== plannedExtensions[index]?.id
    ))
  ) {
    throw new Error(`Client setup Application Pack '${plan.pack.id}' Extension closure does not match its immutable plan.`);
  }
  const recipe = pack.workspace_setup?.home;
  const taskRecipe = pack.workspace_setup?.task;
  const requestHome = plan.workspace.request?.home;
  const requestTask = plan.workspace.request?.task;
  const selectedPackageIds = new Set(expectedPackages);
  const expectedProjectDefaults = (pack.workspace_setup?.project_defaults ?? [])
    .filter((entry) => selectedPackageIds.has(entry.package_id))
    .map((entry) => {
      const components = pack.components.filter((component) => component.package_id === entry.package_id);
      if (components.length !== 1 || !["application", "driver"].includes(components[0].product_kind)) {
        throw new Error(`Client setup Project default '${entry.package_id}' has no ordinary Application or Driver owner.`);
      }
      return {
        package_id: entry.package_id,
        product_kind: components[0].product_kind,
        product_id: components[0].product_id,
        default_for_new_tasks: entry.default_for_new_tasks,
      };
    })
    .sort((left, right) => left.package_id.localeCompare(right.package_id));
  const plannedProjectDefaults = [...(plan.workspace.project_defaults ?? [])]
    .sort((left, right) => left.package_id.localeCompare(right.package_id));
  const plannedProjectDefaultContract = plannedProjectDefaults.map((entry) => ({
    package_id: entry.package_id,
    product_kind: entry.product_kind,
    product_id: entry.product_id,
    default_for_new_tasks: entry.default_for_new_tasks,
  }));
  const requestProjectKeys = new Set((plan.workspace.request?.projects ?? []).map((project) => project.key));
  const presetChoices = plan.workspace.request?.agentPresets ?? [];
  const presetIds = new Set();
  for (const choice of presetChoices) {
    const preset = pack.workspace_setup?.agent_presets?.find((entry) => entry.id === choice.presetId);
    if (!preset || presetIds.has(choice.presetId)
        || choice.packId !== pack.id || choice.expectedPackManifestDigest !== pack.manifest_digest
        || !selectedPackageIds.has(preset.component_package_id)
        || !Array.isArray(choice.projectKeys) || choice.projectKeys.length === 0
        || new Set(choice.projectKeys).size !== choice.projectKeys.length || choice.projectKeys.some((key) => !requestProjectKeys.has(key))
        || choice.providerInterfaceId !== preset.provider_interface_id || choice.providerInterfaceId !== requestHome?.providerInterfaceId
        || choice.providerModel !== requestHome?.providerModel || choice.reasoningEffort !== (requestHome?.reasoningEffort ?? "")
        || (choice.serviceTier ?? "") !== (requestHome?.serviceTier ?? "")) {
      throw new Error(`Client setup Agent preset does not match its reviewed pack, Projects, or provider selection.`);
    }
    presetIds.add(choice.presetId);
  }
  if (plan.workspace.request?.applicationConsentAuthority !== undefined) {
    throw new Error("Home applicationConsentAuthority setup selection is retired and requires an explicit new setup plan");
  }
  const canonicalWorkspaceRequest = {
    operationId: plan.workspace.request?.operationId,
    root: plan.workspace.request?.root,
    home: {
      ...(requestHome ?? {}),
      reasoningEffort: requestHome?.reasoningEffort ?? "",
    },
    task: requestTask,
    projects: plan.workspace.request?.projects,
    ...(plan.workspace.request?.agentPresets?.length ? { agentPresets: plan.workspace.request.agentPresets } : {}),
  };
  if (
    !recipe
    || !taskRecipe
    || plan.provider.interface_id !== recipe.provider_interface_id
    || requestHome?.providerInterfaceId !== recipe.provider_interface_id
    || requestHome?.workerInterfaceId !== recipe.worker_interface_id
    || stableClientSetupJson(requestHome?.doorHostSlots ?? {}) !== stableClientSetupJson(recipe.door_host_slots ?? {})
    || typeof requestHome?.providerModel !== "string" || !requestHome.providerModel.trim()
    || stableClientSetupJson(requestHome?.contextArtifactBindings ?? []) !== stableClientSetupJson(recipe.context_artifact_bindings)
    || stableClientSetupJson(requestHome?.selectedWorkerEnvironmentKinds ?? []) !== stableClientSetupJson(recipe.selected_worker_environment_kinds)
    || stableClientSetupJson(requestTask) !== stableClientSetupJson({
      workerInterfaceId: taskRecipe.worker_interface_id,
      contextArtifactBindings: taskRecipe.context_artifact_bindings,
      selectedWorkerEnvironmentKinds: taskRecipe.selected_worker_environment_kinds,
    })
    || stableClientSetupJson(plannedProjectDefaultContract) !== stableClientSetupJson(expectedProjectDefaults)
    || plannedProjectDefaults.some((entry) => entry.project_keys.some((projectKey) => !requestProjectKeys.has(projectKey)))
    || plan.workspace.request_digest !== createHash("sha256").update(stableClientSetupJson(canonicalWorkspaceRequest)).digest("hex")
  ) {
    throw new Error(`Client setup Application Pack '${plan.pack.id}' workspace recipe does not match its immutable plan.`);
  }
  return pack;
}

async function clientSetupExtensionProof(plan) {
  const snapshot = await getClientExtensionStore().snapshot();
  if (snapshot.stateStatus !== "valid") return null;
  const references = [];
  for (const expected of plan.extensions) {
    const evidence = snapshot.lifecycle[expected.id];
    if (
      evidence?.receiptState !== "valid"
      || evidence.desiredState !== expected.desired_state
      || evidence.sourceRevision?.digest !== expected.source_digest
      || evidence.sourceRevision?.revision !== expected.version
      || !/^[a-f0-9]{64}$/.test(evidence.receiptDigest)
    ) return null;
    references.push(clientSetupReference("extension_lifecycle_receipt", expected.id, {
      extension_id: expected.id,
      operation_id: evidence.operationId,
      request_digest: evidence.requestDigest,
      desired_state: evidence.desiredState,
      source_revision: evidence.sourceRevision,
      completed_at: evidence.completedAt,
      receipt_digest: evidence.receiptDigest,
    }));
  }
  return references;
}

function getClientProviderMetadata() {
  return createClientProviderMetadata({
    registryRoot: () => path.join(resolveOctopusHome(), "registry"),
    sourceRegistryRoot: resolveLocalPackageRegistry,
    accountClient: getChatgptSignIn,
    reference: clientSetupReference,
  });
}

async function clientSetupWorkspaceProof(plan, operationId) {
  const state = await readWorkspaceSetupState({
    octopusHomeRoot: resolveOctopusHome(),
    registryRoot: path.join(resolveOctopusHome(), "registry"),
  });
  if (
    state.status !== "configured"
    || state.receipt?.operation_id !== operationId
    || state.receipt.request_digest !== plan.workspace.request_digest
  ) return null;
  const defaultReferences = await clientSetupProjectDefaultProof(plan, state.receipt);
  if (!defaultReferences) return null;
  const triggers = await setupTriggerProof(path.join(resolveOctopusHome(), "registry"), resolveOctopusHome(), await clientSetupTriggerEntries(plan));
  if (!triggers) return null;
  return [
    clientSetupReference("workspace_receipt", operationId, state.receipt),
    ...defaultReferences,
    ...triggers.map((trigger) => clientSetupReference("workspace_receipt", `trigger.${trigger.interface_id}`, trigger)),
  ];
}

async function clientSetupTriggerEntries(plan) {
  const pack = requireClientSetupPack(await readLocalPackageCatalog(clientSetupPackageRoots()), plan);
  return setupTriggerEntries(pack, plan.pack.selected_recommended);
}

async function applyClientSetupTriggers(plan, operationId) {
  const octopusHome = resolveOctopusHome();
  const registryRoot = path.join(octopusHome, "registry");
  const state = await readWorkspaceSetupState({ octopusHomeRoot: octopusHome, registryRoot });
  if (
    state.status !== "configured"
    || state.receipt?.operation_id !== operationId
    || state.receipt.request_digest !== plan.workspace.request_digest
  ) throw new Error("Workspace receipt is not ready for its triggers.");
  const entries = await clientSetupTriggerEntries(plan);
  await armSetupTriggers(registryRoot, octopusHome, entries, {
    commitClientAuthoring: (authoringRequest) => commitProjectionClientAuthoring(octopusHome, registryRoot, authoringRequest),
  });
  getProjectionStore("live").invalidate();
  if (!await setupTriggerProof(registryRoot, octopusHome, entries)) {
    throw new Error("Workspace triggers were armed without canonical read-back proof.");
  }
}

async function applyClientSetupProjectDefaults(plan, operationId) {
  const octopusHome = resolveOctopusHome();
  const registryRoot = path.join(octopusHome, "registry");
  const state = await readWorkspaceSetupState({
    octopusHomeRoot: octopusHome,
    registryRoot,
  });
  if (
    state.status !== "configured"
    || state.receipt?.operation_id !== operationId
    || state.receipt.request_digest !== plan.workspace.request_digest
  ) throw new Error("Workspace receipt is not ready for Project defaults.");
  for (const projectDefault of plan.workspace.project_defaults) {
    const context = await clientSetupProjectDefaultContext(projectDefault, state.receipt);
    if (context.proven) continue;
    const request = projectDefault.product_kind === "application"
      ? {
          action: "configure_application",
          applicationId: projectDefault.product_id,
          projects: context.targets.map((target) => ({
            eeId: target.eeId,
            available: true,
            selectedByDefault: projectDefault.default_for_new_tasks,
            expectedDigest: target.recordDigest,
          })),
        }
      : {
          action: "configure_driver",
          driverName: projectDefault.product_id,
          projects: context.targets.map((target) => ({
            eeId: target.eeId,
            available: true,
            selectedByDefault: projectDefault.default_for_new_tasks,
            hostSlots: target.hostSlots,
            expectedDigest: target.recordDigest,
          })),
        };
    await mutateOctopusOsWorkspace(
      registryRoot,
      octopusHome,
      context.scope,
      request,
      {
        commitClientAuthoring: (authoringRequest) => commitProjectionClientAuthoring(
          octopusHome,
          registryRoot,
          authoringRequest,
        ),
      },
    );
    getProjectionStore("live").invalidate();
  }
  if (!await clientSetupProjectDefaultProof(plan, state.receipt)) {
    throw new Error("Workspace Project defaults were applied without canonical read-back proof.");
  }
}

async function clientSetupProjectDefaultProof(plan, receipt) {
  const references = [];
  for (const projectDefault of plan.workspace.project_defaults) {
    const context = await clientSetupProjectDefaultContext(projectDefault, receipt);
    if (!context.proven) return null;
    references.push(clientSetupReference(
      "workspace_receipt",
      `default.${projectDefault.product_kind}.${projectDefault.product_id}`,
      {
        package_id: projectDefault.package_id,
        product_kind: projectDefault.product_kind,
        product_id: projectDefault.product_id,
        default_for_new_tasks: projectDefault.default_for_new_tasks,
        project_keys: projectDefault.project_keys,
        targets: context.targets.map((target) => ({
          ee_id: target.eeId,
          policy_allowed: target.policyAllowed,
          selected_by_default: target.selectedByDefault,
          record_digest: target.recordDigest,
        })),
      },
    ));
  }
  return references;
}

async function clientSetupProjectDefaultContext(projectDefault, receipt) {
  const store = getProjectionStore("live");
  store.invalidate();
  const scope = await store.read(projectionReadRequest("projection.operator.os.scope"));
  if (
    scope?.status !== "ready"
    || scope.root_ee_id !== receipt.root_ee_id
    || scope.home_run_id !== receipt.home_run_id
    || scope.home_interface_id !== receipt.home_interface_id
  ) throw new Error("Workspace scope does not match its receipt while applying Project defaults.");
  const projectReceipts = new Map(receipt.project_ee_ids.map((entry) => [entry.key, entry.ee_id]));
  const missingProjectKey = projectDefault.project_keys.find((projectKey) => !projectReceipts.has(projectKey));
  if (missingProjectKey) {
    throw new Error(`Workspace Project default '${projectDefault.product_id}' references unknown Project '${missingProjectKey}'.`);
  }
  const targetIds = [
    receipt.root_ee_id,
    ...projectDefault.project_keys.map((projectKey) => projectReceipts.get(projectKey)),
  ];
  const environments = new Map(
    [scope.root_environment, ...(scope.projects ?? [])]
      .filter(Boolean)
      .map((environment) => [environment.ee_id, environment]),
  );
  if (targetIds.some((eeId) => !environments.has(eeId))) {
    throw new Error("Workspace receipt Project targets are missing from the canonical scope.");
  }
  const method = projectDefault.product_kind === "application"
    ? "projection.operator.os.applications"
    : "projection.operator.os.drivers";
  const projection = await store.read(projectionReadRequest(method));
  const records = projectDefault.product_kind === "application" ? projection.applications : projection.drivers;
  const capability = (records ?? []).find((entry) => (
    projectDefault.product_kind === "application"
      ? entry.id === projectDefault.product_id
      : entry.name === projectDefault.product_id
  ));
  if (!capability || capability.installed !== true) {
    throw new Error(`Workspace Project default '${projectDefault.product_id}' is not installed.`);
  }
  const projectStates = new Map((capability.project_states ?? []).map((entry) => [entry.ee_id, entry]));
  const targets = targetIds.map((eeId) => {
    const environment = environments.get(eeId);
    const state = projectStates.get(eeId);
    if (!state || state.record_digest !== environment.record_digest) {
      throw new Error(`Workspace Project default '${projectDefault.product_id}' has stale EE evidence for '${eeId}'.`);
    }
    return {
      eeId,
      recordDigest: environment.record_digest,
      policyAllowed: state.policy_allowed === true,
      selectedByDefault: state.selected_by_default === true,
      hostSlots: projectDefault.product_kind === "driver" && state.host_slots && typeof state.host_slots === "object"
        ? state.host_slots
        : {},
    };
  });
  return {
    scope,
    targets,
    proven: targets.every((target) => (
      target.policyAllowed
      && target.selectedByDefault === projectDefault.default_for_new_tasks
    )),
  };
}

function clientSetupReference(kind, id, evidence) {
  return {
    kind,
    id,
    digest: createHash("sha256").update(stableClientSetupJson(evidence)).digest("hex"),
    evidence,
  };
}

function stableClientSetupJson(value) {
  if (Array.isArray(value)) return `[${value.map(stableClientSetupJson).join(",")}]`;
  if (value && typeof value === "object") {
    return `{${Object.keys(value).sort().map((key) => `${JSON.stringify(key)}:${stableClientSetupJson(value[key])}`).join(",")}}`;
  }
  return JSON.stringify(value);
}

async function planWorkspaceHomeSurface({ eeInstancesRoot, request }) {
  const octopusHome = resolveOctopusHome();
  const registryRoot = path.join(octopusHome, "registry");
  const { planOperatorChatSurface } = await loadOperatorChatClient();
  return planOperatorChatSurface({
    octopusHome: () => octopusHome,
    installationRoot: resolveInstallationRoot,
    registryRoot: () => registryRoot,
    eeInstancesRoot: () => eeInstancesRoot,
  }, request);
}

async function withWorkspaceAgentPreset(choice, eeInstancesRoot, operation) {
  const home = resolveOctopusHome();
  if (choice.home !== home) throw new Error("Agent preset belongs to another Home");
  const verifyHome = () => {
    if (resolveOctopusHome() !== home) throw new Error("Home changed during Agent preset authoring");
  };
  const { planOperatorChatSurface } = await loadOperatorChatClient();
  verifyHome();
  const registryRoot = path.join(home, "registry");
  return withInstalledAgentPreset(clientSetupPackageRoots(), choice, async (source) => {
    const providerInterfaceId = requiredString(choice.providerInterfaceId, "Agent provider interface");
    const providerModel = requiredString(choice.providerModel, "Agent provider model");
    const token = createHash("sha256").update(`${choice.operationId}\0${choice.projectEeId}\0${choice.presetId}`).digest("hex").slice(0, 32);
    if (typeof choice.reasoningEffort !== "string" || choice.reasoningEffort.length > 80) throw new Error("Agent reasoning effort is invalid");
    const preflight = await planOperatorChatSurface({
      octopusHome: () => home,
      installationRoot: resolveInstallationRoot,
      registryRoot: () => registryRoot,
      eeInstancesRoot: () => eeInstancesRoot,
    }, {
        chatId: `agent_preflight_${token}`, cycle: `agent_preflight_${token}`,
        eeId: choice.projectEeId, clientSurfaceRole: "task",
        workerInterfaceId: source.recipe.worker_interface_id,
        providerInterfaceId, providerModel,
        reasoningEffort: choice.reasoningEffort,
        serviceTier: choice.serviceTier,
        doorHostSlots: {},
        contextArtifactBindings: source.recipe.context_artifact_bindings,
        selectedWorkerEnvironmentKinds: source.recipe.selected_worker_environment_kinds,
    });
    verifyHome();
    const profile = preflight.authoringProfile;
    if (profile.ee_id !== choice.projectEeId || profile.worker_choice_interface !== source.sourceInterface.id || preflight.providerModel !== providerModel) throw new Error("Agent preset planning changed its Project, worker, or model");
    if (choice.serviceTier && preflight.serviceTier !== choice.serviceTier) throw new Error("Agent preset planning changed its selected service tier");
    if (source.recipe.selected_application_ids.length) throw new Error("Agent preset application binding planning is unavailable");
    const settings = {
      schema_version: 1, worker_interface: source.sourceInterface.id, project_ee_id: choice.projectEeId,
      provider_interface_id: providerInterfaceId, provider_model: providerModel, reasoning_effort: choice.reasoningEffort,
      ...(choice.serviceTier ? { service_tier: choice.serviceTier } : {}),
      application_ids: source.recipe.selected_application_ids,
      worker_environment_kinds: source.recipe.selected_worker_environment_kinds,
    };
    return operation({ ...source, profile, settings, verify: async () => { verifyHome(); await source.verify(); verifyHome(); } });
  });
}

async function planWorkspaceAgentPreset({ choice, projectEeId, operationId, eeInstancesRoot }) {
  return withWorkspaceAgentPreset({ ...choice, home: resolveOctopusHome(), projectEeId, operationId }, eeInstancesRoot, async (preset) => ({
    ...planClientAgentTemplate({
      operationId, projectEeId, sourceInterface: preset.sourceInterface, sourcePolicy: preset.sourcePolicy,
      sourceProfile: preset.profile, settings: preset.settings, name: preset.recipe.name, icon: preset.recipe.icon,
    }),
    verify: preset.verify,
  }));
}

function getCodexUsageReader() {
  if (!codexUsageReader) {
    codexUsageReader = createCodexUsageReader({
      cacheRoot: path.join(app.getPath("userData"), "usage-cache"),
    });
  }
  return codexUsageReader;
}

function getClientArtifactStreamStore() {
  if (!clientArtifactStreamStore) {
    const octopusHome = resolveOctopusHome();
    clientArtifactStreamStore = createClientArtifactStreamStore({
      clientArtifactsRoot: (interfaceId) => path.join(octopusHome, "interface-hand-in", interfaceId, "client"),
      projectionStore: () => getProjectionStore("live"),
      handInArtifact: (input) => handInArtifact({ ...input, octopusHome }),
      authorizeSubmission: authorizeClientInterfaceArtifactSubmission,
    });
  }
  return clientArtifactStreamStore;
}

function getClientAttachmentStagingStore() {
  if (!clientAttachmentStagingStore) {
    clientAttachmentStagingStore = createClientAttachmentStagingStore({
      root: () => path.join(app.getPath("userData"), "attachment-staging"),
    });
  }
  return clientAttachmentStagingStore;
}

const DEFAULT_APP_ZOOM_FACTOR = 1;
const MIN_APP_ZOOM_FACTOR = 0.5;
const MAX_APP_ZOOM_FACTOR = 2.5;
const APP_ZOOM_STEP = 0.1;
let appZoomFactor = loadAppZoomFactor();
const MIN_WINDOW_BOUNDS = { width: 480, height: 600 };
const windowBackdropModes = new WeakMap();
const windowLiveResizeStates = new WeakMap();
const windowAppearances = new WeakMap();
const nativeChromeWindows = new WeakSet();
const customChromeWindows = new WeakSet();
const historyNavigationStates = new WeakMap();

app.setName("Octopus");
if (isDev) {
  app.commandLine.appendSwitch("remote-debugging-port", "9223");
}

function normalizeAppZoomFactor(value) {
  if (typeof value !== "number" || !Number.isFinite(value)) return DEFAULT_APP_ZOOM_FACTOR;
  const stepped = Math.round(value / APP_ZOOM_STEP) * APP_ZOOM_STEP;
  return Math.min(MAX_APP_ZOOM_FACTOR, Math.max(MIN_APP_ZOOM_FACTOR, Number(stepped.toFixed(2))));
}

function loadAppZoomFactor() {
  try {
    const stat = fs.lstatSync(clientZoomPath);
    if (!stat.isFile() || stat.isSymbolicLink() || stat.size > 1024) return DEFAULT_APP_ZOOM_FACTOR;
    const parsed = JSON.parse(fs.readFileSync(clientZoomPath, "utf8"));
    return normalizeAppZoomFactor(parsed?.factor);
  } catch (error) {
    if (error && error.code === "ENOENT") return DEFAULT_APP_ZOOM_FACTOR;
    return DEFAULT_APP_ZOOM_FACTOR;
  }
}

function persistAppZoomFactor() {
  const temporary = `${clientZoomPath}.${process.pid}.${randomUUID()}.tmp`;
  fs.writeFileSync(temporary, `${JSON.stringify({ version: 1, factor: appZoomFactor })}\n`, { mode: 0o600, flag: "wx" });
  fs.renameSync(temporary, clientZoomPath);
  fs.chmodSync(clientZoomPath, 0o600);
}

function applyAppZoomFactor() {
  for (const win of BrowserWindow.getAllWindows()) {
    if (win.isDestroyed() || win.webContents.isDestroyed()) continue;
    win.webContents.setZoomFactor(appZoomFactor);
    if (nativeChromeWindows.has(win)) win.setWindowButtonPosition(nativeTrafficLightPosition(appZoomFactor));
    publishWindowState(win);
  }
}

function updateAppZoomFactor(next) {
  const normalized = normalizeAppZoomFactor(next);
  if (normalized === appZoomFactor) return;
  appZoomFactor = normalized;
  persistAppZoomFactor();
  applyAppZoomFactor();
}

function bindAppZoomShortcuts(win) {
  win.webContents.on("before-input-event", (event, input) => {
    if (!(input.meta || input.control) || input.alt) return;
    const code = input.code || "";
    if (code === "Equal" || code === "NumpadAdd") {
      event.preventDefault();
      updateAppZoomFactor(appZoomFactor + APP_ZOOM_STEP);
    } else if (code === "Minus" || code === "NumpadSubtract") {
      event.preventDefault();
      updateAppZoomFactor(appZoomFactor - APP_ZOOM_STEP);
    } else if (code === "Digit0" || code === "Numpad0") {
      event.preventDefault();
      updateAppZoomFactor(DEFAULT_APP_ZOOM_FACTOR);
    }
  });
}

function createWindow(inheritedAccessToken = "", options = {}) {
  clientWindowClose.assertCanCreateWindow();
  const primary = options.primary !== false;
  const openRequest = options.initialTarget === undefined
    ? {}
    : validateClientWindowOpenRequest({ initialTarget: options.initialTarget });
  if (primary && clientWindows.primaryEntry()) throw new Error("The primary Octopus client window already exists.");
  const iconPath = appIconPath();
  const isMac = process.platform === "darwin";
  const windowState = primary ? loadWindowState() : peerClientWindowState();
  const accessClaim = createAccessInheritanceClaim(inheritedAccessToken);
  const win = new BrowserWindow({
    width: windowState.bounds.width,
    height: windowState.bounds.height,
    ...(Number.isFinite(windowState.bounds.x) && Number.isFinite(windowState.bounds.y)
      ? { x: windowState.bounds.x, y: windowState.bounds.y }
      : {}),
    minWidth: MIN_WINDOW_BOUNDS.width,
    minHeight: MIN_WINDOW_BOUNDS.height,
    title: "Octopus",
    ...(iconPath ? { icon: iconPath } : {}),
    backgroundColor: isMac ? "#00000000" : "#181818",
    show: false,
    titleBarStyle: isMac ? "hiddenInset" : "hidden",
    transparent: isMac,
    ...(isMac ? {
      trafficLightPosition: nativeTrafficLightPosition(appZoomFactor),
      vibrancy: "under-window",
      visualEffectState: "active",
    } : {}),
    webPreferences: {
      preload: path.join(__dirname, "preload.cjs"),
      backgroundThrottling: false,
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: true,
      webviewTag: true,
      spellcheck: true,
      v8CacheOptions: "bypassHeatCheckAndEagerCompile",
      ...(accessClaim ? { additionalArguments: [`--octopus-access-claim=${accessClaim}`] } : {}),
    },
  });
  win.webContents.on("will-attach-webview", (event, _preferences, _params, details) => {
    if (!details?.webContents || !getNativeBrowserSurface().validatePreparedGuest(win.webContents, details.webContents)) event.preventDefault();
  });
  const windowBootstrap = clientWindows.register(win, {
    accessToken: inheritedAccessToken,
    primary,
    ...(options.reattachWindowId ? { reattachWindowId: options.reattachWindowId } : {}),
    ...(openRequest.initialTarget ? { initialTarget: openRequest.initialTarget } : {}),
  });
  clientWindowClose.trackWindow(win, windowBootstrap.windowId);
  bindAccessInheritanceClaim(accessClaim, win);
  bindAppZoomShortcuts(win);
  if (primary) primaryWindow = win;
  const clientWebContentsId = win.webContents.id;
  for (const [name, event] of [["unresponsive", "client.renderer.unresponsive"], ["responsive", "client.renderer.responsive"]]) {
    win.on(name, () => { void appendClientLog("warn", event).catch(() => {}); });
  }
  win.webContents.on("render-process-gone", (_event, details) => {
    void appendClientLog("error", "client.renderer.gone", { message: `Renderer exited: ${details.reason}; exit code ${details.exitCode}` }).catch(() => {});
  });
  win.on("closed", () => {
    paneWindowDrag.removeOwner(clientWebContentsId);
    if (clientWindows.entries().length === 0 && paneDragPreviewHost && !paneDragPreviewHost.win.isDestroyed()) paneDragPreviewHost.win.destroy();
    if (primaryWindow === win) primaryWindow = null;
    if (accessClaim) accessInheritanceClaims.delete(accessClaim);
  });

  if (isMac) {
    nativeChromeWindows.add(win);
    win.setWindowButtonVisibility(true);
  } else customChromeWindows.add(win);
  applyWindowBackdrop(win, true);

  win.once("ready-to-show", () => {
    if (options.deferredShow) return;
    if (primary && windowState.maximized) win.maximize();
    if (primary && windowState.fullscreen) win.setFullScreen(true);
    win.show();
    if (!primary) win.focus();
  });
  if (primary) trackWindowState(win);
  for (const event of ["enter-full-screen", "leave-full-screen", "maximize", "unmaximize"]) {
    win.on(event, () => publishWindowState(win));
  }
  if (isMac) {
    win.on("will-resize", () => {
      if (windowLiveResizeStates.get(win) === true) return;
      windowLiveResizeStates.set(win, true);
      applyWindowBackdrop(win, true);
    });
    win.on("resized", () => {
      if (windowLiveResizeStates.get(win) !== true) return;
      windowLiveResizeStates.set(win, false);
      applyWindowBackdrop(win, true);
    });
    for (const event of ["focus", "blur", "show", "hide", "move", "resize"]) {
      win.on(event, () => applyWindowBackdrop(win));
    }
    win.on("swipe", (_event, direction) => {
      const history = historyNavigationStates.get(win);
      if (direction === "left" && history?.canNavigateBack) {
        win.webContents.send("octopus:navigate-back");
      } else if (direction === "right" && history?.canNavigateForward) {
        win.webContents.send("octopus:navigate-forward");
      }
    });
  }
  attachContextMenu(win);
  win.webContents.setWindowOpenHandler(({ url }) => {
    if (isExternalBrowserUrl(url)) void shell.openExternal(url);
    return { action: "deny" };
  });
  win.webContents.on("did-finish-load", () => {
    win.webContents.setZoomFactor(appZoomFactor);
    applyWindowBackdrop(win, true);
    publishWindowState(win);
  });

  loadRendererWindow(win);
  return win;
}

function presentPaneDestination(win) {
  if (win.isDestroyed()) return;
  const presentation = paneDestinationPresentations.get(win);
  paneDestinationPresentations.delete(win);
  if (presentation?.fullscreen) win.setFullScreen(true);
  else if (presentation?.maximized) win.maximize();
  win.show();
  win.focus();
}

async function capturePaneDragPreview(source, captureRect) {
  paneDragPreviewWindow();
  const zoom = source.win.webContents.getZoomFactor();
  const image = await source.win.webContents.capturePage({
    x: Math.floor(captureRect.x * zoom), y: Math.floor(captureRect.y * zoom),
    width: Math.ceil(captureRect.width * zoom), height: Math.ceil(captureRect.height * zoom),
  });
  if (source.win.isDestroyed() || image.isEmpty()) throw new Error("Source pane chrome could not be captured.");
  return { image, width: Math.ceil(captureRect.width * zoom), height: Math.ceil(captureRect.height * zoom), zoom };
}

const PANE_DRAG_PREVIEW_MARGIN = 24;
let paneDragPreviewHost = null;
let paneDragPreviewGeneration = 0;

function paneDragPreviewWindow() {
  if (paneDragPreviewHost && !paneDragPreviewHost.win.isDestroyed()) return paneDragPreviewHost;
  const isMac = process.platform === "darwin";
  const win = new BrowserWindow({
    width: 1, height: 1,
    show: false, frame: false, transparent: true, focusable: false, fullscreenable: false,
    skipTaskbar: true, alwaysOnTop: true, resizable: false, movable: false, minimizable: false, maximizable: false, hasShadow: false,
    ...(isMac ? { type: "panel" } : {}),
    webPreferences: { sandbox: true, contextIsolation: true, nodeIntegration: false, backgroundThrottling: false },
  });
  win.setIgnoreMouseEvents(true);
  win.setAlwaysOnTop(true, "pop-up-menu");
  if (isMac) win.setVisibleOnAllWorkspaces(true, { visibleOnFullScreen: true, skipTransformProcessType: true });
  win.webContents.setWindowOpenHandler(() => ({ action: "deny" }));
  void win.loadURL("data:text/html;charset=utf-8,").catch(() => {});
  const host = { win };
  win.once("closed", () => { if (paneDragPreviewHost === host) paneDragPreviewHost = null; });
  paneDragPreviewHost = host;
  return host;
}

async function openPaneDragPreview(capture, appearance) {
  const margin = PANE_DRAG_PREVIEW_MARGIN;
  const radius = Math.round(appearance.radius * capture.zoom * 100) / 100;
  const { win: preview } = paneDragPreviewWindow();
  const generation = ++paneDragPreviewGeneration;
  const current = () => generation === paneDragPreviewGeneration && !preview.isDestroyed();
  preview.hide();
  preview.setContentSize(capture.width + margin * 2, capture.height + margin * 2);
  const html = `<meta http-equiv="Content-Security-Policy" content="default-src 'none'; img-src data:; style-src 'unsafe-inline'"><style>html,body{margin:0;overflow:hidden;background:transparent}div{position:absolute;left:${margin}px;top:${margin}px;width:${capture.width}px;height:${capture.height}px;overflow:hidden;border-radius:${radius}px;background:${appearance.background || "transparent"};box-shadow:0 12px 32px rgb(0 0 0 / 22%),0 2px 8px rgb(0 0 0 / 14%)}img{display:block;width:${capture.width}px;height:${capture.height}px}</style><div><img alt="" src="${capture.image.toDataURL()}"></div>`;
  let readyTimer;
  try {
    await Promise.race([
      preview.loadURL(`data:text/html;charset=utf-8,${encodeURIComponent(html)}`),
      new Promise((_, reject) => { readyTimer = setTimeout(() => reject(new Error("Pane preview did not become ready.")), 5000); }),
    ]);
  } catch (error) {
    if (current()) preview.hide();
    throw error;
  } finally { clearTimeout(readyTimer); }
  if (!current()) throw new Error("Pane preview was replaced before it was ready.");
  preview.webContents.setZoomFactor(1);
  return {
    close() { if (current()) { paneDragPreviewGeneration += 1; preview.hide(); } },
    hide() { if (current()) preview.hide(); },
    setPosition(x, y) { if (!current()) throw new Error("Pane preview closed."); preview.setPosition(Math.round(x - margin), Math.round(y - margin)); },
    showInactive() { if (!current()) throw new Error("Pane preview closed."); preview.showInactive(); },
  };
}

function createQuickTaskWindow(accessToken) {
  if (quickTaskWindow && !quickTaskWindow.isDestroyed()) {
    if (quickTaskAccessToken !== accessToken) throw new Error("The quick task window belongs to another access owner.");
    if (quickTaskWindow.isMinimized()) quickTaskWindow.restore();
    quickTaskWindow.show();
    quickTaskWindow.focus();
    return quickTaskWindow;
  }
  const isMac = process.platform === "darwin";
  const accessClaim = createAccessInheritanceClaim(accessToken);
  const win = new BrowserWindow({
    width: 680,
    height: 430,
    minWidth: 520,
    minHeight: 320,
    title: "Quick task — Octopus",
    backgroundColor: isMac ? (nativeTheme.shouldUseDarkColors ? "#171717" : "#f7f7f7") : "#181818",
    show: false,
    titleBarStyle: isMac ? "hidden" : "hidden",
    trafficLightPosition: { x: -100, y: -100 },
    webPreferences: {
      preload: path.join(__dirname, "preload.cjs"),
      backgroundThrottling: false,
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: true,
      spellcheck: true,
      v8CacheOptions: "bypassHeatCheckAndEagerCompile",
      additionalArguments: [`--octopus-access-claim=${accessClaim}`],
    },
  });
  bindAccessInheritanceClaim(accessClaim, win);
  bindAppZoomShortcuts(win);
  quickTaskWindow = win;
  quickTaskAccessToken = accessToken;
  if (isMac) win.setWindowButtonVisibility(false);
  applyWindowBackdrop(win, true);
  attachContextMenu(win);
  win.webContents.setWindowOpenHandler(({ url }) => {
    if (isExternalBrowserUrl(url)) void shell.openExternal(url);
    return { action: "deny" };
  });
  win.webContents.on("did-finish-load", () => {
    win.webContents.setZoomFactor(appZoomFactor);
    applyWindowBackdrop(win, true);
    publishWindowState(win);
  });
  win.once("ready-to-show", () => {
    win.center();
    win.show();
    win.focus();
  });
  win.on("closed", () => {
    accessInheritanceClaims.delete(accessClaim);
    if (quickTaskWindow === win) { quickTaskWindow = null; quickTaskAccessToken = ""; }
  });
  loadRendererWindow(win, { window: "quick-task" });
  return win;
}

function createWorkerEnvironmentWindow(accessToken, target) {
  const key = `${accessToken}:${target.workerEnvironmentId}`;
  const existing = workerEnvironmentWindows.get(key);
  if (existing && !existing.win.isDestroyed()) {
    existing.target = target;
    if (existing.win.isMinimized()) existing.win.restore();
    if (!existing.win.isVisible()) existing.win.show();
    existing.win.focus();
    publishWorkerEnvironmentWindowTarget(existing.win, target);
    return existing.win;
  }

  const isMac = process.platform === "darwin";
  const accessClaim = createAccessInheritanceClaim(accessToken);
  const win = new BrowserWindow({
    width: 1080,
    height: 760,
    minWidth: 620,
    minHeight: 440,
    title: "Worker Environment — Octopus",
    backgroundColor: isMac ? (nativeTheme.shouldUseDarkColors ? "#171717" : "#f7f7f7") : "#181818",
    show: false,
    titleBarStyle: "hidden",
    trafficLightPosition: { x: -100, y: -100 },
    webPreferences: {
      preload: path.join(__dirname, "preload.cjs"),
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: true,
      spellcheck: true,
      v8CacheOptions: "bypassHeatCheckAndEagerCompile",
      additionalArguments: [`--octopus-access-claim=${accessClaim}`],
    },
  });
  bindAccessInheritanceClaim(accessClaim, win);
  bindAppZoomShortcuts(win);
  workerEnvironmentWindows.set(key, { accessClaim, accessToken, target, win });
  if (isMac) win.setWindowButtonVisibility(false);
  applyWindowBackdrop(win, true);
  attachContextMenu(win);
  win.webContents.setWindowOpenHandler(({ url }) => {
    if (isExternalBrowserUrl(url)) void shell.openExternal(url);
    return { action: "deny" };
  });
  win.webContents.on("did-finish-load", () => {
    win.webContents.setZoomFactor(appZoomFactor);
    applyWindowBackdrop(win, true);
    publishWindowState(win);
    publishWorkerEnvironmentWindowTarget(win, workerEnvironmentWindows.get(key)?.target ?? target);
  });
  win.once("ready-to-show", () => {
    win.center();
    win.show();
    win.focus();
  });
  win.on("closed", () => {
    accessInheritanceClaims.delete(accessClaim);
    workerEnvironmentWindows.delete(key);
  });
  loadRendererWindow(win, {
    window: "worker-environment",
    runId: target.runId,
    environmentId: target.workerEnvironmentId,
    panel: target.panelKind,
    surfaceId: target.surfaceId,
    filePath: target.filePath,
    documentId: target.clientSurface?.documentId,
    documentRevision: target.clientSurface?.revision == null ? "" : String(target.clientSurface.revision),
    sourceDigest: target.clientSurface?.sourceDigest,
  });
  return win;
}

function createTaskMonitorWindow(accessToken, runId) {
  const key = `${accessToken}:${runId}`;
  const existing = taskMonitorWindows.get(key);
  if (existing && !existing.win.isDestroyed()) {
    if (existing.win.isMinimized()) existing.win.restore();
    if (!existing.win.isVisible()) existing.win.show();
    existing.win.focus();
    return existing.win;
  }
  const isMac = process.platform === "darwin";
  const accessClaim = createAccessInheritanceClaim(accessToken);
  const win = new BrowserWindow({
    width: 520,
    height: 620,
    minWidth: 380,
    minHeight: 420,
    title: "Task Monitor — Octopus",
    backgroundColor: isMac ? (nativeTheme.shouldUseDarkColors ? "#171717" : "#f7f7f7") : "#181818",
    show: false,
    titleBarStyle: "hidden",
    trafficLightPosition: { x: -100, y: -100 },
    alwaysOnTop: true,
    webPreferences: {
      preload: path.join(__dirname, "preload.cjs"),
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: true,
      spellcheck: true,
      v8CacheOptions: "bypassHeatCheckAndEagerCompile",
      additionalArguments: [`--octopus-access-claim=${accessClaim}`],
    },
  });
  bindAccessInheritanceClaim(accessClaim, win);
  bindAppZoomShortcuts(win);
  taskMonitorWindows.set(key, { accessClaim, accessToken, runId, win });
  if (isMac) win.setWindowButtonVisibility(false);
  applyWindowBackdrop(win, true);
  attachContextMenu(win);
  win.webContents.setWindowOpenHandler(({ url }) => {
    if (isExternalBrowserUrl(url)) void shell.openExternal(url);
    return { action: "deny" };
  });
  win.webContents.on("did-finish-load", () => {
    win.webContents.setZoomFactor(appZoomFactor);
    applyWindowBackdrop(win, true);
    publishWindowState(win);
  });
  win.once("ready-to-show", () => {
    win.show();
    win.focus();
  });
  win.on("closed", () => {
    accessInheritanceClaims.delete(accessClaim);
    taskMonitorWindows.delete(key);
  });
  loadRendererWindow(win, { window: "task-monitor", runId });
  return win;
}

function publishWorkerEnvironmentWindowTarget(win, target) {
  if (!win || win.isDestroyed() || win.webContents.isDestroyed()) return;
  win.webContents.send("octopus:worker-environment-window-target", target);
}

function loadRendererWindow(win, query = {}) {
  // App windows remain on their shell. Links use explicit browser or Files actions;
  // renderer-initiated document navigation must not replace the application.
  win.webContents.on("will-navigate", (event) => event.preventDefault());
  if (isDev) {
    const url = new URL(process.env.VITE_DEV_SERVER_URL);
    for (const [key, value] of Object.entries(query)) {
      if (value) url.searchParams.set(key, value);
    }
    void win.loadURL(url.toString());
  } else {
    const boundedQuery = Object.fromEntries(Object.entries(query).filter(([, value]) => Boolean(value)));
    void win.loadFile(path.join(__dirname, "../dist/index.html"), Object.keys(boundedQuery).length ? { query: boundedQuery } : undefined);
  }
}

function createAccessInheritanceClaim(token) {
  if (typeof token !== "string" || !token) return "";
  const claim = randomUUID();
  accessInheritanceClaims.set(claim, { token, webContentsId: 0 });
  return claim;
}

function bindAccessInheritanceClaim(claim, win) {
  if (!claim) return;
  const inherited = accessInheritanceClaims.get(claim);
  if (!inherited || !win || win.isDestroyed()) return;
  inherited.webContentsId = win.webContents.id;
}

const DEFAULT_WINDOW_BOUNDS = { width: 1120, height: 720 };

function windowStatePath() {
  return path.join(app.getPath("userData"), "window-state.json");
}

function windowAppearancePath() {
  return path.join(app.getPath("userData"), "window-appearance.json");
}

function applyThemePreference(preference) {
  nativeTheme.themeSource = preference === "light" || preference === "dark" ? preference : "system";
}

function loadStoredThemePreference() {
  try {
    const saved = JSON.parse(require("node:fs").readFileSync(windowAppearancePath(), "utf8"));
    if (saved?.preference === "light" || saved?.preference === "dark") return saved.preference;
  } catch {}
  return "system";
}

function loadWindowState() {
  const fallback = { bounds: { ...DEFAULT_WINDOW_BOUNDS }, maximized: false, fullscreen: false };
  let saved;
  try {
    saved = JSON.parse(require("node:fs").readFileSync(windowStatePath(), "utf8"));
  } catch {
    return fallback;
  }
  const bounds = objectOrEmpty(saved.bounds);
  const width = Math.max(MIN_WINDOW_BOUNDS.width, numberValue(bounds.width, DEFAULT_WINDOW_BOUNDS.width));
  const height = Math.max(MIN_WINDOW_BOUNDS.height, numberValue(bounds.height, DEFAULT_WINDOW_BOUNDS.height));
  const state = {
    bounds: { x: bounds.x, y: bounds.y, width, height },
    maximized: Boolean(saved.maximized),
    fullscreen: Boolean(saved.fullscreen),
  };
  if (Number.isFinite(state.bounds.x) && Number.isFinite(state.bounds.y)) {
    const { screen } = require("electron");
    const visible = screen.getAllDisplays().some((display) => {
      const area = display.workArea;
      return state.bounds.x < area.x + area.width
        && state.bounds.x + width > area.x
        && state.bounds.y < area.y + area.height
        && state.bounds.y + height > area.y;
    });
    if (!visible) {
      delete state.bounds.x;
      delete state.bounds.y;
    }
  }
  return state;
}

function peerClientWindowState() {
  const fallback = { bounds: { ...DEFAULT_WINDOW_BOUNDS }, maximized: false, fullscreen: false };
  const reference = clientWindows.select({ focusedWindow: BrowserWindow.getFocusedWindow() })?.win;
  if (!reference || reference.isDestroyed()) return fallback;
  const referenceBounds = reference.getNormalBounds();
  const display = screen.getDisplayMatching(referenceBounds);
  return {
    bounds: cascadeClientWindowBounds(referenceBounds, display?.workArea, DEFAULT_WINDOW_BOUNDS),
    maximized: false,
    fullscreen: false,
  };
}

function trackWindowState(win) {
  let timer = null;
  const capture = () => {
    timer = null;
    if (win.isDestroyed()) return;
    const state = {
      bounds: win.isMaximized() || win.isFullScreen() ? win.getNormalBounds() : win.getBounds(),
      maximized: win.isMaximized(),
      fullscreen: win.isFullScreen(),
    };
    require("node:fs").writeFile(windowStatePath(), JSON.stringify(state, null, 2), () => {});
  };
  const schedule = () => {
    if (timer) clearTimeout(timer);
    timer = setTimeout(capture, 400);
  };
  for (const event of ["resize", "move", "maximize", "unmaximize", "enter-full-screen", "leave-full-screen"]) {
    win.on(event, schedule);
  }
  win.on("close", () => {
    if (timer) clearTimeout(timer);
    capture();
  });
}

function attachContextMenu(win) {
  win.webContents.on("context-menu", (_event, params) => {
    const template = [];
    if (params.misspelledWord) {
      const suggestions = (params.dictionarySuggestions || []).slice(0, 5);
      if (suggestions.length) {
        template.push(...suggestions.map((suggestion) => ({
          label: suggestion,
          click: () => win.webContents.replaceMisspelling(suggestion),
        })));
      } else {
        template.push({ label: "No spelling suggestions", enabled: false });
      }
      template.push({
        label: "Add to dictionary",
        click: () => win.webContents.session.addWordToSpellCheckerDictionary(params.misspelledWord),
      }, { type: "separator" });
    }
    if (params.isEditable) {
      template.push(
        { role: "cut", enabled: params.editFlags.canCut },
        { role: "copy", enabled: params.editFlags.canCopy },
        { role: "paste", enabled: params.editFlags.canPaste },
        { type: "separator" },
        { role: "selectAll" },
      );
    } else if (params.selectionText.trim()) {
      template.push({ role: "copy" });
    }
    if (template.length) Menu.buildFromTemplate(template).popup({ window: win });
  });
}

function revealClientWindow(win) {
  if (!win || win.isDestroyed()) return null;
  if (win.isMinimized()) win.restore();
  if (!win.isVisible()) win.show();
  win.focus();
  return win;
}

function selectedClientWindow(accessToken, focusedWindow = BrowserWindow.getFocusedWindow()) {
  const entry = clientWindows.select({
    focusedWindow,
    ...(accessToken === undefined ? {} : { accessToken }),
  });
  return entry?.win ?? null;
}

function ensurePrimaryClientWindow() {
  return revealClientWindow(clientWindows.primaryEntry()?.win ?? createWindow());
}

function ensureClientWindowForAccess(accessToken, focusedWindow = BrowserWindow.getFocusedWindow()) {
  getClientAccessController().requireSession(accessToken);
  const existing = selectedClientWindow(accessToken, focusedWindow);
  if (existing) return existing;
  return createWindow(accessToken, { primary: !clientWindows.primaryEntry() });
}

function focusAppropriateClientWindow() {
  return revealClientWindow(selectedClientWindow(undefined) ?? createWindow());
}

function authenticatedClientWindowToken(win) {
  const entry = clientWindows.entryForWindow(win);
  if (!entry?.accessToken) return "";
  try {
    getClientAccessController().requireSession(entry.accessToken);
    return entry.accessToken;
  } catch {
    return "";
  }
}

function openNewClientWindowFromMenu() {
  const focused = BrowserWindow.getFocusedWindow();
  const source = clientWindows.entryForWindow(focused)?.win
    ?? clientWindows.select({ focusedWindow: focused })?.win;
  const token = authenticatedClientWindowToken(source);
  if (!token) return ensurePrimaryClientWindow();
  return createWindow(token, { primary: false });
}

function sendClientWindowEvent(win, channel, payload) {
  const publish = () => {
    if (!win.isDestroyed() && !win.webContents.isDestroyed()) win.webContents.send(channel, payload);
  };
  if (win.webContents.isLoadingMainFrame()) win.webContents.once("did-finish-load", publish);
  else publish();
}

const gotSingleInstanceLock = app.requestSingleInstanceLock();
if (!gotSingleInstanceLock) {
  app.quit();
} else {
  saveClientLaunchSelection(clientLaunchSelectionPath, { home: resolveOctopusHome(), codexHome: resolveOperatorCodexHome() });
  app.on("second-instance", () => {
    void app.whenReady().then(() => {
      ensureRuntimeHost();
      focusAppropriateClientWindow();
      void initializeRuntimeHost().catch((error) => appendClientLog("error", "client.startup_failed", {
        message: error instanceof Error ? error.message : String(error),
      }));
    }).catch((error) => appendClientLog("error", "client.startup_failed", {
      message: error instanceof Error ? error.message : String(error),
    }));
  });

  app.whenReady().then(() => {
    setAppIdentity();
    applyThemePreference(loadStoredThemePreference());
    grantLocalMediaPermissions();
    ensureRuntimeHost();
    createWindow();
    void initializeRuntimeHost().catch((error) => appendClientLog("error", "client.startup_failed", {
      message: error instanceof Error ? error.message : String(error),
    }));
    app.on("activate", () => {
      setAppIdentity();
      focusAppropriateClientWindow();
    });
  });
}

function grantLocalMediaPermissions() {
  const isLocalRenderer = (webContents) => {
    const url = webContents?.getURL?.() ?? "";
    return url.startsWith("http://127.0.0.1:") || url.startsWith("http://localhost:") || url.startsWith("file://");
  };
  session.defaultSession.setPermissionRequestHandler((webContents, permission, callback) => {
    callback(permission === "media" && isLocalRenderer(webContents));
  });
  session.defaultSession.setPermissionCheckHandler((webContents, permission) => (
    permission === "media" && isLocalRenderer(webContents)
  ));
}

app.on("window-all-closed", () => {
  if (process.platform !== "darwin") app.quit();
});

for (const signal of ["SIGINT", "SIGTERM"]) {
  process.once(signal, () => app.quit());
}

process.on("message", (message) => {
  if (message?.type === "octopus-dev-quit") app.quit();
});

app.on("before-quit", (event) => {
  if (quitReady) {
    closeClientResources();
    return;
  }
  event.preventDefault();
  if (!quitCleanupStarted) clientWindowClose.requestQuit();
});

function startClientQuitCleanup() {
  if (quitCleanupStarted) return;
  quitCleanupStarted = true;
  void (async () => {
    const startupRecovery = runtimeInitializationPromise;
    if (startupRecovery) {
      try {
        await startupRecovery;
      } catch (error) {
        await appendClientLog("error", "client.startup_failed", {
          message: error instanceof Error ? error.message : String(error),
        }).catch(() => {});
      }
    }
    if (relaunchRequested) app.relaunch();
    const interrupted = relaunchRequested ? 0 : terminalHost?.interruptLaunch() ?? 0;
    const stopObservation = interrupted > 0 && runtimeHost
      ? runtimeHost.waitUntilStopped()
      : Promise.resolve(true);
    try {
      const [stopped] = await Promise.all([stopObservation, terminalHost?.terminate()]);
      if (!stopped) throw new Error("Octopus runtime did not stop after its launch terminal received Ctrl-C and closed");
    } finally {
      runtimeHost?.close();
      terminalHost?.closeAll();
      closeClientResources();
      nativeBrowserPresentations?.stop();
      nativeBrowserDownloadHistory?.stop();
      nativeDesktopPermissions?.stop();
      nativeDesktopSettings?.stop();
      nativeDesktopPresentations?.stop();
      nativeClientPresentations?.stop();
      await nativeBrowserHost?.stop();
      nativeBrowserHost = null;
    }
  })().catch(async (error) => {
    await appendClientLog("error", "runtime.stop.failed", {
      message: error instanceof Error ? error.message : String(error),
    });
  }).finally(() => {
    quitReady = true;
    app.quit();
  });
}

function closeClientResources() {
  chatgptSignIn?.dispose();
  applicationConnectionHost?.dispose();
  clearGlobalDictationShortcut();
  cdpViewerManager?.closeAll();
  processViewerManager?.closeAll();
  clientSurfaceDocumentStore?.close();
  clientExtensionStore?.close();
  void clientArtifactStreamStore?.abortAll();
  void clientAttachmentStagingStore?.abortAll();
  for (const store of projectionStores.values()) store.close?.();
  projectionStores.clear();
}

function clearGlobalDictationShortcut() {
  if (!globalDictationShortcut) return;
  globalShortcut.unregister(globalDictationShortcut);
  globalDictationShortcut = "";
  globalDictationAccessToken = "";
}

function clientShortcutToElectronAccelerator(value) {
  if (typeof value !== "string") return "";
  const parts = value.split("+").map((part) => part.trim()).filter(Boolean);
  const code = parts.pop();
  if (!code) return "";
  const acceleratorKeys = {
    Backquote: "`",
    BracketLeft: "[",
    BracketRight: "]",
    Comma: ",",
    Period: ".",
    Slash: "/",
    Semicolon: ";",
    Quote: "'",
    Backslash: "\\",
    Minus: "-",
    Equal: "=",
    Enter: "Enter",
    Escape: "Escape",
    Space: "Space",
    ArrowUp: "Up",
    ArrowDown: "Down",
    ArrowLeft: "Left",
    ArrowRight: "Right",
    Tab: "Tab",
    Backspace: "Backspace",
    Delete: "Delete",
  };
  const key = /^Key[A-Z]$/.test(code)
    ? code.slice(3)
    : /^Digit[0-9]$/.test(code)
      ? code.slice(5)
      : acceleratorKeys[code] || (/^F(?:[1-9]|1[0-2])$/.test(code) ? code : "");
  if (!key) return "";
  const modifierMap = { Mod: "CommandOrControl", Control: "Control", Meta: "Command", Alt: "Alt", Shift: "Shift" };
  const modifiers = parts.flatMap((part) => modifierMap[part] ? [modifierMap[part]] : []);
  if (modifiers.length !== parts.length || modifiers.length === 0) return "";
  return [...new Set(modifiers), key].join("+");
}

function ensureRuntimeHost() {
  if (runtimeHost) return runtimeHost;
  const host = createRuntimeHost({
    home: resolveOctopusHome,
    sourceRegistry: resolveSourceRegistry,
    runtimeBinary: resolveRuntimeBinary,
    repoRoot: resolveRepoRoot,
    launchCwd: resolveRuntimeLaunchCwd,
    launchCommand: resolveRuntimeLaunchCommand,
    launchEnv: resolveRuntimeLaunchEnv,
    endpoint: runtimeControlSocketPath,
  });
  runtimeHost = host;
  let runtimeProjectionHasBeenReady = false;
  host.on("state", (state) => {
    const projectionReady = state.phase === "ready";
    const projectionChanged = projectionReady && !runtimeProjectionHasBeenReady;
    if (projectionReady) runtimeProjectionHasBeenReady = true;
    if (projectionChanged) projectionStores.get("live")?.invalidate(false);
    publishRuntimeEvent("octopus:runtime-host-state", state);
  });
  host.observe();
  return host;
}

async function initializeRuntimeHost() {
  const host = ensureRuntimeHost();
  if (runtimeInitializationComplete) return host;
  if (!runtimeInitializationPromise) {
    runtimeInitializationPromise = (async () => {
      const octopusHome = resolveOctopusHome();
      const packageRoots = clientSetupPackageRoots();
      const assertStopped = async () => {
          if (resolveOctopusHome() !== octopusHome) throw new Error("Home changed during workspace recovery.");
          const state = await host.probe();
          if (state.phase !== "unmanaged" || state.ready || state.ownership !== "none" || state.pid !== null || state.error) {
            throw new Error("Close the running Octopus runtime before recovering its interrupted workspace transactions.");
          }
        };
      const releaseGuard = registerLocalPackageMutationGuard({ stateRoot: packageRoots.stateRoot, guard: assertStopped });
      try {
        const state = await host.probe();
        if (state.pid === null) {
          await assertStopped();
          await recoverLocalPackageTransactions(packageRoots);
          await assertStopped();
          const registryRoot = path.join(octopusHome, "registry");
          await recoverWorkspaceTransactions({ octopusHomeRoot: octopusHome, registryRoot,
            publication: workspaceStorage(octopusHome),
            retirementBarrier: {
              verifyBefore: (context) => taskRetirementBarrier(octopusHome).then((barrier) => barrier.verifyBefore(context)),
              withHeldProof: (context, operation) => taskRetirementBarrier(octopusHome).then((barrier) => barrier.withHeldProof(context, operation)),
            },
          });
        }
      } finally {
        releaseGuard();
      }
      // Only the idle package host starts here. Room engines require admitted tool requests.
      try { await startNativeBrowserEngine(); } catch (error) {
        await appendClientLog("error", "browser.native_host.unavailable", { message: error.message });
      }
      projectionStores.get("live")?.invalidate();
      runtimeInitializationComplete = true;
      if (nativeBrowserHost) void getNativeBrowserSurface().revalidate();
      return host;
    })().finally(() => {
      runtimeInitializationPromise = null;
    });
  }
  return runtimeInitializationPromise;
}

function getTerminalHost() {
  if (!terminalHost) {
    terminalHost = createTerminalHost();
    terminalHost.on("data", (payload) => {
      publishTerminalEvent("octopus:terminal-data", payload);
    });
    terminalHost.on("exit", (payload) => {
      publishTerminalEvent("octopus:terminal-exit", payload);
    });
  }
  return terminalHost;
}

function publishTerminalEvent(channel, payload) {
  const owner = payload?.owner;
  if (!owner || !Number.isSafeInteger(owner.senderId)) return;
  if (owner.scope === "access") {
    try {
      getClientAccessController().requireSession(owner.token, ["live"]);
    } catch {
      terminalHost?.closeOwner(owner.token);
      return;
    }
  }
  const win = BrowserWindow.getAllWindows().find((candidate) => (
    !candidate.isDestroyed() && candidate.webContents.id === owner.senderId
  ));
  if (!win || win.webContents.isDestroyed()) {
    terminalHost?.closeSender(owner.senderId);
    return;
  }
  const { owner: _owner, ...event } = payload;
  win.webContents.send(channel, event);
}

function terminalOwner(event, token) {
  return { scope: "access", senderId: event.sender.id, token };
}

function requireRuntimeStateAccess(event, token) {
  if (typeof token !== "string" || !token) {
    if (!primaryWindow || primaryWindow.isDestroyed() || primaryWindow.webContents.id !== event.sender.id) {
      throw new Error("Pre-password runtime state belongs to the primary Octopus window");
    }
    return { mode: "bootstrap", profile: null };
  }
  const session = getClientAccessController().requireSession(token, ["live"]);
  return { mode: "live", profile: session.profile };
}

function launchTerminalOwner(event, token) {
  const access = requireRuntimeStateAccess(event, token);
  const entry = clientWindows.entryForWebContents(event.sender.id);
  if (!entry || (access.mode === "bootstrap" && !entry.primary)) {
    throw new Error("The Octopus boot terminal requires a full client window with the current access authority");
  }
  return { scope: "launch", senderId: event.sender.id, token: "" };
}

function publishRuntimeEvent(channel, payload) {
  if (quitCleanupStarted) return false;
  let published = false;
  for (const [senderId, subscription] of runtimeEventSubscriptions) {
    if (subscription.sender.isDestroyed()) {
      runtimeEventSubscriptions.delete(senderId);
      continue;
    }
    try {
      requireRuntimeStateAccess({ sender: subscription.sender }, subscription.token);
    } catch {
      runtimeEventSubscriptions.delete(senderId);
      continue;
    }
    subscription.sender.send(channel, payload);
    published = true;
  }
  return published;
}

function labRuntimeHostState(token) {
  if (typeof token !== "string" || !token) return null;
  return getClientAccessController().requireSession(token).profile === "lab"
    ? getProjectionStore("lab").runtimeHostState()
    : null;
}

function subscribeRuntimeEvents(event, token) {
  const lab = labRuntimeHostState(token);
  if (lab) {
    runtimeEventSubscriptions.delete(event.sender.id);
    event.sender.send("octopus:runtime-host-state", lab);
    return { subscribed: true };
  }
  requireRuntimeStateAccess(event, token);
  runtimeEventSubscriptions.set(event.sender.id, { sender: event.sender, token });
  event.sender.once("destroyed", () => {
    runtimeEventSubscriptions.delete(event.sender.id);
  });
  if (runtimeHost) event.sender.send("octopus:runtime-host-state", runtimeHost.snapshot());
  return { subscribed: true };
}

function unsubscribeRuntimeEvents(event) {
  runtimeEventSubscriptions.delete(event.sender.id);
  return { subscribed: false };
}

async function runtimeBootStatusHandler(event, token) {
  const lab = labRuntimeHostState(token);
  if (lab) return lab;
  requireRuntimeStateAccess(event, token);
  return ensureRuntimeHost().probe();
}

function trackTerminalSender(sender) {
  if (terminalSenderIds.has(sender.id)) return;
  const senderId = sender.id;
  terminalSenderIds.add(senderId);
  sender.once("destroyed", () => {
    terminalSenderIds.delete(senderId);
    terminalHost?.closeSender(senderId);
  });
}

ipcMain.handle("octopus:transport-defaults", async (_event, token) => {
  const { profile } = getClientAccessController().requireSession(token);
  const octopusHome = resolveOctopusHome();
  return profile === "lab"
    ? { endpoint: getProjectionStore("lab").runtimeHostState().endpoint, repoRoot: null, octopusHome: null, registryRoot: null, stateRoot: null, ledgerPath: null }
    : {
      endpoint: defaultRuntimeEndpoint(),
      repoRoot: resolveRepoRoot(),
      octopusHome,
      registryRoot: path.join(octopusHome, "registry"),
      stateRoot: path.join(octopusHome, "state"),
      ledgerPath: path.join(octopusHome, "ledger", "movements.jsonl"),
    };
});

ipcMain.handle("octopus:app-info", async () => clientApplicationInfo());

ipcMain.handle("octopus:support-open", async (event, token) => {
  if (!clientWindows.entryForWebContents(event.sender.id) || event.senderFrame !== event.sender.mainFrame) throw new Error("Problem reports must be opened from the Octopus client.");
  getClientAccessController().requireSession(token);
  getClientSupportHost().open(token);
});

ipcMain.handle("octopus:client-doctor", async (_event, token) => {
  getClientAccessController().requireSession(token, ["live"]);
  return runClientDoctor({ roots: clientSetupPackageRoots() });
});

ipcMain.handle("octopus:boot-status", runtimeBootStatusHandler);

ipcMain.handle("octopus:client-relaunch", async (_event, token) => {
  getClientAccessController().requireSession(token, ["live"]);
  if (quitCleanupStarted) throw new Error("The client is already closing.");
  relaunchRequested = true;
  setImmediate(() => app.quit());
  return { relaunching: true };
});

ipcMain.handle("octopus:runtime-events-subscribe", subscribeRuntimeEvents);

ipcMain.handle("octopus:runtime-events-unsubscribe", unsubscribeRuntimeEvents);

ipcMain.handle("octopus:terminal-open", async (event, token, request) => {
  getClientAccessController().requireSession(token, ["live"]);
  trackTerminalSender(event.sender);
  const launch = { ...request, cwd: request?.cwd || resolveRepoRoot() || os.homedir() };
  return getTerminalHost().open(terminalOwner(event, token), launch);
});

ipcMain.handle("octopus:terminal-write", async (event, token, terminalId, sessionId, data) => {
  getClientAccessController().requireSession(token, ["live"]);
  return getTerminalHost().write(terminalOwner(event, token), terminalId, sessionId, data);
});

ipcMain.handle("octopus:terminal-resize", async (event, token, terminalId, sessionId, request) => {
  getClientAccessController().requireSession(token, ["live"]);
  return getTerminalHost().resize(terminalOwner(event, token), terminalId, sessionId, request);
});

ipcMain.handle("octopus:terminal-close", async (event, token, terminalId) => {
  getClientAccessController().requireSession(token, ["live"]);
  return { closed: getTerminalHost().close(terminalOwner(event, token), terminalId) };
});

ipcMain.handle("octopus:launch-terminal-open", async (event, token, request) => {
  const owner = launchTerminalOwner(event, token);
  trackTerminalSender(event.sender);
  const existing = terminalHost?.openExisting(owner, {
    terminalId: "launch",
    cols: request?.cols,
    rows: request?.rows,
  }) ?? null;
  if (existing) return existing;
  const host = await initializeRuntimeHost();
  const state = await host.probe();
  if (state.ready) throw new Error("Octopus runtime is already ready for this Home");
  return getTerminalHost().open(owner, {
    terminalId: "launch",
    cwd: state.launchCwd,
    env: state.launchEnv,
    preparedCommand: state.launchCommand,
    cols: request?.cols,
    rows: request?.rows,
  });
});

ipcMain.handle("octopus:launch-terminal-write", async (event, token, sessionId, data) => (
  getTerminalHost().write(launchTerminalOwner(event, token), "launch", sessionId, data)
));

ipcMain.handle("octopus:launch-terminal-resize", async (event, token, sessionId, request) => (
  getTerminalHost().resize(launchTerminalOwner(event, token), "launch", sessionId, request)
));

let chatgptSignIn = null;
function getChatgptSignIn() {
  if (!chatgptSignIn) {
    chatgptSignIn = createChatgptSignIn({
      codexBinary: () => path.join(resolveInstallationRoot(), "bin", "codex"),
      codexHome: resolveOperatorCodexHome,
      octopusHome: resolveOctopusHome,
      version: () => app.getVersion(),
      openExternal: (url) => shell.openExternal(url),
      onAccountChanged: () => {
        for (const win of BrowserWindow.getAllWindows()) {
          if (!win.isDestroyed()) win.webContents.send("octopus:chatgpt-account-changed");
        }
      },
    });
  }
  return chatgptSignIn;
}

ipcMain.handle("octopus:chatgpt-account", async (event, token) => {
  requireRuntimeStateAccess(event, token);
  return getChatgptSignIn().account();
});

ipcMain.handle("octopus:chatgpt-rate-limits", async (event, token) => {
  requireRuntimeStateAccess(event, token);
  return getChatgptSignIn().rateLimits();
});

ipcMain.handle("octopus:chatgpt-logout", async (_event, token) => {
  getClientAccessController().requireSession(token, ["live"]);
  return getChatgptSignIn().logout();
});

ipcMain.handle("octopus:chatgpt-sign-in-start", async (event, token) => {
  requireRuntimeStateAccess(event, token);
  return getChatgptSignIn().start();
});

ipcMain.handle("octopus:chatgpt-sign-in-status", async (event, token, loginId) => {
  requireRuntimeStateAccess(event, token);
  return getChatgptSignIn().status(loginId);
});

ipcMain.handle("octopus:chatgpt-sign-in-cancel", async (event, token, loginId) => {
  requireRuntimeStateAccess(event, token);
  return getChatgptSignIn().cancel(loginId);
});

ipcMain.handle("octopus:clipboard-write", async (_event, token, value) => {
  getClientAccessController().requireSession(token);
  if (typeof value !== "string") throw new Error("clipboard value must be a string");
  clipboard.writeText(value);
  return { copied: true };
});

ipcMain.handle("octopus:client-log", async (_event, token, request) => {
  const { profile } = getClientAccessController().requireSession(token);
  const level = ["info", "warning", "error"].includes(request?.level) ? request.level : "info";
  const event = String(request?.event || "renderer.event").slice(0, 160);
  const message = String(request?.message || "").slice(0, 8_000);
  await appendClientLog(level, event, { profile, message });
  return { written: true };
});

ipcMain.handle("octopus:open-client-logs", async (_event, token) => {
  const { profile } = getClientAccessController().requireSession(token);
  const logPath = await appendClientLog("info", "diagnostics.logs_opened", { profile });
  const result = await shell.openPath(path.dirname(logPath));
  if (result) throw new Error(result);
  return { path: logPath };
});

ipcMain.handle("octopus:client-attachments-select", async (event, token, request) => {
  getClientAccessController().requireSession(token);
  const win = BrowserWindow.fromWebContents(event.sender);
  if (!win || win.isDestroyed()) throw new Error("Attachment selection requires an active window.");
  return selectClientAttachments(dialog, win, request, {
    stageBytes: (metadata, bytes) => getClientAttachmentStagingStore().stageBytes(token, metadata, bytes),
    stageFile: (metadata, sourcePath) => getClientAttachmentStagingStore().stageFile(token, metadata, sourcePath),
    discard: async (handle) => {
      const store = getClientAttachmentStagingStore();
      await store.discard(await store.consume(token, handle));
    },
  });
});

ipcMain.handle("octopus:stage-dropped-attachments", async (event, token, paths) => {
  getClientAccessController().requireSession(token);
  const win = BrowserWindow.fromWebContents(event.sender);
  if (!win || win.isDestroyed()) throw new Error("Attachment drop requires an active window.");
  const store = getClientAttachmentStagingStore();
  return stageClientAttachmentPaths(paths, {
    stageBytes: (metadata, bytes) => store.stageBytes(token, metadata, bytes),
    stageFile: (metadata, sourcePath) => store.stageFile(token, metadata, sourcePath),
    discard: async (handle) => store.discard(await store.consume(token, handle)),
  });
});

ipcMain.handle("octopus:client-attachment-preview", async (event, token, request) => {
  const win = BrowserWindow.fromWebContents(event.sender);
  const current = () => {
    getClientAccessController().requireSession(token);
    if (!win || win.isDestroyed() || event.sender.isDestroyed() || win.webContents !== event.sender
      || event.senderFrame !== event.sender.mainFrame) throw new Error("Attachment preview requires its active client window.");
  };
  current();
  const preview = await getClientAttachmentStagingStore().readPreview(token, request);
  current();
  return preview;
});

ipcMain.handle("octopus:attachment-stages-discard", async (_event, token, handles) => {
  getClientAccessController().requireSession(token);
  if (!Array.isArray(handles) || handles.length > 12) throw new Error("Invalid attachment cleanup request.");
  const store = getClientAttachmentStagingStore();
  const results = await Promise.allSettled(handles.map(async (handle) => store.discard(await store.consume(token, handle))));
  const errors = results.filter((result) => result.status === "rejected").map((result) => result.reason);
  if (errors.length) throw new AggregateError(errors, "Some attachment stages could not be discarded.");
});

ipcMain.handle("octopus:attachment-stage-begin", async (_event, token, request) => {
  getClientAccessController().requireSession(token);
  return getClientAttachmentStagingStore().begin(token, request);
});

ipcMain.handle("octopus:attachment-stage-append", async (_event, token, request) => {
  getClientAccessController().requireSession(token);
  return getClientAttachmentStagingStore().append(token, { ...request, bytes: bufferFromIpcBytes(request?.bytes) });
});

ipcMain.handle("octopus:attachment-stage-finalize", async (_event, token, request) => {
  getClientAccessController().requireSession(token);
  return getClientAttachmentStagingStore().finalize(token, request);
});

ipcMain.handle("octopus:attachment-stage-abort", async (_event, token, request) => {
  getClientAccessController().requireSession(token);
  return getClientAttachmentStagingStore().abort(token, request);
});

ipcMain.handle("octopus:client-directory-select", async (event, token, request) => {
  getClientAccessController().requireSession(token, ["live"]);
  const win = BrowserWindow.fromWebContents(event.sender);
  if (!win || win.isDestroyed()) throw new Error("Folder selection requires an active window.");
  const mode = request?.mode === "file" ? "file" : "directory";
  const title = boundedOptionalField(request?.title, "Folder selection title", 96)
    || (mode === "file" ? "Choose a file" : "Choose a folder");
  const buttonLabel = boundedOptionalField(request?.buttonLabel, "Folder selection button label", 48) || "Choose";
  const result = await dialog.showOpenDialog(win, {
    buttonLabel,
    message: title,
    properties: [...(mode === "file" ? ["openFile"] : ["openDirectory", "createDirectory"]), ...(request?.allowMultiple === true ? ["multiSelections"] : [])],
  });
  const selected = result.filePaths[0];
  return result.canceled || !selected
    ? { canceled: true, path: "", paths: [] }
    : { canceled: false, path: selected, paths: result.filePaths };
});

ipcMain.handle("octopus:client-file-save", async (event, token, request) => {
  getClientAccessController().requireSession(token);
  const browserWindow = BrowserWindow.fromWebContents(event.sender);
  if (!browserWindow || browserWindow.isDestroyed()) throw new Error("Saving requires an active window.");
  return saveClientFile({
    browserWindow,
    bytes: bufferFromIpcBytes(request?.bytes),
    dialog,
    filename: request?.filename,
  });
});

ipcMain.handle("octopus:client-file-download-copy", async (_event, token, request) => {
  getClientAccessController().requireSession(token);
  return downloadClientFileCopy({
    bytes: bufferFromIpcBytes(request?.bytes),
    filename: request?.filename,
    root: app.getPath("downloads"),
  });
});

ipcMain.handle("octopus:client-file-open", async (_event, token, request) => {
  getClientAccessController().requireSession(token);
  return openClientFile({
    action: request?.action,
    bytes: bufferFromIpcBytes(request?.bytes),
    filename: request?.filename,
    root: path.join(app.getPath("temp"), "Octopus", "Artifacts"),
    shell,
  });
});

ipcMain.handle("octopus:client-file-open-targets", async (_event, token, request) => {
  getClientAccessController().requireSession(token);
  return listClientFileOpenTargets({
    app,
    bytes: bufferFromIpcBytes(request?.bytes),
    downloadsRoot: app.getPath("downloads"),
    filename: request?.filename,
    root: path.join(app.getPath("temp"), "Octopus", "Artifacts"),
  });
});

ipcMain.handle("octopus:client-file-open-with", async (_event, token, request) => {
  getClientAccessController().requireSession(token);
  return openClientFileWithApplication({
    applicationId: request?.applicationId,
    bytes: bufferFromIpcBytes(request?.bytes),
    filename: request?.filename,
    root: path.join(app.getPath("temp"), "Octopus", "Artifacts"),
  });
});

ipcMain.on("octopus:client-file-start-drag", (event, token, request) => {
  let profile = "unknown";
  try {
    ({ profile } = getClientAccessController().requireSession(token));
  } catch (error) {
    void appendClientLog("warning", "artifact.drag_rejected", { profile, message: error instanceof Error ? error.message : String(error) });
    return;
  }
  void materializeClientFile({
    bytes: bufferFromIpcBytes(request?.bytes),
    filename: request?.filename,
    root: path.join(app.getPath("temp"), "Octopus", "Artifacts"),
  }).then(async (target) => {
    if (event.sender.isDestroyed()) return;
    const icon = await app.getFileIcon(target, { size: "small" });
    event.sender.startDrag({ file: target, icon });
  }).catch((error) => {
    void appendClientLog("warning", "artifact.drag_failed", { profile, message: error instanceof Error ? error.message : String(error) });
  });
});

ipcMain.on("octopus:client-window-bootstrap", (event) => {
  event.returnValue = clientWindows.bootstrapForWebContents(event.sender.id);
});

ipcMain.on("octopus:client-pane-transfer-begin", (event, token, request) => {
  try {
    getClientAccessController().requireSession(token);
    clientWindows.setAccessToken(event.sender.id, token);
    event.returnValue = { ok: true, value: clientWindows.beginPaneTransfer(event.sender.id, request) };
  } catch (error) {
    event.returnValue = {
      ok: false,
      error: { message: error instanceof Error ? error.message : String(error) },
    };
  }
});

ipcMain.on("octopus:access-inherit", (event, claimValue) => {
  const claim = typeof claimValue === "string" ? claimValue : "";
  const inherited = accessInheritanceClaims.get(claim);
  event.returnValue = inherited?.webContentsId === event.sender.id ? inherited.token : "";
});

function bindClientWindowAccessToken(event, token) {
  if (!clientWindows.entryForWebContents(event.sender.id)) return;
  clientWindows.setAccessToken(event.sender.id, token);
}

function requireClientLockWindow(event, token) {
  const entry = clientWindows.entryForWebContents(event.sender.id);
  if (!entry?.accessToken || entry.accessToken !== token) {
    throw new Error("Client access lock belongs to its authenticated full client window.");
  }
  return entry;
}

ipcMain.handle("octopus:access-status", async (event, token) => {
  const status = getClientAccessController().status(token);
  if (status.authenticated) bindClientWindowAccessToken(event, token);
  return status;
});
ipcMain.handle("octopus:access-configure", async (event, password) => {
  const result = getClientAccessController().configure(password);
  clearTouchIdEnrollment();
  bindClientWindowAccessToken(event, result.token);
  return result;
});
ipcMain.handle("octopus:access-unlock", async (event, password) => {
  const result = getClientAccessController().unlock(password);
  bindClientWindowAccessToken(event, result.token);
  return result;
});
ipcMain.handle("octopus:touch-id-status", async () => touchIdStatus());
ipcMain.handle("octopus:touch-id-set-enabled", async (_event, token, enabled) => {
  getClientAccessController().requireSession(token, ["live"]);
  if (typeof enabled !== "boolean") throw new Error("Touch ID preference requires an explicit enabled state.");
  writeTouchIdPreference();
  if (!enabled) {
    clearTouchIdEnrollment();
    return touchIdStatus();
  }
  await promptTouchId("Enable Touch ID to unlock Octopus");
  writeTouchIdEnrollment();
  return touchIdStatus();
});
ipcMain.handle("octopus:touch-id-unlock", async (event) => {
  const enrollment = readTouchIdEnrollmentRecord();
  const fingerprint = getClientAccessController().credentialFingerprint();
  if (!enrollment || !fingerprint || enrollment.credentialFingerprint !== fingerprint) {
    throw new Error("Touch ID is not enabled for the current Administrator password.");
  }
  await promptTouchId("Unlock Octopus");
  const currentEnrollment = readTouchIdEnrollmentRecord();
  const currentFingerprint = getClientAccessController().credentialFingerprint();
  if (
    !currentEnrollment
    || !currentFingerprint
    || currentEnrollment.credentialFingerprint !== currentFingerprint
    || currentEnrollment.credentialFingerprint !== enrollment.credentialFingerprint
    || currentEnrollment.sealedVerifier !== enrollment.sealedVerifier
  ) {
    throw new Error("Touch ID enrollment changed before unlock completed.");
  }
  verifyTouchIdEnrollment(currentEnrollment);
  const result = getClientAccessController().unlockWithTouchId();
  bindClientWindowAccessToken(event, result.token);
  return result;
});
ipcMain.handle("octopus:client-window-open", async (event, token, requestValue) => {
  getClientAccessController().requireSession(token);
  if (!clientWindows.entryForWebContents(event.sender.id)) {
    throw new Error("Only an authenticated full Octopus client window can open a peer window.");
  }
  clientWindows.setAccessToken(event.sender.id, token);
  const request = validateClientWindowOpenRequest(requestValue);
  const win = createWindow(token, {
    primary: false,
    ...(request.initialTarget ? { initialTarget: request.initialTarget } : {}),
  });
  const bootstrap = clientWindows.bootstrapForWebContents(win.webContents.id);
  if (!bootstrap || bootstrap.primary) {
    if (!win.isDestroyed()) win.close();
    throw new Error("Peer client window bootstrap registration failed.");
  }
  try {
    return await clientWindows.waitUntilReady(bootstrap.windowId);
  } catch (error) {
    if (!win.isDestroyed()) win.close();
    throw error;
  }
});
ipcMain.handle("octopus:client-window-ready", async (event, token, request) => {
  getClientAccessController().requireSession(token);
  const entry = clientWindows.entryForWebContents(event.sender.id);
  if (!entry || entry.primary || entry.accessToken !== token) {
    throw new Error("Client window readiness requires its authenticated peer bootstrap.");
  }
  return clientWindows.markReady(event.sender.id, request);
});
function currentClientWindowCloseSender(event) {
  const entry = clientWindows.entryForWebContents(event.sender.id);
  return entry && entry.win.webContents === event.sender && event.senderFrame === event.sender.mainFrame
    ? event.sender : null;
}
ipcMain.handle("octopus:client-window-close-register", (event, token) => {
  const sender = currentClientWindowCloseSender(event);
  if (!sender) throw new Error("Window close guard requires the current main client frame.");
  return clientWindowClose.register(sender, token);
});
ipcMain.handle("octopus:client-window-close-unregister", (event, token, request) => {
  const sender = currentClientWindowCloseSender(event);
  if (sender) clientWindowClose.unregister(sender, token, request);
});
ipcMain.handle("octopus:client-window-close-resolve", (event, token, request) => {
  const sender = currentClientWindowCloseSender(event);
  return sender ? clientWindowClose.resolve(sender, token, request) : { accepted: false };
});
ipcMain.handle("octopus:client-pane-transfer-accept", async (event, token, request) => {
  getClientAccessController().requireSession(token);
  clientWindows.setAccessToken(event.sender.id, token);
  return clientWindows.acceptPaneTransfer(event.sender.id, request);
});
ipcMain.handle("octopus:client-pane-transfer-commit", async (event, token, request) => {
  getClientAccessController().requireSession(token);
  clientWindows.setAccessToken(event.sender.id, token);
  const result = clientWindows.commitPaneTransfer(event.sender.id, request);
  const source = clientWindows.entryForWebContents(result.sourceWebContentsId);
  if (!source || source.win.webContents.isDestroyed()) {
    clientWindows.abortCommittedPaneTransfer(
      event.sender.id,
      request,
      new Error("Client pane transfer source window closed before commit delivery."),
    );
    return result.outcome;
  }
  try {
    source.win.webContents.send("octopus:client-pane-transfer-completed", result.completion);
  } catch (error) {
    clientWindows.abortCommittedPaneTransfer(event.sender.id, request, error);
  }
  return result.outcome;
});
ipcMain.on("octopus:client-pane-transfer-acknowledge", (event, token, request) => {
  try {
    getClientAccessController().requireSession(token);
    clientWindows.setAccessToken(event.sender.id, token);
    event.returnValue = { ok: true, value: clientWindows.acknowledgePaneTransfer(event.sender.id, request) };
  } catch (error) {
    event.returnValue = {
      ok: false,
      error: { message: error instanceof Error ? error.message : String(error) },
    };
  }
});
ipcMain.handle("octopus:client-pane-transfer-pending", async (event, token) => {
  getClientAccessController().requireSession(token);
  clientWindows.setAccessToken(event.sender.id, token);
  return clientWindows.pendingPaneTransferCompletions(event.sender.id);
});
ipcMain.handle("octopus:client-pane-transfer-destination-pending", async (event, token) => {
  getClientAccessController().requireSession(token);
  clientWindows.setAccessToken(event.sender.id, token);
  return clientWindows.pendingDestinationPaneTransfers(event.sender.id);
});
ipcMain.handle("octopus:client-pane-transfer-resume", async (event, token, request) => {
  getClientAccessController().requireSession(token);
  clientWindows.setAccessToken(event.sender.id, token);
  return clientWindows.resumePaneTransfer(event.sender.id, request);
});
ipcMain.handle("octopus:client-pane-transfer-finalize", async (event, token, request) => {
  getClientAccessController().requireSession(token);
  clientWindows.setAccessToken(event.sender.id, token);
  return clientWindows.finalizePaneTransfer(event.sender.id, request);
});
ipcMain.handle("octopus:client-pane-transfer-reject", (event, token, request) => {
  getClientAccessController().requireSession(token);
  const entry = clientWindows.entryForWebContents(event.sender.id);
  if (!entry || entry.accessToken !== token) throw new Error("Destination rejection requires its authenticated owner.");
  return clientWindows.rejectDestinationPaneTransfer(event.sender.id, request);
});
ipcMain.handle("octopus:client-pane-transfer-cancel", async (event, token, request) => {
  getClientAccessController().requireSession(token);
  clientWindows.setAccessToken(event.sender.id, token);
  return clientWindows.cancelPaneTransfer(event.sender.id, request);
});
for (const [channel, method] of [
  ["begin", "begin"], ["move", "move"], ["finish", "finish"],
  ["cancel", "cancel"], ["reattach", "reattach"], ["detach", "detach"], ["drop-zone", "registerDropZone"], ["anchor", "anchor"],
]) {
  ipcMain.handle(`octopus:client-pane-drag-${channel}`, (event, token, request) => {
    getClientAccessController().requireSession(token);
    const entry = clientWindows.entryForWebContents(event.sender.id);
    if (!entry || entry.accessToken !== token) throw new Error("Pane window drag requires its authenticated client owner.");
    return paneWindowDrag[method](event.sender.id, request);
  });
}
ipcMain.handle("octopus:client-pane-source-finalizations", (event, token) => {
  getClientAccessController().requireSession(token);
  const entry = clientWindows.entryForWebContents(event.sender.id);
  if (!entry || entry.accessToken !== token) throw new Error("Source finalizations require their authenticated owner.");
  return clientWindows.pendingSourceFinalizations(event.sender.id);
});
ipcMain.handle("octopus:client-pane-source-finalization-ack", (event, token, request) => {
  getClientAccessController().requireSession(token);
  const entry = clientWindows.entryForWebContents(event.sender.id);
  if (!entry || entry.accessToken !== token) throw new Error("Source finalization requires its authenticated window.");
  return clientWindows.acknowledgeSourceFinalization(event.sender.id, request);
});
ipcMain.handle("octopus:access-lock", async (event, token) => {
  requireClientLockWindow(event, token);
  const sender = currentClientWindowCloseSender(event);
  if (!sender) throw new Error("Logout requires the current main client frame.");
  const settlement = await clientWindowClose.requestLogout(sender, token, () => {
    const result = getClientAccessController().lock(token);
    clientSupportHost?.closeOwner(token);
    return Promise.resolve().then(async () => {
      const targets = clientWindows.entries().filter((entry) => entry.accessToken === token);
      const desktopOwners = new Set(targets.map((entry) => entry.webContentsId));
      for (const entry of [...workerEnvironmentWindows.values(), ...taskMonitorWindows.values()]) {
        if (entry.accessToken === token && !entry.win.isDestroyed()) desktopOwners.add(entry.win.webContents.id);
      }
      if (quickTaskAccessToken === token && quickTaskWindow && !quickTaskWindow.isDestroyed()) desktopOwners.add(quickTaskWindow.webContents.id);
      const retained = targets.find((entry) => entry.primary) ?? targets.find((entry) => entry.win.webContents === sender);
      for (const [id, subscription] of runtimeEventSubscriptions) if (subscription.token === token) runtimeEventSubscriptions.delete(id);
      clientWindows.clearAccessToken(token);
      for (const entry of targets) sendClientWindowEvent(entry.win, "octopus:client-access-locked", result);
      if (globalDictationAccessToken === token) clearGlobalDictationShortcut();
      cdpViewerManager?.closeOwner(token);
      await nativeBrowserSurface?.closeOwner(token);
      if (!nativeDesktopAccessEntries().length) { nativeBrowserPresentations?.revokeAll(); nativeBrowserDownloadHistory?.revokeAll(); nativeDesktopPresentations?.revokeAll(); }
      scheduleNativeDesktopPresentations();
      processViewerManager?.closeOwner(token);
      terminalHost?.closeOwner(token);
      if (quickTaskAccessToken === token && quickTaskWindow && !quickTaskWindow.isDestroyed()) quickTaskWindow.close();
      for (const entry of workerEnvironmentWindows.values()) if (entry.accessToken === token && !entry.win.isDestroyed()) entry.win.close();
      for (const entry of taskMonitorWindows.values()) if (entry.accessToken === token && !entry.win.isDestroyed()) entry.win.close();
      for (const entry of targets) if (entry !== retained && !entry.win.isDestroyed()) entry.win.close();
      const cleanup = await Promise.allSettled([
        clientArtifactStreamStore?.abortOwner(token),
        clientAttachmentStagingStore?.abortOwner(token),
        ...[...desktopOwners].map((ownerId) => { nativeDesktopPermissions?.closeOwner(ownerId); nativeDesktopSettings?.closeOwner(ownerId); return desktopSurfaceHost?.pauseOwner(ownerId); }),
      ]);
      const failures = cleanup.filter((item) => item.status === "rejected").map((item) => item.reason);
      if (failures.length) throw new AggregateError(failures, "Client access locked but resource cleanup failed.");
      return result;
    });
  });
  return settlement.accepted ? settlement.value : getClientAccessController().status(token);
});
ipcMain.handle("octopus:quick-task-open", async (_event, token) => {
  getClientAccessController().requireSession(token);
  createQuickTaskWindow(token);
  return { opened: true };
});
ipcMain.handle("octopus:quick-task-close", async (event, token) => {
  getClientAccessController().requireSession(token);
  const win = BrowserWindow.fromWebContents(event.sender);
  if (win && win === quickTaskWindow && !win.isDestroyed()) win.close();
  return { closed: true };
});
ipcMain.handle("octopus:quick-task-open-main", async (event, token, runIdValue) => {
  getClientAccessController().requireSession(token);
  const runId = stringValue(runIdValue);
  if (!runId || runId.length > 512 || /[\u0000-\u001f]/.test(runId)) throw new Error("A bounded run id is required");
  const source = BrowserWindow.fromWebContents(event.sender);
  const win = revealClientWindow(ensureClientWindowForAccess(token, source));
  sendClientWindowEvent(win, "octopus:open-run-requested", runId);
  if (source && source === quickTaskWindow && !source.isDestroyed()) source.close();
  return { opened: true, runId };
});
ipcMain.handle("octopus:worker-environment-window-open", async (_event, token, request) => {
  getClientAccessController().requireSession(token);
  const target = workerEnvironmentWindowTarget(request);
  createWorkerEnvironmentWindow(token, target);
  return { opened: true, ...target };
});
ipcMain.handle("octopus:worker-environment-window-open-main", async (event, token, request) => {
  getClientAccessController().requireSession(token);
  const target = workerEnvironmentWindowTarget(request);
  const source = BrowserWindow.fromWebContents(event.sender);
  const win = revealClientWindow(ensureClientWindowForAccess(token, source));
  sendClientWindowEvent(win, "octopus:open-worker-environment-requested", target);
  return { opened: true, ...target };
});
ipcMain.handle("octopus:worker-environment-window-pinned", async (event, token, enabled) => {
  getClientAccessController().requireSession(token);
  const win = BrowserWindow.fromWebContents(event.sender);
  const detached = [...workerEnvironmentWindows.values()].some((entry) => entry.win === win);
  if (!win || win.isDestroyed() || !detached) throw new Error("Pinning is available only in a Worker Environment popout.");
  const pinned = enabled === true;
  win.setAlwaysOnTop(pinned, "floating");
  return { pinned };
});
ipcMain.handle("octopus:task-monitor-window-open", async (_event, token, runIdValue) => {
  getClientAccessController().requireSession(token);
  const runId = stringValue(runIdValue);
  if (!runId || runId.length > 512 || /[\u0000-\u001f]/.test(runId)) throw new Error("A bounded run id is required.");
  createTaskMonitorWindow(token, runId);
  return { opened: true, runId };
});
ipcMain.handle("octopus:task-monitor-window-pinned", async (event, token, enabled) => {
  getClientAccessController().requireSession(token);
  const win = BrowserWindow.fromWebContents(event.sender);
  const monitor = [...taskMonitorWindows.values()].some((entry) => entry.win === win);
  if (!win || win.isDestroyed() || !monitor) throw new Error("Pinning is available only in a Task Monitor window.");
  const pinned = enabled === true;
  win.setAlwaysOnTop(pinned, "floating");
  return { pinned };
});
ipcMain.handle("octopus:global-dictation-shortcut", async (_event, token, request) => {
  getClientAccessController().requireSession(token);
  clearGlobalDictationShortcut();
  if (request?.enabled !== true) return { enabled: false, shortcut: "", registered: false };
  const shortcut = clientShortcutToElectronAccelerator(request?.shortcut);
  if (!shortcut) throw new Error("Global dictation requires a valid assigned shortcut.");
  const registered = globalShortcut.register(shortcut, () => {
    try {
      const win = revealClientWindow(ensureClientWindowForAccess(token));
      sendClientWindowEvent(win, "octopus:global-dictation-requested");
    } catch (error) {
      clearGlobalDictationShortcut();
      void appendClientLog("warning", "dictation.shortcut_rejected", {
        message: error instanceof Error ? error.message : String(error),
      });
    }
  });
  if (!registered) throw new Error(`${shortcut} is already owned by another application.`);
  globalDictationShortcut = shortcut;
  globalDictationAccessToken = token;
  return { enabled: true, shortcut, registered: true };
});
ipcMain.handle("octopus:native-command-shortcuts", async (_event, token, shortcuts) => {
  getClientAccessController().requireSession(token);
  const allowed = new Set([
    "new_chat",
    "open_command_menu",
    "open_settings",
    "navigate_back",
    "navigate_forward",
    "previous_surface",
    "next_surface",
    "toggle_sidebar",
    "toggle_split",
    "toggle_task_summary",
    "open_terminal",
    "open_browser",
    "open_changes",
    "start_dictation",
    "attach_files",
    "attach_projected_file",
    "copy_task_markdown",
    "copy_run_id",
    "copy_working_directory",
    "toggle_pin_task",
    "open_model_picker",
    "open_side_task",
  ]);
  nativeCommandShortcuts = Object.fromEntries(Object.entries(objectOrEmpty(shortcuts)).flatMap(([command, values]) => {
    if (!allowed.has(command) || !Array.isArray(values)) return [];
    const accelerator = values.map(clientShortcutToElectronAccelerator).find(Boolean);
    return accelerator ? [[command, accelerator]] : [];
  }));
  setApplicationMenu();
  return { configured: Object.keys(nativeCommandShortcuts).length };
});
ipcMain.handle("octopus:login-item-settings", async (_event, token, request) => {
  getClientAccessController().requireSession(token, ["live"]);
  const supported = app.isPackaged;
  if (request && Object.prototype.hasOwnProperty.call(request, "enabled")) {
    if (!supported) throw new Error("Open at login is available only in a packaged Octopus application.");
    app.setLoginItemSettings({ openAtLogin: request.enabled === true, openAsHidden: false });
  }
  const settings = app.getLoginItemSettings();
  return { supported, enabled: supported && settings.openAtLogin === true };
});
ipcMain.handle("octopus:lab-access-read", async (_event, token) => getClientAccessController().labAccess(token));
ipcMain.handle("octopus:lab-access-update", async (_event, token, password) => getClientAccessController().updateLabAccess(token, password));
ipcMain.handle("octopus:access-details", async (_event, token) => getClientAccessController().accessDetails(token));
ipcMain.handle("octopus:administrator-access-update", async (_event, token, request) => {
  const result = getClientAccessController().updateAdministratorAccess(token, request?.currentPassword, request?.password);
  clearTouchIdEnrollment();
  return result;
});

ipcMain.handle("octopus:request-microphone-access", async (_event, token) => {
  getClientAccessController().requireSession(token);
  if (process.platform !== "darwin") {
    return { granted: true, status: "browser-managed" };
  }
  const status = systemPreferences.getMediaAccessStatus("microphone");
  if (status === "granted") return { granted: true, status };
  if (status !== "not-determined" && status !== "unknown") {
    return { granted: false, status };
  }
  const granted = await systemPreferences.askForMediaAccess("microphone");
  return {
    granted,
    status: granted ? "granted" : systemPreferences.getMediaAccessStatus("microphone"),
  };
});

ipcMain.handle("octopus:camera-access-status", async (_event, token) => {
  getClientAccessController().requireSession(token);
  if (process.platform !== "darwin") return { granted: true, status: "browser-managed" };
  const status = systemPreferences.getMediaAccessStatus("camera");
  return { granted: status === "granted", status };
});

ipcMain.handle("octopus:gesture-trace-save", async (_event, token, text) => {
  getClientAccessController().requireSession(token, ["live"]);
  return saveGestureTrace({ root: clientProfileRoot, text });
});

ipcMain.handle("octopus:gesture-trace-reveal", async (_event, token, target) => {
  getClientAccessController().requireSession(token, ["live"]);
  return revealGestureTrace({ root: clientProfileRoot, target, shell });
});

ipcMain.handle("octopus:request-camera-access", async (_event, token) => {
  getClientAccessController().requireSession(token);
  if (process.platform !== "darwin") {
    return { granted: true, status: "browser-managed" };
  }
  const status = systemPreferences.getMediaAccessStatus("camera");
  if (status === "granted") return { granted: true, status };
  if (status !== "not-determined" && status !== "unknown") {
    return { granted: false, status };
  }
  const granted = await systemPreferences.askForMediaAccess("camera");
  return {
    granted,
    status: granted ? "granted" : systemPreferences.getMediaAccessStatus("camera"),
  };
});

ipcMain.handle("octopus:client-context-menu", (event, request) => clientContextMenu.show(event, request));
ipcMain.handle("octopus:client-context-menu-cancel", (event, request) => clientContextMenu.cancel(event, request));

ipcMain.handle("octopus:window-state", async (event) => {
  const win = BrowserWindow.fromWebContents(event.sender);
  return windowStateSnapshot(win);
});

ipcMain.handle("octopus:power-save-blocker", async (event, token, enabled) => {
  getClientAccessController().requireSession(token);
  const key = event.sender.id;
  const current = powerSaveBlockers.get(key);
  if (enabled === true) {
    if (typeof current === "number" && powerSaveBlocker.isStarted(current)) return { enabled: true };
    const id = powerSaveBlocker.start("prevent-app-suspension");
    powerSaveBlockers.set(key, id);
    event.sender.once("destroyed", () => {
      const active = powerSaveBlockers.get(key);
      if (typeof active === "number" && powerSaveBlocker.isStarted(active)) powerSaveBlocker.stop(active);
      powerSaveBlockers.delete(key);
    });
    return { enabled: true };
  }
  if (typeof current === "number" && powerSaveBlocker.isStarted(current)) powerSaveBlocker.stop(current);
  powerSaveBlockers.delete(key);
  return { enabled: false };
});

ipcMain.on("octopus:history-navigation-state", (event, state) => {
  const win = BrowserWindow.fromWebContents(event.sender);
  if (!win || win.isDestroyed()) return;
  historyNavigationStates.set(win, {
    canNavigateBack: state?.canNavigateBack === true,
    canNavigateForward: state?.canNavigateForward === true,
  });
});

ipcMain.on("octopus:window-control", (event, action) => {
  const win = BrowserWindow.fromWebContents(event.sender);
  if (!win || win.isDestroyed()) return;
  if (action === "close") win.close();
  else if (action === "minimize") win.minimize();
  else if (action === "zoom") win.setFullScreen(!win.isFullScreen());
});

ipcMain.on("octopus:window-appearance", (event, appearance) => {
  const win = BrowserWindow.fromWebContents(event.sender);
  if (!win || win.isDestroyed()) return;
  windowAppearances.set(win, {
    theme: appearance?.theme === "light" ? "light" : "dark",
    translucent: appearance?.translucent === true,
  });
  const preference = appearance?.preference === "light" || appearance?.preference === "dark"
    ? appearance.preference
    : "system";
  applyThemePreference(preference);
  require("node:fs").writeFile(windowAppearancePath(), JSON.stringify({ preference }, null, 2), () => {});
  applyWindowBackdrop(win, true);
});

ipcMain.handle("octopus:validate-runtime-endpoint", async (_event, token, request) => {
  getClientAccessController().requireSession(token, ["live"]);
  const octopusHome = resolveOctopusHome();
  const endpoint = stringValue(request?.endpoint);
  try {
    return {
      valid: true,
      endpoint: validateArtifactEndpoint(endpoint, octopusHome, OPERATOR_CHAT_INTERFACE),
    };
  } catch (error) {
    return {
      valid: false,
      endpoint,
      error: error instanceof Error ? error.message : String(error),
    };
  }
});

ipcMain.handle("octopus:provider-interfaces", async (_event, token) => {
  const { profile } = getClientAccessController().requireSession(token);
  if (profile === "live") await initializeRuntimeHost();
  const repoRoot = profile === "live" ? resolveRepoRoot() : "";
  const registryRoot = profile === "live" ? path.join(resolveOctopusHome(), "registry") : "";
  try {
    const providers = (await getProjectionStore(profile).read(projectionReadRequest(
      "projection.operator.providers.list",
    ))).providers;
    const catalog = profile === "live" ? await getClientProviderMetadata().catalog(providers) : providers;
    getClientAccessController().requireSession(token, [profile]);
    if (profile === "live" && registryRoot !== path.join(resolveOctopusHome(), "registry")) {
      throw new Error("Provider catalogue belongs to another Home.");
    }
    return {
      repoRoot,
      registryRoot: registryRoot || null,
      providers: catalog,
    };
  } catch (error) {
    return {
      repoRoot,
      registryRoot: registryRoot || null,
      providers: [],
      error: error instanceof Error ? error.message : String(error),
    };
  }
});

ipcMain.handle("octopus:projection-read", async (_event, token, request) => {
  const { profile } = getClientAccessController().requireSession(token);
  if (profile === "live") await prepareLiveProjectionRead();
  const result = await getProjectionStore(profile).read(request);
  if (profile === "live") scheduleNativeDesktopPresentations();
  if (profile === "live" && request.method === "projection.operator.os.local_packages") {
    const catalog = await getClientProviderMetadata().setupCatalog(result);
    getClientAccessController().requireSession(token, [profile]);
    return catalog;
  }
  return result;
});

ipcMain.handle("octopus:workspace-setup-state", async (_event, token) => {
  getClientAccessController().requireSession(token, ["live"]);
  const state = await inspectWorkspaceSetupState({
    octopusHomeRoot: resolveOctopusHome(),
    registryRoot: path.join(resolveOctopusHome(), "registry"),
  });
  return {
    status: state.status,
    errors: state.errors,
    receipt: state.receipt ? {
      operationId: state.receipt.operation_id,
      requestDigest: state.receipt.request_digest,
      completedAt: state.receipt.completed_at,
      rootEeId: state.receipt.root_ee_id,
      homeRunId: state.receipt.home_run_id,
      homeInterfaceId: state.receipt.home_interface_id,
      projects: state.receipt.project_ee_ids.map((project) => ({
        key: project.key,
        eeId: project.ee_id,
      })),
    } : null,
  };
});

ipcMain.handle("octopus:client-setup-read", async (_event, token, request) => {
  getClientAccessController().requireSession(token, ["live"]);
  return getClientSetupStore().read(request);
});

ipcMain.handle("octopus:client-setup-read-current", async (_event, token) => {
  getClientAccessController().requireSession(token, ["live"]);
  return getClientSetupStore().readCurrent();
});

ipcMain.handle("octopus:client-setup-begin", async (_event, token, request) => {
  getClientAccessController().requireSession(token, ["live"]);
  await validateClientSetupPlan(request?.plan);
  return getClientSetupStore().begin(request);
});

ipcMain.handle("octopus:client-setup-resume", async (event, token, request) => {
  getClientAccessController().requireSession(token, ["live"]);
  try {
    const result = await getClientSetupStore().resume(request, {
      onProgress: (progress) => {
        latestSetupProgress = { ...progress, state: "running", at: new Date().toISOString() };
        void appendClientLog("info", "setup.progress", progress).catch(() => {});
        if (!event.sender.isDestroyed()) event.sender.send("octopus:client-setup-progress", progress);
      },
    });
    if (latestSetupProgress) latestSetupProgress = { ...latestSetupProgress, state: result.status === "complete" ? "complete" : "paused" };
    return result;
  } catch (error) {
    if (latestSetupProgress) latestSetupProgress = { ...latestSetupProgress, state: "failed", phase: "failed", at: new Date().toISOString() };
    void appendClientLog("error", "setup.failed", { message: error instanceof Error ? error.message : String(error) }).catch(() => {});
    throw error;
  }
});

ipcMain.handle("octopus:scheduled-interface-mutate", async (_event, token, request) => {
  const { profile } = getClientAccessController().requireSession(token);
  if (profile === "lab") return getProjectionStore("lab").mutateScheduledInterface(request);
  getClientAccessController().requireSession(token, ["live"]);
  await prepareLiveProjectionRead();
  const outcome = await mutateScheduledInterface(
    path.join(resolveOctopusHome(), "registry"),
    resolveOctopusHome(),
    request,
    {
      commitClientAuthoring: (authoringRequest) => commitProjectionClientAuthoring(
        resolveOctopusHome(),
        path.join(resolveOctopusHome(), "registry"),
        authoringRequest,
      ),
    },
  );
  getProjectionStore("live").invalidate();
  return outcome;
});

ipcMain.handle("octopus:application-private-configuration-mutate", async (_event, token, request) => {
  const { profile } = getClientAccessController().requireSession(token);
  if (profile === "lab") return getProjectionStore("lab").mutateApplicationPrivateConfiguration(request);
  getClientAccessController().requireSession(token, ["live"]);
  await initializeRuntimeHost();
  const octopusHome = resolveOctopusHome();
  const outcome = await mutateApplicationPrivateConfiguration(
    path.join(octopusHome, "registry"),
    octopusHome,
    request,
  );
  getProjectionStore("live").invalidate(false);
  return outcome;
});

function getApplicationConnectionHost() {
  applicationConnectionHost ??= createApplicationConnectionHost({
    resolveTargets: createRegistryConnectionTargets({
      registryRoot: () => path.join(resolveOctopusHome(), "registry"),
      octopusHome: resolveOctopusHome,
      launchEnvironment: () => ({ ...process.env, ...resolveRuntimeLaunchEnv() }),
      openExternal: (url) => shell.openExternal(url),
    }),
    scope: resolveOctopusHome,
  });
  return applicationConnectionHost;
}

async function applicationConnectionOperation(token, operation, { writes = false } = {}) {
  let live = false;
  try {
    const { profile } = getClientAccessController().requireSession(token);
    if (profile !== "lab") {
      getClientAccessController().requireSession(token, ["live"]);
      await initializeRuntimeHost();
      live = true;
    }
    return { contract: "octopus.application-connection.v1", ok: true, value: await operation(live ? null : getProjectionStore("lab")) };
  } catch (error) {
    return {
      contract: "octopus.application-connection.v1",
      ok: false,
      error: {
        message: error instanceof Error && error.message ? error.message : "Octopus couldn't finish the connection.",
        code: error?.code === "cancelled" ? "cancelled" : "failed",
      },
    };
  } finally {
    if (writes && live) getProjectionStore("live").invalidate(false);
  }
}

ipcMain.handle("octopus:application-connection-probe", (_event, token, request) => applicationConnectionOperation(token, (lab) => (
  lab ? lab.applicationConnectionProbe(request) : getApplicationConnectionHost().probe(request)
)));

ipcMain.handle("octopus:application-connection-connect", (event, token, request) => applicationConnectionOperation(token, async (lab) => {
  const sender = event.sender;
  const cancel = () => {
    const operationId = { operationId: request?.operationId };
    if (lab) lab.applicationConnectionCancel(operationId);
    else getApplicationConnectionHost().cancel(operationId);
  };
  const navigation = (_event, _url, isInPlace, isMainFrame) => {
    if (isMainFrame && !isInPlace) cancel();
  };
  sender.once("destroyed", cancel);
  sender.on("render-process-gone", cancel);
  sender.on("did-start-navigation", navigation);
  const options = {
    onProgress: (progress) => {
      if (!sender.isDestroyed()) sender.send("octopus:application-connection-progress", progress);
    },
  };
  try {
    return await (lab ? lab.applicationConnectionConnect(request, options) : getApplicationConnectionHost().connect(request, options));
  } finally {
    sender.removeListener("destroyed", cancel);
    sender.removeListener("render-process-gone", cancel);
    sender.removeListener("did-start-navigation", navigation);
  }
}, { writes: true }));

ipcMain.handle("octopus:application-connection-cancel", (_event, token, request) => applicationConnectionOperation(token, (lab) => (
  lab ? lab.applicationConnectionCancel(request) : getApplicationConnectionHost().cancel(request)
)));

ipcMain.handle("octopus:application-connection-disconnect", (_event, token, request) => applicationConnectionOperation(token, (lab) => (
  lab ? lab.applicationConnectionDisconnect(request) : getApplicationConnectionHost().disconnect(request)
), { writes: true }));

ipcMain.handle("octopus:os-workspace-mutate", async (event, token, request) => {
  getClientAccessController().requireSession(token, ["live"]);
  await prepareLiveProjectionRead();
  const store = getProjectionStore("live");
  const scope = await store.read(projectionReadRequest("projection.operator.os.scope"));
  const agentTemplateMutation = [
    "create_agent_template",
    "configure_agent_template",
    "delete_agent_template",
  ].includes(String(request?.action ?? ""));
  const chatContexts = agentTemplateMutation
    ? await store.read(projectionReadRequest("projection.operator.chat.contexts"))
    : null;
  const octopusHome = resolveOctopusHome();
  const registryRoot = path.join(octopusHome, "registry");
  let outcome;
  if (["create_agent_template", "configure_agent_template"].includes(request?.action)) {
    const declared = await store.read(projectionReadRequest("projection.operator.providers.list"));
    const providers = await getClientProviderMetadata().catalog(declared.providers.filter((provider) => provider.id === request.providerInterfaceId));
    getClientAccessController().requireSession(token, ["live"]);
    if (resolveOctopusHome() !== octopusHome) throw new Error("Home changed during Agent model validation.");
    validateProviderModelSelection(providers, request);
  }
  if (request?.action === "initialize_workspace") {
    if (scope?.status !== "unbound") {
      throw new Error(`Workspace setup requires an unbound Octopus OS scope, found '${stringValue(scope?.status) || "unknown"}'`);
    }
    const { withOperatorWorkspaceTopologyLock } = await loadOperatorChatClient();
    outcome = await withOperatorWorkspaceTopologyLock(octopusHome, () => initializeWorkspace({
        octopusHomeRoot: octopusHome,
        registryRoot,
        publication: workspaceStorage(octopusHome),
        retirementBarrier: {
          verifyBefore: (context) => taskRetirementBarrier(octopusHome).then((barrier) => barrier.verifyBefore(context)),
          withHeldProof: (context, operation) => taskRetirementBarrier(octopusHome).then((barrier) => barrier.withHeldProof(context, operation)),
        },
        request,
        planOperatorSurface: planWorkspaceHomeSurface,
        planBrowserNativeHost,
        planDesktopApplicationPreferences,
        planAgentPreset: planWorkspaceAgentPreset,
        onProgress: (progress) => {
          if (!event.sender.isDestroyed()) event.sender.send("octopus:workspace-setup-progress", progress);
        },
      }));
    await retryNativeBrowserAfterSetup();
  } else {
    const taskLifecycle = (await (await loadOperatorChatClient()).loadOperatorTaskLifecycle({
      registryRoot: () => registryRoot, installationRoot: resolveInstallationRoot,
    })).taskLifecycle;
    outcome = await mutateOctopusOsWorkspace(
      registryRoot,
      octopusHome,
      scope,
      request,
      {
        taskLifecycle,
        sourceRegistryRoot: resolveSourceRegistry(),
        localPackageRegistryRoot: resolveLocalPackageRegistry(),
        localPackageLibraryRoot: resolveLocalPackageLibrary(),
        localPackagePublication: workspaceStorage(octopusHome),
        assertLocalPackageMutation: (packageIds) => getOperatorChatClientLoader().assertMutationAllowed(packageIds),
        chatContexts,
        withAgentPreset: (choice, operation) => withWorkspaceAgentPreset(choice, path.join(octopusHome, "state", "ee_instances"), operation),
        mutateWorkspaceProject: async (projectScope, projectRequest) => {
          const { withOperatorWorkspaceTopologyLock } = await loadOperatorChatClient();
          return withOperatorWorkspaceTopologyLock(octopusHome, () => mutateWorkspaceProject({
            octopusHomeRoot: octopusHome,
            registryRoot,
            publication: workspaceStorage(octopusHome),
            retirementBarrier: {
              verifyBefore: (context) => taskRetirementBarrier(octopusHome).then((barrier) => barrier.verifyBefore(context)),
              withHeldProof: (context, operation) => taskRetirementBarrier(octopusHome).then((barrier) => barrier.withHeldProof(context, operation)),
            },
            scope: projectScope,
            request: projectRequest,
            planOperatorSurface: planWorkspaceHomeSurface,
          }));
        },
        onApplicationProgress: (progress) => {
          if (!event.sender.isDestroyed()) event.sender.send("octopus:application-lifecycle-progress", progress);
        },
        releaseTaskWorkerEnvironments: async (releaseRequest) => {
          const { releaseOperatorTaskWorkerEnvironments } = await loadOperatorChatClient();
          return releaseOperatorTaskWorkerEnvironments({
            clientArtifactsRoot: (interfaceId) => path.join(
              octopusHome,
              "interface-hand-in",
              interfaceId,
              "client",
            ),
            octopusHome: () => octopusHome,
            handInArtifact: (input) => handInArtifact({ ...input, octopusHome }),
          }, releaseRequest);
        },
        withTaskAuthoringLock: async (runId, operation) => {
          const { withOperatorRunAuthoringLock } = await loadOperatorChatClient();
          return withOperatorRunAuthoringLock(runId, operation);
        },
        readTaskStatus: async (runId) => {
          if (resolveOctopusHome() !== octopusHome) throw new Error("Runtime Home changed before the task could be archived");
          store.invalidate(false, "runs");
          const status = await store.read(projectionReadRequest("projection.operator.chats.status", { run_ids: [runId] }));
          return status.chats.find((row) => row.run_id === runId);
        },
        planTaskApplicationAmendment: async (amendmentRequest) => {
          const { planOperatorChatTaskAmendment } = await loadOperatorChatClient();
          return planOperatorChatTaskAmendment({
            octopusHome: () => octopusHome,
            installationRoot: resolveInstallationRoot,
            registryRoot: () => registryRoot,
          }, amendmentRequest);
        },
        commitClientAuthoring: (authoringRequest) => commitProjectionClientAuthoring(
          octopusHome,
          registryRoot,
          authoringRequest,
        ),
      },
    );
  }
  store.invalidate();
  return outcome;
});

let clientBrowserManager;
ipcMain.handle("octopus:client-browser", (event, token, request) => {
  if (event.senderFrame !== event.sender.mainFrame) throw new Error("Browser requires the owning client frame.");
  clientBrowserManager ??= require("./clientBrowser.cjs").createClientBrowser({ electron: require("electron"),
    getHome: resolveOctopusHome, requireAccess: value => getClientAccessController().requireSession(value, ["live"]) });
  return clientBrowserManager.handle(token, event.sender, request);
});

ipcMain.handle("octopus:cdp-viewer-open", async (event, token, request) => {
  getClientAccessController().requireSession(token, ["live"]);
  return getCdpViewerManager().open(token, event.sender, request);
});

for (const [channel, method] of [["prepare", "prepare"], ["ack", "acknowledge"], ["sync", "sync"], ["cursor-arrived", "cursorArrived"], ["action", "action"], ["downloads", "downloads"], ["close", "close"]]) {
  ipcMain.handle(`octopus:native-browser-viewer-${channel}`, async (event, token, request) => {
    getClientAccessController().requireSession(token, ["live"]);
    if (event.senderFrame !== event.sender.mainFrame) throw new Error("Native browser presentation requires the owning client frame.");
    await initializeRuntimeHost();
    return getNativeBrowserSurface()[method](token, event.sender, request);
  });
}

ipcMain.handle("octopus:native-browser-page-close", async (event, token, request) => {
  getClientAccessController().requireSession(token, ["live"]);
  if (event.senderFrame !== event.sender.mainFrame) throw new Error("Native browser page close requires the owning client frame.");
  requireNativeBrowserHome(request?.home);
  await initializeRuntimeHost();
  return getNativeBrowserSurface().closePage(token, event.sender, request);
});

ipcMain.handle("octopus:browser-saved-permissions", async (event, token, request) => {
  getClientAccessController().requireSession(token, ["live"]);
  if (event.senderFrame !== event.sender.mainFrame) throw new Error("Browser Settings require the owning client frame.");
  return getNativeBrowserSurface().permissions(token, event.sender, request);
});

ipcMain.handle("octopus:desktop-saved-application-permissions", async (event, token, request) => {
  const win = BrowserWindow.fromWebContents(event.sender), home = resolveOctopusHome();
  const current = () => {
    getClientAccessController().requireSession(token, ["live"]);
    if (!win || win.isDestroyed() || event.sender.isDestroyed() || win.webContents !== event.sender
      || event.senderFrame !== event.sender.mainFrame || resolveOctopusHome() !== home) throw new Error("Application Settings lost their exact live client window and Home.");
  };
  current(); getDesktopSurfaceHost(event.sender);
  return getNativeDesktopSettings().applicationPermissions(request, { id: event.sender.id, current,
    identity: createHash("sha256").update(token).digest("hex") });
});

ipcMain.handle("octopus:desktop-computer-use-settings", async (event, token, request) => {
  if (!request || typeof request !== "object" || Array.isArray(request) || !["status", "setup", "set_enabled"].includes(request.operation)
    || Object.keys(request).some(key => !["operation", "enabled"].includes(key))
    || (request.operation === "set_enabled" ? typeof request.enabled !== "boolean" || Object.keys(request).length !== 2 : Object.keys(request).length !== 1)) throw new Error("Invalid Computer Use Settings request.");
  const win = BrowserWindow.fromWebContents(event.sender), home = resolveOctopusHome();
  const current = () => {
    getClientAccessController().requireSession(token, ["live"]);
    if (!win || win.isDestroyed() || event.sender.isDestroyed() || win.webContents !== event.sender
      || event.senderFrame !== event.sender.mainFrame || resolveOctopusHome() !== home) throw new Error("Computer Use Settings lost its current window or Home.");
  };
  current(); getDesktopSurfaceHost(event.sender);
  const owner = { id: event.sender.id, current, windowHandle: () => { current(); return win.getNativeWindowHandle(); } };
  if (request.operation === "setup") return getNativeDesktopPermissions().open({ setup: true }, owner);
  const source = await getNativeDesktopSettings().source(owner);
  const preference = await source.preference(request.operation === "set_enabled" ? request.enabled : undefined);
  if (request.operation === "set_enabled" && !preference.enabled) {
    nativeDesktopPresentations?.revokeAll();
    await getDesktopSurfaceHost().stopConfiguredApplications(owner.id, current);
  }
  const snapshot = await source.exchange("status", null);
  current();
  return { enabled: preference.enabled, ready: snapshot.permissions.length === 2 && snapshot.permissions.every(row => row.status === "granted"),
    permissions: snapshot.permissions.map(({ kind, status }) => ({ kind, status })) };
});

ipcMain.handle("octopus:desktop-permissions-open", async (event, token, request) => {
  const win = BrowserWindow.fromWebContents(event.sender), capturedHome = resolveOctopusHome();
  const current = () => {
    getClientAccessController().requireSession(token, ["live"]);
    if (!win || win.isDestroyed() || event.sender.isDestroyed() || win.webContents !== event.sender
      || event.senderFrame !== event.sender.mainFrame || resolveOctopusHome() !== capturedHome) throw new Error("Native permissions require the exact live client window and Home.");
  };
  current(); getDesktopSurfaceHost(event.sender);
  return getNativeDesktopPermissions().open(request, { id: event.sender.id, current, windowHandle: () => { current(); return win.getNativeWindowHandle(); } });
});

ipcMain.handle("octopus:desktop-surface-request", async (event, token, request) => {
  getClientAccessController().requireSession(token, ["live"]);
  const win = BrowserWindow.fromWebContents(event.sender);
  if (!win || win.isDestroyed()) throw new Error("Desktop viewing requires an active window.");
  return getDesktopSurfaceHost(event.sender).request(request, event.sender.id);
});

ipcMain.handle("octopus:desktop-application-view", async (event, token, request) => {
  const win = BrowserWindow.fromWebContents(event.sender), home = resolveOctopusHome();
  const current = () => {
    getClientAccessController().requireSession(token, ["live"]);
    if (!win || win.isDestroyed() || event.sender.isDestroyed() || win.webContents !== event.sender
      || event.senderFrame !== event.sender.mainFrame || resolveOctopusHome() !== home) throw new Error("Application view lost its owning client window or Home.");
  };
  current(); getDesktopSurfaceHost(event.sender);
  let input = request;
  if (request?.operation === "attach") {
    const rect = request.rect, zoom = event.sender.getZoomFactor(), [width, height] = win.getContentSize();
    if (!rect || typeof rect !== "object" || Array.isArray(rect) || Object.keys(rect).length !== 4 || !Number.isFinite(zoom) || zoom <= 0
      || !["x","y","width","height"].every(key => Number.isFinite(rect[key])) || rect.x < 0 || rect.y < 0 || rect.width <= 0 || rect.height <= 0
      || (rect.x + rect.width) * zoom > width + 0.5 || (rect.y + rect.height) * zoom > height + 0.5 || !win.isVisible() || win.isMinimized()) throw new Error("Application view requires its visible client rectangle.");
    input = { ...request, rect: Object.fromEntries(Object.entries(rect).map(([key, value]) => [key, value * zoom])) };
  }
  return getNativeDesktopPresentations().applicationView(input, { id: event.sender.id, current,
    windowHandle: () => { current(); return win.getNativeWindowHandle(); },
    hostIdentity: () => { current(); const owner = nativeDesktopWindowHosts.get(event.sender.id); if (!owner) throw new Error("Application view has no registered native window."); return owner; } });
});

ipcMain.handle("octopus:cdp-viewer-action", async (event, token, request) => {
  getClientAccessController().requireSession(token, ["live"]);
  return getCdpViewerManager().action(token, event.sender, request);
});

ipcMain.handle("octopus:cdp-viewer-close", async (event, token, request) => {
  getClientAccessController().requireSession(token, ["live"]);
  return getCdpViewerManager().close(token, event.sender, request);
});

ipcMain.handle("octopus:process-viewer-open", async (event, token, request) => {
  getClientAccessController().requireSession(token, ["live"]);
  return getProcessViewerManager().open(token, event.sender, request);
});

ipcMain.handle("octopus:process-viewer-action", async (event, token, request) => {
  getClientAccessController().requireSession(token, ["live"]);
  return getProcessViewerManager().action(token, event.sender, request);
});

ipcMain.handle("octopus:process-viewer-close", async (event, token, request) => {
  getClientAccessController().requireSession(token, ["live"]);
  return getProcessViewerManager().close(token, event.sender, request);
});

ipcMain.handle("octopus:worker-environment-files-list", async (_event, token, request) => {
  getClientAccessController().requireSession(token, ["live"]);
  return getWorkerEnvironmentFileReader().list(request);
});

ipcMain.handle("octopus:worker-environment-file-read", async (_event, token, request) => {
  getClientAccessController().requireSession(token, ["live"]);
  return getWorkerEnvironmentFileReader().read(request);
});

ipcMain.handle("octopus:worker-environment-file-preview", async (_event, token, request) => {
  getClientAccessController().requireSession(token, ["live"]);
  return getWorkerEnvironmentFileReader().readPreview(request);
});

ipcMain.handle("octopus:worker-environment-file-attachment", async (_event, token, request) => {
  getClientAccessController().requireSession(token, ["live"]);
  return getWorkerEnvironmentFileReader().readAttachment(request);
});

ipcMain.handle("octopus:worker-environment-file-write", async (_event, token, request) => {
  getClientAccessController().requireSession(token, ["live"]);
  return getWorkerEnvironmentFileReader().write(request);
});

ipcMain.handle("octopus:worker-environment-file-system-action", async (_event, token, request) => {
  getClientAccessController().requireSession(token, ["live"]);
  const action = String(request?.action || "");
  if (!["resolve", "reveal", "open"].includes(action)) throw new Error("Worker Environment file system action is invalid");
  const resolved = await getWorkerEnvironmentFileReader().resolvePath(request);
  if (action === "reveal") shell.showItemInFolder(resolved.absolutePath);
  if (action === "open") {
    const result = await shell.openPath(resolved.absolutePath);
    if (result) throw new Error(result);
  }
  return { ...resolved, action };
});

ipcMain.handle("octopus:os-files-roots", async (_event, token) => {
  return osFileStoreFor(token).roots();
});

ipcMain.handle("octopus:os-files-list", async (_event, token, request) => {
  return osFileStoreFor(token).list(request);
});

ipcMain.handle("octopus:os-file-read", async (_event, token, request) => {
  return osFileStoreFor(token).read(request);
});

ipcMain.handle("octopus:os-file-write", async (_event, token, request) => {
  return osFileStoreFor(token).write(request);
});

ipcMain.handle("octopus:os-file-system-action", async (_event, token, request) => {
  const store = osFileStoreFor(token);
  const action = String(request?.action || "");
  if (!["resolve", "reveal", "open"].includes(action)) throw new Error("OS file system action is invalid");
  const resolved = await store.resolvePath(request);
  if (action === "reveal") shell.showItemInFolder(resolved.absolutePath);
  if (action === "open") {
    const result = await shell.openPath(resolved.absolutePath);
    if (result) throw new Error(result);
  }
  return { ...resolved, action };
});

ipcMain.handle("octopus:worker-environment-git-inspect", async (_event, token, request) => {
  getClientAccessController().requireSession(token, ["live"]);
  return getWorkerEnvironmentOperatorActions().inspect(request);
});

ipcMain.handle("octopus:worker-environment-operator-action", async (_event, token, request) => {
  getClientAccessController().requireSession(token, ["live"]);
  return getWorkerEnvironmentOperatorActions().perform(request);
});

ipcMain.handle("octopus:client-surface-documents", async (_event, token) => {
  const { profile } = getClientAccessController().requireSession(token);
  return filterClientSurfaceSnapshot(await getClientSurfaceDocumentStore().snapshot(), profile);
});

ipcMain.handle("octopus:client-extensions", async (_event, token) => {
  getClientAccessController().requireSession(token, ["live"]);
  return clientExtensionSnapshot();
});

ipcMain.handle("octopus:client-extension-mutate", async (_event, token, request) => {
  getClientAccessController().requireSession(token, ["live"]);
  await getClientExtensionStore().mutate(request);
  return clientExtensionSnapshot();
});

ipcMain.handle("octopus:client-extension-credential", async (_event, token, request) => {
  getClientAccessController().requireSession(token, ["live"]);
  await getClientExtensionStore().setCredential(request);
  return clientExtensionSnapshot();
});

ipcMain.handle("octopus:client-extension-credential-check", async (_event, token, request) => {
  getClientAccessController().requireSession(token, ["live"]);
  const { origin, checkPath, secret } = await getClientExtensionStore().credential(request?.id, request?.sourceId);
  if (!checkPath) return { checked: false, ok: false, status: 0 };
  const response = await electronNet.fetch(`${origin}${checkPath}`, {
    headers: { Authorization: `Bearer ${secret}` },
    signal: AbortSignal.timeout(10_000),
  });
  return { checked: true, ok: response.ok, status: response.status };
});

ipcMain.handle("octopus:client-extension-lifecycle", async (_event, token, request) => {
  getClientAccessController().requireSession(token, ["live"]);
  const outcome = await getClientExtensionStore().applyLifecycle(request);
  return { ...outcome, snapshot: await clientExtensionSnapshot() };
});

ipcMain.handle("octopus:client-extensions-reveal", async (_event, token) => {
  getClientAccessController().requireSession(token, ["live"]);
  const store = getClientExtensionStore();
  await store.ensureRoot();
  const result = await shell.openPath(store.root);
  if (result) throw new Error(result);
  return { opened: true, path: store.root };
});

ipcMain.handle("octopus:client-extension-filesystem-read", async (_event, token, request) => {
  getClientAccessController().requireSession(token, ["live"]);
  return readClientExtensionFilesystemSource(request);
});

ipcMain.handle("octopus:client-extension-action", async (event, token, request) => {
  getClientAccessController().requireSession(token, ["live"]);
  launchTerminalOwner(event, token);
  try {
    const store = getClientExtensionStore();
    const result = await store.withVerifiedLifecycleSnapshot(({ installedExtensionIds }) => (
      executeClientExtensionAction({
        octopusHome: resolveOctopusHome(),
        codexHome: resolveOperatorCodexHome(),
        installedExtensionIds,
      }, request)
    ));
    await appendClientLog("info", "client_extension.action_completed", {
      actionId: result.actionId,
      extensionId: result.extensionId,
      incidentId: result.incidentId,
      interfaceId: result.interfaceId,
      credentialUpdated: result.credentialUpdated,
      runtimeRestarted: result.runtimeRestarted,
      sharedCredential: result.sharedCredential,
    }).catch(() => undefined);
    notifyClientExtensionsChanged(store.generation());
    return clientExtensionActionSuccess(result);
  } catch (error) {
    await appendClientLog("error", "client_extension.action_failed", {
      actionId: stringValue(request?.actionId),
      extensionId: stringValue(request?.extensionId),
      incidentId: stringValue(request?.incident?.id),
      interfaceId: stringValue(request?.incident?.interfaceId),
      diagnostic: {
        code: stringValue(error?.code),
        message: String(error?.message || error || "Unknown client extension action failure").slice(0, 8_000),
        name: stringValue(error?.name) || "Error",
      },
    }).catch(() => undefined);
    return clientExtensionActionFailure(error);
  }
});

ipcMain.handle("octopus:client-codex-usage-read", async (_event, token, request) => {
  getClientAccessController().requireSession(token, ["live"]);
  return getCodexUsageReader().readUsage(request);
});

ipcMain.handle("octopus:provider-models-refresh", async (_event, token, request) => {
  getClientAccessController().requireSession(token, ["live"]);
  return getClientProviderMetadata().models(request);
});

ipcMain.handle("octopus:composer-prediction", async (event, token, request) => {
  const home = resolveOctopusHome();
  const current = () => {
    getClientAccessController().requireSession(token, ["live"]);
    if (resolveOctopusHome() !== home || event.sender.isDestroyed()) throw new Error("Client suggestion lost its current Home or window");
  };
  current();
  const read = (method, params) => getProjectionStore("live").read(projectionReadRequest(method, params));
  const readSource = async (runId) => {
    current();
    const { chats } = await read("projection.operator.chats.list", {});
    const chat = chats.find((chat) => chat.run_id === runId);
    if (!chat?.writable || chat.archived || chat.turn_open || chat.turn_state !== "idle") return null;
    const { messages } = await read("projection.operator.chat.messages", { run_id: runId });
    const terminal = messages.findLast((row) => row.artifact_id === chat.last_terminal_message?.artifact_id && row.terminal && !row.error);
    return terminal ? { chat, terminal } : null;
  };
  const source = await readSource(request?.runId);
  if (!source) return null;
  const { requestComposerPrediction } = await loadOperatorChatClient();
  try {
    const result = await requestComposerPrediction({
      octopusHome: () => home, readSource, assertCurrent: current,
      readArtifacts: async (params) => (await read("projection.interface_artifacts.list", params)).artifacts,
      commit: (plan) => { current(); return commitProjectionClientAuthoring(home, path.join(home, "registry"), plan); },
      handInArtifact: (input) => { current(); return handInArtifact({ ...input, octopusHome: home }); },
    }, request);
    current();
    const latest = await readSource(request.runId);
    return latest?.terminal.artifact_id === source.terminal.artifact_id ? result : null;
  } catch (error) {
    void appendClientLog("warning", "composer.prediction_failed", { message: String(error?.message ?? error) });
    throw error;
  }
});

ipcMain.handle("octopus:task-title", async (event, token, request) => {
  const home = resolveOctopusHome();
  const current = () => {
    getClientAccessController().requireSession(token, ["live"]);
    if (resolveOctopusHome() !== home || event.sender.isDestroyed()) throw new Error("Task title lost its current Home or window");
  };
  current();
  const read = (method, params) => getProjectionStore("live").read(projectionReadRequest(method, params));
  const readSource = async (runId) => {
    current();
    const { chats } = await read("projection.operator.chats.list", {});
    const chat = chats.find((row) => row.run_id === runId);
    if (!chat?.title_generation_needed || !chat.writable || chat.archived) return null;
    const input = chat.title_source;
    return input ? { chat, input } : null;
  };
  const { requestTaskTitle } = await loadOperatorChatClient();
  try {
    return await requestTaskTitle({ octopusHome: () => home, readSource, assertCurrent: current,
      readArtifacts: async (params) => (await read("projection.interface_artifacts.list", params)).artifacts,
      commit: (plan) => { current(); return commitProjectionClientAuthoring(home, path.join(home, "registry"), plan); },
      handInArtifact: (input) => { current(); return handInArtifact({ ...input, octopusHome: home }); },
    }, request);
  } catch (error) {
    void appendClientLog("warning", "task.title_failed", { message: String(error?.message ?? error) });
    throw error;
  }
});

ipcMain.handle("octopus:runtime-status", async (_event, token, request) => {
  const lab = labRuntimeHostState(token);
  if (lab) {
    return {
      up: lab.ready,
      endpoint: lab.endpoint,
      phase: lab.phase,
      ownership: lab.ownership,
      pid: lab.pid,
      home: lab.home,
      sourceRegistry: lab.sourceRegistry,
      runtimeBinary: lab.runtimeBinary,
      hostError: lab.error,
    };
  }
  getClientAccessController().requireSession(token, ["live"]);
  const endpoint = request && typeof request.endpoint === "string" && request.endpoint.trim()
    ? request.endpoint.trim()
    : defaultRuntimeEndpoint();
  let target;
  try {
    target = parseEndpoint(endpoint);
  } catch (error) {
    return { up: false, endpoint, error: error instanceof Error ? error.message : String(error) };
  }
  const probe = await new Promise((resolve) => {
    const socket = target.kind === "unix"
      ? net.createConnection(target.path)
      : net.createConnection({ host: target.host, port: target.port });
    let done = false;
    const finish = (up, error) => {
      if (done) return;
      done = true;
      try { socket.destroy(); } catch {}
      resolve(error ? { up, endpoint, error: String(error) } : { up, endpoint });
    };
    const timer = setTimeout(() => finish(false, "timeout"), 1500);
    socket.once("connect", () => { clearTimeout(timer); finish(true); });
    socket.once("error", (err) => { clearTimeout(timer); finish(false, err && err.message ? err.message : err); });
  });
  const host = runtimeHost?.snapshot() ?? null;
  return {
    ...probe,
    phase: host?.phase ?? (probe.up ? "ready" : "unmanaged"),
    ownership: host?.ownership ?? "none",
    pid: host?.pid ?? null,
    home: host?.home ?? resolveOctopusHome(),
    sourceRegistry: host?.sourceRegistry ?? resolveSourceRegistry(),
    runtimeBinary: host?.runtimeBinary ?? resolveRuntimeBinary(),
    hostError: host?.error ?? null,
  };
});

function retainChatAttachmentsAfterStorageFailure(token, attachments, error) {
  if (error?.code !== "ENOSPC" && error?.code !== "EDQUOT" && error?.code !== "home_storage_blocked") return;
  for (const attachment of attachments) getClientAttachmentStagingStore().restore(token, attachment);
  attachments.length = 0;
}

ipcMain.handle("octopus:submit-chat-turn", async (_event, token, request) => {
  let profile = "unknown";
  let stagedAttachments = [];
  let submissionStarted = false;
  try {
    ({ profile } = getClientAccessController().requireSession(token));
    if (profile === "lab") {
      submissionStarted = true;
      return operatorChatSubmitSuccess(await getProjectionStore("lab").submitChatTurn(request));
    }
    await ensureRuntimeHost().assertStorageWritable();
    await prepareLiveProjectionRead();
    const octopusHome = resolveOctopusHome();
    const { submitOperatorTurn } = await loadOperatorChatClient();
    const requestedPayloads = request?.payloadArtifactBindings === undefined
      ? undefined : payloadArtifactBindings(request.payloadArtifactBindings);
    const revalidatePayloadSource = async () => {
      if (getClientAccessController().requireSession(token).profile !== "live" || resolveOctopusHome() !== octopusHome) {
        throw new Error("Selected file source lost its exact current client access or Home");
      }
    };
    for (const input of Array.isArray(request?.attachments) ? request.attachments : []) {
      const attachment = await getClientAttachmentStagingStore().consume(token, input?.stage);
      stagedAttachments.push(attachment);
    }
    await revalidatePayloadSource();
    submissionStarted = true;
    const result = await submitOperatorTurn(
      {
        clientArtifactsRoot: (interfaceId) => path.join(
          octopusHome,
          "interface-hand-in",
          interfaceId,
          "client",
        ),
        octopusHome: () => octopusHome,
        installationRoot: resolveInstallationRoot,
        registryRoot: () => path.join(octopusHome, "registry"),
        handInArtifact: (input) => handInArtifact({
          ...input,
          endpoint: stringValue(input.endpoint) === "unix://"
            ? defaultRuntimeEndpoint()
            : input.endpoint,
          octopusHome,
        }),
        readArtifactPayload: (input) => readContentAddressedArtifactPayload({
          ...input,
          blobsRoot: path.join(octopusHome, "ledger", "blobs"),
        }),
        readProjection: (method, params) => getProjectionStore("live").read(projectionReadRequest(method, params)),
        preparePayloadArtifactBindings: ({ payloadArtifactBindings: selectedBase }) => prepareContentAddressedPayloadBindings({
          blobsRoot: path.join(octopusHome, "ledger", "blobs"),
          attachments: stagedAttachments,
          payloadArtifactBindings: selectedBase,
          revalidate: revalidatePayloadSource,
        }),
      },
      {
        endpoint: request?.endpoint,
        timeoutMs: request?.timeoutMs,
        messageId: request?.messageId,
        recoverPreparedMessage: request?.recoverPreparedMessage,
        replacesMessage: request?.replacesMessage,
        runId: request?.runId,
        providerInterfaceId: request?.providerInterfaceId,
        providerModel: request?.providerModel,
        reasoningEffort: request?.reasoningEffort,
        serviceTier: request?.serviceTier,
        eeId: request?.eeId,
        workerInterfaceId: request?.workerInterfaceId,
        doorHostSlots: request?.doorHostSlots,
        externalInputs: request?.externalInputs,
        externalRelays: request?.externalRelays,
        modelOutputs: request?.modelOutputs,
        contextArtifactBindings: request?.contextArtifactBindings,
        ...(requestedPayloads === undefined ? {} : { payloadArtifactBindings: requestedPayloads }),
        selectedWorkerEnvironmentKinds: request?.selectedWorkerEnvironmentKinds,
        selectedTools: request?.selectedTools,
        clientSurfaceRole: request?.clientSurfaceRole,
        clientContext: request?.clientContext,
        continuation: request?.continuation,
        text: request?.text,
        attachments: stagedAttachments,
      },
    );
    return operatorChatSubmitSuccess(result);
  } catch (error) {
    if (profile === "live") runtimeHost?.recordStorageFailure(error);
    retainChatAttachmentsAfterStorageFailure(token, stagedAttachments, error);
    await appendClientLog("error", "operator_chat.submit_failed", {
      profile,
      runId: stringValue(request?.runId),
      surface: stringValue(request?.clientSurfaceRole),
      diagnostic: operatorChatSubmitDiagnostic(error),
    }).catch(() => undefined);
    return operatorChatSubmitFailure(error, { submissionStarted });
  } finally {
    const cleanup = await Promise.allSettled(stagedAttachments.map((attachment) => (
      getClientAttachmentStagingStore().discard(attachment)
    )));
    const cleanupFailures = cleanup
      .filter((result) => result.status === "rejected")
      .map((result) => operatorChatSubmitDiagnostic(result.reason));
    if (cleanupFailures.length) {
      await appendClientLog("warning", "operator_chat.attachment_cleanup_failed", {
        profile,
        failures: cleanupFailures,
      }).catch(() => undefined);
    }
  }
});

ipcMain.handle("octopus:operator-chat-artifacts-read", async (_event, token, request) => {
  getClientAccessController().requireSession(token, ["live"]);
  const octopusHome = resolveOctopusHome();
  const { readOperatorChatArtifacts } = await loadOperatorChatClient();
  const batch = await readOperatorChatArtifacts(
    {
      octopusHome: () => octopusHome,
      readArtifactPayload: (input) => Object.hasOwn(input, "projection")
        ? readContentAddressedArtifactProjection({ ...input, blobsRoot: path.join(octopusHome, "ledger", "blobs") })
        : readContentAddressedArtifactPayload({
          ...input, blobsRoot: path.join(octopusHome, "ledger", "blobs"),
          maxPayloadBytes: input?.label === "turn.stream" ? 256 * 1024 * 1024 : undefined,
        }),
    },
    request,
  );
  return assertOperatorChatArtifactBatch(batch);
});

ipcMain.handle("octopus:submit-operator-turn-control", async (_event, token, request) => {
  return captureArtifactDelivery(async (deliver) => {
    getClientAccessController().requireSession(token, ["live"]);
    const octopusHome = resolveOctopusHome();
    const { submitOperatorTurnControl } = await loadOperatorChatClient();
    return submitOperatorTurnControl({
      octopusHome: () => octopusHome,
      clientArtifactsRoot: (interfaceId) => path.join(octopusHome, "interface-hand-in", interfaceId, "client"),
      handInArtifact: (input) => deliver({ ...input, octopusHome }),
    }, request);
  });
});

ipcMain.handle("octopus:submit-chat-steer", async (_event, token, request) => {
  let profile = "unknown";
  let submissionStarted = false;
  const stagedAttachments = [];
  try {
    ({ profile } = getClientAccessController().requireSession(token));
    if (profile === "lab") return operatorChatSubmitSuccess(await getProjectionStore("lab").submitChatSteer(request));
    await ensureRuntimeHost().assertStorageWritable();
    await prepareLiveProjectionRead();
    const octopusHome = resolveOctopusHome();
    const { submitOperatorTurn } = await loadOperatorChatClient();
    const requestedPayloads = request?.payloadArtifactBindings === undefined
      ? undefined : payloadArtifactBindings(request.payloadArtifactBindings);
    const revalidatePayloadSource = async () => {
      if (getClientAccessController().requireSession(token).profile !== "live" || resolveOctopusHome() !== octopusHome) {
        throw new Error("Selected file source lost its exact current client access or Home");
      }
    };
    for (const input of Array.isArray(request?.attachments) ? request.attachments : []) {
      stagedAttachments.push(await getClientAttachmentStagingStore().consume(token, input?.stage));
    }
    await revalidatePayloadSource();
    submissionStarted = true;
    const result = await submitOperatorTurn({
      clientArtifactsRoot: (interfaceId) => path.join(octopusHome, "interface-hand-in", interfaceId, "client"),
      octopusHome: () => octopusHome,
      installationRoot: resolveInstallationRoot,
      registryRoot: () => path.join(octopusHome, "registry"),
      handInArtifact: (input) => handInArtifact({
        ...input,
        endpoint: stringValue(input.endpoint) === "unix://"
          ? defaultRuntimeEndpoint()
          : input.endpoint,
        octopusHome,
      }),
      readArtifactPayload: (input) => readContentAddressedArtifactPayload({
        ...input,
        blobsRoot: path.join(octopusHome, "ledger", "blobs"),
      }),
      readProjection: (method) => getProjectionStore("live").read(projectionReadRequest(method)),
      preparePayloadArtifactBindings: ({ payloadArtifactBindings: selectedBase }) => prepareContentAddressedPayloadBindings({
        blobsRoot: path.join(octopusHome, "ledger", "blobs"),
        attachments: stagedAttachments,
        payloadArtifactBindings: selectedBase,
        revalidate: revalidatePayloadSource,
      }),
    }, {
      endpoint: request?.endpoint,
      timeoutMs: request?.timeoutMs,
      messageId: request?.messageId,
      recoverPreparedMessage: request?.recoverPreparedMessage,
      runId: request?.runId,
      providerInterfaceId: request?.providerInterfaceId,
      providerModel: request?.providerModel,
      reasoningEffort: request?.reasoningEffort,
        serviceTier: request?.serviceTier,
      eeId: request?.eeId,
      workerInterfaceId: request?.workerInterfaceId,
      clientSurfaceRole: request?.clientSurfaceRole,
      clientContext: request?.clientContext,
      ...(requestedPayloads === undefined ? {} : { payloadArtifactBindings: requestedPayloads }),
      text: request?.text,
      attachments: stagedAttachments,
    });
    return operatorChatSubmitSuccess({
      interfaceId: result.chatInterfaceId,
      runId: result.runId,
      label: result.label,
      messageId: result.messageId,
      attachmentCount: result.attachmentCount,
      contentHash: result.contentHash,
      messageRecovered: result.messageRecovered,
      verdict: result.messageVerdict,
    });
  } catch (error) {
    if (profile === "live") runtimeHost?.recordStorageFailure(error);
    retainChatAttachmentsAfterStorageFailure(token, stagedAttachments, error);
    if (error?.code !== "home_storage_blocked" && error?.code !== "ENOSPC" && error?.code !== "EDQUOT") throw error;
    return operatorChatSubmitFailure(error, { submissionStarted });
  } finally {
    await Promise.allSettled(stagedAttachments.map((attachment) => getClientAttachmentStagingStore().discard(attachment)));
  }
});

ipcMain.handle("octopus:update-chat-pins", async (_event, token, request) => {
  const { profile } = getClientAccessController().requireSession(token);
  if (profile === "lab") return getProjectionStore("lab").updateChatPins(request);
  await prepareLiveProjectionRead();
  const octopusHome = resolveOctopusHome();
  const { updateOperatorChatPins } = await loadOperatorChatClient();
  return updateOperatorChatPins({
    octopusHome: () => octopusHome,
    installationRoot: resolveInstallationRoot,
  }, request);
});

ipcMain.handle("octopus:transcribe-dictation", async (_event, token, request) => {
  const attemptId = request?.attemptId;
  const key = `${token}:${attemptId}`;
  let controller;
  try {
    getClientAccessController().requireSession(token, ["live"]);
    if (typeof attemptId !== "string" || !/^[a-f0-9-]{36}$/.test(attemptId) || clientDictationRequests.has(key)) {
      throw new Error("Invalid transcription attempt.");
    }
    controller = new AbortController();
    clientDictationRequests.set(key, controller);
    const value = await transcribeClientDictation({
      bytes: bufferFromIpcBytes(request?.bytes),
      durationMs: request?.durationMs,
      mediaType: request?.mediaType,
      provider: request?.provider,
      sampleRateHz: request?.sampleRateHz,
      timeoutMs: request?.timeoutMs,
    }, {
      clientVersion: app.getVersion(),
      cwd: app.getPath("home"),
      fetch: electronNet.fetch,
      signal: controller.signal,
    });
    return { ok: true, value };
  } catch (error) {
    return { ok: false, error: dictationFailure(error) };
  } finally {
    if (controller) clientDictationRequests.delete(key);
  }
});

ipcMain.handle("octopus:dictation-stream-start", async (event, token, request) => {
  const key = `${token}:${request?.attemptId}`;
  let stream;
  const release = () => {
    stream?.close();
    if (clientDictationStreams.get(key)?.stream === stream) clientDictationStreams.delete(key);
    event.sender.removeListener("destroyed", release);
  };
  try {
    getClientAccessController().requireSession(token, ["live"]);
    if (!/^[a-f0-9-]{36}$/.test(request?.attemptId ?? "") || [...clientDictationStreams.values()].some((entry) => entry.owner === event.sender.id)) throw new Error("Dictation is already active.");
    stream = createClientDictationStream(request, {
      clientVersion: app.getVersion(), cwd: app.getPath("home"),
      onEvent(payload) {
        try {
          getClientAccessController().requireSession(token, ["live"]);
          if (!event.sender.isDestroyed()) event.sender.send("octopus:dictation-stream-event", { attemptId: request.attemptId, ...payload });
        } catch { release(); }
      },
    });
    clientDictationStreams.set(key, { stream, owner: event.sender.id, release });
    event.sender.once("destroyed", release);
    await stream.ready;
    return { ok: true, value: null };
  } catch (error) {
    release();
    return { ok: false, error: dictationFailure(error) };
  }
});

ipcMain.on("octopus:dictation-stream-audio", (event, token, attemptId, bytes) => {
  const entry = clientDictationStreams.get(`${token}:${attemptId}`);
  if (!entry || entry.owner !== event.sender.id) return;
  try {
    getClientAccessController().requireSession(token, ["live"]);
    entry.stream.append(bufferFromIpcBytes(bytes));
  } catch { entry.release(); }
});

ipcMain.handle("octopus:dictation-stream-finish", async (event, token, attemptId) => {
  const entry = clientDictationStreams.get(`${token}:${attemptId}`);
  try {
    getClientAccessController().requireSession(token, ["live"]);
    if (!entry || entry.owner !== event.sender.id) throw new Error("Dictation is no longer active.");
    return { ok: true, value: await entry.stream.finish() };
  } catch (error) {
    return { ok: false, error: dictationFailure(error) };
  } finally { if (entry?.owner === event.sender.id) entry.release(); }
});

ipcMain.handle("octopus:cancel-dictation", (event, token, attemptId) => {
  getClientAccessController().requireSession(token, ["live", "lab"]);
  clientDictationRequests.get(`${token}:${attemptId}`)?.abort();
  const stream = clientDictationStreams.get(`${token}:${attemptId}`);
  if (stream?.owner === event.sender.id) stream.release();
});

for (const [channel, operation] of [
  ["octopus:dictation-recordings-read", (profile) => clientDictationRecordings.state(profile)],
  ["octopus:dictation-recordings-configure", (profile, enabled) => clientDictationRecordings.configure(profile, enabled)],
  ["octopus:dictation-recording-save", (profile, request) => clientDictationRecordings.save(profile, { ...request, bytes: bufferFromIpcBytes(request?.bytes) })],
  ["octopus:dictation-recording-read", (profile, id) => clientDictationRecordings.read(profile, id)],
  ["octopus:dictation-recording-update", (profile, request) => clientDictationRecordings.update(profile, request)],
  ["octopus:dictation-recording-delete", async (profile, id) => {
    if (id === null) await clientDictationRecordings.clear(profile);
    else await clientDictationRecordings.remove(profile, id);
    return null;
  }],
]) {
  ipcMain.handle(channel, async (_event, token, request) => {
    try {
      const { profile } = getClientAccessController().requireSession(token, ["live", "lab"]);
      return { ok: true, value: await operation(profile, request) };
    } catch {
      return { ok: false, error: { code: "recording-storage", message: "Couldn't access dictation recordings. Try again and check available disk space." } };
    }
  });
}

ipcMain.handle("octopus:submit-interface-artifact", async (_event, token, request) => {
  const { profile } = getClientAccessController().requireSession(token, ["live", "lab"]);
  if (profile === "lab") return getProjectionStore("lab").submitInterfaceArtifact(request);
  await prepareLiveProjectionRead();
  const octopusHome = resolveOctopusHome();
  return submitClientInterfaceArtifact({
    clientArtifactsRoot: (interfaceId) => path.join(octopusHome, "interface-hand-in", interfaceId, "client"),
    projectionStore: () => getProjectionStore("live"),
    handInArtifact: (input) => handInArtifact({ ...input, octopusHome }),
    authorizeSubmission: authorizeClientInterfaceArtifactSubmission,
  }, request);
});

ipcMain.handle("octopus:artifact-stream-begin", async (_event, token, request) => {
  getClientAccessController().requireSession(token, ["live"]);
  return getClientArtifactStreamStore().begin(token, request);
});

ipcMain.handle("octopus:artifact-stream-append", async (_event, token, request) => {
  getClientAccessController().requireSession(token, ["live"]);
  return getClientArtifactStreamStore().append(token, {
    ...request,
    bytes: bufferFromIpcBytes(request?.bytes),
  });
});

ipcMain.handle("octopus:artifact-stream-finalize", async (_event, token, request) => {
  getClientAccessController().requireSession(token, ["live"]);
  return getClientArtifactStreamStore().finalize(token, request);
});

ipcMain.handle("octopus:artifact-stream-abort", async (_event, token, request) => {
  getClientAccessController().requireSession(token, ["live"]);
  return getClientArtifactStreamStore().abort(token, request);
});

function setAppIdentity() {
  app.setName("Octopus");
  setApplicationMenu();
  const iconPath = appIconPath();
  if (iconPath) {
    app.setAboutPanelOptions({
      applicationName: "Octopus",
      applicationVersion: "0.1.0",
      iconPath,
    });
  }
  if (process.platform === "darwin" && iconPath && app.dock && !app.isPackaged) {
    app.dock.setIcon(iconPath);
  }
}

function setApplicationMenu() {
  if (process.platform !== "darwin") return;
  const commandItem = (label, command) => ({
    label,
    ...(nativeCommandShortcuts[command] ? { accelerator: nativeCommandShortcuts[command] } : {}),
    click: () => sendNativeClientCommand(command),
  });
  Menu.setApplicationMenu(Menu.buildFromTemplate([
    {
      label: "Octopus",
      submenu: [
        { role: "about", label: "About Octopus" },
        commandItem("Settings…", "open_settings"),
        { type: "separator" },
        { role: "services" },
        { type: "separator" },
        { role: "hide", label: "Hide Octopus" },
        { role: "hideOthers" },
        { role: "unhide" },
        { type: "separator" },
        { role: "quit", label: "Quit Octopus" },
      ],
    },
    {
      label: "File",
      submenu: [
        { label: "New Window", click: () => openNewClientWindowFromMenu() },
        { type: "separator" },
        commandItem("New Task", "new_chat"),
        commandItem("Attach Files…", "attach_files"),
        commandItem("Attach Projected File…", "attach_projected_file"),
        commandItem("Copy Task as Markdown", "copy_task_markdown"),
        commandItem("Copy Run ID", "copy_run_id"),
        commandItem("Copy Working Directory", "copy_working_directory"),
        commandItem("Pin or Unpin Task", "toggle_pin_task"),
        { type: "separator" },
        { role: "close" },
      ],
    },
    {
      label: "Edit",
      submenu: [
        { role: "undo" },
        { role: "redo" },
        { type: "separator" },
        { role: "cut" },
        { role: "copy" },
        { role: "paste" },
        { role: "selectAll" },
        { type: "separator" },
        commandItem("Toggle Dictation", "start_dictation"),
      ],
    },
    {
      label: "View",
      submenu: [
        commandItem("Command Menu…", "open_command_menu"),
        { type: "separator" },
        commandItem("Back", "navigate_back"),
        commandItem("Forward", "navigate_forward"),
        commandItem("Previous Surface", "previous_surface"),
        commandItem("Next Surface", "next_surface"),
        { type: "separator" },
        commandItem("Toggle Sidebar", "toggle_sidebar"),
        commandItem("Toggle Split View", "toggle_split"),
        commandItem("Toggle Task Summary", "toggle_task_summary"),
        commandItem("Open Terminal", "open_terminal"),
        commandItem("Open Worker Environment Browser", "open_browser"),
        commandItem("Open Worker Environment Changes", "open_changes"),
        commandItem("Open Model Picker", "open_model_picker"),
        commandItem("Open Side Task", "open_side_task"),
        { type: "separator" },
        { role: "togglefullscreen" },
      ],
    },
    { role: "windowMenu" },
    {
      label: "Help", role: "help",
      submenu: [
        { label: "Documentation", click: () => openHelpLink("https://github.com/miuuyy/Octopus-SI-OS/blob/main/docs/client.md") },
        { label: "Troubleshooting", click: () => openHelpLink("https://github.com/miuuyy/Octopus-SI-OS/blob/main/TROUBLESHOOTING.md") },
        { type: "separator" },
        { label: "Report a Problem…", click: () => {
          const win = BrowserWindow.getFocusedWindow();
          const token = win ? clientWindows.entryForWebContents(win.webContents.id)?.accessToken ?? "" : "";
          getClientSupportHost().open(token);
        } },
      ],
    },
  ]));
}

function openHelpLink(url) {
  void shell.openExternal(url).catch(() => dialog.showMessageBox({ type: "error", message: "Could not open your browser.", detail: url }));
}

function sendNativeClientCommand(command) {
  const focused = BrowserWindow.getFocusedWindow();
  const win = focused && !focused.isDestroyed() ? focused : selectedClientWindow(undefined);
  if (!win || win.isDestroyed()) return;
  sendClientWindowEvent(win, "octopus:native-client-command", command);
}

function appIconPath() {
  const candidates = [
    isDev ? path.join(__dirname, "../public/octopus-dev-dock-icon.png") : "",
    process.platform === "darwin" ? path.join(__dirname, "../dist/octopus.icns") : "",
    process.platform === "darwin" ? path.join(__dirname, "../public/octopus.icns") : "",
    path.join(__dirname, "../dist/octopus-app-icon.png"),
    path.join(__dirname, "../public/octopus-app-icon.png"),
    path.join(__dirname, "../dist/favicon.png"),
    path.join(__dirname, "../public/favicon.png"),
  ].filter(Boolean);
  for (const candidate of candidates) {
    try {
      require("node:fs").accessSync(candidate);
      return candidate;
    } catch {}
  }
  return "";
}

function defaultRuntimeEndpoint() {
  const configured = stringValue(process.env.OCTOPUS_APP_LISTEN);
  if (configured) return normalizedEndpoint(configured, resolveOctopusHome(), OPERATOR_CHAT_INTERFACE);
  return `unix://${runtimeControlSocketPath()}`;
}

function runtimeControlSocketPath() {
  return normalizedEndpoint("", resolveOctopusHome(), OPERATOR_CHAT_INTERFACE).slice("unix://".length);
}

function resolveSourceRegistry() {
  const configured = stringValue(process.env.OCTOPUS_SOURCE_REGISTRY);
  if (configured) return configured;
  if (app.isPackaged) {
    const bundled = path.join(app.getAppPath(), "registry");
    if (!fs.existsSync(path.join(bundled, "interface"))) {
      throw new Error(`packaged source registry '${bundled}' is incomplete`);
    }
    return bundled;
  }
  const installed = resolveInstalledSourceRegistry();
  if (installed) return installed;
  return path.join(resolveRepoRoot(), "registry");
}

function resolveLocalPackageRegistry() {
  const configured = stringValue(process.env.OCTOPUS_LOCAL_PACKAGE_REGISTRY);
  if (configured) return configured;
  const root = app.isPackaged
    ? path.join(app.getAppPath(), "local-packages", "registry")
    : path.join(resolveRepoRoot(), "registry");
  if (!fs.existsSync(path.join(root, ".octopus-source.json"))) {
    throw new Error(`local package registry '${root}' is incomplete`);
  }
  return root;
}

function resolveLocalPackageLibrary() {
  const configured = stringValue(process.env.OCTOPUS_LOCAL_PACKAGE_LIBRARY);
  if (configured) {
    if (!path.isAbsolute(configured)) throw new Error("OCTOPUS_LOCAL_PACKAGE_LIBRARY must be absolute");
    return configured;
  }
  const root = app.isPackaged
    ? path.join(app.getAppPath(), "local-packages")
    : path.join(resolveRepoRoot(), "app", "distribution", "local-packages");
  if (!fs.existsSync(path.join(root, "packages")) || !fs.existsSync(path.join(root, "packs"))) {
    throw new Error(`local package library '${root}' is incomplete`);
  }
  return root;
}

function resolveInstalledSourceRegistry() {
  const manifestPath = path.join(resolveOctopusHome(), "registry", ".octopus-source-manifest.json");
  if (!fs.existsSync(manifestPath)) return "";
  let manifest;
  try {
    manifest = JSON.parse(fs.readFileSync(manifestPath, "utf8"));
  } catch (error) {
    throw new Error(`runtime source manifest '${manifestPath}' cannot be read: ${error instanceof Error ? error.message : String(error)}`);
  }
  const sourceRegistry = stringValue(manifest?.source_registry);
  if (![1, 2].includes(manifest?.schema_version) || !sourceRegistry || !path.isAbsolute(sourceRegistry)) {
    throw new Error(`runtime source manifest '${manifestPath}' is malformed`);
  }
  if (manifest.schema_version === 2 && !/^[A-Za-z0-9][A-Za-z0-9._-]*$/.test(stringValue(manifest?.source_id))) {
    throw new Error(`runtime source manifest '${manifestPath}' has no valid source identity`);
  }
  return sourceRegistry;
}

function resolveRuntimeBinary() {
  const configured = stringValue(process.env.OCTOPUS_RUNTIME_BIN);
  if (configured) return configured;
  const name = process.platform === "win32" ? "octopus.exe" : "octopus";
  if (app.isPackaged) return path.join(app.getAppPath(), "bin", name);
  const root = resolveRepoRoot();
  const release = path.join(root, "target", "release", name);
  if (require("node:fs").existsSync(release)) return release;
  return path.join(root, "target", "debug", name);
}

function resolveRuntimeLaunchCommand() {
  return `${shellDisplay(resolveRuntimeBinary())} up`;
}

function resolveRuntimeLaunchCwd() {
  if (app.isPackaged) return os.homedir();
  return resolveRepoRoot() || os.homedir();
}

function resolveRuntimeLaunchEnv() {
  const env = {
    OCTOPUS_HOME: resolveOctopusHome(),
    OCTOPUS_SOURCE_REGISTRY: resolveSourceRegistry(),
    OCTOPUS_MACHINE_CODEX_AUTH_FILE: path.join(resolveOperatorCodexHome(), "auth.json"),
  };
  if (stringValue(process.env.OCTOPUS_RUNTIME_BIN)) return env;
  const root = resolveInstallationRoot();
  if (!root) return env;
  if (app.isPackaged) {
    return {
      ...env,
      OCTOPUS_INSTALLATION_ROOT: root,
      PATH: [path.join(root, "bin"), process.env.PATH ?? ""].join(path.delimiter),
    };
  }
  const targets = [path.join(root, "target", "release"), path.join(root, "target", "debug")];
  return { ...env, PATH: [...targets, process.env.PATH ?? ""].join(path.delimiter) };
}

function shellDisplay(value) {
  if (/^[A-Za-z0-9_./:\\-]+$/.test(value)) return value;
  if (process.platform === "win32") return `"${value.replaceAll('"', '""')}"`;
  return `'${value.replaceAll("'", `'\\''`)}'`;
}

function resolveOctopusHome() {
  const configured = stringValue(process.env.OCTOPUS_HOME);
  if (configured && !path.isAbsolute(configured)) {
    throw new Error("OCTOPUS_HOME must be absolute");
  }
  return path.resolve(configured || path.join(os.homedir(), ".octopus"));
}

function resolveOperatorCodexHome() {
  const configured = stringValue(process.env.CODEX_HOME);
  if (configured && !path.isAbsolute(configured)) {
    throw new Error("CODEX_HOME must be absolute");
  }
  return path.resolve(configured || path.join(os.homedir(), ".codex"));
}

function resolveInstallationRoot() {
  const configured = stringValue(process.env.OCTOPUS_INSTALLATION_ROOT);
  if (configured) {
    if (!path.isAbsolute(configured)) {
      throw new Error("OCTOPUS_INSTALLATION_ROOT must be absolute");
    }
    return configured;
  }
  if (app.isPackaged) return app.getAppPath();
  return resolveRepoRoot();
}

function resolveRepoRoot() {
  const envRoot = stringValue(process.env.OCTOPUS_REPO_ROOT);
  const candidates = [
    envRoot,
    app.getAppPath(),
    path.resolve(app.getAppPath(), ".."),
    process.cwd(),
    path.resolve(process.cwd(), ".."),
    path.resolve(__dirname, "..", ".."),
  ].filter(Boolean);
  for (const candidate of candidates) {
    try {
      const registry = path.join(candidate, "registry", "interface");
      require("node:fs").accessSync(registry);
      return candidate;
    } catch {}
  }
  return "";
}

function resolveClientSurfaceRoot() {
  const configured = stringValue(process.env.OCTOPUS_CLIENT_SURFACES_DIR);
  if (configured) {
    if (!path.isAbsolute(configured)) {
      throw new Error("OCTOPUS_CLIENT_SURFACES_DIR must be absolute");
    }
    return configured;
  }
  const sourceRoot = resolveClientSurfaceSourceCheckout();
  if (sourceRoot) return path.join(sourceRoot, "app", "client-surfaces");
  return path.join(app.getPath("userData"), "client-surfaces");
}

function resolveClientSurfaceSourceCheckout() {
  const starts = [process.cwd(), __dirname];
  const visited = new Set();
  for (const start of starts) {
    let candidate = path.resolve(start);
    while (!visited.has(candidate)) {
      visited.add(candidate);
      try {
        require("node:fs").accessSync(path.join(candidate, "registry", "interface"));
        require("node:fs").accessSync(path.join(candidate, "app", "client-surfaces", "README.md"));
        return candidate;
      } catch {}
      const parent = path.dirname(candidate);
      if (parent === candidate) break;
      candidate = parent;
    }
  }
  return "";
}

function parseEndpoint(endpoint) {
  const octopusHome = resolveOctopusHome();
  const normalized = normalizedEndpoint(
    requiredString(endpoint, "endpoint"),
    octopusHome,
    OPERATOR_CHAT_INTERFACE,
  );
  if (normalized.startsWith("unix://")) {
    const socketPath = normalized.slice("unix://".length) || defaultRuntimeEndpoint().slice("unix://".length);
    if (!path.isAbsolute(socketPath)) throw new Error(`unix endpoint path must be absolute: ${normalized}`);
    return {
      kind: "unix",
      path: socketPath,
      requestPath: "/rpc",
      hostHeader: "localhost",
    };
  }
  const url = new URL(normalized);
  if (url.protocol !== "ws:" && url.protocol !== "wss:") {
    throw new Error(`unsupported runtime endpoint protocol: ${url.protocol}`);
  }
  if (url.protocol === "wss:") {
    throw new Error("wss runtime endpoint is not supported by the local Electron bridge yet");
  }
  return {
    kind: "tcp",
    host: url.hostname,
    port: Number(url.port || 80),
    requestPath: `${url.pathname && url.pathname !== "/" ? url.pathname : "/rpc"}${url.search}`,
    hostHeader: url.host,
  };
}

function isExternalBrowserUrl(value) {
  try {
    const url = new URL(value);
    return url.protocol === "http:" || url.protocol === "https:";
  } catch {
    return false;
  }
}

function stringValue(value) {
  return typeof value === "string" ? value.trim() : "";
}

function formatError(error) {
  return error instanceof Error ? error.message : String(error);
}

function workerEnvironmentWindowTarget(value) {
  const request = objectOrEmpty(value);
  const workerEnvironmentId = stringValue(request.workerEnvironmentId);
  const runId = stringValue(request.runId);
  const panelKind = stringValue(request.panelKind) || "overview";
  const surfaceId = boundedOptionalField(request.surfaceId, "Worker Environment surface id", 512);
  const filePath = boundedOptionalField(request.filePath, "Worker Environment file path", 4_096);
  if (!workerEnvironmentId || workerEnvironmentId.length > 512 || /[\u0000-\u001f]/.test(workerEnvironmentId)) {
    throw new Error("A bounded Worker Environment id is required.");
  }
  if (runId.length > 512 || /[\u0000-\u001f]/.test(runId)) throw new Error("Run id is invalid.");
  if (!WORKER_ENVIRONMENT_PANEL_KINDS.has(panelKind)) throw new Error(`Unsupported Worker Environment panel: ${panelKind}`);
  if (filePath && !surfaceId) throw new Error("A Worker Environment file target requires an exact filesystem surface id.");
  const clientSurfaceValue = objectOrEmpty(request.clientSurface);
  const documentId = boundedOptionalField(clientSurfaceValue.documentId, "Client surface document id", 512);
  const sourceDigest = boundedOptionalField(clientSurfaceValue.sourceDigest, "Client surface source digest", 128);
  const revision = Number(clientSurfaceValue.revision);
  const hasClientSurface = Boolean(documentId || sourceDigest || clientSurfaceValue.revision !== undefined);
  if (hasClientSurface && (!surfaceId || !documentId || !sourceDigest || !Number.isSafeInteger(revision) || revision < 0)) {
    throw new Error("An exact client surface target requires surface id, document id, revision, and source digest.");
  }
  return {
    runId,
    workerEnvironmentId,
    panelKind,
    ...(surfaceId ? { surfaceId } : {}),
    ...(filePath ? { filePath } : {}),
    ...(hasClientSurface ? { clientSurface: { documentId, revision, sourceDigest } } : {}),
  };
}

function boundedOptionalField(value, label, maxLength) {
  if (value == null || value === "") return "";
  if (typeof value !== "string") throw new Error(`${label} must be a string.`);
  const text = value.trim();
  if (!text || text.length > maxLength || /[\u0000-\u001f]/.test(text)) throw new Error(`${label} is invalid.`);
  return text;
}

function objectOrEmpty(value) {
  return value && typeof value === "object" && !Array.isArray(value) ? value : {};
}

function requiredString(value, field) {
  const text = stringValue(value);
  if (!text) throw new Error(`${field} is required`);
  return text;
}

function numberValue(value, fallback) {
  return typeof value === "number" && Number.isFinite(value) ? value : fallback;
}

function windowStateSnapshot(win) {
  return {
    nativeControlInset: win && nativeChromeWindows.has(win) ? nativeControlInset(appZoomFactor) : win && customChromeWindows.has(win) ? 76 : 0,
    customWindowControls: Boolean(win && customChromeWindows.has(win)),
    fullScreen: Boolean(win && !win.isDestroyed() && win.isFullScreen()),
    maximized: Boolean(win && !win.isDestroyed() && win.isMaximized()),
    opaqueSurface: Boolean(win && !win.isDestroyed() && windowBackdropModes.get(win)),
    resizing: Boolean(win && !win.isDestroyed() && windowLiveResizeStates.get(win)),
  };
}

function publishWindowState(win) {
  if (win.isDestroyed() || win.webContents.isDestroyed()) return;
  win.webContents.send("octopus:window-state-changed", windowStateSnapshot(win));
}

function applyWindowBackdrop(win, force = false) {
  if (process.platform !== "darwin" || win.isDestroyed()) return;
  const appearance = windowAppearances.get(win);
  const translucent = appearance?.translucent !== false;
  const bounds = win.getBounds();
  const scaleFactor = screen.getDisplayMatching(bounds).scaleFactor;
  const physicalWidth = bounds.width * scaleFactor;
  const physicalHeight = bounds.height * scaleFactor;
  const oversized = Math.max(physicalWidth, physicalHeight) >= 3840
    && Math.min(physicalWidth, physicalHeight) >= 2160;
  const opaqueSurface = !translucent || oversized;
  if (!force && windowBackdropModes.get(win) === opaqueSurface) return;
  if (!opaqueSurface) {
    win.setBackgroundColor("#00000000");
    win.setVibrancy("under-window");
  } else {
    win.setBackgroundColor(nativeTheme.shouldUseDarkColors ? "#000000" : "#f9f9f9");
    win.setVibrancy(null);
  }
  windowBackdropModes.set(win, opaqueSurface);
  publishWindowState(win);
}

function bufferFromIpcBytes(value) {
  if (Buffer.isBuffer(value)) return value;
  if (value instanceof ArrayBuffer) return Buffer.from(value);
  if (ArrayBuffer.isView(value)) return Buffer.from(value.buffer, value.byteOffset, value.byteLength);
  throw new Error("attachment bytes must be an ArrayBuffer");
}

async function loadOperatorChatClient() {
  await initializeRuntimeHost();
  return getOperatorChatClientLoader().load();
}

async function authorizeClientInterfaceArtifactSubmission(request) {
  if (!isOperatorChatClientInterfaceCandidate(request?.interfaceId)) return;
  const { assertGenericOperatorChatArtifactAllowed } = await loadOperatorChatClient();
  await assertGenericOperatorChatArtifactAllowed(request);
}

function getOperatorChatClientLoader() {
  if (!operatorChatClientLoader) {
    operatorChatClientLoader = createOperatorChatClientLoader({
      packageRoots: clientSetupPackageRoots,
      clientPath: () => path.join(
        resolveOctopusHome(),
        "registry",
        "interface",
        OPERATOR_CHAT_INTERFACE,
        "client.cjs",
      ),
    });
  }
  return operatorChatClientLoader;
}

async function projectEePolicies() {
  const binary = resolveRuntimeBinary();
  const output = await new Promise((resolve, reject) => {
    const child = spawn(binary, ["project-ee-policies"], {
      cwd: path.dirname(binary),
      env: { ...process.env, ...resolveRuntimeLaunchEnv() },
      stdio: ["ignore", "pipe", "pipe"],
    });
    const stdout = [];
    const stderr = [];
    let stdoutBytes = 0;
    let stderrBytes = 0;
    let exceeded = false;
    child.stdout.on("data", (chunk) => {
      stdoutBytes += chunk.length;
      if (stdoutBytes > MAX_POLICY_PROJECTION_BYTES) {
        exceeded = true;
        child.kill();
        return;
      }
      stdout.push(chunk);
    });
    child.stderr.on("data", (chunk) => {
      stderrBytes += chunk.length;
      if (stderrBytes > MAX_POLICY_PROJECTION_BYTES) {
        exceeded = true;
        child.kill();
        return;
      }
      stderr.push(chunk);
    });
    child.on("error", (error) => reject(new Error(
      error?.code === "ENOENT"
        ? `policy projector binary is not built at '${binary}'`
        : error.message,
    )));
    child.on("close", (code) => {
      if (exceeded) {
        reject(new Error("policy projector output exceeded its bounded projection size"));
        return;
      }
      if (code !== 0) {
        reject(new Error(Buffer.concat(stderr).toString("utf8").trim() || `policy projector exited with code ${code}`));
        return;
      }
      resolve(Buffer.concat(stdout).toString("utf8"));
    });
  });
  const projection = JSON.parse(output);
  if (!projection || typeof projection !== "object" || !Array.isArray(projection.environments)) {
    throw new Error("policy projector returned a malformed snapshot");
  }
  return projection;
}
