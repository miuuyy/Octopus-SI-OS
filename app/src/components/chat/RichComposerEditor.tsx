import { forwardRef, useEffect, useImperativeHandle, useLayoutEffect, useRef, useState, type RefObject } from "react";
import { baseKeymap, toggleMark } from "prosemirror-commands";
import { history, redo, undo } from "prosemirror-history";
import { InputRule, inputRules, wrappingInputRule } from "prosemirror-inputrules";
import { keymap } from "prosemirror-keymap";
import { defaultMarkdownParser, defaultMarkdownSerializer, schema } from "prosemirror-markdown";
import { Slice, type MarkType } from "prosemirror-model";
import { Plugin, TextSelection, EditorState } from "prosemirror-state";
import { liftListItem, sinkListItem } from "prosemirror-schema-list";
import { Decoration, DecorationSet, EditorView } from "prosemirror-view";
import { createPortal } from "react-dom";
import { composerSuggestionKey, composerSuggestionPlugin, visibleComposerSuggestion, type ComposerSuggestion } from "./composerSuggestion";
import { cancelComposerDictation, composerDictationKey, composerDictationPlugin, finishComposerDictation, startComposerDictation, updateComposerDictation } from "./composerDictation";
import { useShouldReduceMotion } from "../../appearance";

export type RichComposerEditorHandle = {
  dom: () => HTMLElement | null;
  focus: () => void;
  insertText: (value: string) => void;
  startDictation: () => void;
  updateDictation: (value: string) => void;
  cancelDictation: () => void;
  finishDictation: (value: string) => string;
};

type Props = {
  ariaActiveDescendant?: string;
  ariaControls?: string;
  className: string;
  onFilesPicked: (files: readonly File[]) => void;
  onKeyDown: (event: globalThis.KeyboardEvent, editor: HTMLElement) => void;
  onTextChange: (value: string) => void;
  placeholder: string;
  suggestion?: ComposerSuggestion | null;
  spellCheck: boolean;
  text: string;
};

type ActiveLink = {
  displayText: string;
  from: number;
  href: string;
  left: number;
  to: number;
  top: number;
};

const PASTED_TEXT_ATTACHMENT_THRESHOLD = 5_000;

export const RichComposerEditor = forwardRef<RichComposerEditorHandle, Props>(function RichComposerEditor({
  ariaActiveDescendant,
  ariaControls,
  className,
  onFilesPicked,
  onKeyDown,
  onTextChange,
  placeholder,
  suggestion = null,
  spellCheck,
  text,
}, forwardedRef) {
  const mountRef = useRef<HTMLDivElement>(null);
  const popoverRef = useRef<HTMLFormElement>(null);
  const viewRef = useRef<EditorView | null>(null);
  const reducedMotion = useShouldReduceMotion();
  const [activeLink, setActiveLink] = useState<ActiveLink | null>(null);
  const onFilesPickedRef = useRef(onFilesPicked);
  const onKeyDownRef = useRef(onKeyDown);
  const onTextChangeRef = useRef(onTextChange);
  const placeholderRef = useRef(placeholder);
  const activateLinkRef = useRef((anchor: HTMLAnchorElement, view: EditorView) => {
    const from = view.posAtDOM(anchor, 0);
    const to = view.posAtDOM(anchor, anchor.childNodes.length);
    if (to <= from) return;
    const anchorRect = anchor.getBoundingClientRect();
    const width = Math.min(448, Math.max(280, window.innerWidth - 32));
    view.dispatch(view.state.tr.setSelection(TextSelection.create(view.state.doc, from, to)));
    setActiveLink({
      displayText: anchor.textContent ?? "",
      from,
      href: anchor.href,
      left: Math.max(16, Math.min(window.innerWidth - width - 16, anchorRect.left)),
      to,
      top: anchorRect.top - 8,
    });
  });
  onFilesPickedRef.current = onFilesPicked;
  onKeyDownRef.current = onKeyDown;
  onTextChangeRef.current = onTextChange;
  placeholderRef.current = placeholder;

  useImperativeHandle(forwardedRef, () => ({
    dom: () => viewRef.current?.dom ?? null,
    focus: () => viewRef.current?.focus(),
    insertText: (value) => {
      const view = viewRef.current;
      if (!view || !value) return;
      view.dispatch(view.state.tr.insertText(value).scrollIntoView());
      view.focus();
    },
    startDictation: () => { const view = viewRef.current; if (view) startComposerDictation(view); },
    updateDictation: (value) => { const view = viewRef.current; if (view) updateComposerDictation(view, value); },
    cancelDictation: () => { const view = viewRef.current; if (view) cancelComposerDictation(view); },
    finishDictation: (value) => {
      const view = viewRef.current;
      if (!view) return "";
      finishComposerDictation(view, value);
      return serializePrompt(view.state);
    },
  }), []);

  useLayoutEffect(() => {
    const mount = mountRef.current;
    if (!mount) return;
    const plugins = composerPlugins(placeholderRef, onKeyDownRef, onFilesPickedRef, activateLinkRef);
    const state = createEditorState(text, plugins);
    const view = new EditorView(mount, {
      state,
      dispatchTransaction(transaction) {
        const next = view.state.apply(transaction);
        view.updateState(next);
        if (transaction.docChanged) {
          setActiveLink(null);
          onTextChangeRef.current(serializePrompt(next));
        }
      },
    });
    view.dom.dataset.octopusComposer = "true";
    view.dom.dataset.virtualkeyboard = "true";
    view.dom.setAttribute("aria-label", placeholderRef.current);
    view.dom.setAttribute("role", "textbox");
    view.dom.setAttribute("aria-multiline", "true");
    view.dom.spellcheck = spellCheck;
    view.dom.translate = false;
    view.dom.style.fontSize = "var(--octopus-chat-font-size)";
    view.dom.style.height = "auto";
    view.dom.style.resize = "none";
    viewRef.current = view;
    return () => {
      viewRef.current = null;
      view.destroy();
    };
  }, []);

  useLayoutEffect(() => {
    const view = viewRef.current;
    if (!view || serializePrompt(view.state) === text) return;
    const suggestionState = composerSuggestionKey.getState(view.state);
    const next = createEditorState(text, view.state.plugins);
    view.updateState(suggestionState ? next.apply(next.tr.setMeta(composerSuggestionKey, suggestionState)) : next);
  }, [text]);

  useEffect(() => {
    const view = viewRef.current;
    if (!view) return;
    view.dom.spellcheck = spellCheck;
  }, [spellCheck]);

  useEffect(() => {
    const view = viewRef.current;
    if (!view) return;
    if (ariaControls) view.dom.setAttribute("aria-controls", ariaControls);
    else view.dom.removeAttribute("aria-controls");
    if (ariaActiveDescendant) view.dom.setAttribute("aria-activedescendant", ariaActiveDescendant);
    else view.dom.removeAttribute("aria-activedescendant");
  }, [ariaActiveDescendant, ariaControls]);

  useEffect(() => {
    const view = viewRef.current;
    if (!view) return;
    view.dom.setAttribute("aria-label", placeholder);
    view.dispatch(view.state.tr.setMeta("composer-placeholder", placeholder));
  }, [placeholder]);

  useLayoutEffect(() => {
    const view = viewRef.current;
    if (view) view.dispatch(view.state.tr.setMeta(composerSuggestionKey, { suggestion }));
  }, [suggestion]);

  useEffect(() => {
    if (!activeLink) return;
    const close = (event: PointerEvent) => {
      if (!(event.target instanceof Node)) return;
      if (popoverRef.current?.contains(event.target)) return;
      setActiveLink(null);
    };
    window.addEventListener("pointerdown", close);
    return () => window.removeEventListener("pointerdown", close);
  }, [activeLink]);

  return (
    <div
      className={`${className} relative`}
      data-dictation-reduced-motion={reducedMotion ? "true" : undefined}
      onMouseDown={(event) => {
        const editor = viewRef.current?.dom;
        if (!editor || event.target instanceof Node && (editor.contains(event.target) || popoverRef.current?.contains(event.target))) return;
        event.preventDefault();
        viewRef.current?.focus();
      }}
    >
      <div ref={mountRef} />
      {activeLink ? createPortal(
        <ComposerLinkPopover
          activeLink={activeLink}
          formRef={popoverRef}
          onClose={() => {
            setActiveLink(null);
            viewRef.current?.focus();
          }}
          onSave={(displayText, href) => {
            const view = viewRef.current;
            if (!view) return;
            const from = Math.min(activeLink.from, view.state.doc.content.size);
            const originalTo = Math.min(activeLink.to, view.state.doc.content.size);
            let transaction = view.state.tr.insertText(displayText, from, originalTo);
            const to = from + displayText.length;
            transaction = transaction.removeMark(from, to, schema.marks.link);
            if (href) transaction = transaction.addMark(from, to, schema.marks.link.create({ href }));
            transaction = transaction.setSelection(TextSelection.create(transaction.doc, to));
            view.dispatch(transaction.scrollIntoView());
            view.focus();
          }}
        />,
        document.body,
      ) : null}
    </div>
  );
});

function ComposerLinkPopover({ activeLink, formRef, onClose, onSave }: {
  activeLink: ActiveLink;
  formRef: RefObject<HTMLFormElement | null>;
  onClose: () => void;
  onSave: (displayText: string, href: string | null) => void;
}) {
  const [displayText, setDisplayText] = useState(activeLink.displayText);
  const [href, setHref] = useState(activeLink.href);
  const [error, setError] = useState("");
  return (
    <form
      aria-label="Link options"
      className="fixed z-[120] grid w-[min(28rem,calc(100vw-2rem))] grid-cols-[minmax(0,0.8fr)_minmax(0,1.2fr)_auto] items-end gap-2 rounded-xl border border-token-border bg-token-dropdown-background/95 p-2 shadow-xl backdrop-blur-xl"
      onKeyDown={(event) => {
        if (event.key !== "Escape") return;
        event.preventDefault();
        onClose();
      }}
      onSubmit={(event) => {
        event.preventDefault();
        const text = displayText.trim();
        const normalized = normalizedHttpUrl(href);
        if (!text) {
          setError("Link text is required");
          return;
        }
        if (!normalized) {
          setError("Enter an HTTP or HTTPS link");
          return;
        }
        onSave(text, normalized);
      }}
      ref={formRef}
      style={{ left: activeLink.left, top: activeLink.top, transform: "translateY(-100%)" }}
    >
      <label className="flex min-w-0 flex-col gap-1 text-[10px] text-token-description-foreground">
        Text
        <input
          autoFocus
          className="h-8 min-w-0 rounded-lg border border-token-border bg-token-input-background px-2 text-xs text-token-foreground outline-none focus:border-token-focus-border"
          onChange={(event) => {
            setDisplayText(event.currentTarget.value);
            setError("");
          }}
          value={displayText}
        />
      </label>
      <label className="flex min-w-0 flex-col gap-1 text-[10px] text-token-description-foreground">
        URL
        <input
          aria-invalid={Boolean(error)}
          className="h-8 min-w-0 rounded-lg border border-token-border bg-token-input-background px-2 text-xs text-token-foreground outline-none focus:border-token-focus-border aria-invalid:border-token-input-validation-error-border"
          onChange={(event) => {
            setHref(event.currentTarget.value);
            setError("");
          }}
          type="url"
          value={href}
        />
      </label>
      <div className="flex items-center gap-1 pb-px">
        <button
          className="h-8 rounded-lg px-2 text-xs text-token-text-secondary hover:bg-token-list-hover-background focus-visible:outline focus-visible:outline-1 focus-visible:outline-token-focus-border"
          onClick={() => onSave(displayText.trim() || activeLink.displayText, null)}
          type="button"
        >Remove</button>
        <button
          className="h-8 rounded-lg bg-token-foreground px-2.5 text-xs text-token-dropdown-background focus-visible:outline focus-visible:outline-2 focus-visible:outline-token-focus-border"
          type="submit"
        >Save</button>
      </div>
      {error ? <span className="col-span-3 text-[10px] text-token-error-foreground" role="alert">{error}</span> : null}
    </form>
  );
}

function createEditorState(text: string, plugins: readonly Plugin[]): EditorState {
  const doc = parsePrompt(text);
  return EditorState.create({
    doc,
    plugins,
    selection: TextSelection.atEnd(doc),
    schema,
  });
}

function serializePrompt(state: EditorState): string {
  if (state.doc.content.content.every((node) => node.type === schema.nodes.paragraph)) {
    return state.doc.content.content
      .map((node) => defaultMarkdownSerializer.serialize(schema.topNodeType.create(null, node)).replace(/\n$/, ""))
      .join("\n");
  }
  return defaultMarkdownSerializer.serialize(state.doc).replace(/\n$/, "");
}

function parsePrompt(value: string) {
  if (looksLikeStructuredMarkdown(value)) return defaultMarkdownParser.parse(value);
  const paragraphs = value.split("\n").map((line) => schema.nodes.paragraph.create(
    null,
    line ? schema.text(line) : undefined,
  ));
  return schema.topNodeType.create(null, paragraphs);
}

function composerPlugins(
  placeholderRef: RefObject<string>,
  onKeyDownRef: RefObject<Props["onKeyDown"]>,
  onFilesPickedRef: RefObject<Props["onFilesPicked"]>,
  activateLinkRef: RefObject<(anchor: HTMLAnchorElement, view: EditorView) => void>,
): Plugin[] {
  return [
    composerDictationPlugin(),
    composerSuggestionPlugin(parsePrompt),
    history(),
    inputRules({
      rules: [
        markInputRule(/(?:^|\s)(\*\*\*([^*\n]+)\*\*\*)$/, [schema.marks.strong, schema.marks.em]),
        markInputRule(/(?:^|\s)(___([^_\n]+)___)$/, [schema.marks.strong, schema.marks.em]),
        markInputRule(/(?:^|\s)(\*\*([^*\n]+)\*\*)$/, [schema.marks.strong]),
        markInputRule(/(?:^|\s)(__([^_\n]+)__)$/, [schema.marks.strong]),
        markInputRule(/(?:^|\s)(\*([^*\n]+)\*)$/, [schema.marks.em]),
        markInputRule(/(?:^|\s)(_([^_\n]+)_)$/, [schema.marks.em]),
        wrappingInputRule(/^\s*([-+*])\s$/, schema.nodes.bullet_list),
        wrappingInputRule(
          /^(\d+)\.\s$/,
          schema.nodes.ordered_list,
          (match) => ({ order: Number(match[1]) }),
          (match, node) => node.childCount + node.attrs.order === Number(match[1]),
        ),
      ],
    }),
    keymap({
      "Shift-Enter": baseKeymap.Enter,
      "Mod-b": toggleMark(schema.marks.strong),
      "Mod-i": toggleMark(schema.marks.em),
      "Mod-z": undo,
      "Mod-y": redo,
      "Mod-Shift-z": redo,
      Tab: sinkListItem(schema.nodes.list_item),
      "Shift-Tab": liftListItem(schema.nodes.list_item),
      "Mod-[": liftListItem(schema.nodes.list_item),
      "Mod-]": sinkListItem(schema.nodes.list_item),
    }),
    keymap(baseKeymap),
    new Plugin({
      props: {
        clipboardTextSerializer(content) {
          return defaultMarkdownSerializer.serialize(schema.topNodeType.create(null, content.content));
        },
        decorations(state) {
          if (visibleComposerSuggestion(state) || composerDictationKey.getState(state)?.text) return null;
          const first = state.doc.firstChild;
          if (state.doc.childCount !== 1 || !first?.isTextblock || first.content.size !== 0) return null;
          return DecorationSet.create(state.doc, [Decoration.node(0, first.nodeSize, {
            class: "placeholder",
            "data-placeholder": placeholderRef.current ?? "",
          })]);
        },
        handleDOMEvents: {
          keydown(view, event) {
            onKeyDownRef.current?.(event, view.dom);
            return event.defaultPrevented;
          },
        },
        handleClick(view, _position, event) {
          const target = event.target instanceof Element ? event.target.closest("a[href]") : null;
          if (!(target instanceof HTMLAnchorElement) || !view.dom.contains(target)) return false;
          event.preventDefault();
          activateLinkRef.current?.(target, view);
          return true;
        },
        handlePaste(view, event) {
          return handleComposerPaste(view, event, onFilesPickedRef.current ?? (() => undefined));
        },
      },
    }),
  ];
}

function markInputRule(expression: RegExp, marks: readonly MarkType[]): InputRule {
  return new InputRule(expression, (state, match, start, end) => {
    const wrapped = match[1];
    const content = match[2];
    if (!wrapped || content == null) return null;
    const from = start + match[0].lastIndexOf(wrapped);
    let transaction = state.tr.delete(from, end).insertText(content, from);
    for (const mark of marks) {
      transaction = transaction
        .addMark(from, from + content.length, mark.create())
        .removeStoredMark(mark);
    }
    return transaction;
  });
}

function handleComposerPaste(
  view: EditorView,
  event: ClipboardEvent,
  onFilesPicked: Props["onFilesPicked"],
): boolean {
  const clipboard = event.clipboardData;
  if (!clipboard) return false;
  const files = Array.from(clipboard.files);
  const imageFiles = files.filter((file) => file.type.startsWith("image/"));
  const otherFiles = files.filter((file) => !file.type.startsWith("image/"));
  const plainText = clipboard.getData("text/plain");
  const html = clipboard.getData("text/html");
  const fileNames = new Set(files.map((file) => file.name));
  const hasSubstantiveText = plainText.split(/\r?\n/).some((line) => {
    const value = line.trim();
    return value !== "" && !fileNames.has(value.replace(/^.*[\\/]/, ""));
  });
  if (otherFiles.length > 0 || imageFiles.length > 0 && (!hasSubstantiveText || htmlContainsOnlyImages(html))) {
    onFilesPicked(files);
    return true;
  }
  if (!plainText) return false;
  if (plainText.length >= PASTED_TEXT_ATTACHMENT_THRESHOLD) {
    onFilesPicked([new File(
      [plainText],
      `pasted-text-${new Date().toISOString().replace(/[:.]/g, "-")}.txt`,
      { type: "text/plain", lastModified: Date.now() },
    )]);
    return true;
  }
  const link = normalizedHttpUrl(plainText);
  if (link && !view.state.selection.empty && selectionContainsTextOnly(view.state)) {
    const { from, to } = view.state.selection;
    view.dispatch(view.state.tr.addMark(from, to, schema.marks.link.create({ href: link })));
    return true;
  }
  if (looksLikeStructuredMarkdown(plainText)) {
    const parsed = defaultMarkdownParser.parse(plainText);
    view.dispatch(view.state.tr.replaceSelection(Slice.maxOpen(parsed.content, true)).scrollIntoView());
    return true;
  }
  return false;
}

function selectionContainsTextOnly(state: EditorState): boolean {
  let containsText = false;
  let containsInlineNode = false;
  state.selection.content().content.descendants((node) => {
    if (node.isText) containsText ||= node.textContent.trim() !== "";
    else if (node.isInline) containsInlineNode = true;
  });
  return containsText && !containsInlineNode;
}

function normalizedHttpUrl(value: string): string | null {
  const candidate = value.trim();
  if (!candidate || /\s/.test(candidate)) return null;
  try {
    const url = new URL(candidate);
    return url.protocol === "http:" || url.protocol === "https:" ? url.href : null;
  } catch {
    return null;
  }
}

function looksLikeStructuredMarkdown(value: string): boolean {
  return /(?:^|\n)\s*(?:[-+*]\s+|\d+\.\s+|#{1,6}\s+|>\s+|```)|\[[^\]\n]+\]\(https?:\/\/[^)\n]+\)|(?:^|\s)(?:\*\*[^*\n]+\*\*|__[^_\n]+__)/m.test(value);
}

function htmlContainsOnlyImages(value: string): boolean {
  if (!value || value.length > 250_000) return false;
  const template = document.createElement("template");
  template.innerHTML = value;
  if (!template.content.querySelector("img")) return false;
  const walker = document.createTreeWalker(template.content, NodeFilter.SHOW_TEXT);
  for (let node = walker.nextNode(); node; node = walker.nextNode()) {
    if (node.textContent?.trim()) return false;
  }
  return true;
}
