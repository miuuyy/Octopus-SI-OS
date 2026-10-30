import { AnimatePresence, motion } from "motion/react";
import "../icon-glyphs/chat";
import "../icon-glyphs/voice";
import "../icon-glyphs/worker";
import { useEffect, useLayoutEffect, useMemo, useRef, useState, useSyncExternalStore, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { subscribeClientCommands } from "../../clientActions";
import type { RuntimeTopologyState } from "../../runtimeTopology";
import { OcLoadingText } from "../../design-system/LoadingSkeleton";
import type { ChatRun } from "../../chatRuns";
import { ClientSurfaceRenderer, resetVoiceWidgetLayout, restoreVoiceWidgetLayout } from "../../client-surfaces/ClientSurfaceRenderer";
import { homeSurfaceIdentity, homeSurfaceLayoutIdentity } from "../../client-surfaces/homePresentation";
import { useHomeSurfaceDocuments } from "../../client-surfaces/useHomeSurfaceDocuments";
import { QueuedMessageList } from "../chat/QueuedMessageList";
import type { AdmittedClientSurfaceDocument, ClientSurfaceActionEvent, ClientSurfaceHostSource } from "../../client-surfaces/types";
import type { EnvironmentNode, RunProjection, TopologySnapshot } from "../../data/topology";
import { OcMenuItem, OcMenuSectionLabel, OcMenuSurface } from "../../design-system/components";
import { readClientChatProjects } from "../../shell/clientWorkspaces";
import { appToast, useAppErrorToast } from "../../toasts";
import type { ChatWorkspace } from "../chat";
import { OperatorRequestSurface } from "../chat/OperatorRequestSurface";
import { MessageContent } from "../chat/MessageContent";
import { conversationTraceAdmission } from "../chat/conversationTrace";
import type { ChatAttachmentView, ChatRenderMessage, ChatToolPresentationIndex } from "../chat/types";
import type { OctopusClientAccessProfile } from "../../electron";
import { Glyph, type GlyphName } from "../IconGlyphs";
import { ShellTooltip } from "../shell/ShellTooltip";
import { ChatComposer } from "../chat/ChatComposer";
import type { ProjectedFileSource } from "../chat/ProjectedFilePicker";
import { HomeCallControls } from "./HomeCallControls";
import { HomeTurnActivity, homeTurnActivity, homeTurnAwaitsClientResult, homeTurnAwaitsFinalReply, homeTurnTerminalNotice } from "./HomeTurnActivity";
import { projectHomeVoiceContext } from "./voiceCallArtifacts";
import { isVoiceCallActive, voicePresentation } from "./voicePresentation";
import { useAppearance } from "../../appearance";
import { VoiceOrbScene } from "./VoiceOrbScene";
import { OctopusOceanScene } from "./OctopusOceanScene";
import type { VoiceSession } from "./useVoiceSession";
import {
  buildVoiceWidgetDocuments,
  type InsightsWidget,
  type TaskProgressWidget,
  type UpcomingScheduledWidgetItem,
  type VoiceWidgetItem,
} from "./voiceWidgetDocuments";
import type { HomeRunActivity } from "./useHomeActivityWorkspace";
import { chatProductError } from "../chat/chatProductError";
import { OctopusLoadingState } from "../OctopusLoadingLogo";
import "../chat/chat.css";
import "./voice.css";


type Props = {
  accessProfile: OctopusClientAccessProfile;
  activity: HomeRunActivity;
  builtinWidgetSources: Record<string, ClientSurfaceHostSource[]>;
  commandsActive?: boolean;
  homeRunId: string;
  installedWidgetIds: string[];
  installedWidgetsReady: boolean;
  onViewModeChange?: (mode: HomeViewMode) => void;
  onWidgetsOpenChange?: (open: boolean) => void;
  viewMode: HomeViewMode;
  session: VoiceSession;
  surfaces: AdmittedClientSurfaceDocument[];
  topology: RuntimeTopologyState;
  widgetsOpen: boolean;
  workspace: ChatWorkspace;
};

export type HomeViewMode = "voice" | "activity";

type ProjectionTarget = {
  environment: EnvironmentNode | null;
  run: RunProjection | null;
};

type ProjectionItem = VoiceWidgetItem;

const VOICE_TITLE_KEY = "octopus.voice.session-title.v1";
const VOICE_RECORDING_KEY = "octopus.voice.save-audio.v1";
const VOICE_SPEAKER_KEY = "octopus.voice.speaker-enabled.v1";
const VOICE_HIDDEN_SURFACES_KEY = "octopus.voice.hidden-surfaces.v1";
const FORMING_DELIVERY_WINDOW_MS = 15_000;
const FORMING_HOLD_MS = 1_600;
const LEAVING_HOLD_MS = 420;
const SURFACE_TRANSITIONS_SETTLE_MS = 1_200;
const HOME_DEFERRED_STOP_MS = 30_000;
const LAB_TASK_PROGRESS: TaskProgressWidget = {
  completion: 62,
  failed: 0,
  running: 7,
  scheduled: 4,
  unseenDone: 16,
  waiting: 3,
};
const LAB_INSIGHTS: InsightsWidget = {
  label: "Leads this month",
  value: "1,284",
  change: "+18%",
  changeDetail: "vs last month",
  series: [2.1, 2.4, 3.6, 3.1, 4.2, 3.8, 5.1, 6.4],
};
export function VoiceView({ accessProfile, activity, builtinWidgetSources, commandsActive = true, homeRunId, installedWidgetIds, installedWidgetsReady, onViewModeChange, onWidgetsOpenChange, session, surfaces, topology, widgetsOpen, workspace, viewMode }: Props) {
  const storageKeys = voiceStorageKeys(accessProfile);
  const [hiddenSurfaceIds, setHiddenSurfaceIds] = useState(() => readStoredStringSet(storageKeys.hiddenSurfaces));
  useEffect(() => {
    const refresh = (event: StorageEvent) => {
      if (event.key === storageKeys.hiddenSurfaces || event.key === null) setHiddenSurfaceIds(readStoredStringSet(storageKeys.hiddenSurfaces));
    };
    window.addEventListener("storage", refresh);
    return () => window.removeEventListener("storage", refresh);
  }, [storageKeys.hiddenSurfaces]);
  const installedWidgetIdsHydratedRef = useRef(false);
  const previousInstalledWidgetIdsRef = useRef(new Set<string>());
  const [sessionTitle, setSessionTitle] = useState(() => readStoredText(storageKeys.title, "Home"));
  const [saveAudio] = useState(() => readStoredBoolean(storageKeys.recording, false));
  const [speakerEnabled, setSpeakerEnabled] = useState(() => readStoredBoolean(storageKeys.speaker, true));
  const widgetCanvasRef = useRef<HTMLDivElement>(null);
  const viewRef = useRef<HTMLElement>(null);
  const viewOpenedAtRef = useRef(Date.now());
  const seenSurfaceIdsRef = useRef(new Set<string>());
  const formingTimersRef = useRef(new Set<number>());
  const [formingSurfaceIds, setFormingSurfaceIds] = useState<ReadonlySet<string>>(() => new Set());
  const visibilityBaselineRef = useRef<ReadonlyMap<string, AdmittedClientSurfaceDocument> | null>(null);
  const leavingTimersRef = useRef(new Set<number>());
  const [leavingSurfaces, setLeavingSurfaces] = useState<ReadonlyMap<string, AdmittedClientSurfaceDocument>>(() => new Map());
  const [surfaceTransitionsArmed, setSurfaceTransitionsArmed] = useState(false);
  const stageMainRef = useRef<HTMLElement>(null);
  const orbProbeRef = useRef<HTMLElement>(null);
  const outputMeter = useSyncExternalStore(
    session.subscribeOutputMeter,
    session.getOutputMeterSnapshot,
    session.getOutputMeterSnapshot,
  );

  const inputMeter = useSyncExternalStore(session.subscribeMeter, session.getMeterSnapshot, session.getMeterSnapshot);

  const boundRunId = homeRunId;
  const homeProjectionLoading = accessProfile === "live"
    && (
      (!workspace.runsLoaded && !workspace.runsError)
      || (Boolean(homeRunId) && (workspace.activeRunId !== homeRunId || workspace.loadingMessages))
    );
  const target = useMemo(() => findProjectionTarget(topology, boundRunId), [boundRunId, topology]);
  const projectedFileSources = useMemo<ProjectedFileSource[]>(() => {
    if (accessProfile !== "live" || !window.octopusTransport?.readWorkerEnvironmentFileAttachment || !target.run) return [];
    const environments = target.run.workerEnvironments.length
      ? target.run.workerEnvironments
      : target.run.workerEnvironment.id
        ? [target.run.workerEnvironment]
        : [];
    return environments.flatMap((environment) => environment.surfaces.filter((surface) => (
      surface.kind === "files"
      && surface.mode === "direct"
      && (
        (surface.protocol.trim().toLowerCase() === "filesystem.readonly" && surface.readOnly)
        || (surface.protocol.trim().toLowerCase() === "filesystem.readwrite" && !surface.readOnly)
      )
    )).map((surface) => ({
      workerEnvironmentId: environment.id,
      surfaceId: surface.id,
      label: `${environment.label} · ${surface.label}`,
    })));
  }, [accessProfile, target.run]);
  const voiceBinding = workspace.runs.find((run) => run.runId === homeRunId && run.clientSurfaceRole === "home")?.voiceContext;
  const homeInterfaceId = workspace.activeRun?.runId === boundRunId
    ? workspace.activeRun.chatInterfaceId
    : workspace.runs.find((run) => run.runId === boundRunId)?.chatInterfaceId || "";
  const homeInterfaceMessages = useMemo(() => workspace.renderedMessages.filter((message) => (
    Boolean(homeInterfaceId)
    && message.runId === boundRunId
    && (message.interfaceId === homeInterfaceId || message.interfaceId === voiceBinding?.delegationInterfaceId
      && message.role === "user" && message.direction === "in" && message.label === "realtime.delegation")
  )), [boundRunId, homeInterfaceId, workspace.renderedMessages, voiceBinding?.delegationInterfaceId]);
  const voiceConversationContext = useMemo(() => projectHomeVoiceContext(homeInterfaceMessages, homeRunId, homeInterfaceId, voiceBinding?.delegationInterfaceId), [homeInterfaceMessages, homeRunId, homeInterfaceId, voiceBinding?.delegationInterfaceId]);
  const homeOperatorRequests = useMemo(() => homeInterfaceMessages.filter((message) =>
    ["approval", "user_input", "elicitation"].includes(message.eventKind ?? "") && message.eventData?.resolved !== true
  ), [homeInterfaceMessages]);
  const latestHomePrompt = useMemo(() => [...homeInterfaceMessages]
    .reverse()
    .find((message) => message.role === "user" && message.direction === "in") ?? null, [homeInterfaceMessages]);
  const latestHomeTurnAtMs = useMemo(() => {
    const latestPromptAtMs = latestHomePrompt?.createdAtMs ?? 0;
    return [...homeInterfaceMessages]
      .reverse()
      .find((message) => message.eventKind === "turn_started" && message.createdAtMs >= latestPromptAtMs)
      ?.createdAtMs ?? latestPromptAtMs;
  }, [homeInterfaceMessages, latestHomePrompt?.createdAtMs]);
  const latestHomeOutcome = useMemo(() => {
    const latestPromptAtMs = latestHomePrompt?.createdAtMs ?? 0;
    const outcomeSince = (sinceMs: number) => [...homeInterfaceMessages]
      .reverse()
      .find((message) => (
        message.direction === "out"
        && message.role === "assistant"
        && (message.eventKind === "message" || message.eventKind === "error")
        && (message.terminal || message.eventKind === "message"
          && message.eventData?.phase === "final_answer"
          && message.eventData?.lifecycle === "completed")
        && message.createdAtMs >= sinceMs
        && Boolean(message.text.trim() || message.attachments?.length || message.error)
      )) ?? null;
    const currentTurn = outcomeSince(latestHomeTurnAtMs);
    if (currentTurn || latestHomeTurnAtMs === latestPromptAtMs) return currentTurn;
    return homeTurnAwaitsFinalReply(homeTurnActivity(homeInterfaceMessages, latestPromptAtMs)) ? null : outcomeSince(latestPromptAtMs);
  }, [homeInterfaceMessages, latestHomePrompt?.createdAtMs, latestHomeTurnAtMs]);
  const latestHomeResponse = latestHomeOutcome
    && latestHomeOutcome.eventKind === "message"
    && !latestHomeOutcome.error
    ? latestHomeOutcome
    : null;
  const latestHomeFailure = latestHomeOutcome
    && (latestHomeOutcome.eventKind === "error" || latestHomeOutcome.error)
    ? latestHomeOutcome
    : null;
  const latestHomeStreamingResponse = useMemo(() => [...homeInterfaceMessages]
    .reverse()
    .find((message) => (
      message.direction === "out"
      && message.role === "assistant"
      && message.eventKind === "message_delta"
      && message.createdAtMs >= latestHomeTurnAtMs
      && Boolean(message.text.trim())
    )) ?? null, [homeInterfaceMessages, latestHomeTurnAtMs]);
  const latestHomeActivity = useMemo(() => homeTurnActivity(
    homeInterfaceMessages, latestHomePrompt?.createdAtMs ?? 0,
  ), [homeInterfaceMessages, latestHomePrompt?.createdAtMs]);
  const displayTitle = session.context?.homeRunId === homeRunId ? session.context.runTitle : sessionTitle;
  const providerId = voiceBinding?.providerId || "";
  const providerLabel = workspace.declaredProviderInterfaces.find((provider) => provider.id === providerId)?.label || "Voice";
  const callActive = isVoiceCallActive(session.phase);
  const callActiveRef = useRef(callActive);
  callActiveRef.current = callActive;
  const callState = voicePresentation(session.phase);
  const [callSceneReady, setCallSceneReady] = useState(callActive);
  const { octopuses: oceanScene } = useAppearance();
  const replyHidden = callActive || callSceneReady;
  const voiceStartDisabledReason = !homeRunId
    ? "Home is not available yet."
    : !providerId
      ? "No voice interface is available for Home."
      : "";
  const projectName = useMemo(() => {
    if (accessProfile === "lab") {
      return readClientChatProjects(accessProfile)
        .find((project) => project.chatRunIds.includes(boundRunId))?.name || "No project";
    }
    const boundRun = workspace.runs.find((run) => run.runId === boundRunId);
    if (!boundRun || !target.environment || target.environment.id === boundRun.osRootEeId) return "No project";
    return target.environment.name || target.environment.id;
  }, [accessProfile, boundRunId, target.environment, workspace.runs]);
  const contextItems = useMemo(() => projectContext(target, projectName), [projectName, target]);
  const toolItems = useMemo(() => projectTools(target), [target]);
  const goalItems = useMemo(() => projectGoals(target), [target]);
  const memoryItems = useMemo(() => projectMemory(target), [target]);
  const taskProgress = useMemo(
    () => accessProfile === "lab"
      ? LAB_TASK_PROGRESS
      : projectTaskProgress(topology, target, boundRunId, workspace.runs),
    [accessProfile, boundRunId, target, topology, workspace.runs],
  );
  const upcomingScheduled = useMemo(
    () => projectUpcomingScheduled(workspace.runs),
    [workspace.runs],
  );
  const insights = useMemo(
    () => accessProfile === "lab" ? LAB_INSIGHTS : projectInsights(activity),
    [accessProfile, activity],
  );
  const installedWidgetIdSet = useMemo(() => new Set(installedWidgetIds), [installedWidgetIds]);
  const builtInSurfaces = useMemo(() => buildVoiceWidgetDocuments({
    context: contextItems,
    goals: goalItems,
    memory: memoryItems,
    insights,
    scheduled: upcomingScheduled,
    taskProgress,
    tools: toolItems,
  }).filter((surface) => installedWidgetIdSet.has(surface.document.id)).map((surface) => ({
    ...surface,
    sourceExtensionId: surface.document.id,
    hostSources: builtinWidgetSources[surface.document.id] ?? [],
  })), [builtinWidgetSources, contextItems, goalItems, insights, installedWidgetIdSet, memoryItems, taskProgress, toolItems, upcomingScheduled]);
  const homeSurfaces = useHomeSurfaceDocuments(surfaces, { accessProfile, runId: boundRunId, interfaceId: homeInterfaceId });
  const visibleHomeSurfaceIds = useMemo(() => new Set(homeSurfaces.visibleDocuments.map(homeSurfaceIdentity)), [homeSurfaces.visibleDocuments]);
  const allSurfaces = useMemo(() => [...builtInSurfaces, ...homeSurfaces.documents].sort((left, right) => (
    left.document.target.order - right.document.target.order
      || left.document.title.localeCompare(right.document.title)
  )), [builtInSurfaces, homeSurfaces.documents]);
  const surfaceVisible = (surface: AdmittedClientSurfaceDocument) => surface.sourceExtensionId
    ? hiddenSurfaceIds !== null
      && !hiddenSurfaceIds.has(surface.document.id)
      && (!installedWidgetIdsHydratedRef.current || previousInstalledWidgetIdsRef.current.has(surface.document.id))
    : visibleHomeSurfaceIds.has(homeSurfaceIdentity(surface));
  const visibleSurfaces = allSurfaces.filter(surfaceVisible);
  const visibleSurfaceKey = visibleSurfaces.map(homeSurfaceIdentity).join("\n");
  const visibleLayoutIds = new Set(visibleSurfaces.map(homeSurfaceLayoutIdentity));
  const visibilityBaseline = surfaceTransitionsArmed ? visibilityBaselineRef.current : null;
  const appearingLayoutIds = new Set(visibilityBaseline ? [...visibleLayoutIds].filter((id) => !visibilityBaseline.has(id)) : []);
  const leaving = new Map([...leavingSurfaces, ...(visibilityBaseline ?? [])].filter(([id]) => !visibleLayoutIds.has(id)));
  const renderedSurfaces = [...visibleSurfaces, ...leaving.values()].sort((left, right) => (
    left.document.target.order - right.document.target.order
      || left.document.title.localeCompare(right.document.title)
  ));
  const leftSurfaces = renderedSurfaces.filter((surface) => (
    (surface.document.target.kind === "voice.widget" || surface.document.target.kind === "voice.window")
    && surface.document.target.slot === "left"
  ));
  const rightSurfaces = renderedSurfaces.filter((surface) => (
    (surface.document.target.kind === "voice.widget" || surface.document.target.kind === "voice.window")
    && surface.document.target.slot === "right"
  ));

  useEffect(() => {
    const seen = seenSurfaceIdsRef.current;
    const delivered = visibleSurfaces.filter((surface) => {
      const identity = homeSurfaceIdentity(surface);
      if (seen.has(identity)) return false;
      seen.add(identity);
      const deliveredAt = surface.sourceDeliveredAt ? Date.parse(surface.sourceDeliveredAt) : Number.NaN;
      return deliveredAt >= viewOpenedAtRef.current - FORMING_DELIVERY_WINDOW_MS;
    });
    if (delivered.length) formSurfaces(delivered.map(homeSurfaceIdentity));
  }, [visibleSurfaceKey]);

  useEffect(() => {
    if (!installedWidgetsReady) return;
    const timer = window.setTimeout(() => setSurfaceTransitionsArmed(true), SURFACE_TRANSITIONS_SETTLE_MS);
    return () => window.clearTimeout(timer);
  }, [installedWidgetsReady]);

  useEffect(() => {
    if (!installedWidgetsReady) return;
    const current = new Map(visibleSurfaces.map((surface) => [homeSurfaceLayoutIdentity(surface), surface] as const));
    const previous = visibilityBaselineRef.current;
    visibilityBaselineRef.current = current;
    if (!previous || !surfaceTransitionsArmed) return;
    const appeared = [...current].filter(([id]) => !previous.has(id)).map(([, surface]) => homeSurfaceIdentity(surface));
    if (appeared.length) formSurfaces(appeared);
    const departed = [...previous].filter(([id]) => !current.has(id));
    if (!departed.length) return;
    const departedIds = departed.map(([id]) => id);
    setLeavingSurfaces((surfaces) => new Map([...surfaces, ...departed]));
    const timer = window.setTimeout(() => {
      leavingTimersRef.current.delete(timer);
      setLeavingSurfaces((surfaces) => new Map([...surfaces].filter(([id]) => !departedIds.includes(id))));
    }, LEAVING_HOLD_MS);
    leavingTimersRef.current.add(timer);
  }, [installedWidgetsReady, surfaceTransitionsArmed, visibleSurfaceKey]);

  useEffect(() => () => {
    formingTimersRef.current.forEach((timer) => window.clearTimeout(timer));
    leavingTimersRef.current.forEach((timer) => window.clearTimeout(timer));
  }, []);

  useAppErrorToast(chatProductError(workspace) || "");
  useAppErrorToast(homeSurfaces.errors.join("\n"));

  useEffect(() => {
    window.localStorage.setItem(storageKeys.title, sessionTitle);
  }, [sessionTitle, storageKeys.title]);

  useEffect(() => {
    window.localStorage.setItem(storageKeys.recording, saveAudio ? "true" : "false");
  }, [saveAudio, storageKeys.recording]);

  useEffect(() => {
    window.localStorage.setItem(storageKeys.speaker, speakerEnabled ? "true" : "false");
    session.setSpeakerEnabled(speakerEnabled);
  }, [session, speakerEnabled, storageKeys.speaker]);

  useEffect(() => {
    if (hiddenSurfaceIds === null || !installedWidgetsReady) return;
    window.localStorage.setItem(storageKeys.hiddenSurfaces, JSON.stringify([...hiddenSurfaceIds]));
  }, [hiddenSurfaceIds, installedWidgetsReady, storageKeys.hiddenSurfaces]);

  useEffect(() => {
    if (!installedWidgetsReady) return;
    const previous = previousInstalledWidgetIdsRef.current;
    previousInstalledWidgetIdsRef.current = new Set([...previous, ...installedWidgetIds]);
    installedWidgetIdsHydratedRef.current = true;
    setHiddenSurfaceIds((current) => {
      if (current === null) return new Set(installedWidgetIds);
      return current;
    });
  }, [installedWidgetIds, installedWidgetsReady]);

  useEffect(() => {
    const stage = widgetCanvasRef.current;
    if (!stage) return;
    let timeout = 0;
    let observed = false;
    const observer = new ResizeObserver(() => {
      if (!observed) {
        observed = true;
        return;
      }
      stage.classList.add("is-stage-resizing");
      window.clearTimeout(timeout);
      timeout = window.setTimeout(() => stage.classList.remove("is-stage-resizing"), 220);
    });
    observer.observe(stage);
    return () => {
      window.clearTimeout(timeout);
      observer.disconnect();
      stage.classList.remove("is-stage-resizing");
    };
  }, []);

  useEffect(() => subscribeClientCommands((command) => {
    if (!commandsActive) return;
    if (command === "voice_primary") {
      handlePrimaryAction();
      return;
    }
    if (command === "voice_end" && callState.canEnd) void session.end();
  }), [boundRunId, callActive, commandsActive, providerId, providerLabel, saveAudio, session, sessionTitle]);

  function startVoice() {
    if (!callState.canStart) return;
    if (voiceStartDisabledReason || !voiceBinding) {
      appToast.warning(voiceStartDisabledReason);
      return;
    }
    const title = sessionTitle.trim() || "Home";
    if (title !== sessionTitle) setSessionTitle(title);
    void session.start({
      ...voiceBinding,
      homeRunId,
      providerLabel,
      runTitle: title,
    }, { saveRecording: saveAudio, context: voiceConversationContext });
  }

  function handlePrimaryAction() {
    if (callState.canStart) {
      startVoice();
      return;
    }
    if (callState.canMute) session.toggleMute();
  }

  function handleSurfaceAction(event: ClientSurfaceActionEvent): boolean {
    if (event.action.event !== "voice.open-activity") return false;
    onViewModeChange?.("activity");
    return true;
  }

  function formSurfaces(identities: string[]) {
    setFormingSurfaceIds((current) => new Set([...current, ...identities]));
    const timer = window.setTimeout(() => {
      formingTimersRef.current.delete(timer);
      setFormingSurfaceIds((current) => new Set([...current].filter((identity) => !identities.includes(identity))));
    }, FORMING_HOLD_MS);
    formingTimersRef.current.add(timer);
  }

  function setSurfaceVisible(surface: AdmittedClientSurfaceDocument, visible: boolean) {
    if (visible && !surfaceVisible(surface)) formSurfaces([homeSurfaceIdentity(surface)]);
    if (!surface.sourceExtensionId) {
      if (visible) homeSurfaces.restore(surface);
      else homeSurfaces.dismiss(surface);
      return;
    }
    setHiddenSurfaceIds((current) => {
      const next = new Set(current === null ? installedWidgetIds : current);
      if (visible) next.delete(surface.document.id);
      else next.add(surface.document.id);
      return next;
    });
  }

  function setWidgetsMenuOpen(next: boolean) {
    onWidgetsOpenChange?.(next);
  }

  const status = homeEnvironmentStatus(target, homeProjectionLoading);
  const orbActivity = accessProfile === "lab"
    ? { source: "preview" as const }
    : {
        source: "live" as const,
        listeningLevel: inputMeter.level,
        speakingLevel: outputMeter.level,
        thinkingLevel: !workspace.runtimeBlockReason && (session.providerActivity || workspace.busy || workspace.submitting) ? 1 : 0,
      };

  if (!callActive && homeProjectionLoading) {
    return (
      <section className="voice-view is-loading">
        <OctopusLoadingState className="surface-content-loading-state" label="Opening Home" />
      </section>
    );
  }

  return (
    <section className={`voice-view is-${session.phase} is-${viewMode} has-home-composer`} ref={viewRef}>
      <div aria-hidden="true" className="voice-home-scene">
        <VoiceOrbScene activity={orbActivity} ambient={!callActive || !callSceneReady} onDispersed={() => { if (!callActiveRef.current) setCallSceneReady(false); }} phase={callSceneReady ? session.phase : "idle"} quietField={oceanScene} sceneSurface={stageMainRef} formationSurface={viewRef} rippleSurface={oceanScene ? undefined : viewRef} sizeProbe={orbProbeRef} />
        {oceanScene ? <OctopusOceanScene activity={orbActivity} calling={callActive} orbProbe={orbProbeRef} rippleSurface={viewRef} /> : null}
      </div>
      <header className="voice-session-heading" data-ocean-obstacle="" data-voice-placement-obstacle="">
        <VoiceSessionTitle
          editable={!callActive}
          onChange={setSessionTitle}
          title={displayTitle}
        />
        {topology.readError ? (
          <button className="voice-session-status is-error" type="button" title={topology.readError}
            disabled={topology.loading} onClick={topology.refresh}>
            {topology.loading ? <OcLoadingText label="Loading environment" /> : "Couldn’t load environment · Retry"}
          </button>
        ) : !target.run && topology.loading ? (
          <div className="voice-session-status is-pending"><OcLoadingText label="Loading environment" /></div>
        ) : status.label ? (
          <div className={`voice-session-status is-${status.tone}`}>
            <span>{status.label}</span>
          </div>
        ) : null}
      </header>

      <HomeWidgetsControl
        accessProfile={accessProfile}
        errors={homeSurfaces.errors}
        hasHiddenSurfaces={allSurfaces.some((surface) => !surfaceVisible(surface)) || Boolean(homeSurfaces.errors.length)}
        onOpenChange={setWidgetsMenuOpen}
        onShowAll={() => { setHiddenSurfaceIds(new Set()); homeSurfaces.restoreAll(); }}
        onSurfaceVisibleChange={setSurfaceVisible}
        open={widgetsOpen}
        surfaceVisible={surfaceVisible}
        surfaces={allSurfaces}
      />
      <main className="voice-session-stage" ref={stageMainRef}>
        <i aria-hidden="true" className="voice-orb-probe" ref={orbProbeRef} />
        <HomeResponseStage
          operatorRequests={homeOperatorRequests}
          onOperatorRequestResponse={workspace.respondToOperatorRequest}
          replyHidden={replyHidden}
          onReplyHidden={() => { if (callActiveRef.current) setCallSceneReady(true); }}
          busy={workspace.busy}
          blockingReason={workspace.runtimeBlockReason}
          activity={latestHomeActivity}
          activityStartedAtMs={latestHomePrompt?.createdAtMs ?? 0}
          toolPresentations={workspace.toolPresentations}
          composer={(
            <HomeCommandDock
              callActive={callActive}
              commandsActive={commandsActive}
              onVoiceStart={handlePrimaryAction}
              voiceStartDisabled={!callState.canStart || Boolean(voiceStartDisabledReason)}
              callControls={<HomeCallControls onSpeakerChange={setSpeakerEnabled} onStart={startVoice} session={session} speakerEnabled={speakerEnabled} />}
              projectedFileSources={projectedFileSources}
              awaitingClientResult={homeTurnAwaitsClientResult(latestHomeActivity, workspace.toolPresentations)}
              workspace={workspace}
            />
          )}
          dragOver={!callActive && workspace.dragOver}
          dropEnabled={!callActive && !workspace.submitting}
          homeInterfaceReady={Boolean(homeInterfaceId)}
          loading={homeProjectionLoading}
          onDragLeave={workspace.onDragLeave}
          onDragOver={workspace.onDragOver}
          onDrop={workspace.dropFiles}
          key="home"
          response={latestHomeResponse}
          submitting={workspace.submitting && !workspace.runtimeBlockReason}
          streamingResponse={latestHomeStreamingResponse}
          failure={latestHomeFailure}
          turnKey={latestHomePrompt?.id ?? latestHomeResponse?.id ?? "empty"}
        />
      </main>

      <AnimatePresence>
        {session.pendingAudioArtifact ? (
          <motion.aside
            animate={{ opacity: 1, y: 0, scale: 1 }}
            aria-label="Incoming audio artifact"
            className="voice-incoming-audio"
            exit={{ opacity: 0, y: 5, scale: 0.985 }}
            initial={{ opacity: 0, y: 5, scale: 0.985 }}
            key={session.pendingAudioArtifact.artifactId}
            transition={{ duration: 0.16, ease: [0.2, 0, 0, 1] }}
          >
            <span className="voice-incoming-audio__icon"><Glyph name="volume" size={16} /></span>
            <span className="voice-incoming-audio__copy">
              <strong>Play incoming audio?</strong>
              <small>{session.pendingAudioArtifact.mimeType} · {formatVoiceBytes(session.pendingAudioArtifact.size)}</small>
            </span>
            <button onClick={() => session.respondToPendingAudioArtifact(false)} type="button">Skip</button>
            <button className="is-primary" onClick={() => session.respondToPendingAudioArtifact(true)} type="button">Play</button>
          </motion.aside>
        ) : null}
      </AnimatePresence>

      <div className="voice-widget-canvas" ref={widgetCanvasRef}>
        {leftSurfaces.map((surface, index) => (
          <ClientSurfaceRenderer
            accessProfile={accessProfile}
            admitted={surface}
            key={homeSurfaceIdentity(surface)}
            onAction={handleSurfaceAction}
            onDismiss={surface.document.target.kind === "voice.window" || !surface.sourceExtensionId ? () => setSurfaceVisible(surface, false) : undefined}
            voiceForming={formingSurfaceIds.has(homeSurfaceIdentity(surface)) || appearingLayoutIds.has(homeSurfaceLayoutIdentity(surface))}
            voiceLeaving={leaving.has(homeSurfaceLayoutIdentity(surface))}
            voicePlacement={{ count: leftSurfaces.length, index, slot: "left" }}
          />
        ))}
        {rightSurfaces.map((surface, index) => (
          <ClientSurfaceRenderer
            accessProfile={accessProfile}
            admitted={surface}
            key={homeSurfaceIdentity(surface)}
            onAction={handleSurfaceAction}
            onDismiss={surface.document.target.kind === "voice.window" || !surface.sourceExtensionId ? () => setSurfaceVisible(surface, false) : undefined}
            voiceForming={formingSurfaceIds.has(homeSurfaceIdentity(surface)) || appearingLayoutIds.has(homeSurfaceLayoutIdentity(surface))}
            voiceLeaving={leaving.has(homeSurfaceLayoutIdentity(surface))}
            voicePlacement={{ count: rightSurfaces.length, index, slot: "right" }}
          />
        ))}
      </div>
    </section>
  );
}

function formatVoiceBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return "Unknown size";
  if (bytes < 1_024) return `${Math.round(bytes)} B`;
  if (bytes < 1_048_576) return `${(bytes / 1_024).toFixed(bytes < 10_240 ? 1 : 0)} KB`;
  return `${(bytes / 1_048_576).toFixed(bytes < 10_485_760 ? 1 : 0)} MB`;
}

function VoiceSessionTitle({ editable, onChange, title }: {
  editable: boolean;
  onChange: (title: string) => void;
  title: string;
}) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(title);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (!editing) setDraft(title);
  }, [editing, title]);

  useEffect(() => {
    if (!editing) return;
    inputRef.current?.focus();
    inputRef.current?.select();
  }, [editing]);

  function commit() {
    const next = draft.trim();
    if (next) onChange(next);
    else setDraft(title);
    setEditing(false);
  }

  if (editing && editable) {
    return (
      <motion.form
        animate={{ opacity: 1, scale: 1 }}
        className="voice-session-title-editor"
        initial={{ opacity: 0, scale: 0.985 }}
        onSubmit={(event) => {
          event.preventDefault();
          commit();
        }}
      >
        <input
          aria-label="Home title"
          maxLength={96}
          onBlur={commit}
          onChange={(event) => setDraft(event.target.value)}
          onKeyDown={(event) => {
            if (event.key !== "Escape") return;
            event.preventDefault();
            setDraft(title);
            setEditing(false);
          }}
          ref={inputRef}
          value={draft}
        />
      </motion.form>
    );
  }

  return (
    <motion.h1
      animate={{ opacity: 1 }}
      initial={{ opacity: 0 }}
      transition={{ duration: 0.18, ease: [0.2, 0, 0, 1] }}
    >
      {editable ? (
        <ShellTooltip label="Rename Home">
          <button aria-label="Rename Home" onClick={() => setEditing(true)} type="button">
            <span>{title}</span>
            <Glyph className="icon-xs" name="edit" size={16} />
          </button>
        </ShellTooltip>
      ) : <span>{title}</span>}
    </motion.h1>
  );
}

function HomeResponseStage({ operatorRequests, onOperatorRequestResponse, replyHidden, onReplyHidden, busy, blockingReason, activity, activityStartedAtMs, toolPresentations, composer, dragOver, dropEnabled, homeInterfaceReady, loading, onDragLeave, onDragOver, onDrop, response, streamingResponse, submitting, failure, turnKey }: {
  operatorRequests: ChatRenderMessage[];
  onOperatorRequestResponse: ChatWorkspace["respondToOperatorRequest"];
  replyHidden: boolean;
  onReplyHidden: () => void;
  busy: boolean;
  blockingReason: string;
  activity: ChatRenderMessage[];
  activityStartedAtMs: number;
  toolPresentations: ChatToolPresentationIndex;
  composer: ReactNode;
  dragOver: boolean;
  dropEnabled: boolean;
  homeInterfaceReady: boolean;
  loading: boolean;
  onDragLeave: ChatWorkspace["onDragLeave"];
  onDragOver: ChatWorkspace["onDragOver"];
  onDrop: ChatWorkspace["dropFiles"];
  response: ChatRenderMessage | null;
  streamingResponse: ChatRenderMessage | null;
  submitting: boolean;
  failure: ChatRenderMessage | null;
  turnKey: string;
}) {
  const stageRef = useRef<HTMLElement>(null);
  const composerFooterRef = useRef<HTMLDivElement>(null);
  const viewportRef = useRef<HTMLDivElement>(null);
  const [scrolled, setScrolled] = useState(false);
  const text = response?.text.trim() ?? "";
  const finalReplyPending = (!response && homeTurnAwaitsFinalReply(activity)) || homeTurnAwaitsClientResult(activity, toolPresentations);
  const activityNotice = finalReplyPending ? null : homeTurnTerminalNotice(activity);
  const working = !blockingReason && (busy || finalReplyPending) && !response && !submitting && !failure && !activityNotice;
  const streamingText = working ? streamingResponse?.text ?? "" : "";
  const responseStreaming = Boolean(streamingText.trim());
  const attachments = response?.attachments ?? [];
  const finalVisible = !submitting && !failure && Boolean(response && (text || attachments.length));
  const responseTimestamp = responseStreaming ? streamingResponse?.createdAtMs : finalVisible ? response?.createdAtMs : undefined;
  const responseDate = responseTimestamp && Number.isFinite(responseTimestamp) && responseTimestamp > 0
    ? new Date(responseTimestamp)
    : null;
  const datedResponse = responseDate && Number.isFinite(responseDate.getTime()) ? responseDate : null;
  const turnFailed = Boolean(failure);
  const quiet = submitting || turnFailed;
  const retainedActivity = !quiet && !loading && (Boolean(activityNotice)
    || activity.some((message) => conversationTraceAdmission(message, toolPresentations) === "visible"));
  const hasResponse = operatorRequests.length > 0 || responseStreaming || working || submitting || finalVisible || turnFailed || retainedActivity;
  const compact = operatorRequests.length > 0 ? false : responseStreaming
    ? isCompactHomeResponse(streamingText, [])
    : submitting
    ? true
    : working
    ? true
    : finalVisible
    ? isCompactHomeResponse(text, attachments)
    : !turnFailed;

  useLayoutEffect(() => {
    const stage = stageRef.current;
    const footer = composerFooterRef.current;
    if (!stage || !footer) return;
    const updateStageMetrics = () => {
      stage.style.setProperty("--home-composer-height", `${Math.ceil(footer.getBoundingClientRect().height)}px`);
      stage.style.setProperty("--home-stage-height", `${Math.floor(stage.clientHeight)}px`);
    };
    updateStageMetrics();
    const observer = new ResizeObserver(updateStageMetrics);
    observer.observe(footer);
    observer.observe(stage);
    return () => {
      observer.disconnect();
      stage.style.removeProperty("--home-composer-height");
      stage.style.removeProperty("--home-stage-height");
    };
  }, []);

  useLayoutEffect(() => {
    const viewport = viewportRef.current;
    if (!viewport) return;
    viewport.scrollTo({ top: 0, behavior: "instant" });
    setScrolled(false);
  }, [turnKey]);

  return (
    <motion.section
      animate={{ opacity: 1 }}
      aria-busy={!blockingReason && (working || submitting)}
      aria-label="Home latest response viewport"
      className={`home-response-stage${working ? " is-generating is-transient" : ""}${hasResponse ? " has-response" : " is-empty"}${compact ? " is-compact" : " is-expanded"}${scrolled ? " is-scrolled" : ""}${dragOver ? " is-drag-over" : ""}${replyHidden ? " is-call" : ""}`}
      initial={{ opacity: 0 }}
      onDragLeave={dropEnabled ? onDragLeave : undefined}
      onDragOver={dropEnabled ? onDragOver : undefined}
      onDrop={dropEnabled ? onDrop : undefined}
      ref={stageRef}
      transition={{ duration: 0.28, ease: [0.23, 1, 0.32, 1] }}
    >
      <span aria-live="polite" className="sr-only">
        {submitting
          ? "Sending to Octopus."
          : turnFailed
            ? "The latest Home turn failed. Review the error banner."
            : working
              ? ""
              : finalVisible
                ? "Octopus replied."
                : ""}
      </span>
      <motion.div
        animate={{ opacity: replyHidden ? 0 : 1 }}
        aria-hidden={replyHidden}
        className="home-response-stage__body"
        inert={replyHidden}
        initial={{ opacity: replyHidden ? 0 : 1 }}
        onAnimationComplete={(definition) => {
          if (replyHidden && typeof definition === "object" && !Array.isArray(definition) && definition.opacity === 0) onReplyHidden();
        }}
        transition={{ duration: replyHidden ? 0.5 : 0.2, ease: [0.2, 0, 0, 1] }}
      >
        <div
          aria-live={working ? "polite" : undefined}
          className="home-response-stage__viewport"
          onScroll={(event) => setScrolled(event.currentTarget.scrollTop > 1)}
          ref={viewportRef}
          tabIndex={hasResponse ? 0 : undefined}
        >
          <div className="home-response-stage__content">
            {failure ? <div className="home-response-failure" role="alert"><strong>Home could not finish this reply</strong><p>{failure.text || failure.label || "Review the failed turn in Home history."}</p></div> : null}
            {operatorRequests.map((message) => <OperatorRequestSurface key={message.id} message={message} onRespond={onOperatorRequestResponse} />)}
            {responseStreaming || finalVisible ? (
              <article
                className={`home-response-output${responseStreaming ? " home-response-output--streaming octopus-assistant-streaming" : ""}`}
                data-ocean-obstacle=""
                data-voice-placement-obstacle=""
                key="assistant-response"
              >
                {datedResponse ? (
                  <header className="home-response-stage__header">
                    <time dateTime={datedResponse.toISOString()}>
                      {formatResponseDay(datedResponse)}
                      {" at "}{datedResponse.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" })}
                    </time>
                  </header>
                ) : null}
                {responseStreaming || text ? (
                  <div className="home-response-output__copy">
                    {responseStreaming ? (
                      <MessageContent streamKey={streamingResponse?.id ?? "home-response"} streaming text={streamingText} />
                    ) : <MessageContent text={text} />}
                  </div>
                ) : null}
                {!responseStreaming && attachments.length ? <HomeResponseAttachments attachments={attachments} /> : null}
              </article>
            ) : working || retainedActivity ? (
              <article className={`home-response-output home-response-output--activity${working ? " home-response-output--live" : ""}`} data-ocean-obstacle="" data-voice-placement-obstacle="" key="activity">
                <HomeTurnActivity activities={activity} blockingReason={blockingReason} startedAtMs={activityStartedAtMs} toolPresentations={toolPresentations} turnInProgress={working} terminalNotice={activityNotice} />
              </article>
            ) : quiet || loading ? null : (
              <div className="home-response-empty" data-ocean-obstacle="" data-voice-placement-obstacle="" key="empty">
                <h2>{homeInterfaceReady ? "What would you like to do?" : "Home is not available yet"}</h2>
                <p>{homeInterfaceReady ? "Talk to Home or send a message." : "The Home connection has not been projected."}</p>
              </div>
            )}
          </div>
        </div>
      </motion.div>
      <div className="home-response-stage__composer-footer" ref={composerFooterRef}>
        <div className="home-response-stage__composer" data-ocean-obstacle="" data-voice-placement-obstacle="">{composer}</div>
      </div>
    </motion.section>
  );
}

function formatResponseDay(at: Date): string {
  const now = new Date();
  const yesterday = new Date(now);
  yesterday.setDate(yesterday.getDate() - 1);
  if (at.toDateString() === now.toDateString()) return "Today";
  if (at.toDateString() === yesterday.toDateString()) return "Yesterday";
  return at.toLocaleDateString([], { day: "numeric", month: "short", year: "numeric" });
}

function isCompactHomeResponse(text: string, attachments: ChatAttachmentView[]): boolean {
  if (attachments.length) return false;
  const lines = text.split(/\r?\n/).filter((line) => line.trim()).length;
  return lines <= 5 && text.length <= 440 && !/\n\s*\|.+\|/.test(text);
}

function HomeResponseAttachments({ attachments }: { attachments: ChatAttachmentView[] }) {
  return (
    <section className="home-response-artifacts" aria-label="Evidence and references">
      <h2><Glyph name="paperclip" size={12} /> Evidence &amp; references</h2>
      <div>
        {attachments.map((attachment) => (
          <article key={attachment.id} title={`${attachment.name}\n${attachment.type || attachment.label}`}>
            <span><Glyph name={attachment.type.startsWith("image/") ? "image" : "file"} size={16} /></span>
            <span>
              <strong>{attachment.name}</strong>
              <small>{[attachment.size > 0 ? formatVoiceBytes(attachment.size) : "", formatResponseArtifactType(attachment)].filter(Boolean).join(" · ")}</small>
            </span>
          </article>
        ))}
      </div>
    </section>
  );
}

function formatResponseArtifactType(attachment: ChatAttachmentView): string {
  const type = attachment.type.trim().toLocaleLowerCase();
  if (type.startsWith("image/")) return type.slice(6).toUpperCase();
  if (type === "application/pdf") return "PDF";
  if (type === "application/zip" || type === "application/x-zip-compressed") return "ZIP";
  if (type.includes("json")) return "JSON";
  if (type.startsWith("text/")) return type === "text/plain" ? "Text" : type.slice(5).toUpperCase();
  return attachment.label || "Artifact";
}

function HomeCommandDock({ awaitingClientResult, callActive, commandsActive, onVoiceStart, voiceStartDisabled, callControls, projectedFileSources, workspace }: {
  awaitingClientResult: boolean;
  callActive: boolean;
  commandsActive: boolean;
  onVoiceStart: () => void;
  voiceStartDisabled: boolean;
  callControls: ReactNode;
  projectedFileSources: readonly ProjectedFileSource[];
  workspace: ChatWorkspace;
}) {
  const [deferredStopAtMs, setDeferredStopAtMs] = useState(0);
  const { interruptActiveTurn, turnOpen } = workspace;
  useEffect(() => {
    if (!deferredStopAtMs) return;
    if (turnOpen) {
      setDeferredStopAtMs(0);
      void interruptActiveTurn();
      return;
    }
    const timer = window.setTimeout(() => setDeferredStopAtMs(0), Math.max(0, deferredStopAtMs + HOME_DEFERRED_STOP_MS - Date.now()));
    return () => window.clearTimeout(timer);
  }, [deferredStopAtMs, interruptActiveTurn, turnOpen]);
  const interrupt = () => {
    if (turnOpen) void interruptActiveTurn();
    else if (awaitingClientResult) setDeferredStopAtMs(Date.now());
  };
  return (
    <div className="home-command-dock">
      {callControls}
      <div hidden={callActive} inert={callActive}>
        <QueuedMessageList
          messages={workspace.queuedTurns}
          paused={workspace.queuePaused}
          sendNowAvailable={workspace.canSendQueued}
          onDelete={workspace.deleteQueuedTurn}
          onEdit={workspace.editQueuedTurn}
          onSendNow={workspace.steerQueuedTurn}
          onReorder={workspace.reorderQueuedTurns}
          onResume={workspace.resumeQueuedTurns}
          onRetry={workspace.retryQueuedTurn}
        />
        <ChatComposer
          accessProfile={workspace.accessProfile}
          activeProvider={workspace.activeProvider}
          attachments={workspace.attachments}
          canSubmit={workspace.canSubmit}
          blockingReason={workspace.runtimeBlockReason}
          contextUsage={workspace.contextUsage}
          hasQueuedMessages={workspace.queuedTurns.length > 0}
          fileInputRef={workspace.fileInputRef}
          interruptPending={workspace.turnControlPending || Boolean(deferredStopAtMs)}
          listenToGlobalCommands={commandsActive && !callActive}
          onFilesPicked={workspace.onFilesPicked}
          onKeyDown={workspace.onComposerKeyDown}
          onInterrupt={interrupt}
          onResume={workspace.resumeActiveTurn}
          onProviderInterfaceChange={workspace.setProviderInterfaceId}
          onProviderModelChange={workspace.setProviderModel}
          onProviderModelsRefresh={workspace.refreshProviderModels}
          onReasoningEffortChange={workspace.setReasoningEffort}
          onServiceTierChange={workspace.chooseServiceTier}
          onRemoveAttachment={workspace.removeAttachment}
          onResolvePausedQueueSubmission={workspace.resolvePausedQueueSubmission}
          onSubmit={workspace.submit}
          onTextChange={workspace.setText}
          onVoiceStart={onVoiceStart}
          placeholder="Message Home"
          providerInterfaceId={workspace.providerInterfaceId}
          providerModel={workspace.providerModel}
          providerInterfaces={workspace.providerInterfaces}
          providerInterfacesLoaded={workspace.providerInterfacesLoaded}
          providerInterfacesRefreshing={workspace.providerInterfacesRefreshing}
          modelSelectionLoading={workspace.modelSelectionLoading}
          projectedFileSources={projectedFileSources}
          pausedQueueSubmissionCount={workspace.pausedQueueSubmissionCount}
          reasoningEffort={workspace.reasoningEffort}
          serviceTier={workspace.serviceTier}
          resumeAvailable={workspace.resumeAvailable}
          runId={workspace.activeRunId}
          suggestionSourceSequence={workspace.activeRun?.lastTerminalMessage?.movementSeq}
          runtimeHome={workspace.runtimeHome}
          isSubmitting={workspace.submitting}
          submissionMode={workspace.busy ? "queue" : "send"}
          canSteer={workspace.canSteer}
          onSteer={workspace.steerCurrentTurn}
          submitBlockReason={workspace.submitBlockReason}
          text={workspace.text}
          turnOpen={workspace.turnOpen || awaitingClientResult || Boolean(deferredStopAtMs)}
          variant="home"
          voiceStartDisabled={voiceStartDisabled}
        />
      </div>
    </div>
  );
}

function HomeWidgetsControl({ accessProfile, errors, hasHiddenSurfaces, onOpenChange, onShowAll, onSurfaceVisibleChange, open, surfaceVisible, surfaces }: {
  accessProfile: OctopusClientAccessProfile;
  errors: readonly string[];
  hasHiddenSurfaces: boolean;
  onOpenChange: (open: boolean) => void;
  onShowAll: () => void;
  onSurfaceVisibleChange: (surface: AdmittedClientSurfaceDocument, visible: boolean) => void;
  open: boolean;
  surfaceVisible: (surface: AdmittedClientSurfaceDocument) => boolean;
  surfaces: AdmittedClientSurfaceDocument[];
}) {
  const rootRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const dismiss = (event: PointerEvent) => {
      if (event.target instanceof Element && event.target.closest("[data-titlebar-surface-action]")) return;
      if (!rootRef.current?.contains(event.target as Node)) onOpenChange(false);
    };
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") onOpenChange(false);
    };
    window.addEventListener("pointerdown", dismiss);
    window.addEventListener("keydown", closeOnEscape);
    return () => {
      window.removeEventListener("pointerdown", dismiss);
      window.removeEventListener("keydown", closeOnEscape);
    };
  }, [onOpenChange, open]);

  return (
    <div className="home-widgets-control" ref={rootRef}>
      <AnimatePresence>
        {open ? (
          <motion.div
            animate={{ opacity: 1, scale: 1, y: 0 }}
            className="home-widgets-popover"
            exit={{ opacity: 0, scale: 0.985, y: 4 }}
            initial={{ opacity: 0, scale: 0.985, y: 4 }}
            transition={{ duration: 0.15, ease: [0.19, 1, 0.22, 1] }}
          >
            <OcMenuSurface aria-label="Home widgets" className="home-widgets-menu-surface" role="menu">
              <OcMenuSectionLabel>Home widgets</OcMenuSectionLabel>
              {errors.map((error, index) => <p className="home-widgets-menu-error" key={index} role="alert">{error}</p>)}
              {surfaces.map((surface) => {
                const visible = surfaceVisible(surface);
                return (
                  <OcMenuItem
                    aria-checked={visible}
                    description={surface.document.target.kind === "voice.window" ? "Window" : "Widget"}
                    key={homeSurfaceIdentity(surface)}
                    label={surface.document.title}
                    leadingIcon={surface.document.glyph}
                    onClick={() => onSurfaceVisibleChange(surface, !visible)}
                    role="menuitemcheckbox"
                    selected={visible}
                  />
                );
              })}
              {surfaces.length === 0 ? <span className="home-widgets-menu-empty">No widgets available.</span> : null}
              <div aria-hidden="true" className="home-widgets-menu-separator" />
              {hasHiddenSurfaces ? <OcMenuItem label="Restore all" leadingIcon="viewAll" onClick={onShowAll} role="menuitem" /> : null}
              <OcMenuItem label="Reset positions" leadingIcon="refresh" onClick={() => {
                try {
                  const previous = resetVoiceWidgetLayout(accessProfile);
                  appToast.info("Home widget positions reset", 8, {
                    action: {
                      label: "Undo",
                      onSelect: () => {
                        try {
                          restoreVoiceWidgetLayout(accessProfile, previous);
                        } catch (failure) {
                          appToast.danger(failure instanceof Error ? failure.message : String(failure));
                        }
                      },
                    },
                    dedupeKey: `voice-widget-layout-reset:${accessProfile}`,
                  });
                } catch (failure) {
                  appToast.danger(failure instanceof Error ? failure.message : String(failure));
                }
              }} role="menuitem" />
            </OcMenuSurface>
          </motion.div>
        ) : null}
      </AnimatePresence>
    </div>
  );
}

function findProjectionTarget(topology: TopologySnapshot, runId: string): ProjectionTarget {
  if (!runId) return { environment: null, run: null };
  for (const environment of topology.environments) {
    const run = environment.runs.find((candidate) => candidate.id === runId);
    if (run) return { environment, run };
  }
  return { environment: null, run: null };
}

function projectContext(target: ProjectionTarget, projectName: string): ProjectionItem[] {
  const run = target.run;
  const artifacts = run?.artifacts || [];
  const files = artifacts.filter((artifact) => /file|path|workspace|attachment|source|code/i.test(
    `${artifact.name} ${artifact.description} ${artifact.tag}`,
  ));
  const recent = artifacts.slice(0, 2).map((artifact) => artifact.name).join("\n");
  const roots = run ? projectedRunRoots(run) : [];
  return [
    { label: "Project", detail: projectName, tone: "accent" },
    { label: "Focus", detail: run?.goal || target.environment?.goal || "No focus" },
    { label: "Files", detail: run ? `${files.length} in context` : "No files" },
    { label: "Recent", detail: recent || "Nothing recent" },
    ...(roots.length
      ? roots.map((root) => ({ label: "Workspace", detail: root }))
      : [{ label: "Workspace", detail: "No workspace" }]),
    ...(run?.interfaces || []).map((item) => ({ label: "Interface", detail: item.name, state: item.status })),
    ...(run?.bindings || []).map((item) => ({ label: "Binding", detail: item.name, state: item.status })),
  ];
}

function projectTools(target: ProjectionTarget): ProjectionItem[] {
  const run = target.run;
  const environments = run?.workerEnvironments.length
    ? run.workerEnvironments
    : run?.workerEnvironment.id
      ? [run.workerEnvironment]
      : [];
  const tools = environments.flatMap((environment) => environment.tools.map((tool) => ({ environment, tool })));
  if (!tools.length) return [{ glyph: "plugins", label: "No active tools", state: "Off" }];
  return tools.map(({ environment, tool }) => ({
    glyph: glyphForTool(tool),
    label: readableTool(tool),
    detail: `${environment.label} · ${tool}`,
    state: normalizeState(environment.status || "Unavailable"),
  }));
}

function projectedRunRoots(run: RunProjection): string[] {
  const environments = run.workerEnvironments.length
    ? run.workerEnvironments
    : run.workerEnvironment.id
      ? [run.workerEnvironment]
      : [];
  const roots = new Set<string>();
  for (const environment of environments) {
    const surfaceRoots = environment.surfaces
      .filter((surface) => surface.kind === "files" && surface.mode === "direct")
      .map((surface) => surface.root.trim())
      .filter(Boolean);
    if (surfaceRoots.length) surfaceRoots.forEach((root) => roots.add(root));
    else if (environment.root.trim()) roots.add(environment.root.trim());
  }
  return [...roots];
}

function projectGoals(target: ProjectionTarget): ProjectionItem[] {
  const values = [target.run?.goal, target.environment?.goal]
    .filter((value): value is string => Boolean(value && value.trim()));
  const unique = [...new Set(values)];
  return unique.length
    ? unique.map((goal) => ({ glyph: "target", label: goal, detail: target.run?.statusLabel || target.environment?.statusLabel }))
    : [{ glyph: "target", label: "No goals", detail: "Nothing is active yet." }];
}

function projectMemory(target: ProjectionTarget): ProjectionItem[] {
  const artifacts = target.run?.artifacts || [];
  const explicitMemory = artifacts.filter((item) => /memory|persona|context/i.test(`${item.name} ${item.description} ${item.tag}`));
  const visible = explicitMemory.length ? explicitMemory : artifacts;
  return visible.map((item) => ({
    glyph: "artifact",
    label: item.name,
    detail: item.description || item.tag,
    state: explicitMemory.length ? item.status : "Bound artifact",
  }));
}

function projectInsights(activity: HomeRunActivity): InsightsWidget {
  const entries = activity.entries.filter((entry) => entry.createdAtMs > 0);
  if (!entries.length) {
    return {
      label: "Home activity",
      value: "0",
      change: "Ready",
      changeDetail: "for the next movement",
      series: [0, 0, 0, 0, 0, 0, 0, 0],
    };
  }
  const first = entries.reduce((minimum, entry) => Math.min(minimum, entry.createdAtMs), entries[0].createdAtMs);
  const last = entries.reduce((maximum, entry) => Math.max(maximum, entry.createdAtMs), entries[0].createdAtMs);
  const duration = Math.max(1, last - first + 1);
  const series = Array.from({ length: 8 }, () => 0);
  for (const entry of entries) {
    const index = Math.min(series.length - 1, Math.floor(((entry.createdAtMs - first) / duration) * series.length));
    series[index] += 1;
  }
  const recent = series.slice(4).reduce((total, value) => total + value, 0);
  const earlier = series.slice(0, 4).reduce((total, value) => total + value, 0);
  const delta = recent - earlier;
  return {
    label: "Home activity",
    value: String(activity.total || entries.length),
    change: delta === 0 ? "Even" : delta > 0 ? `+${delta}` : String(delta),
    changeDetail: "recent movements",
    series,
  };
}

export function projectTaskProgress(
  topology: TopologySnapshot,
  target: ProjectionTarget,
  homeRunId: string,
  chatRuns: ChatRun[],
): TaskProgressWidget {
  const environmentIds = homeEnvironmentIds(topology, target.environment?.id || target.run?.eeId || "");
  const scopedRuns = chatRuns.filter((run) => run.clientSurfaceRole === "task" && run.writable
    && run.runId !== homeRunId && !run.archived && environmentIds.has(run.eeId));
  const now = Date.now();
  let unseenDone = 0, running = 0, waiting = 0, scheduled = 0, failed = 0;
  for (const run of scopedRuns) {
    if (run.turnState === "failed") failed++;
    else if (run.turnOpen) {
      if (run.turnState === "working") running++;
      else waiting++;
    } else if (run.nextScheduledAtMs > now) scheduled++;
    else if (run.lastTerminalMessage) unseenDone++;
    else waiting++;
  }
  const total = unseenDone + running + waiting + failed;
  return {
    completion: total ? (unseenDone / total) * 100 : 0,
    failed,
    running,
    scheduled,
    unseenDone,
    waiting,
  };
}

function projectUpcomingScheduled(chatRuns: ChatRun[]): UpcomingScheduledWidgetItem[] {
  const now = Date.now();
  return chatRuns
    .filter((run) => run.nextScheduledAtMs > now)
    .sort((left, right) => left.nextScheduledAtMs - right.nextScheduledAtMs)
    .map((run) => ({
      at: formatScheduledTime(run.nextScheduledAtMs, now),
      label: run.title,
    }));
}

function homeEnvironmentIds(topology: TopologySnapshot, rootId: string): Set<string> {
  const ids = new Set<string>();
  if (!rootId) return ids;
  ids.add(rootId);
  let changed = true;
  while (changed) {
    changed = false;
    for (const environment of topology.environments) {
      if (!environment.parentId || !ids.has(environment.parentId) || ids.has(environment.id)) continue;
      ids.add(environment.id);
      changed = true;
    }
  }
  return ids;
}

function formatScheduledTime(timestamp: number, now: number): string {
  const at = new Date(timestamp);
  const today = new Date(now);
  const tomorrow = new Date(now);
  tomorrow.setDate(tomorrow.getDate() + 1);
  const sameDay = at.toDateString() === today.toDateString();
  const nextDay = at.toDateString() === tomorrow.toDateString();
  const day = sameDay ? "Today" : nextDay ? "Tomorrow" : at.toLocaleDateString([], { day: "numeric", month: "short" });
  return `${day}, ${at.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" })}`;
}

function glyphForTool(tool: string): GlyphName {
  const value = tool.toLowerCase();
  if (value.includes("browser") || value.includes("web")) return "browser";
  if (value.includes("terminal") || value.includes("exec") || value.includes("shell")) return "terminal";
  if (value.includes("file") || value.includes("read") || value.includes("write")) return "file";
  if (value.includes("code") || value.includes("patch") || value.includes("git")) return "changes";
  if (value.includes("memory") || value.includes("artifact")) return "artifact";
  return "plugins";
}

function readableTool(tool: string): string {
  const tail = tool.split(/[./:]/).filter(Boolean).pop() || tool;
  return tail.replace(/[_-]+/g, " ").replace(/\b\w/g, (letter) => letter.toUpperCase());
}

function normalizeState(value: string): string {
  const normalized = value.trim().toLowerCase();
  if (["active", "running", "ready", "attached"].includes(normalized)) return "Ready";
  if (["idle", "parked"].includes(normalized)) return "Standby";
  if (!normalized || normalized === "unknown") return "Unavailable";
  return normalized.replace(/\b\w/g, (letter) => letter.toUpperCase());
}

function homeEnvironmentStatus(target: ProjectionTarget, loading: boolean): { label: string; tone: string } {
  if (loading) return { label: "", tone: "pending" };
  const environment = target.environment;
  const label = environment?.name || environment?.goal || target.run?.eeId || "Execution environment unavailable";
  if (environment?.status === "active") return { label, tone: "local" };
  if (environment?.status === "blocked") return { label, tone: "error" };
  if (environment?.status === "idle") return { label, tone: "muted" };
  return { label, tone: "idle" };
}

function shortLabel(value: string, limit: number): string {
  const compact = value.trim();
  return compact.length > limit ? `${compact.slice(0, Math.max(1, limit - 1))}…` : compact;
}

function readStoredText(key: string, fallback: string): string {
  const value = window.localStorage.getItem(key)?.trim();
  return value || fallback;
}

function readStoredBoolean(key: string, fallback: boolean): boolean {
  const value = window.localStorage.getItem(key);
  if (value === "true") return true;
  if (value === "false") return false;
  return fallback;
}

function readStoredStringSet(key: string): Set<string> | null {
  const raw = window.localStorage.getItem(key);
  if (raw === null) return null;
  const value: unknown = JSON.parse(raw);
  if (!Array.isArray(value) || value.some((item) => typeof item !== "string")) {
    throw new Error("Saved Home widget visibility is malformed");
  }
  return new Set(value);
}

function voiceStorageKeys(accessProfile: OctopusClientAccessProfile) {
  return {
    title: `${VOICE_TITLE_KEY}.${accessProfile}`,
    recording: `${VOICE_RECORDING_KEY}.${accessProfile}`,
    speaker: `${VOICE_SPEAKER_KEY}.${accessProfile}`,
    hiddenSurfaces: `${VOICE_HIDDEN_SURFACES_KEY}.${accessProfile}`,
  };
}
