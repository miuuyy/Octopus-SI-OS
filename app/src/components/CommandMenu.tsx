import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { isolateModalLayer, isTopmostModal, restoreModalFocus } from "../design-system/modalFocus";
import { searchChatTaskMessages, type ChatRun, type ChatRunMessage } from "../chatRuns";
import { requestChatMessageReveal } from "../chatMessageReveal";
import { clientCommands, dispatchClientCommand, type ClientCommandId } from "../clientActions";
import { clientExperimentEnabled, recordClientExperimentSignal, recordMenuCommandUse } from "../clientExperiments";
import { formatKeyboardShortcuts, useKeyboardShortcuts } from "../keyboardShortcuts";
import { appToast } from "../toasts";
import type { OctopusClientAccessProfile } from "../electron";
import {
  clientTaskTitle,
  recordClientDestination,
  recordClientTaskOpened,
  workspaceForSurface,
  useClientTaskOrganization,
} from "../shell/clientWorkspaces";
import type { ClientSurfaceDescriptor } from "../shell/surfaceTypes";
import { navigationSectionLabel } from "../shell/navigation";
import { Glyph, type GlyphName } from "./IconGlyphs";

type CommandMenuEntry = {
  id: string;
  label: string;
  detail: string;
  group: string;
  glyph: GlyphName;
  shortcut?: string;
  closeOnRun?: boolean;
  run: () => void;
};

type TaskMessageMatch = {
  message: ChatRunMessage;
  run: ChatRun;
  snippet: string;
  sourceRunId: string;
};

type TaskMessageSearchState = {
  matches: TaskMessageMatch[];
  hasOlder: boolean;
  nextBeforeSeq: number;
  taskCount: number;
  runCount: number;
  error: string;
};

const emptyTaskMessageSearch: TaskMessageSearchState = {
  matches: [],
  hasOlder: false,
  nextBeforeSeq: -1,
  taskCount: 0,
  runCount: 0,
  error: "",
};

type Props = {
  activeSurfaceId: string;
  accessProfile: OctopusClientAccessProfile;
  chatRuns: ChatRun[];
  onClose: () => void;
  onOpenRun: (runId: string) => void;
  onSelectSurface: (surface: ClientSurfaceDescriptor) => void;
  open: boolean;
  surfaces: ClientSurfaceDescriptor[];
  voiceActive: boolean;
};

const commandPresentation: Partial<Record<ClientCommandId, { glyph: GlyphName; group: string }>> = {
  new_chat: { glyph: "newTask", group: "Task" },
  open_voice: { glyph: "voice", group: "Task" },
  open_settings: { glyph: "settings", group: "App" },
  focus_search: { glyph: "search", group: "Navigation" },
  navigate_back: { glyph: "navigateBack", group: "Navigation" },
  navigate_forward: { glyph: "navigateBack", group: "Navigation" },
  previous_surface: { glyph: "arrowLeft", group: "Navigation" },
  next_surface: { glyph: "arrowLeft", group: "Navigation" },
  toggle_sidebar: { glyph: "sidebarClosed", group: "Panels" },
  toggle_split: { glyph: "splitPane", group: "Panels" },
  toggle_task_summary: { glyph: "taskSummary", group: "Panels" },
  open_terminal: { glyph: "terminal", group: "Panels" },
  open_browser: { glyph: "browser", group: "Panels" },
  open_changes: { glyph: "changes", group: "Panels" },
  review_changes: { glyph: "changes", group: "Task" },
  start_dictation: { glyph: "microphone", group: "Task" },
  attach_files: { glyph: "paperclip", group: "Task" },
  attach_projected_file: { glyph: "folder", group: "Task" },
  copy_task_markdown: { glyph: "copy", group: "Task" },
  copy_run_id: { glyph: "copy", group: "Task" },
  copy_working_directory: { glyph: "folder", group: "Task" },
  toggle_pin_task: { glyph: "pin", group: "Task" },
  open_model_picker: { glyph: "sliders", group: "Task" },
  open_side_task: { glyph: "splitPane", group: "Task" },
  previous_operator_message: { glyph: "chevronDown", group: "Navigation" },
  next_operator_message: { glyph: "chevronDown", group: "Navigation" },
  toggle_gesture_control: { glyph: "hand", group: "App" },
  voice_primary: { glyph: "microphone", group: "Voice" },
  voice_end: { glyph: "phoneEnd", group: "Voice" },
};

const SHORTCUT_COACH_USES = 3;

function coachShortcut(accessProfile: OctopusClientAccessProfile, command: { id: ClientCommandId; label: string }, shortcut: string) {
  if (!shortcut || !clientExperimentEnabled(accessProfile, "shortcut_coach")) return;
  if (recordMenuCommandUse(accessProfile, command.id) !== SHORTCUT_COACH_USES) return;
  recordClientExperimentSignal(accessProfile, "shortcut_coach");
  appToast.info(`${command.label}: ${shortcut}`, 6, { dedupeKey: `shortcut-coach:${command.id}` });
}

const groupOrder = ["Suggested", "Task", "Recent", "Surfaces", "Message results", "Recent tasks", "Navigation", "Panels", "Voice", "App"];

export function CommandMenu({
  accessProfile,
  activeSurfaceId,
  chatRuns,
  onClose,
  onOpenRun,
  onSelectSurface,
  open,
  surfaces,
  voiceActive,
}: Props) {
  const keyboardShortcuts = useKeyboardShortcuts(accessProfile);
  const organization = useClientTaskOrganization(accessProfile);
  const dialogRef = useRef<HTMLDivElement>(null);
  const layerRef = useRef<HTMLDivElement>(null);
  const previousFocus = useMemo(() => open && document.activeElement instanceof HTMLElement ? document.activeElement : null, [open]);
  useLayoutEffect(() => {
    if (open && layerRef.current) return isolateModalLayer(layerRef.current);
  }, [open]);
  const inputRef = useRef<HTMLInputElement>(null);
  const messageSearchRequestRef = useRef(0);
  const onCloseRef = useRef(onClose);
  onCloseRef.current = onClose;
  const [query, setQuery] = useState("");
  const [selectedIndex, setSelectedIndex] = useState(0);
  const [messageSearch, setMessageSearch] = useState<TaskMessageSearchState>(emptyTaskMessageSearch);
  const [searchingMessages, setSearchingMessages] = useState(false);
  const visibleChatRuns = useMemo(() => {
    return chatRuns.filter((run) => !run.archived);
  }, [chatRuns]);

  useEffect(() => {
    const requestId = messageSearchRequestRef.current + 1;
    messageSearchRequestRef.current = requestId;
    const needle = normalize(query);
    if (!open || needle.length < 2) {
      setMessageSearch(emptyTaskMessageSearch);
      setSearchingMessages(false);
      return;
    }
    let cancelled = false;
    const timer = window.setTimeout(() => {
      setSearchingMessages(true);
      void searchChatTaskMessages(query, visibleChatRuns.map((run) => run.runId))
        .then((page) => {
          if (cancelled || messageSearchRequestRef.current !== requestId) return;
          const runById = new Map(visibleChatRuns.map((run) => [run.runId, run]));
          setMessageSearch({
            matches: page.matches.flatMap((match) => {
              const run = runById.get(match.taskRunId);
              return run ? [{ message: match.message, run, snippet: match.snippet, sourceRunId: match.sourceRunId }] : [];
            }),
            hasOlder: page.hasOlder,
            nextBeforeSeq: page.nextBeforeSeq,
            taskCount: page.scope.taskCount,
            runCount: page.scope.runCount,
            error: "",
          });
        })
        .catch((failure) => {
          if (cancelled || messageSearchRequestRef.current !== requestId) return;
          setMessageSearch({
            ...emptyTaskMessageSearch,
            taskCount: visibleChatRuns.length,
            error: failure instanceof Error ? failure.message : String(failure),
          });
        })
        .finally(() => {
          if (!cancelled && messageSearchRequestRef.current === requestId) setSearchingMessages(false);
        });
    }, 180);
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [open, query, visibleChatRuns]);

  const loadOlderMessageMatches = useCallback(() => {
    if (searchingMessages || !messageSearch.hasOlder || messageSearch.nextBeforeSeq < 0) return;
    const taskRunIds = visibleChatRuns.map((run) => run.runId);
    const requestId = messageSearchRequestRef.current + 1;
    messageSearchRequestRef.current = requestId;
    setSearchingMessages(true);
    void searchChatTaskMessages(query, taskRunIds, { beforeSeq: messageSearch.nextBeforeSeq })
      .then((page) => {
        if (messageSearchRequestRef.current !== requestId) return;
        const runById = new Map(visibleChatRuns.map((run) => [run.runId, run]));
        setMessageSearch((current) => {
          const nextMatches = page.matches.flatMap((match) => {
            const run = runById.get(match.taskRunId);
            return run ? [{ message: match.message, run, snippet: match.snippet, sourceRunId: match.sourceRunId }] : [];
          });
          const seen = new Set(current.matches.map((match) => `${match.run.runId}:${match.message.id}`));
          return {
            matches: [...current.matches, ...nextMatches.filter((match) => !seen.has(`${match.run.runId}:${match.message.id}`))],
            hasOlder: page.hasOlder,
            nextBeforeSeq: page.nextBeforeSeq,
            taskCount: page.scope.taskCount,
            runCount: page.scope.runCount,
            error: "",
          };
        });
      })
      .catch((failure) => {
        if (messageSearchRequestRef.current !== requestId) return;
        setMessageSearch((current) => ({
          ...current,
          error: failure instanceof Error ? failure.message : String(failure),
        }));
      })
      .finally(() => {
        if (messageSearchRequestRef.current === requestId) setSearchingMessages(false);
      });
  }, [messageSearch.hasOlder, messageSearch.nextBeforeSeq, query, searchingMessages, visibleChatRuns]);

  const entries = useMemo(() => {
    const commandEntries = clientCommands.flatMap<CommandMenuEntry>((command) => {
      if (command.id === "open_command_menu") return [];
      if ((command.id === "start_dictation" || command.id === "attach_files" || command.id === "attach_projected_file" || command.id === "open_model_picker") && activeSurfaceId !== "chat") return [];
      if ((command.id === "copy_task_markdown" || command.id === "copy_run_id" || command.id === "copy_working_directory" || command.id === "toggle_pin_task" || command.id === "open_side_task" || command.id === "toggle_task_summary" || command.id === "review_changes") && activeSurfaceId !== "chat") return [];
      if ((command.id === "previous_operator_message" || command.id === "next_operator_message") && activeSurfaceId !== "chat") return [];
      if ((command.id === "voice_primary" || command.id === "voice_end") && activeSurfaceId !== "voice") return [];
      if (command.id === "voice_end" && !voiceActive) return [];
      const presentation = commandPresentation[command.id] ?? { glyph: "settings" as const, group: "App" };
      return [{
        id: `command:${command.id}`,
        label: command.label,
        detail: command.detail,
        group: presentation.group,
        glyph: presentation.glyph,
        shortcut: formatKeyboardShortcuts(keyboardShortcuts[command.id] ?? []) || undefined,
        run: () => {
          dispatchClientCommand(command.id, "surface");
          coachShortcut(accessProfile, command, formatKeyboardShortcuts(keyboardShortcuts[command.id] ?? []));
        },
      }];
    });
    const surfaceEntries = surfaces.map<CommandMenuEntry>((surface) => {
      const workspaceKey = workspaceForSurface(surface);
      return {
        id: `surface:${surface.id}`,
        label: surface.label,
        detail: navigationSectionLabel(surface),
        group: "Surfaces",
        glyph: surface.glyph,
        shortcut: surface.id === activeSurfaceId ? "Current" : undefined,
        run: () => {
          onSelectSurface(surface);
          recordClientDestination(accessProfile, { workspaceKey, surfaceId: surface.id, runId: "" });
        },
      };
    });
    const recentRank = new Map(organization.recentRunIds.map((runId, index) => [runId, index]));
    const sortedRuns = [...visibleChatRuns]
      .sort((left, right) => {
        const leftRank = recentRank.get(left.runId) ?? Number.MAX_SAFE_INTEGER;
        const rightRank = recentRank.get(right.runId) ?? Number.MAX_SAFE_INTEGER;
        return leftRank - rightRank || right.updatedAtMs - left.updatedAtMs;
      });
    const runEntries = (normalize(query) ? sortedRuns : sortedRuns.slice(0, 12))
      .map<CommandMenuEntry>((run) => ({
        id: `run:${run.runId}`,
        label: clientTaskTitle(run.runId, run.title, organization),
        detail: `${run.messageCount} ${run.messageCount === 1 ? "message" : "messages"} · ${relativeDate(run.updatedAtMs)}${run.continuedFrom ? " · continued run" : ""}`,
        group: "Recent tasks",
        glyph: "chat",
        run: () => {
          onOpenRun(run.runId);
          recordClientTaskOpened(accessProfile, run.runId);
          recordClientDestination(accessProfile, { workspaceKey: "octopus-os", surfaceId: "chat", runId: run.runId });
        },
      }));
    const messageEntries = messageSearch.matches.map<CommandMenuEntry>(({ message, run, snippet }) => ({
      id: `message:${run.runId}:${message.id}`,
      label: clientTaskTitle(run.runId, run.title, organization),
      detail: `${message.role === "user" ? "Operator" : message.role === "assistant" ? "Agent" : "System"} · ${snippet}`,
      group: "Message results",
      glyph: "chat",
      run: () => {
        onOpenRun(run.runId);
        recordClientTaskOpened(accessProfile, run.runId);
        recordClientDestination(accessProfile, { workspaceKey: "octopus-os", surfaceId: "chat", runId: run.runId });
        requestChatMessageReveal({ runId: run.runId, messageId: message.id });
      },
    }));
    if (messageSearch.hasOlder) {
      messageEntries.push({
        id: `message-search-more:${messageSearch.nextBeforeSeq}`,
        label: "Search older task history",
        detail: `Continue search for “${query.trim()}” across ${messageSearch.taskCount} visible tasks`,
        group: "Message results",
        glyph: "search",
        closeOnRun: false,
        run: loadOlderMessageMatches,
      });
    }
    const surfaceById = new Map(surfaces.map((surface) => [surface.id, surface]));
    const runById = new Map(visibleChatRuns.map((run) => [run.runId, run]));
    const recentEntries = organization.recentDestinations.slice(0, 8).flatMap<CommandMenuEntry>((destination) => {
      if (destination.runId) {
        const run = runById.get(destination.runId);
        if (!run) return [];
        return [{
          id: `recent:${destination.key}`,
          label: clientTaskTitle(run.runId, run.title, organization),
          detail: "Tasks",
          group: "Recent",
          glyph: "chat",
          run: () => {
            onOpenRun(run.runId);
            recordClientTaskOpened(accessProfile, run.runId);
            recordClientDestination(accessProfile, { workspaceKey: "octopus-os", surfaceId: "chat", runId: run.runId });
          },
        }];
      }
      const surface = surfaceById.get(destination.surfaceId);
      if (!surface) return [];
      return [{
        id: `recent:${destination.key}`,
        label: surface.label,
        detail: navigationSectionLabel(surface),
        group: "Recent",
        glyph: surface.glyph,
        run: () => {
          onSelectSurface(surface);
          recordClientDestination(accessProfile, { workspaceKey: destination.workspaceKey, surfaceId: surface.id, runId: "" });
        },
      }];
    });
    return [...commandEntries, ...recentEntries, ...surfaceEntries, ...messageEntries, ...runEntries];
  }, [accessProfile, activeSurfaceId, keyboardShortcuts, loadOlderMessageMatches, messageSearch.hasOlder, messageSearch.matches, messageSearch.nextBeforeSeq, messageSearch.taskCount, onOpenRun, onSelectSurface, organization, query, surfaces, visibleChatRuns, voiceActive]);
  const filtered = useMemo(() => {
    const needle = normalize(query);
    const candidates = needle
      ? entries
          .map((entry) => ({ entry, score: matchScore(entry, needle) }))
          .filter((candidate) => candidate.score >= 0)
          .sort((left, right) => right.score - left.score || groupRank(left.entry.group) - groupRank(right.entry.group) || left.entry.label.localeCompare(right.entry.label))
          .map((candidate) => candidate.entry)
      : entries.filter((entry) => (
          entry.id === "command:new_chat"
          || entry.id === "command:open_voice"
          || entry.id === "command:open_settings"
          || entry.group === "Recent"
          || entry.group === "Surfaces"
          || entry.group === "Recent tasks"
      ));
    let titleMatches = 0;
    return candidates.filter((entry) => !needle || !entry.id.startsWith("run:") || titleMatches++ < 12).map((entry) => ({
      ...entry,
      group: needle ? entry.group : entry.id.startsWith("command:") ? "Suggested" : entry.group,
    }));
  }, [entries, query]);
  const grouped = useMemo(() => groupOrder.flatMap((group) => {
    const items = filtered.filter((entry) => entry.group === group);
    return items.length ? [{ group, items }] : [];
  }), [filtered]);
  const orderedEntries = useMemo(() => grouped.flatMap((group) => group.items), [grouped]);

  useEffect(() => {
    if (!open) return;
    setQuery("");
    setSelectedIndex(0);
    const frame = window.requestAnimationFrame(() => inputRef.current?.focus());
    const handleKeyDown = (event: globalThis.KeyboardEvent) => {
      if (event.defaultPrevented || !isTopmostModal(layerRef.current)) return;
      if (event.key === "Escape") {
        event.preventDefault();
        onCloseRef.current();
        return;
      }
      if (event.key !== "Tab") return;
      const focusable = [...(dialogRef.current?.querySelectorAll<HTMLElement>('input:not([disabled]), button:not([disabled]), [href], [tabindex]:not([tabindex="-1"])') ?? [])];
      if (!focusable.length) return;
      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      if (event.shiftKey && document.activeElement === first) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first.focus();
      }
    };
    window.addEventListener("keydown", handleKeyDown);
    return () => {
      window.cancelAnimationFrame(frame);
      window.removeEventListener("keydown", handleKeyDown);
      restoreModalFocus(previousFocus);
    };
  }, [open, previousFocus]);

  useEffect(() => {
    setSelectedIndex((current) => Math.min(current, Math.max(0, orderedEntries.length - 1)));
  }, [orderedEntries.length]);

  useLayoutEffect(() => {
    const selected = orderedEntries[selectedIndex];
    if (!open || !selected) return;
    document.getElementById(commandDomId(selected.id))?.scrollIntoView({ block: "nearest" });
  }, [open, orderedEntries, selectedIndex]);

  if (!open) return null;

  function activate(entry: CommandMenuEntry | undefined) {
    if (!entry) return;
    if (entry.closeOnRun !== false) onClose();
    entry.run();
  }

  function onKeyDown(event: React.KeyboardEvent<HTMLInputElement>) {
    if (event.key === "Escape") {
      event.preventDefault();
      onClose();
      return;
    }
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      if (!orderedEntries.length) return;
      const delta = event.key === "ArrowDown" ? 1 : -1;
      setSelectedIndex((current) => (current + delta + orderedEntries.length) % orderedEntries.length);
      return;
    }
    if (event.key === "Enter") {
      event.preventDefault();
      activate(orderedEntries[selectedIndex]);
    }
  }

  let flatIndex = -1;
  return (
    <div
      aria-label="Command menu"
      aria-modal="true"
      className="command-menu-overlay fixed inset-0"
      ref={layerRef}
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
      role="dialog"
    >
      <div className="octopus-command-menu fixed left-1/2 z-50 flex -translate-x-1/2 flex-col overflow-hidden rounded-2xl border border-token-border text-token-foreground" ref={dialogRef}>
        <input
          aria-activedescendant={orderedEntries[selectedIndex]?.id ? commandDomId(orderedEntries[selectedIndex].id) : undefined}
          aria-autocomplete="list"
          aria-controls="octopus-command-menu-list"
          aria-expanded={open}
          className="command-menu-input w-full min-w-0 shrink-0 bg-transparent text-token-foreground outline-none placeholder:text-token-input-placeholder-foreground"
          onChange={(event) => {
            setQuery(event.currentTarget.value);
            setSelectedIndex(0);
          }}
          onKeyDown={onKeyDown}
          placeholder="Search tasks or run a command"
          ref={inputRef}
          role="combobox"
          value={query}
        />
        <div className="command-menu-list min-h-0 overflow-y-auto" id="octopus-command-menu-list" role="listbox">
          {grouped.map(({ group, items }) => (
            <section aria-label={group} className="flex flex-col gap-1" key={group}>
              <div className="pe-2 ps-[calc(var(--spacing)*.75)] pt-2 text-sm text-token-description-foreground">{group}</div>
              {items.map((entry) => {
                flatIndex += 1;
                const index = flatIndex;
                const selected = selectedIndex === index;
                const rotate = entry.id === "command:navigate_forward"
                  || entry.id === "command:next_surface"
                  || entry.id === "command:previous_operator_message";
                return (
                  <button
                    aria-selected={selected}
                    className="command-menu-item flex w-full items-center gap-2 text-left text-token-foreground outline-none"
                    id={commandDomId(entry.id)}
                    key={entry.id}
                    onClick={() => activate(entry)}
                    onMouseMove={() => setSelectedIndex(index)}
                    role="option"
                    type="button"
                  >
                    <Glyph className={`shrink-0 opacity-75${rotate ? " rotate-180" : ""}`} name={entry.glyph} size={15} />
                    <span className="min-w-0 flex-1">
                      <span className="block truncate">{entry.label}</span>
                      {query ? <span className="block truncate text-sm text-token-description-foreground">{entry.detail}</span> : null}
                    </span>
                    {entry.shortcut ? <span className="shrink-0 text-sm text-token-description-foreground">{entry.shortcut}</span> : null}
                  </button>
                );
              })}
            </section>
          ))}
          {!filtered.length ? <div className="command-menu-empty-state text-lg text-token-description-foreground">{searchingMessages ? "Searching indexed task transcripts…" : "No matches"}</div> : null}
        </div>
        {messageSearch.error ? <div className="command-menu-status text-sm text-token-error-foreground" role="status" tabIndex={0}>Task transcript search unavailable · {messageSearch.error}</div> : null}
        {normalize(query).length >= 2 && !messageSearch.error ? (
          <div className="command-menu-status text-sm text-token-description-foreground" role="status" tabIndex={0}>
            {searchingMessages
              ? "Searching indexed task transcripts…"
              : `Finalized operator and agent messages · ${messageSearch.taskCount} visible tasks · ${messageSearch.runCount} runs`}
          </div>
        ) : null}
      </div>
    </div>
  );
}

function matchScore(entry: CommandMenuEntry, needle: string): number {
  const label = normalize(entry.label);
  const haystack = normalize(`${entry.label} ${entry.detail} ${entry.group}`);
  if (label === needle) return 1000;
  if (label.startsWith(needle)) return 800 - label.length;
  const labelIndex = label.indexOf(needle);
  if (labelIndex >= 0) return 600 - labelIndex;
  const haystackIndex = haystack.indexOf(needle);
  if (haystackIndex >= 0) return 300 - haystackIndex;
  const tokens = needle.split(" ").filter(Boolean);
  if (tokens.every((token) => haystack.includes(token))) return 100 - haystack.length;
  const compactNeedle = needle.replace(/\s+/g, "");
  const compactLabel = label.replace(/\s+/g, "");
  const subsequence = subsequenceScore(compactLabel, compactNeedle);
  if (subsequence >= 0) return 80 + subsequence;
  const initials = label.split(/[^\p{L}\p{N}]+/u).filter(Boolean).map((word) => word[0]).join("");
  return initials.startsWith(compactNeedle) ? 70 - initials.length : -1;
}

function subsequenceScore(haystack: string, needle: string): number {
  if (!needle) return 0;
  let cursor = 0;
  let score = 0;
  let previous = -2;
  for (const character of needle) {
    const index = haystack.indexOf(character, cursor);
    if (index < 0) return -1;
    score += index === previous + 1 ? 3 : Math.max(0, 2 - index);
    previous = index;
    cursor = index + 1;
  }
  return score - Math.max(0, haystack.length - needle.length) / 4;
}

function normalize(value: string): string {
  return value.trim().toLocaleLowerCase().replace(/\s+/g, " ");
}

function groupRank(group: string): number {
  const index = groupOrder.indexOf(group);
  return index < 0 ? groupOrder.length : index;
}

function commandDomId(id: string): string {
  return `command-menu-${id.replace(/[^a-z0-9_-]/gi, "-")}`;
}

function relativeDate(timestamp: number): string {
  const elapsed = Math.max(0, Date.now() - timestamp);
  if (elapsed < 60_000) return "Just now";
  if (elapsed < 3_600_000) return `${Math.floor(elapsed / 60_000)}m ago`;
  if (elapsed < 86_400_000) return `${Math.floor(elapsed / 3_600_000)}h ago`;
  return new Date(timestamp).toLocaleDateString();
}
