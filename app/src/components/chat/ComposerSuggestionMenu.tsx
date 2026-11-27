import { useEffect, useLayoutEffect, useRef, useState, type CSSProperties, type ReactNode, type RefObject } from "react";
import { Glyph } from "../IconGlyphs";

export type SuggestionMatchSegment = {
  text: string;
  isMatch: boolean;
};

export function findFuzzyMatchOffsets(text: string, query: string): Array<{ startOffset: number; endOffset: number }> | null {
  const needle = query.trim().toLocaleLowerCase();
  if (!needle) return [];
  const haystack = text.toLocaleLowerCase();
  const directIndex = haystack.indexOf(needle);
  if (directIndex >= 0) {
    return [{ startOffset: directIndex, endOffset: directIndex + needle.length }];
  }
  const offsets: Array<{ startOffset: number; endOffset: number }> = [];
  let cursor = 0;
  for (const char of needle) {
    const found = haystack.indexOf(char, cursor);
    if (found < 0) return null;
    const prev = offsets[offsets.length - 1];
    if (prev && prev.endOffset === found) prev.endOffset = found + 1;
    else offsets.push({ startOffset: found, endOffset: found + 1 });
    cursor = found + 1;
  }
  return offsets;
}

export function highlightSuggestionMatch(text: string, query: string): SuggestionMatchSegment[] {
  const chars = Array.from(text);
  const needle = query.trim();
  if (!needle) return chars.map((ch) => ({ text: ch, isMatch: true }));
  const lowerText = text.toLocaleLowerCase();
  const lowerNeedle = needle.toLocaleLowerCase();
  const direct = lowerText.indexOf(lowerNeedle);
  if (direct >= 0) {
    const startChar = Array.from(lowerText.slice(0, direct)).length;
    const endChar = startChar + Array.from(lowerNeedle).length;
    return chars.map((ch, idx) => ({ text: ch, isMatch: idx >= startChar && idx < endChar }));
  }
  const offsets = findFuzzyMatchOffsets(text, needle) ?? [];
  let codeUnitIndex = 0;
  return chars.map((ch) => {
    const pos = codeUnitIndex;
    codeUnitIndex += ch.length;
    const isMatch = offsets.some(({ startOffset, endOffset }) => pos >= startOffset && pos < endOffset);
    return { text: ch, isMatch };
  });
}

export function scoreSuggestionQuery(query: string, fields: readonly (string | undefined)[]): number {
  const needle = query.trim().toLocaleLowerCase();
  if (!needle) return 0;
  let best = -1;
  for (let fieldIndex = 0; fieldIndex < fields.length; fieldIndex += 1) {
    const raw = fields[fieldIndex];
    if (!raw) continue;
    const value = raw.toLocaleLowerCase();
    const fieldBias = fieldIndex === 0 ? 40 : fieldIndex === 1 ? 30 : 10;
    if (value === needle) {
      best = Math.max(best, 400 + fieldBias);
      continue;
    }
    if (value.startsWith(needle)) {
      best = Math.max(best, 300 + fieldBias - Math.min(40, value.length - needle.length));
      continue;
    }
    const direct = value.indexOf(needle);
    if (direct >= 0) {
      const boundaryBonus = direct === 0 || /[\s/_.-]/.test(value[direct - 1] ?? "") ? 40 : 0;
      best = Math.max(best, 200 + fieldBias + boundaryBonus - Math.min(40, direct));
      continue;
    }
    const offsets = findFuzzyMatchOffsets(value, needle);
    if (offsets && offsets.length > 0) {
      const span = offsets[offsets.length - 1].endOffset - offsets[0].startOffset;
      const contiguousBonus = Math.max(0, 30 - (offsets.length - 1) * 6);
      const compactBonus = Math.max(0, 40 - (span - needle.length) * 2);
      best = Math.max(best, 60 + fieldBias + contiguousBonus + compactBonus);
    }
  }
  return best;
}

export function SuggestionHighlightedText({ text, query }: { text: string; query: string }) {
  if (!query.trim()) return <>{text}</>;
  const segments = highlightSuggestionMatch(text, query);
  return (
    <>
      {segments.map((segment, index) => (
        <span
          className={segment.isMatch || /^\s+$/.test(segment.text)
            ? undefined
            : "text-[var(--color-codex-description,var(--color-text-description))]"}
          key={index}
        >
          {segment.text}
        </span>
      ))}
    </>
  );
}

export function useComposerTopMenuMaxHeight(
  element: HTMLElement | null,
  enabled: boolean,
  placement: "top" | "bottom" = "top",
) {
  useLayoutEffect(() => {
    if (!enabled || !element || typeof window === "undefined") return;
    let rafId: number | null = null;
    let previous = "";
    const update = () => {
      const rect = element.getBoundingClientRect();
      const zoom = window.octopusTransport?.windowZoomFactor?.() || 1;
      const available = placement === "top"
        ? Math.max(120, Math.floor((rect.height === 0 ? rect.top : rect.bottom) / zoom - 46 - 8))
        : Math.max(120, Math.floor((window.innerHeight - rect.top) / zoom - 8));
      const next = `${available}px`;
      if (next !== previous) {
        previous = next;
        element.style.setProperty("--composer-top-menu-max-height", next);
      }
    };
    const schedule = () => {
      if (rafId !== null) return;
      rafId = window.requestAnimationFrame(() => {
        rafId = null;
        update();
      });
    };
    update();
    const observer = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(schedule);
    observer?.observe(document.documentElement);
    window.addEventListener("resize", schedule);
    window.addEventListener("scroll", schedule, true);
    return () => {
      if (rafId !== null) window.cancelAnimationFrame(rafId);
      observer?.disconnect();
      window.removeEventListener("resize", schedule);
      window.removeEventListener("scroll", schedule, true);
      element.style.removeProperty("--composer-top-menu-max-height");
    };
  }, [element, enabled, placement]);
}

export function ComposerSuggestionMenu({
  ariaLabel,
  children,
  className = "",
  containerRef,
  dataAttribute,
  fadeMaskVariant = "both",
  header,
  ignoreOutsideRefs,
  listId,
  listRef,
  listRole = "listbox",
  onDismiss,
  onScroll,
  zIndexClassName = "z-50",
}: {
  ariaLabel?: string;
  children: ReactNode;
  className?: string;
  containerRef?: RefObject<HTMLDivElement | null>;
  dataAttribute?: Record<string, string | undefined>;
  fadeMaskVariant?: "both" | "bottom" | "top";
  header?: ReactNode;
  ignoreOutsideRefs?: ReadonlyArray<RefObject<HTMLElement | null>>;
  listId?: string;
  listRef?: RefObject<HTMLDivElement | null>;
  listRole?: "listbox" | "menu";
  onDismiss?: () => void;
  onScroll?: (scrolled: boolean) => void;
  zIndexClassName?: string;
}) {
  const internalContainerRef = useRef<HTMLDivElement>(null);
  const resolvedContainerRef = containerRef ?? internalContainerRef;
  const [wrapperEl, setWrapperEl] = useState<HTMLDivElement | null>(null);
  const [scrolled, setScrolled] = useState(false);

  useComposerTopMenuMaxHeight(wrapperEl, true, "top");

  useEffect(() => {
    if (!onDismiss) return;
    const handlePointerDown = (event: globalThis.PointerEvent) => {
      const target = event.target;
      if (!(target instanceof Node)) return;
      if (resolvedContainerRef.current?.contains(target)) return;
      if (ignoreOutsideRefs?.some((ref) => ref.current?.contains(target))) return;
      onDismiss();
    };
    window.addEventListener("pointerdown", handlePointerDown);
    return () => window.removeEventListener("pointerdown", handlePointerDown);
  }, [ignoreOutsideRefs, onDismiss, resolvedContainerRef]);

  const fadeMaskClass = fadeMaskVariant === "bottom"
    ? "vertical-scroll-fade-mask-bottom"
    : fadeMaskVariant === "top"
      ? "vertical-scroll-fade-mask-top"
      : "vertical-scroll-fade-mask";

  return (
    <div
      {...dataAttribute}
      className={`composer-suggestion-overlay absolute right-0 bottom-full left-0 ${zIndexClassName} mb-2 origin-bottom ${className}`}
      ref={(node) => {
        setWrapperEl(node);
        (resolvedContainerRef as { current: HTMLDivElement | null }).current = node;
      }}
    >
      <div
        className="composer-suggestion-surface relative flex w-full flex-col overflow-hidden rounded-[var(--radius-suggestion-menu,var(--radius-2xl))] border border-default bg-surface-elevated-secondary p-1 text-sm text-default shadow-[var(--suggestion-menu-shadow,none)]"
        style={{
          maxHeight: "min(var(--max-height-suggestion-menu, 320px), var(--composer-top-menu-max-height, 320px), calc(100vh - 64px))",
        } as CSSProperties}
      >
        {header ? (
          <div className={scrolled ? "border-b border-default" : undefined}>
            {header}
          </div>
        ) : null}
        <div
          aria-label={ariaLabel}
          className={`${fadeMaskClass} flex w-full min-h-0 flex-1 flex-col overflow-y-auto`}
          data-mention-list-scroll-area=""
          id={listId}
          onScroll={(event) => {
            const nextScrolled = event.currentTarget.scrollTop > 0;
            if (nextScrolled !== scrolled) {
              setScrolled(nextScrolled);
              onScroll?.(nextScrolled);
            }
          }}
          ref={listRef}
          role={listRole}
        >
          {children}
        </div>
      </div>
    </div>
  );
}

export function ComposerSuggestionSectionLabel({ children, sticky = true }: {
  children: ReactNode;
  sticky?: boolean;
}) {
  return (
    <div
      className={`composer-suggestion-section-label shrink-0 px-row-x py-1 text-sm text-[var(--color-codex-description,var(--color-text-description))]${sticky ? " is-sticky sticky top-0 z-10 bg-surface-elevated-secondary" : ""}`}
    >
      {children}
    </div>
  );
}

export function ComposerSuggestionEmpty({ children }: { children: ReactNode }) {
  return (
    <div className="px-row-x py-row-y text-sm text-tertiary">
      {children}
    </div>
  );
}

export function ComposerSuggestionShortcutPill({ children }: { children: ReactNode }) {
  return (
    <kbd className="inline-flex h-4 min-w-4 items-center justify-center rounded-md border-0 bg-current/10 px-1.5 py-0 font-sans text-xs leading-4 text-current shadow-none">
      {children}
    </kbd>
  );
}

export function ComposerSuggestionRow({
  description,
  disabled = false,
  hasSubmenu = false,
  highlighted,
  icon,
  id,
  onHighlight,
  onSelect,
  query = "",
  rightAccessory,
  role = "option",
  scrollIntoViewOnHighlight = false,
  title,
}: {
  description?: string;
  disabled?: boolean;
  hasSubmenu?: boolean;
  highlighted: boolean;
  icon?: ReactNode;
  id?: string;
  onHighlight?: () => void;
  onSelect: () => void;
  query?: string;
  rightAccessory?: ReactNode;
  role?: "option" | "menuitem";
  scrollIntoViewOnHighlight?: boolean;
  title: string;
}) {
  const rowRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    if (!highlighted || !scrollIntoViewOnHighlight) return;
    rowRef.current?.scrollIntoView?.({ block: "nearest" });
  }, [highlighted, scrollIntoViewOnHighlight]);

  const hasTrailing = Boolean(rightAccessory) || hasSubmenu;

  return (
    <button
      aria-disabled={disabled || undefined}
      aria-selected={role === "option" ? highlighted : undefined}
      className={`composer-suggestion-row cursor-interaction flex min-h-[var(--app-menu-item-height,0px)] w-full shrink-0 items-center overflow-hidden rounded-[var(--suggestion-menu-item-radius,var(--radius-xl))] px-row-x py-row-y text-start text-sm text-default outline-hidden focus:bg-[var(--color-background-secondary-ghost-hover,var(--color-token-list-hover-background))] disabled:cursor-default disabled:opacity-45 ${
        highlighted
          ? "bg-[var(--color-background-secondary-ghost-hover,var(--color-token-list-hover-background))] opacity-100"
          : "opacity-75"
      }`}
      data-highlighted={highlighted ? "" : undefined}
      disabled={disabled}
      id={id}
      onClick={onSelect}
      onMouseDown={(event) => event.preventDefault()}
      onMouseMove={() => {
        if (!disabled && !highlighted) onHighlight?.();
      }}
      ref={rowRef}
      role={role}
      type="button"
    >
      <div className="flex w-full min-w-0 items-center gap-2">
        {icon ? (
          <span
            className="inline-flex size-4 flex-none items-center justify-center text-current"
            data-leading-icon-slot="menu"
          >
            {icon}
          </span>
        ) : null}
        <div className="flex w-full min-w-0 flex-1 items-center gap-2">
          <div className={`truncate ${description ? "min-w-0 max-w-[60%]" : "min-w-0 flex-1"}`}>
            {query ? <SuggestionHighlightedText query={query} text={title} /> : title}
          </div>
          {description ? (
            <span className="min-w-0 flex-1 truncate text-sm text-[var(--color-codex-description,var(--color-text-description))]">
              {description}
            </span>
          ) : null}
          {hasTrailing ? (
            <div className="ms-auto flex shrink-0 items-center gap-2">
              {rightAccessory ? <span className="shrink-0 opacity-80">{rightAccessory}</span> : null}
              {hasSubmenu ? (
                <Glyph className="icon-xs shrink-0 -rotate-90 text-tertiary rtl:rotate-90" name="chevronDown" size={16} />
              ) : null}
            </div>
          ) : null}
        </div>
      </div>
    </button>
  );
}
