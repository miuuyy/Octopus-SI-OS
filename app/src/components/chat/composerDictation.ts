import { Plugin, PluginKey, TextSelection, type EditorState } from "prosemirror-state";
import { Decoration, DecorationSet, type EditorView } from "prosemirror-view";

export type DictationWordArrival = {
  from: number;
  to: number;
  startedAt: number;
  delay: number;
};

export type DictationDraft = {
  from: number;
  to: number;
  text: string;
  hideInitialCaret: boolean;
  continuationPosition: number;
  appendAtEnd: boolean;
  arrivals: readonly DictationWordArrival[];
};

const WORD_REVEAL_DURATION_MS = 500;
const WORD_ARRIVAL_STEP_MS = 30;
const WORD_ARRIVAL_MAX_DELAY_MS = 120;

export const composerDictationKey = new PluginKey<DictationDraft | null>("composer-dictation");

export function composerDictationPlugin() {
  return new Plugin<DictationDraft | null>({
    key: composerDictationKey,
    state: {
      init: () => null,
      apply(transaction, current) {
        const update = transaction.getMeta(composerDictationKey) as DictationDraft | null | undefined;
        if (update !== undefined) return update;
        if (!current) return null;
        if (!transaction.docChanged) {
          return current.hideInitialCaret && transaction.selectionSet
            ? { ...current, hideInitialCaret: false }
            : current;
        }
        const resolved = transaction.doc.resolve(
          current.appendAtEnd
            ? transaction.doc.content.size
            : transaction.mapping.map(current.continuationPosition, 1),
        );
        const continuationPosition = resolved.parent.inlineContent
          ? resolved.pos
          : TextSelection.near(resolved, -1).to;
        const from = current.appendAtEnd ? continuationPosition : transaction.mapping.map(current.from, 1);
        const to = current.appendAtEnd ? continuationPosition : transaction.mapping.map(current.to, 1);
        return {
          ...current,
          from,
          to,
          hideInitialCaret: false,
          continuationPosition,
          arrivals: [],
        };
      },
    },
    props: {
      attributes(state): Record<string, string> {
        return composerDictationKey.getState(state)?.hideInitialCaret
          ? { style: "caret-color: transparent", "data-dictation-hide-caret": "true" }
          : {};
      },
      decorations(state) {
        const draft = composerDictationKey.getState(state);
        if (!draft?.text) return null;
        const { leading, trailing, value } = spacedTranscriptParts(state, draft);
        const lastStartedAt = draft.arrivals.at(-1)?.startedAt ?? 0;
        const key = `${draft.text}\0${lastStartedAt}\0${draft.arrivals.length}`;
        return DecorationSet.create(state.doc, [Decoration.widget(draft.from, () => {
          const span = document.createElement("span");
          span.className = "composer-dictation-draft";
          if (leading) span.appendChild(document.createTextNode(leading));
          appendArrivalSegments(span, value, draft.arrivals);
          if (trailing) span.appendChild(document.createTextNode(trailing));
          return span;
        }, { side: -1, key })]);
      },
    },
    view() {
      return {
        update(view, prevState) {
          const nextStartedAt = composerDictationKey.getState(view.state)?.arrivals.at(-1)?.startedAt;
          const prevStartedAt = composerDictationKey.getState(prevState)?.arrivals.at(-1)?.startedAt;
          if (nextStartedAt != null && nextStartedAt !== prevStartedAt) {
            for (const element of view.dom.querySelectorAll(`[data-dictation-arrival="${nextStartedAt}"]`)) {
              for (const animation of element.getAnimations?.() ?? []) {
                animation.currentTime = 0;
              }
            }
          }
        },
      };
    },
  });
}

function appendArrivalSegments(container: HTMLElement, text: string, arrivals: readonly DictationWordArrival[]) {
  if (!arrivals.length) {
    container.appendChild(document.createTextNode(text));
    return;
  }
  let cursor = 0;
  for (const arrival of arrivals) {
    const start = Math.max(cursor, Math.min(arrival.from, text.length));
    const end = Math.max(start, Math.min(arrival.to, text.length));
    if (start > cursor) container.appendChild(document.createTextNode(text.slice(cursor, start)));
    if (end > start) {
      const word = document.createElement("span");
      word.className = "composer-dictation-word-arrival";
      word.dataset.dictationArrival = String(arrival.startedAt);
      word.style.animationDelay = `${arrival.delay}ms`;
      word.textContent = text.slice(start, end);
      container.appendChild(word);
    }
    cursor = end;
  }
  if (cursor < text.length) container.appendChild(document.createTextNode(text.slice(cursor)));
}

function tokenizeArrivals(tail: string, baseOffset: number, startedAt: number): DictationWordArrival[] {
  if (!tail) return [];
  const arrivals: DictationWordArrival[] = [];
  let index = 0;
  const SegmenterCtor = typeof Intl !== "undefined" ? Intl.Segmenter : undefined;
  if (typeof SegmenterCtor === "function") {
    const segmenter = new SegmenterCtor(undefined, { granularity: "word" });
    for (const part of segmenter.segment(tail)) {
      if (!part.segment || /^\s+$/.test(part.segment)) continue;
      arrivals.push({
        from: baseOffset + part.index,
        to: baseOffset + part.index + part.segment.length,
        startedAt,
        delay: Math.min(WORD_ARRIVAL_STEP_MS * index++, WORD_ARRIVAL_MAX_DELAY_MS),
      });
    }
    return arrivals;
  }
  for (const match of tail.matchAll(/\S+/g)) {
    const matchIndex = match.index ?? 0;
    arrivals.push({
      from: baseOffset + matchIndex,
      to: baseOffset + matchIndex + match[0].length,
      startedAt,
      delay: Math.min(WORD_ARRIVAL_STEP_MS * index++, WORD_ARRIVAL_MAX_DELAY_MS),
    });
  }
  return arrivals;
}

function spacedTranscriptParts(state: EditorState, draft: Pick<DictationDraft, "from" | "to" | "text">) {
  const before = state.doc.textBetween(Math.max(0, draft.from - 1), draft.from);
  const after = state.doc.textBetween(draft.to, Math.min(state.doc.content.size, draft.to + 1));
  return {
    leading: before && !/\s$/.test(before) ? " " : "",
    value: draft.text,
    trailing: after && !/^\s/.test(after) ? " " : "",
  };
}

function spacedTranscript(state: EditorState, draft: Pick<DictationDraft, "from" | "to" | "text">) {
  const { leading, value, trailing } = spacedTranscriptParts(state, draft);
  return `${leading}${value}${trailing}`;
}

export function startComposerDictation(view: EditorView) {
  const { doc, selection } = view.state;
  const emptyDoc = doc.childCount === 1 && doc.firstChild?.isTextblock === true && doc.firstChild.content.size === 0;
  const next: DictationDraft = {
    from: selection.from,
    to: selection.to,
    text: "",
    hideInitialCaret: emptyDoc,
    continuationPosition: selection.to,
    appendAtEnd: emptyDoc,
    arrivals: [],
  };
  view.dispatch(view.state.tr.setMeta(composerDictationKey, next).setMeta("addToHistory", false));
}

export function updateComposerDictation(view: EditorView, text: string) {
  const draft = composerDictationKey.getState(view.state);
  if (!draft) return;
  const now = typeof performance !== "undefined" ? performance.now() : Date.now();
  const extendsPrevious = text.startsWith(draft.text);
  const kept = extendsPrevious
    ? draft.arrivals.filter((arrival) => now - arrival.startedAt < WORD_REVEAL_DURATION_MS + arrival.delay)
    : [];
  const added = extendsPrevious
    ? tokenizeArrivals(text.slice(draft.text.length), draft.text.length, now)
    : tokenizeArrivals(text, 0, now);
  const next: DictationDraft = {
    ...draft,
    text,
    arrivals: [...kept, ...added],
  };
  view.dispatch(view.state.tr.setMeta(composerDictationKey, next).setMeta("addToHistory", false).scrollIntoView());
}

export function cancelComposerDictation(view: EditorView) {
  view.dispatch(view.state.tr.setMeta(composerDictationKey, null).setMeta("addToHistory", false));
}

export function finishComposerDictation(view: EditorView, text: string) {
  const draft = composerDictationKey.getState(view.state) ?? {
    from: view.state.selection.from,
    to: view.state.selection.to,
    text: "",
    hideInitialCaret: false,
    continuationPosition: view.state.selection.to,
    appendAtEnd: false,
    arrivals: [],
  };
  const transaction = view.state.tr.setMeta(composerDictationKey, null);
  if (text.trim()) transaction.insertText(spacedTranscript(view.state, { ...draft, text }), draft.from, draft.to);
  view.dispatch(transaction.scrollIntoView());
  view.focus();
}
