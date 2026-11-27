import { useEffect, useId, useLayoutEffect, useRef, useState, type ComponentProps, type KeyboardEvent } from "react";
import { createPortal } from "react-dom";
import { OcMenuItem, OcMenuSeparator, OcMenuSurface } from "../../design-system/components";
import { restoreModalFocus } from "../../design-system/modalFocus";
import { formatKeyboardShortcuts, keyboardShortcutFromEvent } from "../../keyboardShortcuts";
import type { ClientTaskActionsMenuRequest } from "../../shell/taskActionsMenu";
import { Glyph } from "../IconGlyphs";
import "./task-actions.css";

type Point = { x: number; y: number };
const SUBMENU_SAFE_TRIANGLE_PAD_PX = 8;
const SUBMENU_GRACE_TIMEOUT_MS = 260;

function cross(a: Point, b: Point, c: Point): number {
  return (a.x - c.x) * (b.y - c.y) - (b.x - c.x) * (a.y - c.y);
}

function pointInSafeTriangle(point: Point, origin: Point, destinationRect: DOMRect, side: "left" | "right"): boolean {
  if (destinationRect.width <= 0 || destinationRect.height <= 0) return false;
  const edgeX = side === "right" ? destinationRect.left : destinationRect.right;
  const endA: Point = { x: edgeX, y: destinationRect.top - SUBMENU_SAFE_TRIANGLE_PAD_PX };
  const endB: Point = { x: edgeX, y: destinationRect.bottom + SUBMENU_SAFE_TRIANGLE_PAD_PX };
  const d1 = cross(point, origin, endA);
  const d2 = cross(point, endA, endB);
  const d3 = cross(point, endB, origin);
  const hasNeg = d1 < 0 || d2 < 0 || d3 < 0;
  const hasPos = d1 > 0 || d2 > 0 || d3 > 0;
  return !(hasNeg && hasPos);
}

function nextTextMatch(items: readonly string[], search: string, current: string | undefined): string | undefined {
  const query = search.length > 1 && Array.from(search).every((letter) => letter === search[0]) ? search[0] : search;
  const start = Math.max(current ? items.indexOf(current) : -1, 0);
  let ordered = items.map((_, index) => items[(start + index) % items.length]);
  if (query.length === 1) ordered = ordered.filter((item) => item !== current);
  const match = ordered.find((item) => item.toLowerCase().startsWith(query.toLowerCase()));
  return match === current ? undefined : match;
}

function DefaultMenuSeparator() {
  return (
    <div
      className="oc-menu-separator w-full px-[var(--app-menu-separator-inset,var(--padding-row-x))] py-[var(--app-menu-separator-gutter,var(--spacing))]"
      role="separator"
    >
      <div className="h-px w-full bg-border" />
    </div>
  );
}

export type TaskAction = {
  id: string;
  label: string;
  description?: string;
  type?: "normal" | "radio";
  enabled?: boolean;
  checked?: boolean;
  icon?: ComponentProps<typeof OcMenuItem>["leadingIcon"];
  tone?: "danger";
  shortcuts?: readonly string[];
  submenu?: TaskAction[];
  onSelect?: () => void;
} | { type: "separator" };

export function TaskActionsPopover({ anchor, actions, title, onClose, onSelect }: {
  anchor: ClientTaskActionsMenuRequest;
  actions: TaskAction[];
  title: string;
  onClose: () => void;
  onSelect: (id: string) => void;
}) {
  const id = useId();
  const layerRef = useRef<HTMLDivElement>(null);
  const [path, setPath] = useState<string[]>([]);
  const focusDepthRef = useRef<number | null>(0);
  const restoreFocusRef = useRef(true);
  const previousFocusRef = useRef(anchor.anchorElement ?? (document.activeElement instanceof HTMLElement ? document.activeElement : null));
  const lastPointerRef = useRef<Point | null>(null);
  const submenuGraceRef = useRef<{ depth: number; origin: Point; timer: ReturnType<typeof setTimeout> | null } | null>(null);
  const searchRef = useRef("");
  const searchTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const callbacks = useRef({ onClose, onSelect });
  callbacks.current = { onClose, onSelect };
  const MenuSeparator = OcMenuSeparator ?? DefaultMenuSeparator;
  const levels = [{ title: `Actions for ${title}`, actions }];
  for (const selected of path) {
    const parent = levels.at(-1)!.actions.find((item) => item.type !== "separator" && item.id === selected);
    if (!parent || parent.type === "separator" || parent.enabled === false || !parent.submenu) break;
    levels.push({ title: parent.label, actions: parent.submenu });
  }
  const panel = (depth: number) => layerRef.current?.querySelector<HTMLElement>(`[data-task-menu-depth="${depth}"]`);
  const items = (depth: number) => [...(panel(depth)?.querySelectorAll<HTMLButtonElement>('[role^="menuitem"]:not(:disabled)') ?? [])];
  const parentItem = (depth: number) => items(depth).find((item) => item.dataset.taskAction === path[depth]);
  const clearSubmenuGrace = () => {
    if (submenuGraceRef.current?.timer) clearTimeout(submenuGraceRef.current.timer);
    submenuGraceRef.current = null;
  };
  const focusRow = (depth: number, target: HTMLButtonElement | undefined) => {
    for (const candidate of items(depth)) {
      const active = candidate === target;
      candidate.tabIndex = active ? 0 : -1;
      candidate.toggleAttribute("data-highlighted", active);
    }
    target?.focus({ preventScroll: true });
    target?.scrollIntoView?.({ block: "nearest" });
  };

  useLayoutEffect(() => {
    const layer = layerRef.current;
    if (!layer) return;
    const place = () => {
      if (anchor.anchorElement && !anchor.anchorElement.isConnected) { callbacks.current.onClose(); return; }
      const panels = [...layer.querySelectorAll<HTMLElement>('[data-task-menu-depth]')];
      for (const [depth, current] of panels.entries()) {
        current.style.maxWidth = `${Math.max(0, window.innerWidth - 16)}px`;
        current.style.maxHeight = `${Math.max(0, window.innerHeight - 16)}px`;
        const bounds = current.getBoundingClientRect();
        let left: number, top: number;
        let originX = "right";
        let originY = "top";
        if (depth === 0) {
          const trigger = anchor.anchorElement?.getBoundingClientRect();
          left = anchor.pointer?.x ?? (trigger?.right ?? anchor.anchorRight) - bounds.width;
          top = anchor.pointer?.y ?? (trigger?.bottom ?? anchor.anchorBottom) + 4;
          originX = anchor.pointer ? "left" : "right";
        } else {
          const trigger = parentItem(depth - 1)?.getBoundingClientRect();
          if (!trigger) continue;
          const opensRight = trigger.right + 6 + bounds.width <= window.innerWidth - 8;
          left = opensRight ? trigger.right + 6 : trigger.left - bounds.width - 6;
          top = trigger.top - 4;
          originX = opensRight ? "left" : "right";
        }
        const clampedLeft = Math.max(8, Math.min(left, window.innerWidth - bounds.width - 8));
        const clampedTop = Math.max(8, Math.min(top, window.innerHeight - bounds.height - 8));
        if (clampedTop < top - 4) originY = "bottom";
        current.style.left = `${clampedLeft}px`;
        current.style.top = `${clampedTop}px`;
        current.style.transformOrigin = `${originX} ${originY}`;
      }
    };
    place();
    const observer = new ResizeObserver(place);
    layer.querySelectorAll<HTMLElement>('[data-task-menu-depth]').forEach((element) => observer.observe(element));
    if (anchor.anchorElement) observer.observe(anchor.anchorElement);
    window.addEventListener("resize", place);
    window.addEventListener("scroll", place, true);
    if (focusDepthRef.current !== null) {
      focusRow(focusDepthRef.current, items(focusDepthRef.current)[0]);
      focusDepthRef.current = null;
    }
    return () => { observer.disconnect(); window.removeEventListener("resize", place); window.removeEventListener("scroll", place, true); };
  }, [anchor, path]);

  useEffect(() => {
    const trigger = anchor.anchorElement;
    const original = new Map(["aria-expanded", "aria-controls", "data-state"].map((name) => [name, trigger?.getAttribute(name)]));
    trigger?.setAttribute("aria-expanded", "true");
    trigger?.setAttribute("aria-controls", id);
    trigger?.setAttribute("data-state", "open");
    const outside = (event: Event) => {
      if (event.composedPath().includes(layerRef.current!) || trigger && event.composedPath().includes(trigger)) return;
      restoreFocusRef.current = false;
      callbacks.current.onClose();
    };
    const blur = () => { restoreFocusRef.current = false; callbacks.current.onClose(); };
    document.addEventListener("pointerdown", outside, true);
    document.addEventListener("focusin", outside);
    window.addEventListener("blur", blur);
    return () => {
      clearSubmenuGrace();
      if (searchTimerRef.current) clearTimeout(searchTimerRef.current);
      document.removeEventListener("pointerdown", outside, true);
      document.removeEventListener("focusin", outside);
      window.removeEventListener("blur", blur);
      for (const [name, value] of original) {
        if (value == null) trigger?.removeAttribute(name);
        else trigger?.setAttribute(name, value);
      }
      if (restoreFocusRef.current && document.hasFocus()) restoreModalFocus(previousFocusRef.current);
    };
  }, [anchor, id]);

  const expand = (depth: number, action: Exclude<TaskAction, { type: "separator" }>, focus: boolean) => {
    if (action.enabled === false) return;
    clearSubmenuGrace();
    if (focus) focusDepthRef.current = depth + 1;
    setPath((current) => current[depth] === action.id && current.length === depth + 1
      ? current : [...current.slice(0, depth), action.id]);
    if (focus && path[depth] === action.id) focusRow(depth + 1, items(depth + 1)[0]);
  };
  const collapse = (depth: number) => {
    clearSubmenuGrace();
    const parent = parentItem(depth - 1);
    setPath((current) => current.slice(0, depth - 1));
    focusRow(depth - 1, parent);
  };
  const activateHover = (depth: number, action: Exclude<TaskAction, { type: "separator" }>, target: HTMLButtonElement) => {
    clearSubmenuGrace();
    focusRow(depth, target);
    if (action.submenu) expand(depth, action, false);
    else setPath((current) => current.slice(0, depth));
  };
  const handleItemPointer = (depth: number, action: Exclude<TaskAction, { type: "separator" }>, target: HTMLButtonElement, point: Point) => {
    const openChildId = path[depth];
    const submenuEl = openChildId && openChildId !== action.id ? panel(depth + 1) : null;
    const parentEl = openChildId && openChildId !== action.id ? parentItem(depth) : null;
    if (submenuEl && parentEl) {
      const origin = submenuGraceRef.current?.depth === depth ? submenuGraceRef.current.origin : lastPointerRef.current;
      const submenuRect = submenuEl.getBoundingClientRect();
      const parentRect = parentEl.getBoundingClientRect();
      const side: "left" | "right" = submenuRect.left >= parentRect.left ? "right" : "left";
      if (origin && pointInSafeTriangle(point, origin, submenuRect, side)) {
        if (submenuGraceRef.current?.timer) clearTimeout(submenuGraceRef.current.timer);
        submenuGraceRef.current = {
          depth,
          origin: point,
          timer: setTimeout(() => {
            if (target.isConnected) activateHover(depth, action, target);
          }, SUBMENU_GRACE_TIMEOUT_MS),
        };
        return;
      }
    }
    activateHover(depth, action, target);
  };
  const keyDown = (event: KeyboardEvent<HTMLDivElement>, depth: number) => {
    if (event.defaultPrevented) return;
    const shortcut = keyboardShortcutFromEvent(event);
    const shortcutAction = (entries: TaskAction[]): string | undefined => {
      for (const action of entries) {
        if (action.type === "separator" || action.enabled === false) continue;
        if (shortcut && action.shortcuts?.includes(shortcut) && !action.submenu) return action.id;
        const nested = action.submenu && shortcutAction(action.submenu);
        if (nested) return nested;
      }
    };
    const actionId = shortcutAction(actions);
    if (actionId && !event.repeat && !event.nativeEvent.isComposing) {
      event.preventDefault(); event.stopPropagation(); callbacks.current.onSelect(actionId); return;
    }
    const rows = items(depth), current = document.activeElement as HTMLButtonElement, index = rows.indexOf(current);
    if (event.key === "Escape" || event.key === "ArrowLeft" && depth > 0) {
      event.preventDefault(); event.stopPropagation();
      if (depth > 0) collapse(depth); else callbacks.current.onClose();
      return;
    }
    if (event.key === "Tab") { event.preventDefault(); callbacks.current.onClose(); return; }
    if (event.key === "ArrowRight") {
      const action = levels[depth].actions.find((item) => item.type !== "separator" && item.id === current?.dataset.taskAction);
      if (action && action.type !== "separator" && action.submenu) { event.preventDefault(); expand(depth, action, true); }
      return;
    }
    if (!rows.length) return;
    const next = event.key === "Home" || event.key === "PageUp" ? 0 : event.key === "End" || event.key === "PageDown" ? rows.length - 1
      : event.key === "ArrowDown" ? (index + 1) % rows.length : event.key === "ArrowUp" ? index < 0 ? rows.length - 1 : (index + rows.length - 1) % rows.length : null;
    if (next !== null) { event.preventDefault(); focusRow(depth, rows[next]); return; }
    if (event.key.length === 1 && !event.metaKey && !event.ctrlKey && !event.altKey) {
      if (event.key === " ") event.preventDefault();
      searchRef.current += event.key;
      if (searchTimerRef.current) clearTimeout(searchTimerRef.current);
      searchTimerRef.current = setTimeout(() => { searchRef.current = ""; }, 1000);
      const labels = rows.map((row) => row.getAttribute("data-text-value") ?? row.textContent?.trim() ?? "");
      const match = nextTextMatch(labels, searchRef.current, labels[index]);
      if (match !== undefined) focusRow(depth, rows[labels.indexOf(match)]);
    }
  };

  return createPortal(<div
    className="task-actions-layer"
    data-settings-layer="true"
    onPointerMove={(event) => {
      if (event.pointerType === "mouse") lastPointerRef.current = { x: event.clientX, y: event.clientY };
    }}
    ref={layerRef}
  >
    {levels.map((level, depth) => <OcMenuSurface key={depth} id={depth === 0 ? id : `${id}-${depth}`}
      aria-label={level.title} aria-orientation="vertical" className="task-actions-menu" data-task-menu-depth={depth}
      role="menu" tabIndex={-1}
      onPointerEnter={() => {
        if (depth > 0 && submenuGraceRef.current && submenuGraceRef.current.depth < depth) clearSubmenuGrace();
      }}
      onKeyDown={(event) => keyDown(event, depth)}>
      {level.actions.map((action, index) => action.type === "separator"
        ? <MenuSeparator key={`separator:${index}`} />
        : <OcMenuItem key={action.id} label={action.label} description={action.description} leadingIcon={action.icon}
          disabled={action.enabled === false} role={action.type === "radio" ? "menuitemradio" : "menuitem"}
          aria-checked={action.type === "radio" ? action.checked : undefined} data-task-action={action.id}
          aria-haspopup={action.submenu ? "menu" : undefined} aria-expanded={action.submenu ? path[depth] === action.id : undefined}
          aria-controls={action.submenu && path[depth] === action.id ? `${id}-${depth + 1}` : undefined}
          tone={action.tone} tabIndex={-1}
          trailing={action.submenu ? <Glyph className="-rotate-90" name="chevronDown" size={16} /> : action.checked ? <Glyph name="check" size={16} /> : action.shortcuts?.length ? <span className="task-action-shortcut">{formatKeyboardShortcuts(action.shortcuts)}</span> : undefined}
          onPointerEnter={(event) => {
            if (event.pointerType !== "mouse" || action.enabled === false) return;
            handleItemPointer(depth, action, event.currentTarget, { x: event.clientX, y: event.clientY });
          }}
          onPointerMove={(event) => {
            if (event.pointerType !== "mouse" || action.enabled === false) return;
            const point = { x: event.clientX, y: event.clientY };
            if (path[depth] && path[depth] !== action.id) {
              handleItemPointer(depth, action, event.currentTarget, point);
            } else if (document.activeElement !== event.currentTarget) {
              focusRow(depth, event.currentTarget);
            }
          }}
          onPointerLeave={(event) => {
            if (event.pointerType !== "mouse") return;
            if (action.submenu && path[depth] === action.id) {
              clearSubmenuGrace();
              submenuGraceRef.current = { depth, origin: { x: event.clientX, y: event.clientY }, timer: null };
            }
          }}
          onClick={() => action.submenu ? expand(depth, action, true) : callbacks.current.onSelect(action.id)} />)}
    </OcMenuSurface>)}
  </div>, document.body);
}
