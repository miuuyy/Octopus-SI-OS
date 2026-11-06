import { useId, useMemo, useRef, useState, type FormEvent, type KeyboardEvent, type ReactNode } from "react";
import { OcButton, OcDialog, OcDialogFooter, OcDialogHeader, OcSwitch, OcTextField, cx } from "../../design-system/components";
import { Glyph } from "../IconGlyphs";
import { MessageTooltip } from "../chat/MessageTooltip";
import {
  INTERVAL_UNITS,
  QUIET_PERIOD_UNITS,
  REPEAT_OPTIONS,
  WEEKDAY_OPTIONS,
  clockLabel,
  cronClock,
  describeCron,
  draftFromTemplate,
  intervalUnit,
  quietPeriodUnit,
  repeatMode,
  scheduledDraftError,
  timezoneName,
  validCron,
  withCronClock,
  withRepeat,
  withRepeatMode,
  type IntervalUnit,
  type QuietPeriodUnit,
  type RepeatMode,
} from "./scheduleModel";
import { QUARTER_HOURS, ScheduleFormDate, ScheduleFormGroup, ScheduleFormMenu, ScheduleFormNumber, ScheduleFormRow, ScheduleFormValue } from "./scheduleFormControls";
import type { ScheduledEditorDraft, ScheduledEditorMode, ScheduledInterfaceProjection, ScheduledScopeOption } from "./types";

export function ScheduledDialog({
  draft: initialDraft,
  error,
  mode,
  pending,
  scopeOptions,
  templates,
  onCancel,
  onSave,
}: {
  draft: ScheduledEditorDraft;
  error: string;
  mode: ScheduledEditorMode;
  pending: boolean;
  scopeOptions: ScheduledScopeOption[];
  templates: ScheduledInterfaceProjection[];
  onCancel: () => void;
  onSave: (draft: ScheduledEditorDraft, source: ScheduledInterfaceProjection) => void;
}) {
  const titleId = useId();
  const instructionsRef = useRef<HTMLTextAreaElement>(null);
  const [draft, setDraft] = useState(initialDraft);
  const [ruleOpen, setRuleOpen] = useState(false);
  const creating = mode.kind === "create";
  const source = templates.find((template) => template.interfaceId === draft.sourceInterfaceId)
    ?? (mode.kind === "create" ? mode.source : mode.task);
  const draftError = scheduledDraftError(draft, creating);
  const blocked = Boolean(draftError) || pending || (creating && !source.cloneEligible);
  const visibleError = draftError && draftError !== "Instructions are required." ? draftError : "";

  function submit(event: FormEvent) {
    event.preventDefault();
    if (!blocked) onSave(draft, source);
  }

  function chooseTemplate(interfaceId: string) {
    const selected = templates.find((template) => template.interfaceId === interfaceId);
    if (!selected) return;
    const replacement = draftFromTemplate(selected);
    setDraft({ ...replacement, interfaceId: draft.interfaceId, instructions: draft.instructions.trim() ? draft.instructions : replacement.instructions });
  }

  return (
    <OcDialog ariaLabelledBy={titleId} busy={pending} initialFocusRef={creating ? instructionsRef : undefined} onClose={onCancel} open position="top" size="xwide" variant="form">
      <form
        aria-busy={pending || undefined}
        className="group/settings flex min-h-0 flex-col gap-0 p-5 text-base leading-normal tracking-normal"
        data-density="form"
        onKeyDown={(event: KeyboardEvent<HTMLFormElement>) => {
          if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) { event.preventDefault(); event.currentTarget.requestSubmit(); }
        }}
        onSubmit={submit}
      >
        <div className="flex w-full shrink-0 flex-col">
          <h2 className="m-0 text-form-title text-default" id={titleId}>{creating ? "Create a trigger" : "Edit trigger"}</h2>
        </div>
        <div className="flex min-h-0 w-full flex-auto flex-col pt-5">
          <div className="hide-scrollbar min-h-0 overflow-y-auto">
            <fieldset className="m-0 min-w-0 border-0 p-0 text-form-body font-normal" disabled={pending}>
              <div className="flex min-w-0 flex-col gap-4">
                {error ? <p className="m-0 text-sm text-danger select-none" role="alert">{error}</p> : null}
                <ScheduleFormGroup>
                  <div className="flex min-h-30 min-w-0 cursor-text gap-2 px-4.5 py-4" onMouseDown={(event) => {
                    if (event.target === event.currentTarget) { event.preventDefault(); instructionsRef.current?.focus(); }
                  }} role="presentation">
                    <textarea
                      aria-label="Instructions"
                      className="min-h-[calc(var(--text-form-body--line-height)*4)] w-full resize-none border-0 bg-transparent p-0 text-form-body text-default outline-none [field-sizing:content] placeholder:text-tertiary"
                      onChange={(event) => setDraft({ ...draft, instructions: event.target.value })}
                      placeholder="Instructions…"
                      ref={instructionsRef}
                      value={draft.instructions}
                    />
                  </div>
                </ScheduleFormGroup>
                <ScheduleSection draft={draft} onChange={setDraft} onEditRule={() => setRuleOpen(true)} scopeOptions={scopeOptions} />
                <AdvancedSection creating={creating} draft={draft} onChange={setDraft} onChooseTemplate={chooseTemplate} source={source} templates={templates} />
                {visibleError ? <p className="m-0 text-sm text-danger select-none" role="alert">{visibleError}</p> : null}
              </div>
            </fieldset>
          </div>
        </div>
        <div className="flex w-full shrink-0 flex-col pt-5">
          <div className="flex w-full flex-wrap items-center justify-end gap-3">
            {creating ? <OcButton className="shrink-0 rounded-full! px-4" disabled={pending} onClick={onCancel} size="md" variant="secondary">Cancel</OcButton> : null}
            <MessageTooltip label={draftError}>
              <span className="inline-flex">
                <OcButton className="shrink-0 rounded-full! px-4" disabled={blocked} size="md" type="submit" variant="primary">
                  {pending ? "Saving…" : creating ? "Create" : "Save"}
                </OcButton>
              </span>
            </MessageTooltip>
          </div>
        </div>
      </form>
      <button
        aria-label="Close dialog"
        className="no-drag absolute top-4 right-3.5 flex size-8 cursor-interaction items-center justify-center rounded-full leading-none text-tertiary hover:bg-primary-ghost-hover hover:text-default focus:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-0 disabled:opacity-40"
        disabled={pending}
        onClick={onCancel}
        type="button"
      >
        <Glyph name="close" size={16} />
      </button>
      {ruleOpen ? <CronRuleDialog cron={draft.cron ?? ""} onClose={() => setRuleOpen(false)} onSave={(cron) => { setDraft({ ...draft, type: "cron", cron }); setRuleOpen(false); }} /> : null}
    </OcDialog>
  );
}

function ScheduleSection({ draft, onChange, onEditRule, scopeOptions }: {
  draft: ScheduledEditorDraft;
  onChange: (draft: ScheduledEditorDraft) => void;
  onEditRule: () => void;
  scopeOptions: ScheduledScopeOption[];
}) {
  const mode = repeatMode(draft);
  const clock = cronClock(draft);
  const zoneName = timezoneName(draft.timezone);
  const rows: ReactNode[] = [
    <ScheduleFormRow control={<RepeatSwitch checked={mode !== "once"} onChange={(repeat) => onChange(withRepeat(draft, repeat))} />} key="repeat-task" label="Repeat task" />,
  ];
  if (mode === "once") {
    const at = draft.dueAt ? new Date(draft.dueAt) : new Date();
    const date = localDate(at);
    rows.push(
      <ScheduleFormRow control={<ScheduleFormDate ariaLabel="Scheduled date" onChange={(value) => onChange({ ...draft, dueAt: localInstant(value, at.getHours(), at.getMinutes()) })} value={date} />} key="date" label="Date" />,
      <ScheduleFormRow control={<TimeMenu hour={at.getHours()} minute={at.getMinutes()} onSelect={(hour, minute) => onChange({ ...draft, dueAt: localInstant(date, hour, minute) })} zone={timezoneName("local", at.getTime())} />} key="start" label="Start time" />,
    );
  } else {
    rows.push(<ScheduleFormRow control={(
      <ScheduleFormMenu<RepeatMode>
        ariaLabel="Repeat schedule"
        onSelect={(next) => { onChange(withRepeatMode(draft, next)); if (next === "custom") onEditRule(); }}
        options={REPEAT_OPTIONS}
        value={mode}
      />
    )} key="repeat" label="Repeat" />);
    if (mode === "weekly") {
      rows.push(<ScheduleFormRow control={<ScheduleFormMenu<number> ariaLabel="Day of the week" onSelect={(weekday) => onChange(withCronClock(draft, { weekday }))} options={WEEKDAY_OPTIONS} value={clock.weekday} />} key="day" label="On" labelSizing="content" />);
    }
    if (mode === "daily" || mode === "weekdays" || mode === "weekly") {
      rows.push(<ScheduleFormRow control={<TimeMenu hour={clock.hour} minute={clock.minute} onSelect={(hour, minute) => onChange(withCronClock(draft, { hour, minute }))} zone={zoneName} />} key="time" label="Time" />);
    }
    if (mode === "hourly") {
      rows.push(<ScheduleFormRow control={<ScheduleFormNumber ariaLabel="Minute past the hour" min={0} onChange={(minute) => onChange(withCronClock(draft, { minute: Math.min(59, minute) }))} value={clock.minute} />} key="minute" label="At minute" />);
    }
    if (mode === "interval") {
      const seconds = draft.intervalSeconds ?? 3600;
      const unit = intervalUnit(seconds);
      const unitSeconds = INTERVAL_UNITS.find((entry) => entry.value === unit)?.seconds ?? 1;
      rows.push(<ScheduleFormRow control={(
        <ScheduleFormNumber
          ariaLabel="Repeat interval"
          decimal={unit === "seconds"}
          min={unit === "seconds" ? 0.001 : 1}
          onChange={(value) => onChange({ ...draft, intervalSeconds: value * unitSeconds })}
          suffix={<ScheduleFormMenu<IntervalUnit> ariaLabel="Interval unit" onSelect={(next) => onChange({ ...draft, intervalSeconds: seconds / unitSeconds * (INTERVAL_UNITS.find((entry) => entry.value === next)?.seconds ?? 1) })} options={INTERVAL_UNITS.map((entry) => ({ id: entry.value, label: unitLabel(entry.label, seconds / unitSeconds) }))} value={unit} />}
          value={seconds / unitSeconds}
        />
      )} key="every" label="Every" />);
    }
    if (mode === "inactivity") rows.push(...inactivityRows(draft, onChange, scopeOptions));
    if (mode === "custom") {
      rows.push(<ScheduleFormRow control={<OcButton className="rounded-full!" onClick={onEditRule} size="xs" variant="ghost">Edit rule</OcButton>} key="rule" label={validCron(draft.cron ?? "") ? describeCron(draft.cron ?? "") : "Advanced schedule"} />);
    }
  }
  return <ScheduleFormGroup variant="input">{rows}</ScheduleFormGroup>;
}

function inactivityRows(draft: ScheduledEditorDraft, onChange: (draft: ScheduledEditorDraft) => void, scopeOptions: ScheduledScopeOption[]): ReactNode[] {
  const inactivity = draft.inactivity ?? { seconds: 3600, interface_match: {} };
  const unit = quietPeriodUnit(inactivity.seconds);
  const unitSeconds = QUIET_PERIOD_UNITS.find((entry) => entry.value === unit)?.seconds ?? 60;
  const scopes = [{ id: "", label: "All work" }, ...scopeOptions.map((option) => ({ id: option.eeId, label: option.name })),
    ...(inactivity.within_ee && !scopeOptions.some((option) => option.eeId === inactivity.within_ee) ? [{ id: inactivity.within_ee, label: inactivity.within_ee }] : [])];
  return [
    <ScheduleFormRow control={(
      <ScheduleFormNumber
        ariaLabel="Quiet period"
        min={1}
        onChange={(value) => onChange({ ...draft, inactivity: { ...inactivity, seconds: value * unitSeconds } })}
        suffix={<ScheduleFormMenu<QuietPeriodUnit> ariaLabel="Quiet period unit" onSelect={(next) => onChange({ ...draft, inactivity: { ...inactivity, seconds: inactivity.seconds / unitSeconds * (QUIET_PERIOD_UNITS.find((entry) => entry.value === next)?.seconds ?? 60) } })} options={QUIET_PERIOD_UNITS.map((entry) => ({ id: entry.value, label: unitLabel(entry.label.toLocaleLowerCase(), inactivity.seconds / unitSeconds) }))} value={unit} />}
        value={inactivity.seconds / unitSeconds}
      />
    )} key="quiet" label="Quiet for" />,
    <ScheduleFormRow control={(
      <ScheduleFormMenu<string>
        ariaLabel="Scope"
        onSelect={(eeId) => onChange({ ...draft, inactivity: { seconds: inactivity.seconds, interface_match: inactivity.interface_match, ...(inactivity.exclude_interface_match ? { exclude_interface_match: inactivity.exclude_interface_match } : {}), ...(eeId ? { within_ee: eeId } : {}) } })}
        options={scopes}
        value={inactivity.within_ee ?? ""}
      />
    )} key="scope" label="Counts work in" />,
  ];
}

function AdvancedSection({ creating, draft, onChange, onChooseTemplate, source, templates }: {
  creating: boolean;
  draft: ScheduledEditorDraft;
  onChange: (draft: ScheduledEditorDraft) => void;
  onChooseTemplate: (interfaceId: string) => void;
  source: ScheduledInterfaceProjection;
  templates: ScheduledInterfaceProjection[];
}) {
  const regionId = useId();
  const [expanded, setExpanded] = useState(false);
  const timeBased = draft.type === "cron" || draft.type === "interval";
  const templateOptions = useMemo(() => templates.map((template) => ({ id: template.interfaceId, label: template.displayName })), [templates]);
  return (
    <div className="flex flex-col">
      <button
        aria-controls={regionId}
        aria-expanded={expanded}
        className="no-drag flex shrink-0 cursor-interaction items-center gap-1 self-start rounded-md! border-0 px-2! py-1! text-sm leading-[18px] whitespace-nowrap text-secondary select-none hover:bg-primary-ghost-hover focus:outline-hidden focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-0"
        id={`${regionId}-trigger`}
        onClick={() => setExpanded(!expanded)}
        type="button"
      >
        Advanced
        <span className={cx("inline-flex motion-safe:transition-transform motion-safe:duration-basic motion-safe:ease-enter-snappy", expanded && "-rotate-180")}>
          <Glyph className="icon-xs" name="chevronDown" size={16} />
        </span>
      </button>
      {expanded ? (
        <div aria-labelledby={`${regionId}-trigger`} className="grid overflow-hidden" id={regionId} role="region">
          <div className="min-h-0 overflow-hidden">
            <div className="flex flex-col gap-3 pt-3 [&>*]:rounded-2xl [&>*]:border [&>*]:border-default">
              {creating ? <ScheduleFormRow control={<ScheduleFormMenu<string> ariaLabel="Trigger template" onSelect={onChooseTemplate} options={templateOptions} value={source.interfaceId} />} label="Template" /> : null}
              <ScheduleFormRow control={creating ? (
                <input
                  aria-label="Interface id"
                  className="min-w-0 flex-1 bg-transparent text-end text-form-body text-secondary outline-none placeholder:text-tertiary focus:text-default"
                  onChange={(event) => onChange({ ...draft, interfaceId: event.target.value.trim().toLowerCase() })}
                  placeholder="world_daily_brief"
                  spellCheck={false}
                  value={draft.interfaceId}
                />
              ) : <ScheduleFormValue>{draft.interfaceId}</ScheduleFormValue>} label="Interface" />
              {timeBased ? (
                <ScheduleFormRow control={(
                  <input
                    aria-label="Time zone"
                    className="min-w-0 flex-1 bg-transparent text-end text-form-body text-secondary outline-none placeholder:text-tertiary focus:text-default"
                    onChange={(event) => onChange({ ...draft, timezone: event.target.value })}
                    placeholder="local, UTC, or Europe/Kyiv"
                    spellCheck={false}
                    value={draft.timezone}
                  />
                )} label="Time zone" />
              ) : null}
              {timeBased ? <ScheduleFormRow control={<RepeatSwitch checked={draft.fireOnStart} label="Run once on start" onChange={(fireOnStart) => onChange({ ...draft, fireOnStart })} />} label="Run once on start" labelSizing="content" /> : null}
              <div className="flex flex-col [&>*:not(:last-child)]:relative [&>*:not(:last-child)]:after:pointer-events-none [&>*:not(:last-child)]:after:absolute [&>*:not(:last-child)]:after:inset-x-5 [&>*:not(:last-child)]:after:bottom-0 [&>*:not(:last-child)]:after:h-px [&>*:not(:last-child)]:after:bg-[var(--color-border-subtle)] [&>*:not(:last-child)]:after:content-['']">
                <ScheduleFormRow control={<ScheduleFormValue>{source.targets.length ? source.targets.map((target) => target.title).join(", ") : "No task yet"}</ScheduleFormValue>} label="Sends to" />
                {source.reminderEnv ? <ScheduleFormRow control={<ScheduleFormValue>{source.reminderEnv}</ScheduleFormValue>} label="Environment override" /> : null}
              </div>
            </div>
          </div>
        </div>
      ) : null}
    </div>
  );
}

function RepeatSwitch({ checked, label = "Repeat task", onChange }: { checked: boolean; label?: string; onChange: (checked: boolean) => void }) {
  return <OcSwitch checked={checked} label={label} onCheckedChange={onChange} />;
}

function TimeMenu({ hour, minute, onSelect, zone }: { hour: number; minute: number; onSelect: (hour: number, minute: number) => void; zone: string }) {
  const value = hour * 60 + minute;
  const options = useMemo(() => {
    const times = [...new Set([...QUARTER_HOURS.map((entry) => entry.hour * 60 + entry.minute), value])].sort((left, right) => left - right);
    return times.map((id) => ({ id, label: clockLabel(Math.floor(id / 60), id % 60) }));
  }, [value]);
  return <ScheduleFormMenu<number> ariaLabel="Time" onSelect={(next) => onSelect(Math.floor(next / 60), next % 60)} options={options} value={value} valueLabel={`${clockLabel(hour, minute)}${zone ? ` ${zone}` : ""}`} />;
}

function CronRuleDialog({ cron, onClose, onSave }: { cron: string; onClose: () => void; onSave: (cron: string) => void }) {
  const titleId = useId();
  const [value, setValue] = useState(cron);
  const valid = validCron(value);
  return (
    <OcDialog ariaLabelledBy={titleId} onClose={onClose} open size="compact">
      <form onSubmit={(event) => { event.preventDefault(); if (valid) onSave(value.trim()); }}>
        <OcDialogHeader description="Use a cron expression to define a custom schedule" title="Edit schedule rule" titleId={titleId} />
        <div className="px-6 pb-4">
          <OcTextField
            autoFocus
            error={value.trim() && !valid ? "Enter a valid cron expression" : undefined}
            label="Cron"
            onChange={(event) => setValue(event.target.value)}
            placeholder="0 9 * * 1-5"
            showLabel
            spellCheck={false}
            value={value}
          />
        </div>
        <OcDialogFooter>
          <OcButton onClick={onClose} variant="ghost">Cancel</OcButton>
          <OcButton disabled={!valid} type="submit" variant="primary">Save</OcButton>
        </OcDialogFooter>
      </form>
    </OcDialog>
  );
}

function unitLabel(plural: string, count: number): string {
  return count === 1 ? plural.replace(/s$/, "") : plural;
}

function localDate(at: Date): string {
  const year = at.getFullYear();
  const month = String(at.getMonth() + 1).padStart(2, "0");
  const day = String(at.getDate()).padStart(2, "0");
  return `${year}-${month}-${day}`;
}

function localInstant(date: string, hour: number, minute: number): string {
  const [year, month, day] = date.split("-").map(Number);
  return new Date(year, month - 1, day, hour, minute, 0, 0).toISOString();
}
