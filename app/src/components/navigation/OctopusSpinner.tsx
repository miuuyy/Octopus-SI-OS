import { useCallback, useState } from "react";
import { cubicCirclePath, threeQuarterRingPath } from "../icon-glyphs/geometric-primitives";

type DocumentObserverEntry = {
  observer: IntersectionObserver;
  targets: Map<Element, (intersecting: boolean) => void>;
};

const documentObservers = new WeakMap<Document, DocumentObserverEntry>();

function observeSpinnerVisibility(
  element: HTMLElement,
  onVisibilityChange: (intersecting: boolean) => void,
): (() => void) | undefined {
  if (typeof IntersectionObserver === "undefined") {
    onVisibilityChange(true);
    return undefined;
  }
  const doc = element.ownerDocument;
  let entry = documentObservers.get(doc);
  if (!entry) {
    const targets = new Map<Element, (intersecting: boolean) => void>();
    const observer = new IntersectionObserver((records) => {
      for (const record of records) {
        targets.get(record.target)?.(record.isIntersecting);
      }
    });
    entry = { observer, targets };
    documentObservers.set(doc, entry);
  }
  onVisibilityChange(false);
  entry.targets.set(element, onVisibilityChange);
  entry.observer.observe(element);
  return () => {
    const current = documentObservers.get(doc);
    if (!current) return;
    current.targets.delete(element);
    current.observer.unobserve(element);
    if (current.targets.size === 0) {
      current.observer.disconnect();
      documentObservers.delete(doc);
    }
  };
}

function attachSynchronizedSpin(element: HTMLSpanElement | null): (() => void) | void {
  if (!element) return;
  const doc = element.ownerDocument;
  let intersecting = false;
  const isActive = () => intersecting && element.isConnected && !doc.hidden;

  const syncPlayState = () => {
    const active = isActive();
    element.style.animationPlayState = active ? "running" : "paused";
    if (!active) return;
    queueMicrotask(() => {
      if (!isActive() || typeof element.getAnimations !== "function") return;
      const animation = element
        .getAnimations()
        .find((candidate) => typeof CSSAnimation !== "undefined" && candidate instanceof CSSAnimation && candidate.animationName === "spin");
      if (animation?.playState !== "running") return;
      animation.ready.then(
        () => {
          if (isActive() && !animation.pending && animation.playState === "running") {
            animation.startTime = 0;
          }
        },
        () => {},
      );
    });
  };

  const onAnimationStart = (event: AnimationEvent) => {
    if (event.target === element && event.animationName === "spin") {
      syncPlayState();
    }
  };

  const unobserve = observeSpinnerVisibility(element, (nextIntersecting) => {
    intersecting = nextIntersecting;
    syncPlayState();
  });

  doc.addEventListener("visibilitychange", syncPlayState);
  element.addEventListener("animationstart", onAnimationStart);

  return () => {
    intersecting = false;
    unobserve?.();
    element.style.removeProperty("animation-play-state");
    doc.removeEventListener("visibilitychange", syncPlayState);
    element.removeEventListener("animationstart", onAnimationStart);
  };
}

export function OctopusSpinner({ className, duration }: { className?: string; duration?: number }) {
  const [animationDelay] = useState(() => `-${Date.now() % (duration ?? 1000)}ms`);
  const ref = useCallback((node: HTMLSpanElement | null) => attachSynchronizedSpin(node), []);
  return (
    <span
      ref={ref}
      aria-hidden="true"
      data-loading-indicator=""
      className="inline-flex h-fit w-fit items-center justify-center leading-none contain-layout contain-paint contain-style motion-safe:animate-spin"
      style={{ animationDelay, animationDuration: duration == null ? undefined : `${duration}ms` }}
    >
      <svg className={className} width="24" height="24" viewBox="0 0 24 24" fill="none">
        <path d={cubicCirclePath(12, 12, 8, 8 * 4 * (Math.sqrt(2) - 1) / 3) + cubicCirclePath(12, 12, 6, 8 * (Math.sqrt(2) - 1))} fill="currentColor" fillRule="evenodd" opacity=".3" />
        <path d={threeQuarterRingPath(12, 12, 8, 6)} fill="currentColor" />
      </svg>
    </span>
  );
}
