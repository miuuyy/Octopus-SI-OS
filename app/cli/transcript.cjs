"use strict";

const toolCatalog = require("../src/components/chat/toolActivityPresentationCatalog.json");
const { normalizeChatRunEventProjection } = require("../build/cli/shared.cjs");
const { nativeTurnRecordedInputs } = require("../electron/projection.cjs");

const fragments = Object.freeze({ message_delta: "text", commentary_delta: "text",
  reasoning_delta: "delta", plan_delta: "delta", tool_output_delta: "delta", file_change_delta: "delta" });
const requestKinds = new Set(["approval", "elicitation", "user_input"]);
const toolKinds = new Set(["tool", "dynamic_tool", "mcp_tool", "tool_output_delta", "terminal_interaction"]);
const lifecycleKinds = new Set(["turn_started", "turn_completed", "turn_control", "interrupted_result", "input_recorded"]);
const record = (value) => value && typeof value === "object" && !Array.isArray(value) ? value : {};
const string = (value) => typeof value === "string" ? value : "";
const array = (value) => Array.isArray(value) ? value : [];
const integer = (value) => Number.isSafeInteger(value) && value >= 0 ? value : null;
const clone = (value) => structuredClone(value);

function pendingStop(message) {
  const data = record(message.data ?? message.activity?.details);
  return message.kind === "turn_control" && ["stop", "interrupt"].includes(data.action)
    && data.status === "acknowledged" && record(data.detail).exact_result_pending === true;
}

function interruptedClosure(message) {
  const data = message.data;
  const nativeTerminal = ["completed", "failed", "interrupted"].includes(data.native_terminal_status);
  return message.kind === "interrupted_result" && ["tool_result", "interface_operation_result"].includes(data.result_kind)
    && [data.correlation_id, data.thread_id, data.turn_id, record(data.source_artifact).id].every((value) => typeof value === "string" && value.trim() === value && value.length > 0)
    && typeof data.ok === "boolean" && data.returned_to_model === false
    && (nativeTerminal ? data.turn_interrupted === (data.native_terminal_status === "interrupted")
      && data.result_disposition === "durably_admitted_after_native_terminal"
      : data.turn_interrupted === true && data.result_disposition === "durably_admitted_after_interrupt");
}

function safeText(value) {
  const input = string(value);
  let result = "";
  for (let i = 0; i < input.length; i += 1) {
    const code = input.charCodeAt(i);
    const esc = code === 0x1b;
    const next = esc ? input.charCodeAt(i + 1) : code;
    const csi = esc ? next === 0x5b : code === 0x9b;
    const osc = esc ? next === 0x5d : code === 0x9d;
    const controlString = osc || (esc ? [0x50, 0x58, 0x5e, 0x5f].includes(next) : [0x90, 0x98, 0x9e, 0x9f].includes(code));
    if (controlString) {
      i += esc ? 2 : 1;
      for (; i < input.length; i += 1) {
        const current = input.charCodeAt(i);
        if (current === 0x9c || osc && current === 7) break;
        if (current === 0x1b && input[i + 1] === "\\") { i += 1; break; }
      }
      continue;
    }
    if (csi) {
      i += esc ? 2 : 1;
      while (i < input.length && !(input.charCodeAt(i) >= 0x40 && input.charCodeAt(i) <= 0x7e)) i += 1;
      continue;
    }
    if (esc) {
      i += 1;
      while (i < input.length && input.charCodeAt(i) >= 0x20 && input.charCodeAt(i) <= 0x2f) i += 1;
      continue;
    }
    if (code === 13) { if (input[i + 1] !== "\n") result += "\n"; continue; }
    if ((code < 32 && code !== 9 && code !== 10) || code >= 0x7f && code <= 0x9f
      || code === 0x061c || code === 0x200e || code === 0x200f
      || code >= 0x202a && code <= 0x202e || code >= 0x2066 && code <= 0x2069) continue;
    result += input[i];
  }
  return result;
}

function displayData(value) {
  if (typeof value === "string") return safeText(value);
  if (Array.isArray(value)) return value.map(displayData);
  if (value && typeof value === "object") return Object.fromEntries(Object.entries(value).map(([key, item]) => [safeText(key), displayData(item)]));
  return value;
}

function identity(data) {
  const event = record(data._event), item = record(data.item), turn = record(data.turn);
  return {
    turnId: string(data.turn_id ?? data.turnId ?? event.turn_id ?? turn.id),
    threadId: string(data.thread_id ?? data.threadId ?? event.thread_id),
    itemId: string(data.item_id ?? data.itemId ?? event.item_id ?? item.id ?? data.id),
    callId: string(data.call_id ?? data.callId ?? data.correlation_id),
  };
}

function toMessage(value, composed = false) {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new TypeError("Transcript message must be an object.");
  const projected = normalizeChatRunEventProjection(value.eventKind ?? value.event_kind ?? "message",
    value.eventData ?? value.event_data, value.text, composed ? "composed" : "fragment");
  const { eventData: data, eventKind: kind, text } = projected;
  const stream = record(data._stream), movement = record(data._movement), origin = record(data._activity_origin);
  const source = value.source ?? (stream.stream_id ? "stream" : "movement");
  const runId = string(value.runId ?? value.run_id);
  const interfaceId = string(value.interfaceId ?? value.interface_id);
  const movementSeq = integer(value.movementSeq ?? value.movement_seq ?? movement.seq);
  const artifactId = string(value.artifactId ?? value.artifact_id);
  const streamId = string(value.stream_id ?? stream.stream_id);
  const streamSeq = integer(value.stream_seq ?? stream.stream_seq);
  const id = string(value.id) || (source === "stream" && streamId && streamSeq !== null
    ? `${streamId}:${streamSeq}` : artifactId ? JSON.stringify([runId, interfaceId, artifactId, movementSeq]) : "");
  if (!id) throw new TypeError("Transcript message is missing its projection identity.");
  const role = ["user", "assistant", "system", "tool"].includes(value.role) ? value.role : value.direction === "in" ? "user" : "assistant";
  return {
    id, kind, role, text, data, source, composed,
    terminal: value.terminal === true && !pendingStop({ kind, data }), error: value.error === true,
    willRetry: value.will_retry === true || data.will_retry === true,
    attachments: array(value.attachments ?? data.attachments),
    metadata: {
      runId, interfaceId, movementSeq, artifactId,
      contentHash: string(value.contentHash ?? value.content_hash),
      label: string(value.label), direction: string(value.direction) || (role === "user" ? "in" : "out"),
      source, streamId, streamSeq,
      sourceMovementSeq: integer(value.source_movement_seq ?? stream.source_movement_seq ?? data.source_movement_seq ?? record(data.additional_details).source_movement_seq),
      byteStartOffset: integer(value.byte_start_offset ?? stream.byte_start_offset),
      byteEndOffset: integer(value.byte_end_offset ?? stream.byte_end_offset),
      createdAtMs: integer(value.createdAtMs ?? value.created_at_ms ?? value.recorded_at_unix_ms) ?? 0,
      fromInterfaceId: string(value.from_interface_id ?? origin.interface_id ?? record(movement.from).interface),
      fromInterfaceRole: string(value.from_interface_role ?? origin.interface_role),
      ...identity(data),
      payloadProjection: clone(record(data._payload_projection)),
      truncated: record(data._payload_projection).truncated === true && record(data._payload_projection).resolved !== true,
      diagnostic: clone(record(data._projection_diagnostic)),
    },
  };
}

function attachmentData(attachment) {
  const value = record(attachment);
  return { ...displayData(value), id: string(value.id ?? value.artifact_id ?? value.payload_content_hash),
    name: safeText(value.name ?? value.filename), type: safeText(value.type ?? value.media_type ?? value.mime_type),
    size: integer(value.size ?? value.size_bytes),
    artifactId: string(value.artifactId ?? value.artifact_id), contentHash: string(value.contentHash ?? value.content_hash) };
}

function shellText(value) {
  if (typeof value === "string") return safeText(value);
  if (!Array.isArray(value) || !value.every((item) => typeof item === "string")) return "";
  return value.map((item) => /^[A-Za-z0-9_./:=+@%,-]+$/.test(item) ? item : `'${safeText(item).replaceAll("'", `'"'"'`)}'`).join(" ");
}

function commandData(message) {
  const data = message.data, invocation = record(data.invocation), args = record(data.arguments ?? invocation.arguments);
  const body = record(data.body ?? data.result ?? record(data.error).details);
  const rawOutput = body.output ?? data.output, output = record(rawOutput);
  const resultState = string(body.state).toLowerCase(), resultFailure = string(body.error_text);
  const sections = [], notices = [];
  const streamText = (value, preview) => {
    if (typeof value === "string") return safeText(value);
    if (typeof record(value).text === "string") return safeText(value.text);
    if (record(preview).lossy === true) notices.push("Text preview contains replacement characters; exact bytes remain in the artifact.");
    if (typeof record(preview).text === "string") return safeText(preview.text);
    if (record(value).encoding === "base64") notices.push("Binary output retained in the artifact; no text preview.");
    return "";
  };
  if (Array.isArray(rawOutput)) {
    const blocks = rawOutput.filter((block) => block?.type === "text" && typeof block.text === "string");
    if (blocks.length) sections.push({ stream: "", text: safeText(blocks.map((block) => block.text).join("\n")) });
    if (rawOutput.some((block) => block?.type !== "text")) notices.push("Non-text output retained in the artifact.");
  } else if (array(output.chunks).length) {
    const groups = [];
    for (const raw of output.chunks) {
      const chunk = record(raw), name = string(chunk.stream);
      const content = typeof chunk.content === "string" ? chunk.content : record(chunk.content).text;
      const text = typeof content === "string" ? content : streamText(chunk.content, chunk.preview);
      if (groups.at(-1)?.stream === name) groups.at(-1).text += text;
      else groups.push({ stream: name, text });
    }
    for (const group of groups) sections.push({ stream: safeText(group.stream), text: safeText(group.text) });
  } else if (Object.hasOwn(output, "stdout") || Object.hasOwn(output, "stderr")) {
    for (const name of ["stdout", "stderr"]) {
      const text = streamText(output[name], output[`${name}_preview`] ?? output[`${name}Preview`]);
      if (text) sections.push({ stream: name, text });
      if (output[`${name}_truncated`] === true || output[`${name}_omitted_bytes`] > 0) {
        const omitted = integer(output[`${name}_omitted_bytes`]);
        notices.push(`${name}: ${omitted === null ? "output truncated" : `${omitted} bytes omitted`}.`);
      }
    }
  } else {
    const text = data.aggregated_output ?? output.aggregated_output ?? output.aggregatedOutput ?? output.output
      ?? (typeof data.output === "string" ? data.output : undefined)
      ?? (typeof body.output === "string" ? body.output : undefined)
      ?? (message.kind === "tool" && typeof data.result === "string" ? data.result : undefined);
    if (typeof text === "string") sections.push({ stream: "", text: safeText(text) });
  }
  if (!sections.some((section) => section.text) && message.toolOutput !== undefined) {
    sections.length = 0;
    sections.push({ stream: safeText(data.stream), text: safeText(message.toolOutput) });
  }
  const content = array(record(data.result).content ?? data.content);
  if (!sections.some((section) => section.text)) {
    for (const block of content) {
      if (block?.type === "text" && typeof block.text === "string") sections.push({ stream: "", text: safeText(block.text) });
      else if (block?.type === "resource" && typeof record(block.resource).text === "string") {
        sections.push({ stream: safeText(block.resource.uri), text: safeText(block.resource.text) });
      }
    }
    if (!sections.length && typeof data.body === "string") sections.push({ stream: "", text: safeText(data.body) });
  }
  if (resultFailure) {
    const separator = sections.at(-1)?.text && !sections.at(-1).text.endsWith("\n") ? "\n" : "";
    sections.push({ stream: "error", text: separator + safeText(resultFailure) });
  }
  const projection = record(data._payload_projection);
  if (projection.truncated === true && projection.resolved !== true) notices.push("Command evidence is a partial preview.");
  if (body.continuation_available === true) notices.push("More output is available.");
  const status = string(output.status || resultState || (message.kind === "tool" ? data.status : "")).toLowerCase();
  const invocationStatus = string(data.status ?? data.lifecycle ?? record(data._event).lifecycle);
  const failure = resultFailure || string(output.failure) || string(record(output.failure).message) || string(record(data.error).message) || string(data.error);
  const exitCode = integer(output.exit_code ?? output.exitCode ?? data.exit_code ?? data.exitCode);
  const phase = output.timed_out === true || data.timed_out === true || ["timed_out", "timeout"].includes(status) ? "timed_out"
    : output.interrupted === true || output.cancelled === true || ["stopped", "interrupted", "cancelled", "canceled", "aborted", "terminated"].includes(status) ? "interrupted"
    : failure || message.error || data.ok === false || ["failed", "error", "declined", "rejected"].includes(status) ? "failed"
    : output.running === true ? "running"
    : output.exited === true || exitCode !== null || body.completed === true && !body.process_id
      || message.kind === "tool" && ["completed", "complete", "done", "success", "exited"].includes(status) ? "exited"
    : resultState === "completed" ? "completed"
    : resultState === "yielded" ? "running"
    : ["started", "running", "working", "pending", "queued", "streaming", "progress", "in_progress"].includes(invocationStatus) ? "running" : "unknown";
  const tool = string(data.tool ?? data.name ?? invocation.tool), baseTool = tool.replace(/__[a-f0-9]+$/i, "");
  const descriptor = toolCatalog[baseTool];
  const command = [args.command, args.argv, args.cmd, data.command, record(data.parsed_cmd ?? data.parsedCmd).cmd, args.script].map(shellText).find(Boolean) || "";
  const processEvidence = command || body.process_id || resultState || resultFailure
    || Object.keys(output).some((key) => ["running", "exited", "exit_code", "stdout", "stderr", "chunks"].includes(key));
  return {
    title: safeText(descriptor?.label || tool || (message.kind === "tool" || message.kind === "tool_output_delta" ? "Command" : message.kind.replaceAll("_", " "))),
    tool: safeText(tool), command, script: safeText(args.script),
    cwd: safeText(args.workdir ?? args.cwd ?? body.workdir ?? body.cwd),
    status: processEvidence ? phase : safeText(invocationStatus || "unknown"), invocationStatus: safeText(invocationStatus),
    exitCode, processId: safeText(body.process_id), continuationAvailable: body.continuation_available === true,
    yielded: resultState === "yielded" || output.running === true && body.completed === false && invocationStatus === "completed",
    sections, output: sections.map((section) => section.text).join(""),
    stdout: sections.filter((section) => section.stream === "stdout").map((section) => section.text).join(""),
    stderr: sections.filter((section) => section.stream === "stderr").map((section) => section.text).join(""),
    failure: safeText(failure), notices: [...new Set(notices)],
    arguments: displayData(data.arguments ?? invocation.arguments), result: displayData(data.body ?? data.result),
    resources: content.filter((block) => block?.type !== "text").map((block) => {
      const resource = record(block.resource);
      return { type: safeText(block.type), uri: safeText(block.uri ?? resource.uri),
        mimeType: safeText(block.mimeType ?? resource.mimeType), name: safeText(block.name), size: integer(block.size) };
    }),
    inputs: [...array(data._terminal_inputs).map(safeText), ...(message.terminalInputs ?? []).map(safeText)],
    durationMs: integer(output.duration_ms ?? output.durationMs ?? data.duration_ms ?? data.durationMs),
  };
}

function requestData(message) {
  const data = message.data;
  const kind = message.kind === "approval" ? "approval"
    : message.kind === "user_input" || data.method === "item/tool/requestUserInput" ? "user_input" : "elicitation";
  const schema = record(data.requested_schema);
  return {
    id: string(data.request_id), kind, status: data.resolved === true ? "resolved" : "pending",
    title: safeText(data.title || data.message || message.text),
    method: safeText(data.method), detail: displayData(data.detail),
    questions: array(schema.questions ?? data.questions).map((raw, index) => {
      const q = record(raw);
      return { id: string(q.id) || `question_${index + 1}`, header: safeText(q.header), question: safeText(q.question),
        isOther: q.isOther === true || q.is_other === true, isSecret: q.isSecret === true || q.is_secret === true,
        options: array(q.options).map((rawOption) => ({ label: safeText(record(rawOption).label),
          description: safeText(record(rawOption).description), value: record(rawOption).label })) };
    }),
    schema: displayData(schema), choices: displayData(record(record(data.detail).permission_offer).offered_choices ?? data.choices),
    response: displayData(data.response),
    payload: clone(data), runId: message.metadata.runId, interfaceId: message.metadata.interfaceId,
    artifactId: message.metadata.artifactId, callId: string(data.call_id),
    actionable: Boolean(string(data.request_id).trim() && string(data.request_id).trim() === data.request_id
      && message.metadata.runId && message.metadata.interfaceId && data.resolved !== true),
  };
}

function present(message) {
  const entry = { id: message.id, role: toolKinds.has(message.kind) ? "tool" : message.role,
    kind: message.kind, text: safeText(message.text), attachments: message.attachments.map(attachmentData),
    error: message.error, terminal: message.terminal, metadata: clone(message.metadata) };
  if (toolKinds.has(message.kind)) {
    entry.activity = commandData(message);
    entry.status = entry.activity.status;
  } else if (requestKinds.has(message.kind)) {
    entry.request = requestData(message);
    entry.status = entry.request.status;
  } else {
    entry.status = pendingStop(message) ? "stopping" : safeText(message.data.status ?? message.data.lifecycle ?? record(message.data._event).lifecycle)
      || (fragments[message.kind] ? "streaming" : message.terminal ? "completed" : "");
    if (!["message", "message_delta", "commentary", "commentary_delta"].includes(message.kind)) {
      entry.activity = { title: safeText(message.kind.replaceAll("_", " ")),
        status: entry.status, details: displayData(message.data) };
    }
  }
  return entry;
}

function renderMessageData(message) { return present(toMessage(message, true)); }

function messageOrder(left, right) {
  const a = left.metadata, b = right.metadata;
  if (a.streamId && a.streamId === b.streamId && a.streamSeq !== null && b.streamSeq !== null) return a.streamSeq - b.streamSeq;
  return a.createdAtMs - b.createdAtMs || (a.movementSeq ?? a.sourceMovementSeq ?? 0) - (b.movementSeq ?? b.sourceMovementSeq ?? 0);
}

function receiptKey(message) {
  const m = message.metadata;
  return JSON.stringify([m.runId, m.interfaceId, message.source,
    message.source === "stream" ? m.streamId : m.artifactId || message.id,
    message.source === "stream" ? m.streamSeq : m.movementSeq]);
}

function createTranscriptReducer(options = {}) {
  const { history = [] } = options;
  if (Object.keys(options).some((key) => key !== "history")) {
    throw new TypeError("createTranscriptReducer accepts { history }; register the complete submission with beginTurn(receipt).");
  }
  const messages = new Map(), seen = new Set(), streams = new Map(), groups = new Map(), sections = new Map();
  const calls = new Map(), requests = new Map(), resolutions = new Map(), observations = [], terminals = [], entryIds = new Map();
  const priorNativeTurns = new Set();
  let turn = null, previousStatus = "";

  function nativeTurnKey(message) {
    const m = message.metadata;
    return m.threadId && m.turnId ? JSON.stringify([m.threadId, m.turnId]) : null;
  }

  function belongsToPriorInput(message) {
    const key = nativeTurnKey(message), m = message.metadata;
    if (key && key === turn.recordedInputTurn) return false;
    return priorNativeTurns.has(key) || m.sourceMovementSeq !== null && m.sourceMovementSeq < turn.admittedSeq;
  }

  function failedSteer(message) {
    const m = message.metadata, data = message.data;
    return message.source === "movement" && m.label === "turn.control.failed" && message.kind === "turn_control"
      && data.action === "steer" && data.status === "failed" && string(data.control_id)
      && integer(data.source_movement_seq) !== null && data.source_movement_seq === m.sourceMovementSeq;
  }

  function failedSubmission(message) {
    return sameScope(message) && failedSteer(message) && message.data.control_id === turn.messageId
      && message.metadata.sourceMovementSeq >= turn.admittedSeq
      && message.metadata.movementSeq > message.metadata.sourceMovementSeq;
  }

  function groupKey(message) {
    const m = message.metadata, data = message.data;
    if (toolKinds.has(message.kind)) {
      const call = m.callId || m.itemId;
      if (!call) return "";
      const callScope = JSON.stringify([m.runId, m.interfaceId, call]);
      const candidates = calls.get(callScope) ?? new Map();
      if (!m.turnId && candidates.size === 1) return [...candidates.values()][0];
      const key = JSON.stringify([callScope, m.fromInterfaceId, m.turnId, "tool"]);
      if (m.turnId && candidates.has(m.turnId)) return candidates.get(m.turnId);
      candidates.set(m.turnId, key); calls.set(callScope, candidates);
      return key;
    }
    const completed = ["message", "commentary"].includes(message.kind)
      && (message.terminal || data.lifecycle === "completed" || record(data._event).lifecycle === "completed");
    const family = completed || ["message_delta", "commentary_delta"].includes(message.kind) ? "assistant_message"
      : message.kind === "reasoning_delta" ? "reasoning" : message.kind === "plan_delta" ? "plan"
      : message.kind === "file_change_delta" ? "file_change" : "";
    if (!family || !m.turnId || !m.itemId || message.role === "user") return "";
    const key = JSON.stringify([m.runId, m.interfaceId, m.fromInterfaceId, m.turnId, m.itemId, family]);
    if (family !== "reasoning") return key;
    if (data.section_break === true) { sections.set(key, (sections.get(key) ?? 0) + 1); return ""; }
    const summary = data.summary_index ?? data.summaryIndex, content = data.content_index ?? data.contentIndex;
    return `${key}:${typeof summary === "number" ? `summary:${summary}` : typeof content === "number" ? `content:${content}` : `section:${sections.get(key) ?? 0}`}`;
  }

  function requestKey(message, id, kind = "") {
    return JSON.stringify([message.metadata.runId, message.metadata.interfaceId, kind, String(id)]);
  }

  function entryEvent(entry, previous, fragment) {
    if (entry.request) return { type: "request", id: entry.id, kind: entry.request.kind,
      payload: clone(entry.request.payload), status: entry.status, entry };
    if (entry.error) return { type: "error", id: entry.id, text: entry.text || entry.activity?.failure || "Provider error", entry };
    if (entry.role === "tool") return { type: "activity", id: entry.id, ...entry.activity, entry };
    if (lifecycleKinds.has(entry.kind)) return { type: "activity", id: entry.id,
      title: safeText(entry.kind.replaceAll("_", " ")), status: entry.status, entry };
    if (previous && fragment && entry.text.startsWith(previous.text)) {
      const text = entry.text.slice(previous.text.length);
      if (text) return { type: "delta", id: entry.id, role: entry.role, text };
      return null;
    }
    return { type: "message", entry };
  }

  function ingest(message, events) {
    const key = receiptKey(message), m = message.metadata;
    const streamKey = JSON.stringify([m.runId, m.interfaceId, m.streamId]);
    if (seen.has(key) || message.source === "stream" && m.streamId && m.streamSeq !== null && m.streamSeq <= (streams.get(streamKey) ?? -1)) return;
    seen.add(key);
    if (message.source === "stream" && m.streamId && m.streamSeq !== null) streams.set(streamKey, m.streamSeq);
    observations.push(message);
    if (message.source === "movement" && (message.terminal || failedSteer(message)) && !message.willRetry) terminals.push(message);
    const group = groupKey(message), oldId = groups.get(group), previous = oldId ? messages.get(oldId) : null;
    let merged = message;
    if (previous) {
      const complete = previous.terminal || previous.data.lifecycle === "completed" || record(previous.data._event).lifecycle === "completed";
      if (complete && ["message_delta", "commentary_delta"].includes(message.kind)) return;
      const append = Boolean(fragments[message.kind]) && !message.composed;
      merged = { ...message, id: previous.id, data: { ...previous.data, ...message.data },
        metadata: { ...previous.metadata, ...m, artifactId: m.artifactId || previous.metadata.artifactId,
          contentHash: m.contentHash || previous.metadata.contentHash, createdAtMs: previous.metadata.createdAtMs,
          fromInterfaceId: previous.metadata.fromInterfaceId || m.fromInterfaceId,
          fromInterfaceRole: previous.metadata.fromInterfaceRole || m.fromInterfaceRole },
        text: append ? previous.text + message.text : message.text || previous.text,
        attachments: message.attachments.length ? message.attachments : previous.attachments,
        toolOutput: previous.toolOutput, terminalInputs: previous.terminalInputs };
      if (message.kind === "tool_output_delta") {
        merged.kind = previous.kind;
        merged.text = previous.text;
        merged.toolOutput = (previous.toolOutput ?? "") + message.text;
      } else if (message.kind === "terminal_interaction") {
        merged.kind = previous.kind;
        merged.text = previous.text;
        merged.terminalInputs = [...(previous.terminalInputs ?? []), string(message.data.stdin)];
      }
    } else if (message.kind === "tool_output_delta") merged = { ...message, toolOutput: message.text };
    if (group) groups.set(group, merged.id);
    if (requestKinds.has(merged.kind)) {
      const id = string(merged.data.request_id), kind = requestData(merged).kind;
      const resolved = resolutions.get(requestKey(merged, id, kind)) ?? resolutions.get(requestKey(merged, id));
      if (resolved) merged = { ...merged, data: { ...merged.data, resolved: true, response: resolved.response ?? merged.data.response } };
      requests.set(requestKey(merged, id, kind), merged.id);
    }
    messages.set(merged.id, merged);
    entryIds.set(key, merged.id);
    const entry = present(merged);
    const event = entryEvent(entry, previous && present(previous), Boolean(fragments[message.kind]));
    if (event) events.push(event);

    const responseField = ["operator_approval_response", "operator_elicitation_response", "operator_user_input_response"]
      .find((field) => record(message.data[field]).request_id);
    const response = responseField ? record(message.data[responseField]) : null;
    const responseKind = responseField ? { operator_approval_response: "approval", operator_elicitation_response: "elicitation",
      operator_user_input_response: "user_input" }[responseField] : "";
    const resolvedId = response?.request_id ?? (message.kind === "operator_request_resolved" ? message.data.requestId : undefined);
    if (typeof resolvedId === "string" || Number.isSafeInteger(resolvedId)) {
      resolutions.set(requestKey(message, resolvedId, responseKind), { response });
      for (const kind of responseKind ? [responseKind] : requestKinds) {
        const targetId = requests.get(requestKey(message, resolvedId, kind));
        const target = messages.get(targetId);
        if (!target) continue;
        const resolved = { ...target, data: { ...target.data, resolved: true, response: response ?? target.data.response } };
        messages.set(targetId, resolved);
        events.push(entryEvent(present(resolved)));
      }
    }
  }

  function sameScope(message) {
    const m = message.metadata;
    return turn && m.runId === turn.runId && m.interfaceId === turn.chatInterfaceId
      && m.direction === "out" && m.fromInterfaceId === turn.modelInterfaceId && m.fromInterfaceRole === "provider";
  }

  function learnCorrelation(message) {
    if (!sameScope(message) || turn.terminal) return;
    const m = message.metadata, data = message.data;
    const retainedInput = message.source === "movement" && m.label === "turn.completed"
      && message.kind === "turn_completed" && m.movementSeq > turn.admittedSeq
      && nativeTurnRecordedInputs(data).some((input) => input.client_id === turn.messageId
        && input.turn_id === m.turnId && input.thread_id === m.threadId);
    const input = retainedInput || message.kind === "input_recorded" && message.source === "stream"
      && data.type === "input_recorded" && data.stage === "input_recorded_in_turn"
      && data.client_id === turn.messageId && data.run === turn.runId && data.provider_interface === turn.modelInterfaceId
      && integer(data.source_movement_seq) === m.sourceMovementSeq
      && ["item/started", "item/completed", "thread/resume"].includes(data.source_event)
      && /^[a-f0-9]{64}$/i.test(m.streamId) && integer(data.seq) === m.streamSeq && m.streamSeq > 0
      && (data.source_event === "thread/resume" && data.native_event_at_ms === undefined || integer(data.native_event_at_ms) !== null)
      && [data.thread_id, data.turn_id, data.item_id, data.client_id].every((value) => typeof value === "string" && value.length > 0 && value.trim() === value);
    const exactSource = m.sourceMovementSeq !== null && turn.sourceMovementSeqs.includes(m.sourceMovementSeq);
    if (!(input || exactSource) || !m.turnId) return;
    if (!input && belongsToPriorInput(message)) return;
    if (turn.turnId && (turn.turnId !== m.turnId || turn.threadId && m.threadId && turn.threadId !== m.threadId)) {
      turn.correlationError = "The current submission has conflicting native turn identities.";
      return;
    }
    turn.turnId = m.turnId;
    if (m.threadId) turn.threadId = m.threadId;
    if (input) turn.recordedInputTurn = nativeTurnKey(message);
    if (input && m.sourceMovementSeq !== null && !turn.sourceMovementSeqs.includes(m.sourceMovementSeq)) turn.sourceMovementSeqs.push(m.sourceMovementSeq);
  }

  function matchesTurn(message) {
    if (!sameScope(message) || turn.correlationError || belongsToPriorInput(message)) return false;
    const m = message.metadata;
    if (m.turnId && turn.turnId && m.turnId !== turn.turnId) return false;
    if (m.threadId && turn.threadId && m.threadId !== turn.threadId) return false;
    return Boolean(m.turnId && turn.turnId === m.turnId)
      || m.sourceMovementSeq !== null && turn.sourceMovementSeqs.includes(m.sourceMovementSeq);
  }

  function admittedMovement(message) {
    if (failedSubmission(message)) return true;
    if (!sameScope(message) || turn.correlationError || belongsToPriorInput(message)) return false;
    const m = message.metadata;
    if (m.turnId && turn.turnId && m.turnId !== turn.turnId) return false;
    if (m.threadId && turn.threadId && m.threadId !== turn.threadId) return false;
    return message.source === "movement" && m.movementSeq !== null && m.movementSeq > turn.admittedSeq;
  }

  function durableOutcome(message) {
    if (failedSubmission(message)) return true;
    if (!admittedMovement(message) || !message.terminal || message.willRetry || pendingStop(message)) return false;
    if (message.kind === "interrupted_result" && !interruptedClosure(message)) return false;
    return !turn.stopPending || interruptedClosure(message) && message.metadata.turnId === turn.stopPending.turnId
      && message.metadata.threadId === turn.stopPending.threadId && message.metadata.movementSeq > turn.stopPending.movementSeq;
  }

  function statusEvent(message, correlated) {
    const m = message.metadata, data = message.data;
    const final = Boolean(correlated && durableOutcome(message));
    const failed = message.error || data.status === "failed" || record(data.turn).status === "failed" || data.native_terminal_status === "failed"
      || message.kind === "interrupted_result" && data.ok === false && data.native_terminal_status !== "completed";
    const pending = requestKinds.has(message.kind) && data.resolved !== true;
    const state = final ? failed ? "failed" : "idle" : turn?.stopPending ? "stopping" : pending ? "waiting" : failed && !message.willRetry ? "failed" : "working";
    if (correlated && !turn.terminal) {
      turn.state = state;
      if (final) {
        turn.terminal = true;
        turn.turnId ||= m.turnId;
        turn.threadId ||= m.threadId;
        turn.outcome = { entryId: entryIds.get(receiptKey(message)) || message.id, movementSeq: m.movementSeq,
          artifactId: m.artifactId, kind: message.kind, proof: "durable_movement_after_admission" };
      }
    }
    return { type: "status", state, terminal: Boolean(final), correlated,
      interrupted: final && (data.native_terminal_status === "interrupted" || record(data.turn).status === "interrupted"
        || message.kind === "interrupted_result" && data.turn_interrupted === true || message.kind === "turn_control" && ["stop", "interrupt"].includes(data.action)),
      ...(final ? { proof: "durable_movement_after_admission" } : {}),
      messageId: correlated ? turn.messageId : undefined, turnId: m.turnId || undefined,
      runId: m.runId, interfaceId: m.interfaceId, entry: present(messages.get(entryIds.get(receiptKey(message))) || message) };
  }

  function updateStatus(incoming, events) {
    for (const message of incoming) {
      if (!sameScope(message)) continue;
      const source = message.metadata.sourceMovementSeq
        ?? (message.source === "movement" ? message.metadata.movementSeq : null);
      const key = nativeTurnKey(message);
      if (key && source !== null && source < turn.admittedSeq) priorNativeTurns.add(key);
    }
    for (const message of incoming) learnCorrelation(message);
    if (turn?.terminal) return;
    for (const message of incoming) {
      if (!pendingStop(message) || !admittedMovement(message)) continue;
      const m = message.metadata;
      if (!m.turnId || !m.threadId) throw new Error("The pending Stop acknowledgement has no exact turn/thread provenance.");
      turn.turnId ||= m.turnId;
      turn.threadId ||= m.threadId;
      turn.stopPending = { turnId: m.turnId, threadId: m.threadId, movementSeq: m.movementSeq };
    }
    const terminal = turn && terminals.findLast(durableOutcome);
    const candidates = terminal ? [terminal] : incoming.filter((message) => lifecycleKinds.has(message.kind)
      || requestKinds.has(message.kind) || message.kind === "operator_request_resolved" || message.error || message.terminal);
    for (const message of candidates) {
      const correlated = Boolean(matchesTurn(message) || admittedMovement(message));
      if (turn && !correlated) continue;
      const event = statusEvent(message, Boolean(correlated));
      const key = JSON.stringify([event.state, event.terminal, event.correlated, event.messageId, event.turnId]);
      if (key !== previousStatus) { events.push(event); previousStatus = key; }
    }
  }

  function beginTurn(receipt) {
    for (const field of ["runId", "chatInterfaceId", "modelInterfaceId", "messageId"]) {
      if (typeof receipt?.[field] !== "string" || !receipt[field]) throw new TypeError(`Turn receipt requires ${field}.`);
    }
    const admittedSeq = integer(record(record(receipt.messageVerdict).admitted).seq);
    if (admittedSeq === null) throw new TypeError("Turn receipt must contain an admitted messageVerdict.seq.");
    return beginWatch({ ...receipt, afterSeq: admittedSeq });
  }

  function beginWatch(input) {
    for (const field of ["runId", "chatInterfaceId", "modelInterfaceId"]) {
      if (typeof input?.[field] !== "string" || !input[field]) throw new TypeError(`Watch requires ${field}.`);
    }
    const admittedSeq = integer(input.afterSeq);
    if (admittedSeq === null) throw new TypeError("Watch requires the recorded turn admission frontier.");
    const providerSource = integer(input.providerSourceMovementSeq);
    priorNativeTurns.clear();
    turn = { runId: input.runId, chatInterfaceId: input.chatInterfaceId, modelInterfaceId: input.modelInterfaceId,
      messageId: string(input.messageId) || undefined, admittedSeq,
      sourceMovementSeqs: [...new Set([admittedSeq, ...(providerSource === null ? [] : [providerSource])])],
      turnId: string(input.turnId), threadId: string(input.threadId), stopPending: null,
      state: "working", terminal: false, outcome: null, correlationError: "", recordedInputTurn: null };
    previousStatus = "";
    const events = [{ type: "status", state: "working", terminal: false, correlated: true, messageId: turn.messageId,
      runId: turn.runId, interfaceId: turn.chatInterfaceId, turnId: turn.turnId || undefined }];
    updateStatus(observations, events);
    return events;
  }

  function pushBatch(batch) {
    const rows = Array.isArray(batch) ? batch : batch?.artifacts;
    if (!Array.isArray(rows)) throw new TypeError("Transcript batch must contain an artifacts array.");
    const incoming = rows.map((row) => toMessage(row)).sort(messageOrder), events = [];
    const start = observations.length;
    for (const message of incoming) ingest(message, events);
    updateStatus(observations.slice(start), events);
    return events;
  }

  if (!Array.isArray(history)) throw new TypeError("Transcript history must be an array.");
  const initial = history.map((message) => toMessage(message, true)).sort(messageOrder);
  for (const message of initial) ingest(message, []);
  return { beginTurn, beginWatch, pushBatch, snapshot: () => [...messages.values()].map(present), currentTurn: () => turn && clone(turn) };
}

function normalizeHistory(messages) { return createTranscriptReducer({ history: messages }).snapshot(); }

module.exports = { normalizeHistory, createTranscriptReducer, renderMessageData, safeText, pendingStop };
