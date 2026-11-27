import {
  ChatComposer,
  ChatMessageList,
  ChatRenderBoundary,
  ChatThreadHeader,
  ChatThreadHeaderActions,
  QueuedMessageList,
  TaskSummaryPanel,
  type ChatWorkspace,
} from "./chat";
import "./icon-glyphs/chat";
import "./icon-glyphs/worker";
import type { ChatRenderMessage } from "./chat/types";
import { markdownToPlainText } from "./chat/markdownPresentation";
import type { ChatPinnedContextItem, ChatRun } from "../chatRuns";
import { AnimatePresence, motion } from "motion/react";
import { useCallback, useEffect, useMemo, useRef, useState, type KeyboardEvent as ReactKeyboardEvent, type RefObject } from "react";
import type { RunProjection, TopologySnapshot, WorkerEnvironmentProjection } from "../data/topology";
import type { AdmittedClientSurfaceDocument } from "../client-surfaces/types";
import { copyText } from "../runtimeFormat";
import { appToast, useAppErrorToast } from "../toasts";
import { Glyph } from "./IconGlyphs";
import { clientDialogOwnsCommands } from "../keyboardShortcuts";
import { workerEnvironmentPanelKind, type WorkerEnvironmentPanelKind } from "./worker-environment/surfaceRegistry";
import { useClientPreferences } from "../clientPreferences";
import { subscribeClientCommands } from "../clientActions";
import { CHAT_MESSAGE_REVEAL_EVENT, consumeChatMessageReveal } from "../chatMessageReveal";
import { OcButton, OcIconButton } from "../design-system/components";
import { dispatchRestoreClientTask } from "../shell/taskActionsMenu";
import type { ProjectedFileSource } from "./chat/ProjectedFilePicker";
import { createReviewTaskSeed } from "./chat/reviewTask";
import { dispatchOpenTaskSourcesPane } from "./chat/taskSourcesPane";
import { WorkspacePaneHeaderActions, useWorkspacePaneHeaderActionsHost } from "./shell/WorkspacePaneHeaderActions";
import { recordClientTaskRead } from "../shell/clientWorkspaces";
import { chatProductError } from "./chat/chatProductError";
import type { FilesOpenTarget } from "./files/types";
import { FilesPaneNavigationProvider } from "./files/FilesPaneNavigation";
import {
  directivePathWithinRoot,
  type AssistantVisualizationTarget,
} from "./chat/assistantDirectives";
import {
  defaultWorkerEnvironmentPaneTarget,
  dispatchOpenWorkerEnvironmentPane,
  type WorkerEnvironmentPaneTarget,
} from "./worker-environment/workerEnvironmentPane";
import "./chat/chat.css";
import { useAppearance } from "../appearance";
import { OctopusSwarm } from "./voice/OctopusSwarm";

export function ChatView({
  workspace: chat,
  commandsActive = true,
  searchFocusTick,
  topology,
  surfaces,
  activityPresentation = "conversation",
  composerVisible = true,
  headerMode = "window",
  onOpenFiles,
}: {
  workspace: ChatWorkspace;
  commandsActive?: boolean;
  searchFocusTick: number;
  topology: TopologySnapshot;
  surfaces: AdmittedClientSurfaceDocument[];
  activityPresentation?: "conversation" | "ledger";
  composerVisible?: boolean;
  headerMode?: "window" | "pane" | "none";
  onOpenFiles: (target: FilesOpenTarget) => void;
}) {
  const headerActionsHost = useWorkspacePaneHeaderActionsHost();
  const clientPreferences = useClientPreferences();
  const { octopuses } = useAppearance();
  const [summaryOpen, setSummaryOpen] = useState(false);
  const [headerEdgeScrolled, setHeaderEdgeScrolled] = useState(false);
  const [summaryDisplayMode, setSummaryDisplayMode] = useState<"overlay" | "shift" | "gutter">(() => taskSummaryDisplayMode(window.innerWidth));
  const [findOpen, setFindOpen] = useState(false);
  const [findMatchCount, setFindMatchCount] = useState(0);
  const [activeFindIndex, setActiveFindIndex] = useState(0);
  const [pinnedContextOpen, setPinnedContextOpen] = useState(false);
  const [revealedPinnedContext, setRevealedPinnedContext] = useState<ChatPinnedContextItem | null>(null);
  const layoutRef = useRef<HTMLDivElement>(null);
  const findInputRef = useRef<HTMLInputElement>(null);
  const reviewTaskPreparingRef = useRef(false);
  const activeRunProjection = useMemo(() => {
    if (!chat.activeRunId) return null;
    for (const environment of topology.environments) {
      const run = environment.runs.find((candidate) => candidate.id === chat.activeRunId);
      if (run) return run;
    }
    return null;
  }, [chat.activeRunId, topology.environments]);
  const openTraceFile = useCallback((path: string, toolId?: string) => {
    try {
      const target = traceFileOpenTarget(path, activeRunProjection, toolId);
      if ("workerEnvironmentId" in target) dispatchOpenWorkerEnvironmentPane(target);
      else onOpenFiles(target);
    } catch (error) {
      appToast.warning(error instanceof Error ? error.message : String(error));
    }
  }, [activeRunProjection, onOpenFiles]);
  useEffect(() => {
    if (!chat.activeRun) return;
    recordClientTaskRead(chat.accessProfile, chat.activeRun.runId, chat.activeRun.updatedAtMs);
  }, [chat.accessProfile, chat.activeRun?.runId, chat.activeRun?.updatedAtMs]);

  const activeWorkerEnvironments = useMemo(() => {
    if (!activeRunProjection) return [];
    if (activeRunProjection.workerEnvironments.length) return activeRunProjection.workerEnvironments;
    return activeRunProjection.workerEnvironment.id ? [activeRunProjection.workerEnvironment] : [];
  }, [activeRunProjection]);
  const projectedFileSources = useMemo<ProjectedFileSource[]>(() => {
    if (chat.accessProfile !== "live" || !window.octopusTransport?.readWorkerEnvironmentFileAttachment || !activeRunProjection) return [];
    const environments = activeRunProjection.workerEnvironments.length
      ? activeRunProjection.workerEnvironments
      : activeRunProjection.workerEnvironment.id
        ? [activeRunProjection.workerEnvironment]
        : [];
    return environments.flatMap((environment) => (
      environment.surfaces.filter((candidate) => (
        candidate.kind === "files"
        && candidate.mode === "direct"
        && (
          (candidate.protocol.trim().toLowerCase() === "filesystem.readonly" && candidate.readOnly)
          || (candidate.protocol.trim().toLowerCase() === "filesystem.readwrite" && !candidate.readOnly)
        )
      )).map((surface) => ({
          workerEnvironmentId: environment.id,
          surfaceId: surface.id,
          label: `${environment.label} · ${surface.label}`,
        }))
    ));
  }, [activeRunProjection, chat.accessProfile]);

  useEffect(() => {
    if (!chat.activeRunId) {
      setSummaryOpen(false);
    }
  }, [chat.activeRunId]);

  useEffect(() => {
    if (chat.pinnedMessages.length === 0 && chat.pinnedContextItems.length === 0 && !chat.continuation) setPinnedContextOpen(false);
  }, [chat.continuation, chat.pinnedContextItems.length, chat.pinnedMessages.length]);

  useEffect(() => {
    const layout = layoutRef.current;
    if (!layout) return;
    const update = (width: number) => setSummaryDisplayMode(taskSummaryDisplayMode(width));
    update(layout.getBoundingClientRect().width);
    const observer = new ResizeObserver((entries) => {
      const width = entries[0]?.contentRect.width;
      if (typeof width === "number") update(width);
    });
    observer.observe(layout);
    return () => observer.disconnect();
  }, []);

  useEffect(() => {
    if (!summaryOpen || summaryDisplayMode !== "overlay") return;
    const dismiss = (event: PointerEvent) => {
      const target = event.target;
      if (!(target instanceof Element)) return;
      if (target.closest("[data-task-summary-popover], [data-task-summary-trigger]")) return;
      setSummaryOpen(false);
    };
    const dismissWithKeyboard = (event: globalThis.KeyboardEvent) => {
      if (event.key !== "Escape" || event.defaultPrevented || !commandsActive || clientDialogOwnsCommands()) return;
      event.preventDefault();
      setSummaryOpen(false);
    };
    window.addEventListener("pointerdown", dismiss);
    window.addEventListener("keydown", dismissWithKeyboard, true);
    return () => {
      window.removeEventListener("pointerdown", dismiss);
      window.removeEventListener("keydown", dismissWithKeyboard, true);
    };
  }, [commandsActive, summaryDisplayMode, summaryOpen]);

  useEffect(() => {
    if (searchFocusTick <= 0) return;
    setFindOpen(true);
    const frame = window.requestAnimationFrame(() => {
      findInputRef.current?.focus();
      findInputRef.current?.select();
    });
    return () => window.cancelAnimationFrame(frame);
  }, [searchFocusTick]);

  useEffect(() => {
    setActiveFindIndex(0);
  }, [chat.messageQuery]);

  useEffect(() => {
    const reveal = () => {
      if (!commandsActive || !chat.activeRunId) return;
      const request = consumeChatMessageReveal(chat.activeRunId);
      if (!request) return;
      window.requestAnimationFrame(() => {
        const target = Array.from(layoutRef.current?.querySelectorAll<HTMLElement>("[data-message-id]") ?? [])
          .find((element) => element.dataset.messageId === request.messageId);
        if (!target) return;
        target.scrollIntoView({ behavior: "smooth", block: "center" });
        target.classList.remove("octopus-message-search-reveal");
        void target.offsetWidth;
        target.classList.add("octopus-message-search-reveal");
        window.setTimeout(() => target.classList.remove("octopus-message-search-reveal"), 1_600);
      });
    };
    reveal();
    window.addEventListener(CHAT_MESSAGE_REVEAL_EVENT, reveal);
    return () => window.removeEventListener(CHAT_MESSAGE_REVEAL_EVENT, reveal);
  }, [chat.activeRunId, chat.renderedMessages.length, commandsActive]);

  useEffect(() => {
    setActiveFindIndex((current) => findMatchCount > 0 ? Math.min(current, findMatchCount - 1) : 0);
  }, [findMatchCount]);

  useEffect(() => subscribeClientCommands((command) => {
    if (!commandsActive) return;
    if (command === "toggle_task_summary") {
      if (!chat.activeRun) return;
      setSummaryOpen((current) => !current);
      return;
    }
    if (command === "copy_run_id") {
      copyActiveRunId();
      return;
    }
    if (command === "copy_working_directory") {
      copyActiveWorkingDirectory();
      return;
    }
    if (command === "review_changes") {
      void startReviewTask();
      return;
    }
    if (command !== "copy_task_markdown" || !chat.activeRun) return;
    const projected = chat.renderedMessages.filter((message) => !message.pending && message.runId === chat.activeRunId);
    void copyText(taskMarkdown(chat.activeRun, projected)).then((copied) => {
      if (copied) appToast.success("Task copied as Markdown");
      else appToast.danger("Could not copy the task");
    });
  }), [activeRunProjection, chat.activeRun, chat.activeRunId, chat.renderedMessages, commandsActive]);

  async function startReviewTask() {
    if (!activeRunProjection) {
      appToast.warning("The active task has no projected run to review.");
      return;
    }
    if (reviewTaskPreparingRef.current) return;
    reviewTaskPreparingRef.current = true;
    try {
      chat.startReviewTask(await createReviewTaskSeed(activeRunProjection));
    } catch (failure) {
      appToast.danger(failure instanceof Error ? failure.message : String(failure));
    } finally {
      reviewTaskPreparingRef.current = false;
    }
  }

  function openEnvironmentCapability(
    environmentId: string,
    panelKind?: WorkerEnvironmentPanelKind,
    surfaceId?: string,
    clientSurface?: WorkerEnvironmentPaneTarget["clientSurface"],
  ) {
    const environment = activeWorkerEnvironments.find((candidate) => candidate.id === environmentId);
    if (!environment) {
      appToast.warning("This task no longer projects that Worker Environment.");
      return;
    }
    dispatchOpenWorkerEnvironmentPane(panelKind || surfaceId || clientSurface
      ? { workerEnvironmentId: environmentId, panelKind, surfaceId, clientSurface }
      : defaultWorkerEnvironmentPaneTarget(environment, surfaces));
    setSummaryOpen(false);
  }

  function resolveAssistantVisualization(path: string): AssistantVisualizationTarget | null {
    for (const environment of activeWorkerEnvironments) {
      for (const surface of environment.surfaces) {
        if (workerEnvironmentPanelKind(surface) !== "files" || surface.mode !== "direct") continue;
        const resolvedPath = directivePathWithinRoot(path, surface.root);
        if (resolvedPath === null || !resolvedPath.toLowerCase().endsWith(".html")) continue;
        return { path: resolvedPath, revision: environment.revision, surfaceId: surface.id, workerEnvironmentId: environment.id };
      }
    }
    return null;
  }

  function openAssistantVisualization(path: string) {
    const target = resolveAssistantVisualization(path);
    if (!target) {
      appToast.warning("This task has no Files surface that can open the visualization.");
      return;
    }
    dispatchOpenWorkerEnvironmentPane({
      workerEnvironmentId: target.workerEnvironmentId,
      panelKind: "file",
      surfaceId: target.surfaceId,
      filePath: target.path,
    });
  }

  function openAssistantReview() {
    for (const environment of activeWorkerEnvironments) {
      const surface = environment.surfaces.find((candidate) => workerEnvironmentPanelKind(candidate) === "changes");
      if (!surface) continue;
      dispatchOpenWorkerEnvironmentPane({
        workerEnvironmentId: environment.id,
        panelKind: "changes",
        surfaceId: surface.id,
      });
      return;
    }
    appToast.warning("This task has no Changes surface for inline review comments.");
  }

  function setSummaryPanelOpen(open: boolean) {
    setSummaryOpen(open);
  }

  function revealTaskMessage(messageId: string) {
    const target = Array.from(document.querySelectorAll<HTMLElement>("[data-message-id]"))
      .find((element) => element.dataset.messageId === messageId);
    if (!target) return;
    target.scrollIntoView({ behavior: "smooth", block: "center" });
    target.classList.remove("octopus-message-search-reveal");
    void target.offsetWidth;
    target.classList.add("octopus-message-search-reveal");
    window.setTimeout(() => target.classList.remove("octopus-message-search-reveal"), 1_600);
  }

  function moveFindResult(direction: -1 | 1) {
    if (findMatchCount <= 0) return;
    setActiveFindIndex((current) => (current + direction + findMatchCount) % findMatchCount);
  }

  function closeFind() {
    setFindOpen(false);
    setFindMatchCount(0);
    setActiveFindIndex(0);
    chat.setMessageQuery("");
  }

  function copyActiveRunId() {
    if (!chat.activeRunId) return;
    void copyText(chat.activeRunId).then((copied) => {
      if (copied) appToast.success("Run ID copied");
      else appToast.danger("Could not copy the run ID");
    });
  }

  function copyActiveWorkingDirectory() {
    const roots = projectedWorkerEnvironmentRoots(activeRunProjection);
    if (!roots.length) {
      appToast.warning("No projected working directory is available for this task");
      return;
    }
    void copyText(roots.join("\n")).then((copied) => {
      if (copied) appToast.success(roots.length === 1 ? "Working directory copied" : `${roots.length} working directories copied`);
      else appToast.danger(roots.length === 1 ? "Could not copy the working directory" : "Could not copy the working directories");
    });
  }

  async function copyActiveTaskMarkdown() {
    if (!chat.activeRun) return;
    const projected = chat.renderedMessages.filter((message) => !message.pending && message.runId === chat.activeRunId);
    const content = taskMarkdown(chat.activeRun, projected);
    if (await copyText(content)) appToast.success("Task copied as Markdown");
    else appToast.danger("Could not copy task Markdown");
  }

  useAppErrorToast(chatProductError(chat));

  const dockedRightWidth = summaryOpen && summaryDisplayMode !== "overlay" ? 316 : 0;
  const summaryContentX = summaryOpen && summaryDisplayMode === "shift" ? -158 : 0;
  const taskSummary = chat.activeRun ? (
    <ChatRenderBoundary
      compact
      detail="The task itself is unchanged. Retry this local summary or copy the renderer details."
      resetKey={`${chat.activeRun.runId}:${chat.renderedMessages.length}:${activeRunProjection?.eventLog.length ?? 0}:${chat.queuedTurns.length}`}
      title="Summary panel couldn't render"
    >
      <TaskSummaryPanel
        active={summaryOpen}
        surfaces={surfaces}
        key={chat.activeRun.runId}
        messages={chat.renderedMessages.filter((message) => !message.runId || message.runId === chat.activeRunId)}
        runProjection={activeRunProjection}
        onOpenEnvironment={openEnvironmentCapability}
        onRevealMessage={revealTaskMessage}
        onShowAllSources={(sources) => {
          dispatchOpenTaskSourcesPane(chat.activeRun!.runId, sources);
          setSummaryOpen(false);
        }}
      />
    </ChatRenderBoundary>
  ) : null;
  const continuationSource = chat.continuation
    ? chat.runs.find((run) => run.runId === chat.continuation?.sourceRunId) ?? null
    : null;

  const composer = (
    <ChatComposer
        accessProfile={chat.accessProfile}
        activeProvider={chat.activeProvider}
        agentPresetId={chat.agentPresetId}
        accessMode={chat.accessMode}
        fullAccessUnavailableReason={chat.fullAccessUnavailableReason}
        agentPresets={chat.composerAgentPresets}
        attachments={chat.attachments}
        applicationIds={chat.applicationIds}
        applicationSelectionLocked={chat.applicationSelectionLocked}
        applicationSelectionLockReason={chat.applicationSelectionLockReason}
        canSubmit={chat.canSubmit}
        blockingReason={chat.runtimeBlockReason}
        contextUsage={chat.contextUsage}
        canQueue={chat.canQueue}
        onQueue={chat.queueCurrentTurn}
        editingQueuedTurn={chat.editingQueuedTurn}
        hasQueuedMessages={chat.queuedTurns.length > 0}
        onCancelQueuedEdit={chat.cancelQueuedEdit}
        eeId={chat.eeId}
        executionContextLocked={Boolean(chat.activeRunId)}
        executionContexts={chat.executionContexts}
        driverNames={chat.driverNames}
        fileInputRef={chat.fileInputRef}
        interruptPending={chat.turnControlPending}
        isSubmitting={chat.submitting}
        listenToGlobalCommands={commandsActive}
        onFilesPicked={chat.onFilesPicked}
        onAgentPresetChange={chat.setAgentPresetId}
        onAccessModeChange={chat.setAccessMode}
        onEeChange={chat.setEeId}
        onKeyDown={chat.onComposerKeyDown}
        onInterrupt={chat.interruptActiveTurn}
        onResume={chat.resumeActiveTurn}
        onProviderInterfaceChange={chat.setProviderInterfaceId}
        onProviderModelChange={chat.setProviderModel}
        onProviderModelsRefresh={chat.refreshProviderModels}
        onReasoningEffortChange={chat.setReasoningEffort}
        onRemoveAttachment={chat.removeAttachment}
        onResolvePausedQueueSubmission={chat.resolvePausedQueueSubmission}
        onSubmit={chat.submit}
        canSteer={chat.canSteer}
        onSteer={chat.steerCurrentTurn}
        onTextChange={chat.setText}
        onApplicationToggle={chat.toggleApplication}
        onDriverToggle={chat.toggleDriver}
        providerInterfaceId={chat.providerInterfaceId}
        providerModel={chat.providerModel}
        providerInterfaces={chat.providerInterfaces}
        providerInterfacesLoaded={chat.providerInterfacesLoaded}
        providerInterfacesRefreshing={chat.providerInterfacesRefreshing}
        modelSelectionLoading={chat.modelSelectionLoading}
        projectedFileSources={projectedFileSources}
        pausedQueueSubmissionCount={chat.pausedQueueSubmissionCount}
        reasoningEffort={chat.reasoningEffort}
        serviceTier={chat.serviceTier}
        onServiceTierChange={chat.chooseServiceTier}
        resumeAvailable={chat.resumeAvailable}
        runId={chat.activeRunId}
        suggestionSourceSequence={chat.activeRun?.lastTerminalMessage?.movementSeq}
        runtimeHome={chat.runtimeHome}
        submissionMode={chat.busy ? "queue" : "send"}
        submitBlockReason={chat.submitBlockReason}
        text={chat.text}
        turnOpen={chat.turnOpen}
        workerEnvironmentRooms={chat.workerEnvironmentRooms}
        workerInterfaceId={chat.workerInterfaceId}
      />
  );

  const composerStack = (
    <div className="chat-composer-queue-stack flex min-w-0 flex-col">
      {chat.continuation ? (
        <div className="chat-continuation-banner">
          <span className="chat-continuation-leading"><Glyph name="worktree" size={15} /></span>
          <span className="chat-continuation-copy">
            <span>Starting from</span>
            <strong title={continuationSource?.title || chat.continuation.sourceRunId}>{continuationSource?.title || chat.continuation.sourceRunId}</strong>
          </span>
          <OcIconButton glyph="close" label="Remove starting context" onClick={chat.clearContinuation} size="xs" variant="ghost" />
        </div>
      ) : null}
      <QueuedMessageList
        messages={chat.queuedTurns}
        paused={chat.queuePaused}
        sendNowAvailable={chat.canSendQueued}
        onDelete={chat.deleteQueuedTurn}
        onEdit={chat.editQueuedTurn}
        onSendNow={chat.steerQueuedTurn}
        onReorder={chat.reorderQueuedTurns}
        onResume={chat.resumeQueuedTurns}
        onRetry={chat.retryQueuedTurn}
      />
      {composer}
    </div>
  );

  const threadMode = Boolean(chat.activeRunId) || chat.activeMessageCount > 0;
  const composerFooter = chat.activeRun?.archived ? (
    <div className="archived-task-footer">
      <Glyph name="archive" size={18} />
      <div><strong>Archived task</strong><span>New messages and artifacts are blocked.</span></div>
      <OcButton variant="ghost" size="sm" onClick={() => dispatchRestoreClientTask(chat.activeRunId)}>Restore task</OcButton>
    </div>
  ) : !composerVisible ? null : threadMode ? (
    <div className="relative z-10 mx-auto flex w-full max-w-(--thread-content-max-width) flex-col px-toolbar">
      <div className="flex flex-col">
        <div className="flex flex-col gap-2">
          <div className="min-w-0">{composerStack}</div>
        </div>
      </div>
    </div>
  ) : (
    <div className="min-w-0 w-full" data-dropdown-overlay-side="top">{composerStack}</div>
  );

  const activeProjectContext = useMemo(() => {
    if (!chat.activeRun) return null;
    const context = chat.executionContexts.find((candidate) => candidate.eeId === chat.activeRun?.eeId && !candidate.isOsRoot);
    return context ? { color: context.presentationColor, icon: context.presentationIcon } : null;
  }, [chat.activeRun, chat.executionContexts]);
  const showInlineThreadHeader = headerMode === "pane" || (headerMode === "window" && Boolean(chat.activeRun));

  return (
    <FilesPaneNavigationProvider onOpen={onOpenFiles}>
    <section className={chatViewClassName(composerVisible, headerMode)}>
      <main className="chat-thread relative isolate flex min-h-0 flex-1 flex-col">
        {!showInlineThreadHeader ? <div
          aria-hidden="true"
          className="chat-thread-top-fade"
          data-app-shell-main-content-top-fade={headerEdgeScrolled ? "visible" : undefined}
        /> : null}
        {showInlineThreadHeader ? <ChatThreadHeader
          actionsVisible={headerMode === "window" ? true : !headerActionsHost}
          accessProfile={chat.accessProfile}
          activeRun={chat.activeRun}
          edgeScrolled={headerEdgeScrolled}
          pinnedContextCount={chat.pinnedMessages.length + chat.pinnedContextItems.length + (chat.continuation ? 1 : 0)}
          pinnedContextOpen={pinnedContextOpen}
          projectContext={activeProjectContext}
          summaryDisplayMode={summaryDisplayMode}
          summaryOpen={summaryOpen}
          summaryPopoverContent={taskSummary}
          mode={headerMode === "pane" ? "pane" : "window"}
          onPinnedContextOpenChange={setPinnedContextOpen}
          onSummaryOpenChange={setSummaryPanelOpen}
        /> : null}
        {headerMode === "pane" && headerActionsHost ? (
          <WorkspacePaneHeaderActions>
            <ChatThreadHeaderActions
              activeRun={chat.activeRun}
              pinnedContextCount={chat.pinnedMessages.length + chat.pinnedContextItems.length + (chat.continuation ? 1 : 0)}
              pinnedContextOpen={pinnedContextOpen}
              summaryDisplayMode={summaryDisplayMode}
              summaryOpen={summaryOpen}
              summaryPopoverContent={taskSummary}
              onPinnedContextOpenChange={setPinnedContextOpen}
              onSummaryOpenChange={setSummaryPanelOpen}
            />
          </WorkspacePaneHeaderActions>
        ) : null}

        <AnimatePresence initial={false}>
          {pinnedContextOpen && (chat.pinnedMessages.length || chat.pinnedContextItems.length || chat.continuation) ? (
            <PinnedContextPanel
              continuation={chat.continuation}
              items={chat.pinnedContextItems}
              messages={chat.pinnedMessages}
              onUnpin={chat.togglePinnedMessage}
              onUnpinItem={chat.unpinContextItem}
              onReveal={(item) => {
                setRevealedPinnedContext(item);
              }}
              rightInset={dockedRightWidth ? dockedRightWidth + 12 : 12}
            />
          ) : null}
        </AnimatePresence>

        {findOpen ? (
          <ThreadFindBar
            activeIndex={activeFindIndex}
            count={findMatchCount}
            inputRef={findInputRef}
            onChange={chat.setMessageQuery}
            onClose={closeFind}
            onMove={moveFindResult}
            rightInset={dockedRightWidth ? dockedRightWidth + 16 : 16}
            value={chat.messageQuery}
          />
        ) : null}

        <div className="chat-run-layout" ref={layoutRef}>
          <div className="chat-conversation-pane">
            <ChatMessageList
              activityPresentation={activityPresentation}
              activeMessageCount={chat.loadingMessages ? 0 : chat.activeMessageCount}
              conversationId={chat.activeRunId}
              contentX={summaryContentX}
              dragOver={composerVisible && chat.dragOver}
              dropEnabled={composerVisible}
              emptyLabel={chat.activeRun?.archived ? "No messages in this archived task" : undefined}
              emptyState={chat.loadingMessages ? "loading-task" : chat.activeRunId || !composerVisible ? "empty-task" : "new-task"}
              backdrop={octopuses ? <OctopusSwarm runs={chat.runs} /> : undefined}
              findActiveIndex={activeFindIndex}
              findQuery={findOpen ? chat.messageQuery : ""}
              messages={chat.loadingMessages ? [] : chat.renderedMessages}
              listenToGlobalCommands={commandsActive}
              pinnedArtifactIds={chat.activeRun?.pinnedArtifactIds ?? []}
              pinningArtifactIds={chat.pinningArtifactIds}
              taskActionsEnabled={composerVisible}
              toolPresentations={chat.toolPresentations}
              onContinueInNewTask={chat.continueInNewTask}
              onEditUserMessage={chat.editUserMessage}
              onSteerUserMessage={chat.steerUserMessage}
              steerableMessageId={chat.steerableMessageId}
              onOperatorRequestResponse={chat.respondToOperatorRequest}
              onOpenAssistantReview={openAssistantReview}
              onOpenAssistantVisualization={openAssistantVisualization}
              onOpenFile={openTraceFile}
              onTogglePinnedMessage={chat.togglePinnedMessage}
              onPinMessageExcerpt={chat.pinMessageExcerpt}
              revealPinnedContext={revealedPinnedContext}
              responseStreaming={chat.busy}
              blockingReason={chat.runtimeBlockReason}
              resolveAssistantVisualization={resolveAssistantVisualization}
              onPinnedContextRevealed={() => setRevealedPinnedContext(null)}
              onFindMatchCountChange={setFindMatchCount}
              onHeaderEdgeScrollChange={setHeaderEdgeScrolled}
              onDragLeave={chat.onDragLeave}
              onDragOver={chat.onDragOver}
              onDrop={chat.dropFiles}
              footer={composerFooter}
            />
          </div>
          {taskSummary && summaryDisplayMode !== "overlay" ? (
            <div
              className={`task-summary-surface${summaryOpen ? " is-open" : ""}`}
              data-display-mode={summaryDisplayMode}
            >
              <div className="task-summary-surface-inner">
                <motion.aside
                  animate={summaryOpen
                    ? { opacity: 1, scale: 1, x: 0 }
                    : { opacity: 0, scale: 0.8, x: "100%" }}
                  aria-hidden={!summaryOpen}
                  className="task-summary-motion"
                  inert={summaryOpen ? undefined : true}
                  initial={false}
                  transition={{ bounce: 0.01, duration: 0.3, type: "spring" }}
                >
                  {taskSummary}
                </motion.aside>
              </div>
            </div>
          ) : null}
        </div>
      </main>
    </section>
    </FilesPaneNavigationProvider>
  );
}

function chatViewClassName(composerVisible: boolean, headerMode: "window" | "pane" | "none"): string {
  return `chat-view${composerVisible ? "" : " is-read-only"}${headerMode === "pane" ? " has-pane-header" : " is-embedded"}`;
}

function traceFileOpenTarget(value: string, run: RunProjection | null, toolId?: string): FilesOpenTarget | Omit<WorkerEnvironmentPaneTarget, "schemaVersion"> {
  const path = value.trim();
  const environments = run?.workerEnvironments.length
    ? run.workerEnvironments
    : run?.workerEnvironment.id ? [run.workerEnvironment] : [];
  function targetForAbsolutePath(absolutePath: string): FilesOpenTarget | Omit<WorkerEnvironmentPaneTarget, "schemaVersion"> {
    const normalized = absolutePath.replaceAll("\\", "/");
    const matches = environments.filter((environment) => !toolId || environment.tools.includes(toolId))
      .flatMap((environment) => environment.surfaces.flatMap((surface) => {
        if (surface.kind !== "files" || surface.mode !== "direct" || !surface.root.trim()) return [];
        const root = surface.root.trim().replaceAll("\\", "/").replace(/\/+$/, "");
        if (normalized !== root && !normalized.startsWith(`${root}/`)) return [];
        const filePath = normalized.slice(root.length).replace(/^\//, "");
        if (filePath.split("/").includes("..") || filePath.includes("\0")) throw new Error("Invalid file path.");
        return [{ root, target: { workerEnvironmentId: environment.id, surfaceId: surface.id, panelKind: "file" as const, filePath } }];
      })).sort((a, b) => b.root.length - a.root.length);
    return matches[0]?.target ?? { absolutePath, kind: "file" };
  }
  if (/^(?:\/|[A-Za-z]:[\\/])/.test(path)) return targetForAbsolutePath(path);
  const relative = path.replaceAll("\\", "/").replace(/^\.\/+/, "");
  if (!relative || relative.includes("\0") || relative.split("/").includes("..")) {
    throw new Error("This file link does not identify a path inside its working directory.");
  }
  const roots = [...new Set(environments
    .filter((environment) => !toolId || environment.tools.includes(toolId))
    .flatMap((environment) => environment.surfaces
      .filter((surface) => surface.kind === "files" && surface.mode === "direct"
        && surface.root.trim().replaceAll("\\", "/").replace(/\/+$/, "")
          === environment.root.trim().replaceAll("\\", "/").replace(/\/+$/, ""))
      .map((surface) => surface.root.trim().replaceAll("\\", "/"))
      .filter((root) => /^(?:\/|[A-Za-z]:\/)/.test(root))
      .map((root) => `${root.replace(/\/+$/, "")}/`)))];
  if (roots.length !== 1) {
    throw new Error(roots.length
      ? "This file has more than one working directory. Open it from the task's Files view."
      : "This file's working directory is unavailable. Open it from the task's Files view.");
  }
  return targetForAbsolutePath(`${roots[0]}${relative}`);
}

function projectedWorkerEnvironmentRoots(
  run: RunProjection | null,
): string[] {
  const environments = run?.workerEnvironments.length
    ? run.workerEnvironments
    : run?.workerEnvironment.id
      ? [run.workerEnvironment]
      : [];
  const roots = new Set<string>();
  for (const environment of environments) {
    const surfaceRoots = environment.surfaces
      .filter((surface) => surface.kind === "files" && surface.mode === "direct")
      .map((surface) => surface.root.trim())
      .filter(validProjectedRoot);
    if (surfaceRoots.length) surfaceRoots.forEach((root) => roots.add(root));
    else if (validProjectedRoot(environment.root)) roots.add(environment.root.trim());
  }
  return [...roots];
}

function validProjectedRoot(value: string): boolean {
  const root = value.trim();
  return Boolean(root) && root !== "none" && root !== "not projected";
}

function PinnedContextPanel({ continuation, items, messages, onReveal, onUnpin, onUnpinItem, rightInset }: {
  continuation: ChatWorkspace["continuation"];
  items: ChatPinnedContextItem[];
  messages: ChatRenderMessage[];
  onReveal: (item: ChatPinnedContextItem) => void;
  onUnpin: (message: ChatRenderMessage) => Promise<void>;
  onUnpinItem: (item: ChatPinnedContextItem) => Promise<void>;
  rightInset: number;
}) {
  const [expanded, setExpanded] = useState(false);
  const total = items.length + messages.length + (continuation ? 1 : 0);
  const visibleItems = expanded ? items : items.slice(0, Math.max(0, 2 - (continuation ? 1 : 0)));
  const remainingSlots = Math.max(0, 2 - (continuation ? 1 : 0) - visibleItems.length);
  const visibleMessages = expanded ? messages : messages.slice(0, remainingSlots);
  const hiddenCount = total - (visibleItems.length + visibleMessages.length + (continuation ? 1 : 0));
  return (
    <motion.aside
      animate={{ opacity: 1, scale: 1 }}
      className="chat-pinned-panel"
      exit={{ opacity: 0, scale: 0.98 }}
      initial={{ opacity: 0, scale: 0.98 }}
      key="pinned-context"
      style={{ right: rightInset }}
      transition={{ duration: 0.15, ease: [0.16, 1, 0.3, 1] }}
    >
      <div className="chat-pinned-panel-head">
        <span className="chat-pinned-panel-title">Pinned</span>
      </div>
      <div className={`chat-pinned-panel-list${expanded ? " is-expanded" : ""}`}>
        {continuation ? (
          <div className="chat-pinned-row is-static">
            <Glyph className="chat-pinned-row-icon" name="newTask" size={16} />
            <span className="chat-pinned-row-label">Continued from task</span>
          </div>
        ) : null}
        {visibleItems.map((item) => (
          <div className="chat-pinned-row" key={item.artifactId}>
            <button
              aria-label="Unpin selection"
              className="chat-pinned-row-pin"
              onClick={() => void onUnpinItem(item)}
              title="Unpin selection"
              type="button"
            >
              <Glyph className="icon-xs" name="pin" size={16} />
            </button>
            <button className="chat-pinned-row-open" onClick={() => onReveal(item)} type="button">
              <span className="chat-pinned-row-label">{item.text}</span>
            </button>
          </div>
        ))}
        {visibleMessages.map((message) => (
          <div className="chat-pinned-row" key={message.artifactId || message.id}>
            <button
              aria-label="Unpin message"
              className="chat-pinned-row-pin"
              onClick={() => void onUnpin(message)}
              title="Unpin message"
              type="button"
            >
              <Glyph className="icon-xs" name="pin" size={16} />
            </button>
            <button
              className="chat-pinned-row-open"
              onClick={() => onReveal({
                artifactId: message.artifactId || message.id,
                sourceArtifactId: message.artifactId || "",
                sourceMessageId: message.id,
                text: message.text,
                start: 0,
                end: message.text.length,
                role: message.role,
              })}
              type="button"
            >
              <span className="chat-pinned-row-label">{message.text ? markdownToPlainText(message.text) : message.artifactId}</span>
            </button>
          </div>
        ))}
        {!expanded && hiddenCount > 0 ? (
          <button className="chat-pinned-row chat-pinned-row-more" onClick={() => setExpanded(true)} type="button">
            <span className="chat-pinned-row-label">View all</span>
          </button>
        ) : null}
      </div>
    </motion.aside>
  );
}

function ThreadFindBar({
  activeIndex,
  count,
  inputRef,
  onChange,
  onClose,
  onMove,
  rightInset,
  value,
}: {
  activeIndex: number;
  count: number;
  inputRef: RefObject<HTMLInputElement | null>;
  onChange: (value: string) => void;
  onClose: () => void;
  onMove: (direction: -1 | 1) => void;
  rightInset: number;
  value: string;
}) {
  function onKeyDown(event: ReactKeyboardEvent<HTMLInputElement>) {
    if (event.key === "Escape") {
      event.preventDefault();
      onClose();
      return;
    }
    if (event.key !== "Enter") return;
    event.preventDefault();
    onMove(event.shiftKey ? -1 : 1);
  }

  return (
    <div className="chat-thread-find pointer-events-none absolute top-2 z-[55] flex justify-end" style={{ right: rightInset }}>
      <div className="chat-thread-find-surface pointer-events-auto flex h-9 items-center rounded-xl bg-token-dropdown-background p-1 text-sm text-token-foreground shadow-lg ring-[0.5px] ring-token-border">
        <label className="flex min-w-0 items-center gap-1.5 px-1.5">
          <Glyph className="icon-xs shrink-0 text-token-description-foreground" name="search" size={16} />
          <input
            aria-label="Find in task"
            id="content-search-input"
            onChange={(event) => onChange(event.target.value)}
            onKeyDown={onKeyDown}
            placeholder="Search task…"
            ref={inputRef}
            spellCheck={false}
            value={value}
          />
        </label>
        <span aria-live="polite" className="chat-thread-find-count whitespace-nowrap px-1.5 text-xs tabular-nums text-token-description-foreground">
          {value.trim() && count > 0 ? `${activeIndex + 1} / ${count}` : "0 results"}
        </span>
        <button aria-label="Previous result" className="chat-thread-find-button" disabled={count <= 0} onClick={() => onMove(-1)} type="button">
          <Glyph className="icon-xs rotate-180" name="chevronDown" size={16} />
        </button>
        <button aria-label="Next result" className="chat-thread-find-button" disabled={count <= 0} onClick={() => onMove(1)} type="button">
          <Glyph className="icon-xs" name="chevronDown" size={16} />
        </button>
        <button aria-label="Close find" className="chat-thread-find-button" onClick={onClose} type="button">
          <Glyph className="icon-xs" name="close" size={16} />
        </button>
      </div>
    </div>
  );
}

function taskMarkdown(run: ChatRun, messages: ChatRenderMessage[]): string {
  const lines = [
    `# ${run.title}`,
    "",
    `- Run: \`${run.runId}\``,
    `- Provider interface: \`${run.providerInterfaceId || "not declared"}\``,
    `- Created: ${new Date(run.createdAtMs).toISOString()}`,
    `- Updated: ${new Date(run.updatedAtMs).toISOString()}`,
    "",
  ];
  for (const message of messages) {
    const speaker = message.role === "user" ? "Operator" : message.role === "assistant" ? "Agent" : "System";
    const event = message.eventKind && message.eventKind !== "message" ? ` · ${message.eventKind}` : "";
    lines.push(`## ${speaker}${event}`, "", `_${new Date(message.createdAtMs).toISOString()}_`, "");
    if (message.text) lines.push(message.text, "");
    for (const attachment of message.attachments ?? []) {
      lines.push(`- Attachment: ${attachment.name} (${attachment.label})`);
    }
    if (message.attachments?.length) lines.push("");
    if (message.artifactId) lines.push(`> Artifact: \`${message.artifactId}\``, "");
  }
  return `${lines.join("\n").trim()}\n`;
}

function taskSummaryDisplayMode(mainContentWidth: number): "overlay" | "shift" | "gutter" {
  const availableSideSpace = (mainContentWidth - 736) / 2;
  if (availableSideSpace < 180) return "overlay";
  if (availableSideSpace < 400) return "shift";
  return "gutter";
}
