import { useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import { OcMenuItem, cx } from "../../design-system/components";
import { OcDropdown } from "../../design-system/OcDropdown";
import type { GlyphName } from "../IconGlyphs";
import { Glyph } from "../IconGlyphs";
import { MessageTooltip } from "../chat/MessageTooltip";
import { formatEvidenceTime, nextScheduledRunAtMs, shortScheduleLabel } from "./scheduleModel";
import type { ScheduledDelivery, ScheduledInterfaceProjection } from "./types";

export type ScheduledRowStatus = "active" | "paused" | "completed" | "invalid";

export type ScheduledRowAction = { glyph: GlyphName; label: string; disabled?: boolean; tone?: "default" | "danger"; onSelect: () => void };

const ROW_SURFACE = "octopus-nav-row octopus-task-row group relative cursor-interaction sidebar-item text-sm hover:bg-token-list-hover-background focus-visible:outline-offset-[-2px] aria-disabled:cursor-default aria-disabled:opacity-50";

export function scheduledRowStatus(task: ScheduledInterfaceProjection): ScheduledRowStatus {
  if (task.status === "invalid") return "invalid";
  if (task.status === "fired") return "completed";
  if (task.status === "disarmed") return "paused";
  if (task.status === "disabled") return "paused";
  return task.schedule?.type === "once" && task.lastRunAtMs !== null && task.nextRunAtMs === null ? "completed" : "active";
}

export function ScheduledTaskRow({ actions, expanded, canExpand, now, onActivate, selected, task }: {
  actions: ScheduledRowAction[];
  canExpand: boolean;
  expanded: boolean;
  now: number;
  onActivate: () => void;
  selected: boolean;
  task: ScheduledInterfaceProjection;
}) {
  const status = scheduledRowStatus(task);
  const oneTime = task.schedule?.type === "once";
  const next = nextScheduledRunAtMs(task, now);
  const date = next ?? task.lastRunAtMs;
  const schedule = oneTime ? "" : shortScheduleLabel(task);
  const separator = <span aria-hidden="true" className="mx-1 last:hidden">·</span>;
  const secondary = (
    <span className="flex w-full min-w-0 items-center gap-1 pe-row-y text-xs leading-4 text-tertiary select-none" data-thread-secondary-title="">
      <span className="min-w-0 flex-1 truncate">
        {status === "invalid" ? <>Needs attention{separator}</> : null}
        {status === "paused" ? <>Paused{schedule || oneTime && date != null ? separator : null}</> : null}
        {status === "completed" ? <>Completed{schedule || date != null ? separator : null}</> : null}
        {(status !== "paused" || oneTime) && date != null ? <><span>{scheduledCalendarTime(date, now, task.timezone, oneTime)}</span>{schedule ? separator : null}</> : null}
        {schedule ? <span>{schedule}</span> : null}
      </span>
    </span>
  );
  return (
    <SidebarRow
      actions={actions}
      active={selected}
      ariaLabel={`${task.displayName}${status === "active" ? "" : `, ${status === "invalid" ? "needs attention" : status}`}`}
      onActivate={onActivate}
      secondary={secondary}
      suffix={canExpand ? (
        <span className={expanded ? "inline-flex text-tertiary" : "hidden text-tertiary group-hover:inline-flex group-focus-visible:inline-flex group-has-[:focus-visible]:inline-flex"}>
          <span className="inline-flex size-5 items-center justify-center"><Glyph className="icon-xs" name={expanded ? "chevronUp" : "chevronDown"} size={16} /></span>
        </span>
      ) : null}
      status={status === "invalid" ? (
        <MessageTooltip label={task.error}>
          <span className="relative flex size-5 shrink-0 items-center justify-center"><Glyph className="shrink-0 text-danger" name="warning" size={16} /></span>
        </MessageTooltip>
      ) : status === "completed" ? (
        <MessageTooltip label="Completed"><span className="inline-flex text-tertiary"><Glyph className="icon-xs" name="check" size={16} /></span></MessageTooltip>
      ) : null}
      title={task.displayName}
      titleTone={status === "active" ? "default" : "muted"}
    />
  );
}

export function ScheduledRunRows({ deliveries, onOpen, onUndo, openable }: {
  deliveries: ScheduledDelivery[];
  onOpen: (delivery: ScheduledDelivery) => void;
  openable: boolean;
  onUndo: (delivery: ScheduledDelivery) => void;
}) {
  const [limit, setLimit] = useState(5);
  if (!deliveries.length) return <p className="m-0 px-2 py-2 text-xs text-secondary">No runs yet</p>;
  return (
    <>
      {deliveries.slice(0, limit).map((delivery) => (
        <SidebarRow
          actions={delivery.journal?.undoable ? [{ glyph: "restore", label: "Undo changes…", onSelect: () => onUndo(delivery) }] : []}
          ariaLabel={`${runTitle(delivery)}, ${formatEvidenceTime(delivery.atMs)}`}
          disabled={!openable || !delivery.chatInterfaceId}
          indent
          key={delivery.seq}
          meta={compactAge(delivery.atMs)}
          onActivate={() => onOpen(delivery)}
          title={runTitle(delivery)}
        />
      ))}
      {deliveries.length > limit ? (
        <div className="flex gap-1 py-1">
          <ShowMoreButton onClick={() => setLimit(deliveries.length)}>Show older</ShowMoreButton>
        </div>
      ) : null}
    </>
  );
}

export function ShowMoreButton({ children, onClick }: { children: ReactNode; onClick: () => void }) {
  return (
    <button className="no-drag flex h-6 cursor-interaction items-center rounded-lg px-2 text-tertiary select-none hover:bg-primary-ghost-hover hover:text-secondary focus:outline-hidden focus-visible:ring-2 focus-visible:ring-ring" onClick={onClick} type="button">
      <span className="text-base">{children}</span>
    </button>
  );
}

export function runTitle(delivery: ScheduledDelivery): string {
  const journal = delivery.journal;
  if (journal?.error) return "Run journal is unreadable";
  if (journal?.undone) return journal.summary ? `Undone · ${journal.summary}` : "Run changes undone";
  if (delivery.title) return delivery.title;
  if (journal?.summary) return journal.summary;
  if (journal?.outcome === "unchanged") return "No update from this run";
  if (journal) {
    const files = journal.changes.filter((change) => change.kind === "file").length;
    return files ? `${files} ${files === 1 ? "file" : "files"} changed` : "Run completed";
  }
  return "Run started";
}

function SidebarRow({ actions = [], active = false, ariaLabel, disabled = false, indent = false, meta, onActivate, secondary, status, suffix, title, titleTone = "default" }: {
  actions?: ScheduledRowAction[];
  active?: boolean;
  ariaLabel: string;
  disabled?: boolean;
  indent?: boolean;
  meta?: string;
  onActivate: () => void;
  secondary?: ReactNode;
  status?: ReactNode;
  suffix?: ReactNode;
  title: string;
  titleTone?: "default" | "muted";
}) {
  const [menuOpen, setMenuOpen] = useState(false);
  const menuRef = useRef<HTMLButtonElement>(null);
  const multiline = secondary != null;
  const activate = () => { if (!disabled) onActivate(); };
  return (
    <div
      aria-current={active ? "true" : undefined}
      aria-disabled={disabled || undefined}
      aria-label={ariaLabel}
      className={cx(
        ROW_SURFACE,
        multiline ? "pt-1.5 pb-2 pe-row-y ps-[var(--padding-row-cell-x,var(--padding-row-x))]" : "h-[var(--height-token-row)] px-[var(--padding-row-cell-x,var(--padding-row-x))] py-row-y",
        (active || menuOpen) && "bg-token-list-hover-background",
      )}
      data-sidebar-nav-item=""
      onClick={activate}
      onKeyDown={(event: KeyboardEvent<HTMLDivElement>) => {
        if (event.currentTarget !== event.target || (event.key !== "Enter" && event.key !== " ")) return;
        event.preventDefault();
        activate();
      }}
      role="button"
      tabIndex={0}
    >
      <div className="flex h-full w-full items-center text-sm leading-4">
        <div className={cx("flex min-w-0 flex-1 items-center gap-2", indent ? "ps-4" : "ps-0.5")}>
          <div className={cx("flex min-w-0 flex-1 self-stretch text-base leading-5", multiline ? "flex-col items-start gap-0.5" : "items-center gap-2")}>
            <div className={cx("flex w-full min-w-0 flex-1 items-center gap-1", actions.length > 0 && "group-focus-visible:pe-6 group-hover:pe-6 group-has-[:focus-visible]:pe-6")}>
              <span className={cx("min-w-0 select-none truncate", suffix ? "w-auto flex-initial" : "flex-1", titleTone === "muted" ? "text-tertiary" : "text-token-foreground")} data-thread-title="" dir="auto">{title}</span>
              {suffix ? <span className="flex shrink-0">{suffix}</span> : null}
            </div>
            {multiline ? <div className="flex w-full min-w-0">{secondary}</div> : null}
          </div>
        </div>
        {status || meta || actions.length ? (
          <div className="sidebar-task-trailing">
            {status ?? (meta ? <span className="sidebar-task-age">{meta}</span> : null)}
            {actions.length ? (
              <div className={cx("sidebar-task-action", menuOpen && "opacity-100! pointer-events-auto!")}>
                <MessageTooltip label={multiline ? "Trigger actions" : "Run actions"}>
                  <button
                    aria-expanded={menuOpen}
                    aria-haspopup="menu"
                    aria-label={multiline ? "Trigger actions" : "Run actions"}
                    className="no-drag flex !h-5 !w-5 cursor-interaction items-center justify-center rounded-md border border-transparent !p-0 !text-token-foreground/50 select-none hover:!text-token-foreground focus:outline-none focus-visible:!text-token-foreground"
                    data-state={menuOpen ? "open" : "closed"}
                    onClick={(event) => { event.stopPropagation(); setMenuOpen(!menuOpen); }}
                    onPointerDown={(event) => event.stopPropagation()}
                    ref={menuRef}
                    type="button"
                  >
                    <Glyph className="translate-x-px" name="more" size={16} />
                  </button>
                </MessageTooltip>
                <OcDropdown align="end" anchorRef={menuRef} aria-label={multiline ? "Trigger actions" : "Run actions"} onOpenChange={setMenuOpen} open={menuOpen} sideOffset={4} style={{ width: 208 }}>
                  {actions.map((action) => (
                    <OcMenuItem disabled={action.disabled} key={action.label} label={action.label} leadingIcon={action.glyph} onClick={(event) => { event.stopPropagation(); setMenuOpen(false); action.onSelect(); }} role="menuitem" tone={action.tone} />
                  ))}
                </OcDropdown>
              </div>
            ) : null}
          </div>
        ) : null}
      </div>
    </div>
  );
}

function compactAge(at: number, now = Date.now()): string {
  if (!at) return "";
  const minutes = Math.floor(Math.max(0, now - at) / 60_000);
  if (minutes < 1) return "now";
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  return hours < 24 ? `${hours}h` : `${Math.floor(hours / 24)}d`;
}

export function scheduledCalendarTime(timestamp: number, now: number, timezone: string, withTime = true): string {
  const timeZone = timezone && timezone !== "local" ? timezone : undefined;
  try {
    const day = new Intl.DateTimeFormat("en-CA", { timeZone, year: "numeric", month: "2-digit", day: "2-digit" });
    const calendarDay = (value: number) => {
      const parts = Object.fromEntries(day.formatToParts(new Date(value)).map((part) => [part.type, part.value]));
      return Date.UTC(Number(parts.year), Number(parts.month) - 1, Number(parts.day));
    };
    const at = new Date(timestamp);
    const minuteOfHour = Number(new Intl.DateTimeFormat("en-US", { timeZone, minute: "numeric" }).format(at));
    const time = new Intl.DateTimeFormat(undefined, { timeZone, hour: "numeric", ...(minuteOfHour === 0 ? {} : { minute: "2-digit" }) }).format(at);
    const dayDifference = (calendarDay(timestamp) - calendarDay(now)) / 86_400_000;
    if (dayDifference === 0) return `Today ${time}`;
    if (dayDifference === 1) return `Tomorrow ${time}`;
    return new Intl.DateTimeFormat(undefined, { timeZone, month: "short", day: "numeric", ...(withTime ? { hour: "numeric", minute: "2-digit" } : {}) }).format(at);
  } catch {
    return "Time unavailable";
  }
}
