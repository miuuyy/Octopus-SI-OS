import { useEffect, useLayoutEffect, useRef, useState, type RefObject } from "react";
import { restoreModalFocus } from "./modalFocus";

const MENU_ITEM_SELECTOR = '[role="menuitem"], [role="menuitemcheckbox"], [role="menuitemradio"]';
const ENTER_DURATION_MS = 150;
const EXIT_DURATION_MS = 100;

export type FloatingMenuMotionPhase = "idle" | "entering" | "entering-active" | "open" | "exiting" | "exiting-active";

function prefersReducedFloatingMenuMotion(): boolean {
  if (typeof document === "undefined" || typeof window === "undefined") return true;
  const root = document.documentElement;
  if (root.classList.contains("reduce-motion") || root.dataset.reducedMotion === "true") return true;
  if (root.classList.contains("full-motion")) return false;
  if (typeof window.matchMedia !== "function") return true;
  return window.matchMedia("(prefers-reduced-motion: reduce)").matches;
}

function nextTextMatch(items: readonly string[], search: string, current: string | undefined): string | undefined {
  const query = search.length > 1 && Array.from(search).every((letter) => letter === search[0]) ? search[0] : search;
  const start = Math.max(current ? items.indexOf(current) : -1, 0);
  let ordered = items.map((_, index) => items[(start + index) % items.length]);
  if (query.length === 1) ordered = ordered.filter((item) => item !== current);
  const match = ordered.find((item) => item.toLowerCase().startsWith(query.toLowerCase()));
  return match === current ? undefined : match;
}

/** Focus, dismissal, typeahead, motion and viewport bounds for menus opened at a pointer position. */
export function useFloatingMenu(
  open: boolean,
  menuRef: RefObject<HTMLDivElement | null>,
  onClose: () => void,
  position: { left: number; top: number } | null,
): { mounted: boolean; exiting: boolean; motionPhase: FloatingMenuMotionPhase } {
  const closeRef = useRef(onClose);
  closeRef.current = onClose;
  const [motionPhase, setMotionPhase] = useState<FloatingMenuMotionPhase>(() => open ? "open" : "idle");
  const wasOpenRef = useRef(false);
  const searchRef = useRef("");
  const searchTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const left = position?.left ?? 0;
  const top = position?.top ?? 0;

  useLayoutEffect(() => {
    const menu = menuRef.current;
    if (!open) {
      if (!wasOpenRef.current || prefersReducedFloatingMenuMotion()) {
        wasOpenRef.current = false;
        setMotionPhase("idle");
        return;
      }
      wasOpenRef.current = false;
      setMotionPhase("exiting");
      if (menu) menu.setAttribute("data-floating-menu", "exiting");
      let rafId = 0;
      const activateExit = () => {
        setMotionPhase("exiting-active");
        if (menuRef.current) menuRef.current.setAttribute("data-floating-menu", "exiting-active");
      };
      if (typeof window !== "undefined" && typeof window.requestAnimationFrame === "function") {
        rafId = window.requestAnimationFrame(activateExit);
      } else {
        activateExit();
      }
      const timer = setTimeout(() => {
        setMotionPhase("idle");
        if (menuRef.current) menuRef.current.removeAttribute("data-floating-menu");
      }, EXIT_DURATION_MS);
      return () => {
        if (rafId && typeof window !== "undefined" && typeof window.cancelAnimationFrame === "function") {
          window.cancelAnimationFrame(rafId);
        }
        clearTimeout(timer);
      };
    }
    wasOpenRef.current = true;
    if (!menu) return;
    const place = () => {
      menu.style.maxHeight = `${Math.max(0, window.innerHeight - 8)}px`;
      menu.style.maxWidth = `${Math.max(0, window.innerWidth - 8)}px`;
      menu.style.overflowY = "auto";
      const bounds = menu.getBoundingClientRect();
      const clampedLeft = Math.max(4, Math.min(left, window.innerWidth - bounds.width - 4));
      const clampedTop = Math.max(4, Math.min(top, window.innerHeight - bounds.height - 4));
      menu.style.left = `${clampedLeft}px`;
      menu.style.top = `${clampedTop}px`;
      const originX = clampedLeft < left - 4 ? "right" : "left";
      const originY = clampedTop < top - 4 ? "bottom" : "top";
      menu.style.transformOrigin = `${originX} ${originY}`;
    };
    place();
    const observer = new ResizeObserver(place);
    observer.observe(menu);
    window.addEventListener("resize", place);
    return () => { observer.disconnect(); window.removeEventListener("resize", place); };
  }, [left, menuRef, open, top]);

  useLayoutEffect(() => {
    const menu = menuRef.current;
    if (!open || !menu) return;
    if (prefersReducedFloatingMenuMotion()) {
      menu.setAttribute("data-floating-menu", "open");
      setMotionPhase("open");
      return;
    }
    menu.setAttribute("data-floating-menu", "entering");
    setMotionPhase("entering");
    let rafId = 0;
    const activateEnter = () => {
      if (menuRef.current) menuRef.current.setAttribute("data-floating-menu", "entering-active");
      setMotionPhase("entering-active");
    };
    if (typeof window !== "undefined" && typeof window.requestAnimationFrame === "function") {
      rafId = window.requestAnimationFrame(activateEnter);
    } else {
      activateEnter();
    }
    const timer = setTimeout(() => {
      if (menuRef.current) menuRef.current.setAttribute("data-floating-menu", "open");
      setMotionPhase("open");
    }, ENTER_DURATION_MS);
    return () => {
      if (rafId && typeof window !== "undefined" && typeof window.cancelAnimationFrame === "function") {
        window.cancelAnimationFrame(rafId);
      }
      clearTimeout(timer);
    };
  }, [menuRef, open]);

  useEffect(() => {
    const menu = menuRef.current;
    if (!open || !menu) return;
    const previousFocus = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    let restore = true;
    if (menu.tabIndex < 0) menu.tabIndex = -1;
    const items = () => [...menu.querySelectorAll<HTMLElement>(MENU_ITEM_SELECTOR)]
      .filter((item) => !item.matches(':disabled, [aria-disabled="true"]') && !item.closest('[inert], [hidden], [aria-hidden="true"]'));
    const syncHighlight = (activeItem: HTMLElement | null) => {
      for (const candidate of items()) {
        const active = candidate === activeItem;
        candidate.tabIndex = active ? 0 : -1;
        candidate.toggleAttribute("data-highlighted", active);
      }
    };
    const focus = (item: HTMLElement | undefined) => {
      syncHighlight(item ?? null);
      item?.focus({ preventScroll: true });
      item?.scrollIntoView?.({ block: "nearest" });
    };
    const frame = window.requestAnimationFrame(() => focus(items()[0]));
    const onFocusIn = (event: FocusEvent) => {
      const target = event.target instanceof Element ? event.target.closest<HTMLElement>(MENU_ITEM_SELECTOR) : null;
      const available = items();
      if (target && available.includes(target)) syncHighlight(target);
      else syncHighlight(null);
    };
    const onPointerMove = (event: PointerEvent) => {
      if (event.defaultPrevented || event.pointerType !== "mouse") return;
      const target = event.target instanceof Element ? event.target.closest<HTMLElement>(MENU_ITEM_SELECTOR) : null;
      const available = items();
      if (target && available.includes(target)) {
        if (document.activeElement !== target) focus(target);
      } else {
        syncHighlight(null);
        if (menu.contains(document.activeElement) && document.activeElement !== menu) {
          menu.focus({ preventScroll: true });
        }
      }
    };
    const onPointerLeave = (event: PointerEvent) => {
      if (event.pointerType !== "mouse") return;
      syncHighlight(null);
      if (menu.contains(document.activeElement) && document.activeElement !== menu) {
        menu.focus({ preventScroll: true });
      }
    };
    const dismiss = (event: PointerEvent) => {
      if (menu.contains(event.target as Node)) return;
      restore = false;
      closeRef.current();
    };
    const keydown = (event: KeyboardEvent) => {
      if (event.defaultPrevented || menu.closest('[inert], [hidden], [aria-hidden="true"]')) return;
      if (event.key === "Escape") {
        event.preventDefault();
        closeRef.current();
        return;
      }
      if (!menu.contains(event.target as Node) || event.metaKey || event.ctrlKey || event.altKey) return;
      if (event.key === "Tab") { event.preventDefault(); closeRef.current(); return; }
      const available = items();
      if (!available.length) return;
      const index = available.indexOf(document.activeElement as HTMLElement);
      const next = event.key === "Home" || event.key === "PageUp" ? 0
        : event.key === "End" || event.key === "PageDown" ? available.length - 1
          : event.key === "ArrowDown" ? (index < 0 ? 0 : (index + 1) % available.length)
            : event.key === "ArrowUp" ? (index < 0 ? available.length - 1 : (index - 1 + available.length) % available.length)
              : null;
      if (next !== null) {
        event.preventDefault();
        focus(available[next]);
        return;
      }
      if ((event.key === "Enter" || (event.key === " " && !searchRef.current)) && index >= 0) {
        event.preventDefault();
        available[index]?.click();
        return;
      }
      if (event.key.length === 1 && !(event.target instanceof HTMLInputElement || event.target instanceof HTMLTextAreaElement)) {
        if (event.key === " ") event.preventDefault();
        searchRef.current += event.key;
        if (searchTimerRef.current) clearTimeout(searchTimerRef.current);
        searchTimerRef.current = setTimeout(() => { searchRef.current = ""; }, 1000);
        const labels = available.map((item) => item.getAttribute("data-text-value") ?? item.textContent?.trim() ?? "");
        const match = nextTextMatch(labels, searchRef.current, labels[index]);
        if (match !== undefined) focus(available[labels.indexOf(match)]);
      }
    };
    const blur = () => { restore = false; closeRef.current(); };
    menu.addEventListener("focusin", onFocusIn);
    menu.addEventListener("pointermove", onPointerMove);
    menu.addEventListener("pointerleave", onPointerLeave);
    window.addEventListener("pointerdown", dismiss);
    window.addEventListener("keydown", keydown);
    window.addEventListener("blur", blur);
    return () => {
      window.cancelAnimationFrame(frame);
      menu.removeEventListener("focusin", onFocusIn);
      menu.removeEventListener("pointermove", onPointerMove);
      menu.removeEventListener("pointerleave", onPointerLeave);
      window.removeEventListener("pointerdown", dismiss);
      window.removeEventListener("keydown", keydown);
      window.removeEventListener("blur", blur);
      searchRef.current = "";
      if (searchTimerRef.current) clearTimeout(searchTimerRef.current);
      if (restore && (document.activeElement === document.body || menu.contains(document.activeElement))) restoreModalFocus(previousFocus);
    };
  }, [menuRef, open]);

  const exiting = !open && (motionPhase === "exiting" || motionPhase === "exiting-active");
  return { mounted: open || exiting, exiting, motionPhase };
}
