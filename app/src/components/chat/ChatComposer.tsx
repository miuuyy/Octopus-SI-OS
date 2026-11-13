import { useEffect, useRef, useState, type FormEvent, type RefObject } from "react";
import type { OctopusClientAccessProfile, OctopusClientAttachmentStageHandle, OctopusProviderInterface } from "../../electron";
import { Glyph } from "../IconGlyphs";
import { cubicCirclePath, threeQuarterRingPath } from "../icon-glyphs/geometric-primitives";
import { ProviderModelPicker } from "./ProviderModelPicker";
import { ContextUsageIndicator } from "./ContextUsageIndicator";
import type { ContextUsage } from "./contextUsage";
import type { ChatAttachment } from "./types";
import { useLocalDictation } from "./useLocalDictation";
import { DictationIcon } from "./DictationIcon";
import { appToast } from "../../toasts";
import { OCTOPUS_COMPACT_SURFACE_CLASS, OcMenuItem, OcButton, OcDialog, OcDialogFooter, OcDialogHeader } from "../../design-system/components";
import { clientCommands, dispatchClientCommand, subscribeClientCommands, type ClientCommandId } from "../../clientActions";
import { useClientPreferences } from "../../clientPreferences";
import { ProjectedFilePicker, type ProjectedFileSource } from "./ProjectedFilePicker";
import { ComposerUtilityBar } from "./ComposerUtilityBar";
import type { ChatExecutionContext, ChatWorkerEnvironmentRoom } from "../../chatRuns";
import type { ClientAgentDefinition } from "../../agentPresets";
import { DEFAULT_TASK_ACCESS_MODE, type TaskAccessMode } from "./taskAccess";
import { ShellTooltip } from "../shell/ShellTooltip";
import { OcDropdown } from "../../design-system/OcDropdown";
import "./queue.css";
import { RichComposerEditor, type RichComposerEditorHandle } from "./RichComposerEditor";
import { ChatAttachmentStrip } from "./ChatAttachmentCard";
import { useComposerSuggestion } from "./useComposerSuggestion";

type Props = {
  accessProfile: OctopusClientAccessProfile;
  text: string;
  attachments: ChatAttachment[];
  providerInterfaces: OctopusProviderInterface[];
  providerInterfacesLoaded?: boolean;
  providerInterfacesRefreshing?: boolean;
  modelSelectionLoading?: boolean;
  activeProvider?: OctopusProviderInterface;
  providerInterfaceId: string;
  providerModel: string;
  reasoningEffort: string;
  serviceTier?: string;
  contextUsage?: ContextUsage | null;
  executionContexts?: ChatExecutionContext[];
  agentPresets?: readonly ClientAgentDefinition[];
  agentPresetId?: string;
  accessMode?: TaskAccessMode;
  fullAccessUnavailableReason?: string;
  applicationIds?: string[];
  applicationSelectionLocked?: boolean;
  applicationSelectionLockReason?: "" | "turn" | "saving" | "read-only";
  driverNames?: string[];
  eeId?: string;
  runId?: string;
  runtimeHome: string;
  suggestionSourceSequence?: number;
  workerEnvironmentRooms?: ChatWorkerEnvironmentRoom[];
  workerInterfaceId?: string;
  executionContextLocked?: boolean;
  submissionMode: "send" | "steer" | "queue";
  canSubmit: boolean;
  canQueue?: boolean;
  onQueue?: () => void;
  canSteer?: boolean;
  onSteer?: () => void;
  editingQueuedTurn?: boolean;
  hasQueuedMessages?: boolean;
  onCancelQueuedEdit?: () => void;
  submitBlockReason?: string;
  blockingReason?: string;
  interruptPending?: boolean;
  fileInputRef: RefObject<HTMLInputElement | null>;
  onTextChange: (value: string) => void;
  onFilesPicked: (files: FileList | readonly (File | OctopusClientAttachmentStageHandle)[] | null) => void;
  onRemoveAttachment: (id: string) => void;
  onProviderInterfaceChange: (providerInterfaceId: string) => void;
  onProviderModelChange: (providerInterfaceId: string, providerModel: string, reasoningEffort?: string) => void;
  onProviderModelsRefresh?: () => void;
  onReasoningEffortChange: (reasoningEffort: string) => void;
  onServiceTierChange?: (serviceTier: string) => void;
  onEeChange?: (eeId: string) => void;
  onAgentPresetChange?: (agentPresetId: string) => void;
  onAccessModeChange?: (accessMode: TaskAccessMode) => void;
  onApplicationToggle?: (applicationId: string) => void;
  onDriverToggle?: (driverName: string) => void;
  onSubmit: (event?: FormEvent, textOverride?: string) => void;
  onKeyDown: (event: globalThis.KeyboardEvent, editor: HTMLElement) => void;
  onInterrupt?: () => void;
  onResume?: () => void;
  projectedFileSources?: readonly ProjectedFileSource[];
  listenToGlobalCommands?: boolean;
  onVoiceStart?: () => void;
  placeholder?: string;
  isSubmitting?: boolean;
  turnOpen?: boolean;
  resumeAvailable?: boolean;
  variant?: "task" | "home";
  voiceStartDisabled?: boolean;
  pausedQueueSubmissionCount?: number;
  onResolvePausedQueueSubmission?: (resolution: "cancel" | "clear" | "send") => void;
};

const EMPTY_PROJECTED_FILE_SOURCES: readonly ProjectedFileSource[] = [];

export function ChatComposer({
  accessProfile,
  text,
  attachments,
  providerInterfaces,
  providerInterfacesLoaded = true,
  providerInterfacesRefreshing = false,
  modelSelectionLoading = false,
  activeProvider,
  providerInterfaceId,
  providerModel,
  reasoningEffort,
  serviceTier,
  contextUsage = null,
  executionContexts,
  agentPresets,
  agentPresetId,
  accessMode,
  fullAccessUnavailableReason,
  applicationIds,
  applicationSelectionLocked,
  applicationSelectionLockReason,
  driverNames,
  eeId,
  runId,
  runtimeHome,
  suggestionSourceSequence,
  workerEnvironmentRooms,
  workerInterfaceId,
  executionContextLocked,
  submissionMode,
  canSubmit,
  canQueue = false,
  onQueue,
  canSteer = false,
  onSteer,
  editingQueuedTurn = false,
  hasQueuedMessages = false,
  onCancelQueuedEdit,
  submitBlockReason = "",
  blockingReason = "",
  interruptPending = false,
  fileInputRef,
  onTextChange,
  onFilesPicked,
  onRemoveAttachment,
  onProviderInterfaceChange,
  onProviderModelChange,
  onProviderModelsRefresh,
  onReasoningEffortChange,
  onServiceTierChange,
  onEeChange,
  onAgentPresetChange,
  onAccessModeChange,
  onApplicationToggle,
  onDriverToggle,
  onSubmit,
  onKeyDown,
  onInterrupt,
  onResume,
  projectedFileSources,
  listenToGlobalCommands = true,
  onVoiceStart,
  placeholder = "Message Octopus",
  isSubmitting = false,
  turnOpen = false,
  resumeAvailable = false,
  variant = "task",
  voiceStartDisabled = false,
  pausedQueueSubmissionCount = 0,
  onResolvePausedQueueSubmission,
}: Props) {
  const availableProjectedFileSources = projectedFileSources ?? EMPTY_PROJECTED_FILE_SOURCES;
  const clientPreferences = useClientPreferences();
  const composerRef = useRef<HTMLDivElement>(null);
  const editorRef = useRef<RichComposerEditorHandle>(null);
  const sendButtonRef = useRef<HTMLButtonElement>(null);
  const [sendMenuOpen, setSendMenuOpen] = useState(false);
  useEffect(() => { setSendMenuOpen(false); }, [runId]);
  const contextMenuRef = useRef<HTMLDivElement>(null);
  const contextTriggerRef = useRef<HTMLButtonElement>(null);
  const [contextMenuOpen, setContextMenuOpen] = useState(false);
  const [projectedFilePickerOpen, setProjectedFilePickerOpen] = useState(false);
  const [projectedFileSourceKey, setProjectedFileSourceKey] = useState("");
  const [slashMenuDismissed, setSlashMenuDismissed] = useState(false);
  const [slashSelectedIndex, setSlashSelectedIndex] = useState(0);
  const slashMatch = /(?:^|\n)\/([a-z-]*)$/i.exec(text);
  const slashNeedle = slashMatch?.[1].toLocaleLowerCase() ?? "";
  const slashEntries = slashMatch && !slashMenuDismissed
    ? COMPOSER_SLASH_COMMANDS
        .map((entry) => ({ ...entry, command: clientCommands.find((command) => command.id === entry.id) }))
        .filter((entry) => entry.command && (!slashNeedle || `${entry.slug} ${entry.command.label} ${entry.command.detail}`.toLocaleLowerCase().includes(slashNeedle)))
    : [];
  const slashMenuOpen = slashEntries.length > 0;
  const dictation = useLocalDictation({
    accessProfile,
    composerId: `${runtimeHome}\0${runId ?? ""}`,
    onTranscriptStart: () => editorRef.current?.startDictation(),
    onTranscriptUpdate: (transcript) => editorRef.current?.updateDictation(transcript),
    onTranscriptCancel: () => editorRef.current?.cancelDictation(),
    onTranscriptInsert: (transcript) => editorRef.current?.finishDictation(transcript),
    onTranscriptSend: (transcript) => {
      const message = editorRef.current?.finishDictation(transcript);
      if (message != null) onSubmit(undefined, message);
    },
  });
  useEffect(() => {
    const message = dictation.error || dictation.storageWarning;
    if (message) appToast.danger(message, undefined, { dedupeKey: "composer-dictation" });
  }, [dictation.error, dictation.storageWarning]);
  const voiceLayoutActive = dictation.isStarting || dictation.isDictating || dictation.isTranscribing;
  const suggestion = useComposerSuggestion(runId ?? "", suggestionSourceSequence, accessProfile === "live"
    && activeProvider?.runtimeHandlerRef === "codex_app_server" && !turnOpen && !isSubmitting
    && !editingQueuedTurn && !voiceLayoutActive && !blockingReason, runtimeHome);
  const hasSubmissionPayload = Boolean(text.trim() || attachments.length);
  const stopAvailable = turnOpen && !hasSubmissionPayload && !editingQueuedTurn && Boolean(onInterrupt);
  const resumeActionAvailable = !stopAvailable
    && !hasSubmissionPayload
    && resumeAvailable
    && Boolean(onResume);
  const resumeActionBlocked = resumeActionAvailable
    && Boolean(submitBlockReason)
    && submitBlockReason !== "Type a message or attach a file.";
  const submitActionLabel = editingQueuedTurn ? "Save queued message" : stopAvailable
    ? "Stop"
    : resumeActionAvailable
      ? "Resume"
      : submissionMode === "steer"
        ? "Steer current reply"
        : submissionMode === "queue"
          ? "Queue follow-up"
          : "Send";
  const submitAccessibleName = editingQueuedTurn ? "Save queued message" : stopAvailable ? "Stop" : resumeActionAvailable ? "Resume" : "Send";
  const submitTooltip = resumeActionBlocked
    ? submitBlockReason
    : !stopAvailable && !resumeActionAvailable && !canSubmit && submitBlockReason
      ? submitBlockReason
      : submitActionLabel;
  const submitPending = !blockingReason && (stopAvailable || resumeActionAvailable
    ? Boolean(interruptPending)
    : Boolean(isSubmitting && variant !== "home"));
  const queueMenuAvailable = Boolean(onQueue && canQueue && turnOpen && !editingQueuedTurn);
  const submitDisabled = stopAvailable || resumeActionAvailable
    ? Boolean(interruptPending || resumeActionBlocked)
    : Boolean(blockingReason || isSubmitting || !canSubmit);
  const projectedFileSource = availableProjectedFileSources.find((source) => projectedSourceKey(source) === projectedFileSourceKey) ?? null;

  async function pickFilesAndFolders() {
    const transport = window.octopusTransport;
    if (!transport?.selectClientAttachments) {
      appToast.danger("Native file selection is unavailable in this client host.");
      return;
    }
    try {
      const selection = await transport.selectClientAttachments({ mode: "files_and_folders" });
      if (selection.canceled || selection.attachments.length === 0) return;
      onFilesPicked(selection.attachments);
    } catch (error) {
      appToast.danger(error instanceof Error ? error.message : String(error));
    }
  }

  useEffect(() => subscribeClientCommands((id) => {
    if (!listenToGlobalCommands) return;
    if (id === "composer.queue" || id === "composer.steer") {
      const focusedComposer = document.activeElement?.closest("[data-thread-find-composer]");
      if (focusedComposer && focusedComposer !== composerRef.current) return;
      if (!turnOpen || editingQueuedTurn) return;
      if (id === "composer.queue") {
        if (canQueue) onQueue?.();
      } else if (canSteer && !isSubmitting) onSteer?.();
      return;
    }
    if (id === "focus_composer") {
      editorRef.current?.focus();
      return;
    }
    if (id === "attach_files") {
      void pickFilesAndFolders();
      return;
    }
    if (id === "attach_projected_file") {
      if (availableProjectedFileSources.length === 0) {
        appToast.warning("The active task has no declared readable Worker Environment filesystem.");
        return;
      }
      if (availableProjectedFileSources.length > 1) {
        setProjectedFilePickerOpen(false);
        setContextMenuOpen(true);
        return;
      }
      setProjectedFileSourceKey(projectedSourceKey(availableProjectedFileSources[0]));
      setContextMenuOpen(false);
      setProjectedFilePickerOpen(true);
      return;
    }
    if (id !== "start_dictation" || dictation.isTranscribing) return;
    if (dictation.isDictating) {
      dictation.stopDictation();
      return;
    }
    void dictation.startDictation();
  }), [availableProjectedFileSources, dictation.isDictating, dictation.isTranscribing, dictation.startDictation, dictation.stopDictation, listenToGlobalCommands, turnOpen, editingQueuedTurn, canQueue, canSteer, isSubmitting, onQueue, onSteer]);

  useEffect(() => {
    setSlashMenuDismissed(false);
    setSlashSelectedIndex(0);
  }, [text]);

  useEffect(() => {
    setSlashSelectedIndex((current) => Math.min(current, Math.max(0, slashEntries.length - 1)));
  }, [slashEntries.length]);

  useEffect(() => {
    if (!contextMenuOpen) return;
    const close = (event: globalThis.PointerEvent) => {
      const target = event.target as Node;
      if (!contextMenuRef.current?.contains(target) && !contextTriggerRef.current?.contains(target)) setContextMenuOpen(false);
    };
    const escape = (event: globalThis.KeyboardEvent) => {
      if (event.key !== "Escape" || event.defaultPrevented || !(event.target instanceof Element)) return;
      const owner = contextTriggerRef.current?.closest("form");
      if (!owner?.contains(event.target) && !contextMenuRef.current?.contains(event.target)) return;
      if (event.target.closest('[role="dialog"]')) return;
      event.preventDefault();
      setContextMenuOpen(false);
    };
    window.addEventListener("pointerdown", close);
    window.addEventListener("keydown", escape, true);
    return () => {
      window.removeEventListener("pointerdown", close);
      window.removeEventListener("keydown", escape, true);
    };
  }, [contextMenuOpen]);

  useEffect(() => {
    if (!projectedFilePickerOpen) return;
    if (projectedFileSource) return;
    setProjectedFilePickerOpen(false);
    setProjectedFileSourceKey("");
  }, [projectedFilePickerOpen, projectedFileSource]);

  return (
    <>
    <div
      aria-busy={!blockingReason && (isSubmitting || interruptPending) || undefined}
      className="flex flex-col"
      data-composer-variant={variant}
      data-thread-find-composer="true"
      onKeyDownCapture={(event) => {
        if (event.key !== "Escape" || !voiceLayoutActive || event.nativeEvent.isComposing || event.target instanceof Element && event.target.closest('[role="dialog"]')) return;
        event.preventDefault();
        event.stopPropagation();
        dictation.abortDictation();
      }}
      ref={composerRef}
    >
      <input
        aria-label="Attach files"
        className="composer-file-input"
        multiple
        onChange={(event) => onFilesPicked(event.currentTarget.files)}
        ref={fileInputRef}
        type="file"
      />
      <div className="flex flex-col gap-2">
        {blockingReason ? <div className="composer-storage-warning" role="alert">{blockingReason}</div> : null}
        <div className="min-w-0" data-octopus-composer-root="">
          <div className="relative">
          {projectedFilePickerOpen && projectedFileSource ? (
            <div className="absolute right-0 bottom-full left-0 z-[60] mb-1" data-composer-projected-file-picker="true">
              <ProjectedFilePicker
                onAttach={(file, path) => {
                  onFilesPicked([file]);
                  onTextChange(appendProjectedFileMention(text, path));
                }}
                onClose={() => setProjectedFilePickerOpen(false)}
                source={projectedFileSource}
              />
            </div>
          ) : null}
          {slashMenuOpen && !projectedFilePickerOpen ? (
            <div className="absolute right-0 bottom-full left-0 z-50 mb-1" data-composer-slash-menu="true">
              <div className={`${OCTOPUS_COMPACT_SURFACE_CLASS} max-h-[320px]`} role="listbox">
                <div className="vertical-scroll-fade-mask flex w-full flex-1 flex-col overflow-y-auto p-1">
                  {slashEntries.map((entry, index) => (
                    <button
                      aria-selected={index === slashSelectedIndex}
                      className={`flex w-full items-center gap-2 rounded-lg px-2 py-1.5 text-left text-sm outline-none${index === slashSelectedIndex ? " bg-token-list-hover-background" : " hover:bg-token-list-hover-background"}`}
                      key={entry.id}
                      onClick={() => runSlashCommand(entry.id)}
                      onMouseDown={(event) => event.preventDefault()}
                      role="option"
                      type="button"
                    >
                      <span className="flex size-6 shrink-0 items-center justify-center rounded-md bg-token-foreground/[0.04] text-token-text-secondary">
                        <Glyph name={slashCommandGlyph(entry.id)} size={14} />
                      </span>
                      <span className="min-w-0 flex-1">
                        <span className="block truncate text-token-foreground">/{entry.slug}</span>
                        <span className="block truncate text-xs text-token-description-foreground">{entry.command?.detail}</span>
                      </span>
                    </button>
                  ))}
                </div>
              </div>
            </div>
          ) : null}
          {contextMenuOpen && !projectedFilePickerOpen ? (
            <div
              className="absolute right-0 bottom-full left-0 z-50 mb-1"
              data-composer-overlay-floating-ui="true"
              data-settings-layer="true"
              ref={contextMenuRef}
            >
              <div
                className={`${OCTOPUS_COMPACT_SURFACE_CLASS} max-h-[320px]`}
                role="menu"
              >
                <div className="vertical-scroll-fade-mask flex w-full flex-1 flex-col overflow-y-auto">
                  <button
                    className="text-token-foreground outline-hidden cursor-interaction flex w-full shrink-0 items-center gap-2 overflow-hidden rounded-lg px-row-x py-row-y text-left text-sm opacity-75 hover:bg-token-list-hover-background hover:opacity-100 focus:bg-token-list-hover-background focus:opacity-100"
                    onClick={() => {
                      setContextMenuOpen(false);
                      void pickFilesAndFolders();
                    }}
                    onMouseDown={(event) => event.preventDefault()}
                    role="menuitem"
                    type="button"
                  >
                    <Glyph className="icon-xs shrink-0" name="paperclip" size={16} />
                    <span className="truncate">Files and folders</span>
                  </button>
                  {availableProjectedFileSources.map((source) => (
                    <button
                      className="text-token-foreground outline-hidden cursor-interaction flex w-full shrink-0 items-center gap-2 overflow-hidden rounded-lg px-row-x py-row-y text-left text-sm opacity-75 hover:bg-token-list-hover-background hover:opacity-100 focus:bg-token-list-hover-background focus:opacity-100"
                      key={projectedSourceKey(source)}
                      onClick={() => {
                        setContextMenuOpen(false);
                        setProjectedFileSourceKey(projectedSourceKey(source));
                        setProjectedFilePickerOpen(true);
                      }}
                      onMouseDown={(event) => event.preventDefault()}
                      role="menuitem"
                      type="button"
                    >
                      <Glyph className="icon-xs shrink-0" name="folder" size={16} />
                      <span className="min-w-0 flex-1">
                        <span className="block truncate">Projected file</span>
                        <span className="block truncate text-xs text-token-description-foreground">{source.label}</span>
                      </span>
                    </button>
                  ))}
                </div>
              </div>
            </div>
          ) : null}
          {variant === "task" && executionContexts && !hasQueuedMessages ? (
            <ComposerUtilityBar
              contexts={executionContexts}
              agentPresets={agentPresets ?? []}
              agentPresetId={agentPresetId ?? ""}
              accessMode={accessMode ?? DEFAULT_TASK_ACCESS_MODE}
              fullAccessUnavailableReason={fullAccessUnavailableReason ?? "Task capabilities are not ready."}
              applicationIds={applicationIds ?? []}
              applicationLocked={applicationSelectionLocked === true}
              applicationLockReason={applicationSelectionLockReason ?? ""}
              driverNames={driverNames ?? []}
              eeId={eeId ?? ""}
              locked={executionContextLocked === true}
              rooms={workerEnvironmentRooms ?? []}
              runId={runId ?? ""}
              workerInterfaceId={workerInterfaceId ?? ""}
              onAgentChange={onAgentPresetChange ?? (() => undefined)}
              onAccessModeChange={onAccessModeChange ?? (() => undefined)}
              onEeChange={onEeChange ?? (() => undefined)}
              onApplicationToggle={onApplicationToggle ?? (() => undefined)}
              onDriverToggle={onDriverToggle ?? (() => undefined)}
            />
          ) : null}
          <div className="composer-surface-chrome relative z-10 flex flex-col bg-token-input-background text-token-foreground composer-multiline-surface">
            <div className="relative z-10 flex min-h-0 flex-1 flex-col">
              <div className="composer-attachments-default">
                {attachments.length ? (
                <ChatAttachmentStrip attachments={attachments} onRemove={onRemoveAttachment} />
                ) : null}
              </div>

              <div className="contents">
                <div className="mb-1 flex-grow overflow-y-auto px-3">
                  <RichComposerEditor
                    className="composer-editor-root text-size-chat [&_.ProseMirror]:focus-visible:outline-none text-token-foreground h-auto max-h-[25dvh] overflow-y-auto [&_.ProseMirror]:h-auto [&_.ProseMirror]:min-h-[2rem] [&_.ProseMirror]:resize-none [&_.ProseMirror_p]:m-0 [&_.ProseMirror_ul]:pl-6 [&_.ProseMirror_ol]:list-decimal [&_.ProseMirror_ol]:pl-6 text-base [&_.ProseMirror]:leading-5"
                    onFilesPicked={onFilesPicked}
                    onKeyDown={handleEditorKeyDown}
                    onTextChange={onTextChange}
                    placeholder={placeholder}
                    suggestion={attachments.length || slashMenuOpen ? null : suggestion}
                    ref={editorRef}
                    spellCheck={clientPreferences.spellcheckEnabled}
                    text={text}
                  />
                </div>

                {dictation.error && dictation.canRetryDictation ? (
                  <div className="composer-voice-recovery" role="status" aria-live="polite">
                    <span>Dictation stopped</span>
                    {dictation.canRetryDictation && !dictation.isTranscribing ? <OcButton onClick={dictation.retryDictation} size="sm">Retry</OcButton> : null}
                    {dictation.lastRecordingId ? <OcButton onClick={() => dispatchClientCommand("open_settings", "surface", { sectionId: "voice-audio", dictationRecordingId: dictation.lastRecordingId! })} size="sm">View recording</OcButton> : null}
                    <button aria-label="Dismiss" className="composer-round-button" onClick={dictation.abortDictation} type="button"><Glyph name="close" size={16} /></button>
                  </div>
                ) : null}
                <div className="composer-footer grid grid-cols-[auto_minmax(0,1fr)_auto] items-center gap-2 px-2 py-1 select-none">
                  <div className="flex min-w-0 items-center gap-2">
                    <div className="composer-context-anchor">
                      <ShellTooltip label="Add files and more" disabled={contextMenuOpen}>
                        <button
                          aria-expanded={contextMenuOpen}
                          aria-label="Add files and more"
                          className="border-token-border no-drag cursor-interaction flex items-center gap-1 border whitespace-nowrap select-none focus:outline-none disabled:cursor-not-allowed disabled:opacity-40 rounded-full text-token-text-tertiary enabled:hover:bg-token-list-hover-background data-[state=open]:bg-token-list-hover-background border-transparent h-token-button-composer px-2 py-0 text-sm leading-[18px] aspect-square items-center justify-center !px-0"
                          data-composer-navigation-target="add-context"
                          data-state={contextMenuOpen ? "open" : "closed"}
                          onMouseDown={(event) => event.preventDefault()}
                          onClick={() => setContextMenuOpen((open) => !open)}
                          ref={contextTriggerRef}
                          type="button"
                        >
                          <Glyph className="icon-xs text-token-text-primary" name="plus" size={16} />
                        </button>
                      </ShellTooltip>
                    </div>
                  </div>

                  <div className="flex items-center" />

                  <div className="flex min-w-0 w-full shrink-0 items-center justify-end gap-2">
                    <div className="flex min-w-0 flex-1 justify-end">
                      <div className="flex shrink-0 items-center gap-2">
                        <ContextUsageIndicator usage={contextUsage} />
                        {variant === "task" && (runId || agentPresetId && workerInterfaceId) ? (
                          <ProviderModelPicker
                            activeProvider={activeProvider}
                            interfaceLocked={executionContextLocked === true}
                            listenToGlobalCommands={listenToGlobalCommands}
                            loading={modelSelectionLoading || !providerInterfacesLoaded || providerInterfacesRefreshing}
                            onModelChange={onProviderModelChange}
                            onRefreshModels={onProviderModelsRefresh}
                            onReasoningEffortChange={onReasoningEffortChange}
                            providerInterfaceId={providerInterfaceId}
                            providerModel={providerModel}
                            providers={providerInterfaces}
                            reasoningEffort={reasoningEffort}
                            serviceTier={serviceTier}
                            onServiceTierChange={onServiceTierChange}
                          />
                        ) : null}
                        <DictationButton
                          canRetry={dictation.canRetryDictation}
                          isStarting={dictation.isStarting}
                          isDictating={dictation.isDictating}
                          isTranscribing={dictation.isTranscribing}
                          waveformCanvasRef={dictation.waveformCanvasRef}
                          onCancel={dictation.abortDictation}
                          onRetry={dictation.retryDictation}
                          onStart={dictation.startDictation}
                          onStop={() => dictation.stopDictation()}
                        />
                        {editingQueuedTurn ? <button className="composer-queue-action" type="button" onClick={onCancelQueuedEdit}>Cancel edit</button> : null}
                        {!voiceLayoutActive && turnOpen && hasSubmissionPayload && onInterrupt ? (
                          <ShellTooltip label="Stop">
                            <button aria-label="Stop" className="composer-queue-action is-icon" disabled={interruptPending} onClick={onInterrupt} type="button">
                              <Glyph name="stop" size={16} />
                            </button>
                          </ShellTooltip>
                        ) : null}
                        {voiceLayoutActive ? (
                          <ShellTooltip label="Send">
                            <button aria-label="Send" className="composer-dictation-send composer-primary-action cursor-interaction size-token-button-composer flex items-center justify-center rounded-full p-0.5 transition-opacity focus-visible:outline-2 disabled:cursor-default disabled:opacity-50"
                              disabled={!dictation.isDictating || isSubmitting || Boolean(submitBlockReason && submitBlockReason !== "Type a message or attach a file.")}
                              onClick={() => dictation.stopDictation("send")} type="button"><Glyph className="icon-xs" name="send" size={16} /></button>
                          </ShellTooltip>
                        ) : variant === "home" && !isSubmitting && !stopAvailable && !resumeActionAvailable && !hasSubmissionPayload ? (
                          <ShellTooltip label="Open Voice">
                            <button
                              aria-label="Open Voice"
                              className="focus-visible:outline-token-button-background cursor-interaction size-token-button-composer flex items-center justify-center rounded-full bg-token-foreground p-0.5 transition-opacity focus-visible:outline-2 disabled:cursor-default disabled:opacity-50"
                              disabled={Boolean(blockingReason) || voiceStartDisabled || !onVoiceStart}
                              onClick={onVoiceStart}
                              type="button"
                            >
                              <Glyph className="icon-xs text-token-dropdown-background" name="voiceMode" size={16} />
                            </button>
                          </ShellTooltip>
                        ) : (
                          <ShellTooltip label={submitTooltip} disabled={sendMenuOpen}>
                            <button
                              aria-busy={submitPending || undefined}
                              aria-label={submitAccessibleName}
                              aria-disabled={submitDisabled || undefined}
                              className="composer-primary-action cursor-interaction size-token-button-composer flex items-center justify-center rounded-full p-0.5 transition-opacity focus-visible:outline-2 disabled:cursor-default disabled:opacity-50 aria-disabled:cursor-default aria-disabled:opacity-50"
                              disabled={submitDisabled && !queueMenuAvailable}
                              ref={sendButtonRef}
                              onContextMenu={(event) => {
                                if (!onQueue || !turnOpen || editingQueuedTurn) return;
                                event.preventDefault(); setSendMenuOpen(true);
                              }}
                              onKeyDown={(event) => {
                                if ((event.key === "ContextMenu" || (event.shiftKey && event.key === "F10")) && onQueue && turnOpen && !editingQueuedTurn) {
                                  event.preventDefault(); setSendMenuOpen(true);
                                }
                              }}
                              onClick={() => {
                                if (submitDisabled) return;
                                stopAvailable ? onInterrupt?.() : resumeActionAvailable ? onResume?.() : onSubmit();
                              }}
                              type="button"
                            >
                              {submitPending
                                ? <ComposerSubmitSpinner />
                                : <Glyph
                                    className={resumeActionAvailable ? "icon-primary-action" : "icon-xs"}
                                    name={stopAvailable ? "stop" : resumeActionAvailable ? "resume" : isSubmitting ? "send" : submissionMode === "steer" ? "sendNow" : submissionMode === "queue" ? "steer" : "send"}
                                    size={resumeActionAvailable ? 20 : 16}
                                  />}
                            </button>
                          </ShellTooltip>
                        )}
                      </div>
                    </div>
                  </div>
                </div>
              </div>
            </div>
          </div>
        </div>
        </div>
      </div>
    </div>
    {onQueue && turnOpen && !editingQueuedTurn ? (
      <OcDropdown anchorRef={sendButtonRef} align="end" open={sendMenuOpen} onOpenChange={setSendMenuOpen}>
        <OcMenuItem role="menuitem" label="Send now" description="Steer the current reply" leadingIcon="sendNow" disabled={!canSteer || !onSteer || isSubmitting}
          onClick={() => { setSendMenuOpen(false); onSteer?.(); }} />
        <OcMenuItem role="menuitem" label="Queue message" description="Send after the current reply finishes" leadingIcon="steer" disabled={!canQueue}
          onClick={() => { setSendMenuOpen(false); onQueue(); }} />
      </OcDropdown>
    ) : null}
    <PausedQueueSubmissionDialog
      count={pausedQueueSubmissionCount}
      onResolve={onResolvePausedQueueSubmission}
    />
    </>
  );

  function handleEditorKeyDown(event: globalThis.KeyboardEvent, editor: HTMLElement) {
    if (event.defaultPrevented || event.isComposing) return;
    if (slashMenuOpen) {
      if (event.key === "ArrowDown" || event.key === "ArrowUp") {
        event.preventDefault();
        event.stopPropagation();
        const direction = event.key === "ArrowDown" ? 1 : -1;
        setSlashSelectedIndex((current) => (current + direction + slashEntries.length) % slashEntries.length);
        return;
      }
      if (event.key === "Enter" || event.key === "Tab") {
        event.preventDefault();
        event.stopPropagation();
        const entry = slashEntries[slashSelectedIndex];
        if (entry) runSlashCommand(entry.id);
        return;
      }
      if (event.key === "Escape") {
        event.preventDefault();
        event.stopPropagation();
        setSlashMenuDismissed(true);
        return;
      }
    }
    if (event.key === "Escape" && voiceLayoutActive) {
      event.preventDefault();
      event.stopPropagation();
      dictation.abortDictation();
      return;
    }
    const dictationSubmit = event.key === "Enter" && !event.shiftKey && !event.altKey
      && (clientPreferences.sendShortcut === "enter" || event.metaKey || event.ctrlKey);
    if (voiceLayoutActive && dictationSubmit) {
      event.preventDefault();
      event.stopPropagation();
      if (dictation.isDictating && !event.repeat) dictation.stopDictation("send");
      return;
    }
    const queueShortcut = event.key === "Enter" && (event.metaKey || event.ctrlKey) && !event.altKey
      && (clientPreferences.sendShortcut === "enter" ? !event.shiftKey : event.shiftKey);
    if (queueShortcut && turnOpen && onQueue && !editingQueuedTurn) {
      event.preventDefault();
      event.stopPropagation();
      if (canQueue && !event.repeat) onQueue();
      return;
    }
    onKeyDown(event, editor);
  }

  function runSlashCommand(id: ClientCommandId) {
    onTextChange(removeComposerSlashQuery(text));
    setSlashMenuDismissed(true);
    window.requestAnimationFrame(() => dispatchClientCommand(id, "surface"));
  }
}

function PausedQueueSubmissionDialog({ count, onResolve }: {
  count: number;
  onResolve?: (resolution: "cancel" | "clear" | "send") => void;
}) {
  const cancelRef = useRef<HTMLButtonElement>(null);
  return (
    <OcDialog
      ariaLabelledBy="paused-queue-submit-title"
      backdropClassName="octopus-dialog-overlay bg-[#00000022]"
      initialFocusRef={cancelRef}
      onClose={() => onResolve?.("cancel")}
      open={count > 0}
      panelClassName="octopus-dialog w-[460px] max-w-[calc(100vw-2rem)] rounded-3xl bg-token-dropdown-background/95 p-5 text-token-foreground ring-[0.5px] ring-token-border backdrop-blur-xl"
    >
      <OcDialogHeader
        className="p-0"
        description={`You are about to send a message. Do you want to clear the ${count === 1 ? "1 message" : `${count} messages`} previously queued?`}
        title="Send message?"
        titleId="paused-queue-submit-title"
      />
      <OcDialogFooter className="mt-5 min-h-0 border-0 p-0">
        <OcButton buttonRef={cancelRef} onClick={() => onResolve?.("cancel")} variant="ghost">Cancel</OcButton>
        <OcButton onClick={() => onResolve?.("clear")} tone="danger" variant="secondary">Clear queue</OcButton>
        <OcButton onClick={() => onResolve?.("send")} variant="primary">Send message</OcButton>
      </OcDialogFooter>
    </OcDialog>
  );
}

function appendProjectedFileMention(current: string, path: string): string {
  const mention = `@${path}`;
  if (current.includes(mention)) return current;
  if (!current.trim()) return mention;
  return `${current.replace(/\s+$/, "")} ${mention}`;
}

const COMPOSER_SLASH_COMMANDS: ReadonlyArray<{ id: ClientCommandId; slug: string }> = [
  { id: "open_model_picker", slug: "model" },
  { id: "attach_files", slug: "attach" },
  { id: "attach_projected_file", slug: "file" },
  { id: "start_dictation", slug: "dictate" },
  { id: "open_terminal", slug: "terminal" },
  { id: "open_browser", slug: "browser" },
  { id: "open_changes", slug: "changes" },
  { id: "review_changes", slug: "review" },
  { id: "toggle_task_summary", slug: "summary" },
  { id: "open_voice", slug: "voice" },
  { id: "open_settings", slug: "settings" },
];

function slashCommandGlyph(id: ClientCommandId) {
  if (id === "open_model_picker") return "sliders" as const;
  if (id === "attach_files") return "paperclip" as const;
  if (id === "attach_projected_file") return "folder" as const;
  if (id === "start_dictation") return "microphone" as const;
  if (id === "open_terminal") return "terminal" as const;
  if (id === "open_browser") return "browser" as const;
  if (id === "open_changes") return "changes" as const;
  if (id === "review_changes") return "changes" as const;
  if (id === "toggle_task_summary") return "viewAll" as const;
  if (id === "open_voice") return "voice" as const;
  return "settings" as const;
}

function ComposerSubmitSpinner() {
  const [animationDelay] = useState(() => `-${Date.now() % 1_000}ms`);
  return (
    <span className="composer-submit-spinner" style={{ animationDelay }}>
      <svg aria-hidden="true" fill="none" viewBox="0 0 24 24" xmlns="http://www.w3.org/2000/svg">
        <path d={cubicCirclePath(12, 12, 8, 8 * 4 * (Math.sqrt(2) - 1) / 3) + cubicCirclePath(12, 12, 6, 8 * (Math.sqrt(2) - 1))} fill="currentColor" fillRule="evenodd" opacity=".3" />
        <path d={threeQuarterRingPath(12, 12, 8, 6)} fill="currentColor" />
      </svg>
    </span>
  );
}

function DictationButton({ canRetry, isStarting, isDictating, isTranscribing, waveformCanvasRef, onCancel, onRetry, onStart, onStop }: {
  canRetry: boolean;
  isStarting: boolean;
  isDictating: boolean;
  isTranscribing: boolean;
  waveformCanvasRef: RefObject<HTMLCanvasElement | null>;
  onCancel: () => void;
  onRetry: () => void;
  onStart: () => Promise<void>;
  onStop: () => void;
}) {
  const label = isTranscribing ? "Finishing dictation" : isStarting ? "Starting dictation; click to cancel" : isDictating ? "Stop dictation" : canRetry ? "Retry dictation" : "Dictate";
  return (
    <ShellTooltip label={label}>
      <button aria-label={label} aria-busy={isStarting || isTranscribing || undefined}
        className={`composer-dictation-button composer-round-button${isDictating || isTranscribing ? " is-active" : ""}`}
        disabled={isTranscribing}
        onClick={() => isStarting ? onCancel() : isDictating ? onStop() : canRetry ? onRetry() : void onStart()}
        type="button">
        {isDictating ? <span className="composer-dictation-waveform"><canvas aria-hidden="true" data-mode="text" ref={waveformCanvasRef} /></span> : null}
        <span className="composer-dictation-icon">{isStarting || isTranscribing ? <span className="composer-spinner" /> : <DictationIcon name={isDictating ? "stop" : canRetry ? "retry" : "microphone"} />}</span>
      </button>
    </ShellTooltip>
  );
}

function projectedSourceKey(source: ProjectedFileSource): string {
  return `${source.workerEnvironmentId}\0${source.surfaceId}`;
}

function removeComposerSlashQuery(value: string): string {
  return value.replace(/(?:^|\n)\/[a-z-]*$/i, "").replace(/\n$/, "");
}
