import { useRef, useState } from "react";
import { OcButton, OcMenuItem } from "../../design-system/components";
import { OcDropdown } from "../../design-system/OcDropdown";
import { Glyph } from "../IconGlyphs";
import { describeInactivityState, describeSchedule, formatEvidenceTime, formatSchedule, nextScheduledRunAtMs, resolvedTimezone } from "./scheduleModel";
import { scheduledRowStatus } from "./ScheduledRows";
import type { ScheduledInterfaceProjection, ScheduledScopeOption } from "./types";

export function ScheduledDetail({ task, pending, canEdit, scopeOptions, onDelete, onEdit, onToggle, onSelectChat }: {
  task: ScheduledInterfaceProjection;
  pending: boolean;
  canEdit: boolean;
  scopeOptions: ScheduledScopeOption[];
  onDelete: () => void;
  onEdit: () => void;
  onToggle: () => void;
  onSelectChat?: (runId: string) => void;
}) {
  const status = scheduledRowStatus(task);
  if (task.eventSource) return (
    <section aria-label={task.displayName} className="flex h-full min-h-0 flex-col overflow-y-auto" data-scheduled-detail="">
      <div className="mx-auto flex w-full max-w-(--thread-content-max-width) flex-col gap-4 px-toolbar pt-panel pb-8">
        <h2 className="m-0 text-xl font-semibold">{task.displayName}</h2>
        <p className="m-0 text-secondary">{task.status === "fired" ? "Fired — this trigger is spent." : task.status === "disarmed" ? "Disarmed — not watching for new events." : task.status === "prepared" ? "Event prepared — awaiting admission." : task.status === "invalid" ? task.error : task.status === "armed" ? "Armed — waiting for the condition." : "Runs when this source delivers an event."}</p>
        {task.eventSource.condition ? <p className="m-0 text-secondary select-text">{task.eventSource.condition}</p> : null}
        {task.eventSource.mailboxWatch ? <MailboxObservations watch={task.eventSource.mailboxWatch} /> : null}
        {task.eventSource.sensorHealth ? <SensorHealth health={task.eventSource.sensorHealth} /> : null}
        {task.eventSource.sources.length ? <p className="m-0 break-words text-sm text-secondary select-text">{task.eventSource.sources.join(", ")}</p> : null}
        <div className="flex flex-col gap-3 rounded-2xl border border-default p-4">
          {task.eventSource.targets.map((target) => (
            <div className="flex min-w-0 items-center gap-3" key={target.runId}>
              <span className="min-w-0 flex-1 truncate">{target.title}</span>
              {onSelectChat && target.chatInterfaceId ? <OcButton onClick={() => onSelectChat(target.runId)} size="sm" variant="secondary">Open task</OcButton> : null}
            </div>
          ))}
        </div>
        <p className="m-0 text-sm text-tertiary">{task.lastRunAtMs ? `Last event received ${formatEvidenceTime(task.lastRunAtMs)}.` : "No events received yet."}</p>
      </div>
    </section>
  );
  return (
    <section aria-label={task.displayName} className="flex h-full min-h-0 flex-col" data-scheduled-detail="">
      <div className="min-h-0 flex-1 overflow-y-auto [scrollbar-gutter:stable]">
        <div className="relative mx-auto flex w-full max-w-(--thread-content-max-width) flex-1 shrink-0 flex-col px-toolbar pt-panel pb-8">
          <div className="relative flex flex-col">
            <div className="flex flex-col gap-0">
              <div className="group flex w-full flex-col items-end justify-end gap-2 pb-6">
                <div className="bg-token-foreground/5 max-w-[77%] min-w-0 overflow-hidden break-words rounded-2xl px-3 py-2 text-left" data-user-message-bubble="true">
                  <div className="flex flex-col items-end gap-1">
                    <div className="text-size-chat relative w-full min-w-0">
                      <div className="text-size-chat whitespace-pre-wrap select-text">{task.instructions || "No instructions. This trigger may use its configured environment override."}</div>
                    </div>
                  </div>
                </div>
              </div>
              <div className="group flex flex-col gap-3">
                <p className="m-0 text-size-chat text-default select-text" data-scheduled-confirmation="">{confirmation(task, status, scopeOptions)}</p>
                <SummaryCard canEdit={canEdit} onDelete={onDelete} onEdit={onEdit} onToggle={onToggle} pending={pending} status={status} task={task} />
              </div>
            </div>
          </div>
        </div>
      </div>
    </section>
  );
}

function MailboxObservations({ watch }: { watch: NonNullable<NonNullable<ScheduledInterfaceProjection["eventSource"]>["mailboxWatch"]> }) {
  return (
    <section aria-label="Mailbox polling observations" className="flex flex-col gap-3 rounded-2xl border border-default p-4">
      <h3 className="m-0 text-sm font-semibold">Mailbox polling</h3>
      <p className="m-0 text-sm text-secondary">Recorded observations, not a connection-health check.</p>
      {watch.mailboxes.map(observation => (
        <div className="flex flex-col gap-1 text-sm select-text" key={observation.mailbox}>
          <strong className="break-words">{observation.mailbox}</strong>
          <p className="m-0 text-secondary">Last successful check: {observation.checkedAt
            ? <time dateTime={observation.checkedAt} title={observation.checkedAt}>{formatEvidenceTime(Date.parse(observation.checkedAt))}</time>
            : "No successful check recorded"}</p>
          <p className="m-0 text-secondary">Last processed UID: {observation.lastUid ?? "Not recorded"}</p>
          <p className="m-0 text-secondary">Server UIDNEXT: {observation.uidNext ?? "Not reported"}</p>
        </div>
      ))}
      <p className="m-0 text-sm text-secondary">Retained admitted messages: {watch.admittedCount ?? "Not recorded"}</p>
    </section>
  );
}

function SensorHealth({ health }: { health: NonNullable<NonNullable<ScheduledInterfaceProjection["eventSource"]>["sensorHealth"]> }) {
  return (
    <section aria-label="Sensor health" className="flex flex-col gap-3 rounded-2xl border border-default p-4" data-sensor-attention={health.attention ? "true" : "false"}>
      <h3 className="m-0 text-sm font-semibold">{health.attention ? "Sensor needs attention" : "Sensor health"}</h3>
      <p className="m-0 text-sm text-secondary select-text">{health.summary}</p>
      <p className="m-0 text-sm text-secondary">Recorded by the sensor in its own state, not a live probe.{health.configurationSource === "private_state"
        ? " Configured in the sensor's private state." : health.configurationSource === "registry" ? " Configured in the sensor's registry folder." : ""}</p>
      {health.facts.map(fact => (
        <p className="m-0 text-sm text-secondary select-text" key={fact.label}>{fact.label}: {fact.at
          ? <><time dateTime={fact.at} title={fact.at}>{formatEvidenceTime(Date.parse(fact.at))}</time>{fact.value ? ` (${fact.value})` : ""}</>
          : fact.value || "Not recorded"}</p>
      ))}
    </section>
  );
}

function SummaryCard({ canEdit, onDelete, onEdit, onToggle, pending, status, task }: {
  canEdit: boolean;
  onDelete: () => void;
  onEdit: () => void;
  onToggle: () => void;
  pending: boolean;
  status: ReturnType<typeof scheduledRowStatus>;
  task: ScheduledInterfaceProjection;
}) {
  const [menuOpen, setMenuOpen] = useState(false);
  const menuRef = useRef<HTMLButtonElement>(null);
  const editable = canEdit && task.valid && !pending;
  const subtitle = status === "paused" ? "Paused" : status === "invalid" ? "Needs attention" : formatSchedule(task);
  return (
    <span className="mt-3 flex w-full max-w-[536px] flex-col self-start overflow-hidden rounded-2xl border border-token-border-default bg-token-main-surface-primary shadow-[0_2px_6px_-1px_rgba(0,0,0,0.03)] select-none" data-automation-card="">
      <span className="group/resource-card relative flex min-h-16 w-full min-w-0 items-center gap-4 p-4">
        <button
          aria-describedby={`${task.interfaceId}-summary`}
          aria-label={`Edit ${task.displayName}`}
          className="peer/resource-card absolute inset-0 cursor-interaction border-0 bg-transparent group-hover/resource-card:bg-token-list-hover-background/30 focus-visible:ring-1 focus-visible:ring-token-focus-border focus-visible:outline-none focus-visible:ring-inset disabled:cursor-default"
          disabled={!editable}
          onClick={onEdit}
          type="button"
        />
        <span className="pointer-events-none relative z-10 flex size-10 shrink-0 items-center justify-center rounded-lg bg-token-bg-secondary text-token-text-secondary">
          <Glyph name="scheduled" size={20} />
        </span>
        <span className="pointer-events-none relative z-10 flex min-w-0 flex-1 flex-col">
          <span className="truncate text-base leading-5 text-token-text-primary">{task.displayName}</span>
          <span className="truncate text-sm leading-5 text-token-text-tertiary" id={`${task.interfaceId}-summary`}>{subtitle}</span>
        </span>
        <span className="pointer-events-auto relative z-10 flex shrink-0 items-center gap-1">
          <button
            className="flex h-token-button-composer-sm cursor-interaction items-center gap-1 rounded-lg border border-token-border bg-transparent px-2 text-sm text-token-foreground hover:bg-token-list-hover-background disabled:cursor-default disabled:opacity-40"
            disabled={!editable}
            onClick={onEdit}
            type="button"
          >Open</button>
          <button
            aria-expanded={menuOpen}
            aria-haspopup="menu"
            aria-label="Trigger actions"
            className="flex size-token-button-composer-sm cursor-interaction items-center justify-center rounded-lg border border-transparent text-token-foreground/50 hover:bg-token-list-hover-background hover:text-token-foreground data-[state=open]:bg-token-list-hover-background"
            data-state={menuOpen ? "open" : "closed"}
            onClick={() => setMenuOpen(!menuOpen)}
            ref={menuRef}
            type="button"
          ><Glyph name="more" size={16} /></button>
          <OcDropdown align="end" anchorRef={menuRef} aria-label="Trigger actions" onOpenChange={setMenuOpen} open={menuOpen} sideOffset={4} style={{ width: 208 }}>
            <OcMenuItem disabled={!editable} label="Edit" leadingIcon="edit" onClick={() => { setMenuOpen(false); onEdit(); }} role="menuitem" />
            {status === "completed" ? null : <OcMenuItem disabled={!editable} label={task.status === "disabled" ? "Resume" : "Pause"} leadingIcon={task.status === "disabled" ? "play" : "pause"} onClick={() => { setMenuOpen(false); onToggle(); }} role="menuitem" />}
            <OcMenuItem disabled={!canEdit || pending} label="Delete" leadingIcon="trash" onClick={() => { setMenuOpen(false); onDelete(); }} role="menuitem" tone="danger" />
          </OcDropdown>
        </span>
      </span>
    </span>
  );
}

function confirmation(task: ScheduledInterfaceProjection, status: ReturnType<typeof scheduledRowStatus>, scopeOptions: ScheduledScopeOption[]): string {
  if (status === "invalid") return `This trigger needs attention. ${task.error}`;
  if (status === "paused") return "This trigger is paused. Resume it to start runs again.";
  if (status === "completed") return "This trigger ran at its chosen time and does not repeat.";
  if (task.schedule?.type === "inactivity") {
    const withinEe = task.schedule.withinEe;
    const where = withinEe ? ` in ${scopeOptions.find((option) => option.eeId === withinEe)?.name ?? withinEe}` : "";
    const lastWork = task.inactivityState?.lastActivityAtMs ? ` Last work was ${formatEvidenceTime(task.inactivityState.lastActivityAtMs)}.` : "";
    return `Your trigger is ready. It runs ${describeSchedule(task).replace(/^After/, "after")}${where}. ${describeInactivityState(task)}.${lastWork}`;
  }
  const next = nextScheduledRunAtMs(task);
  if (!next) return "Your trigger is saved. You can review its schedule and settings in trigger details.";
  const zone = task.schedule?.type === "once" ? undefined : resolvedTimezone(task.timezone);
  let time: string;
  try {
    time = new Intl.DateTimeFormat(undefined, { month: "short", day: "numeric", hour: "numeric", minute: "2-digit", timeZoneName: "short", timeZone: zone }).format(new Date(next));
  } catch {
    time = new Date(next).toLocaleString();
  }
  return `Your trigger is scheduled. Its next run is ${time}${next !== task.nextRunAtMs || task.nextRunSource === "" ? " (estimated)" : ""}.`;
}
