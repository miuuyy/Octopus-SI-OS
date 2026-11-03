import {
  normalizeChatRunEventProjection,
  type ChatRun,
  type ChatRunEventKind,
  type ChatRunMessage,
} from "../../chatRuns";
import type { OctopusOperatorChatArtifact } from "../../electron";
import type {
  ChatRenderMessage,
  ChatToolPresentationIndex,
} from "./types";
import { clientToolActivityPresentation } from "./toolPresentation";

type ActivityOrigin = {
  interface_id: string;
  interface_role: string;
};

type StreamIdentityMessage = {
  runId?: string;
  interfaceId?: string;
  eventKind?: ChatRenderMessage["eventKind"];
  eventData?: Record<string, unknown>;
};

export function toRender(message: ChatRunMessage): ChatRenderMessage {
  return {
    id: message.id,
    runId: message.runId,
    role: message.role,
    text: message.text,
    createdAtMs: message.createdAtMs,
    label: message.label,
    direction: message.direction,
    artifactId: message.artifactId,
    contentHash: message.contentHash,
    interfaceId: message.interfaceId,
    eventKind: message.eventKind,
    eventData: message.eventData,
    latencyMs: message.latencyMs,
    attachments: message.attachments,
    error: message.error,
    terminal: message.terminal,
  };
}

function projectedMessageIdentity(message: ChatRenderMessage): string {
  return JSON.stringify([message.runId ?? "", message.interfaceId ?? "", message.id]);
}

function liveEventIdentity(eventData: Record<string, unknown>, eventKind: ChatRunEventKind) {
  const item = eventRecord(eventData.item);
  const turn = eventRecord(eventData.turn);
  const turnId = eventString(eventData.turn_id ?? eventData.turnId ?? turn.id);
  const itemId = eventString(eventData.item_id ?? eventData.itemId ?? item.id ?? eventData.id);
  const lifecycle = eventString(eventData.lifecycle)
    || (eventKind === "item_started" || eventKind === "turn_started" ? "started" : "")
    || (eventKind === "turn_completed" ? "completed" : "")
    || (eventKind.endsWith("_delta") || eventKind.endsWith("_progress") ? "progress" : "");
  const startedAtMs = eventNumber(eventData.started_at_ms ?? eventData.startedAtMs ?? turn.started_at ?? turn.startedAt);
  const completedAtMs = eventNumber(eventData.completed_at_ms ?? eventData.completedAtMs ?? turn.completed_at ?? turn.completedAt);
  const durationMs = eventNumber(eventData.duration_ms ?? eventData.durationMs ?? turn.duration_ms ?? turn.durationMs);
  return {
    turn_id: turnId,
    item_id: itemId,
    lifecycle,
    started_at_ms: startedAtMs || null,
    completed_at_ms: completedAtMs || null,
    duration_ms: durationMs || null,
  };
}

export function interfaceRolesFromProjection(value: unknown): Record<string, string> {
  const projection = eventRecord(value);
  const rows = Array.isArray(projection.interfaces) ? projection.interfaces : [];
  return Object.fromEntries(rows.flatMap((value) => {
    const row = eventRecord(value);
    const interfaceId = eventString(row.rule_id ?? row.id);
    return interfaceId ? [[interfaceId, eventString(row.role)]] : [];
  }));
}

export function toolPresentationsFromProjection(toolsValue: unknown): ChatToolPresentationIndex {
  const toolsProjection = eventRecord(toolsValue);
  const rows = Array.isArray(toolsProjection.tools) ? toolsProjection.tools : [];
  return Object.fromEntries(rows.flatMap((value) => {
    const row = eventRecord(value);
    const id = eventString(row.id ?? row.name);
    if (!id) return [];
    const presentation = clientToolActivityPresentation(id);
    return presentation ? [[id, presentation]] : [];
  }));
}

export function liveToolCallOriginIndex(messages: ChatRunMessage[]): Map<string, ActivityOrigin> {
  const origins = new Map<string, ActivityOrigin>();
  for (const message of messages) {
    if (message.eventKind !== "dynamic_tool" || eventString(message.eventData.status) !== "started") continue;
    const callId = eventString(message.eventData.call_id ?? message.eventData.callId ?? message.eventData.correlation_id);
    const origin = eventRecord(message.eventData._activity_origin);
    const interfaceId = eventString(origin.interface_id);
    if (!callId || !interfaceId) continue;
    origins.set(callId, {
      interface_id: interfaceId,
      interface_role: eventString(origin.interface_role),
    });
  }
  return origins;
}

export function liveArtifactMessage(
  artifact: OctopusOperatorChatArtifact,
  toolCallOrigins: Map<string, ActivityOrigin>,
  turnStartedAtMs = 0,
): ChatRunMessage {
  const normalizedEvent = normalizeChatRunEventProjection(
    artifact.event_kind,
    artifact.event_data,
    artifact.text,
    "fragment",
  );
  const eventKind = normalizedEvent.eventKind;
  const sourceInterfaceId = artifact.from_interface_id;
  const eventData = normalizedEvent.eventData;
  const existingMovement = eventRecord(eventData._movement);
  const existingMetadata = eventRecord(existingMovement.metadata);
  const existingRuntime = eventRecord(existingMetadata.runtime);
  const callId = eventString(eventData.call_id ?? eventData.callId ?? eventData.correlation_id);
  let activityOrigin: ActivityOrigin = {
    interface_id: sourceInterfaceId,
    interface_role: artifact.from_interface_role,
  };
  if (eventKind === "dynamic_tool" && callId) {
    if (eventString(eventData.status) === "started") {
      toolCallOrigins.set(callId, activityOrigin);
    } else {
      activityOrigin = toolCallOrigins.get(callId) ?? activityOrigin;
    }
  }
  if (artifact.source === "stream") {
    delete eventData._movement;
    return {
      id: `${artifact.stream_id}:${artifact.stream_seq}`,
      runId: artifact.run_id,
      role: "assistant",
      label: artifact.label,
      direction: "out",
      text: normalizedEvent.text,
      createdAtMs: artifact.recorded_at_unix_ms,
      movementSeq: artifact.source_movement_seq,
      artifactId: "",
      contentHash: "",
      interfaceId: artifact.interface_id,
      eventKind,
      eventData: {
        ...eventData,
        _event: liveEventIdentity(eventData, eventKind),
        _stream: {
          stream_id: artifact.stream_id,
          stream_seq: artifact.stream_seq,
          byte_start_offset: artifact.byte_start_offset,
          byte_end_offset: artifact.byte_end_offset,
          source_movement_seq: artifact.source_movement_seq,
          recorded_at_unix_ms: artifact.recorded_at_unix_ms,
        },
        _activity_origin: activityOrigin,
      },
      latencyMs: turnStartedAtMs
        ? Math.max(0, artifact.recorded_at_unix_ms - turnStartedAtMs)
        : 0,
      attachments: [],
      error: artifact.error,
      terminal: artifact.terminal,
    };
  }
  delete eventData._stream;
  return {
    id: artifact.artifact_id || `${artifact.run_id}:out:${artifact.movement_seq}`,
    runId: artifact.run_id,
    role: "assistant",
    label: artifact.label,
    direction: "out",
    text: normalizedEvent.text,
    createdAtMs: artifact.recorded_at_unix_ms,
    movementSeq: artifact.movement_seq,
    artifactId: artifact.artifact_id,
    contentHash: artifact.content_hash,
    interfaceId: artifact.interface_id,
    eventKind,
    eventData: {
      ...eventData,
      _event: liveEventIdentity(eventData, eventKind),
      _movement: {
        seq: artifact.movement_seq,
        trace_id: eventString(existingMovement.trace_id),
        recorded_at_unix_ms: artifact.recorded_at_unix_ms,
        from: sourceInterfaceId ? { Interface: sourceInterfaceId } : null,
        to: artifact.interface_id ? { Interface: artifact.interface_id } : null,
        metadata: {
          runtime: {
            run: artifact.run_id,
            ee: eventString(existingRuntime.ee),
            worker: eventString(existingRuntime.worker),
            checkpoint: artifact.checkpoint_id,
            interface: artifact.interface_id,
            epoch: eventNumber(existingRuntime.epoch),
            trace_id: eventString(existingRuntime.trace_id),
          },
        },
      },
      _activity_origin: activityOrigin,
    },
    latencyMs: turnStartedAtMs
      ? Math.max(0, artifact.recorded_at_unix_ms - turnStartedAtMs)
      : 0,
    attachments: [],
    error: artifact.error,
    terminal: artifact.terminal,
  };
}

function artifactReferenceHash(content: unknown): string {
  return Array.isArray(content) && content.length === 32 && content.every((byte) => Number.isInteger(byte) && byte >= 0 && byte <= 255)
    ? content.map((byte: number) => byte.toString(16).padStart(2, "0")).join("") : "";
}

export function mergeChatRunMessages(
  current: ChatRunMessage[],
  incoming: ChatRunMessage[],
  mode: "incremental" | "snapshot" = "incremental",
): ChatRunMessage[] {
  const textEdits = new Map<string, Record<string, unknown>>();
  for (const message of mode === "snapshot" ? incoming : [...current, ...incoming]) {
    const edit = message.eventData?._client_text_edit;
    if (edit && message.role === "assistant" && message.eventKind === "message") {
      textEdits.set(streamIdentity(message, "assistant_message"), eventRecord(edit));
    }
  }
  const snapshotStreams = new Map<string, ChatRunMessage>();
  if (mode === "snapshot") {
    for (const message of incoming) {
      const key = chatRunMessageStreamKey(message);
      if (!key) continue;
      const previous = snapshotStreams.get(key);
      if (!previous || chatRunMessageFollows(message, previous)) snapshotStreams.set(key, message);
    }
  }
  const retained = mode === "snapshot"
    ? current.filter((message) => {
      const streamKey = chatRunMessageStreamKey(message);
      const snapshot = snapshotStreams.get(streamKey);
      return !snapshot || chatRunMessageFollows(message, snapshot);
    })
    : current;
  const rows: ChatRunMessage[] = [];
  const byId = new Map<string, number>();
  for (const message of [...retained, ...incoming]) {
    const existing = byId.get(message.id);
    if (existing !== undefined && chatRunMessageFollows(rows[existing], message)) continue;
    const index = existing ?? rows.length;
    rows[index] = message;
    byId.set(message.id, index);
  }
  const sorted = rows.sort((left, right) => (
    left.createdAtMs - right.createdAtMs
    || left.movementSeq - right.movementSeq
  ));
  for (const receipt of sorted.filter((message) => message.label === "interface.context.updated"
    && eventRecord(message.eventData?._activity_origin).interface_role === "provider"
    && message.eventData?.phase === "message_revised")) {
    const revised = eventRecord(receipt.eventData?.source_artifact);
    const original = eventRecord(receipt.eventData?.replaces_message);
    const next = sorted.find((message) => message.role === "user" && message.artifactId === revised.id && message.contentHash === artifactReferenceHash(revised.content));
    const previous = sorted.find((message) => message.role === "user" && message.artifactId === original.id && message.contentHash === artifactReferenceHash(original.content));
    if (next && previous && previous.movementSeq < next.movementSeq) {
      sorted[sorted.indexOf(next)] = { ...next, eventData: { ...next.eventData, message_revision_receipt: { ...receipt.eventData,
        from_movement_seq: eventRecord(previous.eventData?.message_revision_receipt).from_movement_seq ?? previous.movementSeq } } };
    }
  }
  const revisions = sorted.filter((message) => message.role === "user" && message.eventData?.message_revision_receipt);
  return sorted.filter((message) => !revisions.some((replacement) => {
    const start = eventRecord(replacement.eventData?.message_revision_receipt).from_movement_seq;
    return message.runId === replacement.runId && typeof start === "number"
      && message.movementSeq >= start && message.movementSeq < replacement.movementSeq;
  })).map(message => {
    const edit = textEdits.get(streamIdentity(message, "assistant_message"));
    const previousEdit = eventRecord(message.eventData?._client_text_edit);
    if (!edit && mode === "snapshot" && typeof previousEdit.original_text === "string") {
      const data = { ...message.eventData };
      delete data._client_text_edit;
      return { ...message, text: previousEdit.original_text, eventData: data };
    }
    if (!edit || message.role !== "assistant" || message.eventKind !== "message"
      || typeof edit.text !== "string" || typeof edit.original_text !== "string") return message;
    if (message.text !== edit.original_text && message.text !== edit.text) return message;
    return { ...message, text: edit.text, eventData: { ...message.eventData, _client_text_edit: edit } };
  });
}

function chatRunMessageStreamKey(message: ChatRunMessage): string {
  const family = streamFamily(message.eventKind);
  if (!family) return "";
  const identity = streamIdentity(message, family);
  if (!identity) return "";
  const discriminator = streamDiscriminator(toRender(message));
  const streamId = eventString(eventRecord(message.eventData?._stream).stream_id);
  return JSON.stringify([message.runId, message.interfaceId, streamId, identity, discriminator]);
}

function chatRunMessageFollows(left: ChatRunMessage, right: ChatRunMessage): boolean {
  const leftStream = eventRecord(left.eventData?._stream);
  const rightStream = eventRecord(right.eventData?._stream);
  if (leftStream.stream_id && rightStream.stream_id) {
    if (leftStream.stream_id !== rightStream.stream_id) return false;
    return eventNumber(leftStream.stream_seq) > eventNumber(rightStream.stream_seq)
      || eventNumber(leftStream.stream_seq) === eventNumber(rightStream.stream_seq)
        && eventNumber(leftStream.byte_end_offset) > eventNumber(rightStream.byte_end_offset);
  }
  return left.movementSeq > right.movementSeq;
}

export function mergeProjectedChatRuns(current: ChatRun[], incoming: ChatRun[]): ChatRun[] {
  const currentByRun = new Map(current.map((run) => [run.runId, run]));
  return incoming.map((run) => {
    const live = currentByRun.get(run.runId);
    if (!live || live.lastMovementSeq <= run.lastMovementSeq) return run;
    return {
      ...run,
      turnOpen: live.turnOpen,
      turnState: live.turnState,
      updatedAtMs: Math.max(run.updatedAtMs, live.updatedAtMs),
      lastMovementSeq: live.lastMovementSeq,
    };
  });
}

export function mergeRenderedMessages(
  projected: ChatRenderMessage[],
  pending: ChatRenderMessage[],
  steering: ChatRenderMessage[],
  showPending: boolean,
): ChatRenderMessage[] {
  const projectedIdentity = new Set(projected.map(projectedMessageIdentity));
  const projectedTerminalErrors = projected.filter((message) => (
    message.error === true && message.terminal === true
  ));
  const pendingWithoutProjectedTerminalErrors = pending.filter((message) => {
    if (message.eventData?._observation_error === true) {
      return !projected.some((next) => next.runId === message.runId
        && eventNumber(eventRecord(next.eventData?._movement).seq) > eventNumber(message.eventData?.after_seq));
    }
    if (message.eventData?._synthetic_turn_error !== true) return true;
    const afterSeq = eventNumber(message.eventData.after_seq);
    const submittedAtMs = eventNumber(message.eventData.submitted_at_ms);
    return !projectedTerminalErrors.some((terminal) => {
      const movementSeq = eventNumber(eventRecord(terminal.eventData?._movement).seq);
      return movementSeq > afterSeq || (
        movementSeq === 0
        && terminal.createdAtMs >= submittedAtMs
      );
    });
  });
  const primary = showPending
    ? [
      ...projected,
      ...pendingWithoutProjectedTerminalErrors.filter((bubble) => (
        !projectedIdentity.has(projectedMessageIdentity(bubble))
      )),
    ]
    : projected;
  const steeringReceipts = new Map<string, { data: Record<string, unknown>; sequence: number }>();
  const recordedInputs = new Map<string, Record<string, unknown>>();
  for (const message of projected) {
    const data = eventRecord(message.eventData);
    if (message.eventKind === "input_recorded") {
      const stream = eventRecord(data._stream);
      const origin = eventRecord(data._activity_origin);
      if (message.direction !== "out" || !message.runId || data.run !== message.runId
        || data.type !== "input_recorded" || data.stage !== "input_recorded_in_turn"
        || data.provider_interface !== origin.interface_id || origin.interface_role !== "provider"
        || !["provider_interface", "run", "thread_id", "turn_id", "item_id", "client_id"].every((key) => (
          typeof data[key] === "string" && data[key] !== "" && data[key] === (data[key] as string).trim()
        )) || !/^[a-f0-9]{64}$/.test(eventString(stream.stream_id))
        || !Number.isSafeInteger(data.seq) || eventNumber(data.seq) <= 0 || data.seq !== stream.stream_seq
        || !["item/started", "item/completed", "thread/resume"].includes(eventString(data.source_event))
        || ((data.source_event !== "thread/resume" || Object.hasOwn(data, "native_event_at_ms"))
          && (!Number.isSafeInteger(data.native_event_at_ms) || eventNumber(data.native_event_at_ms) < 0))) continue;
      const key = JSON.stringify([message.runId, data.provider_interface, data.client_id]);
      if (!recordedInputs.has(key)) recordedInputs.set(key, data);
      continue;
    }
    const controlId = eventString(data.control_id);
    const sequence = eventNumber(eventRecord(data._movement).seq);
    if (message.eventKind !== "turn_control" || message.direction !== "out" || !message.runId
      || !message.artifactId || sequence <= 0 || !controlId || data.action !== "steer"
      || !["acknowledged", "failed"].includes(eventString(data.status))) continue;
    const key = JSON.stringify([message.runId, controlId]);
    if ((steeringReceipts.get(key)?.sequence ?? -1) < sequence) steeringReceipts.set(key, { data, sequence });
  }
  const merged = [
    ...primary,
    ...steering.filter((bubble) => !projectedIdentity.has(projectedMessageIdentity(bubble))),
  ].map((message) => {
    if (message.role !== "user" || message.eventKind !== "steering" || !message.runId) return message;
    if (message.eventData?.steering_status === "input_recorded") return message;
    const recorded = recordedInputs.get(JSON.stringify([
      message.runId, message.eventData?.steering_provider_interface, message.id,
    ]));
    if (recorded) return { ...message, pending: false, eventData: { ...message.eventData,
      steering_status: "input_recorded", steering_input_recorded: recorded } };
    const receipt = steeringReceipts.get(JSON.stringify([message.runId, message.id]));
    const previous = eventRecord(message.eventData?.steering_control);
    if (!receipt || receipt.sequence <= eventNumber(eventRecord(previous._movement).seq)) return message;
    return { ...message, pending: false, eventData: { ...message.eventData,
      steering_status: receipt.data.status, steering_control: receipt.data } };
  });
  const anchored = new Map<string, ChatRenderMessage[]>();
  const ordinary = merged.flatMap((message) => {
    const anchorId = eventString(message.eventData?.anchor_message_id);
    if (!anchorId) return [message];
    const anchoredMessages = anchored.get(anchorId);
    if (anchoredMessages) anchoredMessages.push(message);
    else anchored.set(anchorId, [message]);
    return [];
  }).sort((left, right) => left.createdAtMs - right.createdAtMs);
  const ordered = ordinary.flatMap((message) => [message, ...(anchored.get(message.id) ?? [])]);
  const placed = new Set(ordinary.map((message) => message.id));
  const orphaned = [...anchored.entries()]
    .filter(([anchorId]) => !placed.has(anchorId))
    .flatMap(([, messages]) => messages)
    .sort((left, right) => left.createdAtMs - right.createdAtMs);
  return coalesceRenderedStreams([...ordered, ...orphaned]);
}

function streamPosition(message: ChatRenderMessage): { streamId: string; seq: number } | null {
  const stream = eventRecord(message.eventData?._stream);
  const streamId = eventString(stream.stream_id);
  const seq = eventNumber(stream.stream_seq);
  return streamId && seq > 0 ? { streamId, seq } : null;
}

function coalesceRenderedStreams(messages: ChatRenderMessage[]): ChatRenderMessage[] {
  const result: ChatRenderMessage[] = [];
  const positions = new Map<string, number>();
  const reasoningSections = new Map<string, number>();
  const textPartsByPosition = new Map<number, string[]>();
  const coveredStreamByPosition = new Map<number, { streamId: string; seq: number }>();
  for (const message of messages) {
    const completedAssistantItem = assistantItemCompleted(message);
    const family = completedAssistantItem ? "assistant_message" : streamFamily(message.eventKind);
    const identity = family ? streamIdentity(message, family) : "";
    if (family === "reasoning" && identity && message.eventData?.section_break === true) {
      reasoningSections.set(identity, (reasoningSections.get(identity) ?? 0) + 1);
      result.push(message);
      continue;
    }
    const explicitDiscriminator = family === "reasoning" ? streamDiscriminator(message) : "";
    const discriminator = explicitDiscriminator || (
      family === "reasoning" && identity
        ? `section:${reasoningSections.get(identity) ?? 0}`
        : ""
    );
    const key = identity && discriminator ? `${identity}:${discriminator}` : identity;
    if (!key || !message.text && !completedAssistantItem) {
      result.push(message);
      continue;
    }
    const position = positions.get(key);
    const at = streamPosition(message);
    if (position === undefined) {
      const nextPosition = result.length;
      positions.set(key, nextPosition);
      textPartsByPosition.set(nextPosition, [message.text]);
      if (at) coveredStreamByPosition.set(nextPosition, at);
      result.push({ ...message, id: `stream:${key}` });
      continue;
    }
    const previous = result[position];
    if (completedAssistantItem) {
      if (assistantItemCompleted(previous) && previous.terminal === true && message.terminal !== true) continue;
      textPartsByPosition.set(position, [message.text]);
      result[position] = { ...message, id: previous.id, createdAtMs: previous.createdAtMs };
      continue;
    }
    if (assistantItemCompleted(previous)) continue;
    const covered = coveredStreamByPosition.get(position);
    if (at && covered?.streamId === at.streamId && at.seq <= covered.seq) continue;
    if (at) coveredStreamByPosition.set(position, at);
    textPartsByPosition.get(position)?.push(message.text);
    const previousEvent = previous.eventData ?? {};
    const nextEvent = message.eventData ?? {};
    const previousIdentity = previousEvent._event && typeof previousEvent._event === "object"
      ? previousEvent._event as Record<string, unknown>
      : {};
    const nextIdentity = nextEvent._event && typeof nextEvent._event === "object"
      ? nextEvent._event as Record<string, unknown>
      : {};
    result[position] = {
      ...previous,
      eventData: {
        ...previousEvent,
        ...nextEvent,
        _event: {
          ...previousIdentity,
          ...nextIdentity,
        },
      },
      artifactId: message.artifactId || previous.artifactId,
      interfaceId: message.interfaceId || previous.interfaceId,
      latencyMs: message.latencyMs ?? previous.latencyMs,
      pending: message.pending,
    };
  }
  for (const [position, parts] of textPartsByPosition) {
    if (parts.length <= 1) continue;
    result[position] = { ...result[position], text: parts.join("") };
  }
  return result;
}

function streamFamily(kind: ChatRenderMessage["eventKind"]): string {
  if (kind === "message_delta" || kind === "commentary_delta") return "assistant_message";
  if (kind === "reasoning_delta") return "reasoning";
  if (kind === "plan_delta") return "plan";
  if (kind === "tool_output_delta") return "tool_output";
  if (kind === "file_change_delta") return "file_change";
  return "";
}

function assistantItemCompleted(message: ChatRenderMessage): boolean {
  if (message.role !== "assistant" || !["message", "commentary"].includes(message.eventKind ?? "message")) return false;
  const data = eventRecord(message.eventData);
  return message.terminal === true || eventString(data.lifecycle ?? eventRecord(data._event).lifecycle) === "completed";
}

function streamIdentity(message: StreamIdentityMessage, family: string): string {
  const event = message.eventData ?? {};
  const nested = event && typeof event._event === "object" && event._event
    ? event._event as Record<string, unknown>
    : {};
  const turnId = eventString(event.turn_id ?? event.turnId ?? nested.turn_id);
  const itemId = eventString(
    event.item_id
    ?? event.itemId
    ?? event.call_id
    ?? event.callId
    ?? nested.item_id,
  );
  return turnId && itemId ? JSON.stringify([
    message.runId ?? "",
    message.interfaceId ?? "",
    eventString(eventRecord(event._activity_origin).interface_id),
    turnId,
    itemId,
    family,
  ]) : "";
}

function streamDiscriminator(message: ChatRenderMessage): string {
  const event = message.eventData ?? {};
  const summary = event.summary_index ?? event.summaryIndex;
  if (typeof summary === "number") return `summary:${summary}`;
  const content = event.content_index ?? event.contentIndex;
  if (typeof content === "number") return `content:${content}`;
  return "";
}

export function eventString(value: unknown): string {
  return typeof value === "string" ? value.trim() : "";
}

export function eventNumber(value: unknown): number {
  const number = typeof value === "number" ? value : Number(value);
  return Number.isFinite(number) ? number : 0;
}

export function eventRecord(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value)
    ? value as Record<string, unknown>
    : {};
}
