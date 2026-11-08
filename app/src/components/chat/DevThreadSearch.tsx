import { useCallback, useEffect, useMemo, useRef, useState, type RefObject } from "react";
import type { ChatRenderMessage } from "./types";

export type DevThreadSearchHost = {
  detachFromLatest: () => void;
  revealTurn: (turnKey: string) => Promise<void>;
};

type Hit = {
  turnKey: string;
  messageId: string;
  family: "reasoning" | "other";
  pre: string;
  post: string;
  preview: string;
};

type DomMatch = { range: Range; exact: boolean };

const HIGHLIGHT_NAME = "octopus-dev-thread-search";
const CONTEXT_CHARS = 32;
const CONTEXT_MATCH_CHARS = 14;
const MAX_EXPANSION_DEPTH = 6;
type SearchInstance = { list: () => HTMLElement | null; open: () => void };
const registry: { installed: boolean; instances: SearchInstance[] } = ((globalThis as { __octopusDevThreadSearch?: { installed: boolean; instances: SearchInstance[] } })
  .__octopusDevThreadSearch ??= { installed: false, instances: [] });
const instances = registry.instances;

export function DevThreadSearch({
  host,
  listRef,
  turnKeyOf,
  turns,
}: {
  host: DevThreadSearchHost;
  listRef: RefObject<HTMLDivElement | null>;
  turnKeyOf: (turn: readonly ChatRenderMessage[], index: number) => string;
  turns: readonly ChatRenderMessage[][];
}) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [debouncedQuery, setDebouncedQuery] = useState("");
  const [activeIndex, setActiveIndex] = useState(-1);
  const [status, setStatus] = useState("");
  const inputRef = useRef<HTMLInputElement>(null);
  const navigationRef = useRef(0);

  const openSearch = useCallback(() => {
    setOpen(true);
    window.requestAnimationFrame(() => {
      inputRef.current?.focus();
      inputRef.current?.select();
    });
  }, []);

  useEffect(() => {
    ensureGlobalShortcut();
    const entry = { list: () => listRef.current, open: openSearch };
    instances.push(entry);
    return () => {
      const index = instances.indexOf(entry);
      if (index >= 0) instances.splice(index, 1);
    };
  }, [listRef, openSearch]);

  useEffect(() => {
    const timer = window.setTimeout(() => setDebouncedQuery(query), 150);
    return () => window.clearTimeout(timer);
  }, [query]);

  const hits = useMemo(() => (open ? searchTurns(turns, turnKeyOf, debouncedQuery) : []), [debouncedQuery, open, turnKeyOf, turns]);

  useEffect(() => {
    setActiveIndex(hits.length ? 0 : -1);
    setStatus("");
    if (!hits.length) clearHighlight();
  }, [hits]);

  useEffect(() => {
    const hit = hits[activeIndex];
    if (!open || !hit) return;
    const navigation = ++navigationRef.current;
    const current = () => navigation === navigationRef.current;
    setStatus("…");
    void revealHit(hit, debouncedQuery, host, listRef, current).then((result) => {
      if (current()) setStatus(result);
    });
  }, [activeIndex, debouncedQuery, hits, host, listRef, open]);

  const close = useCallback(() => {
    navigationRef.current += 1;
    clearHighlight();
    setOpen(false);
    setStatus("");
  }, []);

  useEffect(() => {
    if (!open) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      event.stopImmediatePropagation();
      close();
    };
    window.addEventListener("keydown", onKeyDown, true);
    return () => window.removeEventListener("keydown", onKeyDown, true);
  }, [close, open]);

  useEffect(() => () => clearHighlight(), []);

  if (!open) return null;
  const move = (direction: 1 | -1) => {
    if (!hits.length) return;
    setActiveIndex((index) => (index + direction + hits.length) % hits.length);
  };
  const hit = hits[activeIndex];
  return (
    <div
      data-dev-thread-search="true"
      data-hit-message-id={hit?.messageId}
      data-hit-turn-key={hit?.turnKey}
      style={{
        position: "absolute",
        top: 8,
        right: 16,
        zIndex: 70,
        display: "flex",
        flexDirection: "column",
        gap: 4,
        width: 380,
        padding: 6,
        borderRadius: 10,
        background: "var(--color-token-dropdown-background, #222)",
        color: "var(--color-token-foreground, #eee)",
        boxShadow: "0 6px 24px rgba(0,0,0,0.25), 0 0 0 0.5px var(--color-token-border, rgba(128,128,128,0.4))",
        fontSize: 13,
      }}
    >
      <style>{`::highlight(${HIGHLIGHT_NAME}) { background-color: rgba(255, 196, 0, 0.55); color: inherit; }`}</style>
      <div style={{ display: "flex", alignItems: "center", gap: 4 }}>
        <input
          aria-label="Search conversation"
          onChange={(event) => setQuery(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === "Enter") {
              event.preventDefault();
              move(event.shiftKey ? -1 : 1);
            }
          }}
          placeholder="Search messages, thoughts, tools"
          ref={inputRef}
          spellCheck={false}
          style={{ flex: 1, minWidth: 0, background: "transparent", border: "none", outline: "none", color: "inherit", padding: "4px 6px" }}
          value={query}
        />
        <span style={{ whiteSpace: "nowrap", fontVariantNumeric: "tabular-nums", opacity: 0.7, fontSize: 12 }}>
          {debouncedQuery.trim() ? `${hits.length ? activeIndex + 1 : 0} / ${hits.length}` : ""}
        </span>
        <button aria-label="Previous match" disabled={!hits.length} onClick={() => move(-1)} style={buttonStyle} type="button">↑</button>
        <button aria-label="Next match" disabled={!hits.length} onClick={() => move(1)} style={buttonStyle} type="button">↓</button>
        <button aria-label="Close search" onClick={close} style={buttonStyle} type="button">✕</button>
      </div>
      {hit ? (
        <div style={{ padding: "0 6px 2px", fontSize: 11, opacity: 0.65, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
          {hit.family === "reasoning" ? "Thought · " : ""}{hit.preview}{status && status !== "found" ? ` · ${status}` : ""}
        </div>
      ) : null}
    </div>
  );
}

const buttonStyle = {
  background: "transparent",
  border: "none",
  color: "inherit",
  cursor: "pointer",
  padding: "2px 6px",
  borderRadius: 6,
} as const;

function ensureGlobalShortcut() {
  if (registry.installed) return;
  registry.installed = true;
  window.addEventListener("keydown", (event) => {
    if (!(event.metaKey || event.ctrlKey) || event.altKey || event.shiftKey || event.code !== "KeyF") return;
    const focused = document.activeElement;
    const visible = instances.filter((entry) => {
      const list = entry.list();
      return Boolean(list?.isConnected && list.getClientRects().length);
    });
    const target = visible.find((entry) => focused && entry.list()?.parentElement?.contains(focused)) ?? visible[0];
    if (!target) return;
    event.preventDefault();
    event.stopImmediatePropagation();
    target.open();
  }, true);
}

function clearHighlight() {
  globalThis.CSS?.highlights?.delete(HIGHLIGHT_NAME);
}

function searchTurns(
  turns: readonly ChatRenderMessage[][],
  turnKeyOf: (turn: readonly ChatRenderMessage[], index: number) => string,
  rawQuery: string,
): Hit[] {
  const needles = queryVariants(rawQuery);
  if (!needles.length) return [];
  const hits: Hit[] = [];
  turns.forEach((turn, turnIndex) => {
    const key = turnKeyOf(turn, turnIndex);
    const seen = new Set<string>();
    for (const message of turn) {
      const family = message.eventKind === "reasoning" || message.eventKind === "reasoning_delta" ? "reasoning" : "other";
      for (const { display, field } of messageFields(message)) {
        const source = display.length === field.length ? display : field;
        for (const needle of needles) {
          let cursor = field.indexOf(needle);
          while (cursor >= 0) {
            const before = field.slice(Math.max(0, cursor - CONTEXT_CHARS), cursor);
            const after = field.slice(cursor + needle.length, cursor + needle.length + CONTEXT_CHARS);
            const pre = alnum(before);
            const post = alnum(after);
            const identity = `${pre.slice(-CONTEXT_MATCH_CHARS)}|${post.slice(0, CONTEXT_MATCH_CHARS)}`;
            if (!seen.has(identity)) {
              seen.add(identity);
              hits.push({
                turnKey: key,
                messageId: message.id,
                family,
                pre,
                post,
                preview: source.slice(Math.max(0, cursor - CONTEXT_CHARS), cursor + needle.length + CONTEXT_CHARS).trim(),
              });
            }
            cursor = field.indexOf(needle, cursor + Math.max(1, needle.length));
          }
        }
      }
    }
  });
  return hits;
}

function queryVariants(rawQuery: string): string[] {
  const base = normalize(rawQuery).trim();
  if (!base) return [];
  const variants = new Set([base]);
  const unescaped = normalize(rawQuery.replace(/\\(["'\\/])/g, "$1").replace(/\\n/g, " ")).trim();
  if (unescaped) variants.add(unescaped);
  return [...variants];
}

function messageFields(message: ChatRenderMessage): Array<{ display: string; field: string }> {
  const fields: Array<{ display: string; field: string }> = [];
  const add = (value: string) => {
    const display = displayData(value);
    const normalized = display.toLocaleLowerCase();
    if (!normalized.trim()) return;
    if (fields.some(({ field }) => field.includes(normalized))) return;
    fields.push({ display, field: normalized });
  };
  add(message.text);
  if (message.label) add(message.label);
  collectStrings(message.eventData, 0, add);
  return fields;
}

function collectStrings(value: unknown, depth: number, add: (value: string) => void) {
  if (depth > 8 || value == null) return;
  if (typeof value === "string") {
    add(value);
    if (/^\s*[[{]/.test(value)) {
      try {
        collectStrings(JSON.parse(value), depth + 1, add);
      } catch {
      }
    }
    return;
  }
  if (Array.isArray(value)) {
    for (const item of value) collectStrings(item, depth + 1, add);
    return;
  }
  if (typeof value === "object") {
    for (const item of Object.values(value as Record<string, unknown>)) collectStrings(item, depth + 1, add);
  }
}

function normalize(value: string): string {
  return foldPunctuation(value.toLocaleLowerCase());
}

function foldPunctuation(value: string): string {
  return value
    .replace(/[\u2018\u2019\u201B\u02BC]/g, "'")
    .replace(/[\u201C\u201D\u201F]/g, "\"")
    .replace(/\s+/g, " ");
}

function displayData(value: string): string {
  return foldPunctuation(value.replace(/\*\*|__|`+/g, ""));
}

function alnum(value: string): string {
  return value.toLocaleLowerCase().replace(/[^\p{L}\p{N}]+/gu, "");
}

async function revealHit(
  hit: Hit,
  rawQuery: string,
  host: DevThreadSearchHost,
  listRef: RefObject<HTMLDivElement | null>,
  current: () => boolean,
): Promise<string> {
  clearHighlight();
  const turnElement = () => findTurnElement(listRef.current, hit.turnKey);
  for (let attempt = 0; attempt < 2 && !turnElement(); attempt += 1) {
    await host.revealTurn(hit.turnKey);
    if (!current()) return "";
  }
  const turn = turnElement();
  if (!turn) return "turn did not mount";
  host.detachFromLatest();
  const needles = queryVariants(rawQuery);
  let match = locate(turn, hit, needles);
  const opened: HTMLElement[] = [];
  for (let depth = 0; !match?.exact && depth < MAX_EXPANSION_DEPTH; depth += 1) {
    const candidates = collapsedToggles(turn).filter((toggle) => !opened.includes(toggle));
    if (!candidates.length) break;
    for (const toggle of candidates) {
      toggle.click();
      opened.push(toggle);
    }
    await settle();
    if (!current()) return "";
    match = locate(turn, hit, needles) ?? match;
  }
  if (!match) {
    await collapse(opened, null);
    return "not rendered in this view";
  }
  await collapse(opened, match.range.startContainer);
  const showMore = match.range.startContainer.parentElement?.closest<HTMLElement>(".collapsible-message:not(.is-expanded)");
  const showMoreToggle = showMore?.querySelector<HTMLElement>(":scope > .collapsible-message-toggle");
  if (showMoreToggle) {
    showMoreToggle.click();
    await settle();
  }
  await delay(380);
  if (!current()) return "";
  const finalMatch = locate(turn, hit, needles) ?? match;
  host.detachFromLatest();
  centerRange(listRef.current, finalMatch.range);
  globalThis.CSS?.highlights?.set(HIGHLIGHT_NAME, new Highlight(finalMatch.range));
  await delay(120);
  if (current()) centerRange(listRef.current, finalMatch.range);
  return finalMatch.exact ? "found" : "found (context approximate)";
}

function findTurnElement(list: HTMLElement | null, turnKey: string): HTMLElement | null {
  if (!list) return null;
  const escaped = globalThis.CSS?.escape ? globalThis.CSS.escape(turnKey) : turnKey.replace(/["\\]/g, "\\$&");
  return list.querySelector<HTMLElement>(`[data-thread-turn-key="${escaped}"]`);
}

function collapsedToggles(root: HTMLElement): HTMLElement[] {
  return Array.from(root.querySelectorAll<HTMLElement>('button[aria-expanded="false"]:not([aria-haspopup])'))
    .filter((toggle) => !toggle.classList.contains("collapsible-message-toggle") && !hiddenNode(toggle));
}

function disclosureContainer(toggle: HTMLElement): HTMLElement | null {
  return toggle.closest<HTMLElement>(".octopus-agent-activity, .octopus-exec-activity, .activity-file-row, .activity-code-block, .octopus-turn-activity")
    ?? toggle.parentElement?.parentElement
    ?? null;
}

async function collapse(opened: HTMLElement[], keep: Node | null) {
  let changed = false;
  for (const toggle of [...opened].reverse()) {
    if (!toggle.isConnected || toggle.getAttribute("aria-expanded") !== "true") continue;
    if (keep && disclosureContainer(toggle)?.contains(keep)) continue;
    toggle.click();
    changed = true;
  }
  if (changed) await settle();
}

function hiddenNode(node: Node): boolean {
  const element = node instanceof Element ? node : node.parentElement;
  return Boolean(element?.closest('[aria-hidden="true"], [inert], style, script, [data-dev-thread-search]'));
}

function locate(root: HTMLElement, hit: Hit, needles: string[]): DomMatch | null {
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT, {
    acceptNode: (node) => (hiddenNode(node) ? NodeFilter.FILTER_REJECT : NodeFilter.FILTER_ACCEPT),
  });
  let text = "";
  const nodes: Text[] = [];
  const offsets: number[] = [];
  let lastWasSpace = false;
  for (let node = walker.nextNode(); node; node = walker.nextNode()) {
    const value = (node as Text).data;
    for (let index = 0; index < value.length; index += 1) {
      let char = normalize(value[index] ?? "");
      if (char === " ") {
        if (lastWasSpace) continue;
        lastWasSpace = true;
      } else {
        lastWasSpace = false;
      }
      char = char.slice(0, 1);
      text += char;
      nodes.push(node as Text);
      offsets.push(index);
    }
  }
  let fallback: DomMatch | null = null;
  for (const needle of needles) {
    let cursor = text.indexOf(needle);
    while (cursor >= 0) {
      const end = cursor + needle.length - 1;
      const range = document.createRange();
      range.setStart(nodes[cursor]!, offsets[cursor]!);
      range.setEnd(nodes[end]!, offsets[end]! + 1);
      const pre = alnum(text.slice(Math.max(0, cursor - CONTEXT_CHARS * 3), cursor));
      const post = alnum(text.slice(end + 1, end + 1 + CONTEXT_CHARS * 3));
      const wantPre = hit.pre.slice(-CONTEXT_MATCH_CHARS);
      const wantPost = hit.post.slice(0, CONTEXT_MATCH_CHARS);
      if (pre.endsWith(wantPre) && post.startsWith(wantPost)) return { range, exact: true };
      fallback ??= { range, exact: false };
      cursor = text.indexOf(needle, cursor + Math.max(1, needle.length));
    }
  }
  return fallback;
}

function centerRange(list: HTMLElement | null, range: Range) {
  if (!list) return;
  const rect = range.getBoundingClientRect();
  const listRect = list.getBoundingClientRect();
  const delta = rect.top + rect.height / 2 - (listRect.top + listRect.height / 2);
  if (Math.abs(delta) > 1) list.scrollBy({ top: delta, behavior: "instant" });
}

function settle(): Promise<void> {
  return new Promise((resolve) => {
    const timer = window.setTimeout(resolve, 250);
    window.requestAnimationFrame(() => window.requestAnimationFrame(() => window.requestAnimationFrame(() => {
      window.clearTimeout(timer);
      resolve();
    })));
  });
}

function delay(ms: number): Promise<void> {
  return new Promise((resolve) => window.setTimeout(resolve, ms));
}
