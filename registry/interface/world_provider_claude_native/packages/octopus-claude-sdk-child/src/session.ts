import { query, type Query, type SDKMessage, type SDKUserMessage } from "@anthropic-ai/claude-agent-sdk";
import type { Event, SessionConfig } from "./protocol.ts";
import { ChildFailure } from "./claude.ts";
import { OctopusToolServer, TOOL_SERVER, type ToolCallRequest } from "./tools.ts";

export const CLIENT_APP = "octopus-claude-native/0.1.0";
const FORWARDED_ENV = ["HOME", "PATH", "USER", "LOGNAME", "LANG", "TMPDIR", "HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "NO_PROXY"];

const BUILTIN_PLUGINS = ["cc-plugin-agents-md@builtin", "cc-plugin-telemetry@builtin", "cc-plugin-plugin-authoring@builtin"];

function claudeEnvironment(): Record<string, string> {
  const env: Record<string, string> = { CLAUDE_AGENT_SDK_CLIENT_APP: CLIENT_APP };
  for (const name of FORWARDED_ENV) {
    const value = process.env[name];
    if (value !== undefined) env[name] = value;
  }
  if (env.TMPDIR !== undefined) env.CLAUDE_CODE_TMPDIR = env.TMPDIR;
  return env;
}

type Emit = (event: Event) => void;

type ActiveTurn = {
  id: string;
  interrupted: boolean;
  claudeError: string | null;
  streamingMessageId: string | null;
  /** A steer text waiting to start its own Claude turn once the interrupted one has ended. */
  steer: string | null;
  /** The turn id the steer's own Claude turn reports under. */
  steerTurnId: string | null;
  /** True while the Claude turn running is the steer's own. */
  steering: boolean;
};

class UserMessageQueue implements AsyncIterable<SDKUserMessage> {
  private pending: SDKUserMessage[] = [];
  private waiting: ((result: IteratorResult<SDKUserMessage>) => void) | null = null;
  private ended = false;

  push(message: SDKUserMessage): void {
    if (this.ended) throw new ChildFailure("session_closed", "the session no longer accepts input");
    if (this.waiting) {
      const resolve = this.waiting;
      this.waiting = null;
      resolve({ value: message, done: false });
      return;
    }
    this.pending.push(message);
  }

  end(): void {
    this.ended = true;
    if (this.waiting) {
      const resolve = this.waiting;
      this.waiting = null;
      resolve({ value: undefined, done: true });
    }
  }

  [Symbol.asyncIterator](): AsyncIterator<SDKUserMessage> {
    return {
      next: () => {
        const message = this.pending.shift();
        if (message) return Promise.resolve({ value: message, done: false });
        if (this.ended) return Promise.resolve({ value: undefined, done: true });
        return new Promise((resolve) => {
          this.waiting = resolve;
        });
      },
    };
  }
}

export class Session {
  private readonly input = new UserMessageQueue();
  private readonly query: Query;
  private readonly grantedTools: Set<string>;
  private readonly tools: OctopusToolServer | null;
  private active: ActiveTurn | null = null;
  private claudeSessionId: string | null;
  private closed = false;
  private failed: ChildFailure | null = null;
  readonly finished: Promise<void>;

  constructor(
    readonly id: string,
    config: SessionConfig,
    claudeExecutable: string,
    private readonly emit: Emit,
  ) {
    this.tools = config.tools.length > 0 ? new OctopusToolServer(config.tools, (call) => this.onToolCall(call)) : null;
    this.grantedTools = new Set(this.tools?.grantedNames ?? []);
    this.claudeSessionId = config.resume;
    this.query = query({
      prompt: this.input,
      options: {
        pathToClaudeCodeExecutable: claudeExecutable,
        env: claudeEnvironment(),
        cwd: config.cwd,
        model: config.model,
        systemPrompt: config.system_prompt,
        ...(config.effort ? { effort: config.effort } : {}),
        ...(config.max_turns ? { maxTurns: config.max_turns } : {}),
        ...(config.resume ? { resume: config.resume } : {}),
        tools: [],
        allowedTools: [...this.grantedTools],
        mcpServers: this.tools ? { [TOOL_SERVER]: this.tools.config } : {},
        strictMcpConfig: true,
        settingSources: [],
        settings: {
          syncClaudeAiPlugins: false,
          disableAllHooks: true,
          disableSkillShellExecution: true,
          enabledPlugins: Object.fromEntries(BUILTIN_PLUGINS.map((plugin) => [plugin, false])),
        },
        skills: [],
        plugins: [],
        verbatimPrompts: true,
        includePartialMessages: true,
        permissionMode: "dontAsk",
        stderr: (data) => process.stderr.write(data),
      },
    });
    this.finished = this.pump();
  }

  turn(turnId: string, text: string): void {
    if (this.failed) throw this.failed;
    if (this.closed) throw new ChildFailure("session_closed", `session '${this.id}' is closed`);
    if (this.active) {
      throw new ChildFailure("turn_in_progress", `turn '${this.active.id}' is still running in session '${this.id}'`);
    }
    this.active = { id: turnId, interrupted: false, claudeError: null, streamingMessageId: null, steer: null, steerTurnId: null, steering: false };
    this.input.push({
      type: "user",
      message: { role: "user", content: text },
      parent_tool_use_id: null,
    });
  }

  /**
   * Redirects the running turn: Claude Code is interrupted at once, and the
   * steer text starts a new Claude turn in the same session under the same
   * Octopus turn id, so the caller sees one turn whose answer follows the
   * steer. Claude Code's own queueing would instead attach the text to the
   * next tool result, or run it after the turn, neither of which the caller
   * can observe.
   */
  async steer(turnId: string, text: string, asTurn: string | null): Promise<void> {
    if (this.failed) throw this.failed;
    if (!this.active || this.active.id !== turnId) {
      throw new ChildFailure("turn_not_active", `turn '${turnId}' is not running in session '${this.id}'`);
    }
    if (this.active.steer !== null || this.active.steering) {
      throw new ChildFailure("steer_in_progress", `turn '${turnId}' is already being steered`);
    }
    this.active.steer = text;
    this.active.steerTurnId = asTurn ?? turnId;
    this.tools?.abandonAll("the turn was steered before this tool call returned");
    await this.query.interrupt();
  }

  async interrupt(turnId: string): Promise<void> {
    if (!this.active || this.active.id !== turnId) {
      throw new ChildFailure("turn_not_active", `turn '${turnId}' is not running in session '${this.id}'`);
    }
    this.active.interrupted = true;
    this.tools?.abandonAll("the turn was interrupted before this tool call returned");
    await this.query.interrupt();
  }

  resolveTool(callId: string, text: string, isError: boolean): void {
    if (this.failed) throw this.failed;
    if (!this.tools) throw new ChildFailure("unknown_tool_call", `session '${this.id}' grants no tools`);
    this.tools.resolve(callId, text, isError);
  }

  private onToolCall(call: ToolCallRequest): void {
    const turn = this.active;
    if (!turn || turn.interrupted) {
      this.tools?.resolve(call.callId, "no turn is waiting for this tool call", true);
      return;
    }
    this.emit({ type: "tool_call", session: this.id, turn: turn.id, call_id: call.callId, name: call.name, input: call.input });
  }

  async close(): Promise<void> {
    if (this.closed) return;
    this.closed = true;
    this.tools?.abandonAll("the session was closed");
    this.input.end();
    this.query.close();
    await this.finished.catch(() => undefined);
  }

  private async pump(): Promise<void> {
    try {
      for await (const message of this.query) {
        this.handle(message);
        if (this.failed) break;
      }
      if (!this.closed && !this.failed) {
        this.fail(new ChildFailure("claude_session_ended", "Claude Code ended the session without being closed"));
      }
    } catch (error) {
      if (!this.closed) {
        this.fail(error instanceof ChildFailure ? error : new ChildFailure("claude_session_failed", errorText(error)));
      }
    } finally {
      this.closeTurnAsFailed();
    }
  }

  private handle(message: SDKMessage): void {
    switch (message.type) {
      case "system":
        if (message.subtype === "init") this.admitInit(message);
        return;
      case "assistant":
        this.admitAssistant(message);
        return;
      case "stream_event":
        this.admitStreamEvent(message);
        return;
      case "result":
        this.admitResult(message);
        return;
      default:
        return;
    }
  }

  private admitInit(message: Extract<SDKMessage, { type: "system"; subtype: "init" }>): void {
    const ungranted = message.tools.filter((tool) => !this.grantedTools.has(tool));
    const servers = message.mcp_servers.filter((server) => server.name !== TOOL_SERVER || this.grantedTools.size === 0);
    if (ungranted.length > 0 || servers.length > 0 || message.plugins.length > 0) {
      this.fail(new ChildFailure(
        "tool_isolation_violated",
        `Claude Code started with capabilities this session did not grant: ${[...ungranted, ...servers.map((s) => `mcp:${s.name}`), ...message.plugins.map((p) => `plugin:${p.name}`)].join(", ")}`,
      ));
      return;
    }
    this.claudeSessionId = message.session_id;
    this.emit({ type: "session", session: this.id, claude_session_id: message.session_id, model: message.model, tools: message.tools });
  }

  private admitStreamEvent(message: Extract<SDKMessage, { type: "stream_event" }>): void {
    if (message.parent_tool_use_id !== null) return;
    const turn = this.active;
    if (!turn) return;
    const event = message.event;
    if (event.type === "message_start") {
      turn.streamingMessageId = event.message.id;
    } else if (event.type === "content_block_delta" && event.delta.type === "text_delta" && event.delta.text.length > 0) {
      if (turn.streamingMessageId === null) return;
      this.emit({ type: "text_delta", session: this.id, turn: turn.id, message_id: turn.streamingMessageId, text: event.delta.text });
    }
  }

  private admitAssistant(message: Extract<SDKMessage, { type: "assistant" }>): void {
    if (message.parent_tool_use_id !== null) return;
    const turn = this.active;
    if (!turn) {
      this.fail(new ChildFailure("unexpected_output", "Claude Code produced output outside a turn"));
      return;
    }
    if (message.error) {
      turn.claudeError ??= message.error;
      return;
    }
    for (const block of message.message.content) {
      if (block.type === "text") {
        this.emit({ type: "item", session: this.id, turn: turn.id, item: { kind: "text", text: block.text, message_id: message.message.id || null } });
      } else if (block.type === "thinking" && block.thinking.length > 0) {
        this.emit({ type: "item", session: this.id, turn: turn.id, item: { kind: "thinking", text: block.thinking, message_id: message.message.id || null } });
      } else if (block.type === "tool_use" && !this.grantedTools.has(block.name)) {
        this.fail(new ChildFailure("tool_isolation_violated", `Claude Code requested ungranted tool '${block.name}'`));
        return;
      }
    }
  }

  private admitResult(message: Extract<SDKMessage, { type: "result" }>): void {
    const turn = this.active;
    if (!turn) {
      this.fail(new ChildFailure("unexpected_output", "Claude Code finished a turn that was not started"));
      return;
    }
    if (turn.steer !== null && !turn.interrupted) {
      const text = turn.steer;
      turn.steer = null;
      turn.steering = true;
      turn.id = turn.steerTurnId ?? turn.id;
      turn.streamingMessageId = null;
      this.claudeSessionId = message.session_id;
      this.emit({ type: "input_recorded", session: this.id, turn: turn.id, text, synthetic: false });
      this.input.push({ type: "user", message: { role: "user", content: text }, parent_tool_use_id: null });
      return;
    }
    this.active = null;
    this.claudeSessionId = message.session_id;
    this.emit({
      type: "usage",
      session: this.id,
      turn: turn.id,
      usage: {
        input_tokens: message.usage.input_tokens,
        output_tokens: message.usage.output_tokens,
        cache_creation_input_tokens: message.usage.cache_creation_input_tokens,
        cache_read_input_tokens: message.usage.cache_read_input_tokens,
      },
      total_cost_usd: message.total_cost_usd,
      duration_ms: message.duration_ms,
      num_turns: message.num_turns,
    });
    const succeeded = message.subtype === "success" && !message.is_error && turn.claudeError === null;
    if (!succeeded && !turn.interrupted) {
      const detail = message.subtype === "success" ? message.result : message.errors.join("; ");
      this.emit({
        type: "error",
        request_id: null,
        session: this.id,
        turn: turn.id,
        code: turn.claudeError ? `claude_${turn.claudeError}` : "claude_turn_failed",
        message: `Claude Code turn failed (${turn.claudeError ?? message.subtype})${detail ? `: ${detail}` : ""}`,
        fatal: false,
      });
    }
    this.emit({
      type: "turn_end",
      session: this.id,
      turn: turn.id,
      status: turn.interrupted ? "interrupted" : succeeded ? "completed" : "failed",
      result: message.subtype === "success" && succeeded ? message.result : null,
      claude_session_id: this.claudeSessionId,
    });
  }

  private fail(failure: ChildFailure): void {
    if (this.failed) return;
    this.failed = failure;
    this.emit({
      type: "error",
      request_id: null,
      session: this.id,
      turn: this.active?.id ?? null,
      code: failure.code,
      message: failure.message,
      fatal: false,
    });
    this.closed = true;
    this.tools?.abandonAll("the session failed");
    this.input.end();
    this.query.close();
  }

  private closeTurnAsFailed(): void {
    const turn = this.active;
    if (!turn) return;
    this.active = null;
    this.emit({
      type: "turn_end",
      session: this.id,
      turn: turn.id,
      status: turn.interrupted ? "interrupted" : "failed",
      result: null,
      claude_session_id: this.claudeSessionId,
    });
  }
}

export function errorText(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
