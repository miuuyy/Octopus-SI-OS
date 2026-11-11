import type { OcxParsedRequest } from "../node_modules/@bitkyc08/opencodex/src/types.ts";
import { buildStableClaudeConversationInput } from "../node_modules/@bitkyc08/opencodex/src/adapters/claude-cli/stable-replay.ts";

function validateImage(url: string): void {
  if (/^data:(image\/(?:png|jpeg|gif|webp));base64,([A-Za-z0-9+/]+={0,2})$/.test(url)) return;
  let remote: URL;
  try { remote = new URL(url); } catch { throw new Error("Claude image has an invalid carrier."); }
  if (remote.protocol !== "https:" || remote.username || remote.password) throw new Error("Claude images require base64 data or an HTTPS URL without credentials.");
}

export function buildClaudeConversationInput(parsed: OcxParsedRequest, options: {
  maxHistoryChars?: number; vendorNameByWire?: ReadonlyMap<string, string>;
} = {}): string[] {
  for (const message of parsed.context.messages) {
    if (!Array.isArray(message.content)) continue;
    for (const part of message.content) {
      if (part.type !== "image") continue;
      if (message.role === "developer") throw new Error("Claude developer instructions cannot contain images.");
      validateImage(part.imageUrl);
    }
  }
  return buildStableClaudeConversationInput(parsed, {
    ...options,
    ...(parsed.context.preserveCallerContext ? { maxHistoryChars: Number.MAX_SAFE_INTEGER } : {}),
  });
}
