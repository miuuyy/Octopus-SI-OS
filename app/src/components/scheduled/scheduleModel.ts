import type { OctopusScheduledDraft } from "../../electron";
import type { ChangeJournal, ChangeJournalEntry, InactivityState, ScheduledDelivery, ScheduledEditorDraft, ScheduledInterfaceProjection, ScheduledScopeOption } from "./types";

export function normalizeScheduledInterface(value: unknown): ScheduledInterfaceProjection | null {
  const row = objectValue(value);
  if (!row) return null;
  const interfaceId = stringValue(row.interface_id);
  const status = row.status === "enabled" || row.status === "disabled" || row.status === "invalid" || row.status === "template" ? row.status : "invalid";
  if (!interfaceId) return null;
  const rawSchedule = objectValue(row.schedule);
  const schedule = rawSchedule?.type === "interval" && positiveNumber(rawSchedule.interval_seconds)
    ? { type: "interval" as const, intervalSeconds: rawSchedule.interval_seconds as number }
    : rawSchedule?.type === "cron" && stringValue(rawSchedule.cron)
      ? { type: "cron" as const, cron: stringValue(rawSchedule.cron) }
      : rawSchedule?.type === "once" && Number.isFinite(Date.parse(stringValue(rawSchedule.due_at)))
        ? { type: "once" as const, dueAt: stringValue(rawSchedule.due_at) }
        : rawSchedule?.type === "inactivity" && positiveNumber(rawSchedule.seconds) && objectValue(rawSchedule.interface_match)
          ? { type: "inactivity" as const, seconds: rawSchedule.seconds, interfaceMatch: rawSchedule.interface_match as Record<string, string>, excludeInterfaceMatch: objectValue(rawSchedule.exclude_interface_match) as Record<string, string> | null, withinEe: stringValue(rawSchedule.within_ee) }
          : null;
  const rawEvent = objectValue(row.event_source);
  const eventSource = rawEvent ? { sources: stringArray(rawEvent.sources), targets: normalizeTargets(rawEvent.targets) } : null;
  const targets = normalizeTargets(row.targets);
  const template = row.template === true;
  const valid = row.valid === true
    && status !== "invalid"
    && (eventSource ? eventSource.targets.length > 0 && status === "enabled"
      : Boolean(schedule) && (template || targets.length > 0)
        && /^[0-9a-f]{64}$/.test(stringValue(row.activation_digest)));
  return {
    interfaceId,
    displayName: stringValue(row.display_name) || interfaceId,
    status: valid ? status : "invalid",
    valid,
    activationDigest: stringValue(row.activation_digest),
    activationPath: stringValue(row.activation_path),
    descriptorPath: stringValue(row.descriptor_path),
    eventSource,
    schedule,
    timezone: stringValue(row.timezone),
    fireOnStart: row.fire_on_start === true,
    instructions: typeof row.instructions === "string" ? row.instructions : "",
    reminderEnv: stringValue(row.reminder_env),
    targets,
    cloneEligible: valid && !eventSource && template,
    template,
    inactivityState: normalizeInactivityState(row.inactivity_state),
    deliveries: Array.isArray(row.deliveries) ? row.deliveries.map(normalizeDelivery).filter((delivery): delivery is ScheduledDelivery => Boolean(delivery)) : [],
    lastRunAtMs: nullableTimestamp(row.last_run_at_ms),
    lastMovementSeq: nullableInteger(row.last_movement_seq),
    nextRunAtMs: nullableTimestamp(row.next_run_at_ms),
    nextRunSource: row.next_run_source === "driver_state" || row.next_run_source === "schedule" ? row.next_run_source : "",
    error: stringValue(row.error) || (valid ? "" : eventSource ? "The event source has no active receiving task." : "The trigger sends to no task, or its activation does not satisfy the cron_life contract."),
  };
}

function normalizeTargets(value: unknown): ScheduledInterfaceProjection["targets"] {
  return Array.isArray(value) ? value.flatMap((entry) => {
    const target = objectValue(entry);
    const runId = stringValue(target?.run_id);
    return runId ? [{ runId, chatInterfaceId: stringValue(target?.chat_interface_id), title: stringValue(target?.title) || runId }] : [];
  }) : [];
}

function normalizeInactivityState(value: unknown): InactivityState | null {
  const state = objectValue(value);
  if (!state) return null;
  const status = ["unstarted", "idle", "waiting", "prepared", "admitted"].includes(stringValue(state.status)) ? stringValue(state.status) as InactivityState["status"] : "unstarted";
  return { status, current: state.current === true, lastActivityAtMs: nullableTimestamp(state.last_activity_at_ms) };
}

function normalizeDelivery(value: unknown): ScheduledDelivery | null {
  const row = objectValue(value);
  if (!row || !Number.isSafeInteger(row.seq) || !stringValue(row.run_id)) return null;
  return {
    seq: row.seq as number,
    atMs: nullableTimestamp(row.at_ms) ?? 0,
    runId: stringValue(row.run_id),
    chatInterfaceId: stringValue(row.chat_interface_id),
    title: stringValue(row.title),
    journal: normalizeJournal(row.journal),
  };
}

function normalizeJournal(value: unknown): ChangeJournal | null {
  const row = objectValue(value);
  if (!row) return null;
  const changes = Array.isArray(row.changes) ? row.changes.flatMap((entry): ChangeJournalEntry[] => {
    const change = objectValue(entry);
    if (!change) return [];
    if (change.kind === "file") {
      const action = change.action === "created" || change.action === "modified" || change.action === "deleted" ? change.action : null;
      const status = change.status === "applied" || change.status === "undone" || change.status === "changed" ? change.status : null;
      return action && status ? [{ kind: "file", path: stringValue(change.path), action, reason: stringValue(change.reason), status, restorable: change.restorable === true, unrestorableReason: stringValue(change.unrestorable_reason) }] : [];
    }
    return [{ kind: "other", description: stringValue(change.description), undo: stringValue(change.undo) }];
  }) : [];
  return {
    digest: stringValue(row.digest),
    firing: stringValue(row.firing),
    outcome: row.outcome === "changed" ? "changed" : "unchanged",
    summary: stringValue(row.summary),
    checks: Array.isArray(row.checks) ? row.checks.flatMap((entry) => {
      const check = objectValue(entry);
      return check ? [{ check: stringValue(check.check), result: stringValue(check.result), evidence: stringValue(check.evidence) }] : [];
    }) : [],
    changes,
    undoable: row.undoable === true && /^[0-9a-f]{64}$/.test(stringValue(row.digest)) && /^[0-9]+$/.test(stringValue(row.firing)),
    undone: row.undone === true,
    error: stringValue(row.error),
  };
}

export function normalizeScopeOptions(value: unknown): ScheduledScopeOption[] {
  return Array.isArray(value) ? value.flatMap((entry) => {
    const row = objectValue(entry);
    return row && stringValue(row.ee_id) ? [{ eeId: stringValue(row.ee_id), name: stringValue(row.name) || stringValue(row.ee_id) }] : [];
  }) : [];
}

export function describeInactivityState(task: Pick<ScheduledInterfaceProjection, "status" | "inactivityState" | "nextRunAtMs">): string {
  if (task.status === "disabled") return "Paused";
  const state = task.inactivityState;
  if (!state || state.status === "unstarted") return "Starts watching once the trigger is running";
  if (!state.current) return "Applying the new quiet period";
  if (state.status === "prepared") return "Starting a run";
  if (state.status === "waiting" && task.nextRunAtMs) return `Starts at ${formatEvidenceTime(task.nextRunAtMs)} if work stays quiet`;
  return "Waiting for new activity";
}

export const QUIET_PERIOD_UNITS = [
  { value: "minutes", label: "Minutes", seconds: 60 },
  { value: "hours", label: "Hours", seconds: 3600 },
] as const;

export type QuietPeriodUnit = typeof QUIET_PERIOD_UNITS[number]["value"];

export function quietPeriodUnit(seconds: number): QuietPeriodUnit {
  return seconds >= 3600 && seconds % 3600 === 0 ? "hours" : "minutes";
}

export function draftFromScheduledInterface(task: ScheduledInterfaceProjection): ScheduledEditorDraft {
  return {
    interfaceId: task.interfaceId,
    sourceInterfaceId: task.interfaceId,
    type: task.schedule?.type ?? "interval",
    intervalSeconds: task.schedule?.type === "interval" ? task.schedule.intervalSeconds : 3600,
    inactivity: task.schedule?.type === "inactivity"
      ? { seconds: task.schedule.seconds, interface_match: task.schedule.interfaceMatch, ...(task.schedule.excludeInterfaceMatch ? { exclude_interface_match: task.schedule.excludeInterfaceMatch } : {}), ...(task.schedule.withinEe ? { within_ee: task.schedule.withinEe } : {}) }
      : { seconds: 3600, interface_match: { interface_type: "world", role: "user", interface_driver_ref: "app_server" }, exclude_interface_match: { interface_driver_ref: "cron_life" } },
    cron: task.schedule?.type === "cron" ? task.schedule.cron : "0 9 * * 1-5",
    dueAt: task.schedule?.type === "once" ? task.schedule.dueAt : "",
    timezone: task.timezone || "local",
    fireOnStart: task.fireOnStart,
    instructions: task.instructions,
  };
}

export function draftFromTemplate(source: ScheduledInterfaceProjection): ScheduledEditorDraft {
  const base = draftFromScheduledInterface(source);
  return {
    ...base,
    interfaceId: nextScheduledInterfaceId(source.interfaceId),
    sourceInterfaceId: source.interfaceId,
    fireOnStart: false,
  };
}

export function mutationDraft(draft: ScheduledEditorDraft): OctopusScheduledDraft {
  return {
    type: draft.type,
    ...(draft.type === "inactivity" ? { inactivity: draft.inactivity } : draft.type === "interval" ? { intervalSeconds: draft.intervalSeconds } : draft.type === "once" ? { dueAt: draft.dueAt } : { cron: draft.cron }),
    timezone: draft.timezone.trim(),
    fireOnStart: draft.fireOnStart,
    instructions: draft.instructions,
  };
}

export function scheduledDraftError(draft: ScheduledEditorDraft, creating: boolean): string {
  if (creating && !/^[a-z0-9][a-z0-9_-]*$/.test(draft.interfaceId)) return "Interface id must use lowercase letters, digits, underscores, or hyphens.";
  if (!draft.instructions.trim()) return "Instructions are required.";
  if (!draft.timezone.trim()) return "Time zone is required.";
  if (draft.type === "inactivity") {
    if (!positiveNumber(draft.inactivity?.seconds) || draft.inactivity.seconds < 60) return "Quiet period must be at least one minute.";
    if (draft.fireOnStart) return "An inactivity trigger waits for activity and then a quiet period.";
  } else if (draft.type === "interval") {
    if (!positiveNumber(draft.intervalSeconds)) return "Interval must be greater than zero.";
  } else if (draft.type === "once") {
    if (!draft.dueAt || !Number.isFinite(Date.parse(draft.dueAt))) return "Choose a date and time.";
    if (creating && Date.parse(draft.dueAt) <= Date.now()) return "Choose a date and time in the future";
    if (draft.fireOnStart) return "A one-time task runs at its chosen time.";
  } else if (!validCronExpression(draft.cron ?? "")) {
    return "Cron must use five numeric fields: minute hour day-of-month month day-of-week.";
  }
  return "";
}

export type RepeatMode = "hourly" | "daily" | "weekdays" | "weekly" | "interval" | "inactivity" | "custom";

export const REPEAT_OPTIONS: ReadonlyArray<{ id: RepeatMode; label: string }> = [
  { id: "hourly", label: "Hourly" },
  { id: "daily", label: "Daily" },
  { id: "weekdays", label: "Weekdays" },
  { id: "weekly", label: "Weekly" },
  { id: "interval", label: "Interval" },
  { id: "inactivity", label: "After inactivity" },
  { id: "custom", label: "Custom" },
];

export const WEEKDAY_OPTIONS: ReadonlyArray<{ id: number; label: string }> = [
  { id: 1, label: "Monday" },
  { id: 2, label: "Tuesday" },
  { id: 3, label: "Wednesday" },
  { id: 4, label: "Thursday" },
  { id: 5, label: "Friday" },
  { id: 6, label: "Saturday" },
  { id: 0, label: "Sunday" },
];

export const INTERVAL_UNITS = [
  { value: "seconds", label: "seconds", seconds: 1 },
  { value: "minutes", label: "minutes", seconds: 60 },
  { value: "hours", label: "hours", seconds: 3600 },
  { value: "days", label: "days", seconds: 86_400 },
] as const;

export type IntervalUnit = typeof INTERVAL_UNITS[number]["value"];

export function intervalUnit(seconds: number): IntervalUnit {
  if (seconds >= 86_400 && seconds % 86_400 === 0) return "days";
  if (seconds >= 3600 && seconds % 3600 === 0) return "hours";
  if (seconds >= 60 && seconds % 60 === 0) return "minutes";
  return "seconds";
}

type CronShape = { mode: "hourly" | "daily" | "weekdays" | "weekly"; hour: number; minute: number; weekday: number };

function cronShape(expression: string): CronShape | null {
  const parts = expression.trim().split(/\s+/);
  if (parts.length !== 5) return null;
  const [minute, hour, dayOfMonth, month, dayOfWeek] = parts;
  if (dayOfMonth !== "*" || month !== "*" || !/^\d{1,2}$/.test(minute) || Number(minute) > 59) return null;
  if (hour === "*") return dayOfWeek === "*" ? { mode: "hourly", hour: 9, minute: Number(minute), weekday: 1 } : null;
  if (!/^\d{1,2}$/.test(hour) || Number(hour) > 23) return null;
  const clock = { hour: Number(hour), minute: Number(minute) };
  if (dayOfWeek === "*") return { mode: "daily", ...clock, weekday: 1 };
  if (dayOfWeek === "1-5") return { mode: "weekdays", ...clock, weekday: 1 };
  if (/^[0-7]$/.test(dayOfWeek)) return { mode: "weekly", ...clock, weekday: Number(dayOfWeek) % 7 };
  return null;
}

function cronFromShape(shape: CronShape): string {
  if (shape.mode === "hourly") return `${shape.minute} * * * *`;
  const days = shape.mode === "daily" ? "*" : shape.mode === "weekdays" ? "1-5" : String(shape.weekday);
  return `${shape.minute} ${shape.hour} * * ${days}`;
}

export function repeatMode(draft: Pick<ScheduledEditorDraft, "type" | "cron">): RepeatMode | "once" {
  if (draft.type === "once" || draft.type === "interval" || draft.type === "inactivity") return draft.type;
  return cronShape(draft.cron ?? "")?.mode ?? "custom";
}

export function cronClock(draft: Pick<ScheduledEditorDraft, "cron">): { hour: number; minute: number; weekday: number } {
  const shape = cronShape(draft.cron ?? "");
  return shape ? { hour: shape.hour, minute: shape.minute, weekday: shape.weekday } : { hour: 9, minute: 0, weekday: 1 };
}

export function withRepeatMode(draft: ScheduledEditorDraft, mode: RepeatMode): ScheduledEditorDraft {
  if (mode === "interval") return { ...draft, type: "interval", intervalSeconds: positiveNumber(draft.intervalSeconds) ? draft.intervalSeconds : 3600 };
  if (mode === "inactivity") return { ...draft, type: "inactivity", fireOnStart: false };
  if (mode === "custom") return { ...draft, type: "cron" };
  return { ...draft, type: "cron", cron: cronFromShape({ mode, ...cronClock(draft) }) };
}

export function withCronClock(draft: ScheduledEditorDraft, change: Partial<{ hour: number; minute: number; weekday: number }>): ScheduledEditorDraft {
  const mode = repeatMode(draft);
  if (mode !== "hourly" && mode !== "daily" && mode !== "weekdays" && mode !== "weekly") return draft;
  return { ...draft, cron: cronFromShape({ mode, ...cronClock(draft), ...change }) };
}

export function withRepeat(draft: ScheduledEditorDraft, repeat: boolean): ScheduledEditorDraft {
  if (!repeat) return { ...draft, type: "once", fireOnStart: false, dueAt: draft.dueAt && Number.isFinite(Date.parse(draft.dueAt)) ? draft.dueAt : defaultDueAt() };
  return { ...draft, type: validCronExpression(draft.cron ?? "") ? "cron" : "interval" };
}

function defaultDueAt(): string {
  const next = new Date();
  next.setMinutes(0, 0, 0);
  next.setHours(next.getHours() + 1);
  return next.toISOString();
}

export function validCron(expression: string): boolean {
  return validCronExpression(expression) && parseCron(expression) !== null;
}

export function resolvedTimezone(timezone: string): string {
  const value = timezone.trim();
  return !value || value === "local" ? localTimezone() || "UTC" : value;
}

export function clockLabel(hour: number, minute: number): string {
  return formatClock(hour, minute);
}

export function timezoneName(timezone: string, at: number = Date.now()): string {
  try {
    return new Intl.DateTimeFormat(undefined, { timeZone: resolvedTimezone(timezone), timeZoneName: "shortGeneric" })
      .formatToParts(new Date(at)).find((part) => part.type === "timeZoneName")?.value ?? "";
  } catch {
    return timezone;
  }
}

export function shortScheduleLabel(task: Pick<ScheduledInterfaceProjection, "schedule" | "eventSource">): string {
  if (task.eventSource) return "On event";
  const schedule = task.schedule;
  if (!schedule || schedule.type === "once") return "";
  if (schedule.type === "interval") return formatInterval(schedule.intervalSeconds);
  if (schedule.type === "inactivity") return describeSchedule(task);
  const mode = cronShape(schedule.cron)?.mode;
  return mode === "hourly" ? "Hourly" : mode === "daily" ? "Daily" : mode === "weekdays" ? "Weekdays" : mode === "weekly" ? "Weekly" : describeCron(schedule.cron);
}

export function formatSchedule(task: Pick<ScheduledInterfaceProjection, "schedule" | "timezone">): string {
  if (task.schedule?.type === "once" || task.schedule?.type === "inactivity") return describeSchedule(task);
  const timezone = task.timezone?.trim();
  return timezone ? `${describeSchedule(task)} · ${timezone}` : describeSchedule(task);
}

export function describeSchedule(task: Pick<ScheduledInterfaceProjection, "schedule" | "eventSource">): string {
  if (task.eventSource) return "On event";
  if (!task.schedule) return "Invalid schedule";
  if (task.schedule.type === "interval") return formatInterval(task.schedule.intervalSeconds);
  if (task.schedule.type === "inactivity") return `After ${formatInterval(task.schedule.seconds).replace(/^Every /, "")} without activity`;
  if (task.schedule.type === "once") {
    const at = Date.parse(task.schedule.dueAt);
    return Number.isFinite(at) ? `Once · ${formatEvidenceTime(at)}` : "Choose a date and time";
  }
  return describeCron(task.schedule.cron);
}

export function scheduleExpression(task: Pick<ScheduledInterfaceProjection, "schedule">): string {
  if (!task.schedule) return "None";
  if (task.schedule.type === "once") return task.schedule.dueAt;
  if (task.schedule.type === "inactivity") return `${task.schedule.seconds}s without activity`;
  return task.schedule.type === "cron" ? task.schedule.cron : `${task.schedule.intervalSeconds}s interval`;
}

export function localTimezone(): string {
  return Intl.DateTimeFormat().resolvedOptions().timeZone || "";
}

export function nextScheduledRunAtMs(
  task: Pick<ScheduledInterfaceProjection, "schedule" | "status" | "timezone" | "lastRunAtMs" | "nextRunAtMs" | "valid">,
  fromMs: number = Date.now(),
): number | null {
  if (!task.valid || task.status !== "enabled" || !task.schedule) return null;
  if (task.schedule.type === "once" || task.schedule.type === "inactivity") return task.nextRunAtMs;
  if (task.nextRunAtMs && task.nextRunAtMs > fromMs) return task.nextRunAtMs;
  if (task.schedule.type === "interval") {
    const period = task.schedule.intervalSeconds * 1000;
    if (period <= 0) return null;
    let next = (task.lastRunAtMs ?? fromMs) + period;
    while (next <= fromMs) next += period;
    return next;
  }
  return nextCronRunAtMs(task.schedule.cron, task.timezone, fromMs);
}

type CronFields = {
  daysOfMonth: Set<number>;
  daysOfWeek: Set<number>;
  domRestricted: boolean;
  dowRestricted: boolean;
  hours: Set<number>;
  minutes: Set<number>;
  months: Set<number>;
};

const WEEKDAY_NAMES = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
const WEEKDAY_SHORT = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

export function describeCron(expression: string): string {
  const raw = expression.trim().split(/\s+/);
  const fields = parseCron(expression);
  if (!fields || raw.length !== 5) return expression;
  const [rawMinute, rawHour, rawDom, rawMonth, rawDow] = raw;
  if (rawHour === "*" && rawDom === "*" && rawMonth === "*" && rawDow === "*") {
    if (rawMinute === "*") return "Every minute";
    const step = stepOf(rawMinute);
    if (step) return `Every ${plural(step, "minute", "minutes")}`;
    if (fields.minutes.size === 1) return `Hourly at :${String([...fields.minutes][0]).padStart(2, "0")}`;
    return `${plural(fields.minutes.size, "time", "times")} an hour`;
  }
  if (rawMonth !== "*" || fields.minutes.size !== 1) return expression;
  const time = clockText(fields);
  if (!time) return expression;
  if (rawDom === "*" && rawDow === "*") {
    return time.startsWith("every ") ? `${time[0].toLocaleUpperCase()}${time.slice(1)}` : `Daily at ${time}`;
  }
  if (rawDom === "*") return joinClock(weekdayText(fields.daysOfWeek), time);
  if (rawDow === "*" && fields.daysOfMonth.size === 1) {
    return joinClock(`Monthly on the ${ordinal([...fields.daysOfMonth][0])}`, time);
  }
  return expression;
}

function joinClock(prefix: string, time: string): string {
  return time.startsWith("every ") ? `${prefix} ${time}` : `${prefix} at ${time}`;
}

function clockText(fields: CronFields): string {
  const minute = [...fields.minutes][0];
  const hours = [...fields.hours].sort((left, right) => left - right);
  if (hours.length === 1) return formatClock(hours[0], minute);
  if (hours.length > 3) {
    const step = hours[1] - hours[0];
    const even = hours.every((hour, index) => index === 0 || hour - hours[index - 1] === step);
    if (even) return `every ${plural(step, "hour", "hours")}`;
  }
  return hours.map((hour) => formatClock(hour, minute)).join(", ");
}

function formatClock(hour: number, minute: number): string {
  const date = new Date(Date.UTC(2024, 0, 1, hour, minute));
  return new Intl.DateTimeFormat(undefined, { hour: "numeric", minute: "2-digit", timeZone: "UTC" }).format(date);
}

function weekdayText(days: Set<number>): string {
  const sorted = [...days].sort((left, right) => left - right);
  const key = sorted.join(",");
  if (key === "1,2,3,4,5") return "Weekdays";
  if (key === "0,6") return "Weekends";
  if (sorted.length === 1) return `Every ${WEEKDAY_NAMES[sorted[0]]}`;
  return sorted.map((day) => WEEKDAY_SHORT[day]).join(", ");
}

function ordinal(day: number): string {
  const rest = day % 100;
  if (rest >= 11 && rest <= 13) return `${day}th`;
  const last = day % 10;
  return `${day}${last === 1 ? "st" : last === 2 ? "nd" : last === 3 ? "rd" : "th"}`;
}

function stepOf(field: string): number | null {
  const match = /^\*\/(\d+)$/.exec(field);
  if (!match) return null;
  const step = Number(match[1]);
  return step > 1 ? step : null;
}

function parseCron(expression: string): CronFields | null {
  const parts = expression.trim().split(/\s+/);
  if (parts.length !== 5) return null;
  const minutes = parseCronField(parts[0], 0, 59);
  const hours = parseCronField(parts[1], 0, 23);
  const daysOfMonth = parseCronField(parts[2], 1, 31);
  const months = parseCronField(parts[3], 1, 12);
  const daysOfWeek = parseCronField(parts[4], 0, 7);
  if (!minutes || !hours || !daysOfMonth || !months || !daysOfWeek) return null;
  if (daysOfWeek.delete(7)) daysOfWeek.add(0);
  return {
    daysOfMonth,
    daysOfWeek,
    domRestricted: parts[2] !== "*",
    dowRestricted: parts[4] !== "*",
    hours,
    minutes,
    months,
  };
}

function parseCronField(field: string, min: number, max: number): Set<number> | null {
  const values = new Set<number>();
  for (const chunk of field.split(",")) {
    const [range, stepText] = chunk.split("/");
    const step = stepText === undefined ? 1 : Number(stepText);
    if (!Number.isInteger(step) || step < 1) return null;
    let start = min;
    let end = max;
    if (range !== "*" && range !== "") {
      const bounds = range.split("-");
      if (bounds.length > 2) return null;
      start = Number(bounds[0]);
      end = bounds.length === 2 ? Number(bounds[1]) : stepText === undefined ? start : max;
      if (!Number.isInteger(start) || !Number.isInteger(end) || start < min || end > max || start > end) return null;
    }
    for (let value = start; value <= end; value += step) values.add(value);
  }
  return values.size ? values : null;
}

function nextCronRunAtMs(cron: string, timezone: string, fromMs: number): number | null {
  const fields = parseCron(cron);
  if (!fields) return null;
  const zone = timezone.trim() || localTimezone() || "UTC";
  let offset = 0;
  try {
    offset = timezoneOffsetMs(zone, fromMs);
  } catch {
    return null;
  }
  const cursor = new Date(fromMs + offset);
  cursor.setUTCSeconds(0, 0);
  cursor.setUTCMinutes(cursor.getUTCMinutes() + 1);
  const limit = fromMs + offset + 366 * 86_400_000;
  while (cursor.getTime() <= limit) {
    if (!fields.months.has(cursor.getUTCMonth() + 1)) {
      cursor.setUTCHours(0, 0, 0, 0);
      cursor.setUTCDate(1);
      cursor.setUTCMonth(cursor.getUTCMonth() + 1);
      continue;
    }
    if (!cronDayMatches(fields, cursor)) {
      cursor.setUTCHours(0, 0, 0, 0);
      cursor.setUTCDate(cursor.getUTCDate() + 1);
      continue;
    }
    if (!fields.hours.has(cursor.getUTCHours())) {
      cursor.setUTCMinutes(0, 0, 0);
      cursor.setUTCHours(cursor.getUTCHours() + 1);
      continue;
    }
    if (!fields.minutes.has(cursor.getUTCMinutes())) {
      cursor.setUTCMinutes(cursor.getUTCMinutes() + 1);
      continue;
    }
    const wall = cursor.getTime();
    try {
      return wall - timezoneOffsetMs(zone, wall - offset);
    } catch {
      return wall - offset;
    }
  }
  return null;
}

function cronDayMatches(fields: CronFields, cursor: Date): boolean {
  const dayOfMonth = fields.daysOfMonth.has(cursor.getUTCDate());
  const dayOfWeek = fields.daysOfWeek.has(cursor.getUTCDay());
  if (fields.domRestricted && fields.dowRestricted) return dayOfMonth || dayOfWeek;
  if (fields.domRestricted) return dayOfMonth;
  if (fields.dowRestricted) return dayOfWeek;
  return true;
}

function timezoneOffsetMs(timeZone: string, at: number): number {
  const parts = new Intl.DateTimeFormat("en-US", {
    day: "2-digit",
    hour: "2-digit",
    hour12: false,
    minute: "2-digit",
    month: "2-digit",
    second: "2-digit",
    timeZone,
    year: "numeric",
  }).formatToParts(new Date(at));
  const values: Record<string, number> = {};
  for (const part of parts) {
    if (part.type !== "literal") values[part.type] = Number(part.value);
  }
  const hour = values.hour === 24 ? 0 : values.hour;
  const asUtc = Date.UTC(values.year, values.month - 1, values.day, hour, values.minute, values.second);
  return asUtc - (at - (at % 1000));
}

export function formatInterval(seconds: number): string {
  if (seconds % 86_400 === 0) return `Every ${plural(seconds / 86_400, "day", "days")}`;
  if (seconds % 3_600 === 0) return `Every ${plural(seconds / 3_600, "hour", "hours")}`;
  if (seconds % 60 === 0) return `Every ${plural(seconds / 60, "minute", "minutes")}`;
  return `Every ${plural(seconds, "second", "seconds")}`;
}

export function formatEvidenceTime(timestamp: number | null): string {
  if (!timestamp) return "Not recorded";
  return new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" }).format(new Date(timestamp));
}

export function relativeEvidenceTime(timestamp: number | null): string {
  if (!timestamp) return "";
  const delta = timestamp - Date.now();
  const future = delta > 0;
  const magnitude = Math.abs(delta);
  const formatter = new Intl.RelativeTimeFormat(undefined, { numeric: "auto" });
  if (magnitude < 90_000) return formatter.format(future ? 1 : -1, "minute");
  if (magnitude < 5_400_000) return formatter.format(Math.round(delta / 60_000), "minute");
  if (magnitude < 129_600_000) return formatter.format(Math.round(delta / 3_600_000), "hour");
  return formatter.format(Math.round(delta / 86_400_000), "day");
}

function nextScheduledInterfaceId(source: string): string {
  const stem = source.replace(/^(world|system)_/, "").replace(/_cron$/, "").replace(/[^a-z0-9_-]/g, "_");
  return `world_${stem || "scheduled"}_${new Date().toISOString().slice(0, 10).replaceAll("-", "")}`;
}

function validCronExpression(expression: string): boolean {
  const fields = expression.trim().split(/\s+/);
  return fields.length === 5 && fields.every((field) => /^[0-9*/,-]+$/.test(field));
}

function plural(value: number, singular: string, pluralForm: string): string {
  return `${value} ${value === 1 ? singular : pluralForm}`;
}

function objectValue(value: unknown): Record<string, unknown> | null {
  return value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : null;
}

function stringValue(value: unknown): string {
  return typeof value === "string" ? value.trim() : "";
}

function stringArray(value: unknown): string[] {
  return Array.isArray(value) ? value.map(stringValue).filter(Boolean) : [];
}

function positiveNumber(value: unknown): value is number {
  return typeof value === "number" && Number.isFinite(value) && value > 0;
}

function nullableTimestamp(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) && value > 0 ? value : null;
}

function nullableInteger(value: unknown): number | null {
  return Number.isSafeInteger(value) && (value as number) >= 0 ? value as number : null;
}
