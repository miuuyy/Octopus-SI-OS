module.exports = { activityDirection, activityPayloadText, appendOperatorChatAssistantMessage, clientEventDataProjection, clientEventHasProcessOutput, clientEventHasUnifiedDiff, clientReasoningSummaryProjection, compactClientEventValue, nativeTurnRecordedInputs, operatorActivityAgent, operatorActivityMessage, operatorChatActivityPayload, operatorChatAssistantItemCompleted, operatorChatAttachments, operatorChatEventData, operatorChatEventIdentity, operatorChatEventKind, operatorChatEventText, operatorChatExecutionScope, operatorChatExpandedStreamRows, operatorChatMovementContext, operatorChatProjectedRowId, operatorChatProviderFailureActivity, operatorChatRequestKey, operatorChatResponseKey, operatorChatRunProvider, operatorChatRuntimeProviderId, operatorChatSearchMessage, operatorChatStreamPositionCovered, operatorChatTurnText, operatorTurnControlEndsTurn, operatorTurnControlOutcome, operatorTurnStreamIdentity, validateOperatorRecordedInput };

const crypto = require("node:crypto");
const { setImmediate: yieldToClient } = require("node:timers/promises");
const { CLIENT_EVENT_DATA_MAX_ARRAY_ITEMS, CLIENT_EVENT_DATA_MAX_BYTES, CLIENT_EVENT_DATA_MAX_DEPTH, CLIENT_EVENT_DATA_MAX_OBJECT_KEYS, CLIENT_EVENT_DATA_MAX_STRING_LENGTH, CLIENT_REASONING_SUMMARY_MAX_BYTES } = require("./constants.cjs");
const { indexedUnsignedInteger, movementBaseKind, movementBoundaryInterface, movementKindText, movementRunId } = require("./movementRows.cjs");
const { isOperatorChatProviderFailureRow } = require("./registryRows.cjs");
const { arrayValue, contentHashHex, ledgerArtifactId, ledgerArtifactLabel, numberValue, objectOrEmpty, requiredString, rowTimeMs, streamFragmentValue, stringValue } = require("./values.cjs");

function nativeTurnRecordedInputs(payload) {
  const turn = payload?.turn;
  const exactId = (value) => typeof value === "string" && value.length > 0
    && value.trim() === value && !/[\r\n\0]/.test(value);
  if (!exactId(payload?.threadId) || !exactId(turn?.id) || turn.itemsView !== "full"
    || !["completed", "failed", "interrupted"].includes(turn.status) || !Array.isArray(turn.items)
    || payload._payload_projection?.truncated === true && payload._payload_projection.resolved !== true
    || payload.thread_id !== undefined && payload.thread_id !== payload.threadId
    || payload.turn_id !== undefined && payload.turn_id !== turn.id) return [];
  const inputs = [];
  for (const item of turn.items) {
    if (item?.type !== "userMessage" || item.clientId === null || item.clientId === undefined) continue;
    if (!exactId(item.id) || !exactId(item.clientId) || !Array.isArray(item.content)) return [];
    inputs.push({ thread_id: payload.threadId, turn_id: turn.id, item_id: item.id, client_id: item.clientId });
  }
  return inputs;
}

function activityDirection(row, runId) {
  if (movementRunId(row) !== runId) return "internal";
  const kind = movementBaseKind(row);
  if (kind === "InterfaceArrival") return "in";
  if (kind === "InterfaceOutbound") return "out";
  return "internal";
}

function activityPayloadText(payload) {
  return [payload.text, payload.message, payload.content, payload.summary, payload.status_message]
    .map(stringValue)
    .find(Boolean) || "";
}

function operatorChatTurnText(payload, direction) {
  if (!payload || typeof payload !== "object") return "";
  if (typeof payload.__raw === "string") return payload.__raw.trim();
  if (direction === "in") {
    const text = stringValue(payload.text);
    if (text) return text;
    const names = arrayValue(payload.attachments).map((item) => stringValue(item?.name)).filter(Boolean);
    return names.length ? names.map((name) => `[attachment] ${name}`).join("\n") : "";
  }
  const data = objectOrEmpty(payload.data);
  const terminal = arrayValue(payload.terminal_replies)[0];
  const text = payload.reply
    ?? (terminal ? terminal.reply : undefined)
    ?? data.text
    ?? payload.text
    ?? payload.message
    ?? payload.error;
  return stringValue(text);
}

function operatorChatEventIdentity(payload, eventKind) {
  const item = objectOrEmpty(payload?.item);
  const turn = objectOrEmpty(payload?.turn);
  const turnId = stringValue(
    payload?.turn_id
    ?? payload?.turnId
    ?? turn.id,
  );
  const itemId = stringValue(
    payload?.item_id
    ?? payload?.itemId
    ?? item.id
    ?? payload?.id,
  );
  const lifecycle = stringValue(payload?.lifecycle)
    || (eventKind === "item_started" || eventKind === "turn_started" ? "started" : "")
    || (eventKind === "turn_completed" ? "completed" : "")
    || (eventKind.endsWith("_delta") || eventKind.endsWith("_progress") ? "progress" : "");
  const startedAtMs = numberValue(
    payload?.started_at_ms
    ?? payload?.startedAtMs
    ?? turn.started_at
    ?? turn.startedAt,
    0,
  );
  const completedAtMs = numberValue(
    payload?.completed_at_ms
    ?? payload?.completedAtMs
    ?? turn.completed_at
    ?? turn.completedAt,
    0,
  );
  const durationMs = numberValue(
    payload?.duration_ms
    ?? payload?.durationMs
    ?? turn.duration_ms
    ?? turn.durationMs,
    0,
  );
  return {
    turn_id: turnId,
    item_id: itemId,
    lifecycle,
    started_at_ms: startedAtMs || null,
    completed_at_ms: completedAtMs || null,
    duration_ms: durationMs || null,
  };
}

function operatorChatMovementContext(row) {
  const metadata = objectOrEmpty(row?.invocation_metadata);
  const runtime = objectOrEmpty(metadata.runtime);
  return {
    seq: numberValue(row?.seq, 0),
    trace_id: stringValue(row?.trace_id),
    recorded_at_unix_ms: rowTimeMs(row),
    from: row?.from ?? null,
    to: row?.to ?? null,
    metadata: {
      runtime: {
        run: stringValue(runtime.run),
        ee: stringValue(runtime.ee),
        worker: stringValue(runtime.worker),
        checkpoint: stringValue(runtime.checkpoint),
        interface: stringValue(runtime.interface),
        epoch: numberValue(runtime.epoch, 0),
        trace_id: stringValue(runtime.trace_id),
      },
    },
  };
}

function operatorChatEventData(payload, eventKind, row) {
  const projected = clientEventDataProjection(payload);
  delete projected.continuation;
  delete projected._movement;
  delete projected._stream;
  if (eventKind === "compaction") delete projected.text;
  if (row?.__stream) {
    return {
      ...projected,
      _event: operatorChatEventIdentity(payload, eventKind),
      _stream: {
        stream_id: row.__stream.stream_id,
        stream_seq: row.__stream.stream_seq,
        byte_start_offset: row.__stream.byte_start_offset,
        byte_end_offset: row.__stream.byte_end_offset,
        source_movement_seq: row.__stream.source_movement_seq,
        recorded_at_unix_ms: row.__stream.recorded_at_unix_ms,
      },
    };
  }
  return {
    ...projected,
    _event: operatorChatEventIdentity(payload, eventKind),
    _movement: operatorChatMovementContext(row),
  };
}

function operatorChatProjectedRowId(row, fallback) {
  if (row?.__stream) {
    return `${row.__stream.stream_id}:${row.__stream.stream_seq}`;
  }
  return ledgerArtifactId(row) || fallback;
}

async function operatorChatExpandedStreamRows(rows, payloads, runId, onHeader) {
  const expanded = [];
  for (const row of rows) {
    if (ledgerArtifactLabel(row) !== "turn.stream") {
      expanded.push(row);
      continue;
    }
    const raw = payloads.get(row)?.__raw;
    if (typeof raw !== "string" || !raw.endsWith("\n")) {
      throw new Error("sealed turn.stream payload must be complete JSONL");
    }
    const bytes = Buffer.from(raw, "utf8");
    if (bytes.byteLength > 256 * 1024 * 1024) throw new Error("sealed turn.stream payload exceeds 256 MiB");
    const records = [];
    let startOffset = 0;
    for (let index = bytes.indexOf(0x0a); index >= 0; index = bytes.indexOf(0x0a, startOffset)) {
      const rawRow = bytes.subarray(startOffset, index > startOffset && bytes[index - 1] === 0x0d ? index - 1 : index);
      if (rawRow.byteLength === 0) throw new Error("sealed turn.stream contains an empty row");
      let value;
      try {
        value = JSON.parse(rawRow.toString("utf8"));
      } catch (error) {
        throw new Error(`sealed turn.stream contains malformed JSON: ${error.message}`);
      }
      if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("sealed turn.stream row must be an object");
      records.push({ value, startOffset, endOffset: index + 1 });
      startOffset = index + 1;
      if (records.length % 128 === 0) await yieldToClient();
    }
    const headerRecord = records.shift();
    const header = headerRecord?.value;
    const providerInterface = movementBoundaryInterface(row, "from");
    if (
      header?.type !== "header"
      || header.schema_version !== 1
      || header.seq !== 0
      || stringValue(header.provider_interface) !== providerInterface
      || stringValue(header.run) !== runId
    ) {
      throw new Error("sealed turn.stream header has the wrong provider/run identity");
    }
    const threadId = stringValue(header.thread_id);
    const turnId = stringValue(header.turn_id);
    const openedSourceMovementSeq = header.opened_source_movement_seq;
    const openedAtUnixMs = header.opened_at_unix_ms;
    if (!threadId || !turnId || !Number.isSafeInteger(openedSourceMovementSeq) || openedSourceMovementSeq < 0) {
      throw new Error("sealed turn.stream header identity is malformed");
    }
    if (!Number.isSafeInteger(openedAtUnixMs) || openedAtUnixMs <= 0) throw new Error("sealed turn.stream header time is malformed");
    const streamId = operatorTurnStreamIdentity([providerInterface, runId, threadId, turnId]);
    let expectedSeq = 1;
    let previousTime = openedAtUnixMs;
    let previousSourceMovementSeq = openedSourceMovementSeq;
    let sealed = false;
    for (const record of records) {
      const value = record.value;
      if (sealed) throw new Error("sealed turn.stream contains rows after seal");
      if (value.seq !== expectedSeq) throw new Error("sealed turn.stream sequence is non-contiguous");
      if (!Number.isSafeInteger(value.recorded_at_unix_ms) || value.recorded_at_unix_ms < previousTime) {
        throw new Error("sealed turn.stream time moved backwards");
      }
      if (!Number.isSafeInteger(value.source_movement_seq) || value.source_movement_seq < previousSourceMovementSeq) {
        throw new Error("sealed turn.stream source movement moved backwards");
      }
      const type = stringValue(value.type);
      if (type === "event" || type === "input_recorded") {
        if (type === "input_recorded") validateOperatorRecordedInput(value, header);
        const label = type === "input_recorded" ? "turn.stream" : stringValue(value.label);
        if (!label) throw new Error("sealed turn.stream event has no label");
        const synthetic = {
          ...row,
          seq: value.source_movement_seq,
          source_line: `${row.source_line}:stream:${value.seq}`,
          recorded_at_unix_ms: value.recorded_at_unix_ms,
          artifact: null,
          __stream: {
            stream_id: streamId,
            stream_seq: value.seq,
            byte_start_offset: record.startOffset,
            byte_end_offset: record.endOffset,
            source_movement_seq: value.source_movement_seq,
            recorded_at_unix_ms: value.recorded_at_unix_ms,
            label,
            ...(type === "input_recorded" ? { event_kind: "input_recorded" } : {}),
          },
        };
        payloads.set(synthetic, type === "input_recorded" ? value : value.payload);
        expanded.push(synthetic);
      } else if (type === "semantic_boundary") {
        if (!stringValue(value.label) || !contentHashHex(value.artifact_payload_blake3)) {
          throw new Error("sealed turn.stream semantic boundary is malformed");
        }
      } else if (type === "recovery") {
        if (!Number.isSafeInteger(value.discarded_byte_count) || value.discarded_byte_count <= 0) {
          throw new Error("sealed turn.stream recovery is malformed");
        }
      } else if (type === "seal") {
        if (!stringValue(value.terminal_label) || !contentHashHex(value.terminal_artifact_payload_blake3)) {
          throw new Error("sealed turn.stream seal is malformed");
        }
        sealed = true;
      } else {
        throw new Error(`sealed turn.stream has unknown row type '${type}'`);
      }
      expectedSeq += 1;
      previousTime = value.recorded_at_unix_ms;
      previousSourceMovementSeq = value.source_movement_seq;
      if (expectedSeq % 128 === 0) await yieldToClient();
    }
    if (!sealed) throw new Error("sealed turn.stream has no seal row");
    onHeader?.(header);
  }
  return expanded;
}

function validateOperatorRecordedInput(value, header) {
  const fields = new Set([
    "type", "seq", "recorded_at_unix_ms", "source_movement_seq", "provider_interface", "run",
    "thread_id", "turn_id", "item_id", "client_id", "stage", "source_event", "native_event_at_ms",
  ]);
  if (Object.keys(value).some((field) => !fields.has(field))) {
    throw new Error("turn.stream input_recorded has unknown fields");
  }
  for (const field of ["provider_interface", "run", "thread_id", "turn_id", "item_id", "client_id"]) {
    if (typeof value[field] !== "string" || !value[field] || value[field].trim() !== value[field]
      || /[\r\n\0]/.test(value[field])) {
      throw new Error(`turn.stream input_recorded has invalid ${field}`);
    }
  }
  for (const field of ["provider_interface", "run", "thread_id", "turn_id"]) {
    if (value[field] !== header[field]) throw new Error(`turn.stream input_recorded has wrong ${field}`);
  }
  if (value.stage !== "input_recorded_in_turn"
    || !["item/started", "item/completed", "thread/resume"].includes(value.source_event)) {
    throw new Error("turn.stream input_recorded has invalid observation stage/source");
  }
  if ((value.source_event !== "thread/resume" || Object.hasOwn(value, "native_event_at_ms"))
    && (!Number.isSafeInteger(value.native_event_at_ms) || value.native_event_at_ms < 0)) {
    throw new Error("turn.stream input_recorded has invalid native event time");
  }
}

function operatorTurnStreamIdentity(parts) {
  const hash = crypto.createHash("sha256");
  for (const part of parts) {
    const bytes = Buffer.from(stringValue(part), "utf8");
    if (bytes.byteLength === 0) throw new Error("turn.stream identity part is empty");
    const length = Buffer.alloc(8);
    length.writeBigUInt64LE(BigInt(bytes.byteLength));
    hash.update(length);
    hash.update(bytes);
  }
  return hash.digest("hex");
}

function clientEventDataProjection(value) {
  const source = objectOrEmpty(value);
  const serialized = JSON.stringify(source);
  const originalBytes = Buffer.byteLength(serialized, "utf8");
  if (originalBytes <= CLIENT_EVENT_DATA_MAX_BYTES) return { ...source };
  const payloadProjection = {
    original_bytes: originalBytes,
    truncated: true,
    ...(clientEventHasUnifiedDiff(source) ? { contains_unified_diff: true } : {}),
    ...(clientEventHasProcessOutput(source) ? { contains_process_output: true } : {}),
  };
  const reasoningSummary = clientReasoningSummaryProjection(source.summary);
  const compact = {
    ...objectOrEmpty(compactClientEventValue(source, 0)),
    ...(reasoningSummary.length ? { summary: reasoningSummary } : {}),
  };
  const projected = {
    ...compact,
    _payload_projection: payloadProjection,
  };
  if (Buffer.byteLength(JSON.stringify(projected), "utf8") <= CLIENT_EVENT_DATA_MAX_BYTES) {
    return projected;
  }
  const shallow = reasoningSummary.length ? { summary: reasoningSummary } : {};
  for (const [key, item] of Object.entries(source).slice(0, CLIENT_EVENT_DATA_MAX_OBJECT_KEYS)) {
    if (key === "summary") continue;
    let projectedItem;
    if (item === null || typeof item === "boolean" || typeof item === "number") {
      projectedItem = item;
    } else if (typeof item === "string") {
      projectedItem = item.length > CLIENT_EVENT_DATA_MAX_STRING_LENGTH
        ? `${item.slice(0, CLIENT_EVENT_DATA_MAX_STRING_LENGTH)}…`
        : item;
    }
    if (projectedItem === undefined) continue;
    const candidate = {
      ...shallow,
      [key]: projectedItem,
      _payload_projection: payloadProjection,
    };
    if (Buffer.byteLength(JSON.stringify(candidate), "utf8") <= CLIENT_EVENT_DATA_MAX_BYTES) {
      shallow[key] = projectedItem;
    }
  }
  return {
    ...shallow,
    _payload_projection: payloadProjection,
  };
}

function clientEventHasUnifiedDiff(value) {
  const source = objectOrEmpty(value);
  const body = objectOrEmpty(source.body ?? source.result);
  const evidence = objectOrEmpty(body.evidence);
  return [
    source.diff,
    source.unified_diff,
    source.unifiedDiff,
    body.diff,
    body.unified_diff,
    body.unifiedDiff,
    evidence.unified_diff,
    evidence.unifiedDiff,
  ].some((candidate) => typeof candidate === "string" && candidate.length > 0);
}

function clientEventHasProcessOutput(value) {
  const source = objectOrEmpty(value);
  const body = objectOrEmpty(source.body ?? source.result ?? objectOrEmpty(source.error).details);
  const output = objectOrEmpty(body.output ?? source.output);
  return [
    output.aggregated_output,
    output.aggregatedOutput,
    output.stdout,
    output.stderr,
    output.chunks,
  ].some((candidate) => (
    typeof candidate === "string" && candidate.length > 0
    || Array.isArray(candidate) && candidate.length > 0
    || candidate && typeof candidate === "object" && Object.keys(candidate).length > 0
  ));
}

function clientReasoningSummaryProjection(value) {
  const summaries = arrayValue(value);
  const projected = [];
  let retainedBytes = 0;
  for (const item of summaries) {
    const sourceText = typeof item === "string" ? item : stringValue(item?.text);
    if (!sourceText) continue;
    let retained = "";
    let truncated = false;
    for (const character of sourceText) {
      const characterBytes = Buffer.byteLength(character, "utf8");
      if (retainedBytes + characterBytes > CLIENT_REASONING_SUMMARY_MAX_BYTES) {
        truncated = true;
        break;
      }
      retained += character;
      retainedBytes += characterBytes;
    }
    if (retained) projected.push(truncated ? `${retained}…` : retained);
    if (truncated || retainedBytes >= CLIENT_REASONING_SUMMARY_MAX_BYTES) break;
  }
  return projected;
}

function compactClientEventValue(value, depth) {
  if (value === null || typeof value === "boolean" || typeof value === "number") return value;
  if (typeof value === "string") {
    return value.length > CLIENT_EVENT_DATA_MAX_STRING_LENGTH
      ? `${value.slice(0, CLIENT_EVENT_DATA_MAX_STRING_LENGTH)}…`
      : value;
  }
  if (depth >= CLIENT_EVENT_DATA_MAX_DEPTH) {
    if (Array.isArray(value)) return [];
    if (value && typeof value === "object") return {};
    return null;
  }
  if (Array.isArray(value)) {
    return value
      .slice(0, CLIENT_EVENT_DATA_MAX_ARRAY_ITEMS)
      .map((item) => compactClientEventValue(item, depth + 1));
  }
  if (value && typeof value === "object") {
    return Object.fromEntries(
      Object.entries(value)
        .slice(0, CLIENT_EVENT_DATA_MAX_OBJECT_KEYS)
        .map(([key, item]) => [key, compactClientEventValue(item, depth + 1)]),
    );
  }
  return null;
}

function operatorChatAssistantItemCompleted(message) {
  const data = objectOrEmpty(message.event_data);
  return ["message", "commentary"].includes(message.event_kind)
    && (message.terminal === true || stringValue(data.lifecycle ?? data._event?.lifecycle) === "completed");
}

function operatorChatStreamPositionCovered(folded, next) {
  const covered = objectOrEmpty(objectOrEmpty(folded)._stream);
  const position = objectOrEmpty(objectOrEmpty(next)._stream);
  return Boolean(position.stream_id)
    && position.stream_id === covered.stream_id
    && Number.isSafeInteger(position.stream_seq)
    && position.stream_seq <= covered.stream_seq;
}

function appendOperatorChatAssistantMessage(messages, items, message) {
  const data = objectOrEmpty(message.event_data), event = objectOrEmpty(data._event);
  const delta = ["message_delta", "commentary_delta"].includes(message.event_kind);
  const completed = operatorChatAssistantItemCompleted(message);
  const turnId = stringValue(data.turn_id ?? data.turnId ?? event.turn_id);
  const itemId = stringValue(data.item_id ?? data.itemId ?? event.item_id);
  const key = (delta || completed) && turnId && itemId
    ? JSON.stringify([message.run_id, message.interface_id, stringValue(data._activity_origin?.interface_id), turnId, itemId]) : "";
  const previous = key ? items.get(key) : null;
  if (!previous) {
    if (message.event_kind === "message" && !message.text) return;
    if (key) items.set(key, message);
    messages.push(message);
    return;
  }
  if (delta && operatorChatAssistantItemCompleted(previous)) return;
  if (delta && operatorChatStreamPositionCovered(previous.event_data, data)) return;
  const text = delta ? previous.text + message.text : message.terminal && !message.text ? previous.text : message.text;
  const eventData = delta ? {
    ...objectOrEmpty(previous.event_data), ...data,
    _event: { ...objectOrEmpty(previous.event_data?._event), ...event },
  } : data;
  Object.assign(previous, message, { id: previous.id, created_at_ms: previous.created_at_ms, text, event_data: eventData });
}

function operatorChatActivityPayload(value, label, protocol) {
  const payload = objectOrEmpty(value);
  const tool = stringValue(payload.tool);
  const correlationId = stringValue(payload.call_id ?? payload.correlation_id);
  const toolCall = label === protocol.toolCallLabel && tool;
  const toolResult = tool && correlationId && (
    label.endsWith(".result")
    || typeof payload.ok === "boolean"
  );
  if (toolCall || toolResult) {
    const refused = Boolean(objectOrEmpty(payload.body).refused);
    const failed = toolResult && (payload.ok === false || refused || Boolean(payload.error));
    const normalized = {
      ...payload,
      call_id: correlationId,
      status: toolCall ? "started" : failed ? "failed" : "completed",
    };
    return {
      eventKind: "dynamic_tool",
      payload: normalized,
      text: tool,
      error: failed,
    };
  }
  const eventKind = operatorChatEventKind(label, protocol, payload);
  if (eventKind === "generic_activity") {
    return {
      eventKind,
      payload,
      text: activityPayloadText(payload) || label || movementKindText(payload?.kind) || "Recorded activity",
      error: payload.ok === false || Boolean(payload.error),
    };
  }
  return {
    eventKind,
    payload,
    text: eventKind === "compaction"
      ? ""
      : eventKind.endsWith("_delta") || eventKind === "message"
        ? operatorChatEventText(payload, eventKind)
        : operatorChatEventText(payload, eventKind) || activityPayloadText(payload) || label,
    error: payload.ok === false || Boolean(payload.error),
  };
}

function operatorChatEventKind(label, protocol, payload) {
  if (label === protocol.terminalOutputLabel) return "message";
  if (label === protocol.errorOutputLabel) return "error";
  if (label === protocol.toolCallLabel) return "dynamic_tool";
  const phase = stringValue(payload?.phase);
  if (label === "agent.message.delta") {
    return phase === "commentary" ? "commentary_delta" : "message_delta";
  }
  if (label === "turn.agent_message") {
    return phase === "commentary" ? "commentary" : ["partial_answer", "final_answer"].includes(phase) ? "message" : "event";
  }
  return {
    "turn.reasoning": "reasoning",
    "turn.reasoning.delta": "reasoning_delta",
    "turn.plan.update": "plan",
    "turn.plan.delta": "plan_delta",
    "turn.token_usage": "usage",
    "turn.exec.command": "tool",
    "turn.exec.output_delta": "tool_output_delta",
    "turn.exec.terminal_interaction": "terminal_interaction",
    "turn.file.change": "file_change",
    "turn.file.change_delta": "file_change_delta",
    "turn.diff.updated": "diff",
    "turn.web_search": "web_search",
    "turn.hook.prompt": "hook_prompt",
    "turn.mcp.tool_call": "mcp_tool",
    "turn.mcp.progress": "mcp_progress",
    "turn.mcp.status": "mcp_status",
    "turn.dynamic_tool_call": "dynamic_tool",
    "turn.collaboration": "collaboration",
    "turn.subagent.activity": "subagent_activity",
    "turn.image.view": "image_view",
    "turn.image.generation": "image_generation",
    "turn.sleep": "sleep",
    "turn.review.entered": "review_entered",
    "turn.review.exited": "review_exited",
    "turn.context.compaction": "compaction",
    "turn.item.started": "item_started",
    "turn.started": "turn_started",
    "turn.completed": "turn_completed",
    "turn.model.rerouted": "model_rerouted",
    "turn.model.verification": "model_verification",
    "turn.auth_recovery.started": "auth_recovery_started",
    "turn.auth_recovery.completed": "auth_recovery_completed",
    "turn.warning": "warning",
    "turn.auto_review.started": "auto_review_started",
    "turn.auto_review.completed": "auto_review_completed",
    "turn.operator_request.resolved": "operator_request_resolved",
    "turn.control.acknowledged": "turn_control",
    "turn.control.failed": "turn_control",
    "turn.interrupted.result": "interrupted_result",
    "account.rate_limits": "rate_limits",
    "hook.result": "hook",
    "goal.status": "goal",
    "operator.approval.request": "approval",
    "operator.elicitation.request": "elicitation",
    "operator.user_input.request": "user_input",
  }[label] || "generic_activity";
}

function operatorChatResponseKey(label, payload, protocol) {
  const requestId = stringValue(
    payload?.operator_approval_response?.request_id
    ?? payload?.operator_elicitation_response?.request_id
    ?? payload?.operator_user_input_response?.request_id,
  );
  if (!requestId) return "";
  if (label === protocol.approvalResponseLabel) return `approval:${requestId}`;
  if (label === protocol.elicitationResponseLabel) return `elicitation:${requestId}`;
  if (label === protocol.userInputResponseLabel) return `user_input:${requestId}`;
  return "";
}

function operatorChatRequestKey(eventKind, payload) {
  const requestId = stringValue(payload?.request_id);
  return requestId && ["approval", "elicitation", "user_input"].includes(eventKind)
    ? `${eventKind}:${requestId}`
    : "";
}

function operatorChatEventText(payload, eventKind) {
  if (eventKind === "compaction") return "";
  if (eventKind === "message_delta" || eventKind === "commentary_delta") return streamFragmentValue(payload?.text);
  if (eventKind === "commentary") return stringValue(payload?.text);
  if (eventKind === "reasoning") {
    return [...arrayValue(payload?.summary), ...arrayValue(payload?.content)]
      .map(stringValue)
      .filter(Boolean)
      .join("\n\n");
  }
  if (["reasoning_delta", "plan_delta", "tool_output_delta", "file_change_delta"].includes(eventKind)) {
    return streamFragmentValue(payload?.delta);
  }
  if (eventKind === "plan") return stringValue(payload?.text);
  if (eventKind === "tool") return stringValue(payload?.command);
  if (eventKind === "terminal_interaction") return stringValue(payload?.stdin);
  if (eventKind === "diff") return stringValue(payload?.diff);
  if (eventKind === "web_search") return stringValue(payload?.query);
  if (eventKind === "mcp_progress") return stringValue(payload?.message);
  if (eventKind === "mcp_tool") {
    const target = [stringValue(payload?.server), stringValue(payload?.tool)].filter(Boolean).join(" · ");
    return target || stringValue(payload?.error?.message);
  }
  if (eventKind === "dynamic_tool") return stringValue(payload?.tool);
  if (eventKind === "subagent_activity") return stringValue(payload?.agentPath ?? payload?.agent_path);
  if (eventKind === "image_view") return stringValue(payload?.path);
  if (eventKind === "image_generation") return stringValue(payload?.revisedPrompt ?? payload?.revised_prompt);
  if (["review_entered", "review_exited"].includes(eventKind)) return stringValue(payload?.review);
  if (eventKind === "model_rerouted") {
    return [stringValue(payload?.fromModel ?? payload?.from_model), stringValue(payload?.toModel ?? payload?.to_model)]
      .filter(Boolean)
      .join(" → ");
  }
  if (eventKind === "auth_recovery_started" || eventKind === "auth_recovery_completed") {
    return stringValue(payload?.message);
  }
  if (eventKind === "warning") return stringValue(payload?.message);
  if (eventKind === "item_started") return stringValue(payload?.item?.type);
  if (eventKind === "file_change") {
    const paths = arrayValue(payload?.changes).map((change) => stringValue(change?.path)).filter(Boolean);
    return paths.join("\n");
  }
  if (eventKind === "goal") return stringValue(payload?.objective);
  if (eventKind === "approval") return stringValue(payload?.title);
  if (eventKind === "elicitation") return stringValue(payload?.message);
  if (eventKind === "user_input") return stringValue(payload?.message) || stringValue(payload?.title);
  if (eventKind === "interrupted_result") {
    const kind = stringValue(payload?.result_kind).replaceAll("_", " ");
    const nativeStatus = stringValue(payload?.native_terminal_status);
    if (nativeStatus) return `${kind || "exact result"} retained · provider turn ${nativeStatus} · not returned to model`;
    return `${kind || "exact result"} admitted after interruption · not returned to model`;
  }
  if (eventKind === "turn_control") {
    const detail = objectOrEmpty(payload?.detail);
    return stringValue(detail.error)
      || stringValue(detail.message)
      || [stringValue(payload?.action), stringValue(payload?.status)].filter(Boolean).join(" · ");
  }
  return operatorChatTurnText(payload, "out");
}

function operatorTurnControlOutcome(payload, row) {
  const controlId = stringValue(payload?.control_id);
  const action = stringValue(payload?.action);
  const status = stringValue(payload?.status);
  if (!controlId || !action || !["acknowledged", "failed"].includes(status)) return null;
  return { controlId, action, status, payload, row };
}

function operatorTurnControlEndsTurn(outcome) {
  return outcome?.status === "acknowledged" && ["interrupt", "stop"].includes(outcome.action);
}

function operatorChatAttachments(payload) {
  return arrayValue(payload?.attachments).map((item, index) => ({
    id: stringValue(item?.payload_content_hash) || `${stringValue(item?.name) || "attachment"}:${index}`,
    name: stringValue(item?.name) || `Attachment ${index + 1}`,
    size: numberValue(item?.size, 0),
    type: stringValue(item?.mime_type) || "application/octet-stream",
    label: stringValue(item?.label) || "operator.file",
    last_modified: numberValue(item?.last_modified, 0),
  }));
}

function operatorChatRunProvider(projection, entry) {
  const record = entry.record;
  const unavailable = {
    id: "",
    boundId: "",
    model: "",
    reasoningEffort: "",
    modelCatalogRefreshable: false,
  };
  if (!record) return unavailable;
  const chatState = objectOrEmpty(record.interface_state?.[entry.chatInterfaceId]);
  const selected = entry.providerChoiceInterfaceId;
  const recordedSelection = stringValue(chatState.provider_choice_interface);
  const bound = stringValue(chatState.provider_runtime_interface);
  if (!selected || recordedSelection !== selected || !bound) return unavailable;
  const archivedFrontier = new Set(arrayValue(chatState.client_archive?.active_checkpoints));
  let boundProjected = false;
  for (const checkpoint of arrayValue(record.checkpoints)) {
    if (checkpoint.status !== "projected" && !archivedFrontier.has(checkpoint.checkpoint_id)) continue;
    const interfaces = [checkpoint.interface, ...arrayValue(checkpoint.out_interfaces)];
    if (interfaces.map(stringValue).includes(bound)) boundProjected = true;
  }
  if (!boundProjected) return unavailable;
  const descriptor = projection.descriptorIndexes?.interface?.get(bound);
  const descriptorMetadata = objectOrEmpty(objectOrEmpty(descriptor?.interface_driver_config).private_config);
  const modelCatalogRequestLabel = stringValue(
    objectOrEmpty(descriptorMetadata.model_catalog_operation).request_label,
  );
  const modelCatalogRefreshable = Boolean(modelCatalogRequestLabel) && arrayValue(record.checkpoints).some((checkpoint) => (
    checkpoint.status === "projected"
    && checkpoint.interface === entry.chatInterfaceId
    && arrayValue(checkpoint.out_interfaces).length === 1
    && checkpoint.out_interfaces[0] === bound
    && arrayValue(checkpoint.interface_artifact_names).includes(modelCatalogRequestLabel)
  ));
  return {
    id: selected,
    boundId: bound,
    model: stringValue(descriptorMetadata.model),
    reasoningEffort: stringValue(descriptorMetadata.reasoning_effort),
    serviceTier: stringValue(descriptorMetadata.service_tier),
    modelCatalogRefreshable,
  };
}

function operatorChatRuntimeProviderId(entry) {
  return stringValue(
    objectOrEmpty(entry.record?.interface_state?.[entry.chatInterfaceId])
      .provider_runtime_interface,
  );
}

function operatorChatProviderFailureActivity(entry, row, value) {
  if (!isOperatorChatProviderFailureRow(entry, row)) return null;
  const payload = objectOrEmpty(value);
  const failureClass = requiredString(payload.failure_class, "provider failure class");
  const sourceMovementSeq = indexedUnsignedInteger(payload.source_movement_seq);
  if (sourceMovementSeq === null) {
    throw new Error("provider failure source_movement_seq must be a non-negative integer");
  }
  const details = objectOrEmpty(payload.details);
  return {
    eventKind: "error",
    payload: { ...payload, failure_code: failureClass },
    text: stringValue(details.error)
      || stringValue(details.message)
      || `Provider failed before completing the turn (${failureClass}).`,
    error: true,
  };
}

function operatorChatExecutionScope(entry) {
  const provider = operatorChatRuntimeProviderId(entry);
  const state = objectOrEmpty(entry.record?.interface_state?.[provider]);
  if (state.run_authoring_binding?.run !== entry.runId) return null;
  const client = objectOrEmpty(entry.record?.interface_state?.[entry.chatInterfaceId]);
  return {
    checkpoints: new Set([
      ...arrayValue(client.cycle_checkpoint_ids),
      ...arrayValue(entry.externalInputs).map((input) => stringValue(input.checkpoint_id)),
      ...arrayValue(entry.record?.checkpoints)
        .filter((checkpoint) => checkpoint.status === "projected")
        .map((checkpoint) => stringValue(checkpoint.checkpoint_id)),
    ].filter(Boolean)),
    sourceMovementSeq: state.pending_handoff?.source_movement_seq ?? null,
  };
}

function operatorActivityMessage(row, payload, protocol) {
  const label = ledgerArtifactLabel(row), kind = operatorChatEventKind(label, protocol, payload);
  return {
    direction: "out", interface_id: movementBoundaryInterface(row, "to"), event_kind: kind,
    event_data: operatorChatEventData(payload, kind, row), text: operatorChatTurnText(payload, "out"),
    error: Boolean(payload?.error) || payload?.ok === false, terminal: label === protocol.terminalOutputLabel,
    artifact_id: ledgerArtifactId(row), movement_seq: row.seq, created_at_ms: rowTimeMs(row),
  };
}

function operatorActivityAgent(projection, eeId, workerId) {
  if (projection.clientAgentCatalogError) throw new Error(projection.clientAgentCatalogError);
  const matches = arrayValue(projection.clientAgentCatalog?.document?.agents)
    .filter((agent) => agent.project_ee_id === eeId && agent.worker_interface === workerId);
  if (matches.length > 1) throw new Error(`Activity has conflicting Agent identities: ${eeId}/${workerId}`);
  return matches[0] ?? null;
}

function operatorChatSearchMessage(row, target, protocol, payload) {
  const label = ledgerArtifactLabel(row);
  const createdAtMs = rowTimeMs(row);
  const movementSeq = numberValue(row.seq, 0);
  const artifactId = ledgerArtifactId(row);
  if (row.kind === "InterfaceArrival" && label === protocol.inputLabel) {
    const text = operatorChatTurnText(payload, "in");
    if (!text) return null;
    return {
      id: stringValue(payload?.message_id) || artifactId || `${target.sourceRunId}:in:${movementSeq}`,
      run_id: target.sourceRunId,
      role: "user",
      label,
      direction: "in",
      event_kind: "message",
      event_data: {},
      text,
      attachments: operatorChatAttachments(payload),
      error: false,
      terminal: true,
      created_at_ms: createdAtMs,
      movement_seq: movementSeq,
      artifact_id: artifactId,
      content_hash: contentHashHex(row?.artifact?.content),
      interface_id: target.chatInterfaceId,
      latency_ms: 0,
    };
  }
  if (row.kind !== "InterfaceOutbound") return null;
  const eventKind = operatorChatEventKind(label, protocol, payload);
  const text = operatorChatEventText(payload, eventKind);
  if (!text) return null;
  return {
    id: artifactId || `${target.sourceRunId}:out:${movementSeq}`,
    run_id: target.sourceRunId,
    role: "assistant",
    label,
    direction: "out",
    event_kind: eventKind,
    event_data: {},
    text,
    attachments: [],
    error: label === protocol.errorOutputLabel,
    terminal: true,
    created_at_ms: createdAtMs,
    movement_seq: movementSeq,
    artifact_id: artifactId,
    content_hash: contentHashHex(row?.artifact?.content),
    interface_id: target.chatInterfaceId,
    latency_ms: 0,
  };
}
