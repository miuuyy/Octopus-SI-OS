import {
  Children,
  createContext,
  isValidElement,
  memo,
  useEffect,
  useContext,
  useId,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
  type ComponentProps,
  type ReactElement,
  type ReactNode,
} from "react";
import Markdown, { type Components, defaultUrlTransform } from "react-markdown";
import rehypeKatex from "rehype-katex";
import { useAppearance } from "../../appearance";
import { openWebLink } from "../../clientBrowser";
import { copyText } from "../../runtimeFormat";
import { Glyph } from "../IconGlyphs";
import { ShellTooltip } from "../shell/ShellTooltip";
import type { HighlightedCode } from "./highlightCode";
import { MARKDOWN_REMARK_PLUGINS, prepareStreamingMarkdown } from "./markdownPresentation";
import "katex/dist/katex.min.css";

type MessageContentProps = {
  compactHeadings?: boolean;
  find?: { query: string; offset: number; activeIndex: number };
  onOpenFile?: (path: string) => void;
  streamKey?: string;
  streaming?: boolean;
  text: string;
  variant?: "assistant" | "reasoning" | "user";
};

type StreamingFadeTimeline = {
  mountedAtByElement: WeakMap<HTMLElement, number>;
  startAtByElement: WeakMap<HTMLElement, number>;
  nextSegmentStartAtMs: number;
  pendingSegmentKeyState: StreamingSegmentKeyState | null;
  segmentKeyState: StreamingSegmentKeyState;
  startAtMsByKey: Map<string, number>;
};

type StreamingSegmentKeyState = {
  nextSegmentId: number;
  previousSegments: Array<{ content: string; key: string }>;
};

const STREAM_FADE_SEGMENT_DELAY_MS = 16;
const STREAM_FADE_SEGMENT_MAX_DELAY_MS = 96;
const STREAM_FADE_DURATION_MS = 150;
const STREAM_PENDING_LEAD = "pending";
const STREAM_PENDING_DELAY_MS = 600_000;
const STREAM_LEAD_TAGS = new Set(["blockquote", "li", "table", "tr"]);

const StreamingTimelineContext = createContext<StreamingFadeTimeline | null>(null);

function createStreamingFadeTimeline(): StreamingFadeTimeline {
  return {
    mountedAtByElement: new WeakMap(),
    startAtByElement: new WeakMap(),
    nextSegmentStartAtMs: 0,
    pendingSegmentKeyState: null,
    segmentKeyState: { nextSegmentId: 0, previousSegments: [] },
    startAtMsByKey: new Map(),
  };
}

function createMarkdownComponents(
  variant: NonNullable<MessageContentProps["variant"]>,
  onOpenFile: MessageContentProps["onOpenFile"],
): Components {
  const reasoning = variant === "reasoning";
  return {
  span: MarkdownSpan,
  strong: ({ children }) => <strong className="font-semibold">{children}</strong>,
  p: ({ children }) => <p className={reasoning ? "m-0 has-[.inline-markdown]:py-0.5" : "octopus-markdown-text octopus-markdown-paragraph"}>{children}</p>,
  h1: ({ children }) => <h1 className={reasoning ? "m-0 font-semibold" : "octopus-markdown-heading octopus-markdown-heading1"}>{children}</h1>,
  h2: ({ children }) => <h2 className={reasoning ? "m-0 font-semibold" : "octopus-markdown-heading octopus-markdown-heading2"}>{children}</h2>,
  h3: ({ children }) => <h3 className={reasoning ? "m-0 font-semibold" : "octopus-markdown-heading octopus-markdown-heading3"}>{children}</h3>,
  h4: ({ children }) => <h4 className={reasoning ? "m-0 font-semibold" : "octopus-markdown-heading octopus-markdown-heading4"}>{children}</h4>,
  h5: ({ children }) => <h5 className={reasoning ? "m-0 font-semibold" : "octopus-markdown-heading octopus-markdown-heading5"}>{children}</h5>,
  h6: ({ children }) => <h6 className={reasoning ? "m-0 font-semibold" : "octopus-markdown-heading octopus-markdown-heading6"}>{children}</h6>,
  ul: ({ children, className }) => (
    <ul className={reasoning
      ? `my-0 list-disc pl-4 ${className ?? ""}`.trim()
      : `octopus-markdown-text octopus-markdown-list ${className?.includes("contains-task-list") ? "octopus-markdown-task-list" : "octopus-markdown-unordered-list"} ${className ?? ""}`.trim()}>
      {children}
    </ul>
  ),
  ol: ({ children, className, start }) => {
    const taskList = className?.includes("contains-task-list") && allChildrenAreTaskItems(children);
    return (
      <ol
        className={reasoning
          ? `my-0 list-decimal pl-8 ${className ?? ""}`.trim()
          : `octopus-markdown-text octopus-markdown-list ${taskList ? "octopus-markdown-task-list" : "octopus-markdown-ordered-list"} ${className ?? ""}`.trim()}
        start={taskList ? undefined : start}
      >
        {children}
      </ol>
    );
  },
  li: ({ children, className, node: _node, ...props }) => (
    <MarkdownListItem
      {...props}
      className={reasoning
        ? `m-0 ${className ?? ""}`.trim()
        : `octopus-markdown-text octopus-markdown-list-item ${className?.includes("task-list-item") ? "octopus-markdown-task-list-item" : ""} ${className ?? ""}`.trim()}
    >
      {children}
    </MarkdownListItem>
  ),
  blockquote: ({ children, node: _node, ...props }) => (
    <StreamingContainer as="blockquote" className="octopus-markdown-blockquote" lead={streamingLeadKey(props)}>{children}</StreamingContainer>
  ),
  hr: ({ node: _node, ...props }) => <StreamingContainer as="hr" className="octopus-markdown-horizontal-rule" lead={streamingSegmentKey(props)} />,
  a: ({ children, href }) => <SafeMarkdownLink href={href} onOpenFile={onOpenFile}>{children}</SafeMarkdownLink>,
  img: ({ alt, node: _node, src, ...props }) => <SafeMarkdownImage alt={alt ?? ""} revealKey={streamingSegmentKey(props)} src={src ?? ""} />,
  pre: ({ children }) => <>{children}</>,
  code: MarkdownCode,
  table: ({ children, node, ...props }) => <MarkdownTable lead={streamingLeadKey(props)} node={node}>{children}</MarkdownTable>,
  thead: ({ children }) => <thead>{children}</thead>,
  tbody: ({ children }) => <tbody className="octopus-markdown-table-body">{children}</tbody>,
  tr: ({ children, node: _node, ...props }) => (
    <StreamingContainer as="tr" className="octopus-markdown-table-row" lead={streamingLeadKey(props)}>{children}</StreamingContainer>
  ),
  th: ({ children }) => (
    <th className={`octopus-markdown-table-header-cell${Children.count(children) === 1 ? " octopus-markdown-single-child-table-cell" : ""}`}>
      {children}
    </th>
  ),
  td: ({ children }) => {
    const numeric = /^\d+$/.test(textFromChildren(children));
    return (
      <td className={`octopus-markdown-table-cell${numeric ? " octopus-markdown-numeric-table-cell" : ""}${Children.count(children) === 1 ? " octopus-markdown-single-child-table-cell" : ""}`}>
        {children}
      </td>
    );
  },
  input: ({ checked, disabled, node: _node, type, ...props }) => type === "checkbox" ? (
    <StreamingContainer as="div" className="relative mr-1.5 inline-flex cursor-default align-[-0.125rem]" lead={streamingSegmentKey(props)}>
      <button
        aria-checked={Boolean(checked)}
        className="border-token-border peer data-[state=checked]:bg-token-checkbox-background data-[state=checked]:text-token-checkbox-foreground data-[state=checked]:border-token-border focus-visible:border-token-border focus-visible:ring-token-checkbox-background/50 focus-visible:ring-1 icon-2xs rounded-xs shrink-0 border shadow-sm outline-none transition-[background-color,border-color,box-shadow] disabled:cursor-not-allowed pointer-events-none"
        data-state={checked ? "checked" : "unchecked"}
        disabled={disabled}
        role="checkbox"
        type="button"
      >
        <span className="flex h-full w-full items-center justify-center text-current">
          {checked ? <Glyph className="icon-xxs flex-shrink-0" name="check" size={10} /> : null}
        </span>
      </button>
    </StreamingContainer>
  ) : null,
  };
}

const MarkdownSourceContext = createContext("");

type StreamingReveal = { className?: string; ref?: (element: HTMLElement | null) => void };

const NO_STREAMING_REVEAL: StreamingReveal = {};

function streamingSegmentKey(props: object): string | undefined {
  const row = props as Record<string, unknown>;
  const value = row["data-stream-segment-key"] ?? row.dataStreamSegmentKey;
  return typeof value === "string" ? value : undefined;
}

function streamingLeadKey(props: object): string | undefined {
  const row = props as Record<string, unknown>;
  const value = row["data-stream-lead-key"] ?? row.dataStreamLeadKey;
  return typeof value === "string" ? value : undefined;
}

function withoutStreamingKeys<T extends object>(props: T): T {
  const {
    "data-stream-segment-key": _segment,
    "data-stream-lead-key": _lead,
    dataStreamSegmentKey: _segmentProperty,
    dataStreamLeadKey: _leadProperty,
    ...rest
  } = props as Record<string, unknown>;
  return rest as T;
}

function joinClassNames(...values: Array<string | undefined>): string | undefined {
  const joined = values.filter(Boolean).join(" ");
  return joined || undefined;
}

function useStreamingReveal(key: string | undefined, container = false): StreamingReveal {
  const timeline = useContext(StreamingTimelineContext);
  if (!timeline || key === undefined) return NO_STREAMING_REVEAL;
  const startsAt = key === STREAM_PENDING_LEAD ? undefined : timeline.startAtMsByKey.get(key);
  const settled = startsAt !== undefined && performance.now() >= startsAt + STREAM_FADE_DURATION_MS;
  if (settled && !container) return NO_STREAMING_REVEAL;
  return {
    className: settled ? undefined : "octopus-markdown-fade-in",
    ref: (element) => {
      if (element) revealStreamingElement(element, key, timeline);
    },
  };
}

function revealStreamingElement(element: HTMLElement, key: string, timeline: StreamingFadeTimeline) {
  let mountedAt = timeline.mountedAtByElement.get(element);
  if (mountedAt === undefined) {
    mountedAt = performance.now();
    timeline.mountedAtByElement.set(element, mountedAt);
  }
  const previous = timeline.startAtByElement.get(element);
  let startsAt = mountedAt + STREAM_PENDING_DELAY_MS;
  if (key !== STREAM_PENDING_LEAD) {
    startsAt = Math.min(previous ?? Number.POSITIVE_INFINITY, streamingStartAt(timeline, key));
    timeline.startAtByElement.set(element, startsAt);
  } else if (previous !== undefined) {
    startsAt = previous;
  }
  const delay = `${Math.round(startsAt - mountedAt)}ms`;
  if (element.style.getPropertyValue("--fade-delay") !== delay) element.style.setProperty("--fade-delay", delay);
}

function streamingStartAt(timeline: StreamingFadeTimeline, key: string): number {
  let startsAt = timeline.startAtMsByKey.get(key);
  if (startsAt === undefined) {
    const now = performance.now();
    startsAt = Math.min(Math.max(timeline.nextSegmentStartAtMs, now), now + STREAM_FADE_SEGMENT_MAX_DELAY_MS);
    timeline.startAtMsByKey.set(key, startsAt);
    timeline.nextSegmentStartAtMs = startsAt + STREAM_FADE_SEGMENT_DELAY_MS;
  }
  return startsAt;
}

function MarkdownSpan({ children, className, node: _node, ...props }: ComponentProps<"span"> & { node?: unknown }) {
  const reveal = useStreamingReveal(streamingSegmentKey(props));
  return <span {...withoutStreamingKeys(props)} className={joinClassNames(className, reveal.className)} ref={reveal.ref}>{children}</span>;
}

function MarkdownListItem({ children, className, ...props }: ComponentProps<"li">) {
  const reveal = useStreamingReveal(streamingLeadKey(props), true);
  return <li {...withoutStreamingKeys(props)} className={joinClassNames(className, reveal.className)} ref={reveal.ref}>{children}</li>;
}

function StreamingContainer({ as: Tag, children, className, lead }: {
  as: "blockquote" | "div" | "hr" | "tr";
  children?: ReactNode;
  className: string;
  lead: string | undefined;
}) {
  const reveal = useStreamingReveal(lead, true);
  return <Tag className={joinClassNames(className, reveal.className)} ref={reveal.ref}>{children}</Tag>;
}

function MarkdownCode({ children, className, node: _node, ...props }: ComponentProps<"code"> & { node?: unknown }) {
  const reveal = useStreamingReveal(streamingSegmentKey(props));
  const raw = String(children);
  const block = Boolean(className) || raw.includes("\n");
  const value = raw.replace(/\n$/, "");
  const language = /^language-(.+)$/.exec(className ?? "")?.[1] ?? "";
  if (!block) {
    return <code className={joinClassNames("octopus-markdown-inline-markdown", reveal.className)} ref={reveal.ref}>{value}</code>;
  }
  if (language.toLocaleLowerCase() === "mermaid") return <MermaidBlock reveal={reveal} value={value} />;
  return <CodeBlock language={language} reveal={reveal} value={value} />;
}

function MarkdownTable({ children, lead, node }: { children: ReactNode; lead: string | undefined; node?: { position?: { start?: { offset?: number }; end?: { offset?: number } } } }) {
  const source = useContext(MarkdownSourceContext);
  const start = node?.position?.start?.offset;
  const end = node?.position?.end?.offset;
  const markdownSource = typeof start === "number" && typeof end === "number"
    ? source.slice(start, end)
    : textFromChildren(children);
  return <TableBlock lead={lead} markdownSource={markdownSource}>{children}</TableBlock>;
}

function SafeMarkdownLink({ children, href = "", onOpenFile }: { children: ReactNode; href?: string; onOpenFile: MessageContentProps["onOpenFile"] }) {
  const external = /^https?:\/\//i.test(href);

  if (!external) {
    if (href.startsWith("#")) return <a href={href}>{children}</a>;
    const filePath = markdownFilePath(href);
    if (!filePath || !onOpenFile) return <span>{children}</span>;
    return (
      <a href={href} onAuxClick={(event) => event.preventDefault()} onClick={(event) => {
        event.preventDefault();
        onOpenFile(filePath);
      }}>{children}</a>
    );
  }
  return <a href={href} rel="noreferrer" onClick={event => {
    event.preventDefault(); openWebLink(href);
  }} onAuxClick={event => {
    if (event.button === 1) { event.preventDefault(); openWebLink(href); }
  }}>{children}</a>;
}

function SafeMarkdownImage({ alt, revealKey, src }: { alt: string; revealKey?: string; src: string }) {
  const reveal = useStreamingReveal(revealKey);
  const [allowed, setAllowed] = useState(false);
  const [failed, setFailed] = useState(false);
  let host = "external source";
  let external = false;
  try {
    const parsed = new URL(src);
    external = parsed.protocol === "https:" || parsed.protocol === "http:";
    host = parsed.hostname;
  } catch {
    external = false;
  }

  useEffect(() => {
    setAllowed(false);
    setFailed(false);
  }, [src]);

  if (!external) {
    return (
      <span className={joinClassNames("my-2 flex min-h-16 w-full items-center gap-3 rounded-xl border border-token-border bg-token-foreground/[0.025] px-3 py-2 text-sm text-token-description-foreground", reveal.className)} ref={reveal.ref}>
        <Glyph className="shrink-0" name="image" size={18} />
        <span className="min-w-0"><strong className="block font-medium text-token-text-secondary">Image preview unavailable</strong><span className="block truncate">{alt || "The message does not provide an admitted media URL."}</span></span>
      </span>
    );
  }
  if (!allowed || failed) {
    return (
      <span className={joinClassNames("my-2 flex min-h-20 w-full items-center gap-3 rounded-xl border border-token-border bg-token-foreground/[0.025] px-3 py-2", reveal.className)} ref={reveal.ref}>
        <span className="flex size-9 shrink-0 items-center justify-center rounded-lg bg-token-list-hover-background text-token-description-foreground"><Glyph name="image" size={18} /></span>
        <span className="min-w-0 flex-1 text-sm"><strong className="block font-medium">{failed ? "Image could not be loaded" : "External image blocked"}</strong><span className="block truncate text-token-description-foreground">{alt || host}</span></span>
        <button
          className="no-drag shrink-0 cursor-interaction rounded-lg bg-token-foreground px-2.5 py-1.5 text-sm text-token-main-surface-primary hover:opacity-90 focus:outline-none focus-visible:ring-1 focus-visible:ring-token-focus-border"
          onClick={() => {
            setFailed(false);
            setAllowed(true);
          }}
          type="button"
        >
          {failed ? "Retry" : "Load image"}
        </button>
      </span>
    );
  }
  return (
    <span className={joinClassNames("my-2 block overflow-auto rounded-xl border border-token-border bg-token-foreground/[0.025] p-2", reveal.className)} ref={reveal.ref}>
      <img
        alt={alt}
        className="mx-auto max-h-[70vh] max-w-full rounded-lg object-contain"
        loading="lazy"
        onError={() => setFailed(true)}
        referrerPolicy="no-referrer"
        src={src}
      />
    </span>
  );
}

function MessageContentView({ compactHeadings = false, find, onOpenFile, streamKey = "response", streaming = false, text, variant = "assistant" }: MessageContentProps) {
  const presentation = useMemo(() => streaming ? prepareStreamingMarkdown(text) : text, [streaming, text]);
  const timelineRef = useRef<{ key: string; timeline: StreamingFadeTimeline } | null>(null);
  if (streaming && timelineRef.current?.key !== streamKey) {
    timelineRef.current = { key: streamKey, timeline: createStreamingFadeTimeline() };
  }
  const streamingTimeline = streaming ? timelineRef.current?.timeline ?? null : null;
  const components = useMemo(
    () => {
      const defaults = createMarkdownComponents(variant, onOpenFile);
      if (!compactHeadings) return defaults;
      const heading: Components["h1"] = ({ children }) => <span>{children}</span>;
      return { ...defaults, h1: heading, h2: heading, h3: heading, h4: heading, h5: heading, h6: heading };
    },
    [compactHeadings, onOpenFile, variant],
  );
  const streamingPlugin = useMemo(
    () => streaming && streamingTimeline ? streamingTextPlugin(streamKey, streamingTimeline) : null,
    [streamKey, streaming, streamingTimeline],
  );
  const rehypePlugins = useMemo(() => [
    rehypeKatex,
    ...(find ? [[findHighlightPlugin, find] as [typeof findHighlightPlugin, NonNullable<MessageContentProps["find"]>]] : []),
    ...(streamingPlugin ? [streamingPlugin] : []),
  ], [find?.activeIndex, find?.offset, find?.query, streamingPlugin]);
  const className = variant === "user"
    ? "octopus-markdown-root [&>*:first-child]:mt-0 octopus-markdown-content [&_li+li]:!mt-0 [&_li>ol]:!mt-0 [&_li>p+p]:!mt-0 [&_li>ul]:!mt-0 [&_ol]:!pl-6 [&_p]:!m-0 [&_p+p]:!mt-5 [&_ul]:!pl-6"
    : variant === "reasoning"
      ? "octopus-markdown-root octopus-markdown-content break-words text-size-chat text-token-conversation-body [&_*]:text-size-chat [&_*]:text-token-non-assistant-body-descendant [&>h1]:mt-2 [&>h2]:mt-2 [&>h3]:mt-2 [&>h1+*]:mt-1 [&>h2+*]:mt-1 [&>h3+*]:mt-1 [&>p+p]:mt-1"
      : "octopus-markdown-root [&>*:first-child]:mt-0 octopus-markdown-content [&>*:last-child]:mb-0 [&>ol:first-child]:mt-0 [&>ul:first-child]:mt-0";

  useLayoutEffect(() => {
    if (!streamingTimeline?.pendingSegmentKeyState) return;
    streamingTimeline.segmentKeyState = streamingTimeline.pendingSegmentKeyState;
    streamingTimeline.pendingSegmentKeyState = null;
  });

  return (
    <MarkdownSourceContext.Provider value={text}>
      <StreamingTimelineContext.Provider value={streamingTimeline}>
        <div className={`${className}${streaming ? " octopus-streaming-markdown" : ""}`} data-markdown-animated={streaming ? "" : undefined}>
          <Markdown
            components={components}
            rehypePlugins={rehypePlugins}
            remarkPlugins={MARKDOWN_REMARK_PLUGINS}
            skipHtml
            urlTransform={safeUrlTransform}
          >
            {presentation}
          </Markdown>
        </div>
      </StreamingTimelineContext.Provider>
    </MarkdownSourceContext.Provider>
  );
}

export const MessageContent = memo(MessageContentView, (previous, next) => (
  previous.text === next.text
  && previous.compactHeadings === next.compactHeadings
  && previous.onOpenFile === next.onOpenFile
  && previous.streamKey === next.streamKey
  && previous.streaming === next.streaming
  && previous.variant === next.variant
  && previous.find?.query === next.find?.query
  && previous.find?.offset === next.find?.offset
  && previous.find?.activeIndex === next.find?.activeIndex
));

function streamingTextPlugin(streamKey: string, timeline: StreamingFadeTimeline) {
  return () => (tree: HastNode) => {
    const contents: string[] = [];
    const targets: HastNode[] = [];
    const leads: Array<{ end: number; first: number; node: HastNode }> = [];
    const unit = (content: string, target: HastNode) => {
      contents.push(content);
      targets.push(target);
    };

    function visit(node: HastNode) {
      const children = node.children;
      if (!children?.length) return;
      for (let index = 0; index < children.length; index += 1) {
        const child = children[index];
        if (child.type === "text" && typeof child.value === "string") {
          const segments = cachedStreamingSegments(child.value);
          if (!segments.length) continue;
          const spans = segments.map((segment) => ({ type: "element", tagName: "span", properties: {}, children: [{ type: "text", value: segment }] }));
          spans.forEach((span, spanIndex) => unit(segments[spanIndex] ?? "", span));
          children.splice(index, 1, ...spans);
          index += spans.length - 1;
          continue;
        }
        if (child.type !== "element") continue;
        const tagName = child.tagName?.toLowerCase() ?? "";
        if (tagName === "pre") {
          const code = child.children?.find((candidate) => candidate.type === "element" && candidate.tagName === "code");
          if (code) unit("\u0000pre", code);
          continue;
        }
        if (["code", "hr", "img", "input"].includes(tagName) || hastClassNames(child).some((name) => name === "katex" || name === "katex-display")) {
          unit(`\u0000${tagName}:${hastText(child)}`, child);
          continue;
        }
        if (["math", "svg", "style"].includes(tagName)) continue;
        const first = contents.length;
        visit(child);
        if (STREAM_LEAD_TAGS.has(tagName)) leads.push({ end: contents.length, first, node: child });
      }
    }

    visit(tree);
    const candidate: StreamingSegmentKeyState = {
      nextSegmentId: timeline.segmentKeyState.nextSegmentId,
      previousSegments: timeline.segmentKeyState.previousSegments,
    };
    const keys = reconcileStreamingSegmentKeys(contents, candidate);
    targets.forEach((target, index) => {
      target.properties = { ...target.properties, dataStreamSegmentKey: `${streamKey}:${keys[index]}` };
    });
    for (const lead of leads) {
      lead.node.properties = {
        ...lead.node.properties,
        dataStreamLeadKey: lead.first < lead.end ? `${streamKey}:${keys[lead.first]}` : STREAM_PENDING_LEAD,
      };
    }
    timeline.pendingSegmentKeyState = candidate;
  };
}

function hastClassNames(node: HastNode): string[] {
  const value = node.properties?.className;
  if (Array.isArray(value)) return value.filter((item): item is string => typeof item === "string");
  return typeof value === "string" ? value.split(/\s+/).filter(Boolean) : [];
}

function hastText(node: HastNode): string {
  if (node.type === "text") return node.value ?? "";
  return (node.children ?? []).map(hastText).join("");
}

const STREAM_SEGMENT_CACHE_LIMIT = 500;
const STREAM_SEGMENT_CACHE = new Map<string, readonly string[]>();
const STREAM_WORD_SEGMENTER = createWordSegmenter();

function createWordSegmenter(): Intl.Segmenter | null {
  try {
    return new Intl.Segmenter(undefined, { granularity: "word" });
  } catch {
    return null;
  }
}

function cachedStreamingSegments(text: string): readonly string[] {
  const cached = STREAM_SEGMENT_CACHE.get(text);
  if (cached) return cached;
  const segments = segmentStreamingText(text, STREAM_WORD_SEGMENTER);
  STREAM_SEGMENT_CACHE.set(text, segments);
  if (STREAM_SEGMENT_CACHE.size > STREAM_SEGMENT_CACHE_LIMIT) {
    const oldest = STREAM_SEGMENT_CACHE.keys().next().value;
    if (typeof oldest === "string") STREAM_SEGMENT_CACHE.delete(oldest);
  }
  return segments;
}

function segmentStreamingText(text: string, segmenter: Intl.Segmenter | null): string[] {
  if (isAscii(text)) {
    const segments = Array.from(text.match(/\s*\S+(?:\s+|$)/g) ?? []);
    return segments.length || !text ? segments : [text];
  }
  if (!segmenter) return Array.from(text.match(/\s*\S+(?:\s+|$)/gu) ?? [text]);
  const segments: string[] = [];
  for (const part of segmenter.segment(text)) {
    if (/^\s*$/u.test(part.segment) || !part.isWordLike) {
      const previous = Math.max(segments.length - 1, 0);
      segments[previous] = `${segments[previous] ?? ""}${part.segment}`;
    } else {
      segments.push(part.segment);
    }
  }
  return segments;
}

function reconcileStreamingSegmentKeys(contents: string[], state: StreamingSegmentKeyState): string[] {
  const available = new Set(state.previousSegments.keys());
  const byContent = new Map<string, number[]>();
  for (let index = state.previousSegments.length - 1; index >= 0; index -= 1) {
    const previous = state.previousSegments[index];
    if (!previous) continue;
    const indexes = byContent.get(previous.content) ?? [];
    indexes.push(index);
    byContent.set(previous.content, indexes);
  }
  const keys = contents.map((content, index) => {
    const samePosition = state.previousSegments[index];
    if (
      samePosition
      && available.has(index)
      && (samePosition.content === content || (samePosition.content.length > 0 && content.startsWith(samePosition.content)))
    ) {
      available.delete(index);
      return samePosition.key;
    }
    const matchingIndexes = byContent.get(content);
    let matchingIndex = matchingIndexes?.pop();
    while (matchingIndex !== undefined && !available.has(matchingIndex)) matchingIndex = matchingIndexes?.pop();
    const matching = matchingIndex === undefined ? undefined : state.previousSegments[matchingIndex];
    if (matchingIndex !== undefined && matching) {
      available.delete(matchingIndex);
      return matching.key;
    }
    const key = `fade-segment-${state.nextSegmentId}`;
    state.nextSegmentId += 1;
    return key;
  });
  state.previousSegments = keys.map((key, index) => ({ content: contents[index] ?? "", key }));
  return keys;
}

function isAscii(value: string): boolean {
  for (let index = 0; index < value.length; index += 1) {
    if (value.charCodeAt(index) > 127) return false;
  }
  return true;
}

type HastNode = {
  type?: string;
  value?: string;
  tagName?: string;
  properties?: Record<string, unknown>;
  children?: HastNode[];
};

function findHighlightPlugin(options: { query: string; offset: number; activeIndex: number }) {
  return (tree: HastNode) => {
    const query = options.query.toLocaleLowerCase();
    let localIndex = 0;

    function visit(node: HastNode) {
      if (!node.children || node.tagName === "code" || node.tagName === "pre") return;
      const nextChildren: HastNode[] = [];
      for (const child of node.children) {
        if (child.type !== "text" || !child.value) {
          visit(child);
          nextChildren.push(child);
          continue;
        }
        const value = child.value;
        const lower = value.toLocaleLowerCase();
        let cursor = 0;
        while (cursor <= value.length - query.length) {
          const matchStart = lower.indexOf(query, cursor);
          if (matchStart < 0) break;
          if (matchStart > cursor) nextChildren.push({ type: "text", value: value.slice(cursor, matchStart) });
          const globalIndex = options.offset + localIndex;
          nextChildren.push({
            type: "element",
            tagName: "mark",
            properties: {
              className: [
                "octopus-thread-find-match",
                globalIndex === options.activeIndex ? "octopus-thread-find-active" : "",
              ].filter(Boolean),
              dataOctopusThreadFindIndex: String(globalIndex),
            },
            children: [{ type: "text", value: value.slice(matchStart, matchStart + options.query.length) }],
          });
          localIndex += 1;
          cursor = matchStart + Math.max(1, options.query.length);
        }
        if (cursor < value.length) nextChildren.push({ type: "text", value: value.slice(cursor) });
      }
      node.children = nextChildren;
    }

    if (query) visit(tree);
  };
}

function TableBlock({ children, lead, markdownSource }: { children: ReactNode; lead?: string; markdownSource: string }) {
  const reveal = useStreamingReveal(lead, true);
  const [copied, setCopied] = useState(false);
  const resetTimerRef = useRef<number | null>(null);

  useEffect(() => () => {
    if (resetTimerRef.current !== null) window.clearTimeout(resetTimerRef.current);
  }, []);

  function copyTable() {
    void copyText(markdownSource).then((copied) => {
      if (!copied) return;
      setCopied(true);
      if (resetTimerRef.current !== null) window.clearTimeout(resetTimerRef.current);
      resetTimerRef.current = window.setTimeout(() => {
        setCopied(false);
        resetTimerRef.current = null;
      }, 1_500);
    });
  }

  return (
    <div className={joinClassNames("octopus-markdown-table-container", reveal.className)} ref={reveal.ref}>
      <div className="octopus-markdown-table-wrapper group relative" data-markdown-table tabIndex={-1}>
        <table className="octopus-markdown-text octopus-markdown-table">{children}</table>
        <div className="absolute end-0 top-0 select-none" data-markdown-copy="exclude">
          <ShellTooltip label={copied ? "Copied" : "Copy table"}>
            <button
              aria-label={copied ? "Copied" : "Copy table"}
              className="border-token-border no-drag cursor-interaction m-1 flex items-center gap-1 border whitespace-nowrap select-none focus:outline-none disabled:cursor-not-allowed disabled:opacity-40 rounded-full electron:rounded-md text-token-text-tertiary enabled:hover:bg-token-list-hover-background data-[state=open]:bg-token-list-hover-background border-transparent electron:p-1 electron:[&>svg]:icon-sm justify-center p-0.5 group-[:not(:hover):not(:focus-within)]:pointer-events-none group-[:not(:hover):not(:focus-within)]:opacity-0"
              onClick={copyTable}
              type="button"
            >
              <Glyph className="icon-xs" name={copied ? "check" : "copy"} size={16} />
            </button>
          </ShellTooltip>
        </div>
      </div>
    </div>
  );
}

let mermaidRenderSequence = 0;

function MermaidBlock({ reveal = NO_STREAMING_REVEAL, value }: { reveal?: StreamingReveal; value: string }) {
  const appearance = useAppearance();
  const systemDark = useSyncExternalStore(subscribeToColorScheme, systemPrefersDark, () => false);
  const theme = appearance.themeMode === "system" ? (systemDark ? "dark" : "light") : appearance.themeMode;
  const renderId = `octopus-mermaid-${useId().replace(/:/g, "")}`;
  const [svg, setSvg] = useState("");
  const [error, setError] = useState("");
  const [sourceOpen, setSourceOpen] = useState(false);
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    let cancelled = false;
    const requestRenderId = `${renderId}-${++mermaidRenderSequence}`;
    setSvg("");
    setError("");
    if (value.length > 50_000) {
      setError("Diagram source exceeds the 50 KB renderer limit.");
      return;
    }
    void import("mermaid").then(async ({ default: mermaid }) => {
      const styles = window.getComputedStyle(document.documentElement);
      const token = (name: string, fallback: string) => styles.getPropertyValue(name).trim() || fallback;
      mermaid.initialize({
        startOnLoad: false,
        securityLevel: "strict",
        suppressErrorRendering: true,
        theme: "base",
        themeVariables: {
          background: token("--color-token-bg-primary", theme === "dark" ? "#181818" : "#ffffff"),
          primaryColor: token("--color-token-dropdown-background", theme === "dark" ? "#252525" : "#f4f4f4"),
          primaryTextColor: token("--color-token-foreground", theme === "dark" ? "#f2f2f2" : "#171717"),
          primaryBorderColor: token("--color-token-border", theme === "dark" ? "#444444" : "#d7d7d7"),
          lineColor: token("--color-token-text-secondary", theme === "dark" ? "#a4a4a4" : "#636363"),
          secondaryColor: token("--color-token-list-hover-background", theme === "dark" ? "#303030" : "#eeeeee"),
          tertiaryColor: token("--color-token-input-background", theme === "dark" ? "#202020" : "#fafafa"),
          fontFamily: token("--font-sans", "-apple-system, BlinkMacSystemFont, sans-serif"),
        },
      });
      const rendered = await mermaid.render(requestRenderId, value);
      if (!cancelled) setSvg(rendered.svg);
    }).catch((reason: unknown) => {
      if (!cancelled) setError(reason instanceof Error ? reason.message : "The diagram could not be rendered.");
    });
    return () => {
      cancelled = true;
      document.getElementById(`d${requestRenderId}`)?.remove();
    };
  }, [appearance, renderId, systemDark, theme, value]);

  useEffect(() => {
    if (!copied) return;
    const timer = window.setTimeout(() => setCopied(false), 1_500);
    return () => window.clearTimeout(timer);
  }, [copied]);

  return (
    <div className={joinClassNames("group/diagram relative my-2 w-full min-w-0 overflow-hidden rounded-xl border border-token-border bg-token-text-code-block-background text-token-foreground", reveal.className)} data-mermaid-diagram ref={reveal.ref}>
      <div className="flex min-h-8 items-center gap-2 px-2 text-sm text-token-description-foreground select-none">
        <span className="min-w-0 flex-1 truncate">Mermaid diagram</span>
        <button
          aria-expanded={sourceOpen}
          className="no-drag cursor-interaction rounded-md px-1.5 py-0.5 hover:bg-token-list-hover-background focus:outline-none focus-visible:ring-1 focus-visible:ring-token-focus-border"
          onClick={() => setSourceOpen((open) => !open)}
          type="button"
        >
          Source
        </button>
        <ShellTooltip label={copied ? "Copied" : "Copy diagram source"}>
          <button
            aria-label={copied ? "Copied" : "Copy diagram source"}
            className="no-drag cursor-interaction rounded-md p-1 hover:bg-token-list-hover-background focus:outline-none focus-visible:ring-1 focus-visible:ring-token-focus-border"
            onClick={() => void copyText(value).then((didCopy) => setCopied(didCopy))}
            type="button"
          >
            <Glyph className="icon-xs" name={copied ? "check" : "copy"} size={15} />
          </button>
        </ShellTooltip>
      </div>
      {sourceOpen ? <CodeBlock language="mermaid" value={value} /> : null}
      {!sourceOpen ? (
        <div className="min-h-20 overflow-auto p-3">
          {svg ? (
            <div
              aria-label="Mermaid diagram generated from the message"
              className="mx-auto min-w-fit [&_svg]:mx-auto [&_svg]:max-w-none"
              dangerouslySetInnerHTML={{ __html: svg }}
              role="img"
            />
          ) : error ? (
            <div className="flex min-h-20 flex-col items-center justify-center gap-1 px-4 py-3 text-center">
              <strong className="text-sm font-medium">Diagram could not be rendered</strong>
              <span className="max-w-xl text-xs text-token-description-foreground">{error}</span>
            </div>
          ) : (
            <div aria-live="polite" className="flex min-h-20 items-center justify-center text-sm text-token-description-foreground">
              Rendering diagram…
            </div>
          )}
        </div>
      ) : null}
    </div>
  );
}

function CodeBlock({ language, reveal = NO_STREAMING_REVEAL, value }: { language: string; reveal?: StreamingReveal; value: string }) {
  const [copied, setCopied] = useState(false);
  const appearance = useAppearance();
  const systemDark = useSyncExternalStore(subscribeToColorScheme, systemPrefersDark, () => false);
  const theme = appearance.themeMode === "system"
    ? (systemDark ? "dark" : "light")
    : appearance.themeMode;
  const highlighted = useHighlightedCode(value, language);
  const shouldWrap = useCodeBlockWordWrap();
  const highlightedPrefix = highlighted !== null && value.startsWith(highlighted.code) ? highlighted : null;
  const highlightedLines = highlightedPrefix?.html.split("\n") ?? null;
  const unhighlightedTail = highlightedPrefix === null ? "" : value.slice(highlightedPrefix.code.length);

  useEffect(() => {
    if (!copied) return;
    const timer = window.setTimeout(() => setCopied(false), 2_000);
    return () => window.clearTimeout(timer);
  }, [copied]);

  return (
    <div
      className={`relative w-full min-w-0 overflow-clip rounded-3xl border border-subtle text-default contain-inline-size ${theme} octopus-markdown-code-block octopus-code-surface${reveal.className ? ` ${reveal.className}` : ""}`}
      data-markdown-copy="code-block"
      data-theme={theme}
      ref={reveal.ref}
    >
      <div
        className="octopus-code-sticky-action-bar flex min-h-12 items-center gap-2 py-1.5 ps-4 pe-1.5 md:ps-5 font-sans text-sm font-medium text-default select-none"
        data-markdown-copy="exclude"
      >
        <div className="min-w-0 flex-1 truncate">{language}</div>
        <div className="ms-auto flex min-w-0 max-w-full items-center">
          <div className="flex min-w-0 flex-wrap items-center justify-end gap-px" data-code-block-controls="" role="presentation">
            <ShellTooltip label={shouldWrap ? "Disable word wrap" : "Enable word wrap"}>
              <button
                aria-label={shouldWrap ? "Disable word wrap" : "Enable word wrap"}
                aria-pressed={shouldWrap}
                className={`border-token-border no-drag cursor-interaction flex items-center gap-1 border whitespace-nowrap select-none focus:outline-none disabled:cursor-not-allowed disabled:opacity-40 rounded-full electron:rounded-md text-token-text-tertiary enabled:hover:bg-token-list-hover-background data-[state=open]:bg-token-list-hover-background border-transparent electron:p-1 electron:[&>svg]:icon-sm justify-center p-0.5${shouldWrap ? " text-token-foreground" : " hover:text-token-foreground"}`}
                onClick={() => setCodeBlockWordWrap(!shouldWrap)}
                type="button"
              >
                {shouldWrap
                  ? <WrapDisabledIcon className="icon-2xs" />
                  : <WrapEnabledIcon className="icon-2xs" />}
              </button>
            </ShellTooltip>
            <ShellTooltip label={copied ? "Copied" : "Copy code"}>
              <button
                aria-label={copied ? "Copied" : "Copy code"}
                className={`border-token-border no-drag cursor-interaction flex items-center gap-1 border whitespace-nowrap select-none focus:outline-none disabled:cursor-not-allowed disabled:opacity-40 rounded-full electron:rounded-md text-token-text-tertiary enabled:hover:bg-token-list-hover-background data-[state=open]:bg-token-list-hover-background border-transparent electron:p-1 electron:[&>svg]:icon-sm justify-center p-0.5${copied ? " text-token-foreground" : " hover:text-token-foreground"}`}
                onClick={() => {
                  copyText(value);
                  setCopied(true);
                }}
                type="button"
              >
                <Glyph className="icon-xs" name={copied ? "check" : "copy"} size={16} />
              </button>
            </ShellTooltip>
          </div>
        </div>
      </div>
      <div
        className={`octopus-code-block-source text-size-chat overflow-auto px-4 pt-0 pb-3 md:px-5 outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset ${shouldWrap ? "wrap-anywhere whitespace-pre-wrap" : "whitespace-pre"}`}
        data-code-block-source=""
        dir="ltr"
      >
        <code className={shouldWrap ? "whitespace-pre-wrap!" : "whitespace-pre!"}>
          {highlightedLines === null ? (
            <span>{value}</span>
          ) : (
            <span>
              {highlightedLines.map((html, index) => (
                <span key={index}>
                  <HighlightedLine html={html} />
                  {index < highlightedLines.length - 1 ? "\n" : null}
                </span>
              ))}
              {unhighlightedTail ? <span>{unhighlightedTail}</span> : null}
            </span>
          )}
        </code>
      </div>
    </div>
  );
}

type HighlightJob = {
  disposed: boolean;
  latestContent: string;
  latestLanguage: string;
  lastStartedAtMs: number | null;
  timeoutHandle: number | null;
};

function useHighlightedCode(content: string, language: string): HighlightedCode | null {
  const [highlighted, setHighlighted] = useState<HighlightedCode | null>(null);
  const jobRef = useRef<HighlightJob>({
    disposed: false,
    latestContent: content,
    latestLanguage: language,
    lastStartedAtMs: null,
    timeoutHandle: null,
  });

  useEffect(() => {
    const job = jobRef.current;
    job.latestContent = content;
    job.latestLanguage = language;
    if (job.timeoutHandle !== null) return;

    const now = performance.now();
    const elapsed = job.lastStartedAtMs === null ? 120 : now - job.lastStartedAtMs;
    const delay = Math.max(0, 120 - elapsed);
    const run = () => {
      job.timeoutHandle = null;
      if (job.disposed) return;
      const nextContent = job.latestContent;
      const nextLanguage = job.latestLanguage;
      job.lastStartedAtMs = performance.now();
      void import("./highlightCode").then(({ highlightCode }) => {
        if (job.disposed) return;
        try {
          setHighlighted(highlightCode(nextContent, nextLanguage));
        } catch (error) {
          if (error instanceof Error && /Unknown language/i.test(error.message)) {
            setHighlighted(null);
            return;
          }
          throw error;
        }
      });
    };

    if (delay === 0) {
      run();
      return;
    }
    job.timeoutHandle = window.setTimeout(run, delay);
  }, [content, language]);

  useEffect(() => {
    const job = jobRef.current;
    job.disposed = false;
    return () => {
      job.disposed = true;
      if (job.timeoutHandle !== null) {
        window.clearTimeout(job.timeoutHandle);
        job.timeoutHandle = null;
      }
    };
  }, []);

  return highlighted;
}

const HighlightedLine = memo(function HighlightedLine({ html }: { html: string }) {
  return <span dangerouslySetInnerHTML={{ __html: html }} />;
});

const CODE_BLOCK_WORD_WRAP_KEY = "octopus.markdownCodeBlockWordWrap";
const codeBlockWordWrapListeners = new Set<() => void>();
let codeBlockWordWrap = loadCodeBlockWordWrap();

function loadCodeBlockWordWrap(): boolean {
  try {
    return typeof window !== "undefined" && window.localStorage.getItem(CODE_BLOCK_WORD_WRAP_KEY) === "true";
  } catch {
    return false;
  }
}

function subscribeToCodeBlockWordWrap(listener: () => void): () => void {
  codeBlockWordWrapListeners.add(listener);
  return () => codeBlockWordWrapListeners.delete(listener);
}

function useCodeBlockWordWrap(): boolean {
  return useSyncExternalStore(subscribeToCodeBlockWordWrap, () => codeBlockWordWrap, () => false);
}

function setCodeBlockWordWrap(next: boolean): void {
  if (codeBlockWordWrap === next) return;
  codeBlockWordWrap = next;
  try {
    window.localStorage.setItem(CODE_BLOCK_WORD_WRAP_KEY, String(next));
  } catch {
    void 0;
  }
  for (const listener of codeBlockWordWrapListeners) listener();
}

function subscribeToColorScheme(listener: () => void): () => void {
  const query = window.matchMedia("(prefers-color-scheme: dark)");
  query.addEventListener("change", listener);
  return () => query.removeEventListener("change", listener);
}

function systemPrefersDark(): boolean {
  return window.matchMedia("(prefers-color-scheme: dark)").matches;
}

function WrapEnabledIcon({ className }: { className?: string }) {
  return (
    <svg className={className} fill="currentColor" height="20" viewBox="0 0 20 20" width="20" xmlns="http://www.w3.org/2000/svg">
      <path d="M10.33 12.668c.367 0 .665.298.665.665l.002 3.333a.665.665 0 0 1-1.33.001l-.002-3.334c0-.367.298-.665.665-.665Zm3.364-5.639a.665.665 0 0 1 .94 0l2.5 2.5c.26.26.26.682 0 .942l-2.5 2.5a.666.666 0 0 1-.94-.942l1.365-1.364H3.33a.665.665 0 1 1 0-1.33h11.728l-1.365-1.364a.666.666 0 0 1 0-.942ZM10.33 2.668c.367 0 .665.298.665.665l.002 3.333a.665.665 0 0 1-1.33.001l-.002-3.334c0-.367.298-.665.665-.665Z" />
    </svg>
  );
}

function WrapDisabledIcon({ className }: { className?: string }) {
  return (
    <svg className={className} fill="currentColor" height="20" viewBox="0 0 20 20" width="20" xmlns="http://www.w3.org/2000/svg">
      <path d="M15.672 2.668c.367 0 .665.298.665.665l-.002 13.333a.665.665 0 0 1-1.33 0l.002-13.333c0-.367.298-.665.665-.665ZM9.586 6.002a3.582 3.582 0 0 1 0 7.163H5.777l.949.948a.665.665 0 1 1-.94.94l-2.084-2.082a.667.667 0 0 1 0-.94l2.083-2.085a.666.666 0 0 1 .94.942l-.947.947h3.808a2.251 2.251 0 0 0 0-4.503H4.169a.666.666 0 0 1 0-1.33h5.417Z" />
    </svg>
  );
}

function allChildrenAreTaskItems(children: ReactNode): boolean {
  const items = Children.toArray(children).filter(
    (child): child is ReactElement<{ className?: string }> => isValidElement<{ className?: string }>(child),
  );
  return items.length > 0 && items.every((item) => String(item.props.className ?? "").includes("task-list-item"));
}

function textFromChildren(children: ReactNode): string {
  return Children.toArray(children)
    .map((child) => {
      if (typeof child === "string" || typeof child === "number") return String(child);
      return isValidElement<{ children?: ReactNode }>(child) ? textFromChildren(child.props.children) : "";
    })
    .join("");
}

function markdownFilePath(href: string): string | null {
  if (!href || href.startsWith("#") || href.startsWith("//")) return null;
  let path = href;
  try {
    if (/^file:/i.test(href)) {
      const url = new URL(href);
      if (url.hostname || url.search || url.hash) return null;
      path = url.pathname.replace(/^\/(?=[A-Za-z]:\/)/, "");
    } else if (/^[A-Za-z][A-Za-z\d+.-]*:/.test(href) && !/^[A-Za-z]:[\\/]/.test(href)) {
      return null;
    }
    path = decodeURIComponent(path);
  } catch {
    return null;
  }
  return path.includes("\0") ? null : path;
}

function safeUrlTransform(url: string): string {
  if (markdownFilePath(url)) return url;
  const transformed = defaultUrlTransform(url);
  if (!transformed) return "";
  if (transformed.startsWith("#")) return transformed;
  try {
    const parsed = new URL(transformed);
    return parsed.protocol === "https:" || parsed.protocol === "http:" ? transformed : "";
  } catch {
    return "";
  }
}
