import { Fragment, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { motion } from "motion/react";
import { useShouldReduceMotion } from "../../appearance";
import { CadencedShimmerText, ShimmerActivityScope } from "../CadencedShimmerText";
import { Glyph } from "../IconGlyphs";
import { MessageContent } from "./MessageContent";
import { reasoningSummaryHeading } from "./reasoningSummary";
import { baseDynamicToolId, dynamicToolGlyph, dynamicToolPresentation, dynamicToolReadLabel, dynamicToolSummary, webSearchActivityDetail, webSearchActivityLabel } from "./toolPresentation";
import { conversationTraceAdmission, isFinalAssistantResponse } from "./conversationTrace";
import { commandActivityModel, commandHasProcessEvidence, commandResultBody } from "./commandActivityModel";
import {
  multiAgentAction,
  multiAgentIds,
  multiAgentStateStatus,
  multiAgentStatus,
  normalizeMultiAgentActivity,
  normalizeSubagentActivity,
  subagentDisplayStatus,
  type MultiAgentAction,
  type MultiAgentActivityItem,
  type MultiAgentStateStatus,
  type MultiAgentStatus,
  type SubagentActivityItem,
} from "./subagentActivity";
import type { ChatRenderMessage, ChatToolPresentationIndex } from "./types";

type Props = {
  activityWindow?: "all" | "latest-context";
  activities: ChatRenderMessage[];
  finalStartedAtMs?: number;
  presentation: "conversation" | "ledger";
  renderActivity: (message: ChatRenderMessage) => ReactNode;
  response?: ChatRenderMessage;
  statusBoundaryVisible?: boolean;
  startedAtMs: number;
  toolPresentations: ChatToolPresentationIndex;
  turnInProgress?: boolean;
  blockingReason?: string;
  revealFind?: { messageId: string; index: number };
};

type ActivityFamily =
  | "commentary"
  | "reasoning"
  | "plan"
  | "exec"
  | "files"
  | "web"
  | "hooks"
  | "mcp"
  | "dynamic_tool"
  | "collaboration"
  | "images"
  | "sleep"
  | "review"
  | "compaction"
  | "model"
  | "warning"
  | "rate_limits"
  | "goal"
  | "operator"
  | "turn_control"
  | "other";

type ActivityEntry = {
  id: string;
  family: ActivityFamily;
  messages: ChatRenderMessage[];
  active: boolean;
  tone: "default" | "warning" | "danger";
};

type ActivityUnit =
  | { id: string; kind: "group"; entries: ActivityEntry[] }
  | { id: string; kind: "multi-agent-group"; entries: ActivityEntry[] }
  | { id: string; kind: "subagent-group"; entries: ActivityEntry[] }
  | { id: string; kind: "standalone"; entry: ActivityEntry };

const GROUPABLE_FAMILIES = new Set<ActivityFamily>([
  "exec",
  "files",
  "web",
  "mcp",
  "dynamic_tool",
]);

type DisclosureState = "collapsed" | "opening" | "expanded" | "closing";
type ActivityGlyph = Parameters<typeof Glyph>[0]["name"];
type CompletedGroupSegment = {
  following: string;
  icon: ActivityGlyph | null;
  leading: string;
};

export function TurnActivity({ activityWindow = "all", activities, finalStartedAtMs, presentation, renderActivity, response, revealFind, statusBoundaryVisible = true, startedAtMs, toolPresentations, turnInProgress, blockingReason = "" }: Props) {
  const reducedMotion = useShouldReduceMotion();
  const syntheticSubmitting = activities.some((message) => message.eventData?.work_status === "submitting");
  const syntheticWorking = activities.some((message) => message.eventData?.work_status === "working");
  const visible = activities.filter((message) => (
    !["submitting", "working"].includes(String(message.eventData?.work_status ?? ""))
    && (
      presentation === "ledger"
      || chatActivityVisible(message, toolPresentations) && safetyBufferingVisible(message)
    )
  ));
  const terminalTraceError = response?.error && response.terminal === true
    ? response
    : [...visible].reverse().find((message) => message.error && message.terminal === true);
  const visibleActivity = terminalTraceError
    ? visible.filter((message) => message !== terminalTraceError)
    : visible;
  const stopped = activities.some(turnWasStopped);
  const stoppedByOperator = activities.some(turnStoppedByOperator);
  const finalStarted = Boolean(response && isFinalAssistantResponse(response));
  const failed = Boolean(terminalTraceError) && !stopped;
  const turnCompleted = activities.some(turnWasCompleted);
  const turnFinished = finalStarted || failed || stopped || turnCompleted;
  const blocked = Boolean(blockingReason) && !turnFinished;
  const working = !blocked && !finalStarted && !failed && !stopped && !turnCompleted && Boolean(
    turnInProgress ?? (
      syntheticWorking
      || syntheticSubmitting
      || coalesceActivities(visibleActivity).some((entry) => entry.active)
    )
  );
  const settled = blocked || presentation === "ledger"
    ? visibleActivity
    : !working
      ? settleOpenTextStreams(visibleActivity, failed ? "failed" : "completed")
      : visibleActivity;
  const presented = presentation === "conversation"
    ? conversationActivityMessages(settled, toolPresentations).filter((message) => (
      !turnFinished || !isSafetyBufferingWarning(message)
    ))
    : settled;
  const units = useMemo(
    () => {
      const projected = buildActivityUnits(
        presented,
        toolPresentations,
        presentation === "ledger",
        working,
      );
      return activityWindow === "latest-context" ? latestContextActivityUnits(projected) : projected;
    },
    [activityWindow, presentation, presented, toolPresentations, working],
  );
  const hasConversationReasoningWork = presentation === "conversation"
    && visibleActivity.some((message) => activityFamily(message) === "reasoning");
  const thinkingFallbackMessage = presentation === "conversation"
    ? latestReasoningSummaryHeading(presented) || "Thinking"
    : "Thinking";
  const progress = activityProgressSurface(units, working);
  const activeGroupId = blocked ? null : progress.activeGroupId;
  const showThinkingFallback = !blocked && progress.showThinkingFallback;
  const [historyDisclosureState, setHistoryDisclosureState] = useState<DisclosureState>("collapsed");
  const [, setClock] = useState(0);
  const historyOpeningFrameRef = useRef<number | null>(null);
  const sectionRef = useRef<HTMLElement>(null);
  const stableStartedAtRef = useRef<number | null>(null);

  useEffect(() => {
    if (!working) return;
    const timer = window.setInterval(() => setClock(Date.now()), 1_000);
    return () => window.clearInterval(timer);
  }, [working]);

  useEffect(() => () => {
    if (historyOpeningFrameRef.current !== null) window.cancelAnimationFrame(historyOpeningFrameRef.current);
  }, []);

  useEffect(() => {
    if (turnFinished) return;
    if (historyOpeningFrameRef.current !== null) {
      window.cancelAnimationFrame(historyOpeningFrameRef.current);
      historyOpeningFrameRef.current = null;
    }
    setHistoryDisclosureState("collapsed");
  }, [turnFinished]);

  const historyExpanded = historyDisclosureState === "opening" || historyDisclosureState === "expanded";
  const historyFullyExpanded = historyDisclosureState === "expanded";
  const toggleHistory = () => {
    if (historyExpanded) {
      setHistoryDisclosureState(reducedMotion ? "collapsed" : "closing");
      return;
    }
    if (historyDisclosureState === "closing" || reducedMotion) {
      setHistoryDisclosureState("expanded");
      return;
    }
    setHistoryDisclosureState("opening");
    historyOpeningFrameRef.current = window.requestAnimationFrame(() => {
      historyOpeningFrameRef.current = null;
      setHistoryDisclosureState((current) => current === "opening" ? "expanded" : current);
    });
  };

  const hasWork = units.length > 0;
  const startingTask = syntheticSubmitting
    && !blocked
    && !hasWork
    && !response
    && !failed
    && !stopped;
  const showStandaloneWorkedBoundary = finalStarted
    && !failed
    && !stopped
    && !hasWork
    && hasConversationReasoningWork;
  const showWorkedForBoundary = !blocked && statusBoundaryVisible
    && (hasWork || failed || stopped || showStandaloneWorkedBoundary);
  if (startingTask) {
    return (
      <section className="octopus-turn-activity min-h-0 min-w-0" data-state="starting">
        <div
          aria-live="polite"
          className="text-size-chat text-token-text-tertiary select-none"
          role="status"
        >
          <CadencedShimmerText active>Starting your task</CadencedShimmerText>
        </div>
      </section>
    );
  }
  if (!hasWork && !working && !failed && !stopped && !blocked && !showStandaloneWorkedBoundary) return null;

  const observedStartedAtMs = positiveTimestamp(startedAtMs)
    ?? activityStartedAtMs(activities)
    ?? earliestActivityTime(activities)
    ?? Date.now();
  if (stableStartedAtRef.current === null || observedStartedAtMs < stableStartedAtRef.current) {
    stableStartedAtRef.current = observedStartedAtMs;
  }
  const workStartedAtMs = stableStartedAtRef.current;
  const completedAtMs = working
    ? Date.now()
    : finalStarted
      ? positiveTimestamp(finalStartedAtMs)
        ?? positiveTimestamp(response?.createdAtMs)
        ?? latestActivityTime(visibleActivity)
        ?? Date.now()
      : Math.max(
        latestActivityTime(visibleActivity) ?? 0,
        positiveTimestamp(response?.createdAtMs) ?? 0,
      ) || Date.now();
  const elapsed = working
    ? Math.max(0, Date.now() - workStartedAtMs)
    : Math.max(0, completedAtMs - workStartedAtMs);
  const activityContent = (
    <ActivityUnits
      activeGroupId={activeGroupId}
      renderActivity={renderActivity}
      showThinkingFallback={showThinkingFallback}
      thinkingFallbackMessage={thinkingFallbackMessage}
      toolPresentations={toolPresentations}
      units={units}
    />
  );
  const collapsibleHistory = statusBoundaryVisible && finalStarted && !failed && !stopped && hasWork;
  const contentsExpanded = !collapsibleHistory || historyFullyExpanded;
  const revealInHistory = Boolean(revealFind && collapsibleHistory && activities.some((message) => message.id === revealFind.messageId));
  const pendingFindRevealRef = useRef<number | null>(null);
  useEffect(() => {
    if (!revealInHistory || !revealFind) return;
    pendingFindRevealRef.current = revealFind.index;
    setHistoryDisclosureState("expanded");
  }, [revealFind?.index, revealFind?.messageId, revealInHistory]);
  useEffect(() => {
    const index = pendingFindRevealRef.current;
    if (index === null || !historyFullyExpanded) return;
    const frame = window.requestAnimationFrame(() => {
      pendingFindRevealRef.current = null;
      sectionRef.current?.querySelector<HTMLElement>(`[data-octopus-thread-find-index="${index}"]`)
        ?.scrollIntoView({ block: "center", behavior: reducedMotion ? "instant" : "smooth" });
    });
    return () => window.cancelAnimationFrame(frame);
  }, [historyFullyExpanded, reducedMotion, revealFind?.index]);
  return (
    <section
      ref={sectionRef}
      className="octopus-turn-activity flex min-h-0 min-w-0 flex-col text-token-text-secondary"
      data-state={blocked ? "blocked" : working ? "working" : stopped ? "stopped" : failed ? "failed" : "complete"}
      onClickCapture={(event) => {
        const target = event.target;
        const toggle = target instanceof Element ? target.closest("button[aria-expanded]") : null;
        if (!turnFinished && toggle?.getAttribute("aria-expanded") === "false") setHistoryDisclosureState("expanded");
      }}
    >
      {blocked ? <div className="composer-storage-warning" role="status">{blockingReason}</div> : null}
      {collapsibleHistory ? (
        <>
          <WorkedForToggle elapsed={elapsed} expanded={historyExpanded} onToggle={toggleHistory} />
          <div aria-hidden="true" className="octopus-worked-for-rule" />
        </>
      ) : showWorkedForBoundary ? (
        <WorkedForBoundary
          elapsed={elapsed}
          reducedMotion={reducedMotion}
          status={working ? "working" : stopped ? stoppedByOperator ? "stopped" : "interrupted" : "worked"}
        />
      ) : null}
      {!collapsibleHistory || historyDisclosureState !== "collapsed" ? (
        <motion.div
          key="activity-content"
          animate={contentsExpanded ? { height: "auto", opacity: 1 } : { height: 0, opacity: 0 }}
          aria-hidden={!contentsExpanded}
          className="octopus-turn-history"
          initial={false}
          inert={!contentsExpanded || undefined}
          onAnimationComplete={() => {
            setHistoryDisclosureState((current) => current === "closing" ? "collapsed" : current);
          }}
          style={{ overflow: collapsibleHistory ? "hidden" : "visible", pointerEvents: contentsExpanded ? "auto" : "none" }}
          transition={{ duration: reducedMotion ? 0 : 0.3, ease: [0.19, 1, 0.22, 1] }}
        >
          {showWorkedForBoundary && (hasWork || showThinkingFallback) ? <ActivityGap /> : null}
          <ShimmerActivityScope active={working}>{activityContent}</ShimmerActivityScope>
        </motion.div>
      ) : null}
      {failed && terminalTraceError ? (
        <>
          {units.length > 0 ? <ActivityGap /> : null}
          <TerminalErrorActivity message={terminalTraceError} renderActivity={renderActivity} />
        </>
      ) : null}
      {finalStarted ? <ActivityGap /> : null}
    </section>
  );
}

function ActivityUnits({ activeGroupId, renderActivity, showThinkingFallback, thinkingFallbackMessage, toolPresentations, units }: {
  activeGroupId: string | null;
  renderActivity: Props["renderActivity"];
  showThinkingFallback: boolean;
  thinkingFallbackMessage: string;
  toolPresentations: ChatToolPresentationIndex;
  units: ActivityUnit[];
}) {
  return (
    <div className="octopus-turn-activity-content">
      {units.map((unit) => {
        if (unit.kind === "group") {
          return (
            <AgentActivityGroup
              activeSlice={unit.id === activeGroupId}
              entries={unit.entries}
              key={unit.id}
              renderActivity={renderActivity}
              thinkingFallbackMessage={thinkingFallbackMessage}
              toolPresentations={toolPresentations}
            />
          );
        }
        if (unit.kind === "multi-agent-group") {
          return <MultiAgentActivityGroup entries={unit.entries} key={unit.id} />;
        }
        if (unit.kind === "subagent-group") {
          return <SubagentActivityGroup entries={unit.entries} key={unit.id} />;
        }
        return <StandaloneActivity entry={unit.entry} key={unit.id} renderActivity={renderActivity} />;
      })}
      {showThinkingFallback ? <ThinkingFallback label={thinkingFallbackMessage} /> : null}
    </div>
  );
}

function AgentActivityGroup({ activeSlice, entries, renderActivity, thinkingFallbackMessage, toolPresentations }: {
  activeSlice: boolean;
  entries: ActivityEntry[];
  renderActivity: Props["renderActivity"];
  thinkingFallbackMessage: string;
  toolPresentations: ChatToolPresentationIndex;
}) {
  const activeEntry = activeSlice
    ? [...entries].reverse().find((entry) => entry.active && entry.family !== "reasoning")
    : undefined;
  const active = activeSlice;
  const completedSummary = activeSlice ? null : completedGroupSummary(entries, toolPresentations);
  const summary = activeEntry
    ? activeActivityLabel(activeEntry, toolPresentations)
    : activeSlice
      ? thinkingFallbackMessage
      : completedSummary?.label ?? "Worked";
  const tone = groupTone(entries);
  const icon = activeSlice && !activeEntry
    ? null
    : activeEntry
      ? activeActivityIcon(activeEntry, toolPresentations)
      : completedSummary?.icon ?? null;
  const summaryKey = activeEntry
    ? `active:${activeEntry.id}`
    : activeSlice
      ? `thinking:${thinkingFallbackMessage}`
      : "summary";
  const visibleEntries = entries.filter((entry) => activityEntryRenderable(entry, toolPresentations));

  return (
    <ActivityDisclosure
      active={active}
      canExpand={visibleEntries.length > 0}
      defaultExpanded={false}
      icon={icon}
      summary={summary}
      summaryKey={summaryKey}
      summaryTransition={activeEntry ? "deferred" : "immediate"}
      shouldAnimateInitialCollapse={activeSlice}
      tone={tone}
    >
      {() => <div className="octopus-agent-activity-viewport">
        <ActivityList activeSlice={activeSlice} entries={visibleEntries} renderActivity={renderActivity} />
      </div>}
    </ActivityDisclosure>
  );
}

function MultiAgentActivityGroup({ entries }: { entries: ActivityEntry[] }) {
  const items = entries.map(multiAgentActivityItem).filter((item): item is MultiAgentActivityItem => item !== null);
  if (!items.length) return null;
  const status = multiAgentGroupStatus(items);
  const count = multiAgentCount(items);
  const summary = `${multiAgentHeaderAction(items[0].action, status)}${count > 0 ? count === 1 ? " an agent" : ` ${count} agents` : ""}`;
  const rows = multiAgentRows(items);
  return (
    <ActivityDisclosure
      active={status === "inProgress"}
      canExpand={rows.length > 0}
      defaultExpanded={false}
      icon="users"
      summary={summary}
      summaryKey={`${items[0].action}:${status}:${count}`}
      summaryTransition={status === "inProgress" ? "deferred" : "immediate"}
      tone={status === "failed" ? "danger" : "default"}
    >
      <div className="octopus-agent-activity-viewport">
        <div className="octopus-multi-agent-rows" data-testid="multi-agent-action-rows">
          {rows.map((row) => <div className="octopus-multi-agent-row" key={row.key}>{row.node}</div>)}
        </div>
      </div>
    </ActivityDisclosure>
  );
}

function SubagentActivityGroup({ entries }: { entries: ActivityEntry[] }) {
  const byAgent = new Map<string, SubagentActivityItem>();
  for (const entry of entries) {
    const item = subagentActivityItem(entry);
    if (item) byAgent.set(item.agentThreadId, item);
  }
  const items = [...byAgent.values()];
  if (!items.length) return null;
  const visible = items.slice(0, 3);
  const hidden = items.length - visible.length;
  const status = subagentGroupStatus(items);
  return (
    <div className="octopus-subagent-activity-group" data-testid="subagent-activity-inline-group">
      {visible.map((item) => (
        <span className="octopus-subagent-chip" key={item.agentThreadId}>
          <span aria-hidden="true" className="octopus-subagent-avatar"><Glyph name="user" size={13} /></span>
          <span className="octopus-subagent-chip-name">{item.displayName}</span>
        </span>
      ))}
      <span className="octopus-subagent-group-status">
        {hidden > 0 ? `and ${hidden} other ${hidden === 1 ? "subagent" : "subagents"} ` : ""}{status}
      </span>
    </div>
  );
}

function multiAgentActivityItem(entry: ActivityEntry): MultiAgentActivityItem | null {
  return normalizeMultiAgentActivity(entry.messages.at(-1));
}

function multiAgentGroupStatus(items: MultiAgentActivityItem[]): MultiAgentStatus {
  if (items.some((item) => item.status === "inProgress")) return "inProgress";
  if (items.some((item) => item.status === "failed")) return "failed";
  return "completed";
}

function multiAgentCount(items: MultiAgentActivityItem[]): number {
  const ids = new Set(items.flatMap(multiAgentIds));
  return ids.size > 0 ? ids.size : items.length;
}

function multiAgentHeaderAction(action: MultiAgentAction, status: MultiAgentStatus): string {
  switch (action) {
    case "spawnAgent":
      return status === "inProgress" ? "Creating" : status === "failed" ? "Failed to create" : "Created";
    case "sendInput":
      return status === "inProgress" ? "Messaging" : status === "failed" ? "Failed to message" : "Messaged";
    case "resumeAgent":
      return status === "inProgress" ? "Resuming" : status === "failed" ? "Failed to resume" : "Resumed";
    case "closeAgent":
      return status === "inProgress" ? "Closing" : status === "failed" ? "Failed to close" : "Closed";
  }
}

function multiAgentRowAction(action: MultiAgentAction, status: MultiAgentStatus): string {
  switch (action) {
    case "spawnAgent":
      return status === "inProgress" ? "Creating" : status === "failed" ? "Failed creating" : "Created";
    case "sendInput":
      return status === "inProgress" ? "Messaging" : status === "failed" ? "Failed messaging" : "Messaged";
    case "resumeAgent":
      return status === "inProgress" ? "Resuming" : status === "failed" ? "Failed resuming" : "Resumed";
    case "closeAgent":
      return status === "inProgress" ? "Closing" : status === "failed" ? "Failed closing" : "Closed";
  }
}

function multiAgentSendInputAction(status: MultiAgentStatus): string {
  return status === "inProgress" ? "Messaging" : status === "failed" ? "Failed to message" : "Messaged";
}

function multiAgentStateLabel(status: MultiAgentStateStatus): string {
  switch (status) {
    case "pendingInit": return "pending init";
    case "running": return "running";
    case "interrupted": return "interrupted";
    case "shutdown": return "shutdown";
    case "completed": return "completed";
    case "errored": return "errored";
    case "notFound": return "not found";
  }
}

function multiAgentStateSuffix(item: MultiAgentActivityItem, agentId: string): string {
  if (item.action === "closeAgent" || item.action === "resumeAgent") return "";
  const state = record(item.agentsStates[agentId]);
  const status = multiAgentStateStatus(state.status);
  if (!status) return "";
  const label = multiAgentStateLabel(status);
  const message = text(state.message);
  return message ? ` (${label}: ${message})` : ` (${label})`;
}

function multiAgentDisplayName(agentId: string): string {
  return `agent-${agentId.slice(0, 8)}`;
}

function multiAgentLabel(agentId: string, model: string): ReactNode {
  return <span className="octopus-multi-agent-name" title={model ? `Uses ${model}` : undefined}>{multiAgentDisplayName(agentId)}</span>;
}

function multiAgentPrompt(prompt: string): ReactNode {
  return <span className="octopus-multi-agent-prompt" title={prompt}>{prompt}</span>;
}

function multiAgentRows(items: MultiAgentActivityItem[]): Array<{ key: string; node: ReactNode }> {
  const rows: Array<{ key: string; node: ReactNode }> = [];
  for (const item of items) {
    const ids = multiAgentIds(item);
    const spawnWithInstructions = item.action === "spawnAgent" && item.status === "completed" && Boolean(item.prompt);
    const sendWithPrompt = item.action === "sendInput" && Boolean(item.prompt);
    if (!ids.length) {
      rows.push({ key: `row-generic-${item.id}`, node: multiAgentRowAction(item.action, item.status) });
    } else {
      for (const agentId of ids) {
        const agent = multiAgentLabel(agentId, item.model);
        const node = spawnWithInstructions ? (
          <span className="octopus-multi-agent-row-nowrap">Created {agent} with the instructions: {multiAgentPrompt(item.prompt)}</span>
        ) : sendWithPrompt ? (
          <span className="octopus-multi-agent-row-nowrap">{multiAgentSendInputAction(item.status)} {agent}: {multiAgentPrompt(item.prompt)}</span>
        ) : (
          <>{multiAgentRowAction(item.action, item.status)} {agent}{multiAgentStateSuffix(item, agentId)}</>
        );
        rows.push({ key: `row-${item.id}-${agentId}`, node });
      }
    }
    if (!spawnWithInstructions && !sendWithPrompt && item.prompt) {
      rows.push({
        key: `meta-prompt-${item.id}`,
        node: <span className="octopus-multi-agent-input">Input: {item.prompt}</span>,
      });
    }
  }
  return rows;
}

function subagentActivityItem(entry: ActivityEntry): SubagentActivityItem | null {
  return normalizeSubagentActivity(entry.messages.at(-1));
}

function subagentGroupStatus(items: SubagentActivityItem[]): string {
  const statuses = items.map((item) => item.displayStatus);
  if (statuses.includes("interrupted")) return "interrupted";
  if (statuses.includes("updated")) return "updated";
  return "started working";
}

function activityEntryRenderable(entry: ActivityEntry, toolPresentations: ChatToolPresentationIndex): boolean {
  if (entry.family === "reasoning") return false;
  if (!entry.active) return true;
  if (entry.family === "exec") return execCategory(entry) === "command";
  if (entry.family !== "dynamic_tool") return true;
  return dynamicToolPresentation(activityDetailMessages(entry)[0], toolPresentations).family !== "read";
}

function ActivityList({ activeSlice, entries, renderActivity }: {
  activeSlice: boolean;
  entries: ActivityEntry[];
  renderActivity: Props["renderActivity"];
}) {
  return (
    <div className="octopus-agent-activity-list">
      {entries.map((entry) => {
        const details = activityDetailMessages(entry);
        return (
          <div
            className="octopus-agent-activity-row"
            data-state={activeSlice && entry.active ? "active" : entry.tone === "danger" ? "error" : "complete"}
            key={entry.id}
          >
            {details.length > 0 ? details.map((message) => (
              <div className="octopus-agent-activity-detail-item" key={message.id}>
                {renderActivity(message)}
              </div>
            )) : <span className="octopus-agent-activity-empty-detail">{activityLabel(entry)}</span>}
          </div>
        );
      })}
    </div>
  );
}

function StandaloneActivity({ entry, renderActivity }: {
  entry: ActivityEntry;
  renderActivity: Props["renderActivity"];
}) {
  const details = activityDetailMessages(entry);
  if (entry.family === "reasoning") {
    return <ReasoningSummaryActivity entry={entry} message={details.at(-1)} />;
  }
  const retryableError = [...details].reverse().find((message) => message.eventKind === "error" && message.terminal !== true);
  if (retryableError) return <RetryableErrorActivity message={retryableError} />;
  return (
    <div
      className="octopus-agent-activity-standalone min-w-0"
      data-tone={entry.tone}
    >
      {details.map((message) => <Fragment key={message.id}>{renderActivity(message)}</Fragment>)}
    </div>
  );
}

function ReasoningSummaryActivity({ entry, message }: {
  entry: ActivityEntry;
  message?: ChatRenderMessage;
}) {
  const elapsed = useReasoningElapsed(entry);
  if (!message?.text.trim()) return null;
  const detail = reasoningDetail(message.text, entry.active);
  const lifecycle = entry.active ? "active" : "complete";
  const summary = entry.active ? "Thinking" : elapsed ? `Thought for ${formatElapsed(elapsed)}` : "Thought";
  return (
    <ActivityDisclosure
      active={entry.active}
      canExpand={Boolean(detail)}
      className="octopus-reasoning-activity"
      defaultExpanded={entry.active}
      followActive
      icon={null}
      summary={summary}
      summaryKey={`${entry.id}:${lifecycle}:${entry.active ? "" : elapsed ?? ""}`}
    >
      <div className="octopus-reasoning-detail">
        <MessageContent
          streamKey={message.id}
          streaming={entry.active}
          text={detail}
          variant="reasoning"
        />
      </div>
    </ActivityDisclosure>
  );
}

function useReasoningElapsed(entry: ActivityEntry): number | null {
  const [clock, setClock] = useState(Date.now);
  useEffect(() => {
    if (!entry.active) return;
    setClock(Date.now());
    const timer = window.setInterval(() => setClock(Date.now()), 1_000);
    return () => window.clearInterval(timer);
  }, [entry.active]);

  const timing = reasoningTiming(entry);
  const elapsed = timing.durationMs
    || (timing.startedAtMs && (entry.active ? clock : timing.completedAtMs)
      ? Math.max(0, (entry.active ? clock : timing.completedAtMs) - timing.startedAtMs)
      : 0);
  return elapsed > 0 ? elapsed : null;
}

function reasoningTiming(entry: ActivityEntry): {
  completedAtMs: number;
  durationMs: number;
  startedAtMs: number;
} {
  let completedAtMs = 0;
  let durationMs = 0;
  let startedAtMs = 0;
  for (const message of entry.messages) {
    const data = record(message.eventData);
    const identity = record(data._event);
    const messageStartedAtMs = positiveTimestampValue(
      identity.started_at_ms ?? identity.startedAtMs ?? data.started_at_ms ?? data.startedAtMs,
    );
    const messageCompletedAtMs = positiveTimestampValue(
      identity.completed_at_ms ?? identity.completedAtMs ?? data.completed_at_ms ?? data.completedAtMs,
    );
    const messageDurationMs = positiveTimestampValue(
      identity.duration_ms ?? identity.durationMs ?? data.duration_ms ?? data.durationMs,
    );
    if (messageStartedAtMs) startedAtMs = startedAtMs ? Math.min(startedAtMs, messageStartedAtMs) : messageStartedAtMs;
    if (messageCompletedAtMs) completedAtMs = Math.max(completedAtMs, messageCompletedAtMs);
    if (messageDurationMs) durationMs = Math.max(durationMs, messageDurationMs);
    if (message.createdAtMs > 0) {
      startedAtMs = startedAtMs ? Math.min(startedAtMs, message.createdAtMs) : message.createdAtMs;
      if (!entry.active) completedAtMs = Math.max(completedAtMs, message.createdAtMs);
    }
  }
  return { completedAtMs, durationMs, startedAtMs };
}

function positiveTimestampValue(value: unknown): number {
  return typeof value === "number" && Number.isFinite(value) && value > 0 ? value : 0;
}

function reasoningDetail(value: string, streaming: boolean): string {
  const content = value.trimStart();
  const strongHeading = content.match(/^\*\*([^\n]*?)\*\*(?:[ \t]*(?:\r?\n)+|[ \t]+|$)/);
  if (strongHeading?.[1]?.trim()) return content.slice(strongHeading[0].length).trim();
  if (streaming && content.startsWith("**")) return "";
  const markdownHeading = content.match(/^#{1,3}[ \t]+([^#\\*_[\]`<>&\r\n]+)(?:\r?\n|$)/);
  if (markdownHeading?.[1]?.trim()) return content.slice(markdownHeading[0].length).trim();
  return content.trim();
}

export function ActivityDisclosure({
  active,
  canExpand,
  children,
  className,
  defaultExpanded = false,
  followActive = false,
  icon,
  interactive = true,
  summary,
  summaryKey,
  summaryTransition = "static",
  shouldAnimateInitialCollapse = false,
  tone = "default",
}: {
  active: boolean;
  canExpand: boolean;
  children: ReactNode | (() => ReactNode);
  className?: string;
  defaultExpanded?: boolean;
  followActive?: boolean;
  icon: Parameters<typeof Glyph>[0]["name"] | null;
  interactive?: boolean;
  summary: string;
  summaryKey: string;
  summaryTransition?: "static" | "immediate" | "deferred";
  shouldAnimateInitialCollapse?: boolean;
  tone?: ActivityEntry["tone"];
}) {
  const reducedMotion = useShouldReduceMotion();
  const [disclosureState, setDisclosureState] = useState<DisclosureState>(() => (
    canExpand
      ? defaultExpanded
        ? "expanded"
        : shouldAnimateInitialCollapse && typeof children !== "function"
          ? "closing"
          : "collapsed"
      : "collapsed"
  ));
  const openingFrameRef = useRef<number | null>(null);
  const previouslyActiveRef = useRef(active);
  const manuallyExpandedRef = useRef<boolean | null>(null);

  useEffect(() => () => {
    if (openingFrameRef.current !== null) window.cancelAnimationFrame(openingFrameRef.current);
  }, []);

  useEffect(() => {
    if (!followActive) {
      previouslyActiveRef.current = active;
      if (!canExpand) setDisclosureState("collapsed");
      return;
    }
    const previouslyActive = previouslyActiveRef.current;
    previouslyActiveRef.current = active;
    if (canExpand && manuallyExpandedRef.current !== null) return;
    if (openingFrameRef.current !== null) {
      window.cancelAnimationFrame(openingFrameRef.current);
      openingFrameRef.current = null;
    }
    if (active && canExpand) {
      setDisclosureState((current) => current === "expanded" || current === "opening" ? current : "opening");
      openingFrameRef.current = window.requestAnimationFrame(() => {
        openingFrameRef.current = null;
        setDisclosureState((current) => current === "opening" ? "expanded" : current);
      });
      return;
    }
    if (!canExpand) {
      setDisclosureState("collapsed");
      return;
    }
    if (previouslyActive && !active) {
      setDisclosureState((current) => current === "collapsed" ? current : "closing");
    }
  }, [active, canExpand, followActive]);

  const disclosure = canExpand && interactive;
  const expanded = disclosureState === "opening" || disclosureState === "expanded";
  const fullyExpanded = disclosureState === "expanded";
  const lifecycle = active ? "active" : tone === "danger" ? "error" : "complete";
  const toggle = () => {
    if (!disclosure) return;
    manuallyExpandedRef.current = !expanded;
    if (expanded) {
      setDisclosureState("closing");
      return;
    }
    if (disclosureState === "closing") {
      setDisclosureState("expanded");
      return;
    }
    setDisclosureState("opening");
    openingFrameRef.current = window.requestAnimationFrame(() => {
      openingFrameRef.current = null;
      setDisclosureState((current) => current === "opening" ? "expanded" : current);
    });
  };
  const headerContents = (
    <>
      <span className="octopus-agent-activity-leading">
        {icon ? <Glyph aria-hidden="true" className="icon-xs shrink-0" name={icon} /> : null}
        <span className="octopus-agent-activity-summary">
          <DeferredActivitySummary
            active={active}
            summary={summary}
            summaryKey={summaryKey}
            transition={summaryTransition}
          />
        </span>
      </span>
      {disclosure ? (
        <span aria-hidden="true" className="octopus-agent-activity-chevron-wrap">
          <Glyph
            className={`octopus-agent-activity-chevron icon-2xs${expanded ? " is-expanded" : ""}`}
            name="chevronDown"
            size={12}
          />
        </span>
      ) : null}
    </>
  );
  return (
    <div className={`octopus-agent-activity min-w-0${className ? ` ${className}` : ""}`} data-state={lifecycle}>
      <div className="octopus-agent-activity-header group/activity-header">
        {disclosure ? (
          <button
            aria-expanded={expanded}
            aria-label={summary}
            className="octopus-agent-activity-toggle cursor-interaction"
            onClick={toggle}
            type="button"
          />
        ) : null}
        {headerContents}
      </div>
      {canExpand && disclosureState !== "collapsed" ? (
        <motion.div
          animate={fullyExpanded ? { height: "auto", opacity: 1 } : { height: 0, opacity: 0 }}
          aria-hidden={!fullyExpanded}
          className="octopus-agent-activity-body"
          initial={false}
          inert={!fullyExpanded || undefined}
          onAnimationComplete={() => {
            setDisclosureState((current) => current === "closing" ? "collapsed" : current);
          }}
          style={{ overflow: "hidden", pointerEvents: fullyExpanded ? "auto" : "none" }}
          transition={{ duration: reducedMotion ? 0 : 0.3, ease: [0.19, 1, 0.22, 1] }}
        >
          {typeof children === "function" ? children() : children}
        </motion.div>
      ) : null}
    </div>
  );
}

function DeferredActivitySummary({ active, summary, summaryKey, transition }: {
  active: boolean;
  summary: string;
  summaryKey: string;
  transition: "static" | "immediate" | "deferred";
}) {
  const [displayed, setDisplayed] = useState(() => ({ key: summaryKey, summary }));
  const lastUpdateRef = useRef<number | null>(null);
  const timeoutRef = useRef<number | null>(null);

  useEffect(() => {
    const now = Date.now();
    lastUpdateRef.current ??= now;
    if (summaryKey === displayed.key) return;
    const next = { key: summaryKey, summary };
    const apply = () => {
      timeoutRef.current = null;
      lastUpdateRef.current = Date.now();
      setDisplayed(next);
    };
    if (transition === "immediate") {
      if (timeoutRef.current !== null) window.clearTimeout(timeoutRef.current);
      apply();
      return;
    }
    const remaining = 1_000 - (now - lastUpdateRef.current);
    if (remaining <= 0 || transition === "static") {
      apply();
      return;
    }
    timeoutRef.current = window.setTimeout(apply, remaining);
    return () => {
      if (timeoutRef.current !== null) {
        window.clearTimeout(timeoutRef.current);
        timeoutRef.current = null;
      }
    };
  }, [displayed.key, summary, summaryKey, transition]);

  const visible = transition === "immediate" || displayed.key === summaryKey ? summary : displayed.summary;
  return (
    <CadencedShimmerText active={active} className="octopus-agent-activity-summary-value">
      {visible}
    </CadencedShimmerText>
  );
}

function ThinkingFallback({ active = true, label = "Thinking" }: { active?: boolean; label?: string }) {
  return (
    <div className="octopus-thinking-fallback text-size-chat text-token-conversation-body">
      <span aria-hidden="true" className="h-4 w-0 shrink-0" />
      <CadencedShimmerText active={active} className="octopus-thinking-fallback-label select-none">{label}</CadencedShimmerText>
    </div>
  );
}

export function TerminalErrorActivity({ message, renderActivity }: {
  message: ChatRenderMessage;
  renderActivity: Props["renderActivity"];
}) {
  return (
    <aside className="octopus-turn-error">
      <div aria-hidden="true" className="octopus-turn-error-background" />
      <div className="octopus-turn-error-icon">
        <Glyph aria-hidden="true" className="icon-sm shrink-0" name="warning" size={16} />
      </div>
      <div className="octopus-turn-error-content">
        <div className="octopus-turn-error-copy">{renderActivity(message)}</div>
      </div>
    </aside>
  );
}

function RetryableErrorActivity({ message }: { message: ChatRenderMessage }) {
  const data = record(message.eventData);
  const attempt = indexedPositiveNumber(data.attempt ?? data.retry_attempt ?? data.retryAttempt);
  const maximum = indexedPositiveNumber(data.max_attempts ?? data.maxAttempts);
  const errorInfo = data.error_info ?? data.errorInfo;
  const disconnected = record(record(errorInfo).responseStreamDisconnected ?? record(errorInfo).response_stream_disconnected);
  const overloaded = errorInfo === "serverOverloaded"
    || errorInfo === "server_overloaded"
    || disconnected.httpStatusCode === 429
    || disconnected.http_status_code === 429;
  const progress = attempt && maximum ? `${attempt}/${maximum}` : "";
  const label = overloaded
    ? `Server is busy, reconnecting${progress ? ` ${progress}` : ""}`
    : progress ? `Reconnecting ${progress}` : message.text || "Reconnecting";
  const details = text(data.additional_details ?? data.additionalDetails);
  return (
    <ActivityDisclosure
      active={false}
      canExpand={Boolean(details)}
      icon="retry"
      summary={label}
      summaryKey={`${attempt}:${maximum}:${label}`}
    >
      <div className="octopus-stream-error-details">{details}</div>
    </ActivityDisclosure>
  );
}

function WorkedForToggle({ elapsed, expanded, onToggle }: {
  elapsed: number;
  expanded: boolean;
  onToggle: () => void;
}) {
  return (
    <button
      aria-expanded={expanded}
      className="octopus-worked-for-toggle cursor-interaction"
      onClick={onToggle}
      type="button"
    >
      <span className="text-token-conversation-body">Worked for {formatElapsed(elapsed)}</span>
      <Glyph className={`octopus-worked-for-chevron icon-2xs${expanded ? " is-expanded" : ""}`} name="chevronDown" size={12} />
    </button>
  );
}

function WorkedForBoundary({ elapsed, reducedMotion, status }: {
  elapsed: number;
  reducedMotion: boolean;
  status: "working" | "worked" | "stopped" | "interrupted";
}) {
  const label = status === "working"
    ? elapsed >= 1_000 ? `Working for ${formatElapsed(elapsed)}` : "Working"
    : status === "stopped"
      ? `You stopped after ${formatElapsed(elapsed)}`
      : status === "interrupted"
        ? `Interrupted after ${formatElapsed(elapsed)}`
        : `Worked for ${formatElapsed(elapsed)}`;
  return (
    <motion.div
      animate={{ height: "auto", opacity: 1 }}
      initial={false}
      style={{ overflow: "hidden" }}
      transition={{ duration: reducedMotion ? 0 : 0.3, ease: [0.19, 1, 0.22, 1] }}
    >
      <div className="octopus-worked-for-boundary">
        <span className="text-token-conversation-body">{label}</span>
        <div aria-hidden="true" />
      </div>
    </motion.div>
  );
}

function ActivityGap() {
  return (
    <div
      aria-hidden="true"
      className="w-full"
      style={{ height: "var(--conversation-item-gap, 16px)" }}
    />
  );
}

function buildActivityUnits(
  messages: ChatRenderMessage[],
  toolPresentations: ChatToolPresentationIndex,
  preserveMovements = false,
  working = false,
): ActivityUnit[] {
  const entries = coalesceActivities(messages, preserveMovements).filter((entry) => (
    preserveMovements || conversationActivityEntryVisible(entry)
  ));
  const units: ActivityUnit[] = [];
  let grouped: ActivityEntry[] = [];
  let explorationOpen = false;
  let multiAgentGrouped: ActivityEntry[] = [];
  let subagentGrouped: ActivityEntry[] = [];

  const flush = () => {
    explorationOpen = false;
    if (!grouped.length) return;
    units.push({
      id: `activity-group:${grouped[0].id}`,
      kind: "group",
      entries: grouped,
    });
    grouped = [];
  };

  const flushMultiAgent = () => {
    if (!multiAgentGrouped.length) return;
    units.push({
      id: `multi-agent-group:${multiAgentActionForEntry(multiAgentGrouped[0])}:${multiAgentGrouped[0].id}`,
      kind: "multi-agent-group",
      entries: multiAgentGrouped,
    });
    multiAgentGrouped = [];
  };

  const flushSubagents = () => {
    if (!subagentGrouped.length) return;
    units.push({
      id: `subagent-group:${subagentGrouped[0].id}`,
      kind: "subagent-group",
      entries: subagentGrouped,
    });
    subagentGrouped = [];
  };

  for (const entry of entries) {
    if (!preserveMovements) {
      if (entry.family === "reasoning" && (entry.active || (working && entry === entries.at(-1)))) {
        if (explorationOpen) grouped.push(entry);
        continue;
      }
      const action = multiAgentActionForEntry(entry);
      if (action) {
        flush();
        flushSubagents();
        const currentAction = multiAgentActionForEntry(multiAgentGrouped[0]);
        if (multiAgentGrouped.length && currentAction !== action) flushMultiAgent();
        multiAgentGrouped.push(entry);
        continue;
      }
      if (subagentActivityItem(entry)) {
        flush();
        flushMultiAgent();
        subagentGrouped.push(entry);
        continue;
      }
      flushMultiAgent();
      flushSubagents();
    }
    if (groupableActivityEntry(entry)) {
      grouped.push(entry);
      explorationOpen = explorationActivityEntry(entry, toolPresentations);
      continue;
    }
    flush();
    units.push({ id: `activity-standalone:${entry.id}`, kind: "standalone", entry });
  }
  flush();
  flushMultiAgent();
  flushSubagents();
  return units;
}

function latestContextActivityUnits(units: ActivityUnit[]): ActivityUnit[] {
  const latestContextIndex = latestActivityUnitIndex(units, (unit) => unit.kind === "standalone");
  if (latestContextIndex >= 0) return units.slice(latestContextIndex);
  return units.length ? [units[units.length - 1]] : [];
}

function latestActivityUnitIndex(
  units: ActivityUnit[],
  predicate: (unit: ActivityUnit) => boolean,
): number {
  for (let index = units.length - 1; index >= 0; index -= 1) {
    if (units[index] && predicate(units[index])) return index;
  }
  return -1;
}

function multiAgentActionForEntry(entry: ActivityEntry | undefined): MultiAgentAction | null {
  if (!entry) return null;
  return multiAgentActivityItem(entry)?.action ?? null;
}

function conversationActivityMessages(
  messages: ChatRenderMessage[],
  toolPresentations: ChatToolPresentationIndex,
): ChatRenderMessage[] {
  return messages.flatMap((message) => {
    if (
      message.eventKind === "dynamic_tool"
      && !dynamicToolPresentation(message, toolPresentations).conversationVisible
    ) return [];
    if (message.eventKind === "collaboration") {
      const data = record(message.eventData);
      if (text(data.tool) === "wait") return [];
      if (!multiAgentAction(data.tool) || !multiAgentStatus(data.status)) return [];
      return [message];
    }
    if (message.eventKind === "subagent_activity") {
      const data = record(message.eventData);
      if (!text(data.agentThreadId ?? data.agent_thread_id) || !subagentDisplayStatus(data.kind)) return [];
      return [message];
    }
    if (activityFamily(message) !== "reasoning") return [message];
    const data = record(message.eventData);
    if (message.eventKind === "reasoning_delta") {
      const summaryIndex = indexedUnsignedNumber(data.summary_index ?? data.summaryIndex);
      const contentIndex = indexedUnsignedNumber(data.content_index ?? data.contentIndex);
      if (summaryIndex === null || contentIndex !== null || !message.text.trim()) return [];
      return [{
        ...message,
        eventData: reasoningSummaryEventData(data, summaryIndex),
      }];
    }
    const summaries = conversationReasoningTexts(data);
    if (!summaries.length) return [];
    return [{
      ...message,
      id: `${message.id}:summary`,
      text: reasoningSummaryContent(summaries),
      eventData: { ...reasoningSummaryEventData(data), summary: summaries },
      pending: false,
    }];
  });
}

function reasoningSummaryEventData(data: Record<string, unknown>, summaryIndex?: number): Record<string, unknown> {
  const safe = { ...data };
  delete safe.content;
  delete safe.content_index;
  delete safe.contentIndex;
  return {
    ...safe,
    _conversation_reasoning_summary: true,
    ...(summaryIndex === undefined ? {} : { summary_index: summaryIndex }),
  };
}

function reasoningSummaryContent(summaries: string[]): string {
  const [first, ...rest] = summaries;
  if (!first || rest.length === 0) return first ?? "";
  return [first.startsWith("**") ? first : `**${first}**`, ...rest].join("\n\n");
}

function conversationReasoningTexts(data: Record<string, unknown>): string[] {
  const summaries = reasoningTextParts(data.summary);
  return summaries.length ? summaries : reasoningTextParts(data.content);
}

function reasoningTextParts(value: unknown): string[] {
  if (!Array.isArray(value)) return [];
  return value
    .map((item) => typeof item === "string" ? item.trim() : text(record(item).text))
    .filter(Boolean);
}

function latestReasoningSummaryHeading(messages: ChatRenderMessage[]): string {
  for (let index = messages.length - 1; index >= 0; index -= 1) {
    const message = messages[index];
    if (!message || activityFamily(message) !== "reasoning") continue;
    const heading = reasoningSummaryHeading(message.text);
    if (heading) return heading;
  }
  return "";
}

function indexedUnsignedNumber(value: unknown): number | null {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0 ? value : null;
}

function activityProgressSurface(units: ActivityUnit[], working: boolean): {
  activeGroupId: string | null;
  showThinkingFallback: boolean;
} {
  if (!working) return { activeGroupId: null, showThinkingFallback: false };
  const activeUnits = units.filter((unit) => (
    (unit.kind === "standalone" ? [unit.entry] : unit.entries)
      .some((entry) => {
        const message = entry.messages.at(-1);
        const blockingRequest = entry.family === "operator"
          && ["approval", "elicitation", "user_input"].includes(message?.eventKind ?? "")
          && record(message?.eventData).resolved !== true;
        return blockingRequest || entry.active;
      })
  ));
  const activeGroup = activeUnits.findLast((unit) => unit.kind === "group");
  const lastUnit = units.at(-1);
  const progressGroup = activeGroup ?? (activeUnits.length === 0 && lastUnit?.kind === "group" ? lastUnit : null);
  return {
    activeGroupId: progressGroup?.id ?? null,
    showThinkingFallback: !progressGroup && activeUnits.length === 0,
  };
}

function coalesceActivities(messages: ChatRenderMessage[], preserveMovements = false): ActivityEntry[] {
  const entries: ActivityEntry[] = [];
  const positions = new Map<string, number>();
  const codeCells = new Map<string, string>();
  const processes = new Map<string, string>();
  for (const message of messages) {
    const family = activityFamily(message);
    const processKey = preserveMovements ? "" : commandProcessKey(message);
    const continuedCell = preserveMovements ? "" : codeModeCell(message, CODE_MODE_CONTINUATION_TOOLS);
    const signature = processes.get(processKey) ?? codeCells.get(continuedCell)
      ?? (preserveMovements ? `movement:${message.id}` : activitySignature(message));
    if (processKey) processes.set(processKey, signature);
    const runCell = preserveMovements ? "" : codeModeCell(message, CODE_MODE_RUN_TOOLS);
    if (runCell) codeCells.set(runCell, signature);
    const position = positions.get(signature);
    if (position !== undefined) {
      const entry = entries[position];
      entry.messages.push(message);
      entry.active = entryIsActive(entry);
      entry.tone = strongerActivityTone(entry.tone, activityTone([message]));
      continue;
    }
    positions.set(signature, entries.length);
    entries.push({
      id: signature || `${family}:${message.id}`,
      family,
      messages: [message],
      active: activityIsActive(message),
      tone: activityTone([message]),
    });
  }
  return entries;
}

function closeTextStreamMessage(
  message: ChatRenderMessage,
  lifecycle: "completed" | "failed",
  reasoningSummary: string[] = [],
): ChatRenderMessage {
  const eventKind = message.eventKind === "commentary_delta" ? "commentary"
    : message.eventKind === "reasoning_delta" ? "reasoning"
      : message.eventKind === "plan_delta" ? "plan"
        : null;
  if (!eventKind) return message;
  const eventData = message.eventData ?? {};
  const identity = record(eventData._event);
  return {
    ...message,
    eventKind,
    eventData: {
      ...eventData,
      ...(eventKind === "reasoning" && reasoningSummary.length ? { summary: reasoningSummary } : {}),
      lifecycle,
      _event: {
        ...identity,
        lifecycle,
      },
      transient: false,
    },
    pending: false,
  };
}

function settleOpenTextStreams(
  messages: ChatRenderMessage[],
  lifecycle: "completed" | "failed",
): ChatRenderMessage[] {
  const openSignatures = new Set(
    coalesceActivities(messages)
      .filter((entry) => entry.active && ["commentary", "reasoning", "plan"].includes(entry.family))
      .map((entry) => entry.id),
  );
  if (openSignatures.size === 0) return messages;
  const lastOpenMessage = new Map<string, number>();
  const openReasoningSummaries = new Map<string, Map<number, string>>();
  messages.forEach((message, index) => {
    const signature = activitySignature(message);
    if (!openSignatures.has(signature)) return;
    lastOpenMessage.set(signature, index);
    if (message.eventKind !== "reasoning_delta" || !message.text.trim()) return;
    const data = record(message.eventData);
    const summaryIndex = indexedUnsignedNumber(data.summary_index ?? data.summaryIndex);
    const contentIndex = indexedUnsignedNumber(data.content_index ?? data.contentIndex);
    if (summaryIndex === null || contentIndex !== null) return;
    const summaries = openReasoningSummaries.get(signature) ?? new Map<number, string>();
    summaries.set(summaryIndex, message.text);
    openReasoningSummaries.set(signature, summaries);
  });
  return messages.map((message, index) => {
    const signature = activitySignature(message);
    const reasoningSummary = [...(openReasoningSummaries.get(signature)?.entries() ?? [])]
      .sort(([left], [right]) => left - right)
      .map(([, summary]) => summary);
    return lastOpenMessage.get(signature) === index
      ? closeTextStreamMessage(message, lifecycle, reasoningSummary)
      : message;
  });
}

function activityDetailMessages(entry: ActivityEntry): ChatRenderMessage[] {
  const lastOf = (kinds: ChatRenderMessage["eventKind"][]) => [...entry.messages].reverse().find((message) => kinds.includes(message.eventKind));
  if (entry.family === "commentary") return uniqueMessages([lastOf(["commentary"]) ?? lastOf(["commentary_delta"]) ?? entry.messages.at(-1)]);
  if (entry.family === "reasoning") {
    const completed = lastOf(["reasoning"]);
    if (completed) {
      const summaries = conversationReasoningTexts(record(completed.eventData));
      if (!summaries.length) return [];
      return [{
        ...completed,
        id: `${completed.id}:summary`,
        text: reasoningSummaryContent(summaries),
        eventData: reasoningSummaryEventData(record(completed.eventData)),
      }];
    }
    const sections = new Map<number, ChatRenderMessage>();
    for (const message of entry.messages) {
      if (message.eventKind !== "reasoning_delta" || !message.text.trim()) continue;
      const data = record(message.eventData);
      const summaryIndex = indexedUnsignedNumber(data.summary_index ?? data.summaryIndex);
      const contentIndex = indexedUnsignedNumber(data.content_index ?? data.contentIndex);
      if (summaryIndex === null || contentIndex !== null) continue;
      sections.set(summaryIndex, message);
    }
    const messages = [...sections.entries()]
      .sort(([left], [right]) => left - right)
      .map(([, message]) => message);
    const latest = messages.at(-1);
    return latest ? [{
      ...latest,
      id: entry.id,
      text: reasoningSummaryContent(messages.map((message) => message.text)),
    }] : [];
  }
  if (entry.family === "plan") return uniqueMessages([lastOf(["plan"]) ?? lastOf(["plan_delta"]) ?? entry.messages.at(-1)]);
  if (entry.family === "web") return uniqueMessages([entry.messages.at(-1)]);
  if (entry.family === "exec") {
    const commandMessages = entry.messages.filter((message) => message.eventKind === "tool");
    const firstCommand = commandMessages[0];
    const lastCommand = commandMessages.at(-1);
    const commandBase = firstCommand && lastCommand ? {
      ...lastCommand,
      id: entry.id,
      eventData: {
        ...record(firstCommand.eventData),
        ...record(lastCommand.eventData),
        arguments: record(firstCommand.eventData).arguments ?? record(lastCommand.eventData).arguments,
        _lifecycle_evidence: commandMessages.map((message) => ({
          artifact_id: message.artifactId ?? "",
          label: message.label ?? "",
          movement: record(message.eventData)._movement ?? null,
        })),
      },
    } : lastCommand;
    const terminal = entry.messages.filter((message) => message.eventKind === "terminal_interaction");
    const commandData = record(commandBase?.eventData);
    const commandHasOutput = [commandData.aggregated_output, commandData.output, commandData.result, commandData.error]
      .some((value) => value !== undefined && value !== null && value !== "");
    const output = commandHasOutput ? undefined : lastOf(["tool_output_delta"]);
    const command = commandBase ? {
      ...commandBase,
      eventData: {
        ...record(commandBase.eventData),
        ...(output ? { aggregated_output: output.text } : {}),
        ...(terminal.length ? { _terminal_inputs: terminal.map((message) => message.text).filter(Boolean) } : {}),
      },
    } : undefined;
    return uniqueMessages([command, ...(command ? [] : terminal), command || terminal.length ? undefined : output ?? entry.messages.at(-1)]);
  }
  if (entry.family === "files") {
    const change = lastOf(["file_change"]) ?? lastOf(["file_change_delta"]);
    const diff = lastOf(["diff"]);
    if (change && diff) return [{
      ...change,
      eventData: {
        ...record(change.eventData),
        _projected_diff: diff.text || text(record(diff.eventData).diff),
      },
    }];
    return uniqueMessages([change ?? diff]);
  }
  if (entry.family === "mcp") {
    const first = entry.messages[0];
    const last = lastOf(["mcp_tool", "mcp_status"]) ?? entry.messages.at(-1);
    if (!first || !last) return [];
    const progress = lastOf(["mcp_progress"]);
    return [{
      ...last,
      id: entry.id,
      eventData: {
        ...record(first.eventData),
        ...record(last.eventData),
        ...(progress?.text ? { progress_message: progress.text } : {}),
        _lifecycle_evidence: entry.messages.map((message) => ({
          artifact_id: message.artifactId ?? "",
          label: message.label ?? "",
          movement: record(message.eventData)._movement ?? null,
        })),
      },
    }];
  }
  if (entry.family === "dynamic_tool") {
    const continuations = entry.messages.filter((message) => codeModeCell(message, CODE_MODE_CONTINUATION_TOOLS));
    const cellMessages = entry.messages.filter((message) => !continuations.includes(message));
    const first = cellMessages[0] ?? entry.messages[0];
    const last = cellMessages.at(-1) ?? entry.messages.at(-1);
    if (!first || !last) return [];
    return [{
      ...last,
      id: entry.id,
      eventData: {
        ...record(first.eventData),
        ...record(last.eventData),
        ...continuedCodeCell(record(last.eventData), continuations),
        ...continuedCommandProcess(entry.messages),
        arguments: record(first.eventData).arguments ?? record(last.eventData).arguments,
        _lifecycle_evidence: entry.messages.map((message) => ({
          artifact_id: message.artifactId ?? "",
          label: message.label ?? "",
          movement: record(message.eventData)._movement ?? null,
        })),
      },
    }];
  }
  return uniqueMessages(entry.messages);
}

const CODE_MODE_RUN_TOOLS = new Set(["code_mode.run"]);
const CODE_MODE_CONTINUATION_TOOLS = new Set(["code_mode.wait", "code_mode.resume"]);

function commandProcessKey(message: ChatRenderMessage): string {
  if (message.eventKind !== "dynamic_tool") return "";
  const data = record(message.eventData);
  const processId = text(commandResultBody(data).process_id) || text(record(data.arguments).process_id);
  return processId ? JSON.stringify([message.runId, message.interfaceId, record(data._activity_origin).interface_id, processId]) : "";
}

function continuedCommandProcess(messages: ChatRenderMessage[]): Record<string, unknown> {
  const settled = messages.filter((message) => text(commandResultBody(record(message.eventData)).process_id));
  if (!settled.length) return {};
  const first = record(messages[0].eventData);
  const latest = record(settled.at(-1)!.eventData);
  const body = commandResultBody(latest);
  const chunks = new Map<string, unknown>();
  for (const message of settled) {
    const output = record(commandResultBody(record(message.eventData)).output);
    for (const [index, chunk] of (Array.isArray(output.chunks) ? output.chunks : []).entries()) {
      const row = record(chunk);
      chunks.set(typeof row.source_seq === "number" ? `seq:${row.source_seq}` : `${message.id}:${index}`, chunk);
    }
  }
  return {
    tool: first.tool,
    body: { ...body, output: { ...record(body.output), chunks: [...chunks.values()] } },
    status: latest.status, ok: latest.ok, error: latest.error,
  };
}

function codeModeCell(message: ChatRenderMessage, tools: ReadonlySet<string>): string {
  if (message.eventKind !== "dynamic_tool") return "";
  const data = record(message.eventData);
  if (!tools.has(baseDynamicToolId(text(data.tool)))) return "";
  return text(commandResultBody(data).cell_id) || text(record(data.arguments).cell_id);
}

function continuedCodeCell(run: Record<string, unknown>, continuations: ChatRenderMessage[]): Record<string, unknown> {
  const settled = continuations.map((message) => record(message.eventData))
    .filter((data) => Object.keys(commandResultBody(data)).length > 0);
  const latest = settled.at(-1);
  if (!latest) return {};
  const outputs = [commandResultBody(run), ...settled.map(commandResultBody)]
    .flatMap((body) => Array.isArray(body.output) ? body.output : []);
  return {
    body: { ...commandResultBody(run), ...commandResultBody(latest), output: outputs },
    ...(latest.ok === undefined ? {} : { ok: latest.ok }),
    ...(latest.error === undefined ? {} : { error: latest.error }),
  };
}

function uniqueMessages(messages: Array<ChatRenderMessage | undefined>): ChatRenderMessage[] {
  const seen = new Set<string>();
  return messages.flatMap((message) => {
    if (!message || seen.has(message.id)) return [];
    seen.add(message.id);
    return [message];
  });
}

function completedGroupSummary(entries: ActivityEntry[], toolPresentations: ChatToolPresentationIndex): {
  icon: ActivityGlyph | null;
  label: string;
} {
  const logical = new Map<ActivityFamily, ActivityEntry[]>();
  for (const entry of entries) logical.set(entry.family, [...(logical.get(entry.family) ?? []), entry]);
  const segments: CompletedGroupSegment[] = [];
  const files = logical.get("files") ?? [];
  const exec = logical.get("exec") ?? [];
  const dynamicTools = logical.get("dynamic_tool") ?? [];
  const web = logical.get("web") ?? [];
  const dynamicPresentations = dynamicTools.map((entry) => dynamicToolPresentation(activityDetailMessages(entry)[0], toolPresentations));
  const dynamicFamilies = dynamicPresentations.map((presentation) => presentation.family);
  const changedPaths = new Set<string>();
  let anonymousFileChanges = 0;
  for (const entry of files) {
    const message = entry.messages.at(-1);
    const data = record(message?.eventData);
    const changes = Array.isArray(data.changes) ? data.changes.map(record) : [];
    if (!changes.length) {
      const path = text(data.path) || text(data.file);
      if (path) changedPaths.add(path);
      else anonymousFileChanges += 1;
      continue;
    }
    for (const change of changes) {
      const path = text(change.path) || text(change.file);
      if (path) changedPaths.add(path);
      else anonymousFileChanges += 1;
    }
  }
  dynamicTools.forEach((entry, index) => {
    if (dynamicFamilies[index] !== "files") return;
    const target = dynamicToolPresentation(activityDetailMessages(entry)[0], toolPresentations).target;
    if (target) changedPaths.add(target);
    else anonymousFileChanges += 1;
  });
  const allMcp = [
    ...(logical.get("mcp") ?? []),
    ...(logical.get("hooks") ?? []),
  ];
  const mcpCommands = allMcp.filter((entry) => (
    text(record(entry.messages.at(-1)?.eventData).server) === "node_repl"
  ));
  const mcp = allMcp.filter((entry) => !mcpCommands.includes(entry));
  const genericDynamicTools = dynamicTools.filter((_, index) => dynamicFamilies[index] === "tool");
  const nativeExploration = exec.reduce((count, entry) => (
    count + (execCategory(entry) === "read" ? 1 : 0)
  ), 0);
  const exploration = nativeExploration + dynamicPresentations.filter((presentation) => presentation.family === "read" && presentation.targetKind === "file").length;
  const artifactExploration = dynamicPresentations.filter((presentation) => presentation.family === "read" && presentation.targetKind === "artifact");
  const searches = exec.reduce((count, entry) => count + (execCategory(entry) === "search" ? 1 : 0), 0);
  const listings = exec.reduce((count, entry) => count + (execCategory(entry) === "list" ? 1 : 0), 0);
  const nativeCommands = Math.max(0, exec.length - nativeExploration - searches - listings);
  const commands = nativeCommands
    + mcpCommands.length
    + dynamicFamilies.filter((family) => family === "exec").length;

  const namedMcp = mcp.map((entry) => {
    const data = record(entry.messages.at(-1)?.eventData);
    return { entry, name: text(data.server) || text(data.name) || text(data.tool) };
  });
  const integrationNames = [...new Set(namedMcp.map(({ name }) => name).filter(Boolean))];
  const unnamedMcpCalls = namedMcp.filter(({ name }) => !name).length;
  if (integrationNames.length) {
    const names = new Intl.ListFormat(undefined, { style: "long", type: "conjunction" }).format(integrationNames);
    segments.push({
      icon: "plugins",
      leading: `Used ${names} ${integrationNames.length === 1 ? "integration" : "integrations"}`,
      following: `used ${names} ${integrationNames.length === 1 ? "integration" : "integrations"}`,
    });
  }
  pushPluralSegment(segments, unnamedMcpCalls, "Called a tool", "Called tools", "plugins");
  const fileChangeCount = changedPaths.size + anonymousFileChanges;
  pushPluralSegment(segments, fileChangeCount, "Edited a file", "Edited files", "edit");
  if (exploration || searches || listings) {
    segments.push({
      icon: explorationGroupIcon(entries, toolPresentations),
      leading: "Read files",
      following: "read files",
    });
  }
  for (const kind of ["read", "list", "search"] as const) {
    if (!artifactExploration.some((presentation) => presentation.kind === kind)) continue;
    const label = kind === "read" ? "Read artifacts" : kind === "list" ? "Listed artifacts" : "Searched artifacts";
    segments.push({ icon: kind === "list" ? "folder" : kind === "search" ? "search" : "read", leading: label, following: label[0].toLowerCase() + label.slice(1) });
  }
  pushPluralSegment(segments, commands, "Ran a command", "Ran commands", "terminal");
  if (web.length || dynamicFamilies.includes("web")) segments.push({ icon: "search", leading: "Searched the web", following: "searched the web" });
  const genericDynamicLabels = new Map(genericDynamicTools.map((entry) => {
    const presentation = dynamicToolPresentation(activityDetailMessages(entry)[0], toolPresentations);
    return [dynamicToolSummary(presentation), dynamicToolGlyph(presentation)] as const;
  }));
  for (const [label, icon] of genericDynamicLabels) {
    segments.push({
      icon,
      leading: label || "Used a tool",
      following: label ? label[0].toLocaleLowerCase() + label.slice(1) : "used a tool",
    });
  }
  if (!segments.length) return { icon: null, label: "Worked" };
  return {
    icon: segments[0].icon,
    label: new Intl.ListFormat(undefined, { style: "long", type: "unit" }).format(
      segments.map((segment, index) => index === 0 ? segment.leading : segment.following),
    ),
  };
}

function pushPluralSegment(
  segments: CompletedGroupSegment[],
  count: number,
  singular: string,
  plural: string,
  icon: ActivityGlyph,
) {
  if (count <= 0) return;
  const leading = count === 1 ? singular : plural;
  segments.push({ icon, leading, following: leading[0].toLocaleLowerCase() + leading.slice(1) });
}

function activeActivityLabel(entry: ActivityEntry, toolPresentations: ChatToolPresentationIndex): string {
  const message = entry.messages.at(-1);
  if (!message) return "Thinking";
  const data = record(message.eventData);
  if (entry.family === "commentary") return "Explaining progress";
  if (entry.family === "reasoning") return "Thinking";
  if (entry.family === "plan") return "Updating plan";
  if (entry.family === "exec") {
    const category = execCategory(entry);
    const parsed = record(data.parsed_cmd ?? data.parsedCmd);
    const target = text(parsed.path) || text(parsed.name) || text(parsed.file) || text(parsed.target);
    const query = text(parsed.query) || text(parsed.pattern);
    const targetLabel = activityPathBasename(target);
    if (category === "read") {
      return targetLabel ? `Reading ${targetLabel}` : "Reading files";
    }
    if (category === "search") {
      if (targetLabel) return `Searching files in ${targetLabel} folder`;
      return query ? `Searching for ${query}` : "Searching files";
    }
    if (category === "list") return targetLabel ? `Listing files in ${targetLabel} folder` : "Listing files";
    const command = commandText(entry).trim();
    return command ? `Running ${command}` : "Running command";
  }
  if (entry.family === "files") return "Editing files";
  if (entry.family === "web") {
    return webSearchActivityLabel(webSearchActivityDetail(message), true);
  }
  if (entry.family === "dynamic_tool") {
    const presentation = dynamicToolPresentation(activityDetailMessages(entry)[0], toolPresentations);
    if (presentation.family === "web") return webSearchActivityLabel(presentation.query, true);
    if (presentation.family === "read") {
      if (presentation.targetKind === "artifact") return dynamicToolReadLabel(presentation, true);
      if (presentation.kind === "read") {
        return presentation.pathLabel ? `Reading ${presentation.pathLabel}` : "Reading files";
      }
      if (presentation.kind === "search") {
        if (presentation.pathLabel) return `Searching files in ${presentation.pathLabel} folder`;
        return presentation.query ? `Searching for ${presentation.query}` : "Searching files";
      }
      return presentation.pathLabel ? `Listing files in ${presentation.pathLabel} folder` : "Listing files";
    }
    if (presentation.family === "files") return presentation.target ? `Editing ${presentation.target}` : "Editing files";
    if (presentation.family === "exec") return presentation.target ? `Running ${presentation.target}` : "Running command";
    const target = dynamicToolActivityLabel(entry, toolPresentations);
    return target || "Using a tool";
  }
  if (entry.family === "mcp" || entry.family === "hooks") {
    const target = [text(data.server), text(data.tool) || text(data.name)].filter(Boolean).join(" · ");
    return target ? `Calling ${target}` : "Calling tool";
  }
  if (entry.family === "collaboration") return "Coordinating agents";
  if (entry.family === "images") return message.eventKind === "image_generation" ? "Generating image" : "Viewing image";
  if (entry.family === "review") return "Reviewing changes";
  if (entry.family === "sleep") return "Waiting";
  return activityLabel(entry);
}

function activityPathBasename(value: string): string {
  const normalized = value.trim().replaceAll("\\", "/").replace(/\/+$/, "");
  return normalized.split("/").filter(Boolean).at(-1) ?? normalized;
}

function activeActivityIcon(
  entry: ActivityEntry,
  toolPresentations: ChatToolPresentationIndex,
): ActivityGlyph | null {
  if (entry.family === "exec") {
    const category = execCategory(entry);
    if (category === "search") return "search";
    if (category === "read") return "read";
    if (category === "list") return "folder";
    return "terminal";
  }
  if (entry.family === "files" || entry.family === "review") return "edit";
  if (entry.family === "web") return "search";
  if (entry.family === "mcp" || entry.family === "hooks") return "plugins";
  if (entry.family === "dynamic_tool") {
    const presentation = dynamicToolPresentation(activityDetailMessages(entry)[0], toolPresentations);
    return dynamicToolGlyph(presentation);
  }
  if (entry.family === "sleep") return "scheduled";
  return null;
}

function explorationGroupIcon(entries: ActivityEntry[], toolPresentations: ChatToolPresentationIndex): ActivityGlyph {
  const representative = entries.find((entry) => explorationActivityEntry(entry, toolPresentations));
  return representative ? activeActivityIcon(representative, toolPresentations) ?? "read" : "read";
}

function explorationActivityEntry(
  entry: ActivityEntry,
  toolPresentations: ChatToolPresentationIndex,
): boolean {
  if (entry.family === "exec") return execCategory(entry) !== "command";
  if (entry.family !== "dynamic_tool") return false;
  return dynamicToolPresentation(activityDetailMessages(entry)[0], toolPresentations).family === "read";
}

function activityLabel(entry: ActivityEntry): string {
  const message = entry.messages.at(-1);
  if (!message) return "Activity";
  const data = record(message.eventData);
  if (entry.family === "compaction") return describeNotice(message) || "Context automatically compacted";
  if (entry.family === "model") {
    const from = text(data.fromModel) || text(data.from_model);
    const to = text(data.toModel) || text(data.to_model);
    return message.eventKind === "model_rerouted" && (from || to)
      ? `Model rerouted · ${[from, to].filter(Boolean).join(" → ")}`
      : message.eventKind === "model_rerouted" ? "Model rerouted" : "Model route verified";
  }
  if (entry.family === "warning") return text(data.message) || message.text || "Warning";
  if (entry.family === "rate_limits") return "Rate limits updated";
  if (entry.family === "goal") return `Goal · ${text(data.status) || "updated"}`;
  if (entry.family === "operator") {
    if (message.eventKind === "approval") return "Approval requested";
    if (message.eventKind === "operator_request_resolved") return "Operator request resolved";
    return "Operator input requested";
  }
  if (entry.family === "turn_control") {
    const action = text(data.action) || "control";
    const status = text(data.status) || "projected";
    if (action === "steer") return status === "acknowledged" ? "Steered turn" : "Steer failed";
    if (action === "stop" || action === "interrupt") return status === "acknowledged" ? "Stopped turn" : "Stop failed";
    return `${action} · ${status}`;
  }
  return message.label || message.text || "Activity";
}

function dynamicToolActivityLabel(entry: ActivityEntry, toolPresentations: ChatToolPresentationIndex): string {
  const presentation = dynamicToolPresentation(activityDetailMessages(entry)[0], toolPresentations);
  return dynamicToolSummary(presentation);
}

function activitySignature(message: ChatRenderMessage): string {
  const data = record(message.eventData);
  const identity = record(data._event);
  const movement = record(data._movement);
  const family = activityFamily(message);
  const reviewId = text(data.review_id) || text(data.reviewId);
  if (family === "review" && reviewId) return `${family}:review:${reviewId}`;
  const callId = text(data.call_id) || text(data.callId);
  if (callId) return `${family}:call:${callId}`;
  const itemId = text(data.item_id) || text(data.itemId) || text(identity.item_id);
  if (itemId) {
    const turnId = text(data.turn_id) || text(data.turnId) || text(identity.turn_id);
    return `${family}:item:${turnId}:${itemId}`;
  }
  const movementSeq = typeof movement.seq === "number" && Number.isSafeInteger(movement.seq)
    ? String(movement.seq)
    : "";
  return `${family}:movement:${movementSeq || message.id}`;
}

function systemActivityMessage(message: ChatRenderMessage): boolean {
  if (message.eventKind !== "dynamic_tool") return false;
  const origin = record(record(message.eventData)._activity_origin);
  const role = text(origin.interface_role).toLocaleLowerCase();
  return role !== "provider";
}

function activityIsActive(message: ChatRenderMessage): boolean {
  if (message.eventKind === "tool" || (message.eventKind === "dynamic_tool" && commandHasProcessEvidence(record(message.eventData)))) {
    const model = commandActivityModel(message);
    return model.phase === "running" && !(model.yielded && model.processId);
  }
  if (message.pending) return true;
  const data = record(message.eventData);
  const status = (
    text(data.status)
    || text(data.state)
    || text(data.lifecycle)
    || (activityFamily(message) === "review" ? text(record(data.review).status) : "")
    || text(record(data._event).lifecycle)
  ).toLocaleLowerCase();
  if (["acknowledged", "completed", "complete", "done", "failed", "declined", "cancelled", "canceled", "success", "succeeded", "exited"].includes(status)) return false;
  if (["started", "starting", "running", "working", "in_progress", "inprogress", "pending", "queued", "streaming", "progress"].includes(status)) return true;
  return ["commentary_delta", "reasoning_delta", "plan_delta", "tool_output_delta", "file_change_delta", "mcp_progress", "auto_review_started", "model_verification", "auth_recovery_started", "sleep"].includes(message.eventKind ?? "");
}

function entryIsActive(entry: ActivityEntry): boolean {
  if (entry.family === "exec") {
    for (let index = entry.messages.length - 1; index >= 0; index -= 1) {
      if (entry.messages[index].eventKind === "tool") return activityIsActive(entry.messages[index]);
    }
  }
  const latest = entry.messages.at(-1);
  if (!latest) return false;
  if (["tool", "file_change", "reasoning", "plan", "mcp_tool", "mcp_status", "auto_review_completed", "review_exited"].includes(latest.eventKind ?? "")) {
    const data = record(latest.eventData);
    const status = (text(data.status) || text(data.state) || text(data.lifecycle) || text(record(data._event).lifecycle)).toLocaleLowerCase();
    return status ? activityIsActive(latest) : false;
  }
  return activityIsActive(latest);
}

function activityTone(messages: ChatRenderMessage[]): ActivityEntry["tone"] {
  if (messages.some((message) => {
    if (message.eventKind !== "tool" && !(message.eventKind === "dynamic_tool" && commandHasProcessEvidence(record(message.eventData)))) return false;
    const model = commandActivityModel(message);
    return ["failed", "timed_out"].includes(model.phase) || (model.exitCode !== null && model.exitCode !== 0);
  })) return "danger";
  if (messages.some((message) => message.error || message.eventKind === "error")) return "danger";
  if (messages.some((message) => {
    const data = record(message.eventData);
    const status = (
      text(data.status)
      || text(data.lifecycle)
      || (activityFamily(message) === "review" ? text(record(data.review).status) : "")
      || text(record(data._event).lifecycle)
    ).toLocaleLowerCase();
    return status.includes("fail") || status === "error";
  })) return "danger";
  if (messages.some((message) => message.eventKind === "warning" || message.eventKind === "rate_limits")) return "warning";
  if (messages.some((message) => {
    const data = record(message.eventData);
    const status = (
      text(data.status)
      || text(data.lifecycle)
      || (activityFamily(message) === "review" ? text(record(data.review).status) : "")
      || text(record(data._event).lifecycle)
    ).toLocaleLowerCase();
    return status.includes("warn") || status.includes("refus") || status.includes("declin") || status.includes("reject");
  })) return "warning";
  return "default";
}

function strongerActivityTone(
  current: ActivityEntry["tone"],
  next: ActivityEntry["tone"],
): ActivityEntry["tone"] {
  if (current === "danger" || next === "danger") return "danger";
  if (current === "warning" || next === "warning") return "warning";
  return "default";
}

function groupTone(entries: ActivityEntry[]): ActivityEntry["tone"] {
  if (entries.some((entry) => entry.tone === "danger")) return "danger";
  if (entries.some((entry) => entry.tone === "warning")) return "warning";
  return "default";
}

function groupableActivityEntry(entry: ActivityEntry): boolean {
  const latest = entry.messages.at(-1);
  const data = record(latest?.eventData);
  if ((entry.family === "mcp" || entry.family === "dynamic_tool") && (
    data.standalone_in_conversation === true
    || data.standaloneInConversation === true
    || text(data.mcp_app_resource_uri)
    || text(data.mcpAppResourceUri)
  )) return false;
  if (entry.family === "review") return reviewStatus(entry) === "inprogress";
  return GROUPABLE_FAMILIES.has(entry.family);
}

function conversationActivityEntryVisible(entry: ActivityEntry): boolean {
  if (entry.family === "commentary") return Boolean(activityDetailMessages(entry)[0]?.text.trim());
  return entry.family !== "review" || reviewStatus(entry) !== "approved";
}

function reviewStatus(entry: ActivityEntry): string {
  const data = record(entry.messages.at(-1)?.eventData);
  return (text(data.status) || text(record(data.review).status)).toLocaleLowerCase().replace(/[_-]/g, "");
}

function activityFamily(message: ChatRenderMessage): ActivityFamily {
  switch (message.eventKind) {
    case "commentary":
    case "commentary_delta":
      return "commentary";
    case "reasoning":
    case "reasoning_delta":
      return "reasoning";
    case "plan":
    case "plan_delta":
      return "plan";
    case "tool":
    case "tool_output_delta":
    case "terminal_interaction":
      return "exec";
    case "file_change":
    case "file_change_delta":
    case "diff":
      return "files";
    case "web_search":
      return "web";
    case "hook":
    case "hook_prompt":
      return "hooks";
    case "mcp_tool":
    case "mcp_progress":
    case "mcp_status":
      return "mcp";
    case "dynamic_tool":
      return "dynamic_tool";
    case "collaboration":
    case "subagent_activity":
      return "collaboration";
    case "image_view":
    case "image_generation":
      return "images";
    case "sleep":
      return "sleep";
    case "review_entered":
    case "review_exited":
    case "auto_review_started":
    case "auto_review_completed":
      return "review";
    case "compaction":
      return "compaction";
    case "model_rerouted":
    case "model_verification":
    case "auth_recovery_started":
    case "auth_recovery_completed":
      return "model";
    case "warning":
    case "error":
      return "warning";
    case "rate_limits":
      return "rate_limits";
    case "goal":
      return "goal";
    case "approval":
    case "elicitation":
    case "user_input":
    case "operator_request_resolved":
      return "operator";
    case "turn_control":
      return "turn_control";
    default:
      return "other";
  }
}

function execCategory(entry: ActivityEntry): "read" | "search" | "list" | "command" {
  const message = entry.messages.at(-1);
  const data = record(message?.eventData);
  const parsed = record(data.parsed_cmd ?? data.parsedCmd);
  const declared = (text(parsed.type) || text(data.command_type) || text(data.commandType) || text(data.action)).toLocaleLowerCase();
  if (["read", "read_file", "read-file"].includes(declared)) return "read";
  if (["search", "find", "grep"].includes(declared)) return "search";
  if (["list", "list_files", "list-files"].includes(declared)) return "list";
  return "command";
}

function commandText(entry: ActivityEntry): string {
  for (const message of [...entry.messages].reverse()) {
    const data = record(message.eventData);
    const parsed = record(data.parsed_cmd ?? data.parsedCmd);
    const argumentsValue = record(data.arguments);
    const argumentCommand = text(argumentsValue.command)
      || text(argumentsValue.cmd)
      || (Array.isArray(argumentsValue.argv) ? argumentsValue.argv.map(text).filter(Boolean).join(" ") : "");
    const command = text(parsed.cmd) || argumentCommand || text(data.command) || (message.eventKind === "tool" ? message.text.split("\n")[0]?.trim() ?? "" : "");
    if (command) return command;
  }
  return "";
}

function latestActivityTime(messages: ChatRenderMessage[]): number | null {
  let latest = 0;
  for (const message of messages) latest = Math.max(latest, message.createdAtMs);
  return latest || null;
}

function formatElapsed(value: number): string {
  const seconds = Math.max(0, Math.floor(value / 1_000));
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  const remainder = seconds % 60;
  if (minutes < 60) return remainder ? `${minutes}m ${remainder}s` : `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  const minuteRemainder = minutes % 60;
  return minuteRemainder ? `${hours}h ${minuteRemainder}m` : `${hours}h`;
}

function describeNotice(message: ChatRenderMessage): string {
  const data = record(message.eventData);
  const descriptor = [message.label, text(data.type), text(data.event), text(data.action), text(data.status)].join(" ").toLocaleLowerCase();
  if (descriptor.includes("compact")) {
    const source = `${text(data.source)} ${text(data.mode)}`.toLocaleLowerCase();
    return source.includes("manual") ? "Context compacted" : "Context automatically compacted";
  }
  if (data.will_retry === true || descriptor.includes("retry")) return activityIsActive(message) ? "Retrying" : "Retried";
  if (descriptor.includes("delivery") || descriptor.includes("refused") || data.verdict !== undefined) {
    const verdict = text(data.verdict) || text(data.status);
    return verdict ? `Artifact delivery · ${verdict}` : "Artifact delivery updated";
  }
  return message.text;
}

function chatActivityVisible(message: ChatRenderMessage, toolPresentations: ChatToolPresentationIndex): boolean {
  return !systemActivityMessage(message)
    && conversationTraceAdmission(message, toolPresentations) === "visible";
}

function isSafetyBufferingWarning(message: ChatRenderMessage): boolean {
  return message.eventKind === "warning"
    && text(record(message.eventData).source) === "model_safety_buffering";
}

function safetyBufferingVisible(message: ChatRenderMessage): boolean {
  return !isSafetyBufferingWarning(message)
    || record(message.eventData).showBufferingUi === true;
}

function turnWasStopped(message: ChatRenderMessage): boolean {
  if (message.eventKind === "interrupted_result") return true;
  return turnStoppedByOperator(message);
}

function turnStoppedByOperator(message: ChatRenderMessage): boolean {
  if (message.eventKind !== "turn_control") return false;
  const data = record(message.eventData);
  const action = text(data.action);
  return (action === "stop" || action === "interrupt") && text(data.status) === "acknowledged";
}

function turnWasCompleted(message: ChatRenderMessage): boolean {
  if (message.eventKind === "turn_completed") return true;
  return message.label?.trim().toLocaleLowerCase() === "turn.completed";
}

function activityStartedAtMs(messages: ChatRenderMessage[]): number | null {
  for (const message of messages) {
    if (message.eventKind !== "turn_started" && message.label?.trim().toLocaleLowerCase() !== "turn.started") continue;
    if (message.createdAtMs > 0) return message.createdAtMs;
  }
  return null;
}

function earliestActivityTime(messages: ChatRenderMessage[]): number | null {
  let earliest = Number.POSITIVE_INFINITY;
  for (const message of messages) {
    if (message.createdAtMs > 0) earliest = Math.min(earliest, message.createdAtMs);
  }
  return Number.isFinite(earliest) ? earliest : null;
}

function positiveTimestamp(value: number | undefined): number | null {
  return typeof value === "number" && Number.isFinite(value) && value > 0 ? value : null;
}

function indexedPositiveNumber(value: unknown): number {
  return typeof value === "number" && Number.isSafeInteger(value) && value > 0 ? value : 0;
}

function record(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : {};
}

function text(value: unknown): string {
  return typeof value === "string" ? value.trim() : "";
}

function stringArray(value: unknown): string[] {
  return Array.isArray(value) ? value.map(text).filter(Boolean) : [];
}
