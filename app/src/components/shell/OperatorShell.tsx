import { lazy, Suspense, useCallback, useEffect, useMemo, useRef, useState, type DragEvent } from "react";
import { AnimatePresence } from "motion/react";
import { AppShell, readSidebarOpen, readSidebarWidth } from "./AppShell";
import { isVoiceCallActive } from "../voice/voicePresentation";
import { SurfacePage } from "./SurfacePage";
import { ClientBrowserTab } from "./ClientBrowserTab";
import { OPEN_WEB_LINK_EVENT, type ClientBrowserState } from "../../clientBrowser";
import { TaskBrowserPane, emptyTaskBrowserTabs, type TaskBrowserTabs } from "./TaskBrowserPane";
import { MarkActivityView } from "../activity/MarkActivityView";
import { ScheduledWorkspaceProvider } from "../scheduled/ScheduledWorkspace";
import type { MarkActivityItem } from "../activity/markActivity";
import { CLIENT_NOTIFICATION_ACTIVATE_EVENT, type ClientNotificationTarget } from "../../clientNotifications";
import { SurfaceHost } from "./SurfaceHost";
import { taskDraftActions } from "./taskDraftActions";
import { RetainedTerminalTabs } from "./RetainedTerminalTabs";
import { RetainedWorkerEnvironmentTabs } from "./RetainedWorkerEnvironmentTabs";
import { terminalLaunchCwd } from "../../shell/terminalLaunchContext";
import { RetainedFilesTabs } from "../files/RetainedFilesTabs";
import { FilesPaneDialogs } from "../files/FilesPaneDialogs";
import {
  acquireFilesWindowClose,
  captureFilesPane,
  ensureFilesPane,
  filesPaneKey,
  filesPaneMatchesSnapshot,
  forgetFilesPane,
  hasFilesPane,
  readFilesPane,
  releaseFilesWindowClose,
  requestCloseAllFilesPanes,
  requestCloseFilesPanes,
  setFilesPaneTransferPending,
  validateFilesPaneRoot,
} from "../files/filesPaneLifecycle";
import { filesSnapshotDirty, normalizeFilesPaneSnapshot } from "../files/filesPaneSnapshot";
import {
  WorkspaceSurfacePane,
  type WorkspacePaneAddAction,
  type WorkspacePaneTab,
} from "./WorkspaceSurfacePane";
import { rollbackDestinationPaneTransfer } from "../../shell/paneTransferState";
import { useClientPaneDropZone } from "../../shell/useClientPaneDropZone";
import { useWorkspacePaneState, type WorkspacePaneState } from "./useWorkspacePaneState";
import { anchorClientPaneDrop, useClientPaneWindowDrag } from "./useClientPaneWindowDrag";
import { paneOwnsWindowTitlebar, secondaryPaneUsesTabStrip } from "./paneChromeOwnership";
import {
  OPEN_WORKER_ENVIRONMENT_PANE_EVENT,
  activeWorkerEnvironmentPaneActivities,
  isControllableNativeBrowserSurface,
  paneTargetTitle,
  resolvePaneClientSurface,
  resolvePaneSurface,
  resolveWorkerEnvironment,
  workerEnvironmentPaneActions,
  type WorkerEnvironmentPaneTarget,
} from "../worker-environment/workerEnvironmentPane";
import { workerEnvironmentPanelKind, type WorkerEnvironmentPanelKind } from "../worker-environment/surfaceRegistry";
import { TopBar } from "../TopBar";
import { SurfaceLoadingState, SurfaceRenderBoundary } from "./SurfaceBoundary";
import type { SelectionRef } from "../../data/topology";
import type { OctopusClientAccessProfile, OctopusClientWindowCloseRequest } from "../../electron";
import { useRuntimeTopology } from "../../runtimeTopology";
import { useClientSurfaces } from "../../shell/surfaceRegistry";
import { applyHomeWidgetAction } from "../../client-surfaces/homeWidgetControls";
import { useClientObservation } from "../../client-surfaces/useClientObservation";
import { useAdmittedClientActions } from "../../shell/useAdmittedClientActions";
import type { ClientSurfaceDescriptor } from "../../shell/surfaceTypes";
import type { ClientExtensionDestination } from "../settings/ExtensionsSection";
import {
  clientWindowChatStorageScope,
  clientWindowStateKey,
  clientWindowStateStorage,
  CLIENT_PANE_DRAG_MIME,
  clientSurfaceRunIdentityReady,
  deriveActiveClientSurfaceIdentity,
  parseClientPaneDragPayload,
  transferableWorkspacePaneTarget,
  workspacePaneTabForTarget,
  workspaceTaskSourcesPaneTab,
  type ClientPaneDragPayload,
  type ClientPaneTransferCompleted,
  type ClientWindowBootstrap,
  type TransferableWorkspacePaneTarget,
} from "../../shell/clientWindowModel";
import {
  DASHBOARD_PINNED_SURFACES_STORAGE_KEY,
  PINNED_TASKS_STORAGE_KEY,
  PROJECT_FILTER_STORAGE_KEY,
  TASK_ORDER_STORAGE_KEY,
  defaultPinnedTaskIds,
  clientTaskTitle,
  clientProjectScopeIds,
  ensureLabSidebarSeeds,
  profileStorageKey,
  readClientProjectFilterEeId,
  readStoredStringList,
  saveClientProjectFilterEeId,
  saveStoredStringList,
  surfaceBelongsToWorkspace,
  workspaceForSurface,
  useClientTaskOrganization,
  type ClientWorkspaceKey,
} from "../../shell/clientWorkspaces";
import { useOctopusOsProjects } from "../../shell/useOctopusOsProjects";
import { activeDestinationStorageKey, clientNavigationDestinations, navigationSectionForSurface, navigationSectionLabel, type ClientNavigationSection } from "../../shell/navigation";
import { useChatWorkspace } from "../chat/useChatWorkspace";
import { projectAssistantCodeComments } from "../chat/assistantDirectives";
import {
  collectTaskSources,
  OPEN_TASK_SOURCES_PANE_EVENT,
  type TaskSourcesPaneTarget,
} from "../chat/taskSourcesPane";
import { defaultDashboardFilters, type DashboardFilters, type DashboardScope, type DashboardTopologyMode } from "../../uiState";
import { appToast } from "../../toasts";
import { normalizeSettingsSection, useClientPreferences, useSetClientPreferences } from "../../clientPreferences";
import { useVoiceSession } from "../voice/useVoiceSession";
import { projectHomeVoiceContext } from "../voice/voiceCallArtifacts";
import type { HomeViewMode } from "../voice/VoiceView";
import { useMarkActivity } from "../activity/useMarkActivity";
import { dispatchClientCommand, subscribeClientCommands, type ClientCommandId } from "../../clientActions";
import { clientDialogOwnsCommands, formatKeyboardShortcut, keyboardCommandConflictsWithEditing, keyboardCommandForEvent, keyboardTargetHasEditableOwner, keyboardTargetHasRichEditingOwner, useKeyboardShortcuts } from "../../keyboardShortcuts";
import { subscribeAirHandGestures, useGestureShortcutPreferences, useSetGestureShortcutPreferences } from "../../gestureShortcuts";
import { useCustomGestureDispatch } from "../../customGestures";
import { useGestureFocus } from "../../gestureFocus";
import { GestureCommandHud } from "../gestures/GestureCommandHud";
import { GestureFocusRing } from "../gestures/GestureFocusRing";
import { GestureTourHost } from "../gestures/GestureTourDialog";
import { startGestureRecognition, stopGestureRecognition } from "../../gestureRecognition";
import type { RuntimeHostView } from "../../runtimeHost";
import type { ClientSurfaceActionEvent } from "../../client-surfaces/types";
import {
  CLIENT_SURFACE_ACTION_EVENT,
  MCP_RESOURCE_ACTION_EVENT,
  OCTOPUS_OPERATOR_DIFF_REVIEW_SEND_EVENT,
  type McpResourceActionDetail,
  type OperatorDiffReviewSendDetail,
} from "../../clientEvents";
import { ARTIFACT_CONTEXT_PREPARED_EVENT } from "../artifacts/artifactContext";
import type { ArtifactContextPreparedDetail } from "../artifacts/types";
import type { FilesOpenTarget } from "../files/types";
import { updateOperatorChatPins } from "../../projectionClient";
import { offerClientProfileStorageRecovery } from "../../clientLocalDataRecovery";
import { BuiltinClientExtensionActionHost } from "../../extensions/builtinActionContributions";
import { inspectClient } from "../../extensions/clientInventory";
import { useExtensionReviewPrompt } from "../../extensions/useExtensionReviewPrompt";
import type { ClientExtensionRegistryState } from "../../extensions/types";
import {
  clientDestination,
  clientWindowInitialDestination,
  initializeSurfaceNavigation,
  isClientCommandId,
  isSidebarOnlySurface,
  normalizeSurfaceNavigation,
  readSecondaryPaneState,
  readSecondaryPaneStateAtKey,
  resolveSelection,
  secondaryPaneTab,
  secondaryPaneStorageKey,
  sameClientDestination,
  stringifyPreparedContext,
  writeSecondaryPaneState,
  writeSecondaryPaneStateAtKey,
  type ClientDestination,
  type SecondaryPaneTabState,
} from "./operatorShellState";

const SettingsPage = lazy(() => import("../settings/SettingsPage").then((module) => ({ default: module.SettingsPage })));
const TaskSourcesPaneTab = lazy(() => import("../chat/TaskSourcesPaneTab").then((module) => ({ default: module.TaskSourcesPaneTab })));
const VoiceCallPuck = lazy(() => import("../voice/VoiceCallPuck").then((module) => ({ default: module.VoiceCallPuck })));
const CommandMenu = lazy(() => import("../CommandMenu").then((module) => ({ default: module.CommandMenu })));

function installedVoiceWidgetIds(extensions: ClientExtensionRegistryState): string[] {
  return [...new Set([
    ...extensions.installedBuiltinWidgetIds,
    ...extensions.voiceSurfaces.map((surface) => surface.document.id),
  ])];
}

function sameStringList(left: readonly string[], right: readonly string[]): boolean {
  return left.length === right.length && left.every((value, index) => value === right[index]);
}

function samePaneTransferTarget(
  left: TransferableWorkspacePaneTarget | null,
  right: TransferableWorkspacePaneTarget,
): boolean {
  return Boolean(left) && JSON.stringify(left) === JSON.stringify(right);
}


type ActiveClientPaneTransfer = {
  reference: ClientPaneDragPayload;
  target: TransferableWorkspacePaneTarget;
};

type WorkspacePaneOwner = {
  key: string;
  read: () => WorkspacePaneState;
  write: (state: WorkspacePaneState) => void;
};

export function OperatorShell({ accessProfile, clientWindow, onLogOut, runtimeHost }: {
  accessProfile: OctopusClientAccessProfile;
  clientWindow: ClientWindowBootstrap;
  onLogOut: () => void;
  runtimeHost: RuntimeHostView;
}) {
  ensureLabSidebarSeeds(accessProfile);
  const windowStateStorage = clientWindowStateStorage(clientWindow.scope);
  const initialTargetConsumptionKey = clientWindowStateKey(
    clientWindow.scope,
    "octopus.client-window.initial-target-consumed.v1",
  );
  const clientWindowInitialTarget = useMemo(() => {
    if (!clientWindow.initialTarget) return undefined;
    try {
      return window.sessionStorage.getItem(initialTargetConsumptionKey) === "true"
        ? undefined
        : clientWindow.initialTarget;
    } catch {
      return clientWindow.initialTarget;
    }
  }, [clientWindow.initialTarget, initialTargetConsumptionKey]);
  const surfaceRegistry = useClientSurfaces(accessProfile);
  const installedHomeWidgetIds = useMemo(
    () => installedVoiceWidgetIds(surfaceRegistry.extensions),
    [surfaceRegistry.extensions.installedBuiltinWidgetIds, surfaceRegistry.extensions.voiceSurfaces],
  );
  const agentProjectWorkspace = useOctopusOsProjects(accessProfile, runtimeHost.home);
  const [projectFilterEeId, setProjectFilterEeIdState] = useState(() => readClientProjectFilterEeId(accessProfile).trim());
  const clientPreferences = useClientPreferences();
  const taskOrganization = useClientTaskOrganization(accessProfile);
  const setClientPreferences = useSetClientPreferences();
  const gesturePreferences = useGestureShortcutPreferences(accessProfile);
  const setGesturePreferences = useSetGestureShortcutPreferences(accessProfile);
  const keyboardShortcuts = useKeyboardShortcuts(accessProfile);
  const homeRunId = agentProjectWorkspace.homeRunId;
  const osRootEeId = agentProjectWorkspace.rootEeId;
  const [effectiveSidebarWidth, setEffectiveSidebarWidth] = useState(() => readSidebarWidth(accessProfile, clientWindow.scope));
  useEffect(() => {
    if (clientWindow.primary) offerClientProfileStorageRecovery(accessProfile);
  }, [accessProfile, clientWindow.primary]);
  const visibleSurfaces = useMemo(() => {
    const hiddenSurfaceIds = new Set(clientPreferences.hiddenSurfaceIds);
    const primaryDestinations = new Set(clientNavigationDestinations.map((destination) => destination.surfaceId));
    return surfaceRegistry.surfaces.filter((surface) => primaryDestinations.has(surface.id) || !hiddenSurfaceIds.has(surface.id));
  }, [clientPreferences.hiddenSurfaceIds, surfaceRegistry.surfaces]);
  const clientContext = useClientObservation({
    accessProfile, runId: homeRunId, interfaceId: agentProjectWorkspace.homeInterfaceId,
    surfaces: surfaceRegistry.voiceSurfaces, extensions: surfaceRegistry.extensions,
    availableSurfaces: visibleSurfaces.filter(surface => !isSidebarOnlySurface(surface)),
    ready: surfaceRegistry.hydrated && !surfaceRegistry.errors.length && !agentProjectWorkspace.error,
  });
  const [primaryPaneBlocked, setPrimaryPaneBlocked] = useState(false);
  const chatWorkspace = useChatWorkspace(
    accessProfile,
    true,
    runtimeHost.ready,
    clientWindowChatStorageScope(clientWindow.scope, "main"),
    "task",
    runtimeHost.home,
    clientContext,
  );
  const homeChatWorkspace = useChatWorkspace(
    accessProfile,
    true,
    runtimeHost.ready,
    clientWindowChatStorageScope(clientWindow.scope, "home"),
    "home",
    runtimeHost.home,
    clientContext,
  );
  const initialDestination = clientWindowInitialTarget ? clientWindowInitialDestination(clientWindowInitialTarget) : null;
  const startupSurfaceId = clientWindow.primary ? clientPreferences.startupSurfaceId : initialDestination?.surfaceId ?? "voice";
  const [surfaceNavigation, setSurfaceNavigation] = useState(
    () => initialDestination ? { entries: [initialDestination], index: 0 }
      : initializeSurfaceNavigation(startupSurfaceId, accessProfile, chatWorkspace.activeRunId, clientWindow.scope),
  );
  const [settingsRecordingId, setSettingsRecordingId] = useState("");
  const activeNavigation = surfaceNavigation;
  const activeDestination = activeNavigation.entries[activeNavigation.index] ?? clientDestination("voice");
  const settingsOpen = Boolean(activeDestination.settingsSection);
  const paneScope = JSON.stringify([runtimeHost.home, activeDestination.surfaceId, activeDestination.runId ?? ""]);
  const paneStateKey = secondaryPaneStorageKey(accessProfile, homeRunId, clientWindow.scope, paneScope);
  const activePaneStateKeyRef = useRef(paneStateKey);
  activePaneStateKeyRef.current = paneStateKey;
  const initialWorkspacePaneState = useMemo<WorkspacePaneState>(() => {
    const restored = readSecondaryPaneState(accessProfile, homeRunId, clientWindow.scope, paneScope);
    const initialTarget = clientWindowInitialTarget && window.sessionStorage.getItem(initialTargetConsumptionKey) !== "true"
      ? clientWindowInitialTarget : undefined;
    const tab = initialTarget ? workspacePaneTabForTarget(initialTarget) : null;
    return {
      ...restored,
      ...(tab ? { open: true, activeTabId: tab.id, tabs: [tab] } : {}),
      layout: restored.layout ?? {
        activePane: tab ? "secondary" : "primary",
        focused: Boolean(tab),
        sidebarOpen: readSidebarOpen(accessProfile, clientWindow.scope, !clientWindow.initialTarget),
        sidebarWidth: readSidebarWidth(accessProfile, clientWindow.scope),
        widthPercent: clientPreferences.secondaryTaskPaneWidthPercent,
      },
    };
  }, [accessProfile, clientPreferences.secondaryTaskPaneWidthPercent, clientWindow.initialTarget, clientWindow.scope, clientWindowInitialTarget, homeRunId, initialTargetConsumptionKey, paneScope]);
  const workspacePane = useWorkspacePaneState(paneStateKey, initialWorkspacePaneState);
  const { state: workspacePaneState, tabs: secondaryTabModel, setOpen: setSecondaryPaneOpen, setFocused: setSecondaryPaneFocused,
    setActivePane, setSidebarWidth } = workspacePane;
  const secondaryPaneOpen = workspacePaneState.open;
  const { activePane, focused: secondaryPaneFocused, sidebarWidth } = workspacePaneState.layout;
  const paneOwner = useMemo<WorkspacePaneOwner>(() => ({
    key: paneStateKey,
    read: workspacePane.getState,
    write: (state) => workspacePane.setStateAt(paneStateKey, state),
  }), [paneStateKey, workspacePane.getState, workspacePane.setStateAt]);
  const transferPaneOwner = useCallback((transferId: string): WorkspacePaneOwner | null => {
    const key = window.sessionStorage.getItem(`octopus.client-pane-owner.${transferId}`);
    if (!key) return null;
    const stored = workspacePane.getStateAt(key) ?? readSecondaryPaneStateAtKey(clientWindow.scope, key);
    if (!stored.layout) return null;
    const state: WorkspacePaneState = { ...stored, layout: stored.layout };
    return { key, read: () => workspacePane.getStateAt(key) ?? state, write: (next) => workspacePane.setStateAt(key, next) };
  }, [clientWindow.scope, workspacePane.getStateAt, workspacePane.setStateAt]);
  const getWorkspacePaneStates = workspacePane.getStates;
  const secondaryChatWorkspace = useChatWorkspace(
    accessProfile,
    secondaryPaneOpen,
    runtimeHost.ready,
    clientWindowChatStorageScope(clientWindow.scope, "secondary"),
    "task",
    runtimeHost.home,
    clientContext,
  );
  const taskWorkspaceNavigationRef = useRef({ main: chatWorkspace, secondary: secondaryChatWorkspace });
  taskWorkspaceNavigationRef.current = { main: chatWorkspace, secondary: secondaryChatWorkspace };
  const voiceSession = useVoiceSession(accessProfile);
  const voiceContext = useMemo(() => projectHomeVoiceContext(
    homeChatWorkspace.renderedMessages, homeRunId,
    homeChatWorkspace.activeRun?.runId === homeRunId ? homeChatWorkspace.activeRun.chatInterfaceId : "",
    homeChatWorkspace.activeRun?.runId === homeRunId ? homeChatWorkspace.activeRun.voiceContext?.delegationInterfaceId : "",
    clientContext,
  ), [homeChatWorkspace.renderedMessages, homeChatWorkspace.activeRun, homeRunId, clientContext]);
  useEffect(() => { void voiceSession.updateContext(voiceContext); }, [voiceSession.updateContext, voiceContext]);
  const setProjectFilterEeId = useCallback((eeId: string) => {
    const next = eeId.trim();
    setProjectFilterEeIdState(next);
    saveClientProjectFilterEeId(accessProfile, next);
  }, [accessProfile]);
  const taskChatRuns = useMemo(
    () => chatWorkspace.runs.filter((run) => (
      run.clientSurfaceRole === "task" && (run.writable || run.archived || clientPreferences.showInternalRuns)
    )),
    [chatWorkspace.runs, clientPreferences.showInternalRuns],
  );
  useEffect(() => {
    if (agentProjectWorkspace.loading) return;
    if (projectFilterEeId && !agentProjectWorkspace.projects.some((project) => project.id === projectFilterEeId)) {
      setProjectFilterEeId("");
    }
  }, [agentProjectWorkspace.loading, agentProjectWorkspace.projects, projectFilterEeId, setProjectFilterEeId]);

  useEffect(() => {
    if (homeRunId) {
      if (homeChatWorkspace.activeRunId !== homeRunId) homeChatWorkspace.selectRun(homeRunId);
      return;
    }
    if (!homeChatWorkspace.activeRunId && osRootEeId && homeChatWorkspace.eeId !== osRootEeId) {
      homeChatWorkspace.setEeId(osRootEeId);
    }
  }, [homeChatWorkspace.activeRunId, homeChatWorkspace.eeId, homeChatWorkspace.selectRun, homeChatWorkspace.setEeId, homeRunId, osRootEeId]);
  const shouldKeepAwake = clientPreferences.keepAwakeDuringRuns && (
    chatWorkspace.busy
    || homeChatWorkspace.busy
    || secondaryChatWorkspace.busy
    || isVoiceCallActive(voiceSession.phase)
  );

  useEffect(() => {
    const bridge = window.octopusTransport;
    if (!bridge?.setPowerSaveBlocker) return;
    void bridge.setPowerSaveBlocker(shouldKeepAwake);
    return () => {
      if (shouldKeepAwake) void bridge.setPowerSaveBlocker(false);
    };
  }, [shouldKeepAwake]);

  useEffect(() => {
    const bridge = window.octopusTransport;
    if (!bridge?.setGlobalDictationShortcut) return;
    const shortcut = keyboardShortcuts.start_dictation?.[0] ?? "";
    void bridge.setGlobalDictationShortcut({
      enabled: clientPreferences.globalQuickDictation,
      shortcut,
    }).catch((error) => {
      if (!clientPreferences.globalQuickDictation) return;
      setClientPreferences({ globalQuickDictation: false });
      appToast.warning(error instanceof Error ? error.message : String(error));
    });
  }, [clientPreferences.globalQuickDictation, keyboardShortcuts.start_dictation, setClientPreferences]);

  useEffect(() => {
    void window.octopusTransport?.setNativeCommandShortcuts?.(keyboardShortcuts).catch((error) => {
      appToast.warning(error instanceof Error ? error.message : String(error));
    });
  }, [keyboardShortcuts]);

  useEffect(() => window.octopusTransport?.onNativeClientCommand?.((command) => {
    if (!isClientCommandId(command)) return;
    if (clientDialogOwnsCommands()) return;
    if (keyboardTargetHasRichEditingOwner(document.activeElement) && keyboardCommandConflictsWithEditing(command, keyboardShortcuts)) return;
    dispatchClientCommand(command, "keyboard");
  }), [keyboardShortcuts]);

  useEffect(() => {
    if (clientWindow.primary || !window.octopusTransport?.clientWindowReady) return;
    const frame = window.requestAnimationFrame(() => {
      void window.octopusTransport?.clientWindowReady({ windowId: clientWindow.windowId }).catch((error) => {
        appToast.danger(error instanceof Error ? error.message : String(error));
      });
    });
    return () => window.cancelAnimationFrame(frame);
  }, [clientWindow.primary, clientWindow.windowId]);

  useEffect(() => {
    if (secondaryPaneOpen) return;
    setSecondaryPaneFocused(false);
    setActivePane("primary");
  }, [secondaryPaneOpen, setActivePane, setSecondaryPaneFocused]);
  useEffect(() => {
    if (!secondaryPaneOpen) return;
    const recordActivePane = (event: Event) => {
      if (!(event.target instanceof Element)) return;
      const pane = event.target.closest<HTMLElement>("[data-pane-role]")?.dataset.paneRole;
      if (pane === "primary" || pane === "secondary") setActivePane(pane);
    };
    window.addEventListener("focusin", recordActivePane, true);
    window.addEventListener("pointerdown", recordActivePane, true);
    return () => {
      window.removeEventListener("focusin", recordActivePane, true);
      window.removeEventListener("pointerdown", recordActivePane, true);
    };
  }, [secondaryPaneOpen, setActivePane]);
  useGestureFocus(accessProfile, gesturePreferences.enabled, gesturePreferences.spatialControl);
  useCustomGestureDispatch(accessProfile, gesturePreferences.enabled && !settingsOpen);
  const activeSurfaceId = activeDestination.surfaceId;
  const shortcutNavigationDestinations = useMemo(() => clientNavigationDestinations.filter((destination) => (
    visibleSurfaces.some((surface) => surface.id === destination.surfaceId)
  )), [visibleSurfaces]);
  const activeSurface = useMemo(() => (
    visibleSurfaces.find((surface) => surface.id === activeSurfaceId)
      ?? visibleSurfaces.find((surface) => surface.id === "voice")
  ), [activeSurfaceId, visibleSurfaces]);
  const activeWorkspaceKey = activeSurface ? workspaceForSurface(activeSurface) : "octopus-os";
  const secondaryTabs = secondaryTabModel.tabs;
  const activeSecondaryTabId = secondaryTabModel.activeTabId;
  const activeSecondaryTab = secondaryTabs.find((tab) => tab.id === activeSecondaryTabId) ?? null;
  const nativeWindowCloseRequestRef = useRef<{ requestId: string; generation: number } | null>(null);
  const presentedBrowserTargetsRef = useRef({ scope: "", baseline: new Map<string, number>(), identities: new Set<string>() });
  const [browserPresentationGeneration, setBrowserPresentationGeneration] = useState(0);
  const [taskBrowserTabs, setTaskBrowserTabs] = useState<Record<string, TaskBrowserTabs>>({});
  const preparedSourceTransfersRef = useRef(new Map<string, ClientPaneTransferCompleted>());
  const [sourceHandoffPending, setSourceHandoffPending] = useState(false);
  const finalizingSourceIdsRef = useRef(new Set<string>());
  const [sourceFinalizationPendingCount, setSourceFinalizationPendingCount] = useState(0);
  const pendingAcceptedPaneTransfersRef = useRef(new Map<string, {
    owner: WorkspacePaneOwner;
    previousActiveTabId: string;
    previousActivePane: "primary" | "secondary";
    previousFocused: boolean;
    previousOpen: boolean;
    previousTabs: SecondaryPaneTabState[];
    detachedWindow: boolean;
    resume: boolean;
    started: boolean;
    tabId: string;
    stagedTab: SecondaryPaneTabState;
  }>());
  const secondaryWorkspaceKey = activeSecondaryTab?.target.workspaceKey ?? "octopus-os";
  const secondarySurfaceId = activeSecondaryTab?.target.kind === "surface" ? activeSecondaryTab.target.surfaceId : "";
  const setSecondaryTabs = secondaryTabModel.setTabs;
  const setActiveSecondaryTabId = secondaryTabModel.selectTab;
  const newSecondaryTaskPhaseRef = useRef<"idle" | "awaiting_empty" | "awaiting_run">("idle");
  const restoreSecondarySelectionRef = useRef(Boolean(initialWorkspacePaneState.tabs.length));
  const activatedSecondaryTabIdRef = useRef("");
  const secondaryWorkspaceSurfaces = useMemo(() => visibleSurfaces.filter((surface) => (
    !isSidebarOnlySurface(surface)
    && surfaceBelongsToWorkspace(surface, secondaryWorkspaceKey)
  )), [secondaryWorkspaceKey, visibleSurfaces]);
  const secondarySurface = useMemo(() => (
    activeSecondaryTab?.target.kind === "surface"
      ? secondaryWorkspaceSurfaces.find((surface) => surface.id === secondarySurfaceId)
      : undefined
  ), [activeSecondaryTab, secondarySurfaceId, secondaryWorkspaceSurfaces]);
  const primarySurfaceTarget = useMemo(() => ({
    runId: activeSurface?.id === "chat" ? activeDestination.runId ?? "" : "",
    surfaceId: activeSurface?.id ?? "",
    workspaceKey: activeWorkspaceKey,
  }), [activeDestination.runId, activeSurface?.id, activeWorkspaceKey]);
  const primaryRunIdentityReady = clientSurfaceRunIdentityReady(primarySurfaceTarget, chatWorkspace.activeRunId);
  const secondaryRunIdentityReady = activeSecondaryTab?.target.kind !== "surface"
    || clientSurfaceRunIdentityReady({
      runId: activeSecondaryTab.target.surfaceId === "chat" ? activeSecondaryTab.target.runId : "",
      surfaceId: activeSecondaryTab.target.surfaceId,
      workspaceKey: activeSecondaryTab.target.workspaceKey,
    }, secondaryChatWorkspace.activeRunId);
  const activeClientSurfaceIdentity = useMemo(() => deriveActiveClientSurfaceIdentity({
    activePane: primaryPaneBlocked && secondaryPaneOpen ? "secondary" : activePane,
    primary: {
      target: primarySurfaceTarget,
      workspaceActiveRunId: chatWorkspace.activeRunId,
    },
    secondary: {
      fallbackWorkspaceKey: secondaryWorkspaceKey,
      open: secondaryPaneOpen,
      target: activeSecondaryTab?.target ?? null,
      workspaceActiveRunId: secondaryChatWorkspace.activeRunId,
    },
  }), [
    activePane,
    activeSecondaryTab,
    chatWorkspace.activeRunId,
    primarySurfaceTarget,
    primaryPaneBlocked,
    secondaryChatWorkspace.activeRunId,
    secondaryPaneOpen,
    secondaryWorkspaceKey,
  ]);
  const activeNavigationSurface = activeSurface;
  const activeNavigationSection = activeNavigationSurface ? navigationSectionForSurface(activeNavigationSurface) : "home";
  const lastTasksDestinationRef = useRef<ClientDestination | null>(null);
  useEffect(() => {
    if (activeNavigationSection !== "tasks" || !primaryRunIdentityReady) return;
    lastTasksDestinationRef.current = clientDestination(primarySurfaceTarget.surfaceId, primarySurfaceTarget.runId);
  }, [primaryRunIdentityReady, primarySurfaceTarget.runId, primarySurfaceTarget.surfaceId, activeNavigationSection]);
  const primaryCommandsActive = !settingsOpen && activeClientSurfaceIdentity.pane === "primary" && activeClientSurfaceIdentity.ready;
  const secondaryCommandsActive = !settingsOpen && activeClientSurfaceIdentity.pane === "secondary" && activeClientSurfaceIdentity.ready;
  const titlebarTransferTarget = useMemo(() => {
    if (!paneOwnsWindowTitlebar(activeClientSurfaceIdentity.pane) || secondaryPaneOpen) return null;
    if (!activeClientSurfaceIdentity.ready || !activeClientSurfaceIdentity.surfaceId) return null;
    return {
      kind: "surface" as const,
      runId: activeClientSurfaceIdentity.runId,
      surfaceId: activeClientSurfaceIdentity.surfaceId,
      workspaceKey: activeClientSurfaceIdentity.workspaceKey,
    };
  }, [activeClientSurfaceIdentity, secondaryPaneOpen]);
  const [selectedRef, setSelectedRef] = useState<SelectionRef | null>(null);
  const [secondarySelectedRef, setSecondarySelectedRef] = useState<SelectionRef | null>(null);
  const topologyFocusRunIds = useMemo(() => (
    [...new Set([
      homeRunId,
      primarySurfaceTarget.runId,
      chatWorkspace.activeRunId,
      secondaryPaneOpen && activeSecondaryTab?.target.kind === "surface" ? activeSecondaryTab.target.runId : "",
      secondaryPaneOpen && activeSecondaryTab?.target.kind === "task_sources" ? activeSecondaryTab.target.target.runId : "",
      secondaryPaneOpen ? secondaryChatWorkspace.activeRunId : "",
      selectedRef?.kind === "run" ? selectedRef.id : "",
      secondaryPaneOpen && secondarySelectedRef?.kind === "run" ? secondarySelectedRef.id : "",
    ].filter(Boolean))]
  ), [
    chatWorkspace.activeRunId,
    activeSecondaryTab,
    homeRunId,
    primarySurfaceTarget.runId,
    secondaryChatWorkspace.activeRunId,
    secondaryPaneOpen,
    secondarySelectedRef,
    selectedRef,
  ]);
  const topologyIncludesLedger = activeSurface?.rendererKind === "builtin.dashboard"
    || (secondaryPaneOpen && secondarySurface?.rendererKind === "builtin.dashboard");
  const topology = useRuntimeTopology(Boolean(
    activeSurface?.chrome.usesTopology
      // The side pane's launcher and exact Worker Environment tabs are topology
      // consumers even when the last ordinary side surface was not one.
      || secondaryPaneOpen,
  ), topologyFocusRunIds, topologyIncludesLedger);
  const activeWorkerEnvironmentAssistantCodeComments = useMemo(() => {
    if (activeSecondaryTab?.target.kind !== "worker_environment") return [];
    const resolved = resolveWorkerEnvironment(topology, activeSecondaryTab.target.target.workerEnvironmentId);
    const runId = resolved?.run?.id ?? resolved?.workerEnvironment.runId ?? "";
    if (!runId) return [];
    const messages = [chatWorkspace, homeChatWorkspace, secondaryChatWorkspace]
      .filter((workspace) => workspace.activeRunId === runId)
      .flatMap((workspace) => workspace.renderedMessages);
    return projectAssistantCodeComments(messages, runId);
  }, [
    activeSecondaryTab,
    chatWorkspace.activeRunId,
    chatWorkspace.renderedMessages,
    homeChatWorkspace.activeRunId,
    homeChatWorkspace.renderedMessages,
    secondaryChatWorkspace.activeRunId,
    secondaryChatWorkspace.renderedMessages,
    topology,
  ]);

  const [query, setQuery] = useState("");
  const [secondaryQuery, setSecondaryQuery] = useState("");
  const [searchFocusTick, setSearchFocusTick] = useState(0);
  const [secondarySearchFocusTick, setSecondarySearchFocusTick] = useState(0);
  const [dashboardScope, setDashboardScope] = useState<DashboardScope>("System");
  const [secondaryDashboardScope, setSecondaryDashboardScope] = useState<DashboardScope>("System");
  const [dashboardTopologyMode, setDashboardTopologyMode] = useState<DashboardTopologyMode>("drilldown");
  const [secondaryDashboardTopologyMode, setSecondaryDashboardTopologyMode] = useState<DashboardTopologyMode>("drilldown");
  const [dashboardWorldTopologyMode, setDashboardWorldTopologyMode] = useState<DashboardTopologyMode>("drilldown");
  const [secondaryDashboardWorldTopologyMode, setSecondaryDashboardWorldTopologyMode] = useState<DashboardTopologyMode>("drilldown");
  const [dashboardFilters, setDashboardFilters] = useState<DashboardFilters>(defaultDashboardFilters);
  const [secondaryDashboardFilters, setSecondaryDashboardFilters] = useState<DashboardFilters>(defaultDashboardFilters);
  const [homeViewMode, setHomeViewMode] = useState<HomeViewMode>("voice");
  const [secondaryHomeViewMode, setSecondaryHomeViewMode] = useState<HomeViewMode>("voice");
  const homeChatIdentityReady = Boolean(homeRunId && homeChatWorkspace.activeRunId === homeRunId
    && homeChatWorkspace.activeRun?.runId === homeRunId && homeChatWorkspace.activeRun.clientSurfaceRole === "home");
  const visibleChatRunIds = useMemo(() => settingsOpen ? [] : [
    !primaryPaneBlocked && activeSurface?.rendererKind === "builtin.chat" && primaryRunIdentityReady ? primarySurfaceTarget.runId : "",
    !primaryPaneBlocked && activeSurface?.rendererKind === "builtin.voice" && homeViewMode === "activity" && homeChatIdentityReady ? homeRunId : "",
    secondaryPaneOpen && secondarySurface?.rendererKind === "builtin.chat" && secondaryRunIdentityReady ? secondaryChatWorkspace.activeRunId : "",
    secondaryPaneOpen && secondarySurface?.rendererKind === "builtin.voice" && secondaryHomeViewMode === "activity" && homeChatIdentityReady ? homeRunId : "",
  ].filter(Boolean), [settingsOpen, primaryPaneBlocked, activeSurface?.rendererKind, primaryRunIdentityReady, primarySurfaceTarget.runId,
    homeViewMode, homeChatIdentityReady, homeRunId, secondaryPaneOpen, secondarySurface?.rendererKind,
    secondaryRunIdentityReady, secondaryChatWorkspace.activeRunId, secondaryHomeViewMode]);
  const markActivity = useMarkActivity(accessProfile, runtimeHost.home, clientWindow.primary, activeNavigationSection === "inbox", visibleChatRunIds);
  const [activitySelection, setActivitySelection] = useState<MarkActivityItem | null>(null);
  const [activitySelectionGroup, setActivitySelectionGroup] = useState<readonly string[]>([]);
  const [secondaryMovementSelection, setSecondaryMovementSelection] = useState<MarkActivityItem | null>(null);
  useEffect(() => { setActivitySelection(null); setSecondaryMovementSelection(null); }, [accessProfile, runtimeHost.home]);
  const [commandMenuOpen, setCommandMenuOpen] = useState(false);
  const [inspectorOpen, setInspectorOpen] = useState(true);
  const [secondaryInspectorOpen, setSecondaryInspectorOpen] = useState(false);
  const [dashboardPinnedSurfaceIds, setDashboardPinnedSurfaceIds] = useState(
    () => readStoredStringList(DASHBOARD_PINNED_SURFACES_STORAGE_KEY, accessProfile),
  );
  const [agentPinnedChatIds, setAgentPinnedChatIds] = useState(
    () => readStoredStringList(PINNED_TASKS_STORAGE_KEY, accessProfile, defaultPinnedTaskIds(accessProfile)),
  );
  const [agentChatOrderIds, setAgentChatOrderIds] = useState(
    () => readStoredStringList(TASK_ORDER_STORAGE_KEY, accessProfile),
  );

  useEffect(() => {
    const dashboardPinnedKey = profileStorageKey(DASHBOARD_PINNED_SURFACES_STORAGE_KEY, accessProfile);
    const pinnedTasksKey = profileStorageKey(PINNED_TASKS_STORAGE_KEY, accessProfile);
    const taskOrderKey = profileStorageKey(TASK_ORDER_STORAGE_KEY, accessProfile);
    const projectFilterKey = profileStorageKey(PROJECT_FILTER_STORAGE_KEY, accessProfile);
    const updateList = (setter: typeof setAgentChatOrderIds, storageKey: string) => {
      setter((current) => {
        const next = readStoredStringList(storageKey, accessProfile);
        return sameStringList(current, next) ? current : next;
      });
    };
    const onStorage = (event: StorageEvent) => {
      if (event.storageArea !== window.localStorage || !event.key) return;
      if (event.key === dashboardPinnedKey) updateList(setDashboardPinnedSurfaceIds, DASHBOARD_PINNED_SURFACES_STORAGE_KEY);
      else if (event.key === pinnedTasksKey) updateList(setAgentPinnedChatIds, PINNED_TASKS_STORAGE_KEY);
      else if (event.key === taskOrderKey) updateList(setAgentChatOrderIds, TASK_ORDER_STORAGE_KEY);
      else if (event.key === projectFilterKey) setProjectFilterEeIdState(readClientProjectFilterEeId(accessProfile).trim());
    };
    window.addEventListener("storage", onStorage);
    return () => window.removeEventListener("storage", onStorage);
  }, [accessProfile]);

  useEffect(() => {
    const surfacesById = new Map(visibleSurfaces.map((surface) => [surface.id, surface]));
    const nextTabs = secondaryTabs.filter((tab) => {
      const target = tab.target;
      if (target.kind === "client_browser" || target.kind === "terminal" || target.kind === "files" || target.kind === "task_sources" || target.kind === "launcher") return true;
      if (target.kind === "worker_environment") return topology.projection.mode !== "live"
        || Boolean(resolveWorkerEnvironment(topology, target.target.workerEnvironmentId));
      const surface = surfacesById.get(target.surfaceId);
      if (!surface) return surfaceRegistry.loading;
      if (!surfaceBelongsToWorkspace(surface, target.workspaceKey)) return false;
      if (target.surfaceId !== "chat") return true;
      return !target.runId
        || !chatWorkspace.runsLoaded
        || chatWorkspace.runs.some((run) => run.runId === target.runId);
    });
    if (nextTabs.length === secondaryTabs.length) return;
    secondaryTabModel.reconcileTabs(nextTabs);
  }, [chatWorkspace.runs, chatWorkspace.runsLoaded, secondaryTabModel.reconcileTabs, secondaryTabs, surfaceRegistry.loading, topology, visibleSurfaces]);

  useEffect(() => {
    if (!homeRunId.trim()) return;
    if ([...pendingAcceptedPaneTransfersRef.current.values()].some((pending) => pending.owner.key === paneStateKey)) return;
    try {
      writeSecondaryPaneState(accessProfile, homeRunId, clientWindow.scope, workspacePaneState, paneScope);
      if (clientWindowInitialTarget) window.sessionStorage.setItem(initialTargetConsumptionKey, "true");
    } catch (error) {
      appToast.warning(`Could not save pane tabs: ${error instanceof Error ? error.message : String(error)}`);
    }
  }, [accessProfile, clientWindow.scope, clientWindowInitialTarget, homeRunId, initialTargetConsumptionKey, paneScope, paneStateKey, workspacePaneState]);

  const effectiveSelection = useMemo(
    () => resolveSelection(selectedRef, topology),
    [selectedRef, topology],
  );
  const effectiveSecondarySelection = useMemo(
    () => resolveSelection(secondarySelectedRef, topology),
    [secondarySelectedRef, topology],
  );

  useEffect(() => {
    saveStoredStringList(DASHBOARD_PINNED_SURFACES_STORAGE_KEY, dashboardPinnedSurfaceIds, accessProfile);
  }, [accessProfile, dashboardPinnedSurfaceIds]);

  useEffect(() => {
    saveStoredStringList(PINNED_TASKS_STORAGE_KEY, agentPinnedChatIds, accessProfile);
  }, [accessProfile, agentPinnedChatIds]);

  useEffect(() => {
    saveStoredStringList(TASK_ORDER_STORAGE_KEY, agentChatOrderIds, accessProfile);
  }, [accessProfile, agentChatOrderIds]);

  const selectDestination = useCallback((destination: ClientDestination) => {
    setSurfaceNavigation((current) => {
      if (sameClientDestination(current.entries[current.index], destination)) return current;
      const entries = [...current.entries.slice(0, current.index + 1), destination].slice(-50);
      return { entries, index: entries.length - 1 };
    });
    setQuery("");
    setSearchFocusTick(0);
  }, []);

  const setSettingsOpen = useCallback((open: boolean) => {
    if (!open) setSettingsRecordingId("");
    setSurfaceNavigation((current) => {
      const destination = current.entries[current.index];
      if (open) {
        if (destination.settingsSection) return current;
        const next = { ...destination, settingsSection: normalizeSettingsSection(clientPreferences.settingsSection) };
        const entries = [...current.entries.slice(0, current.index + 1), next].slice(-50);
        return { entries, index: entries.length - 1 };
      }
      if (!destination.settingsSection) return current;
      const previous = current.entries[current.index - 1];
      if (previous && !previous.settingsSection) return { ...current, index: current.index - 1 };
      const entries = current.entries.map((entry, index) => (
        index === current.index ? clientDestination(destination.surfaceId, destination.runId) : entry
      ));
      return { ...current, entries };
    });
    setClientPreferences({ settingsSearch: "" });
  }, [clientPreferences.settingsSection, setClientPreferences]);

  const selectSettingsSection = useCallback((section: string) => {
    const settingsSection = normalizeSettingsSection(section);
    setClientPreferences({ settingsSection });
    setSurfaceNavigation((current) => {
      const destination = current.entries[current.index];
      if (!destination.settingsSection || destination.settingsSection === settingsSection) return current;
      const entries = current.entries.map((entry, index) => (
        index === current.index ? { ...destination, settingsSection } : entry
      ));
      return { ...current, entries };
    });
  }, [setClientPreferences]);

  const selectSurface = useCallback((surfaceId: string, exactRunId?: string) => {
    if (!visibleSurfaces.some((surface) => surface.id === surfaceId)) {
      appToast.warning("This surface is unavailable.");
      return;
    }
    selectDestination(clientDestination(surfaceId, exactRunId ?? chatWorkspace.activeRunId));
  }, [chatWorkspace.activeRunId, selectDestination, visibleSurfaces]);

  const navigateSurfaceHistory = useCallback((delta: -1 | 1) => {
    const index = Math.max(0, Math.min(surfaceNavigation.entries.length - 1, surfaceNavigation.index + delta));
    if (index === surfaceNavigation.index) return;
    const destination = surfaceNavigation.entries[index];
    const current = surfaceNavigation.entries[surfaceNavigation.index];
    if (destination.surfaceId !== current.surfaceId || destination.runId !== current.runId) {
      if (destination.surfaceId === "chat") chatWorkspace.selectRun(destination.runId ?? "");
    }
    setSurfaceNavigation((current) => ({ ...current, index }));
    setQuery("");
    setSearchFocusTick(0);
  }, [chatWorkspace, surfaceNavigation]);

  useEffect(() => {
    if (activeDestination.surfaceId !== "chat" || activeDestination.runId || !chatWorkspace.activeRunId) return;
    setSurfaceNavigation((current) => {
      const destination = current.entries[current.index];
      if (destination?.surfaceId !== "chat" || destination.runId) return current;
      const entries = current.entries.map((entry, index) => (
        index === current.index ? { ...entry, runId: chatWorkspace.activeRunId } : entry
      ));
      return { ...current, entries };
    });
  }, [activeDestination.runId, activeDestination.surfaceId, chatWorkspace.activeRunId]);

  useEffect(() => {
    if (surfaceRegistry.loading) return;
    const chatRunIds = chatWorkspace.loadingRuns ? undefined : new Set([
      ...chatWorkspace.runs.map((run) => run.runId),
      chatWorkspace.activeRunId,
    ].filter(Boolean));
    setSurfaceNavigation((current) => normalizeSurfaceNavigation(current, visibleSurfaces, chatRunIds));
  }, [chatWorkspace.activeRunId, chatWorkspace.loadingRuns, chatWorkspace.runs, surfaceRegistry.loading, visibleSurfaces]);

  useEffect(() => {
    if (surfaceRegistry.loading || !activeSurface) return;
    windowStateStorage.setItem(clientWindowStateKey(clientWindow.scope, activeDestinationStorageKey(accessProfile)), activeSurface.id);
  }, [accessProfile, activeSurface?.id, clientWindow.scope, surfaceRegistry.loading, windowStateStorage]);

  const activateSecondaryTab = useCallback((tab: SecondaryPaneTabState) => {
    setActiveSecondaryTabId(tab.id);
    if (activePaneStateKeyRef.current !== paneStateKey) return;
    activatedSecondaryTabIdRef.current = JSON.stringify([paneStateKey, tab.id]);
    setSecondaryQuery("");
    setSecondarySearchFocusTick(0);
    if (tab.target.kind !== "surface") return;
    if (tab.target.surfaceId === "chat") {
      const secondary = taskWorkspaceNavigationRef.current.secondary;
      if (tab.target.runId) {
        newSecondaryTaskPhaseRef.current = "idle";
        secondary.selectRun(tab.target.runId);
      } else {
        newSecondaryTaskPhaseRef.current = secondary.activeRunId ? "awaiting_empty" : "awaiting_run";
        secondary.createChat();
      }
    } else {
      newSecondaryTaskPhaseRef.current = "idle";
    }
  }, [paneStateKey, setActiveSecondaryTabId]);

  const openSecondaryTab = useCallback((tab: SecondaryPaneTabState) => {
    secondaryTabModel.openTab(tab, activeSecondaryTab?.target.kind === "launcher" && tab.target.kind !== "launcher" ? activeSecondaryTab.id : undefined);
    activateSecondaryTab(tab);
    setSecondaryPaneOpen(true);
    setActivePane("secondary");
  }, [activateSecondaryTab, activeSecondaryTab, secondaryTabModel.openTab, setActivePane, setSecondaryPaneOpen]);

  useEffect(() => {
    const tab = secondaryTabs.find((candidate) => candidate.id === activeSecondaryTabId) ?? secondaryTabs[0];
    if (!tab) return;
    if (!restoreSecondarySelectionRef.current && activatedSecondaryTabIdRef.current === JSON.stringify([paneStateKey, tab.id])) return;
    restoreSecondarySelectionRef.current = false;
    activateSecondaryTab(tab);
  }, [activateSecondaryTab, activeSecondaryTabId, paneStateKey, secondaryTabs]);

  const openSecondaryChatTab = useCallback((runId: string) => {
    openSecondaryTab(secondaryPaneTab("octopus-os", "chat", runId));
  }, [openSecondaryTab]);

  const openTerminalTab = useCallback(() => {
    if (!activeClientSurfaceIdentity.ready) {
      appToast.warning("Wait for the selected task before opening its terminal.");
      return;
    }
    const selected = activeClientSurfaceIdentity.pane === "secondary" ? activeSecondaryTab?.target : null;
    const target = selected?.kind === "launcher" ? selected.source : selected;
    let cwd: string | undefined;
    try {
      cwd = target?.kind === "terminal"
        ? target.cwd
        : terminalLaunchCwd(topology, activeClientSurfaceIdentity.runId, target?.kind === "worker_environment" ? target.target : undefined);
    } catch (error) {
      appToast.warning(error instanceof Error ? error.message : String(error));
      return;
    }
    const terminalId = `user-${crypto.randomUUID()}`;
    openSecondaryTab({
      id: `terminal:${terminalId}`,
      target: { kind: "terminal", terminalId, title: "", workspaceKey: activeClientSurfaceIdentity.workspaceKey, ...(cwd ? { cwd } : {}) },
    });
  }, [activeClientSurfaceIdentity, activeSecondaryTab, openSecondaryTab, topology]);

  const openFilesTab = useCallback((target?: FilesOpenTarget, workspaceKey = activeClientSurfaceIdentity.workspaceKey) => {
    openSecondaryTab(workspacePaneTabForTarget({
      kind: "files",
      workspaceKey,
      ...(target ? { openTarget: target } : {}),
    }));
  }, [activeClientSurfaceIdentity.workspaceKey, openSecondaryTab]);

  const openTaskSourcesPane = useCallback((target: TaskSourcesPaneTarget) => {
    openSecondaryTab(workspaceTaskSourcesPaneTab("octopus-os", target));
  }, [openSecondaryTab]);

  const updateTerminalTitle = useCallback((terminalId: string, titleValue: string) => {
    const title = titleValue.trim().replace(/[\u0000-\u001f\u007f]/g, "").slice(0, 120);
    if (!title) return;
    setSecondaryTabs((current) => current.map((tab) => (
      tab.target.kind === "terminal" && tab.target.terminalId === terminalId
        ? { ...tab, target: { ...tab.target, title } }
        : tab
    )));
  }, [setSecondaryTabs]);

  const openWorkerEnvironmentPane = useCallback((raw: Omit<WorkerEnvironmentPaneTarget, "schemaVersion">) => {
    const resolved = resolveWorkerEnvironment(topology, raw.workerEnvironmentId);
    const document = raw.clientSurface
      ? resolvePaneClientSurface(surfaceRegistry.workerEnvironmentSurfaces, { schemaVersion: 1, ...raw })
      : null;
    const surface = resolved && !raw.filePath && !raw.clientSurface ? resolvePaneSurface(resolved.workerEnvironment, raw) : null;
    const target: WorkerEnvironmentPaneTarget = {
      schemaVersion: 1,
      workerEnvironmentId: raw.workerEnvironmentId,
      surfaceId: document && document.document.target.kind === "worker_environment.surface"
        ? document.document.target.surface_id
        : surface?.id ?? raw.surfaceId,
      panelKind: raw.filePath ? "file" : document ? "custom" : surface ? workerEnvironmentPanelKind(surface) : raw.panelKind,
      filePath: raw.filePath,
      clientSurface: raw.clientSurface,
    };
    openSecondaryTab(workspacePaneTabForTarget({
      kind: "worker_environment",
      target,
      workspaceKey: activeClientSurfaceIdentity.workspaceKey,
    }));
  }, [activeClientSurfaceIdentity.workspaceKey, openSecondaryTab, surfaceRegistry.workerEnvironmentSurfaces, topology]);

  const clientBrowserPaneOwnerRef = useRef({ open: openSecondaryTab, workspaceKey: activeClientSurfaceIdentity.workspaceKey });
  clientBrowserPaneOwnerRef.current = { open: openSecondaryTab, workspaceKey: activeClientSurfaceIdentity.workspaceKey };
  useEffect(() => {
    let disposed = false;
    const bridge = window.octopusTransport;
    const show = (page: ClientBrowserState, owner = clientBrowserPaneOwnerRef.current) => {
      if (disposed) { void bridge?.clientBrowser({ action: "close", id: page.id }); return; }
      owner.open({ id: `browser:${page.id}`, target: { kind: "client_browser", pageId: page.id,
        title: page.title, workspaceKey: owner.workspaceKey } });
    };
    const open = (event: Event) => {
      if (!(event instanceof CustomEvent) || typeof event.detail?.url !== "string") return;
      event.preventDefault();
      if (accessProfile !== "live" || !bridge?.clientBrowser) { appToast.danger("The in-app browser is unavailable."); return; }
      const owner = clientBrowserPaneOwnerRef.current;
      void bridge.clientBrowser({ action: "open", url: event.detail.url }).then(page => { if (page) show(page, owner); })
        .catch(error => appToast.danger(error instanceof Error ? error.message : String(error)));
    };
    const stop = bridge?.onClientBrowserOpened((page) => show(page));
    const stopState = bridge?.onClientBrowserState((page) => {
      for (const [key, state] of Object.entries(getWorkspacePaneStates())) {
        if (!state.tabs.some((tab) => tab.target.kind === "client_browser" && tab.target.pageId === page.id && tab.target.title !== page.title)) continue;
        workspacePane.setStateAt(key, (current) => ({ ...current, tabs: current.tabs.map((tab) =>
          tab.target.kind === "client_browser" && tab.target.pageId === page.id ? { ...tab, target: { ...tab.target, title: page.title } } : tab) }));
      }
    });
    window.addEventListener(OPEN_WEB_LINK_EVENT, open);
    return () => { disposed = true; window.removeEventListener(OPEN_WEB_LINK_EVENT, open); stop?.(); stopState?.(); };
  }, [accessProfile, runtimeHost.home, getWorkspacePaneStates, workspacePane.setStateAt]);

  const secondaryPaneTabs = useMemo<WorkspacePaneTab[]>(() => secondaryTabs.map((tab) => {
    const target = tab.target;
    if (target.kind === "client_browser") return { glyph: "browser" as const, id: tab.id, showWhenSingle: true, title: target.title || "Browser" };
    if (target.kind === "launcher") {
      return { glyph: "browser" as const, id: tab.id, launcher: true, showWhenSingle: true, title: "New tab" };
    }
    if (target.kind === "files") {
      return { glyph: "folder" as const, id: tab.id, showWhenSingle: true, title: "Files" };
    }
    if (target.kind === "terminal") {
      const terminals = secondaryTabs.filter((candidate) => candidate.target.kind === "terminal");
      const terminalIndex = terminals.findIndex((candidate) => candidate.id === tab.id);
      const fallback = terminals.length > 1 ? `Terminal ${terminalIndex + 1}` : "Terminal";
      return { glyph: "terminal" as const, id: tab.id, showWhenSingle: true, title: target.title || fallback };
    }
    if (target.kind === "task_sources") {
      return { glyph: "viewAll" as const, id: tab.id, showWhenSingle: true, title: "Sources" };
    }
    if (target.kind === "worker_environment") {
      const resolved = resolveWorkerEnvironment(topology, target.target.workerEnvironmentId);
      if (!resolved) return { glyph: "ee" as const, id: tab.id, showWhenSingle: true, title: "Worker Environment" };
      const named = paneTargetTitle(
        resolved.workerEnvironment,
        target.target,
        resolvePaneClientSurface(surfaceRegistry.workerEnvironmentSurfaces, target.target),
      );
      return { glyph: named.glyph, id: tab.id, showWhenSingle: true, title: named.title };
    }
    const surface = visibleSurfaces.find((candidate) => candidate.id === target.surfaceId);
    const activeRunIdentityPending = tab.id === activeSecondaryTabId
      && target.surfaceId === "chat"
      && !secondaryRunIdentityReady;
    const run = !activeRunIdentityPending && target.runId
      ? secondaryChatWorkspace.runs.find((candidate) => candidate.runId === target.runId)
      : null;
    return {
      glyph: target.surfaceId === "chat" ? "chat" : surface?.glyph ?? "viewAll",
      id: tab.id,
      showWhenSingle: true,
      title: target.surfaceId === "chat"
        ? activeRunIdentityPending ? "Opening task"
          : target.runId ? clientTaskTitle(target.runId, run?.title ?? "", taskOrganization) : "New task"
        : surface?.label || target.surfaceId,
    };
  }), [activeSecondaryTabId, secondaryChatWorkspace.runs, secondaryRunIdentityReady, secondaryTabs, surfaceRegistry.workerEnvironmentSurfaces, taskOrganization, topology, visibleSurfaces]);

  const paneShortcut = useCallback((command: ClientCommandId) => {
    const bindings = keyboardShortcuts[command] ?? [];
    return bindings.length ? formatKeyboardShortcut(bindings[0]) : undefined;
  }, [keyboardShortcuts]);

  const workerEnvironmentActionRunIds = useMemo(() => {
    if (!activeClientSurfaceIdentity.ready) return [];
    if (activeClientSurfaceIdentity.pane === "secondary") {
      const target = activeSecondaryTab?.target;
      if (target?.kind === "worker_environment") {
        const resolved = resolveWorkerEnvironment(topology, target.target.workerEnvironmentId);
        const runId = resolved?.run?.id || resolved?.workerEnvironment.runId;
        return runId ? [runId] : [];
      }
      if (target?.kind === "task_sources") return [target.target.runId];
      if (target?.kind === "launcher") return target.runId ? [target.runId] : [];
      if (target?.kind === "surface") {
        const runId = target.surfaceId === "voice" ? homeRunId : activeClientSurfaceIdentity.runId;
        return runId ? [runId] : [];
      }
    }
    const runId = primarySurfaceTarget.surfaceId === "voice" ? homeRunId
      : primaryRunIdentityReady ? primarySurfaceTarget.runId : "";
    return runId ? [runId] : [];
  }, [activeClientSurfaceIdentity, activeSecondaryTab, homeRunId, primaryRunIdentityReady, primarySurfaceTarget, topology]);

  const workerEnvironmentActions = useMemo(
    () => workerEnvironmentPaneActions(topology, workerEnvironmentActionRunIds, surfaceRegistry.workerEnvironmentSurfaces),
    [surfaceRegistry.workerEnvironmentSurfaces, topology, workerEnvironmentActionRunIds],
  );

  const openLauncherTab = useCallback(() => {
    const target = activeClientSurfaceIdentity.pane === "secondary" ? activeSecondaryTab?.target : undefined;
    const source = target?.kind === "launcher" ? target.source
      : target?.kind === "terminal" ? { kind: "terminal" as const, cwd: target.cwd }
        : target?.kind === "worker_environment" ? { kind: "worker_environment" as const, target: target.target }
          : undefined;
    openSecondaryTab({
      id: `launcher:${crypto.randomUUID()}`,
      target: { kind: "launcher", runId: workerEnvironmentActionRunIds[0] ?? "", workspaceKey: activeClientSurfaceIdentity.workspaceKey, ...(source ? { source } : {}) },
    });
  }, [activeClientSurfaceIdentity, activeSecondaryTab, openSecondaryTab, workerEnvironmentActionRunIds]);

  useEffect(() => {
    const scope = JSON.stringify([accessProfile, runtimeHost.home]);
    if (!runtimeHost.ready || !runtimeHost.home || topology.projection.mode !== "live"
      || topology.projection.endpoint !== runtimeHost.home) return;
    if (presentedBrowserTargetsRef.current.scope !== scope) {
      presentedBrowserTargetsRef.current = {
        scope,
        baseline: new Map(activeWorkerEnvironmentPaneActivities(topology).filter((activity) => !activity.running).map((activity) => [activity.presentationId, activity.sequence])),
        identities: new Set(),
      };
    }
    if (!activeClientSurfaceIdentity.ready || nativeWindowCloseRequestRef.current || pendingAcceptedPaneTransfersRef.current.size
      || preparedSourceTransfersRef.current.size || sourceHandoffPending || sourceFinalizationPendingCount) return;
    const activities = activeWorkerEnvironmentPaneActivities(topology, workerEnvironmentActionRunIds);
    const unseen = activities.filter((activity) => !presentedBrowserTargetsRef.current.identities.has(activity.presentationId)
      && activity.sequence > (presentedBrowserTargetsRef.current.baseline.get(activity.presentationId) ?? 0));
    if (!unseen.length) return;
    const tabs = unseen.map((activity) => workspacePaneTabForTarget({
      kind: "worker_environment",
      target: { schemaVersion: 1, ...activity.target },
      workspaceKey: activeClientSurfaceIdentity.workspaceKey,
    }));
    if (activeClientSurfaceIdentity.pane === "secondary" && activeSecondaryTab?.target.kind === "surface"
      && ["chat", "voice"].includes(activeSecondaryTab.target.surfaceId) && workerEnvironmentActionRunIds.length === 1) {
      const key = JSON.stringify([accessProfile, runtimeHost.home, workerEnvironmentActionRunIds[0]]);
      setTaskBrowserTabs((current) => {
        const previous = current[key] ?? emptyTaskBrowserTabs;
        const additions = unseen.map((activity) => ({ id: activity.identity, target: { schemaVersion: 1 as const, ...activity.target } }));
        const known = new Set(previous.tabs.map((tab) => tab.id));
        const selected = unseen.filter((activity) => activity.activate).at(-1);
        return { ...current, [key]: { activeTabId: selected?.identity ?? previous.activeTabId, open: Boolean(selected) || previous.open,
          tabs: [...previous.tabs, ...additions.filter((tab) => { if (known.has(tab.id)) return false; known.add(tab.id); return true; })] } };
      });
      for (const activity of unseen) presentedBrowserTargetsRef.current.identities.add(activity.presentationId);
      if (unseen.some((activity) => activity.activate)) setSecondaryPaneFocused(true);
      return;
    }
    setSecondaryTabs((current) => {
      const known = new Set(current.map((tab) => tab.id));
      const additions = tabs.filter((tab) => {
        if (known.has(tab.id)) return false;
        known.add(tab.id);
        return true;
      });
      return additions.length ? [...current, ...additions] : current;
    });
    for (const activity of unseen) presentedBrowserTargetsRef.current.identities.add(activity.presentationId);
    const selected = unseen.findLastIndex((activity) => activity.activate);
    if (activeClientSurfaceIdentity.pane === "primary" && selected >= 0) {
      activateSecondaryTab(tabs[selected]);
      setSecondaryPaneOpen(true);
    }
  }, [accessProfile, activateSecondaryTab, activeClientSurfaceIdentity, activeSecondaryTab, browserPresentationGeneration, runtimeHost.home, runtimeHost.ready, setSecondaryPaneFocused, setSecondaryPaneOpen, setSecondaryTabs, sourceFinalizationPendingCount, sourceHandoffPending, topology, workerEnvironmentActionRunIds]);

  const secondaryPaneAddActions = useMemo<WorkspacePaneAddAction[]>(() => [
    { glyph: "terminal" as const, id: "operator:terminal", section: "workspace", shortcut: paneShortcut("open_terminal"), title: "Terminal" },
    { glyph: "folder" as const, id: "operator:files", section: "workspace", title: "Files" },
    ...workerEnvironmentActions.map((action) => ({
      description: action.description,
      glyph: action.glyph,
      group: action.panelKind === "files" ? { id: "task-files", title: "Task files", glyph: "folder" as const }
        : ["connection", "runtime", "container", "worktree", "overview"].includes(action.panelKind)
          ? { id: "environment-details", title: "Environment details", glyph: "ee" as const } : undefined,
      id: `we:${action.id}`,
      moreOnly: ["connection", "runtime", "container", "worktree", "overview", "custom"].includes(action.panelKind),
      section: "environment",
      shortcut: action.panelKind === "browser" ? paneShortcut("open_browser")
        : action.panelKind === "changes" ? paneShortcut("open_changes")
          : undefined,
      title: action.title,
    })),
    ...(workerEnvironmentActionRunIds.length ? [{ glyph: "chat" as const, id: "chat:side", section: "tasks", shortcut: paneShortcut("open_side_task"), title: "Side task" }] : []),
    { glyph: "newTask" as const, id: "chat:new", section: "tasks", title: "New task" },
    ...(["dashboard", "octopus-os"] as const).flatMap((workspaceKey) => visibleSurfaces
      .filter((surface) => (
        surface.id !== "chat"
        && !isSidebarOnlySurface(surface)
        && surfaceBelongsToWorkspace(surface, workspaceKey)
      ))
      .sort((left, right) => left.order - right.order || left.label.localeCompare(right.label))
      .map((surface) => ({
        glyph: surface.glyph,
        id: `surface:${workspaceKey}:${surface.id}`,
        moreOnly: true,
        section: navigationSectionLabel(surface),
        title: surface.label,
      }))),
  ], [paneShortcut, visibleSurfaces, workerEnvironmentActionRunIds, workerEnvironmentActions]);

  useEffect(() => {
    const activeTab = secondaryTabs.find((tab) => tab.id === activeSecondaryTabId);
    const runId = secondaryChatWorkspace.activeRunId;
    if (!activeTab || activeTab.target.kind !== "surface" || activeTab.target.surfaceId !== "chat") {
      newSecondaryTaskPhaseRef.current = "idle";
      return;
    }
    if (activeTab.target.runId) {
      newSecondaryTaskPhaseRef.current = "idle";
      return;
    }
    if (newSecondaryTaskPhaseRef.current === "awaiting_empty") {
      if (!runId) newSecondaryTaskPhaseRef.current = "awaiting_run";
      return;
    }
    if (newSecondaryTaskPhaseRef.current !== "awaiting_run" || !runId) return;
    const completedTab = secondaryPaneTab(activeTab.target.workspaceKey, activeTab.target.surfaceId, runId);
    newSecondaryTaskPhaseRef.current = "idle";
    secondaryTabModel.replaceTab(activeTab.id, completedTab);
  }, [activeSecondaryTabId, secondaryChatWorkspace.activeRunId, secondaryTabs]);

  const openChat = useCallback((runId: string, newTaskProjectId?: string) => {
    if (runId && projectFilterEeId) {
      const run = chatWorkspace.runs.find((candidate) => candidate.runId === runId);
      if (!run || !clientProjectScopeIds(agentProjectWorkspace.projects, projectFilterEeId).has(run.eeId)) setProjectFilterEeId("");
    }
    if (runId) chatWorkspace.selectRun(runId);
    else chatWorkspace.createChat(newTaskProjectId);
    selectSurface("chat", runId);
  }, [agentProjectWorkspace.projects, chatWorkspace, projectFilterEeId, selectSurface, setProjectFilterEeId]);

  function workspaceWithTaskDraftActions(source: typeof chatWorkspace) {
    return { ...source, ...taskDraftActions(source, chatWorkspace, () => {
      setProjectFilterEeId("");
      selectSurface("chat", "");
    }) };
  }

  const openVoice = useCallback(() => {
    const callHomeRunId = isVoiceCallActive(voiceSession.phase)
      ? voiceSession.context?.homeRunId
      : "";
    const targetRunId = callHomeRunId || homeRunId;
    if (targetRunId && targetRunId !== homeChatWorkspace.activeRunId) {
      homeChatWorkspace.selectRun(targetRunId);
    }
    selectSurface("voice");
  }, [homeChatWorkspace, homeRunId, selectSurface, voiceSession.context?.homeRunId, voiceSession.phase]);

  const reportedVoiceFailure = useRef("");
  const homeCallControlsVisible = !settingsOpen && (
    (activeSurface?.id === "voice" && homeViewMode === "voice")
    || (secondaryPaneOpen && secondarySurfaceId === "voice")
  );
  useEffect(() => {
    const failure = voiceSession.error || voiceSession.deviceError;
    if (reportedVoiceFailure.current === failure) return;
    reportedVoiceFailure.current = failure;
    if (!failure || homeCallControlsVisible) return;
    appToast.danger(voiceSession.error ? "Voice call failed." : "Check your audio devices.", 5, {
      action: {
        label: "Open Home",
        onSelect: () => { openVoice(); setHomeViewMode("voice"); },
      },
    });
  }, [homeCallControlsVisible, openVoice, voiceSession.deviceError, voiceSession.error]);

  const openHomeRun = useCallback((runId: string) => {
    if (!runId) return;
    homeChatWorkspace.selectRun(runId);
    selectSurface("voice");
  }, [homeChatWorkspace, selectSurface]);

  const openSidebarSurface = useCallback((surfaceId: string) => {
    selectSurface(surfaceId);
  }, [selectSurface]);

  const openSidebarChat = useCallback((runId: string) => {
    openChat(runId);
  }, [openChat]);

  const openSurfaceRun = useCallback((runId: string, role: "home" | "task") => {
    if (role === "home") openHomeRun(runId);
    else openChat(runId);
  }, [openChat, openHomeRun]);

  const openSecondarySurfaceRun = useCallback((runId: string, role: "home" | "task") => {
    if (role === "home") {
      homeChatWorkspace.selectRun(runId);
      openSecondaryTab(secondaryPaneTab("octopus-os", "voice", ""));
    } else openSecondaryChatTab(runId);
  }, [homeChatWorkspace, openSecondaryChatTab, openSecondaryTab]);

  const openNavigationSection = useCallback((section: ClientNavigationSection) => {
    if (section === "home") {
      openVoice();
      setHomeViewMode("voice");
      return;
    }
    if (section === "tasks") {
      const remembered = lastTasksDestinationRef.current;
      const previous = remembered && visibleSurfaces.some((surface) => surface.id === remembered.surfaceId)
        ? remembered : [...surfaceNavigation.entries].reverse().find((destination) => {
        const surface = visibleSurfaces.find((candidate) => candidate.id === destination.surfaceId);
        return surface && navigationSectionForSurface(surface) === "tasks";
      });
      if (previous && previous.surfaceId !== "chat") {
        selectSurface(previous.surfaceId);
      } else {
        const runId = previous ? previous.runId ?? "" : chatWorkspace.activeRunId;
        if (runId && chatWorkspace.runsLoaded && !taskChatRuns.some((run) => run.runId === runId)) {
          appToast.warning("This task is no longer available. Select another task or create one with the plus button.");
          return;
        }
        chatWorkspace.selectRun(runId);
        selectSurface("chat", runId);
      }
      return;
    }
    const destination = clientNavigationDestinations.find((candidate) => candidate.id === section);
    if (!destination) return;
    const previous = section === "dashboard" ? [...surfaceNavigation.entries].reverse().find((candidate) => {
      const surface = visibleSurfaces.find((surface) => surface.id === candidate.surfaceId);
      return surface && navigationSectionForSurface(surface) === "dashboard";
    }) : undefined;
    selectSurface(previous?.surfaceId || destination.surfaceId);
  }, [chatWorkspace, openVoice, selectSurface, surfaceNavigation.entries, taskChatRuns, visibleSurfaces]);

  const openPrimarySurface = useCallback((surface: ClientSurfaceDescriptor, exactTarget = false) => {
    const destination = clientNavigationDestinations.find((candidate) => candidate.surfaceId === surface.id);
    if (destination && !exactTarget) {
      openNavigationSection(destination.id);
      return;
    }
    selectSurface(surface.id);
  }, [openNavigationSection, selectSurface]);

  const openExtensionDestination = useCallback((destination: ClientExtensionDestination) => {
    if (destination.kind === "settings") {
      selectSettingsSection(destination.sectionId);
      return;
    }
    if (destination.kind === "home") {
      setSettingsOpen(false);
      openVoice();
      return;
    }
    const surface = surfaceRegistry.surfaces.find((candidate) => candidate.id === destination.surfaceId);
    if (!surface) {
      appToast.warning("This extension's tab is not available yet.");
      return;
    }
    setSettingsOpen(false);
    openPrimarySurface(surface, true);
  }, [openPrimarySurface, openVoice, selectSettingsSection, setSettingsOpen, surfaceRegistry.surfaces]);

  const selectInboxItem = useCallback((item: MarkActivityItem, ids: readonly string[] = [item.id]) => {
    const current = markActivity.projectedItems.find((candidate) => candidate.id === item.id);
    if (!current) { appToast.warning("This notification is no longer available."); return; }
    setActivitySelection({ ...current });
    const group = [...new Set([current.id, ...ids])];
    setActivitySelectionGroup(group);
    const unread = group.filter((id) => !markActivity.read.has(id));
    if (unread.length && !markActivity.markRead(unread)) appToast.warning("Could not save this notification's read state.");
  }, [markActivity.projectedItems, markActivity.markRead, markActivity.read]);

  const openActivityMark = useCallback((item: MarkActivityItem) => {
    const current = markActivity.projectedItems.find((candidate) => candidate.id === item.id);
    if (!current) { appToast.warning("This activity is no longer available in the current projection."); return; }
    if (current.navigation.kind === "run") {
      if (current.navigation.clientSurfaceRole === "home") {
        homeChatWorkspace.selectRun(current.navigation.runId);
        setSecondaryHomeViewMode("activity");
        openSecondaryTab(secondaryPaneTab("octopus-os", "voice", current.navigation.runId));
      } else {
        openSecondaryChatTab(current.navigation.runId);
      }
    } else if (current.sequence === null || !Number.isSafeInteger(current.sequence) || current.sequence < 0 || current.sequence >= Number.MAX_SAFE_INTEGER) {
      appToast.warning("This mark has no addressable movement sequence to open.");
      return;
    } else {
      setSecondaryMovementSelection({ ...current });
      openSecondaryTab(secondaryPaneTab("dashboard", "event-log", ""));
    }
    setActivitySelection({ ...current });
    if (!markActivity.read.has(current.id) && !markActivity.markRead([current.id])) appToast.warning("Could not save this notification's read state.");
  }, [markActivity.projectedItems, markActivity.markRead, markActivity.read, homeChatWorkspace.selectRun, openSecondaryChatTab, openSecondaryTab]);

  useEffect(() => {
    const activate = (event: Event) => {
      if (!(event instanceof CustomEvent)) return;
      const target = event.detail as ClientNotificationTarget | undefined;
      if (target?.kind !== "mark") return;
      const item = markActivity.projectedItems.find((candidate) => candidate.id === target.movementId);
      if (!item) { appToast.warning("This activity is no longer available in the current projection."); return; }
      selectSurface("inbox");
      selectInboxItem(item);
    };
    window.addEventListener(CLIENT_NOTIFICATION_ACTIVATE_EVENT, activate);
    return () => window.removeEventListener(CLIENT_NOTIFICATION_ACTIVATE_EVENT, activate);
  }, [markActivity.projectedItems, selectInboxItem, selectSurface]);

  useEffect(() => window.octopusTransport?.onGlobalDictationRequested?.(() => {
    setSettingsOpen(false);
    setCommandMenuOpen(false);
    openChat(chatWorkspace.activeRunId);
    window.setTimeout(() => dispatchClientCommand("start_dictation", "keyboard"), 80);
  }), [chatWorkspace.activeRunId, openChat, setSettingsOpen]);

  useEffect(() => window.octopusTransport?.onOpenRunRequested?.((runId) => {
    if (typeof runId !== "string" || !runId.trim()) return;
    setSettingsOpen(false);
    setCommandMenuOpen(false);
    openChat(runId.trim());
  }), [openChat, setSettingsOpen]);

  useEffect(() => {
    const prepareReview = (event: Event) => {
      if (!(event instanceof CustomEvent)) return;
      const detail = event.detail as OperatorDiffReviewSendDetail | undefined;
      if (detail?.schemaVersion !== 1 || !detail.runId || !detail.text || !detail.context) return;
      const workspace = chatWorkspace;
      openChat(detail.runId);
      workspace.setText(`${detail.text}\n\n${detail.context}`);
      event.preventDefault();
      appToast.success(`${detail.comments.length} review ${detail.comments.length === 1 ? "comment" : "comments"} added to the task composer.`);
    };
    window.addEventListener(OCTOPUS_OPERATOR_DIFF_REVIEW_SEND_EVENT, prepareReview);
    return () => window.removeEventListener(OCTOPUS_OPERATOR_DIFF_REVIEW_SEND_EVENT, prepareReview);
  }, [chatWorkspace, openChat]);

  useEffect(() => {
    const prepareArtifact = async (event: Event) => {
      if (!(event instanceof CustomEvent)) return;
      const detail = event.detail as ArtifactContextPreparedDetail | undefined;
      if (
        detail?.schema !== "octopus.client.artifact_context"
        || detail.schemaVersion !== 1
        || !detail.artifact?.artifactId
        || !detail.prompt?.trim()
      ) return;
      const workspace = chatWorkspace;
      const targetRunId = detail.artifact.runId.trim();
      if (targetRunId) {
        try {
          await updateOperatorChatPins({
            artifactId: detail.artifact.artifactId,
            pinned: true,
            runId: targetRunId,
          });
          await workspace.refreshRuns();
        } catch (error) {
          appToast.danger(error instanceof Error ? error.message : String(error));
          return;
        }
        openChat(targetRunId);
      }
      const reference = stringifyPreparedContext({
        artifact_id: detail.artifact.artifactId,
        context_ref: detail.contextRef,
        interface_id: detail.artifact.interfaceId,
        label: detail.artifact.label,
        renderer: detail.artifact.renderer,
        run_id: detail.artifact.runId,
        ...(detail.selection ? { selection: detail.selection } : {}),
      });
      if (!reference) {
        appToast.danger("Artifact context is too large for the task composer.");
        return;
      }
      workspace.setText(`${detail.prompt.trim()}\n\nArtifact context reference:\n${reference}`);
      event.preventDefault();
      appToast.success("Artifact context added to the task composer.");
    };
    window.addEventListener(ARTIFACT_CONTEXT_PREPARED_EVENT, prepareArtifact);
    return () => window.removeEventListener(ARTIFACT_CONTEXT_PREPARED_EVENT, prepareArtifact);
  }, [chatWorkspace, openChat]);

  useEffect(() => {
    const prepareMcpAction = (event: Event) => {
      if (!(event instanceof CustomEvent)) return;
      const detail = event.detail as McpResourceActionDetail | undefined;
      if (
        detail?.schemaVersion !== 1
        || !detail.action?.trim()
        || !detail.provenance?.runId?.trim()
        || !detail.provenance.interfaceId?.trim()
        || !detail.provenance.resourceUri?.trim()
      ) return;
      const workspace = chatWorkspace;
      openChat(detail.provenance.runId.trim());
      const action = stringifyPreparedContext({
        action: detail.action,
        payload: detail.payload,
        provenance: detail.provenance,
        schema: "octopus.client.mcp_resource_action",
        schema_version: 1,
      });
      if (!action) {
        appToast.danger("MCP action is too large for the task composer.");
        return;
      }
      workspace.setText(`Continue this task using the selected MCP resource action.\n\n${action}`);
      event.preventDefault();
      appToast.success("MCP action added to the task composer.");
    };
    window.addEventListener(MCP_RESOURCE_ACTION_EVENT, prepareMcpAction);
    return () => window.removeEventListener(MCP_RESOURCE_ACTION_EVENT, prepareMcpAction);
  }, [chatWorkspace, openChat]);

  useEffect(() => window.octopusTransport?.onOpenWorkerEnvironmentRequested?.((target) => {
    if (!target?.workerEnvironmentId) return;
    setSettingsOpen(false);
    setCommandMenuOpen(false);
    openWorkerEnvironmentPane({
      workerEnvironmentId: target.workerEnvironmentId,
      panelKind: target.panelKind === "overview" ? undefined : target.panelKind,
      surfaceId: target.surfaceId,
      filePath: target.filePath,
      clientSurface: target.clientSurface,
    });
  }), [openWorkerEnvironmentPane, setSettingsOpen]);

  useEffect(() => {
    const openPane = (event: Event) => {
      if (!(event instanceof CustomEvent)) return;
      const detail = event.detail as WorkerEnvironmentPaneTarget | undefined;
      if (detail?.schemaVersion !== 1 || !detail.workerEnvironmentId) return;
      setSettingsOpen(false);
      setCommandMenuOpen(false);
      openWorkerEnvironmentPane(detail);
    };
    window.addEventListener(OPEN_WORKER_ENVIRONMENT_PANE_EVENT, openPane);
    return () => window.removeEventListener(OPEN_WORKER_ENVIRONMENT_PANE_EVENT, openPane);
  }, [openWorkerEnvironmentPane, setSettingsOpen]);

  useEffect(() => {
    const openPane = (event: Event) => {
      if (!(event instanceof CustomEvent)) return;
      const detail = event.detail as TaskSourcesPaneTarget | undefined;
      if (detail?.schemaVersion !== 1 || !detail.runId.trim() || !Array.isArray(detail.sources)) return;
      setSettingsOpen(false);
      setCommandMenuOpen(false);
      openTaskSourcesPane(detail);
    };
    window.addEventListener(OPEN_TASK_SOURCES_PANE_EVENT, openPane);
    return () => window.removeEventListener(OPEN_TASK_SOURCES_PANE_EVENT, openPane);
  }, [openTaskSourcesPane, setSettingsOpen]);

  const handleSelect = useCallback((ref: SelectionRef | null) => {
    setSelectedRef(ref);
    if (ref) setInspectorOpen(true);
  }, []);

  const handleSecondarySelect = useCallback((ref: SelectionRef | null) => {
    setSecondarySelectedRef(ref);
    if (ref) setSecondaryInspectorOpen(true);
  }, []);

  const selectSecondaryTab = useCallback((tabId: string) => {
    const tab = secondaryTabs.find((candidate) => candidate.id === tabId);
    if (!tab) return;
    activateSecondaryTab(tab);
    setActivePane("secondary");
  }, [activateSecondaryTab, secondaryTabs, setActivePane]);

  const secondaryClosePendingRef = useRef(false);
  const closeSecondaryTabSet = useCallback(async (tabIds: string[], focusTabId?: string) => {
    if (secondaryClosePendingRef.current) return;
    secondaryClosePendingRef.current = true;
    try {
      const requested = new Set(tabIds);
      const closing = transferSourceStateRef.current.secondaryTabs.filter((tab) => requested.has(tab.id));
      const fileKeys = closing.filter((tab) => tab.target.kind === "files").map((tab) => filesPaneKey(accessProfile, tab.id, paneStateKey));
      if (!await requestCloseFilesPanes(fileKeys)) return;
      for (const key of fileKeys) {
        const snapshot = captureFilesPane(key);
        if (snapshot && (filesSnapshotDirty(snapshot) || readFilesPane(key).transferPending)) return;
      }
      let state = transferSourceStateRef.current;
      if (closing.some((tab) => state.secondaryTabs.find((current) => current.id === tab.id) !== tab)) return;
      for (const tab of closing) {
        if (tab.target.kind === "client_browser") { await window.octopusTransport?.clientBrowser({ action: "close", id: tab.target.pageId }); continue; }
        if (tab.target.kind !== "worker_environment") continue;
        const room = resolveWorkerEnvironment(topology, tab.target.target.workerEnvironmentId);
        const surface = room && resolvePaneSurface(room.workerEnvironment, tab.target.target);
        if (!isControllableNativeBrowserSurface(surface)) continue;
        if (!window.octopusTransport?.closeNativeBrowserPage) throw new Error("The native browser is not connected.");
        await window.octopusTransport.closeNativeBrowserPage({ home: topology.projection.endpoint, workerEnvironmentId: tab.target.target.workerEnvironmentId, surfaceId: surface.id });
      }
      state = transferSourceStateRef.current;
      if (closing.some((tab) => state.secondaryTabs.find((current) => current.id === tab.id) !== tab)) return;
      const closingIds = new Set(closing.map((tab) => tab.id));
      const tabs = state.secondaryTabs.filter((tab) => !closingIds.has(tab.id));
      const index = state.secondaryTabs.findIndex((tab) => tab.id === state.activeSecondaryTabId);
      const nextActive = focusTabId && tabs.find((tab) => tab.id === focusTabId)
        || tabs.find((tab) => tab.id === state.activeSecondaryTabId)
        || tabs[Math.min(index, tabs.length - 1)];
      secondaryTabModel.replaceState({ tabs, activeTabId: nextActive?.id ?? "" });
      for (const tab of closing) {
        if (tab.target.kind === "terminal") void window.octopusTransport?.closeTerminal(tab.target.terminalId).catch(() => undefined);
        if (tab.target.kind === "files") forgetFilesPane(filesPaneKey(accessProfile, tab.id, paneStateKey));
      }
      if (!nextActive) setSecondaryPaneOpen(false);
      else if (nextActive.id !== state.activeSecondaryTabId) activateSecondaryTab(nextActive);
    } catch (error) {
      appToast.danger(error instanceof Error ? error.message : String(error));
    } finally {
      secondaryClosePendingRef.current = false;
    }
  }, [accessProfile, activateSecondaryTab, paneStateKey, secondaryTabModel.replaceState, setSecondaryPaneOpen, topology]);

  const closeSecondaryTab = useCallback((tabId: string) => {
    void closeSecondaryTabSet([tabId]);
  }, [closeSecondaryTabSet]);

  const reorderSecondaryTabs = secondaryTabModel.reorderTabs;

  const closeOtherSecondaryTabs = useCallback((tabId: string) => {
    const tabs = transferSourceStateRef.current.secondaryTabs;
    if (!tabs.some((tab) => tab.id === tabId)) return;
    void closeSecondaryTabSet(tabs.filter((tab) => tab.id !== tabId).map((tab) => tab.id), tabId);
  }, [closeSecondaryTabSet]);

  const closeSecondaryTabsToRight = useCallback((tabId: string) => {
    const tabs = transferSourceStateRef.current.secondaryTabs;
    const index = tabs.findIndex((tab) => tab.id === tabId);
    if (index >= 0) void closeSecondaryTabSet(tabs.slice(index + 1).map((tab) => tab.id));
  }, [closeSecondaryTabSet]);

  const openTargetInClientWindow = useCallback(async (target: NonNullable<ReturnType<typeof transferableWorkspacePaneTarget>>) => {
    const open = window.octopusTransport?.openClientWindow;
    if (!open) throw new Error("Native client windows are unavailable.");
    return open({ initialTarget: target });
  }, []);

  const paneTransferBlockReason = useCallback((target: TransferableWorkspacePaneTarget, pane: "primary" | "secondary") => {
    if (target.kind === "worker_environment" && (!target.target.panelKind || target.target.panelKind === "browser")) return "Live browser adoption is unavailable for this viewer. The source browser remains in its window.";
    if (target.kind !== "surface" || target.surfaceId !== "chat") return "";
    return (pane === "secondary" ? secondaryChatWorkspace : chatWorkspace).paneTransferBlockReason(target.runId);
  }, [chatWorkspace, secondaryChatWorkspace]);
  const paneTransferBlockReasonRef = useRef(paneTransferBlockReason);
  paneTransferBlockReasonRef.current = paneTransferBlockReason;
  const transferSourceStateRef = useRef({
    paneScope,
    paneStateKey,
    layout: workspacePaneState.layout,
    activePane,
    activeSecondaryTabId,
    primaryRunIdentityReady,
    secondaryPaneFocused,
    secondaryPaneOpen,
    secondaryRunIdentityReady,
    secondaryTabs,
    titlebarTransferTarget,
  });
  transferSourceStateRef.current = {
    paneScope,
    paneStateKey,
    layout: workspacePaneState.layout,
    activePane,
    activeSecondaryTabId,
    primaryRunIdentityReady,
    secondaryPaneFocused,
    secondaryPaneOpen,
    secondaryRunIdentityReady,
    secondaryTabs,
    titlebarTransferTarget,
  };
  useEffect(() => {
    const bridge = window.octopusTransport;
    if (!bridge) return;
    let disposed = false;
    let registration: { windowId: string; generation: number } | null = null;
    let queued: OctopusClientWindowCloseRequest | null = null;
    let current: { request: OctopusClientWindowCloseRequest; controller: AbortController } | null = null;
    const cancelCurrent = () => {
      if (!current) return;
      current.controller.abort();
      releaseFilesWindowClose(current.request.requestId);
      if (nativeWindowCloseRequestRef.current?.requestId === current.request.requestId) nativeWindowCloseRequestRef.current = null;
      current = null;
      if (!disposed) setBrowserPresentationGeneration((generation) => generation + 1);
    };
    const handleClose = async (request: OctopusClientWindowCloseRequest) => {
      if (disposed || request.windowId !== clientWindow.windowId) return;
      if (!registration) { queued = request; return; }
      if (request.generation !== registration.generation) return;
      if (current?.request.requestId === request.requestId) return;
      cancelCurrent();
      const pending = { request, controller: new AbortController() };
      current = pending;
      nativeWindowCloseRequestRef.current = { requestId: request.requestId, generation: request.generation };
      const ownsRequest = () => !disposed && current === pending && !pending.controller.signal.aborted;
      try {
        const approved = await requestCloseAllFilesPanes(undefined, pending.controller.signal);
        if (!ownsRequest()) return;
        const allowClose = approved && acquireFilesWindowClose(request.requestId);
        if (allowClose) {
          for (const [key, state] of Object.entries(getWorkspacePaneStates())) {
            const tabs = state.tabs.map((tab) => {
              if (tab.target.kind !== "files") return tab;
              const fileKey = filesPaneKey(accessProfile, tab.id, key);
              if (!hasFilesPane(fileKey)) return tab;
              const snapshot = normalizeFilesPaneSnapshot(readFilesPane(fileKey).snapshot);
              if (!snapshot) throw new Error("The current Files document could not be preserved before closing.");
              return { ...tab, target: { kind: "files" as const, workspaceKey: tab.target.workspaceKey, snapshot, ...(snapshot.target ? { openTarget: snapshot.target } : {}) } };
            });
            writeSecondaryPaneStateAtKey(clientWindow.scope, key, { ...state, tabs });
          }
        }
        const outcome = await bridge.resolveClientWindowClose({ windowId: request.windowId, requestId: request.requestId, generation: request.generation, allowClose });
        if (ownsRequest() && (!allowClose || !outcome.accepted)) cancelCurrent();
      } catch (error) {
        if (!ownsRequest()) return;
        cancelCurrent();
        void bridge.resolveClientWindowClose({ windowId: request.windowId, requestId: request.requestId, generation: request.generation, allowClose: false }).catch(() => undefined);
        appToast.danger(error instanceof Error ? error.message : String(error));
      }
    };
    const requested = bridge.onClientWindowCloseRequested((request) => { void handleClose(request); });
    const cancelled = bridge.onClientWindowCloseCancelled((request) => {
      if (request.windowId !== clientWindow.windowId) return;
      if (queued?.requestId === request.requestId && queued.generation === request.generation) queued = null;
      if (current?.request.requestId === request.requestId && current.request.generation === request.generation) cancelCurrent();
    });
    void bridge.registerClientWindowCloseGuard().then((guard) => {
      if (disposed) {
        return bridge.unregisterClientWindowCloseGuard(guard);
      }
      if (guard.windowId !== clientWindow.windowId) throw new Error("Native window close guard does not match this window.");
      registration = guard;
      const request = queued;
      queued = null;
      if (request) void handleClose(request);
    }).catch((error) => {
      if (!disposed) appToast.danger(error instanceof Error ? error.message : String(error));
    });
    return () => {
      disposed = true;
      queued = null;
      cancelCurrent();
      requested();
      cancelled();
      if (registration) void bridge.unregisterClientWindowCloseGuard(registration).catch((error) => appToast.danger(error instanceof Error ? error.message : String(error)));
    };
  }, [accessProfile, clientWindow.scope, clientWindow.windowId, getWorkspacePaneStates]);

  const beginPaneTransfer = useCallback((sourceTabId: string, target: TransferableWorkspacePaneTarget, sourcePane: "primary" | "secondary") => {
    if (nativeWindowCloseRequestRef.current) throw new Error("Finish or cancel this window's close request before moving a pane.");
    const begin = window.octopusTransport?.beginClientPaneTransfer;
    if (!begin) throw new Error("Native client pane transfer is unavailable.");
    if (target.kind === "files") {
      const key = filesPaneKey(accessProfile, sourceTabId, paneStateKey);
      ensureFilesPane(key, target.openTarget, target.snapshot);
      const snapshot = captureFilesPane(key);
      if (!snapshot) throw new Error("The source Files pane is unavailable.");
      target = { ...target, snapshot };
    }
    const reference = begin({ sourcePane, sourceTabId, target });
    if (reference.sourceWindowId !== clientWindow.windowId) {
      throw new Error("Client pane transfer source does not match this window.");
    }
    try { window.sessionStorage.setItem(`octopus.client-pane-owner.${reference.transferId}`, paneStateKey); }
    catch (error) {
      void window.octopusTransport?.cancelClientPaneTransfer({ transferId: reference.transferId });
      throw error;
    }
    return { reference, target } satisfies ActiveClientPaneTransfer;
  }, [accessProfile, clientWindow.windowId, paneStateKey]);

  const cancelPaneTransfer = useCallback((transfer: ActiveClientPaneTransfer) => {
    const cancel = window.octopusTransport?.cancelClientPaneTransfer;
    if (cancel) void cancel({ transferId: transfer.reference.transferId }).catch(() => undefined);
  }, []);

  const sourceTransferMatches = useCallback((completion: ClientPaneTransferCompleted, finalizing = false) => {
    if (completion.sourceWindowId !== clientWindow.windowId) return false;
    const owner = transferPaneOwner(completion.transferId);
    if (!owner) return false;
    const state = transferSourceStateRef.current;
    const source = owner.read();
    const current = owner.key === state.paneStateKey;
    if (!current && (!finalizing || completion.sourcePane === "primary")) return false;
    const wholeWindow = completion.sourceTabId === `client-window:${clientWindow.windowId}`;
    const currentTab = source.tabs.find((tab) => tab.id === completion.sourceTabId);
    const currentTarget = wholeWindow ? state.titlebarTransferTarget : currentTab ? transferableWorkspacePaneTarget(currentTab.target) : null;
    if (wholeWindow && (clientWindow.primary || source.tabs.length > 0)) return false;
    if (currentTarget?.kind === "files" && completion.target.kind === "files") {
      if (!completion.target.snapshot) return false;
      if (!samePaneTransferTarget({ ...currentTarget, snapshot: undefined }, { ...completion.target, snapshot: undefined })) return false;
      try {
        if (!filesPaneMatchesSnapshot(filesPaneKey(accessProfile, completion.sourceTabId, owner.key), completion.target.snapshot)) return false;
      } catch {
        return false;
      }
    } else if (!samePaneTransferTarget(currentTarget, completion.target)) return false;
    if (current && completion.sourcePane === "secondary" && currentTab?.id === state.activeSecondaryTabId && !state.secondaryRunIdentityReady) return false;
    if (current && completion.sourcePane === "primary" && !state.primaryRunIdentityReady) return false;
    return !current || !paneTransferBlockReasonRef.current(completion.target, completion.sourcePane);
  }, [accessProfile, clientWindow.primary, clientWindow.windowId, transferPaneOwner]);

  const handleClientPaneTransferCompletion = useCallback((completion: ClientPaneTransferCompleted) => {
    const bridge = window.octopusTransport;
    if (!bridge || preparedSourceTransfersRef.current.has(completion.transferId)) return;
    const state = transferSourceStateRef.current;
    if (!state.primaryRunIdentityReady || !state.secondaryRunIdentityReady) return;
    const released = sourceTransferMatches(completion);
    if (released) {
      preparedSourceTransfersRef.current.set(completion.transferId, completion);
      if (completion.target.kind === "files") setFilesPaneTransferPending(filesPaneKey(accessProfile, completion.sourceTabId, transferSourceStateRef.current.paneStateKey), true);
    }
    try {
      bridge.acknowledgeClientPaneTransfer({ released, transferId: completion.transferId });
      setSourceHandoffPending(preparedSourceTransfersRef.current.size > 0);
      if (!released) appToast.warning("The source changed during handoff. It was retained and the destination move was cancelled.");
    } catch (error) {
      preparedSourceTransfersRef.current.delete(completion.transferId);
      if (completion.target.kind === "files") setFilesPaneTransferPending(filesPaneKey(accessProfile, completion.sourceTabId, transferSourceStateRef.current.paneStateKey), false);
      appToast.danger(error instanceof Error ? error.message : String(error));
    }
  }, [accessProfile, sourceTransferMatches]);

  useEffect(() => {
    const bridge = window.octopusTransport;
    if (!bridge) return;
    const cancelled = bridge.onClientPaneTransferCancelled((completion) => {
      const prepared = preparedSourceTransfersRef.current.get(completion.transferId);
      preparedSourceTransfersRef.current.delete(completion.transferId);
      const owner = transferPaneOwner(completion.transferId);
      if (owner && prepared && completion.sourceWindowId === clientWindow.windowId && completion.target.kind === "files") setFilesPaneTransferPending(filesPaneKey(accessProfile, completion.sourceTabId, owner.key), false);
      setSourceHandoffPending(preparedSourceTransfersRef.current.size > 0);
    });
    const finishSource = async (completion: ClientPaneTransferCompleted) => {
      if (completion.sourceWindowId !== clientWindow.windowId || finalizingSourceIdsRef.current.has(completion.transferId)) return;
      finalizingSourceIdsRef.current.add(completion.transferId);
      setSourceFinalizationPendingCount((count) => count + 1);
      preparedSourceTransfersRef.current.delete(completion.transferId);
      setSourceHandoffPending(preparedSourceTransfersRef.current.size > 0);
      const marker = `octopus.client-pane-finalized.${completion.transferId}`;
      let closeWindow = false;
      try {
        const owner = transferPaneOwner(completion.transferId);
        if (!owner) throw new Error("The pane transfer has no exact source owner.");
        if (window.sessionStorage.getItem(marker) == null) {
          if (!sourceTransferMatches(completion, true)) {
            appToast.warning("Destination handoff completed, but the changed source was retained.");
            window.sessionStorage.setItem(marker, "retained");
            if (completion.target.kind === "files") setFilesPaneTransferPending(filesPaneKey(accessProfile, completion.sourceTabId, owner.key), false);
          } else {
            const state = owner.read();
            if (completion.sourceTabId === `client-window:${clientWindow.windowId}`) closeWindow = true;
            else {
              const index = state.tabs.findIndex((tab) => tab.id === completion.sourceTabId);
              const tabs = state.tabs.filter((tab) => tab.id !== completion.sourceTabId);
              const activeTabId = state.activeTabId === completion.sourceTabId ? tabs[Math.min(index, tabs.length - 1)]?.id ?? "" : state.activeTabId;
              const open = tabs.length > 0 && state.open;
              const next = { ...state, activeTabId, open, tabs,
                layout: open ? state.layout : { ...state.layout, focused: false, activePane: "primary" as const } };
              writeSecondaryPaneStateAtKey(clientWindow.scope, owner.key, next);
              owner.write(next);
              if (completion.target.kind === "files") forgetFilesPane(filesPaneKey(accessProfile, completion.sourceTabId, owner.key), completion.target.snapshot);
              closeWindow = !clientWindow.primary && tabs.length === 0
                && !Object.entries(getWorkspacePaneStates()).some(([key, pane]) => key !== owner.key && pane.tabs.length);
            }
            window.sessionStorage.setItem(marker, closeWindow ? "close-window" : "removed");
          }
        } else closeWindow = window.sessionStorage.getItem(marker) === "close-window";
        await bridge.acknowledgeClientPaneSourceFinalization({ transferId: completion.transferId });
        if (closeWindow) bridge.windowControl("close");
      } catch (error) {
        finalizingSourceIdsRef.current.delete(completion.transferId);
        appToast.danger(`Destination is ready; source cleanup is pending: ${error instanceof Error ? error.message : String(error)}`);
      } finally {
        setSourceFinalizationPendingCount((count) => count - 1);
      }
    };
    const finalized = bridge.onClientPaneTransferFinalized((completion) => { void finishSource(completion); });
    if (primaryRunIdentityReady && secondaryRunIdentityReady) void bridge.pendingClientPaneSourceFinalizations().then((receipts) => {
      for (const receipt of receipts) void finishSource(receipt);
    }).catch((error) => appToast.danger(error instanceof Error ? error.message : String(error)));

    return () => { cancelled(); finalized(); };
  }, [accessProfile, clientWindow.primary, clientWindow.scope, clientWindow.windowId, getWorkspacePaneStates, primaryRunIdentityReady, secondaryRunIdentityReady, sourceTransferMatches, transferPaneOwner]);

  useEffect(() => {
    const bridge = window.octopusTransport;
    if (!bridge?.onClientPaneTransferCompleted || !bridge.pendingClientPaneTransfers) return;
    const dispatch = (completion: ClientPaneTransferCompleted) => {
      void handleClientPaneTransferCompletion(completion);
    };
    const unsubscribe = bridge.onClientPaneTransferCompleted(dispatch);
    void bridge.pendingClientPaneTransfers().then((completions) => {
      for (const completion of completions) dispatch(completion);
    }).catch((error) => {
      appToast.danger(error instanceof Error ? error.message : String(error));
    });
    return unsubscribe;
  }, [handleClientPaneTransferCompletion, primaryRunIdentityReady, secondaryRunIdentityReady]);

  const openChatInNewWindow = useCallback((runId: string) => {
    void openTargetInClientWindow({
      kind: "surface",
      runId,
      surfaceId: "chat",
      workspaceKey: "octopus-os",
    }).catch((error) => {
      appToast.danger(error instanceof Error ? error.message : String(error));
    });
  }, [openTargetInClientWindow]);

  const secondaryTabContextActions = useCallback((tab: WorkspacePaneTab) => {
    const state = secondaryTabs.find((candidate) => candidate.id === tab.id);
    if (!state || state.id === activeSecondaryTabId && !secondaryRunIdentityReady || !transferableWorkspacePaneTarget(state.target)) return [];
    return [
      { glyph: "openExternal" as const, id: "move_to_new_window", label: "Move to New Window" },
      ...(clientWindow.reattachWindowId ? [{ glyph: "restore" as const, id: "reattach_to_source", label: "Return to Original Window" }] : []),
    ];
  }, [activeSecondaryTabId, clientWindow.reattachWindowId, secondaryRunIdentityReady, secondaryTabs]);

  const secondaryTabCanNativeDrag = useCallback((tab: WorkspacePaneTab) => {
    const state = secondaryTabs.find((candidate) => candidate.id === tab.id);
    if (state?.id === activeSecondaryTabId && !secondaryRunIdentityReady) return false;
    return Boolean(state && transferableWorkspacePaneTarget(state.target));
  }, [activeSecondaryTabId, secondaryRunIdentityReady, secondaryTabs]);

  const handleSecondaryTabContextAction = useCallback((actionId: string, tab: WorkspacePaneTab) => {
    if (actionId !== "move_to_new_window" && actionId !== "reattach_to_source") return;
    const state = transferSourceStateRef.current;
    const source = state.secondaryTabs.find((candidate) => candidate.id === tab.id);
    const target = source ? transferableWorkspacePaneTarget(source.target) : null;
    if (!target || tab.id === state.activeSecondaryTabId && !state.secondaryRunIdentityReady) return;
    const blocked = paneTransferBlockReasonRef.current(target, "secondary");
    if (blocked) { appToast.warning(blocked); return; }
    const bridge = window.octopusTransport;
    if (!bridge) { appToast.danger("Native pane transfer is unavailable."); return; }
    let transfer: ActiveClientPaneTransfer;
    try { transfer = beginPaneTransfer(tab.id, target, "secondary"); }
    catch (error) { appToast.danger(error instanceof Error ? error.message : String(error)); return; }
    const handoff = actionId === "reattach_to_source" ? bridge.reattachClientPane : bridge.detachClientPane;
    void handoff({ transferId: transfer.reference.transferId }).catch((error) => {
      cancelPaneTransfer(transfer);
      appToast.danger(error instanceof Error ? error.message : String(error));
    });
  }, [beginPaneTransfer, cancelPaneTransfer]);

  const beginSecondaryWindowDrag = useCallback((tabId: string) => {
    const state = transferSourceStateRef.current;
    const tab = state.secondaryTabs.find((candidate) => candidate.id === tabId);
    const target = tab ? transferableWorkspacePaneTarget(tab.target) : null;
    if (!target || tabId === state.activeSecondaryTabId && !state.secondaryRunIdentityReady) throw new Error("Source pane is not ready to transfer.");
    const blocked = paneTransferBlockReasonRef.current(target, "secondary");
    if (blocked) throw new Error(blocked);
    return beginPaneTransfer(tabId, target, "secondary").reference;
  }, [beginPaneTransfer]);
  const reportPaneDragError = useCallback((error: unknown) => appToast.danger(error instanceof Error ? error.message : String(error)), []);
  const secondaryWindowPointerDown = useClientPaneWindowDrag({ begin: beginSecondaryWindowDrag, onError: reportPaneDragError, onReorder: reorderSecondaryTabs });
  const beginTitlebarWindowDrag = useCallback(() => {
    const state = transferSourceStateRef.current;
    if (clientWindow.primary || !state.titlebarTransferTarget || state.secondaryTabs.length > 0) throw new Error("Only an independent single-surface window can be reattached from its titlebar.");
    const blocked = paneTransferBlockReasonRef.current(state.titlebarTransferTarget, state.activePane);
    if (blocked) throw new Error(blocked);
    return beginPaneTransfer(`client-window:${clientWindow.windowId}`, state.titlebarTransferTarget, state.activePane).reference;
  }, [beginPaneTransfer, clientWindow.primary, clientWindow.windowId]);
  const titlebarWindowPointerDown = useClientPaneWindowDrag({ begin: beginTitlebarWindowDrag, onError: reportPaneDragError });

  const handleClientPaneDragOver = useCallback((event: DragEvent<HTMLDivElement>) => {
    if (!Array.from(event.dataTransfer.types).includes(CLIENT_PANE_DRAG_MIME)) return;
    event.preventDefault();
    event.dataTransfer.dropEffect = "move";
  }, []);

  const stageDestinationPaneTransfer = useCallback((transfer: ClientPaneTransferCompleted, resume: boolean) => {
    if (!homeRunId.trim()) return;
    if (pendingAcceptedPaneTransfersRef.current.has(transfer.transferId)) return;
    if (nativeWindowCloseRequestRef.current) {
      void window.octopusTransport?.rejectClientPaneTransfer({ transferId: transfer.transferId }).catch((error) => appToast.danger(error instanceof Error ? error.message : String(error)));
      appToast.warning("The pane remains in its source window while this window is closing.");
      return;
    }
    const state = transferSourceStateRef.current;
    const tab = workspacePaneTabForTarget(transfer.target);
    if (transfer.target.kind === "files") {
      if (!transfer.target.snapshot) throw new Error("Files handoff has no document snapshot.");
      const key = filesPaneKey(accessProfile, tab.id, paneStateKey);
      ensureFilesPane(key, transfer.target.openTarget, transfer.target.snapshot);
      setFilesPaneTransferPending(key, true);
    }
    const detachedWindow = Boolean(clientWindow.reattachWindowId) && state.secondaryTabs.length === 0;
    pendingAcceptedPaneTransfersRef.current.set(transfer.transferId, {
      owner: paneOwner,
      previousActiveTabId: state.activeSecondaryTabId,
      previousActivePane: state.activePane,
      previousFocused: state.secondaryPaneFocused,
      previousOpen: state.secondaryPaneOpen,
      previousTabs: state.secondaryTabs,
      detachedWindow,
      resume,
      started: false,
      tabId: tab.id,
      stagedTab: tab,
    });
    openSecondaryTab(tab);
    setSecondaryPaneFocused(detachedWindow || Boolean(clientWindow.reattachWindowId) && state.secondaryPaneFocused);
  }, [accessProfile, clientWindow.reattachWindowId, homeRunId, openSecondaryTab, paneOwner, paneStateKey, setSecondaryPaneFocused]);

  const handleClientPaneDrop = useCallback((event: DragEvent<HTMLDivElement>) => {
    const reference = parseClientPaneDragPayload(event.dataTransfer.getData(CLIENT_PANE_DRAG_MIME));
    if (!reference) return;
    event.preventDefault();
    event.dataTransfer.dropEffect = "move";
    if (reference.sourceWindowId === clientWindow.windowId) return;
    const accept = window.octopusTransport?.acceptClientPaneTransfer;
    if (!accept) {
      appToast.danger("Native client pane transfer is unavailable.");
      return;
    }
    void accept({ transferId: reference.transferId }).then((accepted) => {
      if (accepted.sourceWindowId !== reference.sourceWindowId || accepted.transferId !== reference.transferId) {
        throw new Error("Client pane transfer acceptance does not match the dragged source.");
      }
      stageDestinationPaneTransfer(accepted, false);
    }).catch((error) => {
      appToast.danger(error instanceof Error ? error.message : String(error));
    });
  }, [clientWindow.windowId, stageDestinationPaneTransfer]);

  useEffect(() => window.octopusTransport?.onClientPaneTransferAccepted((accepted) => {
    stageDestinationPaneTransfer(accepted, false);
  }), [stageDestinationPaneTransfer]);

  useEffect(() => {
    const pending = window.octopusTransport?.pendingClientPaneDestinationTransfers;
    if (!pending) return;
    let active = true;
    void pending().then((transfers) => {
      if (!active) return;
      for (const transfer of transfers) stageDestinationPaneTransfer(transfer, transfer.committed);
    }).catch((error) => {
      appToast.danger(error instanceof Error ? error.message : String(error));
    });
    return () => { active = false; };
  }, [stageDestinationPaneTransfer]);

  const rollbackAcceptedPaneTransfer = useCallback((transferId: string) => {
    const pending = pendingAcceptedPaneTransfersRef.current.get(transferId);
    if (!pending) return;
    pendingAcceptedPaneTransfersRef.current.delete(transferId);
    const state = pending.owner.read();
    const next = rollbackDestinationPaneTransfer(
      state,
      { tabs: pending.previousTabs, activeTabId: pending.previousActiveTabId },
      pending.stagedTab,
    );
    const open = next.tabs.length > 0 && (state.activeTabId === pending.tabId ? pending.previousOpen || next.tabs.length > pending.previousTabs.length : state.open);
    const layout = state.activeTabId === pending.tabId
      ? { ...state.layout, focused: open && pending.previousFocused, activePane: open ? pending.previousActivePane : "primary" as const }
      : state.layout;
    const restored = { ...state, ...next, open, layout };
    try { writeSecondaryPaneStateAtKey(clientWindow.scope, pending.owner.key, restored); }
    catch (error) { appToast.danger(`Could not persist the destination rollback: ${error instanceof Error ? error.message : String(error)}`); }
    pending.owner.write(restored);
    if (pending.stagedTab.target.kind === "files" && !next.tabs.some((tab) => tab.id === pending.tabId)) {
      forgetFilesPane(filesPaneKey(accessProfile, pending.tabId, pending.owner.key), pending.stagedTab.target.snapshot);
    } else if (pending.stagedTab.target.kind === "files") {
      setFilesPaneTransferPending(filesPaneKey(accessProfile, pending.tabId, pending.owner.key), false);
    }
  }, [accessProfile, clientWindow.scope]);

  useEffect(() => window.octopusTransport?.onClientPaneTransferCancelled((completion) => {
    if (completion.sourceWindowId !== clientWindow.windowId) rollbackAcceptedPaneTransfer(completion.transferId);
  }), [clientWindow.windowId, rollbackAcceptedPaneTransfer]);

  useEffect(() => {
    const bridge = window.octopusTransport;
    if (!bridge || pendingAcceptedPaneTransfersRef.current.size === 0) return;
    const tabIds = new Set(secondaryTabs.map((tab) => tab.id));
    for (const [transferId, pending] of pendingAcceptedPaneTransfersRef.current) {
      if (pending.started || pending.owner.key !== paneStateKey || !tabIds.has(pending.tabId) || pending.tabId !== activeSecondaryTabId || !secondaryRunIdentityReady) continue;
      pending.started = true;
      const requireCurrentDestination = () => {
        const state = transferSourceStateRef.current;
        const currentTab = state.secondaryTabs.find((tab) => tab.id === pending.tabId);
        const stagedTarget = transferableWorkspacePaneTarget(pending.stagedTab.target);
        if (state.paneStateKey !== pending.owner.key || pendingAcceptedPaneTransfersRef.current.get(transferId) !== pending || !currentTab || !stagedTarget || !samePaneTransferTarget(transferableWorkspacePaneTarget(currentTab.target), stagedTarget) || state.activeSecondaryTabId !== pending.tabId || !state.secondaryRunIdentityReady) throw new Error("Destination changed before handoff finalized.");
        if (pending.stagedTab.target.kind === "files") {
          const snapshot = pending.stagedTab.target.snapshot;
          if (!snapshot || !filesPaneMatchesSnapshot(filesPaneKey(accessProfile, pending.tabId, paneStateKey), snapshot)) throw new Error("Destination Files snapshot changed before handoff finalized.");
        }
        return state;
      };
      const outcome = (async () => {
        if (pending.stagedTab.target.kind === "files") await validateFilesPaneRoot(filesPaneKey(accessProfile, pending.tabId, paneStateKey), bridge);
        requireCurrentDestination();
        return pending.resume
          ? bridge.resumeClientPaneTransfer({ transferId })
          : bridge.commitClientPaneTransfer({ transferId });
      })();
      void outcome.then(async () => {
        if (pending.stagedTab.target.kind === "files") await validateFilesPaneRoot(filesPaneKey(accessProfile, pending.tabId, paneStateKey), bridge);
        if (pending.detachedWindow) await anchorClientPaneDrop(transferId, pending.tabId);
        const state = requireCurrentDestination();
        writeSecondaryPaneState(accessProfile, homeRunId, clientWindow.scope, {
          activeTabId: state.activeSecondaryTabId, open: state.secondaryPaneOpen, tabs: state.secondaryTabs, layout: state.layout,
        }, state.paneScope);
        await bridge.finalizeClientPaneTransfer({ transferId });
        pendingAcceptedPaneTransfersRef.current.delete(transferId);
        if (pending.stagedTab.target.kind === "files") setFilesPaneTransferPending(filesPaneKey(accessProfile, pending.tabId, paneStateKey), false);
      }).catch(async (error) => {
        rollbackAcceptedPaneTransfer(transferId);
        try { await bridge.rejectClientPaneTransfer({ transferId }); }
        catch (rejectionError) {
          appToast.danger(String(rejectionError));
        }
        appToast.danger(error instanceof Error ? error.message : String(error));
      });
    }
  }, [accessProfile, activeSecondaryTabId, clientWindow.scope, homeRunId, paneStateKey, rollbackAcceptedPaneTransfer, secondaryRunIdentityReady, secondaryTabs]);


  const addSecondaryTab = useCallback((actionId: string) => {
    if (actionId === "operator:terminal") {
      openTerminalTab();
      return;
    }
    if (actionId === "operator:files") {
      openFilesTab();
      return;
    }
    if (actionId === "chat:new") {
      openSecondaryChatTab("");
      return;
    }
    if (actionId === "chat:side") {
      const runId = workerEnvironmentActionRunIds[0];
      if (runId) openSecondaryChatTab(runId);
      return;
    }
    if (actionId.startsWith("we:")) {
      const action = workerEnvironmentActions.find((candidate) => `we:${candidate.id}` === actionId);
      if (action) openWorkerEnvironmentPane(action.target);
      return;
    }
    const [kind, rawWorkspaceKey, ...surfaceParts] = actionId.split(":");
    const surfaceId = surfaceParts.join(":");
    const workspaceKey = rawWorkspaceKey === "dashboard" || rawWorkspaceKey === "octopus-os"
      ? rawWorkspaceKey
      : null;
    if (kind !== "surface" || !workspaceKey || !surfaceId) return;
    const surface = visibleSurfaces.find((candidate) => (
      candidate.id === surfaceId
      && !isSidebarOnlySurface(candidate)
      && surfaceBelongsToWorkspace(candidate, workspaceKey)
    ));
    if (!surface) return;
    openSecondaryTab(secondaryPaneTab(workspaceKey, surface.id, ""));
  }, [openFilesTab, openSecondaryChatTab, openSecondaryTab, openTerminalTab, openWorkerEnvironmentPane, visibleSurfaces, workerEnvironmentActionRunIds, workerEnvironmentActions]);

  const toggleSecondaryPane = useCallback(() => {
    if (secondaryPaneOpen) {
      setSecondaryPaneOpen(false);
      return;
    }
    const selected = secondaryTabs.find((tab) => tab.id === activeSecondaryTabId) ?? secondaryTabs[0];
    if (selected) {
      activateSecondaryTab(selected);
      setSecondaryPaneOpen(true);
      setActivePane("secondary");
      return;
    }
    setSecondaryPaneOpen(true);
    setActivePane("secondary");
  }, [activateSecondaryTab, activeSecondaryTabId, secondaryPaneOpen, secondaryTabs, setActivePane, setSecondaryPaneOpen]);

  const closeSecondaryPane = useCallback(() => {
    setSecondaryPaneOpen(false);
  }, [setSecondaryPaneOpen]);

  useEffect(() => subscribeAirHandGestures((gesture, source) => {
    if (!gesturePreferences.enabled || settingsOpen) return;
    if (accessProfile === "live" && source === "test") return;
    if (clientDialogOwnsCommands()) return;
    const command = gesturePreferences.mappings[gesture];
    if (command !== "none") dispatchClientCommand(command, "gesture");
  }), [accessProfile, gesturePreferences, settingsOpen]);

  useEffect(() => {
    if (!gesturePreferences.enabled) {
      stopGestureRecognition();
      return;
    }
    void startGestureRecognition({
      cameraDeviceId: gesturePreferences.cameraDeviceId,
      mirrorX: gesturePreferences.mirrorX,
      sensitivity: gesturePreferences.sensitivity,
    });
  }, [
    gesturePreferences.cameraDeviceId,
    gesturePreferences.enabled,
    gesturePreferences.mirrorX,
    gesturePreferences.sensitivity,
  ]);

  useEffect(() => () => stopGestureRecognition(), []);

  useEffect(() => subscribeClientCommands((command, _source, settingsDestination) => {
    if (command === "open_command_menu") {
      setCommandMenuOpen(true);
      return;
    }
    if (command === "new_chat") {
      setCommandMenuOpen(false);
      openChat("", activeNavigationSection === "tasks" ? projectFilterEeId || undefined : undefined);
      return;
    }
    if (command === "open_voice") {
      openVoice();
      return;
    }
    if (command === "open_settings") {
      setCommandMenuOpen(false);
      setSettingsRecordingId(settingsDestination?.dictationRecordingId ?? "");
      setSettingsOpen(true);
      if (settingsDestination?.sectionId) selectSettingsSection(settingsDestination.sectionId);
      return;
    }
    if (command === "toggle_gesture_control") {
      setGesturePreferences((current) => ({ enabled: !current.enabled }));
      return;
    }
    if (command === "toggle_pin_task") {
      const runId = activeClientSurfaceIdentity.runId;
      if (!runId) return;
      setAgentPinnedChatIds((current) => current.includes(runId)
        ? current.filter((candidate) => candidate !== runId)
        : [runId, ...current]);
      return;
    }
    if (command === "open_side_task") {
      const runId = activeClientSurfaceIdentity.runId;
      if (!runId) return;
      openSecondaryChatTab(runId);
      return;
    }
    if (command === "open_terminal") {
      setCommandMenuOpen(false);
      openTerminalTab();
      return;
    }
    const requestedWorkerEnvironmentPanel: WorkerEnvironmentPanelKind | null = command === "open_browser" ? "browser"
        : command === "open_changes" ? "changes"
          : null;
    if (requestedWorkerEnvironmentPanel) {
      setCommandMenuOpen(false);
      const action = workerEnvironmentActions.find((candidate) => candidate.panelKind === requestedWorkerEnvironmentPanel);
      if (!action) {
        appToast.warning(`No active Worker Environment projects a ${requestedWorkerEnvironmentPanel} surface.`);
        return;
      }
      openWorkerEnvironmentPane(action.target);
      return;
    }
    if (command === "focus_search") {
      if (activeClientSurfaceIdentity.pane === "secondary") {
        setSecondarySearchFocusTick((tick) => tick + 1);
      } else {
        setSearchFocusTick((tick) => tick + 1);
      }
      return;
    }
    if (command === "toggle_split") {
      toggleSecondaryPane();
      return;
    }
    if (command === "navigate_back" || command === "navigate_forward") {
      navigateSurfaceHistory(command === "navigate_back" ? -1 : 1);
      return;
    }
    if (command !== "previous_surface" && command !== "next_surface") return;
    const index = shortcutNavigationDestinations.findIndex((destination) => destination.id === activeNavigationSection);
    if (index < 0 || shortcutNavigationDestinations.length === 0) return;
    const direction = command === "previous_surface" ? -1 : 1;
    const next = (index + direction + shortcutNavigationDestinations.length) % shortcutNavigationDestinations.length;
    openNavigationSection(shortcutNavigationDestinations[next].id);
  }), [accessProfile, activeClientSurfaceIdentity, activeNavigationSection, navigateSurfaceHistory, openChat, openNavigationSection, openSecondaryChatTab, openTerminalTab, openVoice, openWorkerEnvironmentPane, projectFilterEeId, selectSettingsSection, setGesturePreferences, setSettingsOpen, shortcutNavigationDestinations, toggleSecondaryPane, workerEnvironmentActions]);

  useEffect(() => {
    const handleClientSurfaceAction = (event: Event) => {
      if (!(event instanceof CustomEvent)) return;
      const detail = event.detail as ClientSurfaceActionEvent | undefined;
      if (!detail?.action?.event) return;
      if (detail.action.event === "client.command") {
        const command = detail.action.payload?.command;
        if (isClientCommandId(command)) {
          dispatchClientCommand(command, "surface");
          return;
        }
        appToast.warning(`Client surface ${detail.documentId} requested an unknown command.`);
        return;
      }
      if (detail.action.event === "client.open-surface") {
        const surfaceId = typeof detail.action.payload?.surface_id === "string" ? detail.action.payload.surface_id : "";
        const surface = visibleSurfaces.find((candidate) => candidate.id === surfaceId);
        if (!surface || isSidebarOnlySurface(surface)) {
          appToast.warning(`Client surface ${detail.documentId} requested an unavailable surface.`);
          return;
        }
        openPrimarySurface(surface);
        return;
      }
      appToast.warning(`Client surface action is not supported: ${detail.action.event}`);
    };
    window.addEventListener(CLIENT_SURFACE_ACTION_EVENT, handleClientSurfaceAction);
    return () => window.removeEventListener(CLIENT_SURFACE_ACTION_EVENT, handleClientSurfaceAction);
  }, [accessProfile, openPrimarySurface, visibleSurfaces]);

  useAdmittedClientActions({
    enabled: accessProfile === "live" && clientWindow.primary && runtimeHost.available && runtimeHost.ready
      && surfaceRegistry.hydrated && agentProjectWorkspace.status === "ready" && !agentProjectWorkspace.error && homeChatWorkspace.runsLoaded
      && homeChatWorkspace.activeRunId === homeRunId && homeChatWorkspace.activeRun?.runId === homeRunId
      && homeChatWorkspace.activeRun?.clientSurfaceRole === "home"
      && homeChatWorkspace.activeRun?.chatInterfaceId === agentProjectWorkspace.homeInterfaceId,
    home: runtimeHost.home,
    runId: homeRunId,
    interfaceId: homeChatWorkspace.activeRun?.chatInterfaceId ?? "",
    surfaces: visibleSurfaces.filter((surface) => !isSidebarOnlySurface(surface)),
    openSurface: (surface) => openPrimarySurface(surface, true),
    openFile: (target) => openFilesTab(target),
    widgets: (action, operationId) => applyHomeWidgetAction({ accessProfile, runId: homeRunId,
      interfaceId: homeChatWorkspace.activeRun?.chatInterfaceId ?? "",
      surfaces: surfaceRegistry.voiceSurfaces, extensions: surfaceRegistry.extensions }, action, operationId),
    inspect: (action) => inspectClient(action, visibleSurfaces.filter((surface) => !isSidebarOnlySurface(surface))),
    relaunch: async () => {
      if (!window.octopusTransport) throw new Error("Client relaunch is unavailable.");
      return window.octopusTransport.relaunchClient();
    },
    onRefusal: (reason) => appToast.warning(reason),
  });

  useExtensionReviewPrompt(surfaceRegistry.extensions, accessProfile === "live" && clientWindow.primary, () => {
    setSettingsOpen(true);
    selectSettingsSection("extensions");
  });

  useEffect(() => {
    if (settingsOpen) setCommandMenuOpen(false);
  }, [settingsOpen]);

  useEffect(() => {
    function onKeyDown(event: KeyboardEvent) {
      if (event.defaultPrevented || commandMenuOpen) return;
      if (clientDialogOwnsCommands()) return;
      const command = keyboardCommandForEvent(event, keyboardShortcuts);
      if (settingsOpen && command !== "navigate_back" && command !== "navigate_forward" && command !== "toggle_sidebar") return;
      if (command) {
        if (keyboardTargetHasRichEditingOwner(event.target) && keyboardCommandConflictsWithEditing(command, keyboardShortcuts)) return;
        event.preventDefault();
        dispatchClientCommand(command, "keyboard");
        return;
      }
      if (!(event.metaKey || event.ctrlKey)) return;
      if (event.shiftKey || event.altKey) return;
      if (keyboardTargetHasEditableOwner(event.target)) return;
      const digit = Number(event.key);
      if (Number.isInteger(digit) && digit >= 1 && digit <= Math.min(9, shortcutNavigationDestinations.length)) {
        event.preventDefault();
        openNavigationSection(shortcutNavigationDestinations[digit - 1].id);
      }
    }
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [commandMenuOpen, keyboardShortcuts, openNavigationSection, settingsOpen, shortcutNavigationDestinations]);

  useEffect(() => {
    const surface = visibleSurfaces.find((candidate) => candidate.id === activeClientSurfaceIdentity.surfaceId);
    document.title = settingsOpen ? "Settings — Octopus" : surface
      ? `${surface.label} — Octopus`
      : "Octopus";
  }, [activeClientSurfaceIdentity, settingsOpen, visibleSurfaces]);

  function toggleDashboardFullscreen() {
    setInspectorOpen((open) => !open);
  }

  const activeTaskSources = useMemo(() => {
    if (activeSecondaryTab?.target.kind !== "task_sources") return [];
    const { runId, sources } = activeSecondaryTab.target.target;
    const workspace = [chatWorkspace, homeChatWorkspace, secondaryChatWorkspace]
      .find((candidate) => candidate.activeRunId === runId);
    return workspace && !workspace.loadingMessages
      ? collectTaskSources(workspace.renderedMessages.filter((message) => !message.runId || message.runId === runId))
      : sources;
  }, [
    activeSecondaryTab,
    chatWorkspace.activeRunId,
    chatWorkspace.loadingMessages,
    chatWorkspace.renderedMessages,
    homeChatWorkspace.activeRunId,
    homeChatWorkspace.loadingMessages,
    homeChatWorkspace.renderedMessages,
    secondaryChatWorkspace.activeRunId,
    secondaryChatWorkspace.loadingMessages,
    secondaryChatWorkspace.renderedMessages,
  ]);

  useClientPaneDropZone(Boolean(activeSurface));

  if (!activeSurface) {
    return null;
  }

  const activeSecondaryPanePresentation = secondaryPaneTabs.find((tab) => tab.id === activeSecondaryTabId);
  const secondaryPaneTabStripVisible = secondaryPaneUsesTabStrip(secondaryPaneTabs);
  const secondaryBrowserRunId = secondarySurfaceId === "voice" ? homeRunId
    : secondarySurfaceId === "chat" && secondaryRunIdentityReady ? secondaryChatWorkspace.activeRunId : "";
  const secondaryBrowserKey = JSON.stringify([accessProfile, runtimeHost.home, secondaryBrowserRunId]);
  const secondarySurfaceContent = secondarySurface ? (
    <SurfaceHost
    commandsActive={secondaryCommandsActive}
    key={secondarySurface.id}
    accessProfile={accessProfile}
    surface={secondarySurface}
    inbox={{ activity: markActivity, selected: activitySelection, selectedGroup: activitySelectionGroup, onSelect: selectInboxItem, onOpen: openActivityMark }}
    topology={topology}
    query={secondaryQuery}
    focusedMovement={secondaryMovementSelection}
    onClearMovement={() => setSecondaryMovementSelection(null)}
    onQueryChange={(value) => { setSecondaryMovementSelection(null); setSecondaryQuery(value); }}
    searchPlaceholder={secondarySurface.chrome.searchPlaceholder ?? "Search surface..."}
    searchFocusTick={secondarySearchFocusTick}
    dashboardScope={secondaryDashboardScope}
    dashboardTopologyMode={secondaryDashboardScope === "World" ? secondaryDashboardWorldTopologyMode : secondaryDashboardTopologyMode}
    dashboardFilters={secondaryDashboardFilters}
    selected={effectiveSecondarySelection}
    chatWorkspace={workspaceWithTaskDraftActions(secondarySurface?.rendererKind === "builtin.voice" ? homeChatWorkspace : secondaryChatWorkspace)}
    homeRunId={homeRunId}
    voiceSession={voiceSession}
    homeViewMode={secondaryHomeViewMode}
    onHomeViewModeChange={setSecondaryHomeViewMode}
    paneRole="secondary"
    paneTabStripVisible={secondaryPaneTabStripVisible}
    inspectorOpen={secondaryInspectorOpen}
    onSelect={handleSecondarySelect}
    onCloseInspector={() => setSecondaryInspectorOpen(false)}
    onOpenFiles={(target) => openFilesTab(target, secondaryWorkspaceKey)}
    voiceSurfaces={surfaceRegistry.voiceSurfaces}
    installedVoiceWidgetIds={installedHomeWidgetIds}
    installedVoiceWidgetsReady={surfaceRegistry.extensions.hydrated}
    builtinVoiceWidgetSources={surfaceRegistry.extensions.builtinWidgetSources}
    workerEnvironmentSurfaces={surfaceRegistry.workerEnvironmentSurfaces}
    onOpenRun={openSecondarySurfaceRun}
          />
  ) : null;
  const secondaryPane = secondarySurface || secondaryPaneOpen ? (
    <WorkspaceSurfacePane
      controlsInWindowHeader
      activeTabId={activeSecondaryTabId}
      addActions={secondaryPaneAddActions}
      canNativeDrag={secondaryTabCanNativeDrag}
      contextActions={secondaryTabContextActions}
      focused={secondaryPaneFocused}
      onClose={closeSecondaryPane}
      onAddAction={addSecondaryTab}
      onNewTab={openLauncherTab}
      onCloseOtherTabs={closeOtherSecondaryTabs}
      onCloseTabsToRight={closeSecondaryTabsToRight}
      onFocusedChange={setSecondaryPaneFocused}
      onContextAction={handleSecondaryTabContextAction}
      onTabClose={closeSecondaryTab}
      onTabWindowPointerDown={(tab, event) => secondaryWindowPointerDown(tab.id, event)}
      handoffPending={sourceHandoffPending}
      onTabReorder={reorderSecondaryTabs}
      onTabSelect={selectSecondaryTab}
      tabs={secondaryPaneTabs}
    >
      <RetainedTerminalTabs
        accessProfile={accessProfile}
        active={secondaryPaneOpen}
        activeTabId={activeSecondaryTabId}
        onNewTerminalTab={openTerminalTab}
        onTitleChange={updateTerminalTitle}
        tabs={secondaryTabs}
      />
      <RetainedFilesTabs
        accessProfile={accessProfile}
        paneStateKey={paneStateKey}
        active={secondaryPaneOpen}
        activeTabId={activeSecondaryTabId}
        tabs={secondaryTabs}
      />
      {secondaryTabs.map(tab => tab.target.kind === "client_browser" ? <div key={tab.id} className="h-full min-h-0" hidden={!secondaryPaneOpen || tab.id !== activeSecondaryTabId}><ClientBrowserTab id={tab.target.pageId} active={accessProfile === "live" && secondaryPaneOpen && tab.id === activeSecondaryTabId && !settingsOpen} /></div> : null)}
      <RetainedWorkerEnvironmentTabs
        accessProfile={accessProfile}
        active={secondaryPaneOpen}
        activeTabId={activeSecondaryTabId}
        assistantCodeComments={activeWorkerEnvironmentAssistantCodeComments}
        snapshot={topology}
        surfaces={surfaceRegistry.workerEnvironmentSurfaces}
        tabs={secondaryTabs}
      />
      <Suspense fallback={<SurfaceLoadingState label={`Loading ${activeSecondaryPanePresentation?.title || "tab"}…`} />}>
      {activeSecondaryTab?.target.kind === "client_browser" || activeSecondaryTab?.target.kind === "terminal" || activeSecondaryTab?.target.kind === "files" || activeSecondaryTab?.target.kind === "worker_environment" ? null : activeSecondaryTab?.target.kind === "task_sources" ? (
        <TaskSourcesPaneTab onOpenFiles={(target) => openFilesTab(target, secondaryWorkspaceKey)} sources={activeTaskSources} />
      ) : !secondarySurface ? null : secondarySurface.rendererKind === "builtin.chat" && !secondaryRunIdentityReady ? (
        <SurfaceLoadingState label="Opening task…" />
      ) : (
        <SurfacePage>
          {secondarySurface.chrome.topbar !== "none" && secondarySurface.rendererKind !== "builtin.event_log" ? (
            <TopBar
              accessProfile={accessProfile}
              title={secondarySurface.label}
              query={secondaryQuery}
              onQueryChange={setSecondaryQuery}
              placeholder={secondarySurface.chrome.searchPlaceholder ?? "Search surface..."}
              mode={secondarySurface.chrome.topbar === "dashboard" ? "dashboard" : "wide"}
              dashboardScope={secondaryDashboardScope}
              onDashboardScopeChange={setSecondaryDashboardScope}
              dashboardTopologyMode={secondaryDashboardScope === "World" ? secondaryDashboardWorldTopologyMode : secondaryDashboardTopologyMode}
              onDashboardTopologyModeChange={secondaryDashboardScope === "World" ? setSecondaryDashboardWorldTopologyMode : setSecondaryDashboardTopologyMode}
              dashboardFilters={secondaryDashboardFilters}
              onDashboardFiltersChange={setSecondaryDashboardFilters}
              inspectorOpen={secondaryInspectorOpen}
              onFullscreen={() => setSecondaryInspectorOpen((open) => !open)}
              searchFocusTick={secondarySearchFocusTick}
              titleInWindowChrome={secondaryPaneTabStripVisible}
            />
          ) : null}
          {secondarySurface.rendererKind === "builtin.chat" || secondarySurface.rendererKind === "builtin.voice" ? (
            <TaskBrowserPane
              accessProfile={accessProfile}
              active={Boolean(secondaryBrowserRunId) && runtimeHost.ready && topology.projection.mode === "live" && topology.projection.endpoint === runtimeHost.home}
              snapshot={topology}
              state={taskBrowserTabs[secondaryBrowserKey] ?? emptyTaskBrowserTabs}
              surfaces={surfaceRegistry.workerEnvironmentSurfaces}
              onChange={(update) => setTaskBrowserTabs((current) => ({ ...current, [secondaryBrowserKey]: update(current[secondaryBrowserKey] ?? emptyTaskBrowserTabs) }))}
            >{secondarySurfaceContent}</TaskBrowserPane>
          ) : secondarySurfaceContent}
        </SurfacePage>
      )}
      </Suspense>
    </WorkspaceSurfacePane>
  ) : null;

  const titlebarTask = activeSurface.rendererKind === "builtin.chat" && primarySurfaceTarget.runId !== homeRunId
    ? taskChatRuns.find((run) => run.runId === primarySurfaceTarget.runId)
    : undefined;

  return (
    <ScheduledWorkspaceProvider key={JSON.stringify([accessProfile, runtimeHost.home])} active={activeSurface.id === "scheduled" || (secondaryPaneOpen && secondarySurfaceId === "scheduled")}
      onCreateTask={(text) => {
        chatWorkspace.createChat("", "", text);
        setProjectFilterEeId("");
        selectSurface("chat", "");
      }}>
      <FilesPaneDialogs />
      <AppShell
        accessProfile={accessProfile}
        paneStateKey={paneStateKey}
        preferredSidebarOpen={workspacePaneState.layout.sidebarOpen}
        onSidebarOpenChange={workspacePane.setSidebarOpen}
        sidebarWidth={sidebarWidth}
        surfaces={visibleSurfaces}
        navigationSection={activeNavigationSection}
        activeSurfaceId={primarySurfaceTarget.surfaceId}
        activeChatRunId={primarySurfaceTarget.runId}
        chatRuns={taskChatRuns}
        chatRunsLoading={chatWorkspace.loadingRuns}
        homeRunId={homeRunId}
        inboxUnreadCount={markActivity.unreadCount}
        inboxArrivalId={markActivity.arrivalId}
        activityContent={<MarkActivityView activity={markActivity} activeId={activitySelection?.id ?? ""} onSelect={selectInboxItem} />}
        dashboardPinnedSurfaceIds={dashboardPinnedSurfaceIds}
        agentPinnedChatIds={agentPinnedChatIds}
        agentChatOrderIds={agentChatOrderIds}
        agentProjectFilterEeId={projectFilterEeId}
        agentProjectWorkspace={agentProjectWorkspace}
        agentExecutionContexts={chatWorkspace.executionContexts}
        agentDefinitions={chatWorkspace.agentPresets}
        agentProviderInterfaces={chatWorkspace.declaredProviderInterfaces}
        agentModelsLoading={!chatWorkspace.providerInterfacesLoaded || chatWorkspace.providerInterfacesRefreshing}
        agentModelError={chatWorkspace.providerInterfaceError}
        onRefreshAgentModels={chatWorkspace.refreshProviderModels}
        dashboardExpanded={!inspectorOpen}
        interactionDisabled={settingsOpen}
        onCloseSettings={() => setSettingsOpen(false)}
        settingsSurface={settingsOpen ? (
          <Suspense fallback={<SurfaceLoadingState label="Loading settings…" />}>
            <SettingsPage
              accessProfile={accessProfile}
              documentRoot={surfaceRegistry.documentRoot}
              documentVerdicts={surfaceRegistry.documentVerdicts}
              documents={surfaceRegistry.documents}
              extensions={surfaceRegistry.extensions}
              onClose={() => setSettingsOpen(false)}
              onOpenExtensionDestination={openExtensionDestination}
              onSectionChange={selectSettingsSection}
              sectionId={activeDestination.settingsSection!}
              recordingId={settingsRecordingId}
              sidebarWidth={effectiveSidebarWidth}
              surfaceErrors={surfaceRegistry.errors}
              surfaces={surfaceRegistry.surfaces}
            />
          </Suspense>
        ) : undefined}
        runtimeHost={runtimeHost}
        secondaryPaneFocused={secondaryPaneFocused}
        secondaryPaneOpen={secondaryPaneOpen}
        secondaryPane={secondaryPane}
        secondaryTaskPaneWidthPercent={workspacePaneState.layout.widthPercent}
        titlebarSurfaceContext={titlebarTask
          ? { glyph: "folder", title: clientTaskTitle(titlebarTask.runId, titlebarTask.title, taskOrganization) }
          : { glyph: activeSurface.chrome.titlebarGlyph ?? activeSurface.glyph, title: activeSurface.label }}
        onTitlebarSurfaceContextPointerDown={!clientWindow.primary && titlebarTransferTarget ? (event) => titlebarWindowPointerDown(clientWindow.windowId, event) : undefined}
        surfaceErrors={surfaceRegistry.errors}
        onNavigationChange={openNavigationSection}
        onClientPaneDragOver={handleClientPaneDragOver}
        onClientPaneDrop={handleClientPaneDrop}
        onSurfaceChange={openSidebarSurface}
        onSelectChat={openSidebarChat}
        onOpenChatInNewWindow={openChatInNewWindow}
        onNewChat={(projectId) => openChat("", projectId)}
        onDashboardPinnedSurfaceIdsChange={setDashboardPinnedSurfaceIds}
        onAgentPinnedChatIdsChange={setAgentPinnedChatIds}
        onAgentChatOrderIdsChange={setAgentChatOrderIds}
        onAgentProjectFilterEeIdChange={setProjectFilterEeId}
        onToggleSecondaryPane={toggleSecondaryPane}
        onSecondaryPaneFocusedChange={setSecondaryPaneFocused}
        onPrimaryPaneBlockedChange={setPrimaryPaneBlocked}
        onSecondaryPaneOpenChange={setSecondaryPaneOpen}
        onSecondaryTaskPaneWidthPercentChange={workspacePane.setWidthPercent}
        onOpenSettings={(section) => { setSettingsOpen(true); if (section) selectSettingsSection(section); }}
        onLogOut={onLogOut}
        onSearch={() => setCommandMenuOpen(true)}
        onSidebarWidthChange={setSidebarWidth}
        onEffectiveSidebarWidthChange={setEffectiveSidebarWidth}
        canNavigateBack={activeNavigation.index > 0}
        canNavigateForward={activeNavigation.index < activeNavigation.entries.length - 1}
        onNavigateBack={() => navigateSurfaceHistory(-1)}
        onNavigateForward={() => navigateSurfaceHistory(1)}
      >
        {activeSurface.chrome.topbar !== "none" && activeSurface.rendererKind !== "builtin.event_log" ? (
          <TopBar
            accessProfile={accessProfile}
            title={activeSurface.label}
            query={query}
            onQueryChange={(value) => { setActivitySelection(null); setQuery(value); }}
            placeholder={activeSurface.chrome.searchPlaceholder ?? "Search surface..."}
            mode={activeSurface.chrome.topbar === "dashboard" ? "dashboard" : "wide"}
            dashboardScope={dashboardScope}
            onDashboardScopeChange={setDashboardScope}
            dashboardTopologyMode={dashboardScope === "World" ? dashboardWorldTopologyMode : dashboardTopologyMode}
            onDashboardTopologyModeChange={dashboardScope === "World" ? setDashboardWorldTopologyMode : setDashboardTopologyMode}
            dashboardFilters={dashboardFilters}
            onDashboardFiltersChange={setDashboardFilters}
            onFullscreen={toggleDashboardFullscreen}
            inspectorOpen={inspectorOpen}
            searchFocusTick={searchFocusTick}
            titleInWindowChrome
          />
        ) : null}
        <SurfaceRenderBoundary resetKey={activeSurface.rendererKind === "builtin.chat" ? `chat:${primarySurfaceTarget.runId || "new"}` : activeSurface.id}>
          {activeSurface.rendererKind === "builtin.chat" && !primaryRunIdentityReady ? (
            <SurfaceLoadingState label="Opening task…" />
          ) : <SurfaceHost
            accessProfile={accessProfile}
            commandsActive={primaryCommandsActive}
            surface={activeSurface}
            inbox={{ activity: markActivity, selected: activitySelection, selectedGroup: activitySelectionGroup, onSelect: selectInboxItem, onOpen: openActivityMark }}
            topology={topology}
            query={query}
            onQueryChange={(value) => { setActivitySelection(null); setQuery(value); }}
            searchPlaceholder={activeSurface.chrome.searchPlaceholder ?? "Search surface..."}
            searchFocusTick={searchFocusTick}
            dashboardScope={dashboardScope}
            dashboardTopologyMode={dashboardScope === "World" ? dashboardWorldTopologyMode : dashboardTopologyMode}
            dashboardFilters={dashboardFilters}
            selected={effectiveSelection}
            chatWorkspace={workspaceWithTaskDraftActions(activeSurface.rendererKind === "builtin.voice" ? homeChatWorkspace : chatWorkspace)}
            homeRunId={homeRunId}
            voiceSession={voiceSession}
            homeViewMode={homeViewMode}
            onHomeViewModeChange={setHomeViewMode}
            paneRole="primary"
            inspectorOpen={inspectorOpen}
            onSelect={handleSelect}
            onCloseInspector={() => setInspectorOpen(false)}
            onOpenFiles={(target) => openFilesTab(target, activeWorkspaceKey)}
            voiceSurfaces={surfaceRegistry.voiceSurfaces}
            installedVoiceWidgetIds={installedHomeWidgetIds}
            installedVoiceWidgetsReady={surfaceRegistry.extensions.hydrated}
            builtinVoiceWidgetSources={surfaceRegistry.extensions.builtinWidgetSources}
            workerEnvironmentSurfaces={surfaceRegistry.workerEnvironmentSurfaces}
            onOpenRun={openSurfaceRun}
          />}
        </SurfaceRenderBoundary>
      </AppShell>
      <Suspense fallback={null}><AnimatePresence>{clientPreferences.floatingCallPuck
        && isVoiceCallActive(voiceSession.phase)
        && !(secondaryPaneOpen && secondaryPaneFocused
          ? secondarySurfaceId === "voice"
          : (activeSurface.id === "voice" && homeViewMode === "voice") || (secondaryPaneOpen && secondarySurfaceId === "voice")) ? (
          <VoiceCallPuck
            key="home-call"
            accessProfile={accessProfile}
            onOpenHome={() => { openVoice(); setHomeViewMode("voice"); }}
            session={voiceSession}
          />
      ) : null}</AnimatePresence></Suspense>
      {commandMenuOpen && !settingsOpen ? (
        <Suspense fallback={null}>
          <CommandMenu
            accessProfile={accessProfile}
            activeSurfaceId={activeClientSurfaceIdentity.surfaceId || activeSurface.id}
            chatRuns={taskChatRuns}
            onClose={() => setCommandMenuOpen(false)}
            onOpenRun={openChat}
            onSelectSurface={openPrimarySurface}
            open
            surfaces={visibleSurfaces.filter((surface) => !isSidebarOnlySurface(surface))}
            voiceActive={isVoiceCallActive(voiceSession.phase)}
          />
        </Suspense>
      ) : null}
      {accessProfile === "live" ? (
        <BuiltinClientExtensionActionHost actions={surfaceRegistry.extensions.installedActions} />
      ) : null}
      <GestureFocusRing />
      <GestureCommandHud accessProfile={accessProfile} />
      {clientWindow.primary ? (
        <GestureTourHost
          accessProfile={accessProfile}
          onOpenSettings={() => {
            setSettingsOpen(true);
            selectSettingsSection("gestures");
          }}
        />
      ) : null}
    </ScheduledWorkspaceProvider>
  );
}
