import type { ClientBrowserRequest, ClientBrowserState } from "./clientBrowser";
import type {
  ClientExtensionActionOutcome,
  ClientExtensionActionRequest,
  ClientExtensionCredentialCheck,
  ClientExtensionCredentialUpdate,
  ClientExtensionLifecycleOutcome,
  ClientExtensionLifecycleRequest,
  ClientExtensionMutation,
  ClientExtensionStoreSnapshot,
} from "./extensions/types";
import type { OctopusClientProjectionReadRequest } from "./generated/projectionReadContract";
import type {
  ClientWindowBootstrap,
  ClientWindowOpenOutcome,
  ClientWindowOpenRequest,
  ClientWindowReadyRequest,
  ClientPaneDragPayload,
  ClientPaneDragBeginRequest,
  ClientPaneDragState,
  ClientPaneDragOutcome,
  ClientPaneDropAnchorRequest,
  ClientPaneRect,
  ClientPaneTransferAcknowledgementRequest,
  ClientPaneTransferAccepted,
  ClientPaneTransferBeginRequest,
  ClientPaneTransferCompleted,
  ClientPaneTransferRequest,
} from "./shell/clientWindowModel";

export type OctopusClientContextMenuItem = { type: "separator" } | {
  id: string;
  label: string;
  type?: "normal" | "checkbox" | "radio";
  enabled?: boolean;
  checked?: boolean;
  accelerator?: string;
  submenu?: OctopusClientContextMenuItem[];
};
export type OctopusClientContextMenuRequest = {
  requestId: string;
  items: OctopusClientContextMenuItem[];
  position?: { x: number; y: number };
};
export type OctopusClientContextMenuResult = { requestId: string; id: string | null };

export {};

export type OctopusClientWindowCloseGuard = { windowId: string; generation: number };
export type OctopusClientWindowCloseRequest = OctopusClientWindowCloseGuard & { requestId: string; reason: "close" | "quit" | "logout" };
export type OctopusClientWindowCloseCancelled = OctopusClientWindowCloseGuard & { requestId: string; reason: "cancelled" | "invalidated" };
export type OctopusClientWindowCloseResolution = OctopusClientWindowCloseGuard & { requestId: string; allowClose: boolean };

declare global {
  interface Window {
    octopusTransport?: {
      showClientContextMenu(request: OctopusClientContextMenuRequest): Promise<OctopusClientContextMenuResult>;
      cancelClientContextMenu(request: { requestId: string }): Promise<{ cancelled: boolean }>;
      windowZoomFactor(): number;
      appInfo(): Promise<OctopusAppInfo>;
      openProblemReport(): Promise<void>;
      runClientDoctor(): Promise<OctopusClientDoctorReport>;
      copyText(value: string): Promise<{ copied: true }>;
      logClientEvent(request: OctopusClientLogRequest): Promise<{ written: true }>;
      openClientLogs(): Promise<{ path: string }>;
      selectClientDirectory(request?: OctopusClientPathSelectionRequest): Promise<OctopusClientDirectorySelection>;
      selectClientAttachments(request: OctopusClientAttachmentSelectionRequest): Promise<OctopusClientAttachmentSelection>;
      stageDroppedAttachments(files: readonly File[]): Promise<OctopusClientAttachmentSelection & { fileIndices: number[] }>;
      discardClientAttachmentStages(handles: readonly OctopusClientAttachmentStageHandle[]): Promise<void>;
      readClientAttachmentPreview(request: { id: string; contentHash: string }): Promise<{ base64: string; mimeType: string; contentHash: string; size: number }>;
      beginClientAttachmentStage(request: OctopusClientAttachmentStageBeginRequest): Promise<OctopusClientAttachmentStageHandle>;
      appendClientAttachmentStage(request: OctopusClientAttachmentStageAppendRequest): Promise<OctopusClientAttachmentStageProgress>;
      finalizeClientAttachmentStage(request: OctopusClientAttachmentStageFinalizeRequest): Promise<OctopusClientAttachmentStageHandle>;
      abortClientAttachmentStage(request: OctopusClientAttachmentStageAbortRequest): Promise<OctopusClientAttachmentStageAbortOutcome>;
      saveClientFile(request: OctopusClientFileRequest): Promise<OctopusClientFileSaveOutcome>;
      downloadClientFileCopy(request: OctopusClientFileRequest): Promise<OctopusClientFileSaveOutcome>;
      openClientFile(request: OctopusClientFileOpenRequest): Promise<OctopusClientFileOpenOutcome>;
      listClientFileOpenTargets(request: OctopusClientFileRequest): Promise<OctopusClientFileOpenTargets>;
      openClientFileWithApplication(request: OctopusClientFileOpenWithRequest): Promise<OctopusClientFileOpenOutcome>;
      startClientFileDrag(request: OctopusClientFileRequest): void;
      accessStatus(): Promise<OctopusClientAccessStatus>;
      configureAccess(password: string): Promise<OctopusClientAccessStatus>;
      unlockAccess(password: string): Promise<OctopusClientAccessStatus>;
      touchIdStatus(): Promise<OctopusTouchIdStatus>;
      setTouchIdEnabled(enabled: boolean): Promise<OctopusTouchIdStatus>;
      unlockWithTouchId(): Promise<OctopusClientAccessStatus>;
      lockAccess(): Promise<OctopusClientAccessStatus>;
      labAccess(): Promise<OctopusClientLabAccess>;
      updateLabAccess(password: string): Promise<OctopusClientLabAccess>;
      accessDetails(): Promise<OctopusClientAccessDetails>;
      updateAdministratorAccess(request: OctopusAdministratorAccessUpdate): Promise<OctopusClientAccessDetails>;
      requestMicrophoneAccess(): Promise<OctopusMicrophoneAccess>;
      requestCameraAccess(): Promise<OctopusCameraAccess>;
      cameraAccessStatus(): Promise<OctopusCameraAccess>;
      saveGestureTrace(text: string): Promise<OctopusGestureTraceSaved>;
      revealGestureTrace(path: string): Promise<{ revealed: boolean; path: string }>;
      clientWindowBootstrap(): ClientWindowBootstrap | null;
      openClientWindow(request?: ClientWindowOpenRequest): Promise<ClientWindowOpenOutcome>;
      clientWindowReady(request: ClientWindowReadyRequest): Promise<ClientWindowOpenOutcome>;
      registerClientWindowCloseGuard(): Promise<OctopusClientWindowCloseGuard>;
      unregisterClientWindowCloseGuard(request: OctopusClientWindowCloseGuard): Promise<void>;
      onClientWindowCloseRequested(listener: (request: OctopusClientWindowCloseRequest) => void): () => void;
      resolveClientWindowClose(request: OctopusClientWindowCloseResolution): Promise<{ accepted: boolean }>;
      onClientWindowCloseCancelled(listener: (request: OctopusClientWindowCloseCancelled) => void): () => void;
      beginClientPaneTransfer(request: ClientPaneTransferBeginRequest): ClientPaneDragPayload;
      beginClientPaneDrag(request: ClientPaneDragBeginRequest): Promise<{ phase: "prepared" | "cancelled" }>;
      moveClientPaneDrag(request: ClientPaneTransferRequest): Promise<{ phase: ClientPaneDragState["phase"]; destinationWindowId: string | null }>;
      finishClientPaneDrag(request: ClientPaneTransferRequest): Promise<ClientPaneDragOutcome>;
      cancelClientPaneDrag(request: ClientPaneTransferRequest): Promise<{ cancelled: true; transferId: string }>;
      reattachClientPane(request: ClientPaneTransferRequest): Promise<ClientPaneDragOutcome>;
      detachClientPane(request: ClientPaneTransferRequest): Promise<ClientPaneDragOutcome>;
      pendingClientPaneSourceFinalizations(): Promise<ClientPaneTransferCompleted[]>;
      acknowledgeClientPaneSourceFinalization(request: ClientPaneTransferRequest): Promise<{ acknowledged: true; transferId: string }>;
      registerClientPaneDropZone(request: ClientPaneRect | null): Promise<void>;
      anchorClientPaneDrop(request: ClientPaneDropAnchorRequest): Promise<{ anchored: boolean }>;
      onClientPaneDragState(listener: (state: ClientPaneDragState) => void): () => void;
      onClientPaneTransferAccepted(listener: (value: ClientPaneTransferAccepted) => void): () => void;
      onClientPaneTransferFinalized(listener: (value: ClientPaneTransferCompleted) => void): () => void;
      onClientPaneTransferCancelled(listener: (value: ClientPaneTransferCompleted) => void): () => void;
      acceptClientPaneTransfer(request: ClientPaneTransferRequest): Promise<ClientPaneTransferAccepted>;
      commitClientPaneTransfer(request: ClientPaneTransferRequest): Promise<ClientPaneTransferCompleted>;
      acknowledgeClientPaneTransfer(request: ClientPaneTransferAcknowledgementRequest): { released: boolean; transferId: string };
      rejectClientPaneTransfer(request: ClientPaneTransferRequest): Promise<{ rejected: true; transferId: string }>;
      cancelClientPaneTransfer(request: ClientPaneTransferRequest): Promise<{ cancelled: true; transferId: string }>;
      pendingClientPaneTransfers(): Promise<ClientPaneTransferCompleted[]>;
      pendingClientPaneDestinationTransfers(): Promise<(ClientPaneTransferCompleted & { committed: boolean })[]>;
      resumeClientPaneTransfer(request: ClientPaneTransferRequest): Promise<ClientPaneTransferCompleted>;
      finalizeClientPaneTransfer(request: ClientPaneTransferRequest): Promise<{ finalized: true; transferId: string }>;
      windowState(): Promise<OctopusWindowState>;
      setHistoryNavigationState(state: OctopusHistoryNavigationState): void;
      setWindowAppearance(appearance: OctopusWindowAppearance): void;
      setPowerSaveBlocker(enabled: boolean): Promise<{ enabled: boolean }>;
      setGlobalDictationShortcut(request: OctopusGlobalDictationShortcutRequest): Promise<OctopusGlobalDictationShortcutState>;
      setNativeCommandShortcuts(shortcuts: Record<string, readonly string[]>): Promise<{ configured: number }>;
      openQuickTaskWindow(): Promise<{ opened: true }>;
      closeQuickTaskWindow(): Promise<{ closed: true }>;
      openRunInMain(runId: string): Promise<{ opened: true; runId: string }>;
      openWorkerEnvironmentWindow(request: OctopusWorkerEnvironmentWindowTarget): Promise<OctopusWorkerEnvironmentWindowOutcome>;
      openWorkerEnvironmentInMain(request: OctopusWorkerEnvironmentWindowTarget): Promise<OctopusWorkerEnvironmentWindowOutcome>;
      setWorkerEnvironmentWindowPinned(enabled: boolean): Promise<{ pinned: boolean }>;
      openTaskMonitorWindow(runId: string): Promise<{ opened: true; runId: string }>;
      setTaskMonitorWindowPinned(enabled: boolean): Promise<{ pinned: boolean }>;
      loginItemSettings(request?: { enabled: boolean }): Promise<OctopusLoginItemSettings>;
      windowControl(action: "close" | "minimize" | "zoom"): void;
      relaunchClient(): Promise<{ relaunching: true }>;
      defaults(): Promise<OctopusTransportDefaults>;
      providerInterfaces(): Promise<OctopusProviderInterfacesResponse>;
      bootStatus(): Promise<OctopusRuntimeHostState>;
      chatgptAccount(): Promise<OctopusChatgptAccount>;
      chatgptRateLimits(): Promise<OctopusChatgptUsage>;
      chatgptLogout(): Promise<OctopusChatgptAccount>;
      onChatgptAccountChanged(listener: () => void): () => void;
      chatgptSignInStart(): Promise<{ loginId: string; authUrl: string }>;
      chatgptSignInStatus(loginId: string): Promise<{ state: "pending" | "completed" | "failed"; error: string }>;
      chatgptSignInCancel(loginId: string): Promise<{ cancelled: boolean }>;
      openLaunchTerminal(request: OctopusTerminalResizeRequest): Promise<OctopusTerminalSnapshot>;
      writeLaunchTerminal(sessionId: string, data: string): Promise<OctopusTerminalSnapshot>;
      resizeLaunchTerminal(sessionId: string, request: OctopusTerminalResizeRequest): Promise<OctopusTerminalSnapshot>;
      openTerminal(request: OctopusTerminalOpenRequest): Promise<OctopusTerminalSnapshot>;
      writeTerminal(terminalId: string, sessionId: string, data: string): Promise<OctopusTerminalSnapshot>;
      resizeTerminal(terminalId: string, sessionId: string, request: OctopusTerminalResizeRequest): Promise<OctopusTerminalSnapshot>;
      closeTerminal(terminalId: string): Promise<{ closed: boolean }>;
      runtimeStatus(request?: OctopusRuntimeStatusRequest): Promise<OctopusRuntimeStatus>;
      validateRuntimeEndpoint(request: OctopusRuntimeEndpointValidationRequest): Promise<OctopusRuntimeEndpointValidation>;
      projectionRead(request: OctopusProjectionReadRequest): Promise<unknown>;
      workspaceSetupState(): Promise<OctopusWorkspaceSetupState>;
      clientSetupRead(request: { operationId: string }): Promise<OctopusClientSetupState | null>;
      clientSetupReadCurrent(): Promise<OctopusClientSetupState | null>;
      clientSetupBegin(request: OctopusClientSetupBeginRequest): Promise<OctopusClientSetupState>;
      clientSetupResume(request: { operationId: string; stopBefore?: "provider" }): Promise<OctopusClientSetupState>;
      mutateScheduledInterface(request: OctopusScheduledMutationRequest): Promise<OctopusScheduledMutationOutcome>;
      mutateApplicationPrivateConfiguration(request: OctopusApplicationPrivateConfigurationMutation): Promise<OctopusApplicationPrivateConfigurationOutcome>;
      applicationConnectionProbe(request: { applicationId: string }): Promise<OctopusApplicationConnectionProbe>;
      applicationConnectionConnect(request: OctopusApplicationConnectionRequest): Promise<OctopusApplicationConnectionState>;
      applicationConnectionCancel(request: { operationId: string }): Promise<{ cancelled: boolean }>;
      applicationConnectionDisconnect(request: { applicationId: string; interfaceId: string }): Promise<OctopusApplicationConnectionState>;
      mutateOctopusOsWorkspace(request: OctopusOsWorkspaceMutationRequest): Promise<OctopusOsWorkspaceMutationOutcome>;
      clientBrowser(request: ClientBrowserRequest): Promise<ClientBrowserState | null>;
      onClientBrowserState(listener: (state: ClientBrowserState) => void): () => void;
      onClientBrowserOpened(listener: (state: ClientBrowserState) => void): () => void;
      openCdpViewer(request: OctopusCdpViewerOpenRequest): Promise<OctopusCdpViewerState>;
      prepareNativeBrowserViewer(request: OctopusNativeBrowserViewerPrepareRequest): Promise<OctopusNativeBrowserViewerPrepared>;
      acknowledgeNativeBrowserViewer(request: OctopusNativeBrowserViewerSyncRequest): Promise<OctopusNativeBrowserViewerState>;
      syncNativeBrowserViewer(request: OctopusNativeBrowserViewerSyncRequest): Promise<OctopusNativeBrowserViewerState>;
      nativeBrowserCursorArrived(request: OctopusNativeBrowserViewerIdentity & { moveSequence: number; metricsRevision: number }): Promise<{ accepted: boolean }>;
      nativeBrowserViewerAction(request: OctopusNativeBrowserViewerActionRequest): Promise<OctopusNativeBrowserViewerState>;
      nativeBrowserDownloads(request: OctopusBrowserDownloadRequest): Promise<OctopusBrowserDownloadSnapshot>;
      closeNativeBrowserViewer(request: OctopusNativeBrowserViewerIdentity): Promise<void>;
      closeNativeBrowserPage(request: { home: string; workerEnvironmentId: string; surfaceId: string }): Promise<void>;
      browserSavedPermissions(request: { operation: "list" } | { operation: "remove"; home: string; hostGeneration: string; id: string }): Promise<OctopusSavedBrowserPermissions>;
      desktopSavedApplicationPermissions(request: OctopusSavedApplicationPermissionRequest): Promise<OctopusSavedApplicationPermissions>;
      desktopComputerUseSettings(request: { operation: "status" } | { operation: "set_enabled"; enabled: boolean }): Promise<{ enabled: boolean; ready: boolean; permissions: Array<{ kind: "screen_capture" | "accessibility"; status: string }> }>;
      desktopComputerUseSettings(request: { operation: "setup" }): Promise<{ opened: true }>;
      openDesktopPermissions(request: import("./components/worker-environment/desktopSurfaceModel").DesktopPermissionRequest): Promise<{ opened: true }>;
      desktopSurfaceRequest(request: import("./components/worker-environment/desktopSurfaceModel").DesktopSurfaceRequest): Promise<import("./components/worker-environment/desktopSurfaceModel").DesktopSurfaceResponse>;
      desktopApplicationView(request: import("./components/worker-environment/desktopSurfaceModel").DesktopApplicationViewRequest): Promise<{ attached: boolean }>;
      cdpViewerAction(request: OctopusCdpViewerActionRequest): Promise<OctopusCdpViewerState | OctopusCdpViewerActionAccepted>;
      closeCdpViewer(request: OctopusCdpViewerCloseRequest): Promise<OctopusCdpViewerCloseOutcome>;
      openProcessViewer(request: OctopusProcessViewerOpenRequest): Promise<OctopusProcessViewerSnapshot>;
      processViewerAction(request: OctopusProcessViewerActionRequest): Promise<OctopusProcessViewerActionOutcome>;
      closeProcessViewer(request: OctopusProcessViewerCloseRequest): Promise<{ sessionId: string; closed: true }>;
      listWorkerEnvironmentFiles(request: OctopusWorkerEnvironmentFilesRequest): Promise<OctopusWorkerEnvironmentDirectory>;
      readWorkerEnvironmentFile(request: OctopusWorkerEnvironmentFilesRequest): Promise<OctopusWorkerEnvironmentFile>;
      readWorkerEnvironmentFilePreview(request: OctopusWorkerEnvironmentFilesRequest): Promise<OctopusWorkerEnvironmentFilePreview>;
      readWorkerEnvironmentFileAttachment(request: OctopusWorkerEnvironmentFilesRequest): Promise<OctopusWorkerEnvironmentFileAttachment>;
      writeWorkerEnvironmentFile(request: OctopusWorkerEnvironmentFileWriteRequest): Promise<OctopusWorkerEnvironmentFileWriteOutcome>;
      performWorkerEnvironmentFileSystemAction(request: OctopusWorkerEnvironmentFileSystemActionRequest): Promise<OctopusWorkerEnvironmentFileSystemActionOutcome>;
      osFileRoots(): Promise<OctopusOsFileRoots>;
      listOsFiles(request: OctopusOsFilesRequest): Promise<OctopusOsDirectory>;
      readOsFile(request: OctopusOsFilesRequest): Promise<OctopusOsFile>;
      writeOsFile(request: OctopusOsFileWriteRequest): Promise<OctopusOsFileWriteOutcome>;
      performOsFileSystemAction(request: OctopusOsFileSystemActionRequest): Promise<OctopusOsFileSystemActionOutcome>;
      inspectWorkerEnvironmentGit(request: OctopusWorkerEnvironmentGitRequest): Promise<OctopusWorkerEnvironmentGitSnapshot>;
      performWorkerEnvironmentOperatorAction(request: OctopusWorkerEnvironmentOperatorActionRequest): Promise<OctopusWorkerEnvironmentOperatorActionOutcome>;
      clientSurfaceDocuments(): Promise<OctopusClientSurfaceDocumentSnapshot>;
      clientExtensions(): Promise<ClientExtensionStoreSnapshot>;
      mutateClientExtension(request: ClientExtensionMutation): Promise<ClientExtensionStoreSnapshot>;
      setClientExtensionCredential(request: ClientExtensionCredentialUpdate): Promise<ClientExtensionStoreSnapshot>;
      checkClientExtensionCredential(request: { id: string; sourceId: string }): Promise<ClientExtensionCredentialCheck>;
      applyClientExtensionLifecycle(request: ClientExtensionLifecycleRequest): Promise<ClientExtensionLifecycleOutcome>;
      revealClientExtensions(): Promise<{ opened: true; path: string }>;
      readClientExtensionFilesystemSource(request: OctopusClientExtensionFilesystemReadRequest): Promise<OctopusClientExtensionFilesystemSource>;
      runClientExtensionAction(request: ClientExtensionActionRequest): Promise<ClientExtensionActionOutcome>;
      readCodexUsage(request: OctopusCodexUsageReadRequest): Promise<OctopusCodexUsageSnapshot>;
      refreshProviderModels(request: OctopusProviderModelsRefreshRequest): Promise<OctopusProviderModelsRefreshResponse>;
      requestComposerPrediction(request: { runId: string }): Promise<{ id: string; text: string | null; runId: string } | null>;
      requestTaskTitle(request: { runId: string }): Promise<{ title: string; runId: string } | null>;
      submitChatTurn(request: OctopusChatTurnRequest): Promise<OctopusChatTurnResponse>;
      readOperatorChatArtifacts(request: OctopusOperatorChatArtifactsRequest): Promise<OctopusOperatorChatArtifactsResponse>;
      submitOperatorTurnControl(request: OctopusOperatorTurnControlRequest): Promise<OctopusOperatorTurnControlResponse>;
      submitChatSteer(request: OctopusChatSteerRequest): Promise<OctopusChatSteerResponse>;
      updateChatPins(request: OctopusChatPinsRequest): Promise<OctopusChatPinsResponse>;
      transcribeDictation(request: OctopusDictationRequest): Promise<OctopusDictationResponse>;
      startDictationStream(request: { attemptId: string; sampleRateHz: number }): Promise<OctopusDictationLocalResult<null>>;
      appendDictationAudio(attemptId: string, bytes: ArrayBuffer): void;
      finishDictationStream(attemptId: string): Promise<OctopusDictationResponse>;
      onDictationStreamEvent(listener: (event: OctopusDictationStreamEvent) => void): () => void;
      cancelDictation(attemptId: string): Promise<void>;
      readDictationRecordings(): Promise<OctopusDictationLocalResult<OctopusDictationRecordingState>>;
      configureDictationRecordings(enabled: boolean): Promise<OctopusDictationLocalResult<OctopusDictationRecordingState>>;
      saveDictationRecording(request: OctopusDictationRequest): Promise<OctopusDictationLocalResult<OctopusDictationRecording | null>>;
      readDictationRecording(id: string): Promise<OctopusDictationLocalResult<{ entry: OctopusDictationRecording; bytes: Uint8Array }>>;
      updateDictationRecording(request: { id: string; text: string; status: "transcribed" | "failed" }): Promise<OctopusDictationLocalResult<OctopusDictationRecording>>;
      deleteDictationRecording(id: string | null): Promise<OctopusDictationLocalResult<null>>;
      submitInterfaceArtifact(request: OctopusInterfaceArtifactRequest): Promise<OctopusInterfaceArtifactResponse>;
      beginInterfaceArtifactStream(request: OctopusArtifactStreamBeginRequest): Promise<OctopusArtifactStreamHandle>;
      appendInterfaceArtifactStream(request: OctopusArtifactStreamAppendRequest): Promise<OctopusArtifactStreamProgress>;
      finalizeInterfaceArtifactStream(request: OctopusArtifactStreamFinalizeRequest): Promise<OctopusArtifactStreamOutcome>;
      abortInterfaceArtifactStream(request: OctopusArtifactStreamAbortRequest): Promise<OctopusArtifactStreamAbortOutcome>;
      onProjectionChanged(listener: (change: OctopusProjectionChange) => void): () => void;
      onRuntimeHostState(listener: (state: OctopusRuntimeHostState) => void): () => void;
      onTerminalData(listener: (event: OctopusTerminalData) => void): () => void;
      onTerminalExit(listener: (event: OctopusTerminalExit) => void): () => void;
      onCdpViewerEvent(listener: (event: OctopusCdpViewerEvent) => void): () => void;
      onNativeBrowserViewerEvent(listener: (event: OctopusNativeBrowserViewerEvent) => void): () => void;
      onProcessViewerEvent(listener: (event: OctopusProcessViewerEvent) => void): () => void;
      onWindowStateChanged(listener: (state: OctopusWindowState) => void): () => void;
      onNavigateBack(listener: () => void): () => void;
      onNavigateForward(listener: () => void): () => void;
      onGlobalDictationRequested(listener: () => void): () => void;
      onNativeClientCommand(listener: (command: string) => void): () => void;
      onClientAccessLocked(listener: (status: OctopusClientAccessStatus) => void): () => void;
      onClientPaneTransferCompleted(listener: (completion: ClientPaneTransferCompleted) => void): () => void;
      onOpenRunRequested(listener: (runId: string) => void): () => void;
      onOpenWorkerEnvironmentRequested(listener: (target: OctopusWorkerEnvironmentWindowTarget) => void): () => void;
      onWorkerEnvironmentWindowTarget(listener: (target: OctopusWorkerEnvironmentWindowTarget) => void): () => void;
      onClientSurfaceDocumentsChanged(listener: (change: OctopusClientSurfaceDocumentChange) => void): () => void;
      onClientExtensionsChanged(listener: (change: OctopusClientExtensionChange) => void): () => void;
      onApplicationLifecycleProgress(listener: (progress: OctopusApplicationLifecycleProgress) => void): () => void;
      onApplicationConnectionProgress(listener: (progress: OctopusApplicationConnectionProgress) => void): () => void;
      onWorkspaceSetupProgress(listener: (progress: OctopusWorkspaceSetupProgress) => void): () => void;
      onClientSetupProgress(listener: (progress: OctopusClientSetupProgress) => void): () => void;
    };
  }
}

export type OctopusProjectionChange = {
  generation: number;
  kind: "ledger" | "runs" | "files" | "structure";
};

export type OctopusClientAttachmentSelectionRequest = {
  mode: "directory" | "files" | "files_and_folders";
};

export type OctopusClientDirectorySelection = {
  canceled: boolean;
  path: string;
  paths: string[];
};

export type OctopusClientPathSelectionRequest = {
  mode: "file" | "directory";
  allowMultiple?: boolean;
  title?: string;
  buttonLabel?: string;
};

export type OctopusClientExtensionChange = {
  generation: number;
};

export type OctopusClientExtensionFilesystemReadRequest = {
  path: string;
  read: "text" | "json" | "directory";
};

export type OctopusClientExtensionDirectoryEntry = {
  kind: "file" | "directory";
  name: string;
  path: string;
  size?: number;
  modifiedAt: string;
};

export type OctopusClientExtensionFilesystemSource =
  | {
      kind: "directory";
      path: string;
      name: string;
      modifiedAt: string;
      entries: OctopusClientExtensionDirectoryEntry[];
      truncated: boolean;
      files: number;
      directories: number;
    }
  | {
      kind: "text";
      path: string;
      name: string;
      size: number;
      modifiedAt: string;
      digest: string;
      text: string;
    }
  | {
      kind: "json";
      path: string;
      name: string;
      size: number;
      modifiedAt: string;
      digest: string;
      value: unknown;
    };

export type OctopusCodexUsageWindowDays = 7 | 30 | 90;

export type OctopusCodexUsageReadRequest = {
  windowDays: OctopusCodexUsageWindowDays;
  timeZone: string;
};

export type OctopusCodexUsageBucket = {
  day: string;
  model: string;
  uncachedInputTokens: number;
  cachedInputTokens: number;
  cacheCreationTokens: number;
  outputTokens: number;
  reasoningTokens: number;
  totalTokens: number;
  costUsd: number;
  cacheSavingsUsd: number;
  priced: boolean;
  records: number;
  sessions: number;
};

export type OctopusCodexUsageSnapshot = {
  schema: "octopus.client_usage";
  schemaVersion: 1;
  provider: "codex";
  readAt: string;
  timeZone: string;
  sinceDay: string;
  untilDay: string;
  buckets: OctopusCodexUsageBucket[];
  source: {
    kind: "codex_session_transcripts";
    resolvedHomePath: string;
    path: string;
    status: "ok" | "missing" | "partial" | "failed";
    scannedFiles: number;
    cachedFiles: number;
    skippedFiles: number;
    failedFiles: number;
    inaccessibleDirectories: number;
    distinctSessions: number;
    malformedRecords: number;
    unmodeledRecords: number;
    message: string | null;
  };
  pricing: {
    status: "fresh" | "cached" | "unavailable";
    source: string;
    fetchedAt: string | null;
    knownModels: number;
  };
  scanDurationMs: number;
};

export type OctopusClientAttachmentSelection = {
  canceled: boolean;
  attachments: OctopusClientAttachmentStageHandle[];
};

export type OctopusClientAttachmentStageBeginRequest = {
  name: string;
  label: string;
  type: string;
  size: number;
  lastModified: number;
};

export type OctopusClientAttachmentStageHandle = OctopusClientAttachmentStageBeginRequest & {
  id: string;
  contentHash?: string;
};

export type OctopusClientAttachmentStageAppendRequest = {
  id: string;
  bytes: ArrayBuffer;
};

export type OctopusClientAttachmentStageProgress = {
  id: string;
  bytes: number;
};

export type OctopusClientAttachmentStageFinalizeRequest = {
  id: string;
};

export type OctopusClientAttachmentStageAbortRequest = {
  id: string;
};

export type OctopusClientAttachmentStageAbortOutcome = {
  id: string;
  aborted: true;
};

export type OctopusClientFileRequest = {
  bytes: ArrayBuffer | Uint8Array;
  filename: string;
};

export type OctopusClientFileOpenRequest = OctopusClientFileRequest & {
  action: "open" | "reveal";
};

export type OctopusClientFileOpenWithRequest = OctopusClientFileRequest & {
  applicationId: string;
};

export type OctopusClientFileOpenTarget = {
  icon: string | null;
  id: string;
  label: string;
};

export type OctopusClientFileOpenTargets = {
  applications: OctopusClientFileOpenTarget[];
  downloadsFolderIcon: string | null;
  fileManagerIcon: string | null;
  preferredApplicationId: string | null;
};

export type OctopusClientFileSaveOutcome = {
  saved: boolean;
  path?: string;
  filename?: string;
};

export type OctopusClientFileOpenOutcome = {
  opened: true;
  path: string;
};

export type OctopusWorkerEnvironmentPanelKind =
  | "browser"
  | "connection"
  | "container"
  | "runtime"
  | "worktree"
  | "files"
  | "terminal"
  | "changes"
  | "desktop"
  | "custom"
  | "overview";

export type OctopusWorkerEnvironmentWindowTarget = {
  runId: string;
  workerEnvironmentId: string;
  panelKind: OctopusWorkerEnvironmentPanelKind;
  surfaceId?: string;
  filePath?: string;
  clientSurface?: {
    documentId: string;
    revision: number;
    sourceDigest: string;
  };
};

export type OctopusWorkerEnvironmentWindowOutcome = OctopusWorkerEnvironmentWindowTarget & {
  opened: true;
};

export type OctopusClientLogRequest = {
  level: "info" | "warning" | "error";
  event: string;
  message: string;
};

export type OctopusGlobalDictationShortcutRequest = {
  enabled: boolean;
  shortcut: string;
};

export type OctopusGlobalDictationShortcutState = {
  enabled: boolean;
  shortcut: string;
  registered: boolean;
};

export type OctopusLoginItemSettings = {
  supported: boolean;
  enabled: boolean;
};

export type OctopusCdpViewerOpenRequest = {
  attachmentId: string;
  visible: boolean;
  workerEnvironmentId: string;
  surfaceId: string;
};

export type OctopusNativeBrowserViewerIdentity = {
  attachmentId: string;
  hostGeneration: string;
  mountGeneration: number;
};

export type OctopusNativeBrowserViewerPrepareRequest = {
  attachmentId: string;
  workerEnvironmentId: string;
  surfaceId: string;
  frameToken: string;
};

export type OctopusNativeBrowserViewerPrepared = {
  adoptionToken: string | null;
  hostGeneration: string;
  mountGeneration: number;
  state: OctopusNativeBrowserViewerState;
};

export type OctopusNativeBrowserViewerSyncRequest = OctopusNativeBrowserViewerIdentity & {
  viewportSequence: number;
  metricsRevision: number;
  stageBounds: { x: number; y: number; width: number; height: number };
  pageBounds: { x: number; y: number; width: number; height: number };
  visualBounds: { x: number; y: number; width: number; height: number };
  scale: number;
  visible: boolean;
};

export type OctopusNativeBrowserViewerActionRequest = OctopusNativeBrowserViewerIdentity & (
  | { action: "reload" | "back" | "forward" | "stop" | "focus" }
  | { action: "navigate"; params: { url: string } }
  | { action: "zoom"; params: { zoomFactor: number } }
  | { action: "find"; params: { query: string; backwards: boolean; matchCase: boolean } }
);

export type OctopusSavedBrowserPermissions = {
  home: string;
  hostGeneration: string;
  removed?: boolean;
  entries: Array<{ id: string; decision: "accept" | "decline"; subject:
    { type: "upload"; origin: string } | { type: "history"; source: { id: string; content: number[] }; source_payload_digest: string } }>;
};
export type OctopusSavedApplicationPermissionRequest = { operation: "list" } | { operation: "remove"; sourceId: string; bundleId: string };
export type OctopusSavedApplicationPermissions = {
  sourceId: string; removed: boolean;
  applications: Array<{ bundle_id: string; display_name: string }>;
};

export type OctopusBrowserDownloadRequest = {
  workerEnvironmentId: string;
  surfaceId: string;
  hostGeneration: string;
  action: "snapshot" | "search" | "clear-history" | "acknowledge" | "open-folder" | "open" | "show-in-folder" | "copy-address" | "copy-path" | "pause" | "resume" | "cancel" | "remove" | "choose-directory" | "reset-directory" | "set-prompt";
  promptForLocation?: boolean;
  id?: string;
  query?: { text: string; offset: number };
};
export type OctopusBrowserDownload = {
  id: string;
  targetId: string;
  filename: string;
  url: string;
  status: "started" | "in_progress" | "paused" | "complete" | "failed" | "canceled" | "unavailable";
  receivedBytes: number;
  totalBytes: number;
  startedAtMs: number;
  updatedAtMs: number;
  fileExists: boolean;
  canCancel: boolean;
  canPause: boolean;
  canResume: boolean;
  userInitiated: boolean;
  artifactPending?: boolean;
  artifactError?: string | null;
  sequence: number;
};
export type OctopusBrowserDownloadHistory = { text: string; offset: number; hasMore: boolean; downloads: OctopusBrowserDownload[] };
export type OctopusBrowserDownloadSnapshot = {
  history?: OctopusBrowserDownloadHistory;
  preferences: { directory: string | null; promptForLocation: boolean };
  sequence: number;
  downloads: OctopusBrowserDownload[];
  unacknowledgedIds: string[];
  error: string | null;
};

export type OctopusNativeBrowserViewerState = OctopusNativeBrowserViewerIdentity & {
  surfaceId: string;
  protocol: "browser.native.v1";
  presentationProtocol: "electron.ownedGuest.v1" | "electron.webContentsView.v1";
  engine: {
    mode: "standard" | "owned";
    presentationProtocol: OctopusNativeBrowserViewerState["presentationProtocol"];
    capabilities: {
      nativeView: boolean;
      ownedGuestEmbedding: boolean;
      scaledPresentation: boolean;
      modelViewport: boolean;
      hiddenPageCapture: boolean;
      navigationHistory: boolean;
      nativeNavigationHistory: boolean;
      persistentDownloadHistory: boolean;
      cursorOverlay: boolean;
    };
    limits: string[];
  };
  presentationPhase: "prepared" | "attached" | "ready";
  viewportSequence: number;
  modelViewport: { revision: number; emulatedSize: null | { width: number; height: number } };
  modelActive: boolean;
  modelTurnKey: string;
  cursor: null | { moveSequence: number; x: number; y: number; metricsRevision: number; sourceMovementSeq: number };
  engineHandle: string;
  targetId: string;
  browserContextId: string;
  workerEnvironmentId: string;
  run: string;
  ee: string;
  connected: boolean;
  visible: boolean;
  readOnly: boolean;
  loading: boolean;
  url: string;
  title: string;
  canGoBack: boolean;
  canGoForward: boolean;
  zoomFactor: number;
  findFound?: boolean;
  findMatches?: number;
  findActiveOrdinal?: number;
};

export type OctopusNativeBrowserViewerEvent = OctopusNativeBrowserViewerIdentity & (
  | { type: "state"; state: OctopusNativeBrowserViewerState }
  | { type: "command"; command: "focus-address" | "find" }
  | { type: "closed" | "error"; error: { code: string; message: string } }
);

export type OctopusCdpViewerState = {
  runId: string;
  targetId: string;
  visible: boolean;
  readOnly: boolean;
  manualControl: boolean;
  manualControllerPresent: boolean;
  navigationError: string;
  sessionId: string;
  workerEnvironmentId: string;
  surfaceId: string;
  connected: boolean;
  loading: boolean;
  url: string;
  title: string;
  canGoBack: boolean;
  canGoForward: boolean;
  zoom: number;
};

export type OctopusCdpViewerMouseAction = {
  sessionId: string;
  action: "mouse";
  eventType: "mousePressed" | "mouseReleased" | "mouseMoved" | "mouseWheel";
  x: number;
  y: number;
  button?: "none" | "left" | "middle" | "right" | "back" | "forward";
  buttons?: number;
  clickCount?: number;
  deltaX?: number;
  deltaY?: number;
  modifiers?: number;
};

export type OctopusCdpViewerKeyAction = {
  sessionId: string;
  action: "key";
  eventType: "keyDown" | "keyUp" | "rawKeyDown" | "char";
  key?: string;
  code?: string;
  text?: string;
  unmodifiedText?: string;
  windowsVirtualKeyCode?: number;
  nativeVirtualKeyCode?: number;
  modifiers?: number;
  autoRepeat?: boolean;
};

export type OctopusCdpViewerActionRequest =
  | { sessionId: string; action: "setVisible"; visible: boolean }
  | { sessionId: string; action: "claimControl" | "releaseControl" | "stop" }
  | { sessionId: string; action: "navigate"; url: string }
  | { sessionId: string; action: "reload" }
  | { sessionId: string; action: "find"; query: string; backwards?: boolean; matchCase?: boolean }
  | { sessionId: string; action: "zoom"; factor: number }
  | { sessionId: string; action: "back" | "forward" | "refreshState" }
  | OctopusCdpViewerMouseAction
  | OctopusCdpViewerKeyAction;

export type OctopusCdpViewerActionAccepted = {
  sessionId: string;
  accepted: true;
  found?: boolean;
};

export type OctopusCdpViewerCloseRequest = {
  sessionId: string;
};

export type OctopusCdpViewerCloseOutcome = {
  sessionId: string;
  closed: true;
};

export type OctopusCdpViewerFrameMetadata = {
  deviceWidth: number;
  deviceHeight: number;
  pageScaleFactor: number;
  offsetTop: number;
  scrollOffsetX: number;
  scrollOffsetY: number;
  timestamp: number;
};

export type OctopusCdpViewerEvent =
  | { sessionId: string; type: "state"; state: OctopusCdpViewerState }
  | { sessionId: string; type: "error"; error: string }
  | { sessionId: string; type: "frame"; data: string; mimeType: "image/jpeg"; metadata: OctopusCdpViewerFrameMetadata };

export type OctopusProcessViewerOpenRequest = {
  workerEnvironmentId: string;
  surfaceId: string;
};

export type OctopusProcessViewerState = {
  workerEnvironmentId: string;
  generation: number;
  processId: string;
  status: string;
  tty: boolean;
  exitCode: number | null;
  closed: boolean;
  failure: string;
  restartAvailable: boolean;
  startNewShellAvailable: boolean;
  launchLabel: string;
};

export type OctopusProcessViewerSnapshot = {
  sessionId: string;
  state: OctopusProcessViewerState;
};

export type OctopusProcessViewerActionRequest =
  | { sessionId: string; action: "write"; data: string }
  | { sessionId: string; action: "resize"; cols: number; rows: number }
  | { sessionId: string; action: "interrupt" | "terminate" | "restart" | "start_new_shell" };

export type OctopusProcessViewerActionOutcome = {
  state: OctopusProcessViewerState;
};

export type OctopusProcessViewerCloseRequest = {
  sessionId: string;
};

export type OctopusProcessViewerEvent = {
  sessionId: string;
  event:
    | { type: "output"; data: string; seq: number }
    | { type: "history_truncated"; retainedFromSeq: number | null }
    | { type: "reset" }
    | { type: "state"; state: OctopusProcessViewerState }
    | { type: "failure"; message: string };
};

export type OctopusWorkerEnvironmentFilesRequest = {
  workerEnvironmentId: string;
  surfaceId: string;
  path: string;
};

export type OctopusWorkerEnvironmentFileEntry = {
  name: string;
  path: string;
  kind: "directory" | "file" | "symlink" | "other";
  size: number | null;
  modifiedAt: string;
};

export type OctopusWorkerEnvironmentDirectory = {
  workerEnvironmentId: string;
  surfaceId: string;
  path: string;
  entries: OctopusWorkerEnvironmentFileEntry[];
};

export type OctopusWorkerEnvironmentFile = {
  workerEnvironmentId: string;
  surfaceId: string;
  path: string;
  name: string;
  size: number;
  modifiedAt: string;
  kind: "text" | "binary";
  text: string;
  truncated: boolean;
  digest: string;
};

export type OctopusWorkerEnvironmentFileWriteRequest = OctopusWorkerEnvironmentFilesRequest & {
  text: string;
  expectedDigest: string;
  expectedModifiedAt: string;
  expectedSize: number;
};

export type OctopusWorkerEnvironmentFileWriteOutcome = OctopusWorkerEnvironmentFile & {
  outsideOctopusEvidence: true;
};

export type OctopusWorkerEnvironmentFilePreview = {
  workerEnvironmentId: string;
  surfaceId: string;
  path: string;
  name: string;
  size: number;
  modifiedAt: string;
  mimeType: "application/pdf" | "image/avif" | "image/bmp" | "image/gif" | "image/jpeg" | "image/png" | "image/webp";
  bytes: ArrayBuffer;
};

export type OctopusWorkerEnvironmentFileAttachment = {
  workerEnvironmentId: string;
  surfaceId: string;
  path: string;
  name: string;
  size: number;
  modifiedAt: string;
  mimeType: string;
  bytes: ArrayBuffer;
};

export type OctopusWorkerEnvironmentFileSystemAction = "resolve" | "reveal" | "open";

export type OctopusWorkerEnvironmentFileSystemActionRequest = OctopusWorkerEnvironmentFilesRequest & {
  action: OctopusWorkerEnvironmentFileSystemAction;
};

export type OctopusWorkerEnvironmentFileSystemActionOutcome = {
  workerEnvironmentId: string;
  surfaceId: string;
  path: string;
  absolutePath: string;
  kind: "directory" | "file";
  action: OctopusWorkerEnvironmentFileSystemAction;
};

export type OctopusOsFileRoot = {
  id: string;
  label: string;
  path: string;
};

export type OctopusOsFileRoots = {
  roots: OctopusOsFileRoot[];
};

export type OctopusOsFilesRequest = {
  rootId: string;
  path?: string;
};

export type OctopusOsFileEntry = {
  name: string;
  path: string;
  kind: "directory" | "file" | "symlink" | "other";
  size: number | null;
  modifiedAt: string;
};

export type OctopusOsDirectory = {
  rootId: string;
  path: string;
  entries: OctopusOsFileEntry[];
};

export type OctopusOsFile = {
  rootId: string;
  path: string;
  name: string;
  size: number;
  modifiedAt: string;
  kind: "text" | "binary";
  text: string;
  truncated: boolean;
  digest: string;
};

export type OctopusOsFileWriteRequest = OctopusOsFilesRequest & {
  text: string;
  expectedDigest: string;
  expectedModifiedAt: string;
  expectedSize: number;
};

export type OctopusOsFileWriteOutcome = OctopusOsFile & {
  outsideOctopusEvidence: true;
};

export type OctopusOsFileSystemAction = "resolve" | "reveal" | "open";

export type OctopusOsFileSystemActionRequest = OctopusOsFilesRequest & {
  action: OctopusOsFileSystemAction;
};

export type OctopusOsFileSystemActionOutcome = {
  rootId: string;
  path: string;
  absolutePath: string;
  kind: "directory" | "file";
  action: OctopusOsFileSystemAction;
};

export type OctopusWorkerEnvironmentOperatorAction = "apply" | "revert" | "stage" | "unstage";

export type OctopusWorkerEnvironmentGitRequest = {
  workerEnvironmentId: string;
  surfaceId: string;
  runId: string;
};

export type OctopusWorkerEnvironmentGitSnapshot = {
  workerEnvironmentId: string;
  surfaceId: string;
  runId: string;
  projectionRevision: string;
  revision: string;
  branch: string;
  base: string;
  branchDiffs: Array<OctopusWorkerEnvironmentGitDiff>;
  branchError: string;
  omittedPaths: Array<{ path: string; reason: "nested-worktree" }>;
  stagedFiles: string[];
  unstagedFiles: string[];
  diffs: Array<OctopusWorkerEnvironmentGitDiff>;
};

export type OctopusWorkerEnvironmentGitDiff = {
  path: string;
  previous_path: string;
  before: string;
  after: string;
  binary: boolean;
  preview_omitted: boolean;
  stage: "branch" | "staged" | "unstaged";
  staged: boolean;
  unstaged: boolean;
  status: "added" | "deleted" | "modified" | "renamed";
};

export type OctopusWorkerEnvironmentOperatorActionRequest = {
  workerEnvironmentId: string;
  surfaceId: string;
  runId: string;
  projectionRevision: string;
  action: OctopusWorkerEnvironmentOperatorAction;
  paths: string[];
};

export type OctopusWorkerEnvironmentOperatorActionOutcome = {
  workerEnvironmentId: string;
  runId: string;
  surfaceId: string;
  projectionRevision: string;
  action: OctopusWorkerEnvironmentOperatorAction;
  paths: string[];
  outsideOctopusEvidence: true;
};

export type OctopusClientSurfaceDocumentSource = {
  path: string;
  digest: string;
  bytes: number;
  modifiedAt: string;
  text: string;
};

export type OctopusClientSurfaceDocumentSnapshot = {
  generation: number;
  root: string;
  documents: OctopusClientSurfaceDocumentSource[];
  errors: string[];
};

export type OctopusClientSurfaceDocumentChange = {
  generation: number;
};

export type OctopusInterfaceArtifactRequest = {
  interfaceId: string;
  runId?: string;
  label: string;
  payload: Record<string, unknown>;
  timeoutMs?: number;
};

export type OctopusDictationRequest = {
  attemptId: string;
  bytes: ArrayBuffer;
  durationMs: number;
  mediaType: "audio/wav";
  provider: "codex";
  sampleRateHz: 24000;
  timeoutMs?: number;
};

export type OctopusDictationResponse = OctopusDictationLocalResult<{
  provider: "codex";
  text: string;
  model: string;
}>;

export type OctopusDictationStreamEvent = { attemptId: string } & (
  { type: "transcript"; text: string } | { type: "stop-requested" } | { type: "error"; error: { code: string; message: string } }
);

export type OctopusDictationLocalResult<T> = { ok: true; value: T } | { ok: false; error: { code: string; message: string } };

export type OctopusDictationRecording = {
  version: 1;
  id: string;
  createdAtMs: number;
  durationMs: number;
  byteLength: number;
  status: "recorded" | "transcribed" | "failed";
  text: string;
};

export type OctopusDictationRecordingState = {
  version: 1;
  enabled: boolean;
  recordings: OctopusDictationRecording[];
  maxRecordings: number;
};

export type OctopusInterfaceArtifactResponse = {
  interfaceId: string;
  runId?: string;
  label: string;
  contentHash: string;
  verdict: unknown;
};

export type OctopusArtifactStreamBeginRequest = {
  interfaceId: string;
  runId?: string;
  label: string;
  mediaType: string;
  extension: string;
  payload?: Record<string, unknown>;
};

export type OctopusArtifactStreamHandle = {
  streamId: string;
  interfaceId: string;
  runId?: string;
  label: string;
  mediaType: string;
};

export type OctopusArtifactStreamAppendRequest = {
  streamId: string;
  bytes: Uint8Array;
};

export type OctopusArtifactStreamProgress = {
  streamId: string;
  bytes: number;
};

export type OctopusArtifactStreamFinalizeRequest = {
  streamId: string;
  timeoutMs?: number;
};

export type OctopusArtifactStreamOutcome = OctopusInterfaceArtifactResponse & {
  mediaType: string;
  mediaContentHash: string;
  mediaBytes: number;
};

export type OctopusArtifactStreamAbortRequest = {
  streamId: string;
};

export type OctopusArtifactStreamAbortOutcome = {
  streamId: string;
  aborted: true;
};

export type OctopusWindowState = {
  customWindowControls: boolean;
  nativeControlInset: number;
  fullScreen: boolean;
  maximized: boolean;
  opaqueSurface: boolean;
  resizing: boolean;
};

export type OctopusHistoryNavigationState = {
  canNavigateBack: boolean;
  canNavigateForward: boolean;
};

export type OctopusWindowAppearance = {
  theme: "light" | "dark";
  preference: "system" | "light" | "dark";
  translucent: boolean;
};

export type OctopusAppInfo = {
  name: string;
  version: string;
  platform: NodeJS.Platform;
  isPackaged: boolean;
};

export type OctopusClientDoctorFinding = {
  code: string;
  subject: string;
  reason: string;
  next_step: string;
  evidence: Record<string, unknown>;
};

export type OctopusClientDoctorReport = {
  schema_version: 2;
  checks: number;
  passed: number;
  findings: OctopusClientDoctorFinding[];
  errors: string[];
};

export type OctopusClientAccessProfile = "live" | "lab";

export type OctopusClientAccessStatus = {
  configured: boolean;
  labAvailable: boolean;
  authenticated: boolean;
  profile: OctopusClientAccessProfile | null;
  expiresAt: string | null;
};

export type OctopusTouchIdStatus = {
  available: boolean;
  enrolled: boolean;
  prompted: boolean;
};

export type OctopusClientLabAccess = {
  source: "default" | "environment" | "administrator";
  minimumLength: number;
};

export type OctopusClientAccessDetails = {
  profile: OctopusClientAccessProfile;
  minimumLength: number;
  administrator: {
    configured: boolean;
    source: "environment" | "credential";
    createdAt: string | null;
    updatedAt: string | null;
  };
  test: {
    available: boolean;
    source: "default" | "environment" | "administrator" | null;
  };
};

export type OctopusAdministratorAccessUpdate = {
  currentPassword: string;
  password: string;
};

export type OctopusMicrophoneAccess = {
  granted: boolean;
  status: string;
};

export type OctopusCameraAccess = {
  granted: boolean;
  status: string;
};

export type OctopusGestureTraceSaved = {
  path: string;
  filename: string;
  bytes: number;
};

export type OctopusTransportDefaults = {
  endpoint: string;
  repoRoot?: string | null;
  octopusHome?: string | null;
  registryRoot?: string | null;
  stateRoot?: string | null;
  ledgerPath?: string | null;
};

export type OctopusRuntimeStatusRequest = {
  endpoint?: string;
};

export type OctopusRuntimeStatus = {
  up: boolean;
  endpoint: string;
  error?: string;
  phase?: string;
  ownership?: "none" | "external" | "managed";
  pid?: number | null;
  home?: string;
  sourceRegistry?: string;
  runtimeBinary?: string;
  hostError?: string | null;
};

export type OctopusHomeStorageHealth = {
  status: "writable" | "blocked" | "unavailable";
  availableBytes: number | null;
  minimumBytes: number;
  code: string | null;
  message: string | null;
};

export type OctopusInterfaceFailure = { interfaceId: string; message: string };

export type OctopusRuntimeHostState = {
  phase: string;
  ready: boolean;
  ownership: "none" | "external";
  pid: number | null;
  home: string;
  sourceRegistry: string;
  runtimeBinary: string;
  repoRoot: string;
  launchCwd: string;
  launchCommand: string;
  launchEnv: Record<string, string>;
  endpoint: string;
  error: string | null;
  storageHealth: OctopusHomeStorageHealth | null;
  interfaceFailures: OctopusInterfaceFailure[];
  updatedAtMs: number;
};

export type OctopusTerminalResizeRequest = {
  cols?: number;
  rows?: number;
};

export type OctopusTerminalOpenRequest = OctopusTerminalResizeRequest & {
  terminalId: string;
  cwd?: string;
  env?: Record<string, string>;
  preparedCommand?: string;
};

export type OctopusTerminalSnapshot = {
  terminalId: string;
  sessionId: string;
  command: string;
  cwd: string;
  transcript: string;
  running: boolean;
  pid: number | null;
  exitCode: number | null;
  signal: number | null;
  error: string | null;
};

export type OctopusTerminalData = {
  terminalId: string;
  sessionId: string;
  data: string;
};

export type OctopusTerminalExit = {
  terminalId: string;
  sessionId: string;
  exitCode: number | null;
  signal: number | null;
  error: string | null;
};

export type OctopusRuntimeEndpointValidationRequest = {
  endpoint: string;
};

export type OctopusRuntimeEndpointValidation = {
  valid: boolean;
  endpoint: string;
  error?: string;
};

export type OctopusChatgptAccount = {
  signedIn: boolean;
  authType: "chatgpt" | "apiKey" | "amazonBedrock" | null;
  email: string | null;
  planType: string | null;
  credentialHome: string;
  octopusHome: string | null;
};

export type OctopusChatgptLimit = {
  limitId: string | null;
  limitName: string | null;
  primary: OctopusProviderRateLimitWindow | null;
  secondary: OctopusProviderRateLimitWindow | null;
  credits: { hasCredits: boolean; unlimited: boolean; balance: string | null } | null;
  individualLimit: { limit: string; used: string; remainingPercent: number; resetsAt: number } | null;
  planType: string | null;
  rateLimitReachedType: string | null;
  spendControlReached: boolean | null;
};

export type OctopusChatgptUsage = {
  rateLimits: OctopusChatgptLimit;
  rateLimitsByLimitId: Record<string, OctopusChatgptLimit> | null;
  observedAtMs: number;
};

export type OctopusProviderInterface = {
  id: string;
  label: string;
  descriptorPath: string;
  runtimeHandlerRef?: string;
  sourceNames: string[];
  model?: string;
  reasoningEffort?: string;
  serviceTier?: string;
  supportedReasoningEfforts: string[];
  models: OctopusProviderModel[];
  modelCatalogError?: string;
  preferred?: boolean;
  rateLimits?: OctopusProviderRateLimits;
};

export type OctopusProviderModel = {
  model: string;
  displayName: string;
  description: string;
  defaultReasoningEffort: string;
  supportedReasoningEfforts: string[];
  showInPicker: boolean;
  isDefault: boolean;
  serviceTiers?: OctopusModelServiceTier[];
  defaultServiceTier?: string | null;
};

export type OctopusModelServiceTier = {
  id: string;
  name: string;
  description: string;
};

export type OctopusProviderModelsRefreshRequest = {
  providerInterfaceId: string;
};

export type OctopusProviderModelsRefreshResponse = {
  providerInterfaceId: string;
  models: OctopusProviderModel[];
  observedAtUnixMs: number;
};

export type OctopusProviderRateLimitWindow = {
  usedPercent: number;
  windowDurationMins: number | null;
  resetsAt: number | null;
};

export type OctopusProviderRateLimits = {
  observedAtMs: number;
  movementSeq: number;
  runId: string;
  limitId: string;
  limitName: string;
  primary: OctopusProviderRateLimitWindow | null;
  secondary: OctopusProviderRateLimitWindow | null;
  credits: {
    hasCredits: boolean;
    unlimited: boolean;
    balance: string | null;
  } | null;
  individualLimit: {
    limit: string;
    used: string;
    remainingPercent: number;
    resetsAt: number;
  } | null;
  planType: string;
  reachedType: string;
};

export type OctopusProviderInterfacesResponse = {
  repoRoot?: string | null;
  registryRoot?: string | null;
  providers: OctopusProviderInterface[];
  error?: string;
};

export type OctopusProjectionReadRequest = OctopusClientProjectionReadRequest;

export type OctopusRuntimeTopologyProjection = {
  generation: number;
  snapshotRaw: unknown;
  eeRaw: unknown;
  runsRaw: unknown;
  runCheckpointsRaw: unknown;
  interfacesRaw: unknown;
  bindingsRaw: unknown;
  artifactsRaw: unknown;
  marksRaw: unknown;
  workerEnvironmentsRaw: unknown;
  movementsRaw: unknown;
  runDetailsRaw: Record<string, { detail: unknown; trace: unknown }>;
};

export type OctopusScheduledDraft = {
  type: "interval" | "cron" | "once" | "inactivity";
  inactivity?: { seconds: number; interface_match: Record<string, string>; exclude_interface_match?: Record<string, string>; within_ee?: string };
  dueAt?: string;
  intervalSeconds?: number;
  cron?: string;
  timezone: string;
  fireOnStart: boolean;
  instructions: string;
};

export type OctopusScheduledMutationRequest =
  | {
      action: "create";
      sourceInterfaceId: string;
      sourceActivationDigest: string;
      interfaceId: string;
      draft: OctopusScheduledDraft;
    }
  | {
      action: "update";
      interfaceId: string;
      expectedDigest: string;
      draft: OctopusScheduledDraft;
    }
  | {
      action: "set_enabled";
      interfaceId: string;
      expectedDigest: string;
      enabled: boolean;
    }
  | {
      action: "delete";
      interfaceId: string;
      expectedDigest: string;
    }
  | {
      action: "undo_changes";
      runId: string;
      expectedJournalDigest: string;
    };

export type OctopusScheduledMutationOutcome = {
  action: OctopusScheduledMutationRequest["action"];
  interfaceId?: string;
  runId?: string;
  activationDigest?: string;
  deleted?: boolean;
  restored?: string[];
};

export type OctopusApplicationPrivateConfigurationMutation = {
  applicationId: string;
  interfaceId: string;
  expectedDigest: string | null;
  updates: Record<string, string>;
  clears: string[];
};

export type OctopusApplicationConnectionState = {
  state: "connected" | "not_connected" | "expired" | "error";
  account?: { label: string; detail?: string };
  message?: string;
};

export type OctopusApplicationConnectionProbe = {
  applicationId: string;
  interfaces: Array<{ interfaceId: string; state: OctopusApplicationConnectionState }>;
};

export type OctopusApplicationConnectionRequest = {
  operationId: string;
  applicationId: string;
  interfaceId: string;
  input: Record<string, string>;
};

export type OctopusApplicationConnectionProgress = {
  operationId: string;
  applicationId: string;
  interfaceId: string;
  message: string;
  url?: string;
  code?: string;
};

export type OctopusApplicationPrivateConfigurationOutcome = {
  interfaceId: string;
  documentStatus: "absent" | "valid";
  documentDigest: string | null;
  configuredFields: number;
};

export type OctopusOsWorkspaceMutationRequest =
  | {
      action: "initialize_workspace";
      agentPresets?: import("./onboarding/workspaceSetup").WorkspaceAgentPresetChoice[];
      operationId: string;
      root: {
        key: "root";
        templateName: "existence";
        name: string;
        goal: string;
      } | {
        key: "root";
        existingEeId: string;
        expectedEeDigest: string;
        name: string;
      };
      home: {
        name: "Home";
        providerInterfaceId: string;
        providerModel: string;
        workerInterfaceId: string;
        reasoningEffort?: string;
  serviceTier?: string;
        doorHostSlots: Record<string, Record<string, string>>;
        contextArtifactBindings: Array<{ id: string }>;
        selectedWorkerEnvironmentKinds: string[];
      };
      task: import("./onboarding/workspaceSetup").WorkspaceInitializeRequest["task"];
      projects: Array<{
        key: string;
        parentKey: "root" | string;
        templateName: "project";
        name: string;
        goal: string;
        presentation: { icon: string; color: string };
      }>;
    }
  | {
      action: "create_project";
      operationId: string;
      workspace: {
        rootEeId: string;
        homeRunId: string;
        homeInterfaceId: string;
        expectedHomeSidecarDigest: string;
      };
      parentEeId: string;
      expectedParentDigest: string;
      templateName: string;
      sourceFolders?: string[];
      name: string;
      goal: string;
      presentation: { icon: string; color: string };
    }
  | {
      action: "add_agent_preset";
      operationId: string;
      home: string;
      workspace: { rootEeId: string; homeRunId: string; homeInterfaceId: string; expectedHomeSidecarDigest: string; expectedAgentCatalogDigest: string };
      projectEeId: string;
      packId: string;
      expectedPackManifestDigest: string;
      presetId: string;
      providerInterfaceId: string;
      providerModel: string;
      reasoningEffort: string;
      serviceTier?: string;
    }
  | {
      action: "create_agent_template";
      operationId: string;
      workspace: {
        rootEeId: string;
        homeRunId: string;
        homeInterfaceId: string;
        expectedHomeSidecarDigest: string;
        expectedAgentCatalogDigest: string;
      };
      projectEeId: string;
      sourceWorkerInterfaceId: string;
      name: string;
      icon: string;
      providerInterfaceId: string;
      providerModel: string;
      reasoningEffort: string;
      serviceTier?: string;
      applicationIds: string[];
      driverNames: string[];
    }
  | {
      action: "configure_agent_template";
      agentId: string;
      projectEeId: string;
      workerInterfaceId: string;
      name: string;
      icon: string;
      providerInterfaceId: string;
      providerModel: string;
      reasoningEffort: string;
      serviceTier?: string;
      applicationIds: string[];
      driverNames: string[];
      expectedClientTemplateDigest: string;
      expectedAgentCatalogDigest: string;
    }
  | {
      action: "delete_agent_template";
      agentId: string;
      projectEeId: string;
      workerInterfaceId: string;
      expectedClientTemplateDigest: string;
      expectedAgentCatalogDigest: string;
    }
  | {
      action: "delete_project";
      operationId: string;
      workspace: {
        rootEeId: string;
        homeRunId: string;
        homeInterfaceId: string;
        expectedHomeSidecarDigest: string;
      };
      eeId: string;
      expectedProjectDigest: string;
    }
  | {
      action: "update_project_presentation";
      operationId: string;
      workspace: {
        rootEeId: string;
        homeRunId: string;
        homeInterfaceId: string;
        expectedHomeSidecarDigest: string;
      };
      projects: Array<{
        eeId: string;
        sourceFolders?: string[];
        expectedProjectDigest?: string;
        name?: string;
        icon: string;
        color: string;
      }>;
    }
  | {
      action: "delete_task";
      runId: string;
      expectedDigest: string;
    }
  | {
      action: "set_task_archived";
      home: string;
      workspace: { rootEeId: string; homeRunId: string; homeInterfaceId: string };
      runId: string;
      archived: boolean;
      expectedDigest: string;
    }
  | {
      action: "install_application";
      applicationId: string;
      expectedSourceDigest: string;
      operationId: string;
    }
  | {
      action: "remove_application";
      applicationId: string;
      expectedInstalledDigest: string;
      operationId: string;
    }
  | {
      action: "install_package";
      packageIds: string[];
      expectedSourceDigests: Record<string, string>;
      operationId: string;
    }
  | {
      action: "remove_package";
      packageIds: string[];
      expectedInstalledDigests: Record<string, string>;
      operationId: string;
    }
  | {
      action: "repair_package";
      packageIds: string[];
      expectedRepairs: Record<string, {
        sourceDigest: string;
        installedDigest: string;
        receiptState: "missing" | "stale";
        receiptDigest: string | null;
      }>;
      operationId: string;
    }
  | {
      action: "apply_application_pack";
      packId: string;
      selectedRecommended: string[];
      expectedSourceDigests: Record<string, string>;
      operationId: string;
    }
  | {
      action: "install_driver";
      driverName: string;
      expectedSourceDigest: string;
      operationId: string;
    }
  | {
      action: "remove_driver";
      driverName: string;
      expectedInstalledDigest: string;
      operationId: string;
    }
  | {
      action: "configure_application";
      applicationId: string;
      projects: Array<{
        eeId: string;
        available: boolean;
        selectedByDefault: boolean;
        expectedDigest: string;
      }>;
    }
  | {
      action: "configure_driver";
      driverName: string;
      projects: Array<{
        eeId: string;
        available: boolean;
        selectedByDefault: boolean;
        hostSlots: Record<string, string>;
        expectedDigest: string;
      }>;
    }
  | {
      action: "configure_task_application";
      runId: string;
      applicationId: string;
      connected: boolean;
      expectedDigest: string;
    };

export type OctopusOsWorkspaceMutationOutcome = {
  action: OctopusOsWorkspaceMutationRequest["action"];
  eeId?: string;
  parentEeId?: string;
  templateName?: string;
  goal?: string;
  runId?: string;
  targetEeId?: string;
  capabilityId?: string;
  available?: boolean;
  selectedByDefault?: boolean;
  connected?: boolean;
  archived?: boolean;
  installed?: boolean;
  changed?: boolean;
  sourceKind?: string;
  archivedPath?: string;
  packageIds?: string[];
  packId?: string;
  removedInterfaces?: string[];
  removedTools?: string[];
  cleanupWarnings?: string[];
  operationId?: string;
  rootEeId?: string;
  homeRunId?: string;
  homeInterfaceId?: string;
  workerInterfaceId?: string;
  agentId?: string;
  clientTemplateDigest?: string;
  agentCatalogDigest?: string;
  receiptDigest?: string;
  projects?: Array<{ key: string; eeId: string }>;
  projectEeIds?: string[];
  projectChanges?: Array<{
    eeId: string;
    available: boolean;
    selectedByDefault: boolean;
  }>;
};

export type OctopusApplicationLifecycleProgress = {
  operationId: string;
  applicationId: string;
  packageId: string;
  operation: "install" | "remove" | "repair";
  phase: "preflight" | "verifying" | "copying" | "backing_up" | "materializing" | "installing" | "removing" | "finalizing" | "receipting" | "complete";
  percent: number;
};

export type OctopusWorkspaceSetupProgress = {
  operationId: string;
  phase: "planning" | "staging" | "applying" | "publishing" | "complete";
  percent: number;
  label: string;
};

export type OctopusWorkspaceSetupState = {
  status: "not_configured" | "configured" | "degraded";
  receipt: {
    operationId: string;
    requestDigest: string;
    completedAt: string;
    rootEeId: string;
    homeRunId: string;
    homeInterfaceId: string;
    projects: Array<{ key: string; eeId: string }>;
  } | null;
  errors: string[];
};

export type OctopusClientSetupPlan = {
  pack: {
    id: string;
    manifest_digest: string;
    selected_recommended: string[];
  };
  packages: Array<{
    id: string;
    source_digest: string;
    desired_state: "installed";
  }>;
  extensions: Array<{
    id: string;
    version: string;
    source_digest: string;
    desired_state: "installed" | "available";
  }>;
  provider: {
    interface_id: string;
    required_state: "account_readable";
  };
  workspace: {
    request_digest: string;
    request: Extract<OctopusOsWorkspaceMutationRequest, { action: "initialize_workspace" }>;
    project_defaults: Array<{
      package_id: string;
      product_kind: "application" | "driver";
      product_id: string;
      default_for_new_tasks: boolean;
      project_keys: string[];
    }>;
  };
};

export type OctopusClientSetupBeginRequest = {
  operationId: string;
  plan: OctopusClientSetupPlan;
};

export type OctopusClientSetupState = {
  status: "in_progress" | "complete";
  operationId: string;
  planDigest: string;
  plan: OctopusClientSetupPlan;
  steps: Array<{
    step: "packages" | "extensions" | "provider" | "workspace";
    state: "pending" | "complete";
    evidenceDigest: string;
  }>;
  updatedAt: string;
  lastError: {
    step: "packages" | "extensions" | "provider" | "workspace";
    message: string;
    observedAt: string;
  } | null;
  completedAt: string;
  receiptDigest: string;
};

export type OctopusClientSetupProgress = {
  operationId: string;
  step: "packages" | "extensions" | "provider" | "workspace";
  phase: "verifying" | "applying" | "complete" | "sealed";
  percent: number;
};

export type OctopusClientContext = readonly Readonly<{ name: string; text: string }>[];

export type OctopusChatTurnRequest = {
  replacesMessage?: { id: string; content: string };
  endpoint?: string;
  timeoutMs?: number;
  messageId?: string;
  recoverPreparedMessage?: boolean;
  runId?: string;
  providerInterfaceId: string;
  providerModel: string;
  reasoningEffort?: string;
  serviceTier?: string;
  eeId?: string;
  workerInterfaceId?: string;
  doorHostSlots?: Record<string, Record<string, string>>;
  externalInputs?: Array<{
    checkpointId: string;
    interfaceId: string;
    artifactNames?: string[];
    requiredVerifiedArtifacts?: Array<{
      mark: string;
      artifactLabel: string;
      verifier: string;
      params: Record<string, unknown>;
    }>;
  }>;
  externalRelays?: Array<{
    checkpointId: string;
    interfaceId: string;
    outboundInterfaceIds: string[];
    requiredVerifiedArtifacts?: Array<{
      mark: string;
      artifactLabel: string;
      verifier: string;
      params: Record<string, unknown>;
    }>;
  }>;
  modelOutputs?: Array<{
    checkpointId: string;
    interfaceId: string;
    requiredVerifiedArtifact: {
      mark: string;
      artifactLabel: string;
      verifier: string;
      params: Record<string, unknown>;
    };
  }>;
  contextArtifactBindings?: Array<{ id: string }>;
  payloadArtifactBindings?: Array<{ id: string; content: number[] }>;
  selectedWorkerEnvironmentKinds?: string[];
  selectedTools?: Array<{
    tool: string;
    interface: string;
    workerEnvironmentKind: string;
  }>;
  clientSurfaceRole: "home" | "task";
  clientContext?: OctopusClientContext;
  continuation?: {
    sourceRunId: string;
    sourceArtifactId: string;
  };
  text: string;
  attachments: Array<{
    id: string;
    name: string;
    label: string;
    type: string;
    size: number;
    lastModified: number;
    stage: OctopusClientAttachmentStageHandle;
  }>;
};

export type OctopusChatSteerRequest = {
  runId: string;
  messageId?: string;
  recoverPreparedMessage?: boolean;
  providerInterfaceId: string;
  providerModel: string;
  eeId?: string;
  workerInterfaceId?: string;
  clientSurfaceRole: "home" | "task";
  clientContext?: OctopusClientContext;
  endpoint?: string;
  text: string;
  timeoutMs?: number;
  attachments: OctopusChatTurnRequest["attachments"];
};

export type OctopusChatPinsRequest = {
  runId: string;
  artifactId: string;
  pinned: boolean;
  excerpt?: {
    sourceMessageId: string;
    text: string;
    start: number;
    end: number;
    role: "user" | "assistant" | "system";
  };
};

export type OctopusChatPinsResponse = {
  runId: string;
  pinnedArtifactIds: string[];
  pinnedContextItems: Array<{
    artifactId: string;
    sourceArtifactId: string;
    sourceMessageId: string;
    text: string;
    start: number;
    end: number;
    role: "user" | "assistant" | "system";
  }>;
  clientWrites: Array<{
    path: string;
    tag: string;
    beforeHash: string | null;
    afterHash: string;
    bytes: number;
    changed: boolean;
  }>;
};

export type OctopusDeliveryVerdict =
  | { admitted: { seq: number } }
  | { refused: { reason: string; detail: unknown } };

export type OctopusChatTurnResponse = {
  runId: string;
  chatInterfaceId: string;
  modelInterfaceId: string;
  workerInterfaceId: string;
  clientSurfaceRole: "home" | "task";
  label: string;
  messageId: string;
  payloadPath: string;
  contentHash: string;
  attachmentCount: number;
  clientWrites: Array<{
    path: string;
    tag: string;
    beforeHash: string | null;
    afterHash: string;
    bytes: number;
    changed: boolean;
  }>;
  messageRecovered: boolean;
  messageVerdict: OctopusDeliveryVerdict;
  outboundCursor: OctopusOperatorChatCursor;
};

export type OctopusOperatorChatCursor = {
  offset: number;
  seq: number | null;
  stream?: OctopusOperatorTurnStreamCursor;
  streams?: Record<string, OctopusOperatorTurnStreamCursor>;
};

export type OctopusOperatorTurnStreamCursor = {
  stream_id: string;
  offset: number;
  seq: number;
  recorded_at_unix_ms: number;
  source_movement_seq: number;
  sealed?: true;
};

export type OctopusOperatorChatArtifactsRequest = {
  interfaceId: string;
  runId: string;
  afterSeq: number;
  cursor?: OctopusOperatorChatCursor;
  waitMs?: number;
  limit?: number;
};

export type OctopusOperatorChatArtifactBase = {
  source: "movement" | "stream";
  recorded_at_unix_ms: number;
  run_id: string;
  interface_id: string;
  from_interface_id: string;
  from_interface_role: string;
  label: string;
  event_kind: string;
  event_data: Record<string, unknown>;
  text: string;
  error: boolean;
  terminal: boolean;
  will_retry: boolean;
};

export type OctopusOperatorChatMovementArtifact = OctopusOperatorChatArtifactBase & {
  source: "movement";
  movement_seq: number;
  checkpoint_id: string;
  artifact_id: string;
  content_hash: string;
};

export type OctopusOperatorChatStreamArtifact = OctopusOperatorChatArtifactBase & {
  source: "stream";
  stream_id: string;
  stream_seq: number;
  byte_start_offset: number;
  byte_end_offset: number;
  source_movement_seq: number;
};

export type OctopusOperatorChatArtifact =
  | OctopusOperatorChatMovementArtifact
  | OctopusOperatorChatStreamArtifact;

export type OctopusOperatorTurnControlRequest = {
  interfaceId: string;
  runId: string;
  controlId: string;
  action: "interrupt" | "resume" | "stop";
  timeoutMs?: number;
};

export type OctopusOperatorTurnControlResponse = {
  interfaceId: string;
  runId: string;
  label: "operator.turn.control";
  controlId: string;
  action: "interrupt" | "resume" | "stop";
  payloadPath: string;
  verdict: OctopusDeliveryVerdict;
} | {
  deliveryError: {
    stage: "not-sent" | "refused" | "admitted" | "outcome-unknown";
    message: string;
  };
};

export type OctopusOperatorChatArtifactsResponse = {
  artifacts: OctopusOperatorChatArtifact[];
  cursor: OctopusOperatorChatCursor;
};

export type OctopusChatSteerResponse = {
  interfaceId: string;
  runId: string;
  label: string;
  messageId: string;
  attachmentCount: number;
  contentHash: string;
  messageRecovered: boolean;
  verdict: OctopusDeliveryVerdict;
};
