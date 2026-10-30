import { z } from "zod";

export const PROTOCOL = "octopus.claude_sdk_child/1";

const requestId = z.number().int().nonnegative();
const name = z.string().min(1).max(256);
const effort = z.enum(["low", "medium", "high", "xhigh", "max"]);

const textInput = z.strictObject({
  type: z.literal("text"),
  text: z.string().min(1),
});

const toolSpec = z.strictObject({
  name: z.string().regex(/^[A-Za-z0-9_-]{1,64}$/),
  description: z.string().min(1),
  input_schema: z.record(z.string(), z.unknown()),
});

export const sessionConfig = z.strictObject({
  model: z.string().min(1),
  system_prompt: z.string(),
  cwd: z.string().min(1),
  effort: effort.optional(),
  resume: z.string().min(1).nullable(),
  max_turns: z.number().int().positive().optional(),
  tools: z.array(toolSpec).max(512),
});

export const startCommand = z.strictObject({
  type: z.literal("start"),
  request_id: requestId,
  session: name,
  config: sessionConfig,
});

export const turnCommand = z.strictObject({
  type: z.literal("turn"),
  request_id: requestId,
  session: name,
  turn: name,
  input: z.array(textInput).min(1),
});

export const steerCommand = z.strictObject({
  type: z.literal("steer"),
  request_id: requestId,
  session: name,
  turn: name,
  as_turn: name.nullable(),
  input: z.array(textInput).min(1),
});

export const interruptCommand = z.strictObject({
  type: z.literal("interrupt"),
  request_id: requestId,
  session: name,
  turn: name,
});

export const toolResultCommand = z.strictObject({
  type: z.literal("tool_result"),
  request_id: requestId,
  session: name,
  turn: name,
  call_id: name,
  content: z.array(textInput),
  is_error: z.boolean(),
});

export const closeCommand = z.strictObject({
  type: z.literal("close"),
  request_id: requestId,
  session: name,
});

export const command = z.discriminatedUnion("type", [
  startCommand,
  turnCommand,
  steerCommand,
  interruptCommand,
  toolResultCommand,
  closeCommand,
]);

const usage = z.strictObject({
  input_tokens: z.number().int().nonnegative(),
  output_tokens: z.number().int().nonnegative(),
  cache_creation_input_tokens: z.number().int().nonnegative(),
  cache_read_input_tokens: z.number().int().nonnegative(),
});

export const readyEvent = z.strictObject({
  type: z.literal("ready"),
  protocol: z.literal(PROTOCOL),
  sdk_version: z.string().min(1),
  claude_version: z.string().min(1),
});

export const ackEvent = z.strictObject({
  type: z.literal("ack"),
  request_id: requestId,
});

export const sessionEvent = z.strictObject({
  type: z.literal("session"),
  session: name,
  claude_session_id: z.string().min(1),
  model: z.string().min(1),
  tools: z.array(z.string()),
});

export const itemEvent = z.strictObject({
  type: z.literal("item"),
  session: name,
  turn: name,
  item: z.strictObject({
    kind: z.enum(["text", "thinking"]),
    text: z.string(),
    message_id: z.string().min(1).nullable(),
  }),
});

export const textDeltaEvent = z.strictObject({
  type: z.literal("text_delta"),
  session: name,
  turn: name,
  message_id: z.string().min(1),
  text: z.string().min(1),
});

export const inputRecordedEvent = z.strictObject({
  type: z.literal("input_recorded"),
  session: name,
  turn: name,
  text: z.string(),
  synthetic: z.boolean(),
});

export const toolCallEvent = z.strictObject({
  type: z.literal("tool_call"),
  session: name,
  turn: name,
  call_id: name,
  name: z.string().min(1),
  input: z.record(z.string(), z.unknown()),
});

export const usageEvent = z.strictObject({
  type: z.literal("usage"),
  session: name,
  turn: name,
  usage,
  total_cost_usd: z.number().nonnegative(),
  duration_ms: z.number().nonnegative(),
  num_turns: z.number().int().nonnegative(),
});

export const turnEndEvent = z.strictObject({
  type: z.literal("turn_end"),
  session: name,
  turn: name,
  status: z.enum(["completed", "interrupted", "failed"]),
  result: z.string().nullable(),
  claude_session_id: z.string().min(1).nullable(),
});

export const errorEvent = z.strictObject({
  type: z.literal("error"),
  request_id: requestId.nullable(),
  session: name.nullable(),
  turn: name.nullable(),
  code: z.string().regex(/^[a-z][a-z0-9_]*$/),
  message: z.string().min(1),
  fatal: z.boolean(),
});

export const event = z.discriminatedUnion("type", [
  readyEvent,
  ackEvent,
  sessionEvent,
  itemEvent,
  textDeltaEvent,
  inputRecordedEvent,
  toolCallEvent,
  usageEvent,
  turnEndEvent,
  errorEvent,
]);

export type Command = z.infer<typeof command>;
export type SessionConfig = z.infer<typeof sessionConfig>;
export type Event = z.infer<typeof event>;

export function protocolJsonSchema(): Record<string, unknown> {
  return {
    $schema: "https://json-schema.org/draft/2020-12/schema",
    $id: "https://octopus.local/schemas/claude_sdk_child_protocol.schema.json",
    title: "Octopus Claude SDK child protocol",
    description:
      "One JSON object per line. The provider life writes `command` frames to the child's stdin; the child writes `event` frames to stdout. Every command is answered by exactly one `ack` or one `error` carrying its request_id.",
    $defs: {
      command: definition(command),
      event: definition(event),
    },
  };
}

function definition(schema: z.ZodType): Record<string, unknown> {
  const { $schema: _dialect, ...rest } = z.toJSONSchema(schema, { target: "draft-2020-12" }) as Record<string, unknown>;
  return rest;
}
