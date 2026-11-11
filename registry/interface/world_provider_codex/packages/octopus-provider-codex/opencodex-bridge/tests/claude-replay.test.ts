import { expect, test } from "bun:test";
import { buildClaudeConversationInput } from "../src/claude-replay.ts";
import type { OcxParsedRequest } from "../node_modules/@bitkyc08/opencodex/src/types.ts";

const image = (data: string) => ({ type: "image" as const, imageUrl: `data:image/png;base64,${data}` });
const request = (messages: unknown[]) => ({ context: { messages } }) as OcxParsedRequest;
const native = (messages: unknown[], maxHistoryChars?: number) => JSON.parse(buildClaudeConversationInput(request(messages), { maxHistoryChars })[0]!);

test("user attachments and tool screenshots retain their exact bytes and conversation order", () => {
  const result = native([
    { role: "user", timestamp: 1, content: [{ type: "text", text: "earlier" }, image("b2xk")] },
    { role: "assistant", timestamp: 2, content: [{ type: "toolCall", id: "call", name: "view_image", arguments: { path: "exact" } }] },
    { role: "toolResult", timestamp: 3, toolCallId: "call", toolName: "view_image", isError: false, content: [image("bmV3")] },
  ]);
  const blocks = result.message.content;
  expect(blocks.map((block: any) => block.type)).toEqual(["text", "image", "text", "text", "image"]);
  expect(blocks.filter((block: any) => block.type === "image").map((block: any) => block.source.data)).toEqual(["b2xk", "bmV3"]);
  expect(blocks[3].text).toContain("TOOL RESULT");
  expect(blocks[3].text).toContain("call");
});

test("whole-message truncation removes historical images with their message and keeps a pending call", () => {
  const messages = [
    { role: "user", content: [{ type: "text", text: "x".repeat(100) }, image("b2xk")] },
    { role: "assistant", content: [{ type: "toolCall", id: "call", name: "view_image", arguments: {} }] },
    { role: "toolResult", toolCallId: "call", toolName: "view_image", isError: false, content: [image("bmV3")] },
  ];
  const content = native(messages, 0).message.content;
  expect(content[0].text).toContain("omitted");
  expect(content.some((block: any) => block.source?.data === "b2xk")).toBe(false);
  expect(content.some((block: any) => block.source?.data === "bmV3")).toBe(true);
  expect(content.some((block: any) => block.text?.includes("view_image"))).toBe(true);
  const strict = request(messages);
  strict.context.preserveCallerContext = true;
  const whole = JSON.parse(buildClaudeConversationInput(strict, { maxHistoryChars: 0 })[0]!).message.content;
  expect(whole.some((block: any) => block.source?.data === "b2xk")).toBe(true);
  expect(whole.some((block: any) => block.text?.includes("omitted"))).toBe(false);
});

test("invalid image carriers fail instead of becoming a text marker", () => {
  for (const imageUrl of ["file:///private/photo.png", "data:image/png;base64,???", "https://user:password@example.com/photo.png"]) {
    expect(() => native([{ role: "user", content: [{ type: "image", imageUrl }] }])).toThrow();
  }
});
