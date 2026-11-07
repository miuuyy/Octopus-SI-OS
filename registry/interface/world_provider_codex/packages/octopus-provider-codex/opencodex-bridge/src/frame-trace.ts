import { spawn as nodeSpawn, type ChildProcess, type SpawnOptions } from "node:child_process";
import { Transform } from "node:stream";

type SpawnFn = (command: string, args: readonly string[], options: SpawnOptions) => ChildProcess;
type ErrorEvent = { type: "error"; message: string; code?: string };

const MAX_ENTRIES = 48;
const MAX_LINE_BYTES = 64 * 1024;
const TRACED_CODES = new Set(["protocol_error", "tool_bridge_init_missing"]);
const REFUSAL_BOUNDARY = "The model's response ended with stop_reason refusal; the CLI's continuation is discarded.";

export class FrameTrace {
  private entries: Array<{ text: string; count: number }> = [];
  private dropped = 0;
  private openMessage = false;
  refused = false;

  boundary(line: string): string | undefined {
    let frame: unknown;
    try {
      frame = JSON.parse(line);
    } catch {
      return undefined;
    }
    if (field(frame, "type") !== "stream_event") return undefined;
    const event = field(frame, "event");
    const kind = field(event, "type");
    if (kind === "message_start") {
      const cut = this.openMessage;
      this.openMessage = true;
      return cut ? "Claude Code started another assistant message before the previous one finished; the cut message and the CLI's own continuation are discarded." : undefined;
    }
    if (kind === "message_stop") this.openMessage = false;
    if (kind === "message_delta" && field(field(event, "delta"), "stop_reason") === "refusal") {
      this.refused = true;
      return REFUSAL_BOUNDARY;
    }
    return undefined;
  }

  record(text: string): void {
    const last = this.entries.at(-1);
    if (last?.text === text) {
      last.count += 1;
      return;
    }
    this.entries.push({ text, count: 1 });
    if (this.entries.length > MAX_ENTRIES) {
      this.entries.shift();
      this.dropped += 1;
    }
  }

  render(): string {
    const items = this.entries.map(({ text, count }) => (count > 1 ? `${text}x${count}` : text));
    return (this.dropped ? [`…${this.dropped} earlier`, ...items] : items).join(" ");
  }
}

function field(value: unknown, key: string): unknown {
  return value !== null && typeof value === "object" ? (value as Record<string, unknown>)[key] : undefined;
}

function text(value: unknown): string | undefined {
  return typeof value === "string" && value.length <= 128 ? value : undefined;
}

export function summarizeFrame(line: string): string {
  let frame: unknown;
  try {
    frame = JSON.parse(line);
  } catch {
    return "unparsed";
  }
  const type = text(field(frame, "type")) ?? "untyped";
  if (type === "stream_event") {
    const event = field(frame, "event");
    const kind = text(field(event, "type")) ?? "untyped";
    const index = field(event, "index");
    const at = typeof index === "number" ? `[${index}]` : "";
    if (kind === "content_block_start") {
      const block = field(event, "content_block");
      const name = text(field(block, "name"));
      return `block_start${at}:${text(field(block, "type")) ?? "untyped"}${name ? `(${name})` : ""}`;
    }
    if (kind === "content_block_delta") return `delta${at}:${text(field(field(event, "delta"), "type")) ?? "untyped"}`;
    if (kind === "content_block_stop") return `block_stop${at}`;
    if (kind === "message_delta") return `message_delta:${text(field(field(event, "delta"), "stop_reason")) ?? "none"}`;
    return kind;
  }
  if (type === "assistant" || type === "user") {
    const content = field(field(frame, "message"), "content");
    const kinds = Array.isArray(content) ? content.map((item) => text(field(item, "type")) ?? "untyped") : [];
    return `${type}(${kinds.join(",")})`;
  }
  if (type === "result" || type === "system") {
    const subtype = text(field(frame, "subtype"));
    const error = field(frame, "is_error") === true ? ",error" : "";
    return `${type}${subtype ? `:${subtype}` : ""}${error}`;
  }
  return type;
}

function observeLines(trace: FrameTrace): Transform {
  const decoder = new TextDecoder();
  let pending = "";
  let skipping = false;
  return new Transform({
    transform(chunk: Buffer, _encoding, callback) {
      const lines = (pending + decoder.decode(chunk, { stream: true })).split("\n");
      pending = lines.pop() ?? "";
      for (const line of lines) {
        if (skipping) {
          skipping = false;
          trace.record("oversized");
        } else if (line.trim()) {
          trace.record(summarizeFrame(line));
          const boundary = trace.boundary(line);
          if (boundary) {
            callback(new Error(boundary));
            return;
          }
        }
      }
      if (pending.length > MAX_LINE_BYTES) {
        pending = "";
        skipping = true;
      }
      callback(null, chunk);
    },
  });
}

export function tracedSpawn(trace: FrameTrace, spawn: SpawnFn = nodeSpawn): SpawnFn {
  return (command, args, options) => {
    const child = spawn(command, args, options);
    const stdout = child.stdout;
    if (stdout) {
      const observed = observeLines(trace);
      stdout.on("error", (error) => observed.destroy(error));
      child.stdout = stdout.pipe(observed);
    }
    return child;
  };
}

export function withFrameTrace<E extends { type: string }>(emit: (event: E) => void, trace: FrameTrace): (event: E) => void {
  return (event) => {
    const error = event as unknown as ErrorEvent;
    if (error.type === "error" && trace.refused && error.message.includes(REFUSAL_BOUNDARY)) {
      emit({ type: "incomplete", reason: "content_filter", retryable: false,
        message: 'Claude Code ended the response with stop_reason "refusal"' } as unknown as E);
      return;
    }
    if (error.type === "error" && error.code !== undefined && TRACED_CODES.has(error.code)) {
      emit({ ...event, message: `${error.message} CLI frames: ${trace.render()}` });
      return;
    }
    emit(event);
  };
}
