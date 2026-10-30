import { useEffect, useSyncExternalStore } from "react";
import type { GestureInteraction, NavigationDirection } from "./airHandEngine";
import { emitVoiceSpatialInteraction } from "./client-surfaces/voiceSpatialControl";
import type { OctopusClientAccessProfile } from "./electron";
import { subscribeGestureState } from "./gestureRecognition";
import { subscribeGestureInteractions } from "./gestureShortcuts";
import { IDLE_AIM, nearestSpatialCandidate, rankSpatialCandidates, readingOrder, trackAim, type AimOptions, type AimState, type SpatialRect } from "./gestureSpatialNavigation";
import { clientDialogOwnsCommands } from "./keyboardShortcuts";

export type GestureFocusState = "focus" | "armed" | "confirm" | "reject" | "grab" | "resize";

export type GestureFocusActivity = Readonly<{ gesture: string; outcome: string; at: number }>;

export type GestureFocusView = Readonly<{
  element: HTMLElement | null;
  state: GestureFocusState;
  hint: string;
  target: string;
  bump: NavigationDirection | null;
  last: GestureFocusActivity | null;
}>;

type Target = { element: HTMLElement; rect: SpatialRect };

type Carry = {
  kind: "drag" | "transform";
  documentId: string;
  element: HTMLElement;
  startX: number;
  startY: number;
  stageWidth: number;
  stageHeight: number;
  dismiss: HTMLElement | null;
  title: string;
};

type AncestorBox = { clip: SpatialRect | null; fixed: boolean };

const FOCUS_ATTR = "data-gesture-focused";
const WIDGET_SELECTOR = "[data-surface-document][data-voice-slot]";
const CONTROL_SELECTOR = [
  "button",
  "a[href]",
  "input:not([type='hidden'])",
  "select",
  "textarea",
  "[contenteditable='true']",
  "[role='button']",
  "[role='link']",
  "[role='tab']",
  "[role='menuitem']",
  "[role='menuitemradio']",
  "[role='menuitemcheckbox']",
  "[role='option']",
  "[role='switch']",
  "[role='checkbox']",
  "[role='radio']",
  "[role='treeitem']",
  "[tabindex]:not([tabindex='-1'])",
  "[data-gesture-focus]",
].join(", ");
const BLOCKED_SELECTOR = "[inert], [aria-hidden='true'], [hidden], [data-state='closed'], [data-gesture-ignore], [data-gesture-ring]";
const SCOPE_SELECTOR = "[role='dialog'], [role='menu']";
const CLOSED_SCOPE_SELECTOR = "[inert], [hidden], [aria-hidden='true'], [data-state='closed']";
const TEXT_ENTRY_SELECTOR = "textarea, select, [contenteditable='true'], input:not([type='button']):not([type='submit']):not([type='reset']):not([type='checkbox']):not([type='radio']):not([type='range']):not([type='color']):not([type='file']):not([type='image'])";
const DISABLED_SELECTOR = ":disabled, [aria-disabled='true']";
const IDLE_MS = 6_000;
const CONFIRM_MS = 280;
const REJECT_MS = 360;
const BUMP_MS = 220;
const REFOCUS_MS = 160;
const CARRY_GAIN = 1.6;
const MIN_TARGET_SIZE = 6;
const SCROLL_FRACTION = 0.6;
const DIRECTION_WORDS: Record<NavigationDirection, string> = { left: "left", right: "right", up: "above", down: "below" };
const AIM_OPTIONS: AimOptions = { dwellMs: 110, marginMin: 8, marginMax: 24, marginFraction: 0.35 };
const PRESS_LOOKBACK_MS = 180;
const FOCUS_LOG_MS = 2_000;
const TARGET_TTL_MS = 250;
const EDGE_SCROLL_MS = 450;
const EDGE_LIMIT = 0.001;

let view: GestureFocusView = Object.freeze({ element: null, state: "focus", hint: "", target: "", bump: null, last: null });
const listeners = new Set<() => void>();

function setView(patch: Partial<GestureFocusView>): void {
  view = Object.freeze({ ...view, ...patch });
  for (const listener of [...listeners]) listener();
}

function subscribeView(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

export function useGestureFocusView(): GestureFocusView {
  return useSyncExternalStore(subscribeView, () => view, () => view);
}

export function useGestureFocus(accessProfile: OctopusClientAccessProfile, enabled: boolean, spatialControl: boolean): void {
  useEffect(() => {
    if (!enabled) return;
    const controller = new GestureFocusController(accessProfile !== "live", spatialControl);
    return () => controller.dispose();
  }, [accessProfile, enabled, spatialControl]);
}

class GestureFocusController {
  private pressed: HTMLElement | null = null;
  private carry: Carry | null = null;
  private aim: AimState = IDLE_AIM;
  private aimCandidate: HTMLElement | null = null;
  private targets: { at: number; entered: HTMLElement | null; list: Target[] } | null = null;
  private edge: { direction: NavigationDirection; scrolledAt: number } | null = null;
  private lastCenter: { x: number; y: number } | null = null;
  private focusLog: { element: HTMLElement; at: number }[] = [];
  private idleTimer = 0;
  private timers = new Set<number>();
  private disposers: Array<() => void> = [];

  constructor(acceptTest: boolean, private readonly spatial: boolean) {
    this.disposers.push(subscribeGestureInteractions((interaction, source) => {
      if (source === "test" && !acceptTest) return;
      this.handle(interaction);
    }));
    this.disposers.push(subscribeGestureState((state) => {
      if (state.hands.length > 0) {
        window.clearTimeout(this.idleTimer);
        this.idleTimer = 0;
        return;
      }
      if (this.idleTimer || !view.element || this.carry) return;
      this.idleTimer = window.setTimeout(() => {
        this.idleTimer = 0;
        this.clear();
      }, IDLE_MS);
    }));
    const forget = () => this.clear();
    const invalidate = () => {
      this.targets = null;
    };
    window.addEventListener("pointerdown", forget, true);
    window.addEventListener("keydown", forget, true);
    window.addEventListener("scroll", invalidate, true);
    window.addEventListener("resize", invalidate);
    this.disposers.push(() => {
      window.removeEventListener("pointerdown", forget, true);
      window.removeEventListener("keydown", forget, true);
      window.removeEventListener("scroll", invalidate, true);
      window.removeEventListener("resize", invalidate);
    });
  }

  dispose(): void {
    for (const dispose of this.disposers) dispose();
    this.disposers = [];
    window.clearTimeout(this.idleTimer);
    for (const timer of this.timers) window.clearTimeout(timer);
    this.timers.clear();
    this.clear();
  }

  private handle(interaction: GestureInteraction): void {
    if (interaction.type === "point") this.point(interaction.phase, interaction.x, interaction.y);
    else if (interaction.type === "navigate") this.navigate(interaction.direction);
    else if (interaction.type === "press") this.press();
    else if (interaction.type === "grab") this.grab(interaction.phase, interaction.dx, interaction.dy);
    else if (interaction.type === "release") this.release(interaction.outcome, interaction.dx, interaction.dy);
    else if (interaction.type === "cancel") this.cancel();
    else this.transform(interaction.phase, interaction.scale, interaction.dx, interaction.dy);
  }

  private point(phase: "begin" | "move" | "end", x: number, y: number): void {
    if (this.carry || this.pressed) return;
    if (phase === "end") {
      this.aimCandidate = null;
      this.edge = null;
      return;
    }
    const now = performance.now();
    const current = this.current();
    const entered = current && !current.matches(WIDGET_SELECTOR) ? current.closest<HTMLElement>(WIDGET_SELECTOR) : null;
    const targets = this.cachedTargets(entered, now);
    const state: AimState = {
      current: current ? targets.findIndex((target) => target.element === current) : -1,
      candidate: this.aimCandidate ? targets.findIndex((target) => target.element === this.aimCandidate) : -1,
      since: this.aim.since,
    };
    this.aim = trackAim(state, { x: x * window.innerWidth, y: y * window.innerHeight }, targets.map((target) => target.rect), now, AIM_OPTIONS);
    this.aimCandidate = targets[this.aim.candidate]?.element ?? null;
    const chosen = targets[this.aim.current];
    if (chosen && chosen.element !== current && chosen.element.isConnected) {
      this.focus(chosen.element, { gesture: "Point", outcome: `Focus ${describe(chosen.element)}` }, false);
    }
    this.pushEdge(x, y, now);
  }

  private pushEdge(x: number, y: number, now: number): void {
    const direction: NavigationDirection | null = y <= EDGE_LIMIT ? "up" : y >= 1 - EDGE_LIMIT ? "down" : x <= EDGE_LIMIT ? "left" : x >= 1 - EDGE_LIMIT ? "right" : null;
    if (!direction) {
      this.edge = null;
      return;
    }
    if (this.edge?.direction !== direction) {
      this.edge = { direction, scrolledAt: now };
      return;
    }
    const current = this.current();
    if (now - this.edge.scrolledAt < EDGE_SCROLL_MS || !current) return;
    this.edge = { direction, scrolledAt: now };
    if (!scrollToward(current, direction)) return;
    this.targets = null;
    this.record("Point", `Scrolled ${direction}`);
  }

  private cachedTargets(entered: HTMLElement | null, now: number): Target[] {
    const cache = this.targets;
    if (cache && cache.entered === entered && now - cache.at < TARGET_TTL_MS) return cache.list;
    const list = collectTargets(entered).filter(reachable);
    this.targets = { at: now, entered, list };
    return list;
  }

  private navigate(direction: NavigationDirection): void {
    if (this.carry) return;
    this.pressed = null;
    const current = this.current();
    const entered = current && !current.matches(WIDGET_SELECTOR) ? current.closest<HTMLElement>(WIDGET_SELECTOR) : null;
    const targets = collectTargets(entered);
    if (!current) {
      const start = this.initialTarget(targets);
      if (start) this.focus(start, { gesture: "Point", outcome: `Focus ${describe(start)}` });
      else this.record("Point", "Nothing to focus here");
      return;
    }
    const origin = visibleRect(current, new Map()) ?? toRect(current.getBoundingClientRect());
    for (const index of rankSpatialCandidates(origin, direction, targets.map((target) => target.rect))) {
      const target = targets[index];
      if (!target || target.element === current || !reachable(target)) continue;
      this.focus(target.element, { gesture: "Point", outcome: `Focus ${describe(target.element)}` });
      return;
    }
    if (scrollToward(current, direction)) {
      this.record("Point", `Scrolled ${direction}`);
      return;
    }
    setView({ bump: direction });
    this.later(() => setView({ bump: null }), BUMP_MS);
    this.record("Point", `Nothing ${DIRECTION_WORDS[direction]}`);
  }

  private press(): void {
    const element = this.settledFocus();
    if (!element) {
      this.record("Pinch", "Nothing focused");
      return;
    }
    if (element.matches(DISABLED_SELECTOR)) {
      this.flash("reject", "Unavailable");
      this.record("Pinch", `${describe(element)} is unavailable`);
      return;
    }
    this.pressed = element;
    setView({ state: "armed", hint: element.matches(WIDGET_SELECTOR) ? "Release to open" : "Release to select" });
  }

  private grab(phase: "begin" | "move", dx: number, dy: number): void {
    if (phase === "begin") {
      const pressed = this.pressed;
      const widget = this.spatial && pressed?.isConnected ? movableWidget(pressed) : null;
      const carry = widget ? beginCarry("drag", widget) : null;
      if (!carry || !widget) return;
      this.pressed = null;
      this.carry = carry;
      emitVoiceSpatialInteraction({ clientX: carry.startX, clientY: carry.startY, documentId: carry.documentId, kind: "drag", phase: "begin" });
      this.focus(widget, null);
      setView({ state: "grab", hint: carry.dismiss ? "Moving · throw to hide" : "Moving" });
    }
    const carry = this.carry;
    if (!carry || carry.kind !== "drag") return;
    emitVoiceSpatialInteraction({
      clientX: carry.startX + dx * carry.stageWidth * CARRY_GAIN,
      clientY: carry.startY + dy * carry.stageHeight * CARRY_GAIN,
      documentId: carry.documentId,
      kind: "drag",
      phase: "move",
    });
  }

  private release(outcome: "select" | "drop" | "fling", dx: number, dy: number): void {
    const carry = this.carry;
    if (!carry || carry.kind !== "drag") {
      const element = this.pressed;
      this.pressed = null;
      if (element?.isConnected) this.activate(element);
      else this.settle();
      return;
    }
    const clientX = carry.startX + dx * carry.stageWidth * CARRY_GAIN;
    const clientY = carry.startY + dy * carry.stageHeight * CARRY_GAIN;
    emitVoiceSpatialInteraction({ clientX, clientY, documentId: carry.documentId, kind: "drag", phase: "move" });
    emitVoiceSpatialInteraction({ clientX, clientY, documentId: carry.documentId, kind: "drag", phase: "end" });
    this.carry = null;
    if (outcome === "fling" && carry.dismiss?.isConnected) {
      carry.dismiss.click();
      this.clear();
      this.record("Throw", `Hid ${carry.title}`);
      this.later(() => {
        const targets = collectTargets(null);
        const index = this.lastCenter ? nearestSpatialCandidate(this.lastCenter, targets.map((target) => target.rect)) : -1;
        const next = targets[index];
        if (next && !view.element) this.focus(next.element, null);
      }, REFOCUS_MS);
      return;
    }
    this.record(outcome === "fling" ? "Throw" : "Pinch and move", `Moved ${carry.title}`);
    this.settle();
  }

  private cancel(): void {
    if (!this.pressed) return;
    this.pressed = null;
    this.record("Pinch", "Cancelled");
    this.settle();
  }

  private transform(phase: "begin" | "move" | "end", scale: number, dx: number, dy: number): void {
    if (phase === "begin") {
      this.pressed = null;
      const element = this.current();
      const widget = this.spatial && element ? movableWidget(element) : null;
      const carry = widget ? beginCarry("transform", widget) : null;
      if (!carry || !widget) {
        this.settle();
        this.record("Two-hand pinch", "Focus a Home widget to resize it");
        return;
      }
      this.carry = carry;
      emitVoiceSpatialInteraction({ clientX: carry.startX, clientY: carry.startY, documentId: carry.documentId, kind: "transform", phase: "begin", scale: 1 });
      this.focus(widget, null);
      setView({ state: "resize", hint: "Resize · 100%" });
      return;
    }
    const carry = this.carry;
    if (!carry || carry.kind !== "transform") return;
    const clientX = carry.startX + dx * carry.stageWidth * CARRY_GAIN;
    const clientY = carry.startY + dy * carry.stageHeight * CARRY_GAIN;
    if (phase === "move") {
      emitVoiceSpatialInteraction({ clientX, clientY, documentId: carry.documentId, kind: "transform", phase: "move", scale });
      setView({ hint: `Resize · ${Math.round(scale * 100)}%` });
      return;
    }
    emitVoiceSpatialInteraction({ clientX, clientY, documentId: carry.documentId, kind: "transform", phase: "end", scale });
    this.carry = null;
    this.record("Two-hand pinch", `Resized ${carry.title} to ${Math.round(scale * 100)}%`);
    this.settle();
  }

  private activate(element: HTMLElement): void {
    const name = describe(element);
    if (element.matches(DISABLED_SELECTOR)) {
      this.flash("reject", "Unavailable");
      this.record("Pinch", `${name} is unavailable`);
      return;
    }
    if (element.matches(WIDGET_SELECTOR)) {
      const inside = collectTargets(element).filter((target) => target.element !== element && element.contains(target.element));
      const first = inside[readingOrder(inside.map((target) => target.rect))[0] ?? -1];
      if (!first) {
        this.flash("reject", "Nothing to open");
        this.record("Pinch", `${name} has no controls`);
        return;
      }
      this.focus(first.element, { gesture: "Pinch", outcome: `Opened ${name}` });
      this.flash("confirm", "Opened");
      return;
    }
    if (element.matches(TEXT_ENTRY_SELECTOR)) {
      element.focus({ preventScroll: true });
      this.flash("confirm", "Ready to type");
      this.record("Pinch", `Typing in ${name}`);
      return;
    }
    element.click();
    this.record("Pinch", `Selected ${name}`);
    if (element.isConnected && view.element === element) this.flash("confirm", "Selected");
    else this.settle();
  }

  private current(): HTMLElement | null {
    const element = view.element;
    if (!element?.isConnected || element.closest(BLOCKED_SELECTOR)) return null;
    return commandScope().contains(element) ? element : null;
  }

  private settledFocus(): HTMLElement | null {
    const current = this.current();
    const before = performance.now() - PRESS_LOOKBACK_MS;
    const settled = [...this.focusLog].reverse().find((entry) => entry.at <= before)?.element;
    if (!current || !settled || settled === current || !settled.isConnected || settled.closest(BLOCKED_SELECTOR) || !commandScope().contains(settled)) return current;
    this.focus(settled, null, false);
    return settled;
  }

  private initialTarget(targets: Target[]): HTMLElement | null {
    const active = document.activeElement;
    const focused = targets.find((target) => target.element === active);
    if (focused) return focused.element;
    const point = this.lastCenter ?? { x: window.innerWidth / 2, y: window.innerHeight / 2 };
    return targets[nearestSpatialCandidate(point, targets.map((target) => target.rect))]?.element ?? null;
  }

  private focus(element: HTMLElement, activity: Omit<GestureFocusActivity, "at"> | null, revealTarget = true): void {
    for (const previous of Array.from(document.querySelectorAll(`[${FOCUS_ATTR}]`))) {
      if (previous !== element) previous.removeAttribute(FOCUS_ATTR);
    }
    element.setAttribute(FOCUS_ATTR, "true");
    if (revealTarget) reveal(element);
    const now = performance.now();
    this.focusLog = [...this.focusLog.filter((entry) => now - entry.at < FOCUS_LOG_MS), { element, at: now }];
    const rect = element.getBoundingClientRect();
    this.lastCenter = { x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 };
    setView({
      element,
      state: "focus",
      hint: "",
      target: describe(element),
      bump: null,
      ...(activity ? { last: { gesture: activity.gesture, outcome: activity.outcome, at: Date.now() } } : {}),
    });
  }

  private clear(): void {
    if (this.carry) {
      const carry = this.carry;
      const rect = carry.element.getBoundingClientRect();
      emitVoiceSpatialInteraction({ clientX: rect.left + rect.width / 2, clientY: rect.top + rect.height / 2, documentId: carry.documentId, kind: carry.kind, phase: "end", scale: 1 });
      this.carry = null;
    }
    this.pressed = null;
    this.aim = IDLE_AIM;
    this.aimCandidate = null;
    this.edge = null;
    this.focusLog = [];
    for (const element of Array.from(document.querySelectorAll(`[${FOCUS_ATTR}]`))) element.removeAttribute(FOCUS_ATTR);
    if (view.element || view.hint || view.bump) setView({ element: null, state: "focus", hint: "", target: "", bump: null });
  }

  private settle(): void {
    if (view.state !== "focus" || view.hint) setView({ state: "focus", hint: "" });
  }

  private flash(state: "confirm" | "reject", hint: string): void {
    setView({ state, hint });
    this.later(() => {
      if (view.state === state) this.settle();
    }, state === "confirm" ? CONFIRM_MS : REJECT_MS);
  }

  private record(gesture: string, outcome: string): void {
    setView({ last: { gesture, outcome, at: Date.now() } });
  }

  private later(callback: () => void, delay: number): void {
    const timer = window.setTimeout(() => {
      this.timers.delete(timer);
      callback();
    }, delay);
    this.timers.add(timer);
  }
}

function collectTargets(entered: HTMLElement | null): Target[] {
  const root = commandScope();
  const boxes = new Map<Element, AncestorBox>();
  const elements = new Set<HTMLElement>([
    ...Array.from(root.querySelectorAll<HTMLElement>(WIDGET_SELECTOR)),
    ...Array.from(root.querySelectorAll<HTMLElement>(CONTROL_SELECTOR)),
  ]);
  const targets: Target[] = [];
  for (const element of elements) {
    if (element === entered) continue;
    if (!element.matches(WIDGET_SELECTOR)) {
      const owner = element.closest(WIDGET_SELECTOR);
      if (owner && owner !== entered) continue;
    }
    if (element.closest(BLOCKED_SELECTOR) || element.matches(DISABLED_SELECTOR) || !rendered(element)) continue;
    const rect = visibleRect(element, boxes);
    if (rect) targets.push({ element, rect });
  }
  return targets;
}

function commandScope(): HTMLElement {
  if (!clientDialogOwnsCommands()) return document.body;
  const open = Array.from(document.querySelectorAll<HTMLElement>(SCOPE_SELECTOR)).filter((scope) => !scope.closest(CLOSED_SCOPE_SELECTOR));
  return open[open.length - 1] ?? document.body;
}

function movableWidget(element: HTMLElement): HTMLElement | null {
  return element.closest<HTMLElement>(WIDGET_SELECTOR);
}

function beginCarry(kind: Carry["kind"], widget: HTMLElement): Carry | null {
  const documentId = widget.dataset.surfaceDocument;
  const stage = widget.parentElement;
  if (!documentId || !stage) return null;
  const rect = widget.getBoundingClientRect();
  const stageRect = stage.getBoundingClientRect();
  if (stageRect.width <= 0 || stageRect.height <= 0) return null;
  return {
    kind,
    documentId,
    element: widget,
    startX: rect.left + rect.width / 2,
    startY: rect.top + rect.height / 2,
    stageWidth: stageRect.width,
    stageHeight: stageRect.height,
    dismiss: widget.querySelector<HTMLElement>("[data-surface-dismiss]"),
    title: describe(widget),
  };
}

function rendered(element: HTMLElement): boolean {
  return element.checkVisibility({ checkOpacity: true, checkVisibilityCSS: true, opacityProperty: true, visibilityProperty: true });
}

function visibleRect(element: HTMLElement, boxes: Map<Element, AncestorBox>): SpatialRect | null {
  const box = element.getBoundingClientRect();
  if (box.width < MIN_TARGET_SIZE || box.height < MIN_TARGET_SIZE) return null;
  let rect = intersect(toRect(box), { left: 0, top: 0, right: window.innerWidth, bottom: window.innerHeight });
  if (getComputedStyle(element).position !== "fixed") {
    for (let parent = element.parentElement; rect && parent && parent !== document.body; parent = parent.parentElement) {
      let ancestor = boxes.get(parent);
      if (!ancestor) {
        ancestor = ancestorBox(parent);
        boxes.set(parent, ancestor);
      }
      if (ancestor.clip) rect = intersect(rect, ancestor.clip);
      if (ancestor.fixed) break;
    }
  }
  if (!rect || rect.right - rect.left < MIN_TARGET_SIZE / 2 || rect.bottom - rect.top < MIN_TARGET_SIZE / 2) return null;
  return rect;
}

function ancestorBox(element: Element): AncestorBox {
  const style = getComputedStyle(element);
  const fixed = style.position === "fixed";
  const clipsX = style.overflowX !== "visible";
  const clipsY = style.overflowY !== "visible";
  if (!clipsX && !clipsY) return { clip: null, fixed };
  const box = element.getBoundingClientRect();
  return {
    clip: {
      left: clipsX ? box.left : Number.NEGATIVE_INFINITY,
      right: clipsX ? box.right : Number.POSITIVE_INFINITY,
      top: clipsY ? box.top : Number.NEGATIVE_INFINITY,
      bottom: clipsY ? box.bottom : Number.POSITIVE_INFINITY,
    },
    fixed,
  };
}

function reachable(target: Target): boolean {
  const hit = document.elementFromPoint((target.rect.left + target.rect.right) / 2, (target.rect.top + target.rect.bottom) / 2);
  if (!hit) return true;
  return hit === target.element || target.element.contains(hit) || hit.contains(target.element);
}

function reveal(element: HTMLElement): void {
  const box = element.getBoundingClientRect();
  const visible = visibleRect(element, new Map());
  if (visible && visible.right - visible.left >= box.width - 1 && visible.bottom - visible.top >= box.height - 1) return;
  element.scrollIntoView({ block: "nearest", inline: "nearest", behavior: scrollBehavior() });
}

function scrollToward(element: HTMLElement, direction: NavigationDirection): boolean {
  const vertical = direction === "up" || direction === "down";
  const forward = direction === "down" || direction === "right";
  for (let parent = element.parentElement; parent; parent = parent.parentElement) {
    const overflow = vertical ? getComputedStyle(parent).overflowY : getComputedStyle(parent).overflowX;
    if (overflow !== "auto" && overflow !== "scroll") continue;
    const limit = vertical ? parent.scrollHeight - parent.clientHeight : parent.scrollWidth - parent.clientWidth;
    const position = vertical ? parent.scrollTop : parent.scrollLeft;
    if (limit <= 1 || (forward ? position >= limit - 1 : position <= 1)) continue;
    const amount = (vertical ? parent.clientHeight : parent.clientWidth) * SCROLL_FRACTION * (forward ? 1 : -1);
    parent.scrollBy(vertical ? { top: amount, behavior: scrollBehavior() } : { left: amount, behavior: scrollBehavior() });
    return true;
  }
  return false;
}

function scrollBehavior(): ScrollBehavior {
  return window.matchMedia("(prefers-reduced-motion: reduce)").matches ? "auto" : "smooth";
}

function describe(element: HTMLElement): string {
  const labelledBy = (element.getAttribute("aria-labelledby") ?? "")
    .split(/\s+/)
    .map((id) => (id ? document.getElementById(id)?.textContent ?? "" : ""))
    .join(" ");
  const field = element instanceof HTMLInputElement || element instanceof HTMLTextAreaElement
    ? `${element.labels?.[0]?.textContent ?? ""}` || element.placeholder
    : "";
  const candidates = [
    element.getAttribute("aria-label"),
    labelledBy,
    element.getAttribute("data-gesture-label"),
    field,
    element.getAttribute("title"),
    element.textContent,
  ];
  for (const candidate of candidates) {
    const text = (candidate ?? "").replace(/\s+/g, " ").trim();
    if (text) return text.length > 48 ? `${text.slice(0, 47)}…` : text;
  }
  return "Unnamed control";
}

function toRect(box: DOMRect): SpatialRect {
  return { left: box.left, top: box.top, right: box.right, bottom: box.bottom };
}

function intersect(left: SpatialRect, right: SpatialRect): SpatialRect | null {
  const rect = {
    left: Math.max(left.left, right.left),
    top: Math.max(left.top, right.top),
    right: Math.min(left.right, right.right),
    bottom: Math.min(left.bottom, right.bottom),
  };
  return rect.right > rect.left && rect.bottom > rect.top ? rect : null;
}
