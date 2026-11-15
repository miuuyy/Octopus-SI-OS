module.exports = { compactOperatorChatLiveEventData, compactOperatorChatLiveEventValue, operatorChatLiveActivityPayload, operatorChatLiveEventEndsTurn, operatorChatLiveEventKind, operatorChatLiveEventText };

const { LIVE_EVENT_DATA_MAX_ARRAY_ITEMS, LIVE_EVENT_DATA_MAX_BYTES, LIVE_EVENT_DATA_MAX_DEPTH, LIVE_EVENT_DATA_MAX_OBJECT_KEYS, LIVE_EVENT_DATA_MAX_STRING_LENGTH } = require("./constants.cjs");
const { arrayValue, isObject, streamFragmentValue, stringValue } = require("./values.cjs");

function operatorChatLiveActivityPayload(label, config, value) {
  const payload = isObject(value) ? value : {};
  const tool = stringValue(payload.tool);
  const correlationId = stringValue(payload.call_id ?? payload.correlation_id);
  const toolCall = label === config.artifact_labels.tool_call && tool;
  const toolResult = tool && correlationId && (
    label.endsWith(".result")
    || typeof payload.ok === "boolean"
  );
  if (toolCall || toolResult) {
    const refused = Boolean(isObject(payload.body) && payload.body.refused);
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
  const eventKind = operatorChatLiveEventKind(label, config, payload);
  return {
    eventKind,
    payload,
    text: eventKind === "compaction"
      ? ""
      : eventKind.endsWith("_delta") || eventKind === "message" || eventKind === "commentary"
        ? operatorChatLiveEventText(payload, eventKind)
        : operatorChatLiveEventText(payload, eventKind) || label,
    error: payload.ok === false || Boolean(payload.error),
  };
}

function operatorChatLiveEventKind(label, config, payload) {
  if (label === config.artifact_labels.terminal_output) return "message";
  if (label === config.artifact_labels.error_output || label === config.artifact_labels.retry_output) return "error";
  if (label === config.artifact_labels.tool_call) return "dynamic_tool";
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
    "turn.input.acknowledged": "turn_control",
    "turn.control.failed": "turn_control",
    "turn.interrupted.result": "interrupted_result",
    "account.rate_limits": "rate_limits",
    "hook.result": "hook",
    "goal.status": "goal",
    "operator.approval.request": "approval",
    "operator.elicitation.request": "elicitation",
    "operator.user_input.request": "user_input",
  }[label] || "event";
}

function operatorChatLiveEventText(payload, eventKind) {
  if (eventKind === "compaction") return "";
  if (eventKind === "message_delta" || eventKind === "commentary_delta") return streamFragmentValue(payload?.text);
  if (["reasoning_delta", "plan_delta", "tool_output_delta", "file_change_delta"].includes(eventKind)) {
    return streamFragmentValue(payload?.delta);
  }
  if (payload?.__raw && typeof payload.__raw === "string") return payload.__raw.trim();
  if (eventKind === "commentary") return stringValue(payload?.text);
  if (eventKind === "reasoning") {
    return [...arrayValue(payload?.summary), ...arrayValue(payload?.content)]
      .map(stringValue)
      .filter(Boolean)
      .join("\n\n");
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
  if (eventKind === "item_started") return stringValue(payload?.item?.type);
  if (eventKind === "file_change") {
    return arrayValue(payload?.changes).map((change) => stringValue(change?.path)).filter(Boolean).join("\n");
  }
  if (eventKind === "warning") return stringValue(payload?.message);
  if (eventKind === "goal") return stringValue(payload?.objective);
  if (eventKind === "approval") return stringValue(payload?.title);
  if (eventKind === "elicitation" || eventKind === "user_input") {
    return stringValue(payload?.message) || stringValue(payload?.title);
  }
  if (eventKind === "interrupted_result") {
    const kind = stringValue(payload?.result_kind).replaceAll("_", " ");
    return `${kind || "exact result"} admitted after interruption · not returned to model`;
  }
  const terminal = arrayValue(payload?.terminal_replies)[0];
  return stringValue(
    payload?.reply
    ?? terminal?.reply
    ?? payload?.data?.text
    ?? payload?.text
    ?? payload?.message
    ?? payload?.error,
  );
}

function operatorChatLiveEventEndsTurn({ config, eventKind, label, payload }) {
  if (label === config.artifact_labels.terminal_output) return true;
  if (label === config.artifact_labels.error_output) return true;
  if (eventKind === "interrupted_result") return true;
  return eventKind === "turn_control"
    && label === "turn.control.acknowledged"
    && stringValue(payload?.status) === "acknowledged"
    && ["interrupt", "stop"].includes(stringValue(payload?.action));
}

function compactOperatorChatLiveEventData(payload) {
  if (!isObject(payload)) return {};
  const originalBytes = Buffer.byteLength(JSON.stringify(payload), "utf8");
  if (originalBytes <= LIVE_EVENT_DATA_MAX_BYTES) return { ...payload };
  const body = payload.body ?? payload.result ?? payload.error?.details;
  const output = body?.output ?? payload.output;
  const containsProcessOutput = isObject(output) && [
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
  const projection = {
    original_bytes: originalBytes,
    truncated: true,
    ...(containsProcessOutput ? { contains_process_output: true } : {}),
  };
  const projected = compactOperatorChatLiveEventValue(payload, 0);
  if (
    isObject(projected)
    && Buffer.byteLength(JSON.stringify(projected), "utf8") <= LIVE_EVENT_DATA_MAX_BYTES
  ) {
    return {
      ...projected,
      _payload_projection: projection,
    };
  }
  const shallow = {};
  for (const [key, item] of Object.entries(payload).slice(0, LIVE_EVENT_DATA_MAX_OBJECT_KEYS)) {
    if (item === null || typeof item === "boolean" || typeof item === "number") {
      shallow[key] = item;
    } else if (typeof item === "string") {
      shallow[key] = item.length > LIVE_EVENT_DATA_MAX_STRING_LENGTH
        ? `${item.slice(0, LIVE_EVENT_DATA_MAX_STRING_LENGTH)}…`
        : item;
    }
  }
  return {
    ...shallow,
    _payload_projection: projection,
  };
}

function compactOperatorChatLiveEventValue(value, depth) {
  if (value === null || typeof value === "boolean" || typeof value === "number") return value;
  if (typeof value === "string") {
    return value.length > LIVE_EVENT_DATA_MAX_STRING_LENGTH
      ? `${value.slice(0, LIVE_EVENT_DATA_MAX_STRING_LENGTH)}…`
      : value;
  }
  if (depth >= LIVE_EVENT_DATA_MAX_DEPTH) {
    if (Array.isArray(value)) return [];
    if (isObject(value)) return {};
    return null;
  }
  if (Array.isArray(value)) {
    return value
      .slice(0, LIVE_EVENT_DATA_MAX_ARRAY_ITEMS)
      .map((item) => compactOperatorChatLiveEventValue(item, depth + 1));
  }
  if (isObject(value)) {
    return Object.fromEntries(
      Object.entries(value)
        .slice(0, LIVE_EVENT_DATA_MAX_OBJECT_KEYS)
        .map(([key, item]) => [key, compactOperatorChatLiveEventValue(item, depth + 1)]),
    );
  }
  return null;
}
