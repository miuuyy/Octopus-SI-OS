import { useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { Glyph } from "../IconGlyphs";

export function CollapsibleMessageBody({ children, text, maxLines = 20, collapseIndicator = "ellipsis" }: {
  children: ReactNode;
  text: string;
  maxLines?: number;
  collapseIndicator?: "ellipsis" | "fade";
}) {
  const clipRef = useRef<HTMLDivElement>(null);
  const contentRef = useRef<HTMLDivElement>(null);
  const [expandedText, setExpandedText] = useState<string | null>(null);
  const [overflowing, setOverflowing] = useState(false);
  const expanded = expandedText === text;
  const collapsed = overflowing && !expanded;
  const showEllipsis = collapsed && collapseIndicator === "ellipsis" && maxLines > 2;
  const visibleLines = maxLines - Number(showEllipsis);

  useLayoutEffect(() => {
    const content = contentRef.current;
    if (!content) return;
    const measure = () => {
      const style = window.getComputedStyle(content);
      const measuredFontSize = Number.parseFloat(style.fontSize);
      const fontSize = Number.isFinite(measuredFontSize) ? measuredFontSize : 13;
      const measuredLineHeight = Number.parseFloat(style.lineHeight);
      const lineHeight = Number.isFinite(measuredLineHeight) ? measuredLineHeight : fontSize * 1.5;
      const limit = Math.ceil(lineHeight * maxLines);
      setOverflowing(Math.ceil(content.scrollHeight) > limit + 1);
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(content);
    return () => observer.disconnect();
  }, [children, maxLines]);

  useLayoutEffect(() => {
    const clip = clipRef.current;
    const content = contentRef.current;
    if (!collapsed || !clip || !content) return;
    const measure = () => {
      const blocks = clip.querySelectorAll('[data-markdown-copy="code-block"]:not(pre)');
      if (!blocks.length) {
        clip.style.maxHeight = `${visibleLines}lh`;
        return;
      }
      const lineHeight = Number.parseFloat(window.getComputedStyle(clip).lineHeight);
      if (!Number.isFinite(lineHeight)) return;
      const bounds = clip.getBoundingClientRect();
      const scale = clip.offsetHeight === 0 ? 1 : bounds.height / clip.offsetHeight;
      let height = Math.ceil(lineHeight * visibleLines);
      for (const block of blocks) {
        const blockBounds = block.getBoundingClientRect();
        const top = (blockBounds.top - bounds.top) / scale;
        const bottom = (blockBounds.bottom - bounds.top) / scale;
        if (top < height && bottom > height) height = Math.ceil(bottom);
      }
      clip.style.maxHeight = `${height}px`;
    };
    measure();
    const resize = new ResizeObserver(measure);
    resize.observe(content);
    const mutation = new MutationObserver(measure);
    mutation.observe(content, { childList: true, subtree: true });
    return () => {
      resize.disconnect();
      mutation.disconnect();
    };
  }, [collapsed, text, visibleLines]);

  useEffect(() => {
    const clip = clipRef.current;
    if (!collapsed || !clip) return;
    const partialMatchSelector = "a[href], area[href]";
    const focusableSelector = 'a[href], area[href], button, input, select, textarea, summary, audio[controls], video[controls], iframe, [contenteditable]:not([contenteditable="false"]), [tabindex]:not([tabindex="-1"])';
    const elements = new Map<Element, string | null>();
    const setVisible = (element: Element, visible: boolean, previousHidden: string | null) => {
      element.toggleAttribute("inert", !visible);
      if (!visible) element.setAttribute("aria-hidden", "true");
      else if (previousHidden === null) element.removeAttribute("aria-hidden");
      else element.setAttribute("aria-hidden", previousHidden);
    };
    const isRectVisibleInClip = (element: Element) => {
      const box = element.getBoundingClientRect();
      const clipBox = clip.getBoundingClientRect();
      return element.matches(partialMatchSelector)
        ? Math.min(box.right, clipBox.right) > Math.max(box.left, clipBox.left)
          && Math.min(box.bottom, clipBox.bottom) > Math.max(box.top, clipBox.top)
        : box.top >= clipBox.top && box.right <= clipBox.right && box.bottom <= clipBox.bottom && box.left >= clipBox.left;
    };
    const intersection = typeof IntersectionObserver === "undefined"
      ? null
      : new IntersectionObserver((entries) => {
          for (const entry of entries) {
            if (!elements.has(entry.target)) continue;
            const visible = entry.isIntersecting && (entry.target.matches(partialMatchSelector) ? entry.intersectionRatio > 0 : entry.intersectionRatio >= 1);
            setVisible(entry.target, visible, elements.get(entry.target) ?? null);
          }
        }, { root: clip, rootMargin: "0px", threshold: [Number.EPSILON, 1] });
    const sync = () => {
      for (const element of elements.keys()) {
        if (clip.contains(element)) continue;
        intersection?.unobserve(element);
        elements.delete(element);
      }
      for (const element of clip.querySelectorAll(focusableSelector)) {
        if (elements.has(element) || element.hasAttribute("inert")) continue;
        const previousHidden = element.getAttribute("aria-hidden");
        elements.set(element, previousHidden);
        setVisible(element, intersection === null && isRectVisibleInClip(element), previousHidden);
        intersection?.observe(element);
      }
    };
    sync();
    const mutation = new MutationObserver(sync);
    mutation.observe(clip, { childList: true, subtree: true });
    return () => {
      intersection?.disconnect();
      mutation.disconnect();
      for (const [element, previousHidden] of elements) setVisible(element, true, previousHidden);
    };
  }, [collapsed, maxLines, text]);

  return (
    <div className={`collapsible-message${overflowing ? " is-overflowing" : ""}${expanded ? " is-expanded" : ""}`}>
      <div className="collapsible-message-body">
        <div
          className={`collapsible-message-content${collapsed && collapseIndicator === "fade" ? " collapsible-message-fade" : ""}`}
          ref={clipRef}
          style={collapsed ? { maxHeight: `${visibleLines}lh` } : undefined}
        >
          <div ref={contentRef}>{children}</div>
        </div>
        {showEllipsis ? <span aria-hidden="true" className="collapsible-message-ellipsis">…</span> : null}
      </div>
      {overflowing ? (
        <button
          aria-expanded={expanded}
          className="collapsible-message-toggle"
          data-markdown-copy="exclude"
          data-thread-find-skip="true"
          onClick={() => setExpandedText((current) => current === text ? null : text)}
          type="button"
        >
          <span>{expanded ? "Show less" : "Show more"}</span>
          <Glyph className={`icon-2xs${expanded ? " rotate-180" : ""}`} name="chevronDown" size={14} />
        </button>
      ) : null}
    </div>
  );
}
