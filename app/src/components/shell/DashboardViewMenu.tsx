import { useMemo, useRef, useState } from "react";
import type { ClientSurfaceDescriptor } from "../../shell/surfaceTypes";
import { isFixedDashboardSurfaceId } from "../../shell/clientWorkspaces";
import { navigationSectionForSurface } from "../../shell/navigation";
import { OcMenuItem, OcMenuSeparator } from "../../design-system/components";
import { OcDropdown } from "../../design-system/OcDropdown";
import { Glyph } from "../IconGlyphs";
import { ShellTooltip } from "./ShellTooltip";

export function DashboardViewMenu({ activeSurfaceId, pinnedSurfaceIds, surfaces, onPinnedSurfaceIdsChange, onSelect }: {
  activeSurfaceId: string;
  pinnedSurfaceIds: readonly string[];
  surfaces: readonly ClientSurfaceDescriptor[];
  onPinnedSurfaceIdsChange: (surfaceIds: string[]) => void;
  onSelect: (surfaceId: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const trigger = useRef<HTMLButtonElement>(null);
  const views = useMemo(() => {
    const pinned = new Map(pinnedSurfaceIds.map((id, index) => [id, index]));
    return surfaces.filter((surface) => navigationSectionForSurface(surface) === "dashboard")
      .sort((left, right) => {
        const leftFixed = isFixedDashboardSurfaceId(left.id);
        const rightFixed = isFixedDashboardSurfaceId(right.id);
        if (leftFixed !== rightFixed) return leftFixed ? -1 : 1;
        if (!leftFixed) {
          const pinOrder = (pinned.get(left.id) ?? Infinity) - (pinned.get(right.id) ?? Infinity);
          if (Number.isFinite(pinOrder) && pinOrder !== 0) return pinOrder;
          if (pinned.has(left.id) !== pinned.has(right.id)) return pinned.has(left.id) ? -1 : 1;
        }
        return left.order - right.order || left.label.localeCompare(right.label);
      });
  }, [pinnedSurfaceIds, surfaces]);
  const activeView = views.find((view) => view.id === activeSurfaceId);
  const pinned = pinnedSurfaceIds.includes(activeSurfaceId);
  const MenuSeparator = OcMenuSeparator ?? (() => (
    <div
      className="oc-menu-separator w-full px-[var(--app-menu-separator-inset,var(--padding-row-x))] py-[var(--app-menu-separator-gutter,var(--spacing))]"
      role="separator"
    >
      <div className="h-px w-full bg-border" />
    </div>
  ));

  return <span className="dashboard-view-menu no-drag" onPointerDown={(event) => event.stopPropagation()}>
    <ShellTooltip label="Switch Dashboard view" disabled={open}>
      <button
        aria-label="Switch Dashboard view"
        className="app-titlebar-action no-drag"
        onClick={() => setOpen(!open)}
        ref={trigger}
        type="button"
      ><Glyph name="chevronDown" size={16} /></button>
    </ShellTooltip>
    <OcDropdown anchorRef={trigger} aria-label="Dashboard views" contentWidth="menuWide" open={open} onOpenChange={setOpen} sideOffset={6}>
      {views.map((view) => <OcMenuItem
        aria-checked={view.id === activeSurfaceId}
        key={view.id}
        label={view.label}
        leadingIcon={view.glyph}
        onClick={() => { setOpen(false); onSelect(view.id); }}
        role="menuitemradio"
        selected={view.id === activeSurfaceId}
      />)}
      {activeView && !isFixedDashboardSurfaceId(activeSurfaceId) ? <>
        <MenuSeparator />
        <OcMenuItem
          label={`${pinned ? "Unpin" : "Pin"} ${activeView.label}`}
          leadingIcon="pin"
          onClick={() => {
            setOpen(false);
            onPinnedSurfaceIdsChange(pinned
              ? pinnedSurfaceIds.filter((id) => id !== activeSurfaceId)
              : [...pinnedSurfaceIds, activeSurfaceId]);
          }}
          role="menuitem"
        />
      </> : null}
    </OcDropdown>
  </span>;
}
