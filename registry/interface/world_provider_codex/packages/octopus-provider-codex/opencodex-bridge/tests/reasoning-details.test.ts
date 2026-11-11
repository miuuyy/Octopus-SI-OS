import { afterEach, beforeEach, expect, spyOn, test } from "bun:test";
import { wrapReasoningDetailsAdapter } from "../src/reasoning-details.ts";
import { encodeNativeReplay } from "../src/native-replay.ts";
import { createOpenAIChatAdapter } from "../node_modules/@bitkyc08/opencodex/src/adapters/openai-chat.ts";
import { buildResponseJSON, bridgeToResponsesSSE } from "../node_modules/@bitkyc08/opencodex/src/bridge.ts";
import { parseRequest } from "../node_modules/@bitkyc08/opencodex/src/responses/parser.ts";
import { namespacedToolName } from "../node_modules/@bitkyc08/opencodex/src/types/tools.ts";
import { createTranslatorBudget, type TranslatorBudget } from "../node_modules/@bitkyc08/opencodex/src/lib/translator-budget.ts";
import type { ProviderAdapter } from "../node_modules/@bitkyc08/opencodex/src/adapters/base.ts";
import type { AdapterEvent, OcxParsedRequest, OcxProviderConfig } from "../node_modules/@bitkyc08/opencodex/src/types.ts";

const scope = { owner: "provider-owner/account-generation", model: "vendor/reasoner", format: "openai-chat-v1" };
const provider = { adapter: "openai-chat", baseUrl: "https://fixture.invalid/v1", apiKey: "fixture-key", defaultModel: scope.model } as OcxProviderConfig;
const tool = { type: "function", name: "read", description: "Read exactly", parameters: { type: "object", properties: { path: { type: "string" } }, required: ["path"] } };
const nativeDetails = [{ type: "reasoning.text", text: "Look first.\n", index: 0, format: "native", unknown: { exact: [1, null, { a: true }] } }, { type: "reasoning.encrypted", data: "SIG/==", id: "native-rs", index: 0, format: "opaque", future_field: false }];
const budgets: TranslatorBudget[] = [];
const budget = (maxTurnBytes?: number) => { const b = createTranslatorBudget({ maxTurnBytes }); budgets.push(b); return b; };
let network: ReturnType<typeof spyOn>;
beforeEach(() => { network = spyOn(globalThis, "fetch").mockImplementation(() => { throw new Error("Live calls forbidden"); }); });
afterEach(() => { expect(network).not.toHaveBeenCalled(); network.mockRestore(); for (const b of budgets.splice(0)) b.dispose(); });
const parsed = (input: unknown[] = [{ role: "user", content: "Go.\n" }], stream = false, tools: unknown[] = [tool]): OcxParsedRequest => parseRequest({ model: scope.model, instructions: "Exact system\n", tools, input, stream }, { preserveCallerContext: true });
const adapter = (b: TranslatorBudget, owner = scope.owner, underlying = createOpenAIChatAdapter(provider)) => wrapReasoningDetailsAdapter(underlying, { ...scope, owner }, b);
const build = (a: ProviderAdapter, b: TranslatorBudget, p = parsed()) => a.buildRequest(p, { headers: new Headers(), translatorBudget: b });
const call = (id = "call_same", name = "read", path = "same") => ({ id, type: "function", function: { name, arguments: JSON.stringify({ path }) } });
const jsonResponse = (reasoning_details: unknown = nativeDetails, tools: unknown[] = [call()], content = "Read this.\n") => Response.json({ choices: [{ index: 0, message: { role: "assistant", content, tool_calls: tools, reasoning_details }, finish_reason: tools.length ? "tool_calls" : "stop" }] });
const continued = (response: Record<string, any>, tools: unknown[] = [tool]) => parsed([
  { role: "user", content: "Go.\n" }, ...JSON.parse(JSON.stringify(response.output)),
  ...response.output.filter((item: any) => item.type === "function_call").toReversed().map((item: any) => ({ type: "function_call_output", call_id: item.call_id, output: `result:${item.call_id}` })),
  { role: "user", content: "Continue." },
], false, tools);
function stream(events: unknown[], chunkSize = 37, done = true): Response {
  const bytes = new TextEncoder().encode(events.map(event => typeof event === "string" ? event : `data:${JSON.stringify(event)}\r\n\r\n`).join("") + (done ? "data: [DONE]\n\n" : ""));
  let offset = 0;
  return new Response(new ReadableStream({ pull(controller) {
    if (offset === bytes.length) controller.close();
    else { controller.enqueue(bytes.slice(offset, offset += chunkSize)); offset = Math.min(offset, bytes.length); }
  } }), { headers: { "content-type": "text/event-stream" } });
}
const frame = (delta: unknown, finish_reason: string | null = null) => ({ choices: [{ index: 0, delta, finish_reason }] });
async function roundTrip(details: unknown = nativeDetails, owner = scope.owner) {
  const b = budget(), a = adapter(b, owner);
  await build(a, b);
  const events = await a.parseResponse!(jsonResponse(details), b);
  const response = buildResponseJSON(events, scope.model, { translatorBudget: b });
  return { response, events, parsed: continued(response) };
}

test("JSON Responses replay survives a new adapter and preserves two native calls, aliases and all opaque fields", async () => {
  const b = budget(), a = adapter(b), namespace = "n".repeat(62);
  const tools = [{ type: "namespace", name: namespace, tools: [tool] }];
  const request = await build(a, b, parsed(undefined, false, tools));
  const wireName = JSON.parse(request.body).tools[0].function.name;
  expect(wireName).not.toBe(namespacedToolName(namespace, "read"));
  const events = await a.parseResponse!(jsonResponse(nativeDetails, [call("call_A", wireName, "a"), call("call_B", wireName, "b")]), b);
  expect(events.at(-2)).toMatchObject({ type: "thinking_signature", afterOutput: true });
  expect(events.at(-1)?.type).toBe("done");
  expect(b.snapshot().currentBytes).toBe(Buffer.byteLength(JSON.stringify(events)));
  const response = buildResponseJSON(events, scope.model, { translatorBudget: b, toolNsMap: new Map([[namespacedToolName(namespace, "read"), { namespace, name: "read" }]]) });
  expect(response.status).toBe("completed");
  expect((response.output as any[]).at(-1).type).toBe("reasoning");
  expect(typeof (response.output as any[]).at(-1).encrypted_content).toBe("string");
  const history = continued(response, tools), before = JSON.stringify(history), nextBudget = budget();
  const wire = JSON.parse((await build(adapter(nextBudget), nextBudget, history)).body);
  expect(JSON.stringify(history)).toBe(before);
  expect(wire.messages.filter((message: any) => message.role === "assistant")).toEqual([{ role: "assistant", content: "Read this.\n", tool_calls: [call("call_A", wireName, "a"), call("call_B", wireName, "b")], reasoning_details: nativeDetails }]);
  expect(wire.messages.filter((message: any) => message.role === "tool").map((message: any) => message.tool_call_id)).toEqual(["call_B", "call_A"]);
  expect(JSON.stringify(wire)).not.toContain("octopus-native-v1:");
  expect(b.snapshot().currentBytes).toBeLessThanOrEqual(Buffer.byteLength(JSON.stringify(response)));
  expect(nextBudget.snapshot().currentBytes).toBe(0);
});

test("single-stream fragmented SSE round trips through the Responses SSE bridge", async () => {
  const b = budget(), a = adapter(b);
  await build(a, b, parsed(undefined, true));
  const source = stream([
    frame({ role: "assistant", reasoning_details: [{ ...nativeDetails[0], text: "Look " }] }),
    frame({ reasoning_details: [{ type: "reasoning.text", text: "first.\n", index: 0 }] }),
    frame({ reasoning_details: [{ ...nativeDetails[1], data: "SIG" }] }),
    frame({ reasoning_details: [{ type: "reasoning.encrypted", data: "/==", index: 0 }] }),
    frame({ content: "Read π 🐙.\n" }),
    frame({ tool_calls: [{ index: 0, ...call(), function: { name: "read", arguments: '{"path":' } }] }),
    frame({ tool_calls: [{ index: 0, function: { arguments: '"same"}' } }] }, "tool_calls"),
  ], 3);
  const tee = spyOn(source.body!, "tee").mockImplementation(() => { throw new Error("No tee"); });
  const output = await bridgeToResponsesSSE(a.parseStream(source, b), scope.model, undefined, undefined, undefined, undefined, 2000, { translatorBudget: b }).text();
  const completed = output.split("\n").filter(line => line.startsWith("data: ") && line !== "data: [DONE]").map(line => JSON.parse(line.slice(6))).find(item => item.type === "response.completed").response;
  const replayBudget = budget(), wire = JSON.parse((await build(adapter(replayBudget), replayBudget, continued(completed))).body);
  expect(wire.messages.find((message: any) => message.role === "assistant")).toMatchObject({ content: "Read π 🐙.\n", reasoning_details: nativeDetails, tool_calls: [call()] });
  expect(tee).not.toHaveBeenCalled(); tee.mockRestore();
  expect(b.snapshot().currentBytes).toBeLessThanOrEqual(completed.output.reduce((bytes: number, item: unknown) => bytes + Buffer.byteLength(JSON.stringify(item)), 0));
});

test("text-only signatures retain their assistant sequence and unsigned history remains intact", async () => {
  const b = budget(), a = adapter(b);
  await build(a, b);
  const response = buildResponseJSON(await a.parseResponse!(jsonResponse(nativeDetails, [], "Same text"), b), scope.model, { translatorBudget: b });
  const p = parsed([{ role: "assistant", content: "Same text" }, { role: "user", content: "Next" }, ...response.output as unknown[], { role: "user", content: "Next again" }]);
  const next = budget(), wire = JSON.parse((await build(adapter(next), next, p)).body);
  const messages = wire.messages.filter((message: any) => message.role === "assistant");
  expect(messages).toEqual([{ role: "assistant", content: "Same text" }, { role: "assistant", content: "Same text", reasoning_details: nativeDetails }]);
  const plainBudget = budget(), plain = adapter(plainBudget);
  await build(plain, plainBudget);
  const output = await plain.parseResponse!(Response.json({ choices: [{ message: { role: "assistant", content: "plain" }, finish_reason: "stop" }] }), plainBudget);
  expect(output.some(event => event.type === "thinking_signature")).toBe(false);
  buildResponseJSON(output, scope.model, { translatorBudget: plainBudget });
});

test("parallel requests with identical call ids keep distinct carriers without a shared cache", async () => {
  const turns = await Promise.all(["one", "two"].map(secret => roundTrip([{ type: "reasoning.encrypted", data: secret }])));
  const restored = await Promise.all(turns.map(async turn => { const b = budget(); return JSON.parse((await build(adapter(b), b, turn.parsed)).body).messages.find((message: any) => message.role === "assistant").reasoning_details; }));
  expect(restored).toEqual([[{ type: "reasoning.encrypted", data: "one" }], [{ type: "reasoning.encrypted", data: "two" }]]);
  const b = budget(), wire = JSON.parse((await build(adapter(b), b, parsed([{ role: "user", content: "unrelated" }, { type: "function_call", call_id: "call_same", name: "read", arguments: '{"path":"same"}' }, { type: "function_call_output", call_id: "call_same", output: "done" }]))).body);
  expect(wire.messages.find((message: any) => message.role === "assistant").reasoning_details).toBeUndefined();
});

test("foreign or changed history is sent without native details; malformed carriers fail before dispatch", async () => {
  const turn = await roundTrip();
  const restored = async (a: ProviderAdapter, b: TranslatorBudget, p: OcxParsedRequest) => JSON.parse((await build(a, b, p)).body).messages
    .filter((message: any) => message.role === "assistant").some((message: any) => message.reasoning_details !== undefined);
  for (const field of ["owner", "model", "format"] as const) {
    const b = budget(), p = structuredClone(turn.parsed), other = { ...scope, [field]: "different" };
    if (field === "model") p.modelId = other.model;
    expect(await restored(wrapReasoningDetailsAdapter(createOpenAIChatAdapter(provider), other, b), b, p)).toBe(false);
  }
  for (const edit of ["text", "id", "arguments", "name", "encoding", "payload", "extra-assistant"]) {
    const p = structuredClone(turn.parsed), message = p.context.messages.find(message => message.role === "assistant")!;
    if (message.role !== "assistant") throw new Error("Missing fixture assistant");
    const b = budget(), carrier = message.content.find(part => part.type === "thinking" && part.signature?.startsWith("octopus-native-v1:"))!;
    const nativeCall = message.content.find(part => part.type === "toolCall")!;
    if (edit === "encoding" && carrier.type === "thinking") carrier.signature = "octopus-native-v1:bad";
    else if (edit === "payload" && carrier.type === "thinking") carrier.signature = encodeNativeReplay(scope, { malformed: true }, b);
    else if (edit === "text") message.content.push({ type: "text", text: "altered" });
    else if (edit === "extra-assistant") p.context.messages.splice(p.context.messages.indexOf(message), 0, { role: "assistant", model: scope.model, timestamp: 1, content: [{ type: "text", text: "unowned" }] });
    else if (nativeCall.type === "toolCall") { if (edit === "id") nativeCall.id = "another"; else if (edit === "name") nativeCall.name = "another"; else nativeCall.arguments = { path: "changed" }; }
    if (edit === "encoding" || edit === "payload") await expect(build(adapter(b), b, p)).rejects.toThrow();
    else expect(await restored(adapter(b), b, p)).toBe(false);
    expect(b.snapshot().currentBytes).toBe(0);
  }
  const b = budget();
  expect(await restored(adapter(b), b, turn.parsed)).toBe(true);
});

test("malformed details and late budget overflow release no executable tool calls", async () => {
  for (const bad of ["not-json", { data: "not-an-array" }, [{ type: "reasoning.encrypted", data: 7 }], [{ type: "reasoning.encrypted", data: "X".repeat(40_000), index: 0 }]]) {
    const b = budget(32_768), a = adapter(b);
    await build(a, b, parsed(undefined, true));
    const source = stream([frame({ content: "visible" }), frame({ tool_calls: [{ index: 0, ...call() }] }, "tool_calls"), bad === "not-json" ? "data: {broken\n\n" : frame({ reasoning_details: bad })]);
    const seen: AdapterEvent[] = [];
    let failure: unknown;
    try { for await (const event of a.parseStream(source, b)) seen.push(event); } catch (error) { failure = error; }
    expect(failure !== undefined || seen.some(event => event.type === "error")).toBe(true);
    expect(seen.some(event => event.type === "text_delta")).toBe(true);
    expect(seen.some(event => event.type === "tool_call_start" || event.type === "tool_call_end" || event.type === "done")).toBe(false);
    expect(b.snapshot().currentBytes).toBe(0);
  }
});

test("conflicting opaque fragments and oversized input carriers fail closed", async () => {
  const b = budget(), a = adapter(b);
  await build(a, b, parsed(undefined, true));
  await expect(a.parseResponse!(stream([frame({ content: "text", reasoning_details: [{ type: "reasoning.encrypted", index: 0, data: "a", opaque: { n: 1 } }] }), frame({ reasoning_details: [{ type: "reasoning.encrypted", index: 0, data: "b", opaque: { n: 2 } }] }, "stop")]), b)).rejects.toThrow("Conflicting");
  expect(b.snapshot().currentBytes).toBe(0);
  const turn = await roundTrip([{ type: "reasoning.encrypted", data: "X".repeat(1000) }]), small = budget(3000);
  await expect(build(adapter(small), small, turn.parsed)).rejects.toMatchObject({ code: "translation_buffer_limit" });
  expect(small.snapshot().currentBytes).toBe(0);
});

test("carrier encoding overflow is refused before buffered tools become executable", async () => {
  const b = budget(100_000), a = adapter(b), seen: AdapterEvent[] = [];
  await build(a, b);
  let failure: unknown;
  try { for await (const event of a.parseStream(jsonResponse([{ type: "reasoning.encrypted", data: "X".repeat(8000) }]), b)) seen.push(event); }
  catch (error) { failure = error; }
  expect(failure).toMatchObject({ code: "translation_buffer_limit", kind: "reasoning" });
  expect(seen.some(event => event.type === "text_delta")).toBe(true);
  expect(seen.some(event => event.type === "tool_call_start" || event.type === "tool_call_end" || event.type === "done")).toBe(false);
  expect(b.snapshot().currentBytes).toBe(0);
});

test("wrapper preserves other adapter methods and rejects reuse of a request", async () => {
  const b = budget(), base = createOpenAIChatAdapter(provider);
  Object.assign(base, { reportsPhysicalSends: true, formatErrorBody() { return this.name; }, localTerminal() { return { reason: this.name }; } });
  const wrapped = adapter(b, scope.owner, base);
  expect(wrapped.reportsPhysicalSends).toBe(true);
  expect(wrapped.formatErrorBody!(400, new Headers(), "")).toBe(base.name);
  expect(wrapped.localTerminal!(parsed())).toEqual({ reason: base.name });
  await build(wrapped, b);
  await expect(build(wrapped, b)).rejects.toThrow("scope and budget");
  const output = await wrapped.parseResponse!(jsonResponse(), b);
  buildResponseJSON(output, scope.model, { translatorBudget: b });
  await expect(wrapped.parseResponse!(jsonResponse(), b)).rejects.toThrow("one built request");
});

test("deadline abort refuses completion and tools without reasoning_details", async () => {
  for (const finished of [false, true]) {
    const b = budget(), a = adapter(b), abort = new AbortController();
    await a.buildRequest(parsed(undefined, true), { headers: new Headers(), translatorBudget: b, abortSignal: abort.signal });
    let interval: ReturnType<typeof setInterval> | undefined, cancelled = false;
    const source = new Response(new ReadableStream({
      start(controller) {
        const send = (delta: unknown, finish: string | null = null) => controller.enqueue(new TextEncoder().encode(`data: ${JSON.stringify(frame(delta, finish))}\n\n`));
        send({ content: "working" });
        if (finished) send({ tool_calls: [{ index: 0, ...call() }] }, "tool_calls");
        interval = setInterval(() => send({ content: "working" }), 2);
      },
      cancel() { cancelled = true; clearInterval(interval); },
    }), { headers: { "content-type": "text/event-stream" } });
    const deadline = setTimeout(() => abort.abort(new DOMException("Fixture request deadline", "TimeoutError")), 25);
    try {
      const output = await bridgeToResponsesSSE(a.parseStream(source, b), scope.model, undefined, undefined, undefined, undefined, 2000, { translatorBudget: b }).text();
      expect(output).toContain("working");
      expect(output).toContain("response.failed");
      expect(output).not.toContain("response.completed");
      expect(output).not.toContain("response.function_call_arguments.done");
      expect(cancelled).toBe(true);
    } finally { clearTimeout(deadline); clearInterval(interval); }
  }
});

test("native terminal proof is required at SSE and JSON EOF without reasoning_details", async () => {
  for (const source of [stream([frame({ content: "partial" })], 37, false), Response.json({ choices: [{ message: { role: "assistant", content: "partial", tool_calls: [call()] } }] })]) {
    const b = budget(), a = adapter(b), seen: AdapterEvent[] = [];
    await build(a, b);
    let failure: unknown;
    try { for await (const event of a.parseStream(source, b)) seen.push(event); } catch (error) { failure = error; }
    expect((failure as Error)?.message).toContain("without a terminal signal");
    expect(seen.some(event => event.type === "text_delta")).toBe(true);
    expect(seen.some(event => event.type === "tool_call_start" || event.type === "tool_call_end" || event.type === "done")).toBe(false);
    expect(b.snapshot().currentBytes).toBe(0);
  }
  const b = budget(), a = adapter(b);
  await build(a, b);
  const events = await a.parseResponse!(stream([frame({ content: "complete" }, "stop")], 37, false), b);
  expect(events.at(-1)?.type).toBe("done");
  buildResponseJSON(events, scope.model, { translatorBudget: b });
});
