import { randomUUID } from "node:crypto";
import { mkdir, rm } from "node:fs/promises";
import { join } from "node:path";
import type { AdapterEvent, OcxMessage, OcxParsedRequest } from "../node_modules/@bitkyc08/opencodex/src/types.ts";
import type { IncomingMeta } from "../node_modules/@bitkyc08/opencodex/src/adapters/base.ts";
import { acpPrompt, conversationMessages, digest, mcpResult, messageKey, object, replayEstimate, type Row, type ToolResult } from "./antigravity-content.ts";
import { openToolEndpoint } from "./antigravity-mcp.ts";
import { ConversationStore, replyEvents, type Call, type Chunk, type Done, type Reply } from "./antigravity-store.ts";

export type AcpConnection = {
  request(method: string, params: Row): Promise<Row>;
  authenticate(): Promise<unknown>;
  kill(): void;
  closed: Promise<number | null>;
  onNotification: (method: string, params: Row) => void;
  onRequest: (method: string, params: Row) => Row | undefined;
  onDiagnostic: (line: string) => void;
};
export type ConversationHost = {
  directory: string; account: string; owner: string; thread: string; serverName: string; maxRequestBytes: number; idleTimeoutMs: number;
  textHoldMs?: number;
  start(directory: string): AcpConnection;
};
export type ConversationInput = {
  parsed: OcxParsedRequest; systemPrompt: string; tools: Row[]; wireByName: ReadonlyMap<string, string>;
  requireToolCall: boolean; incoming: IncomingMeta; emit(event: AdapterEvent): void;
};

export class AcpFailure extends Error {
  constructor(message: string, readonly code = "antigravity_turn_failed", readonly status = 502, readonly retryable = false) { super(message); }
  event(): Extract<AdapterEvent, { type: "error" }> {
    return { type: "error", message: this.message, code: this.code, status: this.status,
      errorType: this.status === 401 ? "authentication_error" : this.status === 400 ? "invalid_request_error" : "upstream_error", retryable: this.retryable };
  }
}

const TEXT_HOLD_MS = 250;

type Outcome = { call: Call } | { done: Done };
type Exchange = {
  input: ConversationInput; chunks: Chunk[]; text?: string; lastText?: string;
  textTimer?: ReturnType<typeof setTimeout>; settled: boolean;
  outcome: ReturnType<typeof Promise.withResolvers<Outcome>>;
};
type LiveCall = { call: Call; result: ReturnType<typeof Promise.withResolvers<Row>> };
type Native = {
  connection: AcpConnection; configuration: string; endpoint?: ReturnType<typeof openToolEndpoint>;
  session: string; calls: LiveCall[]; waiting?: LiveCall; finished: boolean; sdkError: boolean;
  used?: number; offered: number;
};

function usage(native: Native, turn?: Row): Done["usage"] {
  const occupancy = native.used;
  if (occupancy === undefined) return undefined;
  // PromptResponse.usage sums every model call made inside one ACP prompt.
  // Only a matching single-call breakdown can describe this Responses exchange.
  if (!turn || native.offered > 0 || turn.totalTokens !== occupancy) {
    return { inputTokens: 0, outputTokens: 0, totalTokens: 0, contextTotalTokens: occupancy };
  }
  const tokens = (key: string) => {
    const value = turn[key] ?? 0;
    if (!Number.isSafeInteger(value) || Number(value) < 0) throw new Error(`Invalid Antigravity usage field ${key}.`);
    return Number(value);
  };
  const cached = tokens("cachedReadTokens");
  const input = tokens("inputTokens") + cached + tokens("cachedWriteTokens");
  const output = tokens("outputTokens");
  return { inputTokens: input, outputTokens: output, totalTokens: tokens("totalTokens"), cachedInputTokens: cached,
    reasoningOutputTokens: tokens("thoughtTokens"), contextTotalTokens: occupancy };
}

class Conversation {
  private readonly root: string;
  private store?: ConversationStore;
  private native?: Native;
  private active?: Exchange;
  private tail = Promise.resolve();
  private callers = 0;
  private stopping = false;
  private idleTimer?: ReturnType<typeof setTimeout>;

  constructor(private host: ConversationHost, private identity: string, private evict: () => void) {
    this.root = join(host.directory, "antigravity-sessions", digest(identity));
  }

  private emitChunk(chunk: Chunk): void {
    const active = this.active;
    if (!active || active.settled) return;
    const previous = active.chunks.at(-1);
    if (chunk.type === "text_delta" && previous?.type === "text_delta") previous.text += chunk.text;
    else if (chunk.type === "thinking_delta" && previous?.type === "thinking_delta") previous.thinking += chunk.thinking;
    else active.chunks.push({ ...chunk });
    active.input.emit(chunk);
  }

  private clearTextTimer(active = this.active): void {
    if (active?.textTimer !== undefined) {
      clearTimeout(active.textTimer);
      active.textTimer = undefined;
    }
  }

  private flushText(): void {
    const active = this.active;
    this.clearTextTimer(active);
    if (active?.text !== undefined) {
      this.emitChunk({ type: "text_delta", text: active.text });
      active.text = undefined;
    }
  }

  private scheduleTextFlush(native: Native): void {
    const active = this.active;
    if (!active || active.settled || active.text === undefined || native.finished || native.sdkError) return;
    this.clearTextTimer(active);
    active.textTimer = setTimeout(() => {
      active.textTimer = undefined;
      if (this.native !== native || this.active !== active || active.settled || native.finished || native.sdkError) return;
      this.flushText();
    }, this.host.textHoldMs ?? TEXT_HOLD_MS);
    active.textTimer.unref();
  }

  private offerNext(native: Native): void {
    const active = this.active;
    if (!active || active.settled || native.waiting || !native.calls.length || native.finished) return;
    this.flushText();
    native.waiting = native.calls.shift()!;
    native.offered++;
    active.settled = true;
    active.outcome.resolve({ call: native.waiting.call });
  }

  private releaseIfIdle(): void {
    if (this.callers) return;
    clearTimeout(this.idleTimer);
    if (this.native) {
      this.idleTimer = setTimeout(() => {
        void this.disconnect().finally(() => this.releaseIfIdle());
      }, this.host.idleTimeoutMs);
      this.idleTimer.unref();
      return;
    }
    this.store?.close();
    this.store = undefined;
    this.evict();
  }

  private async disconnect(): Promise<void> {
    clearTimeout(this.idleTimer);
    this.clearTextTimer();
    const native = this.native;
    if (!native) return;
    this.native = undefined;
    native.connection.kill();
    const error = new Error("The native tool connection closed.");
    for (const entry of [...native.calls, ...(native.waiting ? [native.waiting] : [])]) entry.result.reject(error);
    await Promise.all([native.endpoint?.close(), native.connection.closed]);
  }

  async stop(): Promise<void> {
    this.stopping = true;
    clearTimeout(this.idleTimer);
    this.active?.outcome.reject(new AcpFailure("Antigravity bridge is stopping.", "aborted"));
    await this.disconnect();
    await this.tail;
    this.releaseIfIdle();
  }

  async run(input: ConversationInput): Promise<void> {
    this.callers++;
    clearTimeout(this.idleTimer);
    const exchange = this.tail.then(() => this.exchange(input));
    this.tail = exchange.catch(() => undefined);
    try { await exchange; }
    catch (cause) {
      const failure = cause instanceof AcpFailure ? cause : new AcpFailure(cause instanceof Error ? cause.message : String(cause));
      input.emit(failure.event());
    } finally {
      this.callers--;
      this.releaseIfIdle();
    }
  }

  private async connect(input: ConversationInput, configuration: string, resume?: string): Promise<Native> {
    await this.disconnect();
    this.checkAbort(input);
    const connection = this.host.start(this.root);
    const native: Native = { connection, configuration, session: resume ?? "", calls: [], offered: 0, finished: false, sdkError: false };
    this.native = native;
    const { tools, wireByName } = input;
    if (tools.length) {
      native.endpoint = openToolEndpoint(this.host.serverName, tools, this.host.maxRequestBytes, async (name, args, signal) => {
        const wire = wireByName.get(name);
        if (this.native !== native || native.finished || !wire || signal.aborted) throw new Error("The MCP call is outside the active catalog.");
        const entry = { call: { id: `call_${randomUUID()}`, name: wire, arguments: args }, result: Promise.withResolvers<Row>() };
        const cancelled = () => {
          entry.result.reject(new Error("Antigravity cancelled its MCP request."));
          if (this.native !== native) return;
          this.active?.outcome.reject(new AcpFailure("Antigravity cancelled its MCP request.", "aborted"));
          void this.disconnect().finally(() => this.releaseIfIdle());
        };
        signal.addEventListener("abort", cancelled, { once: true });
        native.calls.push(entry);
        this.offerNext(native);
        try { return await entry.result.promise; }
        finally { signal.removeEventListener("abort", cancelled); }
      });
    }
    connection.onNotification = (method, params) => {
      if (this.native !== native || method !== "session/update" || params.sessionId !== native.session) return;
      const update = object(params.update);
      // ACP progress is upstream activity even when it carries no text (or its
      // last text chunk is held until the SDK's success/failure verdict).
      if (this.active && !this.active.settled && typeof update?.sessionUpdate === "string") {
        this.active.input.emit({ type: "heartbeat" });
      }
      if (update?.sessionUpdate === "usage_update" && Number.isSafeInteger(update.used) && Number(update.used) > 0) native.used = Number(update.used);
      if (this.active && !this.active.settled && !native.sdkError
        && (update?.sessionUpdate === "tool_call" || update?.sessionUpdate === "tool_call_update")) {
        this.flushText();
      }
      const content = object(update?.content);
      if (!this.active || this.active.settled || content?.type !== "text" || typeof content.text !== "string") return;
      if (update?.sessionUpdate === "agent_message_chunk") {
        if (!native.sdkError) this.flushText();
        this.active.text = content.text;
        this.active.lastText = content.text;
        this.scheduleTextFlush(native);
      } else if (update?.sessionUpdate === "agent_thought_chunk" && !native.sdkError) {
        this.flushText();
        this.emitChunk({ type: "thinking_delta", thinking: content.text });
      }
    };
    connection.onDiagnostic = line => {
      // In this release prompt() logs its exception and emits the formatted error as the last message chunk with end_turn.
      const marker = /^E\d{4} [\d:.]+\s+\d+ server\.py:\d+\] (?:Antigravity SDK error processing prompt|Agent connection lost|Failed to rebuild agent) for session ([\w-]+)/.exec(line);
      if (marker && marker[1] === native.session) {
        native.sdkError = true;
        this.clearTextTimer();
      }
    };
    connection.onRequest = (method, params) => {
      if (method !== "session/request_permission") return undefined;
      const mcp = object(object(object(params.toolCall)?._meta)?.mcp);
      const allowed = this.native === native && !!native.endpoint && !native.finished && native.session === params.sessionId && mcp?.server === this.host.serverName
        && typeof mcp.tool === "string" && wireByName.has(mcp.tool);
      if (allowed && !native.sdkError) this.flushText();
      const once = allowed && Array.isArray(params.options) ? params.options.map(object).find(option => option?.kind === "allow_once") : undefined;
      return { outcome: once ? { outcome: "selected", optionId: once.optionId } : { outcome: "cancelled" } };
    };
    await connection.authenticate();
    const systemPrompt = tools.length ? [
      input.systemPrompt,
      `Call the client functions with call_mcp_tool on ServerName "${this.host.serverName}".`,
      "Use the exact ToolName and supply Arguments matching its inputSchema. The complete current MCP catalog follows.",
      JSON.stringify(tools),
    ].join("\n") : input.systemPrompt;
    const setup = {
      cwd: join(this.root, "workspace"), mcpServers: native.endpoint ? [native.endpoint.descriptor] : [],
      _meta: { antigravity: { enabledTools: native.endpoint ? ["call_mcp_tool"] : [], systemPrompt: { append: systemPrompt },
        mcpMaxOutputBytes: this.host.maxRequestBytes * 2 } },
    };
    if (resume) await connection.request("session/resume", { ...setup, sessionId: resume });
    else {
      const result = await connection.request("session/new", setup);
      if (typeof result.sessionId !== "string" || !/^[A-Za-z0-9-]+$/.test(result.sessionId)) throw new Error("Antigravity returned no native session identity.");
      native.session = result.sessionId;
    }
    await connection.request("session/set_config_option", { sessionId: native.session, configId: "model", value: input.parsed.modelId });
    const effort = input.parsed.options.reasoning;
    if (effort && effort !== "none") await connection.request("session/set_config_option", { sessionId: native.session, configId: "thinking_level", value: effort });
    return native;
  }

  private async prompt(native: Native, blocks: Row[]): Promise<void> {
    try {
      const result = await native.connection.request("session/prompt", { sessionId: native.session, prompt: blocks });
      native.finished = true;
      this.clearTextTimer();
      // stdout and stderr are independent pipes. EOF of both fences the server's success/failure verdict.
      native.connection.kill();
      await native.connection.closed;
      if (this.native !== native) return;
      if (native.sdkError) {
        const message = this.active?.text || this.active?.lastText || "Antigravity's agent failed during this turn.";
        if (/\binput token count(?:\s*\([\d,]+\))? exceeds the maximum number of tokens allowed\b/i.test(message)) {
          throw new AcpFailure(message, "context_length_exceeded", 400);
        }
        throw new AcpFailure(message, "antigravity_execution_failed");
      }
      if (result.stopReason === "cancelled") throw new AcpFailure("Antigravity cancelled the turn.", "aborted");
      if (result.stopReason === "refusal") throw new AcpFailure("Antigravity refused this request.", "antigravity_refusal", 400);
      if (result.stopReason !== "end_turn" && result.stopReason !== "max_tokens") throw new Error(`Antigravity stopped with '${String(result.stopReason)}'.`);
      this.flushText();
      this.active?.outcome.resolve({ done: { type: "done", stopReason: result.stopReason === "end_turn" ? "stop" : "max_tokens", usage: usage(native, object(result.usage)) } });
    } catch (error) {
      if (this.native === native) this.active?.outcome.reject(error);
    } finally {
      native.finished = true;
      if (this.native === native && !this.active) await this.disconnect();
      this.releaseIfIdle();
    }
  }

  private checkAbort(input: ConversationInput): void {
    if (this.stopping || input.incoming.abortSignal?.aborted) throw new AcpFailure("Antigravity request was cancelled.", "aborted");
  }

  private dispatch(store: ConversationStore, history: OcxMessage[], keys: string[], configuration: string):
    { kind: "tool"; native: Native; pending: LiveCall; result: ToolResult } | { kind: "prompt"; messages: OcxMessage[]; resume?: string } {
    const head = store.head();
    if (!head || keys.length < head.count || digest(keys.slice(0, head.count)) !== head.prefix) return { kind: "prompt", messages: history };
    const previous = store.reply(head.request);
    if (!previous) throw new Error("Antigravity conversation checkpoint has no recorded response.");
    const delta = history.slice(head.count);
    const call = previous.call;
    if (!call) return delta.length ? { kind: "prompt", messages: delta, resume: head.session } : { kind: "prompt", messages: history };
    const results = delta.filter((m): m is ToolResult => m.role === "toolResult" && m.toolCallId === call.id);
    if (results.length !== 1) {
      throw new AcpFailure(`Antigravity requires one result for '${call.id}', received ${results.length}.`, "antigravity_tool_result_mismatch", 400);
    }
    const native = this.native;
    if (delta.length === 1 && native && !native.finished && native.configuration === configuration && native.session === head.session
      && native.waiting?.call.id === call.id) return { kind: "tool", native, pending: native.waiting, result: results[0]! };
    return { kind: "prompt", messages: history };
  }

  private async exchange(input: ConversationInput): Promise<void> {
    this.checkAbort(input);
    if (!this.store) {
      for (const path of [this.root, ...["home", "workspace", "tmp"].map(name => join(this.root, name))]) await mkdir(path, { recursive: true, mode: 0o700 });
      this.store = new ConversationStore(this.root, this.identity);
    }
    const store = this.store;
    const history = conversationMessages(input.parsed.context.messages);
    const keys = history.map(messageKey);
    const configuration = digest([input.parsed.modelId, input.parsed.options.reasoning, input.systemPrompt, input.tools, [...input.wireByName], input.requireToolCall]);
    const estimate = replayEstimate(input.systemPrompt, input.tools, history, input.wireByName);
    const request = digest([configuration, keys]);
    const replay = store.reply(request);
    if (replay) {
      for (const event of replyEvents(replay, estimate)) input.emit(event);
      return;
    }
    const step = this.dispatch(store, history, keys, configuration);
    const active: Exchange = { input, chunks: [], settled: false, outcome: Promise.withResolvers<Outcome>() };
    active.outcome.promise.catch(() => undefined);
    this.active = active;
    const abort = () => {
      active.outcome.reject(new AcpFailure("Antigravity request was cancelled.", "aborted"));
      this.native?.connection.kill();
    };
    input.incoming.abortSignal?.addEventListener("abort", abort, { once: true });
    try {
      this.checkAbort(input);
      input.incoming.beforePhysicalSend?.();
      const native = step.kind === "tool" ? step.native : await this.connect(input, configuration, step.resume);
      this.checkAbort(input);
      // Session setup and previous tool exchanges can report an earlier checkpoint.
      native.used = undefined;
      if (step.kind === "tool") {
        const result = mcpResult(step.result);
        store.beginGeneration();
        input.incoming.onPhysicalSend?.({ ordinal: 1 });
        native.waiting = undefined;
        step.pending.result.resolve(result);
        this.offerNext(native);
      } else {
        const staging = join(this.root, "input");
        await rm(staging, { recursive: true, force: true });
        await mkdir(staging, { mode: 0o700 });
        const vendorNames = new Map([...input.wireByName].map(([name, wire]) => [wire, name]));
        const blocks = await acpPrompt(step.messages, vendorNames, staging);
        this.checkAbort(input);
        store.beginGeneration();
        input.incoming.onPhysicalSend?.({ ordinal: 1 });
        void this.prompt(native, blocks);
      }
      const outcome = await active.outcome.promise;
      active.settled = true;
      if ("done" in outcome && input.requireToolCall) throw new AcpFailure("Antigravity ended without the required tool call.", "antigravity_tool_call_required");
      const call = "call" in outcome ? outcome.call : undefined;
      const done: Done = "done" in outcome ? outcome.done : { type: "done", stopReason: "tool_use", usage: usage(native) };
      const reply: Reply = { chunks: active.chunks, call, done, ...(done.usage ? { usageSource: "acp_usage_update" as const } : {}) };
      const text = active.chunks.flatMap(chunk => chunk.type === "text_delta" ? [chunk.text] : []).join("");
      const assistant: Extract<OcxMessage, { role: "assistant" }> = { role: "assistant", timestamp: 0, content: [] };
      if (text) assistant.content.push({ type: "text", text });
      if (call) assistant.content.push({ type: "toolCall", ...call });
      if (assistant.content.length) {
        const last = history.at(-1);
        if (last?.role === "assistant") keys[keys.length - 1] = messageKey({ ...last, content: [...last.content, ...assistant.content] });
        else keys.push(messageKey(assistant));
      }
      store.finish({ session: native.session, count: keys.length, prefix: digest(keys), request }, reply);
      for (const event of replyEvents(reply, estimate).filter(event => event.type !== "text_delta" && event.type !== "thinking_delta")) input.emit(event);
      if (!call || native.finished) await this.disconnect();
    } catch (error) {
      await this.disconnect();
      this.checkAbort(input);
      throw error;
    } finally {
      this.clearTextTimer(active);
      input.incoming.abortSignal?.removeEventListener("abort", abort);
      this.active = undefined;
    }
  }
}

const conversations = new Map<string, { account: string; conversation: Conversation }>();

export async function runAntigravityConversation(host: ConversationHost, input: ConversationInput): Promise<void> {
  const identity = JSON.stringify([host.directory, host.account, host.owner, host.thread]);
  let entry = conversations.get(identity);
  if (!entry) {
    const conversation = new Conversation(host, identity, () => {
      if (conversations.get(identity)?.conversation === conversation) conversations.delete(identity);
    });
    entry = { account: host.account, conversation };
    conversations.set(identity, entry);
  }
  await entry.conversation.run(input);
}

export async function closeAntigravityConversations(account?: string): Promise<void> {
  await Promise.all([...conversations.values()].filter(entry => account === undefined || account === entry.account).map(entry => entry.conversation.stop()));
}
