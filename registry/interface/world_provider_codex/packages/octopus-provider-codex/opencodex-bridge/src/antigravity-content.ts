import { createHash } from "node:crypto";
import { writeFile } from "node:fs/promises";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { canonicalClaudeJson } from "../node_modules/@bitkyc08/opencodex/src/adapters/claude-cli/stable-replay.ts";
import { namespacedToolName } from "../node_modules/@bitkyc08/opencodex/src/types/tools.ts";
import type { OcxMessage } from "../node_modules/@bitkyc08/opencodex/src/types.ts";
import release from "../antigravity-acp.json" with { type: "json" };

export type Row = Record<string, unknown>;
export const object = (value: unknown): Row | undefined =>
  value !== null && typeof value === "object" && !Array.isArray(value) ? value as Row : undefined;
export const digest = (value: unknown): string => createHash("sha256").update(JSON.stringify(value)).digest("hex");
export type ToolResult = Extract<OcxMessage, { role: "toolResult" }>;
type Media = { type: "media"; mimeType: string; data: string; extension: string };
type Part = { type: "text"; text: string } | Media;
type Content = Extract<OcxMessage, { role: "user" }>["content"];

const documentExtensions: Record<string, string> = {
  "application/pdf": "pdf", "application/json": "json", "text/plain": "txt", "text/css": "css",
  "text/csv": "csv", "text/html": "html", "text/javascript": "js", "text/xml": "xml", "text/rtf": "rtf",
};

function contentParts(content: Content): Part[] {
  if (typeof content === "string") return [{ type: "text", text: content }];
  return content.map(part => {
    if (part.type === "text") return part;
    if (part.type === "image") {
      const image = /^data:(image\/(?:png|jpeg|webp|bmp));base64,([A-Za-z0-9+/]+={0,2})$/.exec(part.imageUrl);
      if (image) return { type: "media", mimeType: image[1]!, data: image[2]!, extension: image[1]!.slice(6) };
      throw new Error("Antigravity requires an inline PNG, JPEG, WebP or BMP image.");
    }
    if (part.type === "document" && Object.hasOwn(documentExtensions, part.mediaType)) {
      return { type: "media", mimeType: part.mediaType, data: part.data, extension: documentExtensions[part.mediaType]! };
    }
    if (part.type === "audio") return { type: "media", mimeType: part.format === "mp3" ? "audio/mpeg" : "audio/wav", data: part.data, extension: part.format };
    throw new Error(`Antigravity cannot carry this ${part.type} input.`);
  });
}

function assistantParts(message: Extract<OcxMessage, { role: "assistant" }>) {
  return message.content.map(part => {
    if (part.type === "toolCall") return { type: part.type, id: part.id, name: namespacedToolName(part.namespace, part.name), arguments: canonicalClaudeJson(part.arguments) };
    if (part.type === "thinking") return { type: part.type, thinking: part.thinking };
    return { type: part.type, text: part.text };
  });
}

export function conversationMessages(messages: OcxMessage[]): OcxMessage[] {
  const normalized: OcxMessage[] = [];
  for (const message of messages) {
    if (message.role === "developer") continue;
    const previous = normalized.at(-1);
    if (message.role === "assistant" && previous?.role === "assistant") previous.content.push(...message.content);
    else normalized.push(message.role === "assistant" ? { ...message, content: [...message.content] } : message);
  }
  return normalized.filter(message => message.role !== "assistant" || message.content.some(part => part.type !== "thinking"));
}

export function messageKey(message: OcxMessage): string {
  if (message.role === "assistant") {
    const visible: ReturnType<typeof assistantParts> = [];
    for (const part of assistantParts(message)) {
      if (part.type === "thinking") continue;
      const previous = visible.at(-1);
      if (part.type === "text" && previous?.type === "text") previous.text += part.text!;
      else visible.push(part);
    }
    return digest([message.role, visible]);
  }
  return digest(message.role === "toolResult"
    ? [message.role, message.toolCallId, message.isError, contentParts(message.content)]
    : [message.role, contentParts(message.content)]);
}

function labelledParts(messages: OcxMessage[], vendorNames: ReadonlyMap<string, string>): Part[] {
  const parts: Part[] = [];
  const text = (value: string) => {
    const previous = parts.at(-1);
    if (previous?.type === "text") previous.text += value;
    else parts.push({ type: "text", text: value });
  };
  for (const message of messages) {
    if (message.role === "developer") continue;
    if (parts.length) text("\n\n");
    if (message.role === "assistant") {
      text("ASSISTANT:\n");
      const lines = assistantParts(message).map(part => {
        if (part.type === "toolCall") return `[Tool call: ${vendorNames.get(part.name!) ?? part.name} (call_id: ${part.id}) with args: ${JSON.stringify(part.arguments)}]`;
        if (part.type === "thinking") return `[Thinking: ${part.thinking}]`;
        return part.text;
      });
      text(lines.join("\n"));
    } else {
      text(message.role === "user" ? "USER:\n" : `TOOL RESULT (call_id: ${message.toolCallId})${message.isError ? " (error)" : ""}:\n`);
      for (const part of contentParts(message.content)) {
        if (part.type === "text") text(part.text);
        else parts.push(part);
      }
    }
  }
  return parts;
}

function textBytes(parts: Part[]): number {
  return parts.reduce((total, part) => total + (part.type === "text" ? Buffer.byteLength(part.text) : 0), 0);
}

export async function acpPrompt(messages: OcxMessage[], vendorNames: ReadonlyMap<string, string>, directory: string): Promise<Row[]> {
  const parts = labelledParts(messages, vendorNames);
  if (!parts.length) throw new Error("Antigravity needs a new input message.");
  const attachText = parts.some(part => part.type === "media") || textBytes(parts) + parts.length > release.inline_text_bytes.prompt;
  const blocks: Row[] = [];
  for (const part of parts) {
    if (part.type === "text" && !attachText) {
      // An embedded text resource is literal input. ACP does not interpret slash commands in it.
      blocks.push({ type: "resource", resource: { uri: `octopus:input/${blocks.length}`, mimeType: "text/plain", text: part.text } });
    } else if (part.type === "media" && (part.mimeType.startsWith("image/") || part.mimeType.startsWith("audio/"))) {
      blocks.push({ type: part.mimeType.split("/")[0], mimeType: part.mimeType, data: part.data });
    } else {
      const extension = part.type === "text" ? "txt" : part.extension;
      const name = `input-${blocks.length}.${extension}`;
      const path = join(directory, name);
      await writeFile(path, part.type === "text" ? part.text : Buffer.from(part.data, "base64"), { flag: "wx", mode: 0o600 });
      blocks.push({ type: "resource_link", uri: pathToFileURL(path).href, name, mimeType: part.type === "text" ? "text/plain" : part.mimeType });
    }
  }
  if (!blocks.some(block => block.type === "text" || block.type === "resource")) {
    blocks.unshift({ type: "text", text: "The ordered input is attached." });
  }
  return blocks;
}

export function mcpResult(message: ToolResult): Row {
  const parts = contentParts(message.content);
  const attachText = parts.some(part => part.type === "media") || textBytes(parts) + parts.length > release.inline_text_bytes.tool_result;
  const content = parts.map((part, index) => {
    if (part.type === "text" && !attachText) return part;
    if (part.type === "media" && (part.mimeType.startsWith("image/") || part.mimeType.startsWith("audio/"))) {
      return { type: part.mimeType.split("/")[0], mimeType: part.mimeType, data: part.data };
    }
    const mimeType = part.type === "text" ? "text/plain" : part.mimeType;
    const blob = part.type === "text" ? Buffer.from(part.text).toString("base64") : part.data;
    return { type: "resource", resource: { uri: `octopus:tool-result/${encodeURIComponent(message.toolCallId)}/${index}`, mimeType, blob } };
  });
  return { content, isError: message.isError };
}
