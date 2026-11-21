import { Database } from "bun:sqlite";
import { closeSync, openSync } from "node:fs";
import { join } from "node:path";
import type { AdapterEvent } from "../node_modules/@bitkyc08/opencodex/src/types.ts";
import { object, type Row } from "./antigravity-content.ts";

export type Call = { id: string; name: string; arguments: Row };
export type Chunk = Extract<AdapterEvent, { type: "thinking_delta" | "text_delta" }>;
export type Done = Extract<AdapterEvent, { type: "done" }>;
export type Reply = { chunks: Chunk[]; call?: Call; done: Done; usageSource?: "acp_usage_update" };
export type Head = { session: string; count: number; prefix: string; request: string };

function checkedReply(text: string): Reply {
  const reply = object(JSON.parse(text));
  const call = object(reply?.call);
  const done = object(reply?.done);
  const usage = done?.usage;
  if (!Array.isArray(reply?.chunks) || !reply.chunks.every(value => {
    const chunk = object(value);
    return chunk?.type === "text_delta" && typeof chunk.text === "string"
      || chunk?.type === "thinking_delta" && typeof chunk.thinking === "string";
  }) || done?.type !== "done" || !["stop", "max_tokens", "tool_use"].includes(String(done.stopReason))
    || (reply.call !== undefined && (!call || typeof call.id !== "string" || !call.id || typeof call.name !== "string" || !call.name || !object(call.arguments)))
    || (done.stopReason === "tool_use") !== Boolean(call)
    || (usage !== undefined && (!object(usage) || !Number.isSafeInteger(object(usage)?.inputTokens)
      || !Number.isSafeInteger(object(usage)?.outputTokens) || !Object.values(usage as Row).every(n => Number.isSafeInteger(n) && Number(n) >= 0)))) {
    throw new Error("Antigravity's retained response is invalid.");
  }
  // Earlier checkpoints could store a prompt-wide sum or a reading from a previous
  // exchange. Preserve the recorded answer and call identity when replaying them.
  if (reply.usageSource === undefined) delete done.usage;
  else if (reply.usageSource !== "acp_usage_update" || !Number.isSafeInteger(object(usage)?.contextTotalTokens)) {
    throw new Error("Antigravity's retained context usage is invalid.");
  }
  return reply as Reply;
}

export function replyEvents(reply: Reply, replayInputTokens: number): AdapterEvent[] {
  const reported = reply.done.usage;
  const replyTokens = Math.ceil(Buffer.byteLength(JSON.stringify([reply.chunks, reply.call])) / 4);
  const replayTokens = replayInputTokens + replyTokens;
  const nativeTokens = reported?.contextTotalTokens;
  const estimated = nativeTokens === undefined || replayTokens > nativeTokens;
  // ACP may compact its own trajectory while the caller still retains the complete
  // replay. Either history must fit when the process or tool catalog changes.
  const usage = {
    ...(reported ?? { inputTokens: 0, outputTokens: 0, totalTokens: 0 }),
    contextTotalTokens: Math.max(nativeTokens ?? 0, replayTokens),
    estimated,
    rawUsage: {
      context_estimated: estimated,
      native_context_tokens: nativeTokens ?? null,
      replay_estimated_tokens: replayTokens,
    },
  };
  const call = reply.call;
  return [...reply.chunks, ...(call ? [
    { type: "tool_call_start" as const, id: call.id, name: call.name },
    { type: "tool_call_delta" as const, arguments: JSON.stringify(call.arguments) }, { type: "tool_call_end" as const },
  ] : []), { ...reply.done, usage }];
}

export class ConversationStore {
  private readonly db: Database;

  constructor(directory: string, identity: string) {
    const path = join(directory, "conversation.sqlite");
    closeSync(openSync(path, "a", 0o600));
    this.db = new Database(path);
    try {
      // The live transport and its durable checkpoint have one writer, including across bridge processes.
      this.db.exec("PRAGMA busy_timeout=0; PRAGMA locking_mode=EXCLUSIVE; PRAGMA synchronous=FULL; BEGIN EXCLUSIVE");
      if (!this.db.query("SELECT name FROM sqlite_master WHERE type='table' LIMIT 1").get()) {
        this.db.exec(`CREATE TABLE owner (identity TEXT PRIMARY KEY) STRICT;
          CREATE TABLE replies (request TEXT PRIMARY KEY, reply TEXT NOT NULL) STRICT;
          CREATE TABLE head (id INTEGER PRIMARY KEY CHECK(id=1), session TEXT NOT NULL, count INTEGER NOT NULL,
            prefix TEXT NOT NULL, request TEXT NOT NULL REFERENCES replies(request)) STRICT`);
        this.db.query("INSERT INTO owner VALUES (?)").run(identity);
      }
      const owner = this.db.query<{ identity: string }, []>("SELECT identity FROM owner").all();
      if (owner.length !== 1 || owner[0]!.identity !== identity) throw new Error("Antigravity conversation belongs to a different provider or thread.");
      this.db.exec("COMMIT");
    } catch (error) { this.db.close(); throw error; }
  }

  head(): Head | undefined {
    const row = this.db.query<Head, []>("SELECT session, count, prefix, request FROM head WHERE id=1").get();
    if (!row) return undefined;
    if (!/^[A-Za-z0-9-]+$/.test(row.session) || !Number.isSafeInteger(row.count) || row.count < 0
      || !/^[a-f0-9]{64}$/.test(row.prefix) || !/^[a-f0-9]{64}$/.test(row.request)) throw new Error("Antigravity conversation checkpoint is invalid.");
    return row;
  }

  reply(request: string): Reply | undefined {
    const row = this.db.query<{ reply: string }, [string]>("SELECT reply FROM replies WHERE request=?").get(request);
    return row ? checkedReply(row.reply) : undefined;
  }

  beginGeneration(): void {
    // Only a completed exchange may reuse a native trajectory. An interrupted generation starts from caller history.
    this.db.exec("DELETE FROM head");
  }

  finish(head: Head, reply: Reply): void {
    this.db.transaction(() => {
      this.db.query("INSERT INTO replies VALUES (?, ?)").run(head.request, JSON.stringify(reply));
      this.db.query("INSERT INTO head VALUES (1, ?, ?, ?, ?)").run(head.session, head.count, head.prefix, head.request);
    })();
  }

  close(): void { this.db.close(); }
}
