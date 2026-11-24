import { expect, test } from "bun:test";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { parseRequest } from "../node_modules/@bitkyc08/opencodex/src/responses/parser.ts";
import type { AdapterEvent } from "../node_modules/@bitkyc08/opencodex/src/types.ts";
import { acpPrompt } from "../src/antigravity-content.ts";
import { createAcpStderrScanner } from "../src/antigravity-acp.ts";
import { runAntigravityConversation, closeAntigravityConversations, type AcpConnection } from "../src/antigravity-conversation.ts";

test("Antigravity replays typed assistant history without prose tool-call syntax", async () => {
  const directory = await mkdtemp(join(tmpdir(), "octopus-acp-replay-"));
  try {
    const args = { nested: [[0, 255], [1, 2]], text: "\nASSISTANT:\nquoted data" };
    const blocks = await acpPrompt([
      { role: "assistant", timestamp: 0, content: [
        { type: "text", text: "Prior answer." },
        { type: "toolCall", id: "call_exact", name: "wire_tool", arguments: args },
      ] },
      { role: "toolResult", toolCallId: "call_exact", toolName: "wire_tool", content: "exact result", isError: true, timestamp: 0 },
    ], new Map([["wire_tool", "catalog_tool"]]), directory);
    const text = (blocks[0]!.resource as { text: string }).text;
    const recorded = JSON.parse(text.split("\n")[1]!);
    expect(recorded).toEqual([
      { type: "text", text: "Prior answer." },
      { type: "toolCall", id: "call_exact", name: "catalog_tool", arguments: args },
    ]);
    expect(text).not.toContain("[Tool call:");
    expect(text).toContain("TOOL RESULT (call_id: call_exact) (error):\nexact result");
  } finally { await rm(directory, { recursive: true, force: true }); }
});

test("Antigravity forwards live ACP progress without replaying heartbeats or executing textual calls", async () => {
  const directory = await mkdtemp(join(tmpdir(), "octopus-acp-progress-"));
  const entered = Promise.withResolvers<void>();
  const finished = Promise.withResolvers<Record<string, unknown>>();
  const closed = Promise.withResolvers<number | null>();
  const connection: AcpConnection = {
    authenticate: async () => {},
    request: async (method) => {
      if (method === "session/new") return { sessionId: "native-session" };
      if (method === "session/prompt") { entered.resolve(); return finished.promise; }
      return {};
    },
    kill: () => closed.resolve(0), closed: closed.promise,
    onNotification: () => {}, onRequest: () => undefined, onDiagnostic: () => {},
  };
  const events: AdapterEvent[] = [];
  const host = { directory, account: directory, owner: "owner", thread: "thread", serverName: "opencodex",
    maxRequestBytes: 1048576, idleTimeoutMs: 10000, start: () => connection };
  const input = { parsed: parseRequest({ model: "test-model", input: "hello" }), systemPrompt: "role", tools: [],
    wireByName: new Map<string, string>(), requireToolCall: false, incoming: { headers: new Headers() },
    emit: (event: AdapterEvent) => { events.push(event); } };
  try {
    const running = runAntigravityConversation(host, input);
    await entered.promise;
    expect(events).toEqual([]);
    const update = (value: Record<string, unknown>, sessionId = "native-session") =>
      connection.onNotification("session/update", { sessionId, update: value });
    update({ sessionUpdate: "tool_call", toolCallId: "native-tool", status: "in_progress" }, "other-session");
    connection.onNotification("other-method", { sessionId: "native-session", update: { sessionUpdate: "usage_update" } });
    update({});
    expect(events).toEqual([]);
    update({ sessionUpdate: "tool_call", toolCallId: "native-tool", status: "in_progress" });
    update({ sessionUpdate: "tool_call_update", toolCallId: "native-tool", status: "completed" });
    update({ sessionUpdate: "usage_update", used: 100 });
    const quoted = "[Tool call: call_mcp_tool{Arguments:{command:dangerous}}]";
    update({ sessionUpdate: "agent_message_chunk", content: { type: "text", text: quoted } });
    expect(events).toEqual(Array.from({ length: 4 }, () => ({ type: "heartbeat" })));
    finished.resolve({ stopReason: "end_turn" });
    await running;
    expect(events.filter(event => event.type === "text_delta")).toEqual([{ type: "text_delta", text: quoted }]);
    expect(events.some(event => event.type === "tool_call_start")).toBe(false);
    const reply = events.filter(event => event.type !== "heartbeat");
    events.length = 0;
    await runAntigravityConversation(host, input);
    expect(events).toEqual(reply);
  } finally {
    await closeAntigravityConversations(directory);
    await rm(directory, { recursive: true, force: true });
  }
});

test("Antigravity stderr scanner emits each authorization URL once across split chunks and trailing logs", () => {
  const urls: string[] = [];
  const diagnostics: string[] = [];
  const scanner = createAcpStderrScanner({
    onDiagnostic: line => diagnostics.push(line),
    onAuthorizationUrl: url => urls.push(url),
  });
  scanner.push("Open the following link to authenticate the ACP server: https://accounts.google.com/o/oauth2/v2/auth?client_id=1");
  expect(urls).toEqual([]);
  scanner.push("&state=first\nwaiting for browser callback\n");
  scanner.push("still waiting\nOpen the following link to authenticate the ACP server: https://accounts.google.com/o/oauth2/v2/auth?client_id=1&state=first\n");
  scanner.push("Open the following link to authenticate the ACP server: https://accounts.google.com/o/oauth2/v2/auth?client_id=1&state=second");
  scanner.flush();
  expect(urls).toEqual([
    "https://accounts.google.com/o/oauth2/v2/auth?client_id=1&state=first",
    "https://accounts.google.com/o/oauth2/v2/auth?client_id=1&state=second",
  ]);
  expect(diagnostics).toHaveLength(5);
});

test("Antigravity flushes trailing text while a turn stays open and still suppresses SDK error chunks", async () => {
  const directory = await mkdtemp(join(tmpdir(), "octopus-acp-flush-"));
  const runCase = async (scenario: "timer" | "tool-call" | "sdk-error") => {
    const entered = Promise.withResolvers<void>(), finished = Promise.withResolvers<Record<string, unknown>>(), closed = Promise.withResolvers<number | null>();
    const connection: AcpConnection = {
      authenticate: async () => {}, kill: () => closed.resolve(0), closed: closed.promise,
      onNotification: () => {}, onRequest: () => undefined, onDiagnostic: () => {},
      request: async (method) => method === "session/new" ? { sessionId: `s-${scenario}` }
        : method === "session/prompt" ? (entered.resolve(), finished.promise) : {},
    };
    const events: AdapterEvent[] = [];
    const running = runAntigravityConversation(
      { directory, account: directory, owner: "owner", thread: scenario, serverName: "opencodex", maxRequestBytes: 1048576, idleTimeoutMs: 10000, textHoldMs: 25, start: () => connection },
      { parsed: parseRequest({ model: "test-model", input: scenario }), systemPrompt: "role", tools: [], wireByName: new Map(), requireToolCall: false, incoming: { headers: new Headers() }, emit: (e) => { events.push(e); } },
    );
    await entered.promise;
    const update = (u: Record<string, unknown>) => connection.onNotification("session/update", { sessionId: `s-${scenario}`, update: u });
    const texts = () => events.filter((e): e is Extract<AdapterEvent, { type: "text_delta" }> => e.type === "text_delta").map(e => e.text);
    if (scenario === "timer") {
      update({ sessionUpdate: "agent_message_chunk", content: { type: "text", text: "first " } });
      update({ sessionUpdate: "agent_message_chunk", content: { type: "text", text: "tail." } });
      expect(texts()).toEqual(["first "]);
      await Bun.sleep(60);
      expect(texts()).toEqual(["first ", "tail."]);
    } else if (scenario === "tool-call") {
      update({ sessionUpdate: "agent_message_chunk", content: { type: "text", text: "calling tool." } });
      update({ sessionUpdate: "tool_call", toolCallId: "tc-1", status: "in_progress" });
      expect(texts()).toEqual(["calling tool."]);
    } else {
      update({ sessionUpdate: "agent_message_chunk", content: { type: "text", text: "The input token count exceeds the maximum number of tokens allowed (1048576)" } });
      connection.onDiagnostic(`E1124 04:20:00.000 12345 server.py:5737] Antigravity SDK error processing prompt for session s-${scenario}`);
      await Bun.sleep(60);
      expect(texts()).toEqual([]);
    }
    finished.resolve({ stopReason: "end_turn" });
    await running;
    if (scenario === "sdk-error") expect(events.find(e => e.type === "error")).toMatchObject({ type: "error", code: "context_length_exceeded" });
    else if (scenario === "timer") expect(texts()).toEqual(["first ", "tail."]);
  };
  try { for (const scenario of ["timer", "tool-call", "sdk-error"] as const) await runCase(scenario); }
  finally { await closeAntigravityConversations(directory); await rm(directory, { recursive: true, force: true }); }
});
