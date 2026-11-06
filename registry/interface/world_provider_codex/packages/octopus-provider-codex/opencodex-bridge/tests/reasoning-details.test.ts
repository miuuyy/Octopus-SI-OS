import { expect, test } from "bun:test";
import { createReasoningDetailsReplay } from "../src/reasoning-details.ts";

function sse(events: unknown[]): ReadableStream<Uint8Array> {
  const text = events.map((event) => `data: ${JSON.stringify(event)}\n\n`).join("") + "data: [DONE]\n\n";
  return new Response(text).body!;
}

test("an assistant tool call gets back its streamed reasoning details unchanged, signature included", async () => {
  const replay = createReasoningDetailsReplay(8);
  await replay.observe(sse([
    { choices: [{ delta: { reasoning_details: [{ type: "reasoning.text", text: "Look ", format: "g", index: 0 }] } }] },
    { choices: [{ delta: { reasoning_details: [{ type: "reasoning.text", text: "first.", format: "g", index: 0 }] } }] },
    { choices: [{ delta: { reasoning_details: [{ type: "reasoning.encrypted", data: "SIG", format: "g", id: "call_1", index: 0 }] } }] },
    { choices: [{ delta: { tool_calls: [{ index: 0, id: "call_1", type: "function", function: { name: "ls", arguments: "{}" } }] } }] },
  ]), true);

  const body = JSON.stringify({
    messages: [
      { role: "user", content: "go" },
      { role: "assistant", content: "", tool_calls: [{ id: "call_1", type: "function", function: { name: "ls", arguments: "{}" } }], reasoning_details: [{ type: "reasoning.text", text: "lossy" }] },
      { role: "tool", tool_call_id: "call_1", content: "ok" },
    ],
  });
  const restored = JSON.parse(replay.restore(body)).messages[1].reasoning_details;
  expect(restored).toEqual([
    { type: "reasoning.text", text: "Look first.", format: "g", index: 0 },
    { type: "reasoning.encrypted", data: "SIG", format: "g", id: "call_1", index: 0 },
  ]);

  const unrelated = JSON.stringify({ messages: [{ role: "assistant", tool_calls: [{ id: "call_9" }] }] });
  expect(replay.restore(unrelated)).toBe(unrelated);
});
