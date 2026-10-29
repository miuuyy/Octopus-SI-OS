import { OcSwitch, cx } from "../../design-system/components";
import { Glyph } from "../IconGlyphs";
import { projectLabel, type ApplicationProject, type ApplicationProjectState, type ApplicationRecord } from "./applicationModel";

export type ApplicationAvailabilityChange = { available: boolean; selected: boolean };

export function ApplicationAvailabilityList({
  application,
  onChange,
  projects,
  rootEeId,
  savingEeId,
  variant,
}: {
  application: ApplicationRecord;
  onChange: (state: ApplicationProjectState, next: ApplicationAvailabilityChange) => void;
  projects: ApplicationProject[];
  rootEeId: string;
  savingEeId: string;
  variant: "dialog" | "page";
}) {
  const projectById = new Map(projects.map((project) => [project.ee_id, project]));
  const states = [...application.project_states].sort((left, right) => Number(right.ee_id === rootEeId) - Number(left.ee_id === rootEeId));
  return (
    <div className={cx("flex flex-col", variant === "dialog" ? "rounded-2xl border border-default px-4 py-1" : "gap-1")}>
      <div className={cx("flex items-center gap-3 text-xs text-secondary select-none", variant === "dialog" ? "pt-2 pb-1" : "px-2 pb-1")}>
        <span className="min-w-0 flex-1" />
        <span className="w-16 shrink-0 text-center">Available</span>
        <span className="w-16 shrink-0 text-center">New tasks</span>
      </div>
      {states.map((state) => {
        const name = projectLabel(projectById.get(state.ee_id), state.ee_id, rootEeId);
        const saving = savingEeId === state.ee_id;
        const blocked = Boolean(savingEeId);
        return (
          <div
            className={cx(
              "flex min-h-10 items-center gap-3",
              variant === "dialog" ? "border-b border-default py-2 last:border-none" : "rounded-lg px-2 py-1.5 hover:bg-token-foreground/3",
            )}
            key={state.ee_id}
          >
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
