import { OcSwitch } from "../../design-system/components";
import { Glyph } from "../IconGlyphs";
import { projectLabel, type ApplicationProject, type ApplicationProjectState, type ApplicationRecord } from "./applicationModel";

export type ApplicationAvailabilityChange = { available: boolean; selected: boolean };

export function ApplicationAvailabilityList({
  application,
  onChange,
  projects,
  rootEeId,
  savingEeId,
}: {
  application: ApplicationRecord;
  onChange: (state: ApplicationProjectState, next: ApplicationAvailabilityChange) => void;
  projects: ApplicationProject[];
  rootEeId: string;
  savingEeId: string;
}) {
  const projectById = new Map(projects.map((project) => [project.ee_id, project]));
  const states = [...application.project_states].sort((left, right) => Number(right.ee_id === rootEeId) - Number(left.ee_id === rootEeId));
  return (
    <div className="flex flex-col overflow-hidden rounded-2xl border border-default px-4 py-1" style={{ backgroundColor: "var(--color-background-panel, var(--color-background-primary-soft-alpha))" }}>
      <div className="flex items-center gap-3 pt-2 pb-1 text-xs text-secondary select-none">
        <span className="min-w-0 flex-1" />
        <span className="w-16 shrink-0 text-center">Available</span>
        <span className="w-16 shrink-0 text-center">New tasks</span>
      </div>
      {states.map((state) => {
        const name = projectLabel(projectById.get(state.ee_id), state.ee_id, rootEeId);
        const saving = savingEeId === state.ee_id;
        const blocked = Boolean(savingEeId);
        return (
          <div className="flex min-h-10 items-center gap-3 border-b border-default py-2 last:border-none" key={state.ee_id}>
            <span className="flex size-7 shrink-0 items-center justify-center rounded-lg bg-token-foreground/5 text-token-text-secondary">
              <Glyph name={state.ee_id === rootEeId ? "home" : "project"} size={14} />
            </span>
            <span className="min-w-0 flex-1">
              <span className="block truncate text-sm text-default">{name}</span>
              {!state.ceiling_allowed && !state.policy_allowed ? <span className="block truncate text-xs text-secondary">Not supported here</span> : null}
            </span>
            <span className="flex w-16 shrink-0 justify-center">
              <OcSwitch
                checked={state.policy_allowed}
                disabled={blocked || saving || (!state.ceiling_allowed && !state.policy_allowed)}
                label={`Available in ${name}`}
                onCheckedChange={(available) => onChange(state, { available, selected: available ? state.selected_by_default : false })}
              />
            </span>
            <span className="flex w-16 shrink-0 justify-center">
              <OcSwitch
                checked={state.selected_by_default}
                disabled={blocked || saving || !state.policy_allowed}
                label={`Add to new tasks in ${name}`}
                onCheckedChange={(selected) => onChange(state, { available: state.policy_allowed, selected })}
              />
            </span>
          </div>
        );
      })}
    </div>
  );
}
