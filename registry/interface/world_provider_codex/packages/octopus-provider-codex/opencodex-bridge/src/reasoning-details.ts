import { isDeepStrictEqual } from "node:util";
import type { AdapterRequest, ProviderAdapter } from "../node_modules/@bitkyc08/opencodex/src/adapters/base.ts";
import type { AdapterEvent, OcxAssistantContentPart, OcxMessage, OcxParsedRequest } from "../node_modules/@bitkyc08/opencodex/src/types.ts";
import { namespacedToolName } from "../node_modules/@bitkyc08/opencodex/src/types/tools.ts";
import { jsonUtf8Bytes } from "../node_modules/@bitkyc08/opencodex/src/lib/json-byte-size.ts";
import { decodeServerSentEvents } from "../node_modules/@bitkyc08/opencodex/src/lib/sse-decoder.ts";
import { releaseTranslatedEvent, retainTranslatedEvent, type TranslatorBudget } from "../node_modules/@bitkyc08/opencodex/src/lib/translator-budget.ts";
import { encodeNativeReplay, takeNativeReplay, type NativeReplayScope } from "./native-replay.ts";

type ObjectValue = Record<string, unknown>;
type Call = { id: string; name: string; arguments: ObjectValue };
type Part = { text: string } | Call;
type Wire = { text: string; calls: Call[] };
type Replay = { version: 1; reasoning_details: ObjectValue[]; projection: Part[]; wire: Wire };
const CONCATENATED = new Set(["text", "summary", "data"]);

function object(value: unknown): ObjectValue {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("Reasoning replay requires a JSON object");
  return value as ObjectValue;
}

function string(value: unknown): string {
  if (typeof value !== "string" || !value.length) throw new Error("Reasoning replay has an invalid identity");
  return value;
}

function details(value: unknown): ObjectValue[] {
  if (!Array.isArray(value)) throw new Error("Native reasoning_details must be an array");
  for (const raw of value) {
    const item = object(raw);
    string(item.type);
    if (item.index !== undefined && (!Number.isSafeInteger(item.index) || (item.index as number) < 0)) throw new Error("Invalid reasoning detail index");
    for (const field of CONCATENATED) {
      if (item[field] !== undefined && typeof item[field] !== "string") throw new Error("Invalid reasoning detail fragment");
    }
    if (item.id !== undefined && item.id !== null) string(item.id);
  }
  return value;
}

function appendText(parts: Part[], text: string): void {
  if (!text) return;
  const last = parts.at(-1);
  if (last && "text" in last) last.text += text;
  else parts.push({ text });
}

function projection(parts: OcxAssistantContentPart[]): Part[] {
  const result: Part[] = [];
  for (const part of parts) {
    if (part.type === "text") appendText(result, part.text);
    else if (part.type === "toolCall") result.push({ id: string(part.id), name: namespacedToolName(part.namespace, part.name), arguments: object(part.arguments) });
    else if (part.type !== "thinking") throw new Error("Unsupported assistant part in reasoning replay");
  }
  return result;
}

function projectedWire(parts: Part[]): Wire {
  return { text: parts.filter(part => "text" in part).map(part => (part as { text: string }).text).join(""), calls: parts.filter((part): part is Call => !("text" in part)) };
}

function wireMessage(message: ObjectValue): Wire | undefined {
  const content = message.content ?? "";
  const parts = typeof content === "string" ? [{ type: "text", text: content }] : content;
  const calls = message.tool_calls ?? [];
  if (!Array.isArray(parts) || !Array.isArray(calls)) return undefined;
  try {
    return {
      text: parts.map(part => {
        if (part?.type !== "text" || typeof part.text !== "string") throw new Error("Unsupported assistant wire content");
        return part.text as string;
      }).join(""),
      calls: calls.map(call => ({ id: string(call?.id), name: string(call?.function?.name), arguments: object(JSON.parse(string(call?.function?.arguments))) })),
    };
  } catch { return undefined; }
}

function replayValue(value: unknown): Replay {
  const replay = object(value);
  if (Object.keys(replay).sort().join(",") !== "projection,reasoning_details,version,wire" || replay.version !== 1 || !Array.isArray(replay.projection)) throw new Error("Malformed reasoning replay carrier");
  details(replay.reasoning_details);
  for (const raw of replay.projection) {
    const part = object(raw);
    if (Object.keys(part).join(",") === "text" && typeof part.text === "string" && part.text) continue;
    if (Object.keys(part).sort().join(",") !== "arguments,id,name") throw new Error("Invalid reasoning replay projection");
    string(part.id); string(part.name); object(part.arguments);
  }
  const wire = object(replay.wire);
  if (Object.keys(wire).sort().join(",") !== "calls,text" || typeof wire.text !== "string" || !Array.isArray(wire.calls)) throw new Error("Invalid reasoning replay wire identity");
  for (const raw of wire.calls) {
    const call = object(raw);
    if (Object.keys(call).sort().join(",") !== "arguments,id,name") throw new Error("Invalid native call link");
    string(call.id); string(call.name); object(call.arguments);
  }
  const result = replay as Replay;
  const outer = projectedWire(result.projection);
  if (!result.projection.length || new Set(result.wire.calls.map(call => call.id)).size !== result.wire.calls.length
    || outer.text !== result.wire.text || !isDeepStrictEqual(outer.calls.map(({ id, arguments: args }) => ({ id, arguments: args })), result.wire.calls.map(({ id, arguments: args }) => ({ id, arguments: args })))) {
    throw new Error("Reasoning replay changed native text, call identities, order or arguments");
  }
  return result;
}

function restoreInput(parsed: OcxParsedRequest, scope: NativeReplayScope, budget: TranslatorBudget) {
  const messages: OcxMessage[] = [];
  const replays = new Map<OcxMessage, Replay>();
  let pending: Extract<OcxMessage, { role: "assistant" }>[] = [];
  const flush = () => { messages.push(...pending); pending = []; };
  for (const original of parsed.context.messages) {
    if (original.role !== "assistant") { flush(); messages.push(original); continue; }
    const message = { ...original, content: [...original.content] };
    const raw = takeNativeReplay(message, scope, budget);
    pending.push(message);
    if (raw === undefined) continue;
    const replay = replayValue(raw);
    const content = pending.flatMap(part => part.content);
    if (!isDeepStrictEqual(projection(content), replay.projection)) continue;
    const combined = { ...pending[0]!, content };
    messages.push(combined);
    replays.set(combined, replay);
    pending = [];
  }
  flush();
  return { parsed: { ...parsed, context: { ...parsed.context, messages } }, replays };
}

function restoreWire(request: AdapterRequest, parsed: OcxParsedRequest, replays: Map<OcxMessage, Replay>, budget: TranslatorBudget): AdapterRequest {
  if (!replays.size) return request;
  const reservation = budget.reserveTransient(8 * Buffer.byteLength(request.body), { kind: "request_copies" });
  try {
    const body = object(JSON.parse(request.body));
    const actual: ObjectValue[] = Array.isArray(body.messages) ? body.messages.filter(message => message?.role === "assistant") : [];
    const expected = parsed.context.messages.filter(message => message.role === "assistant");
    if (actual.length !== expected.length) return request;
    let restored = false;
    for (const [index, message] of expected.entries()) {
      const replay = replays.get(message);
      if (!replay || !isDeepStrictEqual(wireMessage(actual[index]!), replay.wire)) continue;
      actual[index]!.reasoning_details = replay.reasoning_details;
      restored = true;
    }
    if (!restored) return request;
    const serialization = budget.reserveTransient(4 * jsonUtf8Bytes(body), { kind: "request_copies" });
    try { return { ...request, body: JSON.stringify(body) }; } finally { serialization.release(); }
  } finally {
    reservation.release();
  }
}

class Observation {
  private details: ObjectValue[] = [];
  private indexed = new Map<string, ObjectValue>();
  private calls = new Map<string, { id?: string; name?: string; arguments: string }>();
  private text = "";
  private parts: Part[] = [];
  private call: { id: string; name: string; arguments: string } | undefined;
  private invalid: unknown;
  private bytes = 0;
  private hasDetails = false;
  private terminal = false;
  private lastOutput = "";

  constructor(private budget: TranslatorBudget, private streamed: boolean) {}

  private retain(characters: number): void {
    const bytes = 2 * characters;
    this.budget.chargeRetained(bytes, { kind: "reasoning" });
    this.bytes += bytes;
  }

  private fold(action: () => void): void {
    try { action(); } catch (error) { this.invalid ??= error; }
  }

  native(raw: unknown): void {
    const root = object(raw);
    const payload = root.choices !== undefined || root.error !== undefined ? root : root.data === undefined ? root : object(root.data);
    if ((payload.error !== undefined && payload.error !== null) || payload.choices === undefined) return;
    if (!Array.isArray(payload.choices) || payload.choices.length > 1) throw new Error("Reasoning replay requires one native choice");
    if (!payload.choices.length) return;
    const choice = object(payload.choices[0]);
    if (choice.index !== undefined && choice.index !== 0) throw new Error("Reasoning replay received another native choice");
    if (choice.finish_reason !== undefined && choice.finish_reason !== null) {
      if (typeof choice.finish_reason !== "string" || !choice.finish_reason.trim()) throw new Error("Invalid native Chat finish reason");
      this.terminal = true;
    }
    const rawMessage = this.streamed ? choice.delta : choice.message;
    if (rawMessage === undefined) return;
    const message = object(rawMessage);
    if (Object.hasOwn(message, "reasoning_details")) { details(message.reasoning_details); this.hasDetails = true; }
    this.fold(() => this.message(message));
  }

  private message(chunk: ObjectValue): void {
    if (chunk.role !== undefined && chunk.role !== "assistant") throw new Error("Reasoning details belong to a non-assistant response");
    if (chunk.content !== undefined && chunk.content !== null) {
      if (typeof chunk.content !== "string") throw new Error("Reasoning replay requires native text content");
      this.retain(chunk.content.length);
      this.text += chunk.content;
    }
    if (Object.hasOwn(chunk, "reasoning_details")) {
      for (const detail of chunk.reasoning_details as ObjectValue[]) {
        this.retain(JSON.stringify(detail).length);
        const key = !this.streamed ? undefined : detail.index !== undefined ? JSON.stringify([detail.type, "index", detail.index]) : typeof detail.id === "string" ? JSON.stringify([detail.type, "id", detail.id]) : undefined;
        const existing = key === undefined ? undefined : this.indexed.get(key);
        if (!existing) {
          const copy = { ...detail };
          this.details.push(copy);
          if (key !== undefined) this.indexed.set(key, copy);
          continue;
        }
        for (const [field, value] of Object.entries(detail)) {
          if (!Object.hasOwn(existing, field)) Object.defineProperty(existing, field, { value, enumerable: true, writable: true, configurable: true });
          else if (CONCATENATED.has(field)) existing[field] = (existing[field] as string) + (value as string);
          else if (!isDeepStrictEqual(existing[field], value)) throw new Error("Conflicting native reasoning detail metadata");
        }
      }
    }
    if (chunk.tool_calls === undefined || chunk.tool_calls === null) return;
    if (!Array.isArray(chunk.tool_calls)) throw new Error("Invalid native tool calls in reasoning replay");
    for (const [position, raw] of chunk.tool_calls.entries()) {
      const call = object(raw), fn = call.function === undefined ? {} : object(call.function);
      const index = this.streamed ? call.index : position;
      if (!Number.isSafeInteger(index) || (index as number) < 0 || (call.type !== undefined && call.type !== "function")) throw new Error("Native reasoning tool call lacks an exact index");
      const key = String(index), saved = this.calls.get(key) ?? { arguments: "" };
      for (const [field, value] of [["id", call.id], ["name", fn.name]] as const) {
        if (value === undefined) continue;
        if (saved[field] !== undefined && saved[field] !== value) throw new Error("Native tool call identity changed while streaming");
        saved[field] = string(value);
      }
      if (fn.arguments !== undefined) {
        if (typeof fn.arguments !== "string") throw new Error("Native tool arguments must be JSON text");
        this.retain(fn.arguments.length);
        saved.arguments += fn.arguments;
      }
      this.calls.set(key, saved);
    }
  }

  done(): void { this.terminal = true; }

  assertCompleted(signal?: AbortSignal): void {
    signal?.throwIfAborted();
    if (!this.terminal) throw new Error("Native Chat response ended without a terminal signal");
  }

  event(event: AdapterEvent): void {
    if (event.type === "thinking_delta" || event.type === "reasoning_raw_delta") this.lastOutput = event.type;
    if (!["text_delta", "tool_call_start", "tool_call_delta", "tool_call_end"].includes(event.type)) return;
    this.lastOutput = event.type;
    this.fold(() => {
      if (event.type === "text_delta") {
        if (this.call) throw new Error("Reasoning replay output interleaves text inside a tool call");
        this.retain(event.text.length);
        appendText(this.parts, event.text);
      } else if (event.type === "tool_call_start") {
        if (this.call) throw new Error("Reasoning replay output overlaps tool calls");
        this.call = { id: string(event.id), name: string(event.name), arguments: "" };
      } else if (event.type === "tool_call_delta") {
        if (!this.call) throw new Error("Reasoning replay output has an orphan tool delta");
        this.retain(event.arguments.length);
        this.call.arguments += event.arguments;
      } else if (event.type === "tool_call_end") {
        if (!this.call || event.incomplete) throw new Error("Reasoning replay output has an incomplete tool call");
        this.parts.push({ ...this.call, arguments: object(JSON.parse(this.call.arguments)) });
        this.call = undefined;
      }
    });
  }

  signature(scope: NativeReplayScope): string | undefined {
    if (!this.hasDetails) return undefined;
    if (this.invalid) throw this.invalid;
    if (this.call || !["text_delta", "tool_call_end"].includes(this.lastOutput)) throw new Error("Reasoning details lack a completed assistant output");
    const reservation = this.budget.reserveTransient(this.bytes, { kind: "reasoning" });
    try {
      const replay = replayValue({ version: 1, reasoning_details: this.details, projection: this.parts, wire: {
        text: this.text, calls: [...this.calls.values()].map(call => ({ id: string(call.id), name: string(call.name), arguments: object(JSON.parse(call.arguments)) })),
      } });
      return encodeNativeReplay(scope, replay, this.budget);
    } finally {
      reservation.release();
    }
  }

  release(): void {
    this.details = []; this.parts = []; this.text = "";
    this.indexed.clear(); this.calls.clear();
    this.budget.releaseRetained(this.bytes, { kind: "reasoning" });
    this.bytes = 0;
  }
}

async function* observedBody(response: Response, streamed: boolean, observation: Observation, budget: TranslatorBudget, signal?: AbortSignal): AsyncGenerator<Uint8Array> {
  signal?.throwIfAborted();
  if (!response.body) throw new Error("Native Chat response has no body");
  const encoder = new TextEncoder();
  if (streamed) {
    for await (const frame of decodeServerSentEvents(response.body, { translatorBudget: budget, signal })) {
      signal?.throwIfAborted();
      if (frame.data.trim() === "[DONE]") { observation.done(); yield encoder.encode("data: [DONE]\n\n"); return; }
      const reservation = budget.reserveTransient(8 * Buffer.byteLength(frame.data) + 64, { kind: "live_transient" });
      try {
        const payload: unknown = JSON.parse(frame.data);
        observation.native(payload);
        yield encoder.encode(`data: ${JSON.stringify(payload)}\n\n`);
      } finally { reservation.release(); }
    }
    signal?.throwIfAborted();
    return;
  }
  const reader = response.body.getReader(), chunks: Uint8Array[] = [];
  let bytes = 0;
  try {
    for (;;) {
      signal?.throwIfAborted();
      const { value, done } = await reader.read();
      signal?.throwIfAborted();
      if (done) break;
      budget.chargeRetained(value.byteLength, { kind: "live_transient" });
      bytes += value.byteLength;
      chunks.push(value);
    }
    const reservation = budget.reserveTransient(8 * bytes, { kind: "live_transient" });
    try {
      const body = Buffer.concat(chunks);
      observation.native(JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(body)));
      yield body;
    } finally { reservation.release(); }
  } finally {
    budget.releaseRetained(bytes, { kind: "live_transient" });
    try { await reader.cancel(); } finally { reader.releaseLock(); }
  }
}

export function wrapReasoningDetailsAdapter(adapter: ProviderAdapter, scope: NativeReplayScope, budget: TranslatorBudget): ProviderAdapter {
  if ([scope.owner, scope.model, scope.format].some(value => typeof value !== "string" || !value.trim())) throw new Error("Reasoning replay requires an explicit owner, model and format");
  scope = { ...scope };
  let started = false, built = false, consumed = false, signal: AbortSignal | undefined;
  const begin = (parserBudget: TranslatorBudget) => {
    if (!built || consumed || parserBudget !== budget) throw new Error("Reasoning replay adapter belongs to one built request and budget");
    consumed = true;
  };
  const buildRequest: ProviderAdapter["buildRequest"] = async (parsed, incoming) => {
    if (started || incoming.translatorBudget !== budget || parsed.modelId !== scope.model) throw new Error("Reasoning replay request does not match its scope and budget");
    started = true;
    signal = incoming.abortSignal;
    const reservation = budget.reserveTransient(8 * jsonUtf8Bytes(parsed.context.messages), { kind: "request_copies" });
    try {
      const restored = restoreInput(parsed, scope, budget);
      const request = await adapter.buildRequest(restored.parsed, incoming);
      const result = restoreWire(request, restored.parsed, restored.replays, budget);
      built = true;
      return result;
    } finally { reservation.release(); }
  };
  const parse: ProviderAdapter["parseStream"] = async function* (response, parserBudget, tier, context) {
    begin(parserBudget);
    const streamed = response.headers.get("content-type")?.toLowerCase().includes("text/event-stream") === true;
    const observation = new Observation(budget, streamed);
    const source = observedBody(response, streamed, observation, budget, signal);
    const body = new ReadableStream<Uint8Array>({
      async pull(controller) {
        try { const next = await source.next(); if (next.done) controller.close(); else controller.enqueue(next.value); }
        catch (error) { controller.error(error); }
      },
      async cancel() { await source.return(); },
    }, { highWaterMark: 0 });
    const observed = new Response(body, { status: response.status, statusText: response.statusText, headers: response.headers });
    let events: AdapterEvent[] | undefined;
    let releasedEvents = 0, heldBytes = 0;
    const held: AdapterEvent[] = [];
    try {
      if (!streamed && !adapter.parseResponse) throw new Error("Chat adapter has no JSON response parser");
      if (!streamed) events = await adapter.parseResponse!(observed, budget, tier);
      for await (const event of events ?? adapter.parseStream(observed, budget, tier, context)) {
        signal?.throwIfAborted();
        observation.event(event);
        if (event.type === "tool_call_start" || (held.length && !["done", "error", "incomplete", "heartbeat"].includes(event.type))) {
          const bytes = 4 * jsonUtf8Bytes(event) + 64;
          budget.chargeRetained(bytes, { kind: "retained_collectors" });
          heldBytes += bytes;
          held.push(event);
          if (events) { releaseTranslatedEvent(event, budget); releasedEvents++; }
          continue;
        }
        if (event.type === "done") {
          observation.assertCompleted(signal);
          const signature = observation.signature(scope);
          const reservation = budget.reserveTransient(signature === undefined ? 0 : 2 * signature.length, { kind: "reasoning" });
          try {
            await source.return();
            observation.release();
            for (const pending of held) {
              observation.assertCompleted(signal);
              yield pending;
            }
            if (signature !== undefined) {
              signal?.throwIfAborted();
              yield { type: "assistant_boundary" };
              signal?.throwIfAborted();
              yield { type: "thinking_signature", signature, afterOutput: true };
            }
          } finally { reservation.release(); }
        }
        if (events) { releaseTranslatedEvent(event, budget); releasedEvents++; }
        if (["done", "error", "incomplete"].includes(event.type)) {
          observation.release();
          held.length = 0;
          budget.releaseRetained(heldBytes, { kind: "retained_collectors" });
          heldBytes = 0;
          await source.return();
        }
        if (event.type === "done") observation.assertCompleted(signal);
        yield event;
        if (["done", "error", "incomplete"].includes(event.type)) return;
      }
      throw new Error("Chat adapter ended without a terminal event");
    } finally {
      if (events) for (const event of events.slice(releasedEvents)) releaseTranslatedEvent(event, budget);
      budget.releaseRetained(heldBytes, { kind: "retained_collectors" });
      observation.release();
      await source.return();
    }
  };
  const parseResponse: NonNullable<ProviderAdapter["parseResponse"]> = async (response, parserBudget, tier) => {
    const events: AdapterEvent[] = [];
    try {
      for await (const event of parse(response, parserBudget, tier)) {
        retainTranslatedEvent(event, budget, events.at(-1));
        events.push(event);
      }
      return events;
    } catch (error) {
      for (const event of events) releaseTranslatedEvent(event, budget);
      throw error;
    }
  };
  return new Proxy(adapter, {
    get(target, key) {
      if (key === "buildRequest") return buildRequest;
      if (key === "parseStream") return parse;
      if (key === "parseResponse") return parseResponse;
      const value = Reflect.get(target, key, target);
      return typeof value === "function" ? value.bind(target) : value;
    },
  });
}
