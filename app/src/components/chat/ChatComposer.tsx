import { useCallback, useEffect, useId, useMemo, useRef, useState, type FormEvent, type RefObject } from "react";
import { AnimatePresence, motion } from "motion/react";
import type { OctopusClientAccessProfile, OctopusClientAttachmentStageHandle, OctopusProviderInterface } from "../../electron";
import { useShouldReduceMotion } from "../../appearance";
import { Glyph } from "../IconGlyphs";
import { OctopusSpinner } from "../navigation/OctopusSpinner";
import { ComposerPrimaryActionIcon, type ComposerPrimaryActionState } from "./ComposerPrimaryActionIcon";
import { ProviderModelPicker } from "./ProviderModelPicker";
import { ContextUsageIndicator } from "./ContextUsageIndicator";
import type { ContextUsage } from "./contextUsage";
import type { ChatAttachment } from "./types";
import { useLocalDictation, type DictationPhase } from "./useLocalDictation";
import { DictationIcon } from "./DictationIcon";
import { appToast } from "../../toasts";
import { OcMenuItem, OcButton, OcDialog, OcDialogFooter, OcDialogHeader } from "../../design-system/components";
import { clientCommands, dispatchClientCommand, subscribeClientCommands, type ClientCommandId } from "../../clientActions";
import { useClientPreferences } from "../../clientPreferences";
import { ProjectedFilePicker, type ProjectedFileSource } from "./ProjectedFilePicker";
import {
  ComposerSuggestionEmpty,
  ComposerSuggestionMenu,
  ComposerSuggestionRow,
  ComposerSuggestionSectionLabel,
  scoreSuggestionQuery,
} from "./ComposerSuggestionMenu";
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
  const reducedMotion = useShouldReduceMotion();
  const slashListId = useId();
  const contextListId = useId();
  const atSourceListId = useId();
  const composerRef = useRef<HTMLDivElement>(null);
  const editorShellRef = useRef<HTMLDivElement>(null);
  const editorRef = useRef<RichComposerEditorHandle>(null);
  const sendButtonRef = useRef<HTMLButtonElement>(null);
  const [sendMenuOpen, setSendMenuOpen] = useState(false);
  useEffect(() => { setSendMenuOpen(false); }, [runId]);
  const contextMenuRef = useRef<HTMLDivElement>(null);
  const contextTriggerRef = useRef<HTMLButtonElement>(null);
  const [contextMenuOpen, setContextMenuOpen] = useState(false);
  const [contextSelectedIndex, setContextSelectedIndex] = useState(0);
  const [contextNavSource, setContextNavSource] = useState<"keyboard" | "pointer">("pointer");
  const [projectedFilePickerOpen, setProjectedFilePickerOpen] = useState(false);
  const [projectedFileSourceKey, setProjectedFileSourceKey] = useState("");
  const [projectedFileInitialQuery, setProjectedFileInitialQuery] = useState("");
  const [slashMenuDismissed, setSlashMenuDismissed] = useState(false);
  const [slashSelectedIndex, setSlashSelectedIndex] = useState(0);
  const [slashNavSource, setSlashNavSource] = useState<"keyboard" | "pointer">("pointer");
  const [atMentionDismissed, setAtMentionDismissed] = useState(false);
  const [atSourceSelectedIndex, setAtSourceSelectedIndex] = useState(0);
  const [atSourceNavSource, setAtSourceNavSource] = useState<"keyboard" | "pointer">("pointer");

  const slashMatch = /(?:^|\n)\/([^\n]*)$/.exec(text);
  const slashRawNeedle = slashMatch?.[1] ?? "";
  const slashNeedle = slashRawNeedle.trim();
  const slashEntries = useMemo(() => {
    if (!slashMatch || slashMenuDismissed) return [];
    const base = COMPOSER_SLASH_COMMANDS
      .map((entry, index) => ({
        ...entry,
        command: clientCommands.find((command) => command.id === entry.id),
        index,
      }))
      .filter((entry) => Boolean(entry.command));
    if (!slashNeedle) return base;
    return base
      .map((entry) => ({
        entry,
        score: scoreSuggestionQuery(slashNeedle, [
          entry.title,
          entry.slug,
          ...entry.aliases,
          entry.description,
          entry.command?.label,
          entry.command?.detail,
        ]),
      }))
      .filter((candidate) => candidate.score >= 0)
      .sort((a, b) => b.score - a.score || a.entry.index - b.entry.index)
      .map((candidate) => candidate.entry);
  }, [slashMatch, slashMenuDismissed, slashNeedle]);
  const slashMenuOpen = Boolean(slashMatch && !slashMenuDismissed && !(slashEntries.length === 0 && /\s/.test(slashRawNeedle)));

  const atMatch = /(?:^|[\s\n])@([^\s]*)$/.exec(text);
  const atNeedle = atMatch?.[1] ?? "";
  const atMentionOpen = Boolean(
    atMatch
    && !atMentionDismissed
    && !slashMenuOpen
    && !projectedFilePickerOpen
    && !contextMenuOpen
    && availableProjectedFileSources.length > 0,
  );
  const atSingleSource = atMentionOpen && availableProjectedFileSources.length === 1
    ? availableProjectedFileSources[0]
    : null;
  const atMultiSourceOpen = atMentionOpen && availableProjectedFileSources.length > 1;
  const atFilteredSources = useMemo(() => {
    if (!atMultiSourceOpen) return [];
    if (!atNeedle) return [...availableProjectedFileSources];
    return availableProjectedFileSources
      .map((source, index) => ({ source, index, score: scoreSuggestionQuery(atNeedle, [source.label]) }))
      .filter((item) => item.score >= 0)
      .sort((a, b) => b.score - a.score || a.index - b.index)
      .map((item) => item.source);
  }, [atMultiSourceOpen, atNeedle, availableProjectedFileSources]);

  const contextMenuItems = useMemo(() => [
    {
      key: "files",
      icon: "paperclip" as const,
      title: "Files and folders",
      description: undefined as string | undefined,
      hasSubmenu: false,
      onSelect: () => {
        setContextMenuOpen(false);
        void pickFilesAndFolders();
      },
    },
    ...availableProjectedFileSources.map((source) => ({
      key: projectedSourceKey(source),
      icon: "folder" as const,
      title: "Projected file",
      description: source.label,
      hasSubmenu: true,
      onSelect: () => {
        setContextMenuOpen(false);
        setProjectedFileInitialQuery("");
        setProjectedFileSourceKey(projectedSourceKey(source));
        setProjectedFilePickerOpen(true);
      },
    })),
  ], [availableProjectedFileSources]);
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
  const [dictationRestoring, setDictationRestoring] = useState(false);
  const previousVoiceLayoutRef = useRef(false);
  useEffect(() => {
    const wasActive = previousVoiceLayoutRef.current;
    previousVoiceLayoutRef.current = voiceLayoutActive;
    if (!wasActive || voiceLayoutActive) return;
    setDictationRestoring(true);
    const frame = window.requestAnimationFrame(() => setDictationRestoring(false));
    return () => window.cancelAnimationFrame(frame);
  }, [voiceLayoutActive]);
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
    setAtMentionDismissed(false);
    setAtSourceSelectedIndex(0);
  }, [text]);

  useEffect(() => {
    setSlashSelectedIndex((current) => Math.min(current, Math.max(0, slashEntries.length - 1)));
  }, [slashEntries.length]);

  useEffect(() => {
    setAtSourceSelectedIndex((current) => Math.min(current, Math.max(0, atFilteredSources.length - 1)));
  }, [atFilteredSources.length]);

  useEffect(() => {
    if (!contextMenuOpen) return;
    setContextSelectedIndex(0);
    setContextNavSource("pointer");
  }, [contextMenuOpen]);

  useEffect(() => {
    if (!contextMenuOpen) return;
    const onKeyDownMenu = (event: globalThis.KeyboardEvent) => {
      if (event.defaultPrevented || event.isComposing || !(event.target instanceof Element)) return;
      if (event.target.closest('[role="dialog"]')) return;
      const owner = contextTriggerRef.current?.closest("form");
      const withinOwner = Boolean(owner?.contains(event.target) || contextMenuRef.current?.contains(event.target) || contextTriggerRef.current?.contains(event.target) || composerRef.current?.contains(event.target));
      if (!withinOwner) return;
      if (event.key === "Escape") {
        event.preventDefault();
        event.stopPropagation();
        setContextMenuOpen(false);
        return;
      }
      if (!contextMenuItems.length) return;
      if (event.key === "ArrowDown" || (event.ctrlKey && !event.metaKey && !event.altKey && event.key === "n")) {
        event.preventDefault();
        event.stopPropagation();
        setContextNavSource("keyboard");
        setContextSelectedIndex((current) => (current + 1) % contextMenuItems.length);
        return;
      }
      if (event.key === "ArrowUp" || (event.ctrlKey && !event.metaKey && !event.altKey && event.key === "p")) {
        event.preventDefault();
        event.stopPropagation();
        setContextNavSource("keyboard");
        setContextSelectedIndex((current) => (current - 1 + contextMenuItems.length) % contextMenuItems.length);
        return;
      }
      if (event.key === "Home") {
        event.preventDefault();
        event.stopPropagation();
        setContextNavSource("keyboard");
        setContextSelectedIndex(0);
        return;
      }
      if (event.key === "End") {
        event.preventDefault();
        event.stopPropagation();
        setContextNavSource("keyboard");
        setContextSelectedIndex(contextMenuItems.length - 1);
        return;
      }
      if (event.key === "Enter" || event.key === "Tab") {
        event.preventDefault();
        event.stopPropagation();
        contextMenuItems[contextSelectedIndex]?.onSelect();
      }
    };
    window.addEventListener("keydown", onKeyDownMenu, true);
    return () => {
      window.removeEventListener("keydown", onKeyDownMenu, true);
    };
  }, [contextMenuItems, contextMenuOpen, contextSelectedIndex]);

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
            <ProjectedFilePicker
              externalQuery={projectedFileInitialQuery || undefined}
              onAttach={(file, path) => {
                onFilesPicked([file]);
                onTextChange(atMatch ? replaceComposerAtMention(text, path) : appendProjectedFileMention(text, path));
                setProjectedFileInitialQuery("");
              }}
              onClose={() => {
                setProjectedFilePickerOpen(false);
                setProjectedFileInitialQuery("");
              }}
              source={projectedFileSource}
            />
          ) : null}
          {atSingleSource ? (
            <ProjectedFilePicker
              autoFocusSearch={false}
              externalQuery={atNeedle}
              ignoreOutsideRefs={[editorShellRef]}
              onAttach={(file, path) => {
                onFilesPicked([file]);
                onTextChange(replaceComposerAtMention(text, path));
                setAtMentionDismissed(true);
              }}
              onClose={() => setAtMentionDismissed(true)}
              source={atSingleSource}
            />
          ) : null}
          {atMultiSourceOpen ? (
            <ComposerSuggestionMenu
              ariaLabel="Select projected file source"
              dataAttribute={{ "data-composer-at-menu": "true" }}
              fadeMaskVariant="bottom"
              ignoreOutsideRefs={[editorShellRef]}
              listId={atSourceListId}
              listRole="listbox"
              onDismiss={() => setAtMentionDismissed(true)}
            >
              <ComposerSuggestionSectionLabel>Projected files</ComposerSuggestionSectionLabel>
              {atFilteredSources.length === 0 ? (
                <ComposerSuggestionEmpty>No matching sources</ComposerSuggestionEmpty>
              ) : (
                atFilteredSources.map((source, index) => (
                  <ComposerSuggestionRow
                    description="Projected file"
                    hasSubmenu
                    highlighted={index === atSourceSelectedIndex}
                    icon={<Glyph name="folder" size={16} />}
                    id={`${atSourceListId}-opt-${index}`}
                    key={projectedSourceKey(source)}
                    onHighlight={() => {
                      setAtSourceNavSource("pointer");
                      setAtSourceSelectedIndex(index);
                    }}
                    onSelect={() => {
                      setAtMentionDismissed(true);
                      setProjectedFileSourceKey(projectedSourceKey(source));
                      setProjectedFileInitialQuery(atNeedle);
                      setProjectedFilePickerOpen(true);
                    }}
                    query={atNeedle}
                    scrollIntoViewOnHighlight={atSourceNavSource === "keyboard"}
                    title={source.label}
                  />
                ))
              )}
            </ComposerSuggestionMenu>
          ) : null}
          {slashMenuOpen && !projectedFilePickerOpen ? (
            <ComposerSuggestionMenu
              ariaLabel="Slash commands"
              dataAttribute={{ "data-composer-slash-menu": "true" }}
              ignoreOutsideRefs={[editorShellRef]}
              listId={slashListId}
              listRole="listbox"
              onDismiss={() => setSlashMenuDismissed(true)}
            >
              {slashEntries.length === 0 ? (
                <ComposerSuggestionEmpty>No commands</ComposerSuggestionEmpty>
              ) : (
                slashEntries.map((entry, index) => (
                  <ComposerSuggestionRow
                    description={entry.description}
                    hasSubmenu={entry.hasSubmenu}
                    highlighted={index === slashSelectedIndex}
                    icon={<Glyph name={slashCommandGlyph(entry.id)} size={16} />}
                    id={`${slashListId}-opt-${index}`}
                    key={entry.id}
                    onHighlight={() => {
                      setSlashNavSource("pointer");
                      setSlashSelectedIndex(index);
                    }}
                    onSelect={() => runSlashCommand(entry.id)}
                    query={slashNeedle}
                    scrollIntoViewOnHighlight={slashNavSource === "keyboard"}
                    title={entry.title}
                  />
                ))
              )}
            </ComposerSuggestionMenu>
          ) : null}
          {contextMenuOpen && !projectedFilePickerOpen ? (
            <ComposerSuggestionMenu
              ariaLabel="Add files and more"
              containerRef={contextMenuRef}
              dataAttribute={{ "data-composer-overlay-floating-ui": "true", "data-settings-layer": "true" }}
              ignoreOutsideRefs={[contextTriggerRef]}
              listId={contextListId}
              listRole="menu"
              onDismiss={() => setContextMenuOpen(false)}
            >
              {contextMenuItems.map((item, index) => (
                <ComposerSuggestionRow
                  description={item.description}
                  hasSubmenu={item.hasSubmenu}
                  highlighted={index === contextSelectedIndex}
                  icon={<Glyph name={item.icon} size={16} />}
                  id={`${contextListId}-opt-${index}`}
                  key={item.key}
                  onHighlight={() => {
                    setContextNavSource("pointer");
                    setContextSelectedIndex(index);
                  }}
                  onSelect={item.onSelect}
                  role="menuitem"
                  scrollIntoViewOnHighlight={contextNavSource === "keyboard"}
                  title={item.title}
                />
              ))}
            </ComposerSuggestionMenu>
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
          <div
            className="composer-surface-chrome relative z-10 flex flex-col text-token-foreground composer-multiline-surface"
            data-composer-body=""
            data-dictation-active={voiceLayoutActive ? "true" : undefined}
            data-dictation-restoring={dictationRestoring ? "true" : undefined}
          >
            <div className="relative z-10 flex min-h-0 flex-1 flex-col">
              <div className="composer-attachments-default">
                {attachments.length ? (
                <ChatAttachmentStrip attachments={attachments} onRemove={onRemoveAttachment} />
                ) : null}
              </div>

              <div className="contents">
                <div className="composer-editor-shell mb-2 flex-grow overflow-y-auto px-3" ref={editorShellRef}>
                  <RichComposerEditor
                    ariaActiveDescendant={
                      slashMenuOpen && slashEntries[slashSelectedIndex]
                        ? `${slashListId}-opt-${slashSelectedIndex}`
                        : atMultiSourceOpen && atFilteredSources[atSourceSelectedIndex]
                          ? `${atSourceListId}-opt-${atSourceSelectedIndex}`
                          : undefined
                    }
                    ariaControls={slashMenuOpen ? slashListId : atMultiSourceOpen ? atSourceListId : undefined}
                    className="composer-editor-root text-size-chat [&_.ProseMirror]:focus-visible:outline-none text-token-foreground h-auto max-h-[25dvh] overflow-y-auto [&_.ProseMirror]:h-auto [&_.ProseMirror]:resize-none [&_.ProseMirror_p]:m-0 [&_.ProseMirror_ul]:pl-6 [&_.ProseMirror_ol]:list-decimal [&_.ProseMirror_ol]:pl-6 text-base [&_.ProseMirror]:leading-5"
                    onFilesPicked={onFilesPicked}
                    onKeyDown={handleEditorKeyDown}
                    onTextChange={onTextChange}
                    placeholder={placeholder}
                    suggestion={attachments.length || slashMenuOpen || atMentionOpen ? null : suggestion}
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
                <div className="composer-footer grid grid-cols-[minmax(0,auto)_auto_minmax(0,1fr)] items-center px-2 select-none">
                  <div className="flex min-w-0 items-center gap-2">
                    <div className="composer-context-anchor">
                      <ShellTooltip label="Add files and more" shortcut="@" disabled={contextMenuOpen}>
                        <button
                          aria-expanded={contextMenuOpen}
                          aria-label="Add files and more"
                          className="border-token-border no-drag cursor-interaction flex items-center justify-center gap-1 border whitespace-nowrap select-none focus:outline-none disabled:cursor-not-allowed disabled:opacity-40 rounded-full text-token-text-tertiary enabled:hover:bg-token-list-hover-background data-[state=open]:bg-token-list-hover-background border-transparent h-token-button-composer px-0 py-0 text-sm leading-[18px] aspect-square"
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
                          isSending={dictation.isSending}
                          phase={dictation.phase}
                          reducedMotion={reducedMotion}
                          waveformCanvasRef={dictation.waveformCanvasRef}
                          onCancel={dictation.abortDictation}
                          onPrepare={dictation.prepareDictation}
                          onRetry={dictation.retryDictation}
                          onStart={dictation.startDictation}
                          onStop={() => dictation.stopDictation()}
                        />
                        {editingQueuedTurn ? <button className="composer-queue-action" type="button" onClick={onCancelQueuedEdit}>Cancel edit</button> : null}
                        {!voiceLayoutActive && turnOpen && hasSubmissionPayload && onInterrupt ? (
                          <ShellTooltip label="Stop">
                            <button aria-label="Stop" className="composer-round-button" disabled={interruptPending} onClick={onInterrupt} type="button">
                              <Glyph name="stop" size={16} />
                            </button>
                          </ShellTooltip>
                        ) : null}
                        {(() => {
                          const dictationSendMode = voiceLayoutActive;
                          const voiceMode = !voiceLayoutActive && variant === "home" && !isSubmitting && !stopAvailable && !resumeActionAvailable && !hasSubmissionPayload;
                          const dictationSendDisabled = !dictation.isDictating || isSubmitting || Boolean(submitBlockReason && submitBlockReason !== "Type a message or attach a file.");
                          const voiceDisabled = Boolean(blockingReason) || voiceStartDisabled || !onVoiceStart;
                          const primaryBusy = dictationSendMode ? dictation.isSending : submitPending;
                          const primaryDisabled = dictationSendMode ? dictationSendDisabled : voiceMode ? voiceDisabled : submitDisabled;
                          const primaryButtonDisabled = dictationSendMode || voiceMode ? primaryDisabled : (submitDisabled && !queueMenuAvailable);
                          const primaryAccessibleName = dictationSendMode ? "Send" : voiceMode ? "Open Voice" : submitAccessibleName;
                          const primaryTooltip = dictationSendMode ? "Send" : voiceMode ? "Open Voice" : submitTooltip;
                          const primaryTooltipDisabled = dictationSendMode ? dictation.isSending : voiceMode ? false : sendMenuOpen;
                          const primaryMorphState: ComposerPrimaryActionState | null = dictationSendMode
                            ? "send"
                            : voiceMode
                              ? "voice"
                              : stopAvailable
                                ? "stop"
                                : resumeActionAvailable
                                  ? null
                                  : isSubmitting || submissionMode === "send"
                                    ? "send"
                                    : null;
                          return (
                            <ShellTooltip label={primaryTooltip} disabled={primaryTooltipDisabled}>
                              <button
                                aria-busy={primaryBusy || undefined}
                                aria-label={primaryAccessibleName}
                                aria-disabled={(!dictationSendMode && !voiceMode && submitDisabled) || undefined}
                                className={`${dictationSendMode ? "composer-dictation-send " : ""}composer-primary-action cursor-interaction size-token-button-composer flex items-center justify-center rounded-full p-0.5 transition-opacity focus-visible:outline-2 disabled:cursor-default${primaryBusy ? " disabled:opacity-100" : " disabled:opacity-50 aria-disabled:cursor-default aria-disabled:opacity-50"}`}
                                disabled={primaryButtonDisabled}
                                ref={sendButtonRef}
                                onContextMenu={(event) => {
                                  if (dictationSendMode || voiceMode || !onQueue || !turnOpen || editingQueuedTurn) return;
                                  event.preventDefault(); setSendMenuOpen(true);
                                }}
                                onKeyDown={(event) => {
                                  if (!dictationSendMode && !voiceMode && (event.key === "ContextMenu" || (event.shiftKey && event.key === "F10")) && onQueue && turnOpen && !editingQueuedTurn) {
                                    event.preventDefault(); setSendMenuOpen(true);
                                  }
                                }}
                                onClick={() => {
                                  if (dictationSendMode) {
                                    if (!dictationSendDisabled) dictation.stopDictation("send");
                                    return;
                                  }
                                  if (voiceMode) {
                                    if (!voiceDisabled) onVoiceStart?.();
                                    return;
                                  }
                                  if (submitDisabled) return;
                                  stopAvailable ? onInterrupt?.() : resumeActionAvailable ? onResume?.() : onSubmit();
                                }}
                                type="button"
                              >
                                {primaryBusy
                                  ? <ComposerSubmitSpinner />
                                  : primaryMorphState !== null
                                    ? <ComposerPrimaryActionIcon className="icon-xs" state={primaryMorphState} />
                                    : <Glyph
                                        className={resumeActionAvailable ? "icon-primary-action" : "icon-xs"}
                                        name={resumeActionAvailable ? "resume" : submissionMode === "steer" ? "sendNow" : "steer"}
                                        size={resumeActionAvailable ? 20 : 16}
                                      />}
                              </button>
                            </ShellTooltip>
                          );
                        })()}
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
    const isCtrlNext = event.ctrlKey && !event.metaKey && !event.altKey && event.key === "n";
    const isCtrlPrev = event.ctrlKey && !event.metaKey && !event.altKey && event.key === "p";
    if (slashMenuOpen) {
      if ((event.key === "ArrowDown" || event.key === "ArrowUp" || isCtrlNext || isCtrlPrev) && slashEntries.length > 0) {
        event.preventDefault();
        event.stopPropagation();
        const direction = event.key === "ArrowDown" || isCtrlNext ? 1 : -1;
        setSlashNavSource("keyboard");
        setSlashSelectedIndex((current) => (current + direction + slashEntries.length) % slashEntries.length);
        return;
      }
      if ((event.key === "Enter" || event.key === "Tab") && slashEntries.length > 0) {
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
    if (atMultiSourceOpen) {
      if ((event.key === "ArrowDown" || event.key === "ArrowUp" || isCtrlNext || isCtrlPrev) && atFilteredSources.length > 0) {
        event.preventDefault();
        event.stopPropagation();
        const direction = event.key === "ArrowDown" || isCtrlNext ? 1 : -1;
        setAtSourceNavSource("keyboard");
        setAtSourceSelectedIndex((current) => (current + direction + atFilteredSources.length) % atFilteredSources.length);
        return;
      }
      if ((event.key === "Enter" || event.key === "Tab") && atFilteredSources.length > 0) {
        event.preventDefault();
        event.stopPropagation();
        const source = atFilteredSources[atSourceSelectedIndex];
        if (source) {
          setAtMentionDismissed(true);
          setProjectedFileSourceKey(projectedSourceKey(source));
          setProjectedFileInitialQuery(atNeedle);
          setProjectedFilePickerOpen(true);
        }
        return;
      }
      if (event.key === "Escape") {
        event.preventDefault();
        event.stopPropagation();
        setAtMentionDismissed(true);
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
      backdropClassName="octopus-dialog-overlay bg-[var(--modal-backdrop-background)]"
      initialFocusRef={cancelRef}
      onClose={() => onResolve?.("cancel")}
      open={count > 0}
      panelClassName="octopus-dialog w-[460px] max-w-[calc(100vw-2rem)] rounded-3xl bg-surface-elevated-secondary/90 p-5 text-token-foreground ring-[0.5px] ring-token-border backdrop-blur-xl"
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

function replaceComposerAtMention(current: string, path: string): string {
  const mention = `@${path} `;
  return current.replace(/(^|[\s\n])@[^\s]*$/, `$1${mention}`);
}

const COMPOSER_SLASH_COMMANDS: ReadonlyArray<{
  id: ClientCommandId;
  slug: string;
  title: string;
  description: string;
  aliases: readonly string[];
  hasSubmenu?: boolean;
}> = [
  { id: "open_model_picker", slug: "model", title: "Model", description: "Choose provider, model, or effort", aliases: ["provider", "effort", "reasoning"], hasSubmenu: true },
  { id: "attach_files", slug: "attach", title: "Attach files", description: "Add files and folders", aliases: ["upload", "folder", "add"] },
  { id: "attach_projected_file", slug: "file", title: "Projected file", description: "Attach a file from Worker Environment", aliases: ["projected", "workspace", "at"], hasSubmenu: true },
  { id: "start_dictation", slug: "dictate", title: "Dictate", description: "Speak your message", aliases: ["mic", "microphone", "speech"] },
  { id: "open_terminal", slug: "terminal", title: "Terminal", description: "Open Worker Environment terminal", aliases: ["shell", "bash", "console"] },
  { id: "open_browser", slug: "browser", title: "Browser", description: "Open Worker Environment browser", aliases: ["web", "preview"] },
  { id: "open_changes", slug: "changes", title: "Changes", description: "View workspace changes", aliases: ["diff", "git"] },
  { id: "review_changes", slug: "review", title: "Code review", description: "Review changes in this task", aliases: ["code review", "diff"] },
  { id: "toggle_task_summary", slug: "summary", title: "Summary", description: "Toggle task summary panel", aliases: ["panel", "overview"] },
  { id: "open_voice", slug: "voice", title: "Voice", description: "Start a voice conversation", aliases: ["call", "talk"] },
  { id: "open_settings", slug: "settings", title: "Settings", description: "Open Octopus settings", aliases: ["preferences", "config"] },
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
  return <OctopusSpinner className="size-4" />;
}

const DICTATION_WAVEFORM_EASE = [0.23, 1, 0.32, 1] as const;

function DictationButton({ canRetry, isStarting, isDictating, isTranscribing, isSending, phase, reducedMotion, waveformCanvasRef, onCancel, onPrepare, onRetry, onStart, onStop }: {
  canRetry: boolean;
  isStarting: boolean;
  isDictating: boolean;
  isTranscribing: boolean;
  isSending: boolean;
  phase: DictationPhase;
  reducedMotion: boolean;
  waveformCanvasRef: RefObject<HTMLCanvasElement | null>;
  onCancel: () => void;
  onPrepare: (settlePromise?: Promise<boolean>) => Promise<void>;
  onRetry: () => void;
  onStart: () => Promise<void>;
  onStop: () => void;
}) {
  const pendingPrepareRef = useRef<{ id: number; settle: (accepted: boolean) => void } | null>(null);
  const preparePromiseRef = useRef<Promise<void> | null>(null);
  const settlePrepared = useCallback((accepted: boolean) => {
    const pending = pendingPrepareRef.current;
    pendingPrepareRef.current = null;
    pending?.settle(accepted);
  }, []);
  const canPrepare = !isStarting && !isDictating && !isTranscribing && !canRetry;

  useEffect(() => {
    if (!canPrepare) return;
    const cancel = () => settlePrepared(false);
    const onKeyDown = (event: globalThis.KeyboardEvent) => {
      if (event.key === "Escape") cancel();
    };
    const onPointerUp = (event: globalThis.PointerEvent) => {
      const pending = pendingPrepareRef.current;
      if (pending?.id === event.pointerId) {
        window.setTimeout(() => {
          if (pendingPrepareRef.current === pending) cancel();
        }, 0);
      }
    };
    window.addEventListener("blur", cancel);
    window.addEventListener("keydown", onKeyDown, true);
    window.addEventListener("pointerup", onPointerUp, true);
    return () => {
      window.removeEventListener("blur", cancel);
      window.removeEventListener("keydown", onKeyDown, true);
      window.removeEventListener("pointerup", onPointerUp, true);
      cancel();
    };
  }, [canPrepare, settlePrepared]);

  const stopBusy = isStarting || (isTranscribing && !isSending);
  const label = isTranscribing && !isSending
    ? "Finishing dictation"
    : isStarting
      ? "Starting dictation; click to cancel"
      : isDictating || isSending
        ? "Stop dictation"
        : canRetry
          ? "Retry dictation"
          : "Dictate";
  const shortcut = phase === "recording" || phase === "starting" ? "Esc" : undefined;

  return (
    <ShellTooltip disabled={phase === "transcribing"} label={label} shortcut={shortcut}>
      <button
        aria-label={label}
        aria-busy={stopBusy || undefined}
        className={`composer-dictation-button composer-round-button${isDictating || isTranscribing ? " is-active" : ""}`}
        disabled={isTranscribing}
        onBlur={() => settlePrepared(false)}
        onPointerCancel={() => settlePrepared(false)}
        onPointerLeave={() => settlePrepared(false)}
        onPointerDown={(event) => {
          if (!event.isPrimary || event.button !== 0 || event.pointerType !== "mouse" || event.ctrlKey) {
            settlePrepared(false);
            return;
          }
          if (!canPrepare || preparePromiseRef.current !== null) return;
          let resolveSettle!: (accepted: boolean) => void;
          const settlePromise = new Promise<boolean>((resolve) => { resolveSettle = resolve; });
          pendingPrepareRef.current = { id: event.pointerId, settle: resolveSettle };
          const prepared = onPrepare(settlePromise);
          preparePromiseRef.current = prepared;
          const clear = () => {
            if (preparePromiseRef.current === prepared) preparePromiseRef.current = null;
          };
          void prepared.then(clear, clear);
        }}
        onClick={(event) => {
          if (event.detail > 0 && pendingPrepareRef.current && canPrepare) {
            settlePrepared(true);
            void onStart();
            return;
          }
          settlePrepared(false);
          if (isStarting) { onCancel(); return; }
          if (isDictating) { onStop(); return; }
          if (canRetry) { onRetry(); return; }
          const pending = preparePromiseRef.current;
          if (!pending) { void onStart(); return; }
          const button = event.currentTarget;
          void pending.catch(() => undefined).then(() => {
            if (button.isConnected && !button.disabled) return onStart();
          });
        }}
        type="button"
      >
        <AnimatePresence initial={false}>
          {isDictating ? (
            <motion.span
              key="waveform"
              animate={{ width: "auto", opacity: 1 }}
              className="composer-dictation-waveform-motion"
              exit={{ width: 0, opacity: 0 }}
              initial={{ width: 0, opacity: 0 }}
              transition={{ duration: reducedMotion ? 0 : 0.3, ease: DICTATION_WAVEFORM_EASE }}
            >
              <span className="composer-dictation-waveform">
                <canvas aria-hidden="true" data-mode="text" ref={waveformCanvasRef} />
              </span>
            </motion.span>
          ) : null}
        </AnimatePresence>
        <span className="composer-dictation-icon">
          {stopBusy ? <span className="composer-spinner" /> : <DictationIcon name={isDictating || isSending ? "stop" : canRetry ? "retry" : "microphone"} />}
        </span>
      </button>
    </ShellTooltip>
  );
}

function projectedSourceKey(source: ProjectedFileSource): string {
  return `${source.workerEnvironmentId}\0${source.surfaceId}`;
}

function removeComposerSlashQuery(value: string): string {
  return value.replace(/(?:^|\n)\/[^\n]*$/, "").replace(/\n$/, "");
}
