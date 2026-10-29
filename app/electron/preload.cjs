const { contextBridge, ipcRenderer, webFrame, webUtils } = require("electron");

const OPERATOR_CHAT_SUBMIT_RESULT = "octopus.operator-chat.submit.v1";
const OPERATOR_CHAT_SUBMIT_ERROR_MESSAGE = "Couldn't send your message. Try again.";
const CLIENT_EXTENSION_ACTION_RESULT = "octopus.client-extension.action.v1";
const CLIENT_EXTENSION_ACTION_ERROR_MESSAGE = "Octopus couldn't complete the repair. Try again.";

function unwrapOperatorChatSubmitResult(result) {
  if (result?.contract === OPERATOR_CHAT_SUBMIT_RESULT && result.ok === true) {
    return result.value;
  }
  const message = result?.contract === OPERATOR_CHAT_SUBMIT_RESULT
    && result.ok === false
    && typeof result.error?.message === "string"
    && result.error.message.trim()
    ? result.error.message
    : OPERATOR_CHAT_SUBMIT_ERROR_MESSAGE;
  const error = new Error(message);
  error.name = "OctopusChatSubmitError";
  error.code = result?.contract === OPERATOR_CHAT_SUBMIT_RESULT
    && result.ok === false
    && typeof result.error?.code === "string"
    ? result.error.code
    : "message_send_failed";
  error.retryable = result?.contract === OPERATOR_CHAT_SUBMIT_RESULT
    && result.ok === false
    && result.error?.retryable === true;
  error.deliveryState = result?.contract === OPERATOR_CHAT_SUBMIT_RESULT
    && result.ok === false
    && result.error?.deliveryState === "not_sent" ? "not_sent" : "unknown";
  throw error;
}

function unwrapClientExtensionActionResult(result) {
  if (result?.contract === CLIENT_EXTENSION_ACTION_RESULT && result.ok === true) {
    return result.value;
  }
  const message = result?.contract === CLIENT_EXTENSION_ACTION_RESULT
    && result.ok === false
    && typeof result.error?.message === "string"
    && result.error.message.trim()
    ? result.error.message
    : CLIENT_EXTENSION_ACTION_ERROR_MESSAGE;
  const error = new Error(message);
  error.name = "OctopusClientExtensionActionError";
  error.code = result?.contract === CLIENT_EXTENSION_ACTION_RESULT
    && result.ok === false
    && typeof result.error?.code === "string"
    ? result.error.code
    : "repair_failed";
  error.retryable = result?.contract === CLIENT_EXTENSION_ACTION_RESULT
    && result.ok === false
    && result.error?.retryable === true;
  throw error;
}

function unwrapApplicationConnectionResult(result) {
  if (result?.contract === "octopus.application-connection.v1" && result.ok === true) return result.value;
  const error = new Error(result?.contract === "octopus.application-connection.v1"
    && typeof result.error?.message === "string"
    && result.error.message.trim()
    ? result.error.message
    : "Octopus couldn't finish the connection.");
  error.name = "OctopusApplicationConnectionError";
  error.code = result?.error?.code === "cancelled" ? "cancelled" : "failed";
  throw error;
}

const inheritedAccessClaim = process.argv.find((argument) => argument.startsWith("--octopus-access-claim="))?.slice("--octopus-access-claim=".length) ?? "";
let accessToken = inheritedAccessClaim ? ipcRenderer.sendSync("octopus:access-inherit", inheritedAccessClaim) : "";
let runtimeEventListenerCount = 0;

async function refreshRuntimeEventSubscription() {
  if (runtimeEventListenerCount <= 0) return;
  await ipcRenderer.invoke("octopus:runtime-events-unsubscribe", accessToken);
  await ipcRenderer.invoke("octopus:runtime-events-subscribe", accessToken);
}

function subscribeRuntimeEvent(channel, listener) {
  const wrapped = (_event, payload) => listener(payload);
  runtimeEventListenerCount += 1;
  ipcRenderer.on(channel, wrapped);
  void refreshRuntimeEventSubscription().catch(() => undefined);
  return () => {
    ipcRenderer.removeListener(channel, wrapped);
    runtimeEventListenerCount = Math.max(0, runtimeEventListenerCount - 1);
    if (runtimeEventListenerCount === 0) {
      void ipcRenderer.invoke("octopus:runtime-events-unsubscribe", accessToken).catch(() => undefined);
    }
  };
}

async function establishAccess(channel, password) {
  const result = await ipcRenderer.invoke(channel, password);
  accessToken = result.token;
  await refreshRuntimeEventSubscription();
  return ipcRenderer.invoke("octopus:access-status", accessToken);
}

function invokeAccess(channel, ...args) {
  return ipcRenderer.invoke(channel, accessToken, ...args);
}

function beginClientPaneTransferSync(request) {
  const result = ipcRenderer.sendSync("octopus:client-pane-transfer-begin", accessToken, request);
  if (result?.ok === true) return result.value;
  throw new Error(typeof result?.error?.message === "string" && result.error.message
    ? result.error.message
    : "Client pane transfer could not begin.");
}

function acknowledgeClientPaneTransferSync(request) {
  const result = ipcRenderer.sendSync("octopus:client-pane-transfer-acknowledge", accessToken, request);
  if (result?.ok === true) return result.value;
  throw new Error(typeof result?.error?.message === "string" && result.error.message
    ? result.error.message
    : "Client pane transfer source acknowledgement failed.");
}

contextBridge.exposeInMainWorld("octopusTransport", {
  showClientContextMenu(request) {
    return ipcRenderer.invoke("octopus:client-context-menu", request);
  },
  cancelClientContextMenu(request) {
    return ipcRenderer.invoke("octopus:client-context-menu-cancel", request);
  },
  windowZoomFactor() {
    return webFrame.getZoomFactor();
  },
  appInfo() {
    return ipcRenderer.invoke("octopus:app-info");
  },
  openProblemReport() {
    return invokeAccess("octopus:support-open");
  },
  runClientDoctor() {
    return invokeAccess("octopus:client-doctor");
  },
  copyText(value) {
    return invokeAccess("octopus:clipboard-write", value);
  },
  logClientEvent(request) {
    return invokeAccess("octopus:client-log", request);
  },
  openClientLogs() {
    return invokeAccess("octopus:open-client-logs");
  },
  selectClientDirectory(request) {
    return invokeAccess("octopus:client-directory-select", request);
  },
  selectClientAttachments(request) {
    return invokeAccess("octopus:client-attachments-select", request);
  },
  async stageDroppedAttachments(files) {
    if (!Array.isArray(files) || files.length > 12) throw new Error("A task can include up to 12 dropped files or folders.");
    const paths = [];
    const fileIndices = [];
    for (const [index, file] of files.entries()) {
      const nativePath = webUtils.getPathForFile(file);
      if (nativePath) {
        paths.push(nativePath);
        fileIndices.push(index);
      }
    }
    const selection = paths.length
      ? await invokeAccess("octopus:stage-dropped-attachments", paths)
      : { canceled: false, attachments: [] };
    return { ...selection, fileIndices };
  },
  readClientAttachmentPreview(request) {
    return invokeAccess("octopus:client-attachment-preview", request);
  },
  discardClientAttachmentStages(handles) {
    return invokeAccess("octopus:attachment-stages-discard", handles);
  },
  beginClientAttachmentStage(request) {
    return invokeAccess("octopus:attachment-stage-begin", request);
  },
  appendClientAttachmentStage(request) {
    return invokeAccess("octopus:attachment-stage-append", request);
  },
  finalizeClientAttachmentStage(request) {
    return invokeAccess("octopus:attachment-stage-finalize", request);
  },
  abortClientAttachmentStage(request) {
    return invokeAccess("octopus:attachment-stage-abort", request);
  },
  saveClientFile(request) {
    return invokeAccess("octopus:client-file-save", request);
  },
  downloadClientFileCopy(request) {
    return invokeAccess("octopus:client-file-download-copy", request);
  },
  openClientFile(request) {
    return invokeAccess("octopus:client-file-open", request);
  },
  listClientFileOpenTargets(request) {
    return invokeAccess("octopus:client-file-open-targets", request);
  },
  openClientFileWithApplication(request) {
    return invokeAccess("octopus:client-file-open-with", request);
  },
  startClientFileDrag(request) {
    ipcRenderer.send("octopus:client-file-start-drag", accessToken, request);
  },
  accessStatus() {
    return ipcRenderer.invoke("octopus:access-status", accessToken);
  },
  configureAccess(password) {
    return establishAccess("octopus:access-configure", password);
  },
  unlockAccess(password) {
    return establishAccess("octopus:access-unlock", password);
  },
  touchIdStatus() {
    return ipcRenderer.invoke("octopus:touch-id-status");
  },
  async setTouchIdEnabled(enabled) {
    return invokeAccess("octopus:touch-id-set-enabled", enabled);
  },
  async unlockWithTouchId() {
    const result = await ipcRenderer.invoke("octopus:touch-id-unlock");
    accessToken = result.token;
    await refreshRuntimeEventSubscription();
    return ipcRenderer.invoke("octopus:access-status", accessToken);
  },
  async lockAccess() {
    const token = accessToken;
    const result = await ipcRenderer.invoke("octopus:access-lock", token);
    if (!result.authenticated && accessToken === token) {
      accessToken = "";
      await ipcRenderer.invoke("octopus:runtime-events-unsubscribe", token);
    }
    return result;
  },
  labAccess() {
    return invokeAccess("octopus:lab-access-read");
  },
  updateLabAccess(password) {
    return invokeAccess("octopus:lab-access-update", password);
  },
  accessDetails() {
    return invokeAccess("octopus:access-details");
  },
  updateAdministratorAccess(request) {
    return invokeAccess("octopus:administrator-access-update", request);
  },
  requestMicrophoneAccess() {
    return invokeAccess("octopus:request-microphone-access");
  },
  requestCameraAccess() {
    return invokeAccess("octopus:request-camera-access");
  },
  cameraAccessStatus() {
    return invokeAccess("octopus:camera-access-status");
  },
  saveGestureTrace(text) {
    return invokeAccess("octopus:gesture-trace-save", text);
  },
  revealGestureTrace(target) {
    return invokeAccess("octopus:gesture-trace-reveal", target);
  },
  clientWindowBootstrap() {
    return ipcRenderer.sendSync("octopus:client-window-bootstrap");
  },
  openClientWindow(request) {
    return invokeAccess("octopus:client-window-open", request);
  },
  clientWindowReady(request) {
    return invokeAccess("octopus:client-window-ready", request);
  },
  registerClientWindowCloseGuard() { return invokeAccess("octopus:client-window-close-register"); },
  unregisterClientWindowCloseGuard(request) { return invokeAccess("octopus:client-window-close-unregister", request); },
  resolveClientWindowClose(request) { return invokeAccess("octopus:client-window-close-resolve", request); },
  onClientWindowCloseRequested(listener) {
    const wrapped = (_event, request) => listener(request);
    ipcRenderer.on("octopus:client-window-close-requested", wrapped);
    return () => ipcRenderer.removeListener("octopus:client-window-close-requested", wrapped);
  },
  onClientWindowCloseCancelled(listener) {
    const wrapped = (_event, request) => listener(request);
    ipcRenderer.on("octopus:client-window-close-cancelled", wrapped);
    return () => ipcRenderer.removeListener("octopus:client-window-close-cancelled", wrapped);
  },
  beginClientPaneTransfer(request) {
    return beginClientPaneTransferSync(request);
  },
  beginClientPaneDrag(request) { return invokeAccess("octopus:client-pane-drag-begin", request); },
  moveClientPaneDrag(request) { return invokeAccess("octopus:client-pane-drag-move", request); },
  finishClientPaneDrag(request) { return invokeAccess("octopus:client-pane-drag-finish", request); },
  cancelClientPaneDrag(request) { return invokeAccess("octopus:client-pane-drag-cancel", request); },
  reattachClientPane(request) { return invokeAccess("octopus:client-pane-drag-reattach", request); },
  detachClientPane(request) { return invokeAccess("octopus:client-pane-drag-detach", request); },
  pendingClientPaneSourceFinalizations() { return invokeAccess("octopus:client-pane-source-finalizations"); },
  acknowledgeClientPaneSourceFinalization(request) { return invokeAccess("octopus:client-pane-source-finalization-ack", request); },
  registerClientPaneDropZone(request) { return invokeAccess("octopus:client-pane-drag-drop-zone", request); },
  anchorClientPaneDrop(request) { return invokeAccess("octopus:client-pane-drag-anchor", request); },
  onClientPaneDragState(listener) {
    const wrapped = (_event, value) => listener(value);
    ipcRenderer.on("octopus:client-pane-drag-state", wrapped);
    return () => ipcRenderer.removeListener("octopus:client-pane-drag-state", wrapped);
  },
  onClientPaneTransferAccepted(listener) {
    const wrapped = (_event, value) => listener(value);
    ipcRenderer.on("octopus:client-pane-transfer-accepted", wrapped);
    return () => ipcRenderer.removeListener("octopus:client-pane-transfer-accepted", wrapped);
  },
  onClientPaneTransferFinalized(listener) {
    const wrapped = (_event, value) => listener(value);
    ipcRenderer.on("octopus:client-pane-transfer-finalized", wrapped);
    return () => ipcRenderer.removeListener("octopus:client-pane-transfer-finalized", wrapped);
  },
  onClientPaneTransferCancelled(listener) {
    const wrapped = (_event, value) => listener(value);
    ipcRenderer.on("octopus:client-pane-transfer-cancelled", wrapped);
    return () => ipcRenderer.removeListener("octopus:client-pane-transfer-cancelled", wrapped);
  },
  acceptClientPaneTransfer(request) {
    return invokeAccess("octopus:client-pane-transfer-accept", request);
  },
  commitClientPaneTransfer(request) {
    return invokeAccess("octopus:client-pane-transfer-commit", request);
  },
  acknowledgeClientPaneTransfer(request) {
    return acknowledgeClientPaneTransferSync(request);
  },
  rejectClientPaneTransfer(request) {
    return invokeAccess("octopus:client-pane-transfer-reject", request);
  },
  cancelClientPaneTransfer(request) {
    return invokeAccess("octopus:client-pane-transfer-cancel", request);
  },
  pendingClientPaneTransfers() {
    return invokeAccess("octopus:client-pane-transfer-pending");
  },
  pendingClientPaneDestinationTransfers() {
    return invokeAccess("octopus:client-pane-transfer-destination-pending");
  },
  resumeClientPaneTransfer(request) {
    return invokeAccess("octopus:client-pane-transfer-resume", request);
  },
  finalizeClientPaneTransfer(request) {
    return invokeAccess("octopus:client-pane-transfer-finalize", request);
  },
  windowState() {
    return ipcRenderer.invoke("octopus:window-state");
  },
  setHistoryNavigationState(state) {
    ipcRenderer.send("octopus:history-navigation-state", state);
  },
  setWindowAppearance(appearance) {
    ipcRenderer.send("octopus:window-appearance", appearance);
  },
  setPowerSaveBlocker(enabled) {
    return invokeAccess("octopus:power-save-blocker", enabled);
  },
  setGlobalDictationShortcut(request) {
    return invokeAccess("octopus:global-dictation-shortcut", request);
  },
  setNativeCommandShortcuts(shortcuts) {
    return invokeAccess("octopus:native-command-shortcuts", shortcuts);
  },
  openQuickTaskWindow() {
    return invokeAccess("octopus:quick-task-open");
  },
  closeQuickTaskWindow() {
    return invokeAccess("octopus:quick-task-close");
  },
  openRunInMain(runId) {
    return invokeAccess("octopus:quick-task-open-main", runId);
  },
  openWorkerEnvironmentWindow(request) {
    return invokeAccess("octopus:worker-environment-window-open", request);
  },
  openWorkerEnvironmentInMain(request) {
    return invokeAccess("octopus:worker-environment-window-open-main", request);
  },
  setWorkerEnvironmentWindowPinned(enabled) {
    return invokeAccess("octopus:worker-environment-window-pinned", enabled);
  },
  openTaskMonitorWindow(runId) {
    return invokeAccess("octopus:task-monitor-window-open", runId);
  },
  setTaskMonitorWindowPinned(enabled) {
    return invokeAccess("octopus:task-monitor-window-pinned", enabled);
  },
  loginItemSettings(request) {
    return invokeAccess("octopus:login-item-settings", request);
  },
  windowControl(action) {
    ipcRenderer.send("octopus:window-control", action);
  },
  relaunchClient() {
    return invokeAccess("octopus:client-relaunch");
  },
  defaults() {
    return invokeAccess("octopus:transport-defaults");
  },
  providerInterfaces() {
    return invokeAccess("octopus:provider-interfaces");
  },
  bootStatus() {
    return invokeAccess("octopus:boot-status");
  },
  chatgptAccount() {
    return invokeAccess("octopus:chatgpt-account");
  },
  chatgptRateLimits() {
    return invokeAccess("octopus:chatgpt-rate-limits");
  },
  chatgptLogout() {
    return invokeAccess("octopus:chatgpt-logout");
  },
  onChatgptAccountChanged(listener) {
    const wrapped = () => listener();
    ipcRenderer.on("octopus:chatgpt-account-changed", wrapped);
    return () => ipcRenderer.removeListener("octopus:chatgpt-account-changed", wrapped);
  },
  chatgptSignInStart() {
    return invokeAccess("octopus:chatgpt-sign-in-start");
  },
  chatgptSignInStatus(loginId) {
    return invokeAccess("octopus:chatgpt-sign-in-status", loginId);
  },
  chatgptSignInCancel(loginId) {
    return invokeAccess("octopus:chatgpt-sign-in-cancel", loginId);
  },
  openLaunchTerminal(request) {
    return invokeAccess("octopus:launch-terminal-open", request);
  },
  writeLaunchTerminal(sessionId, data) {
    return invokeAccess("octopus:launch-terminal-write", sessionId, data);
  },
  resizeLaunchTerminal(sessionId, request) {
    return invokeAccess("octopus:launch-terminal-resize", sessionId, request);
  },
  openTerminal(request) {
    return invokeAccess("octopus:terminal-open", request);
  },
  writeTerminal(terminalId, sessionId, data) {
    return invokeAccess("octopus:terminal-write", terminalId, sessionId, data);
  },
  resizeTerminal(terminalId, sessionId, request) {
    return invokeAccess("octopus:terminal-resize", terminalId, sessionId, request);
  },
  closeTerminal(terminalId) {
    return invokeAccess("octopus:terminal-close", terminalId);
  },
  runtimeStatus(request) {
    return invokeAccess("octopus:runtime-status", request);
  },
  validateRuntimeEndpoint(request) {
    return invokeAccess("octopus:validate-runtime-endpoint", request);
  },
  projectionRead(request) {
    return invokeAccess("octopus:projection-read", request);
  },
  workspaceSetupState() {
    return invokeAccess("octopus:workspace-setup-state");
  },
  clientSetupRead(request) {
    return invokeAccess("octopus:client-setup-read", request);
  },
  clientSetupReadCurrent() {
    return invokeAccess("octopus:client-setup-read-current");
  },
  clientSetupBegin(request) {
    return invokeAccess("octopus:client-setup-begin", request);
  },
  clientSetupResume(request) {
    return invokeAccess("octopus:client-setup-resume", request);
  },
  mutateScheduledInterface(request) {
    return invokeAccess("octopus:scheduled-interface-mutate", request);
  },
  mutateApplicationPrivateConfiguration(request) {
    return invokeAccess("octopus:application-private-configuration-mutate", request);
  },
  async applicationConnectionProbe(request) {
    return unwrapApplicationConnectionResult(await invokeAccess("octopus:application-connection-probe", request));
  },
  async applicationConnectionConnect(request) {
    return unwrapApplicationConnectionResult(await invokeAccess("octopus:application-connection-connect", request));
  },
  async applicationConnectionCancel(request) {
    return unwrapApplicationConnectionResult(await invokeAccess("octopus:application-connection-cancel", request));
  },
  async applicationConnectionDisconnect(request) {
    return unwrapApplicationConnectionResult(await invokeAccess("octopus:application-connection-disconnect", request));
  },
  mutateOctopusOsWorkspace(request) {
    return invokeAccess("octopus:os-workspace-mutate", request);
  },
  openCdpViewer(request) {
    return invokeAccess("octopus:cdp-viewer-open", request);
  },
  prepareNativeBrowserViewer(request) {
    return invokeAccess("octopus:native-browser-viewer-prepare", request);
  },
  acknowledgeNativeBrowserViewer(request) {
    return invokeAccess("octopus:native-browser-viewer-ack", request);
  },
  nativeBrowserCursorArrived(request) {
    return invokeAccess("octopus:native-browser-viewer-cursor-arrived", request);
  },
  syncNativeBrowserViewer(request) {
    return invokeAccess("octopus:native-browser-viewer-sync", request);
  },
  nativeBrowserDownloads(request) {
    return invokeAccess("octopus:native-browser-viewer-downloads", request);
  },
  nativeBrowserViewerAction(request) {
    return invokeAccess("octopus:native-browser-viewer-action", request);
  },
  closeNativeBrowserViewer(request) {
    return invokeAccess("octopus:native-browser-viewer-close", request);
  },
  clientBrowser(request) { return invokeAccess("octopus:client-browser", request); },
  onClientBrowserState(listener) {
    const handler = (_event, state) => listener(state);
    ipcRenderer.on("octopus:client-browser-state", handler);
    return () => ipcRenderer.removeListener("octopus:client-browser-state", handler);
  },
  onClientBrowserOpened(listener) {
    const handler = (_event, state) => listener(state);
    ipcRenderer.on("octopus:client-browser-opened", handler);
    return () => ipcRenderer.removeListener("octopus:client-browser-opened", handler);
  },
  closeNativeBrowserPage(request) {
    return invokeAccess("octopus:native-browser-page-close", request);
  },
  browserSavedPermissions(request) {
    return invokeAccess("octopus:browser-saved-permissions", request);
  },
  desktopSavedApplicationPermissions(request) {
    return invokeAccess("octopus:desktop-saved-application-permissions", request);
  },
  desktopComputerUseSettings(request) {
    return invokeAccess("octopus:desktop-computer-use-settings", request);
  },
  openDesktopPermissions(request) {
    return invokeAccess("octopus:desktop-permissions-open", request);
  },
  desktopSurfaceRequest(request) {
    return invokeAccess("octopus:desktop-surface-request", request);
  },
  desktopApplicationView(request) {
    return invokeAccess("octopus:desktop-application-view", request);
  },
  cdpViewerAction(request) {
    return invokeAccess("octopus:cdp-viewer-action", request);
  },
  closeCdpViewer(request) {
    return invokeAccess("octopus:cdp-viewer-close", request);
  },
  openProcessViewer(request) {
    return invokeAccess("octopus:process-viewer-open", request);
  },
  processViewerAction(request) {
    return invokeAccess("octopus:process-viewer-action", request);
  },
  closeProcessViewer(request) {
    return invokeAccess("octopus:process-viewer-close", request);
  },
  listWorkerEnvironmentFiles(request) {
    return invokeAccess("octopus:worker-environment-files-list", request);
  },
  readWorkerEnvironmentFile(request) {
    return invokeAccess("octopus:worker-environment-file-read", request);
  },
  readWorkerEnvironmentFilePreview(request) {
    return invokeAccess("octopus:worker-environment-file-preview", request);
  },
  readWorkerEnvironmentFileAttachment(request) {
    return invokeAccess("octopus:worker-environment-file-attachment", request);
  },
  writeWorkerEnvironmentFile(request) {
    return invokeAccess("octopus:worker-environment-file-write", request);
  },
  performWorkerEnvironmentFileSystemAction(request) {
    return invokeAccess("octopus:worker-environment-file-system-action", request);
  },
  osFileRoots() {
    return invokeAccess("octopus:os-files-roots");
  },
  listOsFiles(request) {
    return invokeAccess("octopus:os-files-list", request);
  },
  readOsFile(request) {
    return invokeAccess("octopus:os-file-read", request);
  },
  writeOsFile(request) {
    return invokeAccess("octopus:os-file-write", request);
  },
  performOsFileSystemAction(request) {
    return invokeAccess("octopus:os-file-system-action", request);
  },
  inspectWorkerEnvironmentGit(request) {
    return invokeAccess("octopus:worker-environment-git-inspect", request);
  },
  performWorkerEnvironmentOperatorAction(request) {
    return invokeAccess("octopus:worker-environment-operator-action", request);
  },
  clientSurfaceDocuments() {
    return invokeAccess("octopus:client-surface-documents");
  },
  clientExtensions() {
    return invokeAccess("octopus:client-extensions");
  },
  mutateClientExtension(request) {
    return invokeAccess("octopus:client-extension-mutate", request);
  },
  setClientExtensionCredential(request) {
    return invokeAccess("octopus:client-extension-credential", request);
  },
  checkClientExtensionCredential(request) {
    return invokeAccess("octopus:client-extension-credential-check", request);
  },
  applyClientExtensionLifecycle(request) {
    return invokeAccess("octopus:client-extension-lifecycle", request);
  },
  revealClientExtensions() {
    return invokeAccess("octopus:client-extensions-reveal");
  },
  readClientExtensionFilesystemSource(request) {
    return invokeAccess("octopus:client-extension-filesystem-read", request);
  },
  async runClientExtensionAction(request) {
    return unwrapClientExtensionActionResult(await invokeAccess("octopus:client-extension-action", request));
  },
  readCodexUsage(request) {
    return invokeAccess("octopus:client-codex-usage-read", request);
  },
  refreshProviderModels(request) {
    return invokeAccess("octopus:provider-models-refresh", request);
  },
  requestComposerPrediction(request) {
    return invokeAccess("octopus:composer-prediction", request);
  },
  requestTaskTitle(request) {
    return invokeAccess("octopus:task-title", request);
  },
  async submitChatTurn(request) {
    return unwrapOperatorChatSubmitResult(await invokeAccess("octopus:submit-chat-turn", request));
  },
  readOperatorChatArtifacts(request) {
    return invokeAccess("octopus:operator-chat-artifacts-read", request);
  },
  submitOperatorTurnControl(request) {
    return invokeAccess("octopus:submit-operator-turn-control", request);
  },
  async submitChatSteer(request) {
    return unwrapOperatorChatSubmitResult(await invokeAccess("octopus:submit-chat-steer", request));
  },
  updateChatPins(request) {
    return invokeAccess("octopus:update-chat-pins", request);
  },
  transcribeDictation(request) {
    return invokeAccess("octopus:transcribe-dictation", request);
  },
  startDictationStream(request) {
    return invokeAccess("octopus:dictation-stream-start", request);
  },
  appendDictationAudio(attemptId, bytes) {
    ipcRenderer.send("octopus:dictation-stream-audio", accessToken, attemptId, bytes);
  },
  finishDictationStream(attemptId) {
    return invokeAccess("octopus:dictation-stream-finish", attemptId);
  },
  onDictationStreamEvent(listener) {
    const wrapped = (_event, payload) => listener(payload);
    ipcRenderer.on("octopus:dictation-stream-event", wrapped);
    return () => ipcRenderer.removeListener("octopus:dictation-stream-event", wrapped);
  },
  cancelDictation(attemptId) {
    return invokeAccess("octopus:cancel-dictation", attemptId);
  },
  readDictationRecordings() {
    return invokeAccess("octopus:dictation-recordings-read");
  },
  configureDictationRecordings(enabled) {
    return invokeAccess("octopus:dictation-recordings-configure", enabled);
  },
  saveDictationRecording(request) {
    return invokeAccess("octopus:dictation-recording-save", request);
  },
  readDictationRecording(id) {
    return invokeAccess("octopus:dictation-recording-read", id);
  },
  updateDictationRecording(request) {
    return invokeAccess("octopus:dictation-recording-update", request);
  },
  deleteDictationRecording(id) {
    return invokeAccess("octopus:dictation-recording-delete", id);
  },
  submitInterfaceArtifact(request) {
    return invokeAccess("octopus:submit-interface-artifact", request);
  },
  beginInterfaceArtifactStream(request) {
    return invokeAccess("octopus:artifact-stream-begin", request);
  },
  appendInterfaceArtifactStream(request) {
    return invokeAccess("octopus:artifact-stream-append", request);
  },
  finalizeInterfaceArtifactStream(request) {
    return invokeAccess("octopus:artifact-stream-finalize", request);
  },
  abortInterfaceArtifactStream(request) {
    return invokeAccess("octopus:artifact-stream-abort", request);
  },
  onProjectionChanged(listener) {
    const wrapped = (_event, payload) => listener(payload);
    ipcRenderer.on("octopus:projection-changed", wrapped);
    return () => ipcRenderer.removeListener("octopus:projection-changed", wrapped);
  },
  onRuntimeHostState(listener) {
    return subscribeRuntimeEvent("octopus:runtime-host-state", listener);
  },
  onTerminalData(listener) {
    const wrapped = (_event, payload) => listener(payload);
    ipcRenderer.on("octopus:terminal-data", wrapped);
    return () => ipcRenderer.removeListener("octopus:terminal-data", wrapped);
  },
  onTerminalExit(listener) {
    const wrapped = (_event, payload) => listener(payload);
    ipcRenderer.on("octopus:terminal-exit", wrapped);
    return () => ipcRenderer.removeListener("octopus:terminal-exit", wrapped);
  },
  onCdpViewerEvent(listener) {
    const wrapped = (_event, payload) => listener(payload);
    ipcRenderer.on("octopus:cdp-viewer-event", wrapped);
    return () => ipcRenderer.removeListener("octopus:cdp-viewer-event", wrapped);
  },
  onNativeBrowserViewerEvent(listener) {
    const wrapped = (_event, payload) => listener(payload);
    ipcRenderer.on("octopus:native-browser-viewer-event", wrapped);
    return () => ipcRenderer.removeListener("octopus:native-browser-viewer-event", wrapped);
  },
  onProcessViewerEvent(listener) {
    const wrapped = (_event, payload) => listener(payload);
    ipcRenderer.on("octopus:process-viewer-event", wrapped);
    return () => ipcRenderer.removeListener("octopus:process-viewer-event", wrapped);
  },
  onWindowStateChanged(listener) {
    const wrapped = (_event, payload) => listener(payload);
    ipcRenderer.on("octopus:window-state-changed", wrapped);
    return () => ipcRenderer.removeListener("octopus:window-state-changed", wrapped);
  },
  onNavigateBack(listener) {
    const wrapped = () => listener();
    ipcRenderer.on("octopus:navigate-back", wrapped);
    return () => ipcRenderer.removeListener("octopus:navigate-back", wrapped);
  },
  onNavigateForward(listener) {
    const wrapped = () => listener();
    ipcRenderer.on("octopus:navigate-forward", wrapped);
    return () => ipcRenderer.removeListener("octopus:navigate-forward", wrapped);
  },
  onGlobalDictationRequested(listener) {
    const wrapped = () => listener();
    ipcRenderer.on("octopus:global-dictation-requested", wrapped);
    return () => ipcRenderer.removeListener("octopus:global-dictation-requested", wrapped);
  },
  onNativeClientCommand(listener) {
    const wrapped = (_event, command) => listener(command);
    ipcRenderer.on("octopus:native-client-command", wrapped);
    return () => ipcRenderer.removeListener("octopus:native-client-command", wrapped);
  },
  onOpenRunRequested(listener) {
    const wrapped = (_event, runId) => listener(runId);
    ipcRenderer.on("octopus:open-run-requested", wrapped);
    return () => ipcRenderer.removeListener("octopus:open-run-requested", wrapped);
  },
  onClientAccessLocked(listener) {
    const wrapped = (_event, status) => {
      const token = accessToken;
      accessToken = "";
      if (token) void ipcRenderer.invoke("octopus:runtime-events-unsubscribe", token).catch((error) => console.error("Client event unsubscribe failed:", error));
      listener(status);
    };
    ipcRenderer.on("octopus:client-access-locked", wrapped);
    return () => ipcRenderer.removeListener("octopus:client-access-locked", wrapped);
  },
  onClientPaneTransferCompleted(listener) {
    const wrapped = (_event, completion) => listener(completion);
    ipcRenderer.on("octopus:client-pane-transfer-completed", wrapped);
    return () => ipcRenderer.removeListener("octopus:client-pane-transfer-completed", wrapped);
  },
  onOpenWorkerEnvironmentRequested(listener) {
    const wrapped = (_event, target) => listener(target);
    ipcRenderer.on("octopus:open-worker-environment-requested", wrapped);
    return () => ipcRenderer.removeListener("octopus:open-worker-environment-requested", wrapped);
  },
  onWorkerEnvironmentWindowTarget(listener) {
    const wrapped = (_event, target) => listener(target);
    ipcRenderer.on("octopus:worker-environment-window-target", wrapped);
    return () => ipcRenderer.removeListener("octopus:worker-environment-window-target", wrapped);
  },
  onClientSurfaceDocumentsChanged(listener) {
    const wrapped = (_event, payload) => listener(payload);
    ipcRenderer.on("octopus:client-surface-documents-changed", wrapped);
    return () => ipcRenderer.removeListener("octopus:client-surface-documents-changed", wrapped);
  },
  onClientExtensionsChanged(listener) {
    const wrapped = (_event, payload) => listener(payload);
    ipcRenderer.on("octopus:client-extensions-changed", wrapped);
    return () => ipcRenderer.removeListener("octopus:client-extensions-changed", wrapped);
  },
  onApplicationConnectionProgress(listener) {
    const wrapped = (_event, payload) => listener(payload);
    ipcRenderer.on("octopus:application-connection-progress", wrapped);
    return () => ipcRenderer.removeListener("octopus:application-connection-progress", wrapped);
  },
  onApplicationLifecycleProgress(listener) {
    const wrapped = (_event, payload) => listener(payload);
    ipcRenderer.on("octopus:application-lifecycle-progress", wrapped);
    return () => ipcRenderer.removeListener("octopus:application-lifecycle-progress", wrapped);
  },
  onWorkspaceSetupProgress(listener) {
    const wrapped = (_event, payload) => listener(payload);
    ipcRenderer.on("octopus:workspace-setup-progress", wrapped);
    return () => ipcRenderer.removeListener("octopus:workspace-setup-progress", wrapped);
  },
  onClientSetupProgress(listener) {
    const wrapped = (_event, payload) => listener(payload);
    ipcRenderer.on("octopus:client-setup-progress", wrapped);
    return () => ipcRenderer.removeListener("octopus:client-setup-progress", wrapped);
  },
});
