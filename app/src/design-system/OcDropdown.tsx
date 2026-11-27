import { useEffect, useId, useLayoutEffect, useRef, useState, type CSSProperties, type HTMLAttributes, type RefObject } from "react";
import { createPortal } from "react-dom";
import { useShouldReduceMotion } from "../appearance";
import { OcMenuSurface } from "./components";
import { isolateModalLayer, restoreModalFocus } from "./modalFocus";
import "./oc-dropdown.css";

type Placement = { left: number; top: number; maxHeight: number; maxWidth: number; side: "top" | "bottom" | "left" | "right" };

export type OcDropdownContentWidth =
  | "menu"
  | "menuNarrow"
  | "menuFixed"
  | "menuBounded"
  | "menuWide"
  | "menuReadout"
  | "railMenu"
  | "sidebar"
  | "workspace"
  | "panel"
  | "panelWide"
  | "panelExtraWide"
  | "modelPicker";

export type OcDropdownProps = Omit<HTMLAttributes<HTMLDivElement>, "role"> & {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  anchorRef: RefObject<HTMLElement | null>;
  align?: "start" | "center" | "end";
  side?: "top" | "bottom" | "right";
  contentWidth?: OcDropdownContentWidth;
  sideOffset?: number;
  alignOffset?: number;
  modal?: boolean;
  role?: "menu" | "dialog";
  surface?: "menu" | "opaque";
  scrollFade?: boolean;
  scrollPeek?: boolean;
  onOpenAutoFocus?: (event: Event) => void;
  onCloseAutoFocus?: (event: Event) => void;
};

const itemSelector = '[role="menuitem"], [role="menuitemcheckbox"], [role="menuitemradio"]';
const ENTER_DURATION_MS = 150;
const EXIT_DURATION_MS = 100;

const CONTENT_WIDTH_MAX: Partial<Record<OcDropdownContentWidth, number>> = {
  menuNarrow: 208,
  menuFixed: 220,
  menuBounded: 320,
  menuWide: 240,
  menuReadout: 260,
  railMenu: 228,
  sidebar: 240,
  panel: 280,
  panelWide: 360,
  panelExtraWide: 408,
  modelPicker: 254,
};

type MotionPhase = "idle" | "entering" | "entering-active" | "open" | "exiting" | "exiting-active";

function dropdownTransformOrigin(side: Placement["side"] | undefined, align: "start" | "center" | "end"): string {
  const resolvedSide = side ?? "bottom";
  if (resolvedSide === "right" || resolvedSide === "left") {
    const x = resolvedSide === "right" ? "left" : "right";
    const y = align === "start" ? "top" : align === "end" ? "bottom" : "center";
    return `${x} ${y}`;
  }
  const x = align === "start" ? "left" : align === "end" ? "right" : "center";
  const y = resolvedSide === "top" ? "bottom" : "top";
  return `${x} ${y}`;
}

function availableMenuItem(item: HTMLElement, element: HTMLElement): boolean {
  return item.closest('[data-oc-dropdown]') === element
    && !item.matches(':disabled,[aria-disabled="true"],[hidden],[data-interactive="false"]')
    && !item.closest('[inert],[aria-hidden="true"]');
}

function menuItems(element: HTMLElement): HTMLElement[] {
  return Array.from(element.querySelectorAll<HTMLElement>(itemSelector)).filter((item) => availableMenuItem(item, element));
}

function nextTextMatch(items: readonly string[], search: string, current: string | undefined): string | undefined {
  const query = search.length > 1 && Array.from(search).every((letter) => letter === search[0]) ? search[0] : search;
  const start = Math.max(current ? items.indexOf(current) : -1, 0);
  let ordered = items.map((_, index) => items[(start + index) % items.length]);
  if (query.length === 1) ordered = ordered.filter((item) => item !== current);
  const match = ordered.find((item) => item.toLowerCase().startsWith(query.toLowerCase()));
  return match === current ? undefined : match;
}

function dropdownPlacement(anchor: Pick<DOMRect, "left" | "right" | "top" | "bottom">, width: number, height: number, viewportWidth: number, viewportHeight: number, align: "start" | "center" | "end", sideOffset: number, alignOffset = 0, preferredSide: "top" | "bottom" | "right" = "bottom"): Placement {
  const maxWidth = Math.max(0, viewportWidth - 16);
  if (preferredSide === "right") {
    const right = Math.max(0, viewportWidth - anchor.right - sideOffset - 6);
    const leftSpace = Math.max(0, anchor.left - sideOffset - 6);
    const side = width > right && leftSpace > right ? "left" : "right";
    const availableWidth = Math.min(maxWidth, side === "left" ? leftSpace : right);
    const renderedWidth = Math.min(width, availableWidth);
    const maxHeight = Math.max(0, viewportHeight - 16);
    const renderedHeight = Math.min(height, maxHeight);
    const alignedTop = align === "end" ? anchor.bottom - renderedHeight - alignOffset
      : align === "center" ? (anchor.top + anchor.bottom - renderedHeight) / 2 + alignOffset : anchor.top + alignOffset;
    return {
      left: side === "right" ? anchor.right + sideOffset : anchor.left - renderedWidth - sideOffset,
      top: Math.max(6, Math.min(alignedTop, viewportHeight - renderedHeight - 6)),
      maxHeight,
      maxWidth: availableWidth,
      side,
    };
  }
  const below = Math.max(0, viewportHeight - anchor.bottom - sideOffset - 6);
  const above = Math.max(0, anchor.top - sideOffset - 6);
  const side = preferredSide === "top" ? height > above && below > above ? "bottom" : "top"
    : height > below && above > below ? "top" : "bottom";
  const maxHeight = Math.min(side === "top" ? above : below, Math.max(0, viewportHeight - 16));
  const renderedWidth = Math.min(width, maxWidth);
  const alignedLeft = align === "end" ? anchor.right - renderedWidth - alignOffset : align === "center" ? (anchor.left + anchor.right - renderedWidth) / 2 + alignOffset : anchor.left + alignOffset;
  const left = Math.max(6, Math.min(alignedLeft, viewportWidth - renderedWidth - 6));
  const top = side === "top" ? anchor.top - Math.min(height, maxHeight) - sideOffset : anchor.bottom + sideOffset;
  return { left, top, maxHeight, maxWidth, side };
}

export function OcDropdown({ open, onOpenChange, anchorRef, align = "start", side = "bottom", contentWidth = "menu", sideOffset, alignOffset = 0, modal = false, role = "menu", surface = "menu", scrollFade = false, scrollPeek = false, onOpenAutoFocus, onCloseAutoFocus, className = "", style, children, onKeyDown, onClick, onPointerMove, ...props }: OcDropdownProps) {
  const reducedMotion = useShouldReduceMotion();
  const reducedMotionRef = useRef(reducedMotion);
  reducedMotionRef.current = reducedMotion;
  const generatedId = useId();
  const id = props.id ?? generatedId;
  const contentRef = useRef<HTMLDivElement>(null);
  const layerRef = useRef<HTMLDivElement>(null);
  const [placement, setPlacement] = useState<Placement | null>(null);
  const [motionPhase, setMotionPhase] = useState<MotionPhase>("idle");
  const positioned = placement !== null;
  const wasOpenRef = useRef(false);
  const keyboardRef = useRef(false);
  const searchRef = useRef("");
  const searchTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const restoreFocusRef = useRef(true);
  const closingRef = useRef(false);
  const callbacksRef = useRef({ onOpenChange, onOpenAutoFocus, onCloseAutoFocus });
  callbacksRef.current = { onOpenChange, onOpenAutoFocus, onCloseAutoFocus };

  function close(restoreFocus: boolean) {
    if (closingRef.current) return;
    closingRef.current = true;
    restoreFocusRef.current = restoreFocus;
    callbacksRef.current.onOpenChange(false);
  }

  useEffect(() => {
    const keyboard = () => { keyboardRef.current = true; };
    const pointer = () => { keyboardRef.current = false; };
    document.addEventListener("keydown", keyboard, true);
    document.addEventListener("pointerdown", pointer, true);
    document.addEventListener("pointermove", pointer, true);
    return () => {
      document.removeEventListener("keydown", keyboard, true);
      document.removeEventListener("pointerdown", pointer, true);
      document.removeEventListener("pointermove", pointer, true);
    };
  }, []);

  useLayoutEffect(() => {
    const anchor = anchorRef.current;
    if (!anchor) return;
    const original = new Map(["aria-haspopup", "aria-controls", "aria-expanded", "data-state"].map((name) => [name, anchor.getAttribute(name)]));
    anchor.setAttribute("aria-haspopup", role);
    anchor.setAttribute("aria-expanded", String(open));
    anchor.setAttribute("data-state", open ? "open" : "closed");
    if (open) anchor.setAttribute("aria-controls", id);
    else anchor.removeAttribute("aria-controls");
    const onAnchorKeyDown = (event: KeyboardEvent) => {
      if (role !== "menu" || event.defaultPrevented || event.ctrlKey || event.metaKey || event.shiftKey || event.key !== "ArrowDown" || anchor.matches(':disabled,[aria-disabled="true"]')) return;
      event.preventDefault();
      keyboardRef.current = true;
      callbacksRef.current.onOpenChange(true);
      if (open && contentRef.current) menuItems(contentRef.current)[0]?.focus({ preventScroll: true });
    };
    anchor.addEventListener("keydown", onAnchorKeyDown);
    return () => {
      anchor.removeEventListener("keydown", onAnchorKeyDown);
      for (const [name, value] of original) {
        if (value === null) anchor.removeAttribute(name);
        else anchor.setAttribute(name, value);
      }
    };
  }, [anchorRef, id, open, role]);

  useLayoutEffect(() => {
    if (!open) {
      if (!wasOpenRef.current || !placement || reducedMotionRef.current) {
        wasOpenRef.current = false;
        setMotionPhase("idle");
        setPlacement(null);
        return;
      }
      wasOpenRef.current = false;
      setMotionPhase("exiting");
      let rafId = 0;
      const startActive = () => setMotionPhase("exiting-active");
      if (typeof window !== "undefined" && typeof window.requestAnimationFrame === "function") {
        rafId = window.requestAnimationFrame(startActive);
      } else {
        startActive();
      }
      const timer = setTimeout(() => {
        setMotionPhase("idle");
        setPlacement(null);
      }, EXIT_DURATION_MS);
      return () => {
        if (rafId && typeof window !== "undefined" && typeof window.cancelAnimationFrame === "function") {
          window.cancelAnimationFrame(rafId);
        }
        clearTimeout(timer);
      };
    }
    wasOpenRef.current = true;
    const content = contentRef.current;
    const anchor = anchorRef.current;
    if (!content || !anchor) return;
    closingRef.current = false;
    restoreFocusRef.current = true;
    const update = () => {
      const bounds = anchor.getBoundingClientRect();
      const offset = sideOffset ?? (5 - (window.octopusTransport?.windowZoomFactor?.() ?? 1));
      const next = dropdownPlacement(bounds, content.getBoundingClientRect().width, content.scrollHeight, window.innerWidth, window.innerHeight, align, offset, alignOffset, side);
      const presetMax = CONTENT_WIDTH_MAX[contentWidth];
      if (presetMax !== undefined) next.maxWidth = Math.min(next.maxWidth, presetMax);
      setPlacement((current) => current && Object.keys(next).every((key) => current[key as keyof Placement] === next[key as keyof Placement]) ? current : next);
    };
    update();
    const observer = new ResizeObserver(update);
    observer.observe(anchor);
    observer.observe(content);
    window.addEventListener("resize", update);
    window.addEventListener("scroll", update, true);
    return () => {
      observer.disconnect();
      window.removeEventListener("resize", update);
      window.removeEventListener("scroll", update, true);
    };
  }, [align, alignOffset, anchorRef, contentWidth, open, side, sideOffset]);

  useLayoutEffect(() => {
    if (!open || !positioned) return;
    if (reducedMotion) {
      setMotionPhase("open");
      return;
    }
    setMotionPhase("entering");
    let rafId = 0;
    const activate = () => setMotionPhase("entering-active");
    if (typeof window !== "undefined" && typeof window.requestAnimationFrame === "function") {
      rafId = window.requestAnimationFrame(activate);
    } else {
      activate();
    }
    const timer = setTimeout(() => setMotionPhase("open"), ENTER_DURATION_MS);
    return () => {
      if (rafId && typeof window !== "undefined" && typeof window.cancelAnimationFrame === "function") {
        window.cancelAnimationFrame(rafId);
      }
      clearTimeout(timer);
    };
  }, [open, positioned, reducedMotion]);

  useLayoutEffect(() => {
    if (!open || !positioned) return;
    const content = contentRef.current;
    const anchor = anchorRef.current;
    if (!content) return;
    const event = new Event("ocDropdown.openAutoFocus", { cancelable: true });
    callbacksRef.current.onOpenAutoFocus?.(event);
    if (!event.defaultPrevented) {
      if (role === "menu") for (const item of menuItems(content)) item.tabIndex = -1;
      content.focus({ preventScroll: true });
      if (role === "menu" && keyboardRef.current) menuItems(content)[0]?.focus({ preventScroll: true });
    }
    const outsidePointer = (event: PointerEvent) => {
      if (event.composedPath().includes(content) || event.composedPath().includes(anchor!)) return;
      if (modal) event.preventDefault();
      close(modal && event.button === 0 && !event.ctrlKey);
    };
    const outsideFocus = (event: FocusEvent) => {
      if (event.composedPath().includes(content) || event.composedPath().includes(anchor!)) return;
      if (modal) content.focus({ preventScroll: true });
      else close(false);
    };
    const escape = (event: KeyboardEvent) => {
      if (event.key !== "Escape" || event.defaultPrevented) return;
      if (!(event.target instanceof Node) || !content.contains(event.target) && !anchor?.contains(event.target)) return;
      event.preventDefault();
      event.stopPropagation();
      close(true);
    };
    const blur = () => close(false);
    document.addEventListener("pointerdown", outsidePointer, true);
    document.addEventListener("focusin", outsideFocus);
    document.addEventListener("keydown", escape);
    window.addEventListener("blur", blur);
    const releaseIsolation = modal && layerRef.current ? isolateModalLayer(layerRef.current) : undefined;
    return () => {
      document.removeEventListener("pointerdown", outsidePointer, true);
      document.removeEventListener("focusin", outsideFocus);
      document.removeEventListener("keydown", escape);
      window.removeEventListener("blur", blur);
      releaseIsolation?.();
      searchRef.current = "";
      if (searchTimerRef.current) clearTimeout(searchTimerRef.current);
      const closeEvent = new Event("ocDropdown.closeAutoFocus", { cancelable: true });
      callbacksRef.current.onCloseAutoFocus?.(closeEvent);
      if (!closeEvent.defaultPrevented && restoreFocusRef.current && document.hasFocus()) restoreModalFocus(anchor);
    };
  }, [anchorRef, modal, open, positioned, role]);

  const exiting = !open && (motionPhase === "exiting" || motionPhase === "exiting-active");
  if (!open && !exiting) return null;
  return createPortal(
    <div className={modal && open ? "oc-dropdown-layer is-modal" : "oc-dropdown-layer"} data-settings-layer="true" ref={layerRef} style={{ zIndex: anchorRef.current?.closest('[data-oc-dialog]') ? 1200 : 300 }}>
      <OcMenuSurface
        {...props}
        id={id}
        aria-hidden={exiting || undefined}
        aria-modal={open && role === "dialog" && modal || undefined}
        aria-orientation={open && role === "menu" ? "vertical" : undefined}
        className={`oc-dropdown${scrollFade ? " vertical-scroll-fade-mask" : ""}${className ? ` ${className}` : ""}`}
        data-oc-dropdown={exiting ? undefined : ""}
        data-align={align}
        data-side={placement?.side ?? "bottom"}
        data-state={exiting ? "closed" : "open"}
        data-entering={motionPhase === "entering" ? "" : undefined}
        data-entering-active={motionPhase === "entering-active" ? "" : undefined}
        data-exiting={motionPhase === "exiting" ? "" : undefined}
        data-exiting-active={motionPhase === "exiting-active" ? "" : undefined}
        data-content-width={contentWidth}
        data-scroll-peek={scrollPeek ? "true" : undefined}
        inert={exiting ? true : undefined}
        role={exiting ? undefined : role}
        surface={surface}
        surfaceRef={contentRef}
        tabIndex={exiting ? undefined : -1}
        style={{
          ...style,
          left: placement?.left,
          top: placement?.top,
          maxHeight: placement?.maxHeight,
          maxWidth: placement?.maxWidth,
          visibility: positioned ? "visible" : "hidden",
          "--radix-dropdown-menu-content-available-width": `${placement?.maxWidth ?? 0}px`,
          "--radix-dropdown-menu-content-available-height": `${placement?.maxHeight ?? 0}px`,
          "--radix-dropdown-menu-content-transform-origin": dropdownTransformOrigin(placement?.side, align),
        } as CSSProperties}
        onFocusCapture={(event) => {
          props.onFocusCapture?.(event);
          if (role !== "menu") return;
          for (const item of menuItems(event.currentTarget)) {
            item.tabIndex = item === event.target ? 0 : -1;
            item.toggleAttribute("data-highlighted", item === event.target);
          }
        }}
        onClick={(event) => {
          onClick?.(event);
          const item = event.target instanceof Element ? event.target.closest<HTMLElement>(itemSelector) : null;
          if (role === "menu" && !event.defaultPrevented && item?.closest('[data-oc-dropdown]') === event.currentTarget && !item.matches(':disabled,[aria-disabled="true"]')) close(true);
        }}
        onPointerMove={(event) => {
          onPointerMove?.(event);
          if (role !== "menu" || event.defaultPrevented || event.pointerType !== "mouse") return;
          const item = event.target instanceof Element ? event.target.closest<HTMLElement>(itemSelector) : null;
          if (item && availableMenuItem(item, event.currentTarget)) item.focus({ preventScroll: true });
          else if (!(event.target instanceof HTMLInputElement)) event.currentTarget.focus({ preventScroll: true });
        }}
        onKeyDown={(event) => {
          onKeyDown?.(event);
          if (event.key === "Escape" && !event.defaultPrevented) {
            event.preventDefault();
            event.stopPropagation();
            close(true);
            return;
          }
          if (role !== "menu" || event.defaultPrevented) return;
          const content = event.currentTarget;
          const items = menuItems(content);
          const index = items.indexOf(document.activeElement as HTMLElement);
          const input = content.querySelector<HTMLInputElement>("input:not(:disabled)");
          if ((event.key === "ArrowUp" || event.key === "Tab" && event.shiftKey) && index === 0 && input) {
            input.focus();
            event.preventDefault();
            event.stopPropagation();
            return;
          }
          if (event.key === "Tab") { event.preventDefault(); return; }
          if (event.target instanceof HTMLInputElement) {
            if (event.key === "ArrowDown" || event.key === "ArrowUp") {
              const item = event.key === "ArrowDown" ? items[0] : items.at(-1);
              if (item) { event.preventDefault(); item.focus(); }
            }
            return;
          }
          if (event.target instanceof HTMLTextAreaElement || event.ctrlKey || event.metaKey || event.altKey) return;
          const next = items.length === 0
            ? null
            : event.key === "Home" || event.key === "PageUp"
              ? 0
              : event.key === "End" || event.key === "PageDown"
                ? items.length - 1
                : event.key === "ArrowDown"
                  ? (index < 0 ? 0 : (index + 1) % items.length)
                  : event.key === "ArrowUp"
                    ? (index < 0 ? items.length - 1 : (index - 1 + items.length) % items.length)
                    : null;
          if (next !== null && !event.shiftKey) {
            event.preventDefault();
            const target = items[next];
            if (target) {
              target.focus({ preventScroll: true });
              target.scrollIntoView?.({ block: "nearest" });
            }
            return;
          }
          if ((event.key === "Enter" || event.key === " " && !searchRef.current) && index >= 0) {
            event.preventDefault();
            items[index].click();
            return;
          }
          if (event.key.length === 1) {
            if (event.key === " ") event.preventDefault();
            searchRef.current += event.key;
            if (searchTimerRef.current) clearTimeout(searchTimerRef.current);
            searchTimerRef.current = setTimeout(() => { searchRef.current = ""; }, 1000);
            const labels = items.map((item) => item.getAttribute("data-text-value") ?? item.textContent?.trim() ?? "");
            const match = nextTextMatch(labels, searchRef.current, labels[index]);
            if (match !== undefined) items[labels.indexOf(match)]?.focus();
          }
        }}
      >{children}</OcMenuSurface>
    </div>,
    document.body,
  );
}
