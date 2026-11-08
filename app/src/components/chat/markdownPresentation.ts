import { parse, postprocess, preprocess } from "micromark";
import { attention } from "micromark-core-commonmark";
import type { Extension, Token } from "micromark-util-types";
import type { Nodes } from "mdast";
import remarkParse from "remark-parse";
import remarkGfm from "remark-gfm";
import remarkMath from "remark-math";
import { unified, type PluggableList } from "unified";

export const MARKDOWN_REMARK_PLUGINS: PluggableList = [remarkGfm, [remarkMath, { singleDollarTextMath: false }]];

const markdownParser = unified().use(remarkParse).use(MARKDOWN_REMARK_PLUGINS).freeze();

export function prepareStreamingMarkdown(source: string): string {
  if (!source.includes("*")) return source;
  const openers = new Map<number, Token>();
  const completion: Extension = {
    text: {
      42: {
        ...attention,
        resolveAll(events, context) {
          const resolved = attention.resolveAll!(events, context);
          let depth = 0;
          for (const [direction, token] of resolved) {
            if (token.type === "strong" || token.type === "emphasis") {
              depth += direction === "enter" ? 1 : -1;
            }
            if (direction === "enter" && depth === 0 && token.type === "data"
              && token._open && !token._close && source[token.start.offset] === "*") {
              openers.set(token.start.offset, { ...token, start: { ...token.start }, end: { ...token.end } });
            }
          }
          return resolved;
        },
      },
    },
  };
  const events = postprocess(parse({
    extensions: [...(markdownParser.data("micromarkExtensions") ?? []), completion],
  }).document().write(preprocess()(source, undefined, true)));
  if (hasUnclosedCode(events, source)) return source;
  const end = source.trimEnd().length;
  let suffix = "";
  for (const opener of openers.values()) {
    const tail = source.slice(opener.end.offset);
    if (!tail || /^\s/.test(tail) || /[\r\n]/.test(tail)) continue;
    suffix += "*".repeat(opener.end.offset - opener.start.offset);
  }
  if (!suffix) return source;
  const presentation = source.slice(0, end) + suffix + source.slice(end);
  const parsed = postprocess(parse({
    extensions: markdownParser.data("micromarkExtensions") ?? [],
  }).document().write(preprocess()(presentation, undefined, true)));
  let consumed = 0;
  for (const [direction, token] of parsed) {
    if (direction !== "enter" || (token.type !== "strongSequence" && token.type !== "emphasisSequence")) continue;
    consumed += Math.max(0, Math.min(end + suffix.length, token.end.offset) - Math.max(end, token.start.offset));
  }
  return consumed === suffix.length ? presentation : source;
}

function hasUnclosedCode(events: ReturnType<typeof postprocess>, source: string): boolean {
  for (const [direction, token] of events) {
    if (direction !== "enter" || token.type !== "data") continue;
    for (let offset = token.start.offset; offset < token.end.offset; offset += 1) {
      if (source[offset] !== "`") continue;
      let escapes = 0;
      for (let previous = offset - 1; previous >= 0 && source[previous] === "\\"; previous -= 1) escapes += 1;
      if (escapes % 2 === 0) return true;
    }
  }
  return false;
}

export function markdownToPlainText(source: string): string {
  return plainText(markdownParser.parse(source));
}

function plainText(node: Nodes): string {
  if (node.type === "html" || node.type === "definition") return "";
  if (node.type === "break") return "\n";
  if ("value" in node) return node.value;
  if ("alt" in node) return node.alt ?? "";
  if (!("children" in node)) return "";
  const separator = node.type === "tableRow" ? "\t"
    : ["root", "blockquote", "list", "listItem", "table"].includes(node.type) ? "\n" : "";
  return node.children.map(plainText).filter((text) => text.length > 0).join(separator);
}
