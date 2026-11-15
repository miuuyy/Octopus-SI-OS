module.exports = { operatorChatMessagesProjection };
const { applyMessageRevision } = require("./messageRevision.cjs");
const { readAssistantTextEdits } = require("./assistantTextEdits.cjs");
const { isHomeVoiceInput } = require("./operatorChatVoiceInput.cjs");

const { compareMovementOrder, movementBoundaryInterface } = require("./movementRows.cjs");
const { appendOperatorChatAssistantMessage, nativeTurnRecordedInputs, operatorChatActivityPayload, operatorChatAttachments, operatorChatEventData, operatorChatEventKind, operatorChatEventText, operatorChatExpandedStreamRows, operatorChatProjectedRowId, operatorChatProviderFailureActivity, operatorChatRequestKey, operatorChatResponseKey, operatorChatRunProvider, operatorChatRuntimeProviderId, operatorChatStreamPositionCovered, operatorChatTurnText, operatorTurnControlEndsTurn, operatorTurnControlOutcome } = require("./operatorChatEvents.cjs");
const { baseInterfaceRows } = require("./registryRows.cjs");
const { arrayValue, contentHashHex, ledgerArtifactId, ledgerArtifactLabel, numberValue, objectOrEmpty, rowTimeMs, stringValue } = require("./values.cjs");

function operatorChatMessagesProjection(store) {
  async function operatorChatMessages(projection, runId) {
    const cached = projection.operatorChatMessagesCache.get(runId);
    if (cached?.generation === store.projectionGeneration) return readAssistantTextEdits(store.octopusHome(), runId, await cached.promise);
    const promise = buildOperatorChatMessages(projection, runId);
    projection.operatorChatMessagesCache.set(runId, { generation: store.projectionGeneration, promise });
    promise.catch(() => {
      if (projection.operatorChatMessagesCache.get(runId)?.promise === promise) {
        projection.operatorChatMessagesCache.delete(runId);
      }
    });
    return readAssistantTextEdits(store.octopusHome(), runId, await promise);
  }

  async function buildOperatorChatMessages(projection, runId) {
    const protocol = objectOrEmpty(projection.operatorChatProtocol);
    const index = await store.operatorChatRunIndex(projection);
    const entry = index.find((candidate) => candidate.runId === runId);
    if (!entry) return [];
    if (!entry.writable) return operatorReadOnlyRunMessages(projection, entry, protocol);
    return buildSingleOperatorChatMessages(projection, entry, protocol);
  }

  async function buildSingleOperatorChatMessages(projection, entry, protocol) {
    if (!entry) return [];
    if (!entry.writable) return operatorReadOnlyRunMessages(projection, entry, protocol);
    const runId = entry.runId;
    const messages = [];
    const operatorResponses = new Map();
    const turnControlOutcomes = new Map();
    const interfaceRoles = new Map(baseInterfaceRows(projection).map((descriptor) => [
      stringValue(descriptor.rule_id ?? descriptor.id),
      stringValue(descriptor.role),
    ]).filter(([interfaceId]) => interfaceId));
    const toolCallOrigins = new Map();
    const voiceInputs = new Map();
    const sourceKey = artifact => {
      const id = stringValue(artifact?.id), hash = contentHashHex(artifact?.content);
      return id && hash ? `${id}:${hash}` : null;
    };
    const activityOriginFor = (row, payload, eventKind) => {
      const sourceInterfaceId = movementBoundaryInterface(row, "from");
      const callId = stringValue(payload?.call_id ?? payload?.correlation_id);
      let activityOrigin = {
        interface_id: sourceInterfaceId,
        interface_role: interfaceRoles.get(sourceInterfaceId) ?? "",
      };
      if (eventKind === "dynamic_tool" && callId) {
        if (stringValue(payload?.status) === "started") {
          toolCallOrigins.set(callId, activityOrigin);
        } else {
          activityOrigin = toolCallOrigins.get(callId) ?? activityOrigin;
        }
      }
      return activityOrigin;
    };
    const projectedEventDataFor = (payload, eventKind, row) => {
      const data = operatorChatEventData(payload, eventKind, row);
      delete data.voice;
      return {
        ...data,
        _activity_origin: activityOriginFor(row, payload, eventKind),
        ...(movementBoundaryInterface(row, "from") === operatorChatRuntimeProviderId(entry)
          && voiceInputs.has(sourceKey(payload?.in_reply_to))
          ? { voice: voiceInputs.get(sourceKey(payload.in_reply_to)) } : {}),
      };
    };
    const payloads = await store.resolveLedgerArtifactPayloads([
      ...entry.inbound,
      ...entry.outbound,
    ], false, { projection: protocol.artifactReadProjection });
    const expandedOutbound = await operatorChatExpandedStreamRows(entry.outbound, payloads, entry.runId);
    for (const row of entry.inbound) {
      if (!isHomeVoiceInput(entry, row)) continue;
      const payload = payloads.get(row), key = sourceKey(row.artifact);
      if (key && stringValue(payload?.correlation_id) && stringValue(payload?.delegation_id)) {
        voiceInputs.set(key, { correlation_id: payload.correlation_id, delegation_id: payload.delegation_id });
      }
    }
    const recordedInputs = new Map();
    const nativeRecordedInputs = new Map();
    const runtimeProvider = operatorChatRuntimeProviderId(entry);
    for (const row of expandedOutbound) {
      if (ledgerArtifactLabel(row) === "turn.completed" && movementBoundaryInterface(row, "from") === runtimeProvider) {
        for (const input of nativeTurnRecordedInputs(payloads.get(row))) {
          if (!nativeRecordedInputs.has(input.client_id)) nativeRecordedInputs.set(input.client_id, {
            ...input, movement_seq: row.seq, artifact_id: ledgerArtifactId(row),
          });
        }
      }
      if (row.__stream?.event_kind !== "input_recorded") continue;
      const receipt = payloads.get(row);
      if (receipt.provider_interface === runtimeProvider && !recordedInputs.has(receipt.client_id)) {
        recordedInputs.set(receipt.client_id, operatorChatEventData(receipt, "input_recorded", row));
      }
    }
    for (const row of entry.inbound) {
      const label = ledgerArtifactLabel(row);
      if (![protocol.approvalResponseLabel, protocol.elicitationResponseLabel, protocol.userInputResponseLabel].includes(label)) continue;
      const payload = payloads.get(row);
      const key = operatorChatResponseKey(label, payload, protocol);
      if (key) operatorResponses.set(key, payload);
    }
    for (const row of entry.outbound) {
      const label = ledgerArtifactLabel(row);
      if (!["turn.control.acknowledged", "turn.control.failed"].includes(label)) continue;
      const outcome = operatorTurnControlOutcome(payloads.get(row), row);
      if (!outcome) continue;
      const previous = turnControlOutcomes.get(outcome.controlId);
      if (!previous || numberValue(previous.row?.seq, 0) < numberValue(row.seq, 0)) {
        turnControlOutcomes.set(outcome.controlId, outcome);
      }
    }
    const ordered = [
      ...entry.inbound.map((row) => ({ direction: "in", row })),
      ...expandedOutbound.map((row) => ({ direction: "out", row })),
    ].sort((left, right) => (
      rowTimeMs(left.row) - rowTimeMs(right.row)
      || numberValue(left.row.seq, 0) - numberValue(right.row.seq, 0)
    ));
    let turnStartedAtMs = 0;
    const assistantMessages = new Map();
    let turnOpen = false;
    const incrementalMessages = new Map();
    for (const movement of ordered) {
      const row = movement.row;
      const payload = payloads.get(row);
      const createdAtMs = rowTimeMs(row);
      if (movement.direction === "in") {
        const voice = isHomeVoiceInput(entry, row);
        if (!voice && ledgerArtifactLabel(row) !== protocol.inputLabel) continue;
        const text = voice ? stringValue(payload?.input_text) : operatorChatTurnText(payload, "in");
        if (!text) continue;
        const steering = turnOpen;
        if (!turnOpen) {
          turnStartedAtMs = createdAtMs;
          assistantMessages.clear();
        }
        turnOpen = true;
        const inboundEventKind = steering ? "steering" : "message";
        const controlOutcome = steering
          ? turnControlOutcomes.get(stringValue(payload?.message_id)) ?? null
          : null;
        const recordedInput = steering ? recordedInputs.get(stringValue(payload?.message_id)) : null;
        const nativeCandidate = steering ? nativeRecordedInputs.get(stringValue(payload?.message_id)) : null;
        const nativeRecordedInput = nativeCandidate?.movement_seq > row.seq ? nativeCandidate : null;
        messages.push({
          id: stringValue(payload?.message_id) || ledgerArtifactId(row) || `${runId}:in:${row.source_line}`,
          run_id: runId,
          role: "user",
          label: ledgerArtifactLabel(row) || protocol.inputLabel,
          direction: "in",
          event_kind: inboundEventKind,
          event_data: {
            ...projectedEventDataFor(payload, inboundEventKind, row),
            ...(voice ? { voice: { correlation_id: payload.correlation_id, delegation_id: payload.delegation_id } } : {}),
            ...(steering ? {
              steering_status: recordedInput || nativeRecordedInput ? "input_recorded" : controlOutcome?.status ?? "submitted",
              steering_provider_interface: runtimeProvider,
              steering_input_recorded: recordedInput ?? null,
              steering_native_input: nativeRecordedInput ?? null,
              steering_control: controlOutcome
                ? operatorChatEventData(controlOutcome.payload, "turn_control", controlOutcome.row)
                : null,
            } : {}),
          },
          text,
          attachments: operatorChatAttachments(payload),
          created_at_ms: createdAtMs,
          movement_seq: numberValue(row.seq, 0),
          artifact_id: ledgerArtifactId(row),
          content_hash: contentHashHex(row?.artifact?.content),
          interface_id: movementBoundaryInterface(row, "from"),
          latency_ms: 0,
          terminal: true,
        });
        continue;
      }
      const label = ledgerArtifactLabel(row) || protocol.terminalOutputLabel;
      if (label === "interface.context.updated" && movementBoundaryInterface(row, "from") === runtimeProvider
          && applyMessageRevision(messages, payload)) {
        assistantMessages.clear();
        incrementalMessages.clear();
        continue;
      }
      if (arrayValue(protocol.hiddenActivityLabels).includes(label)) continue;
      const providerFailure = operatorChatProviderFailureActivity(entry, row, payload);
      const eventKind = row.__stream?.event_kind ?? providerFailure?.eventKind ?? operatorChatEventKind(label, protocol, payload);
      const activityPayload = providerFailure?.payload ?? payload;
      const text = providerFailure?.text ?? operatorChatEventText(payload, eventKind);
      const latencyMs = turnStartedAtMs ? Math.max(0, createdAtMs - turnStartedAtMs) : 0;
      const requestKey = operatorChatRequestKey(eventKind, activityPayload);
      const projectedEventData = projectedEventDataFor(activityPayload, eventKind, row);
      const eventData = requestKey
        ? { ...projectedEventData, resolved: operatorResponses.has(requestKey), response: operatorResponses.get(requestKey) ?? null }
        : projectedEventData;
      if (eventKind === "message_delta" || eventKind === "commentary_delta") {
        if (!text) continue;
        const streamed = {
          id: operatorChatProjectedRowId(row, `${runId}:stream:${row.source_line}`),
          run_id: runId,
          role: "assistant",
          label,
          direction: "out",
          event_kind: eventKind,
          event_data: eventData,
          text,
          error: false,
          terminal: false,
          created_at_ms: createdAtMs,
          movement_seq: numberValue(row.seq, 0),
          artifact_id: ledgerArtifactId(row),
          content_hash: contentHashHex(row?.artifact?.content),
          interface_id: movementBoundaryInterface(row, "to"),
          latency_ms: latencyMs,
        };
        appendOperatorChatAssistantMessage(messages, assistantMessages, streamed);
        continue;
      }
      const incrementalKey = store.operatorChatIncrementalEventKey(activityPayload, eventKind);
      if (incrementalKey) {
        const current = incrementalMessages.get(incrementalKey);
        if (current && operatorChatStreamPositionCovered(current.event_data, eventData)) continue;
        if (current) {
          if (text) current.text += text;
          current.event_data = {
            ...objectOrEmpty(current.event_data),
            ...eventData,
            _event: {
              ...objectOrEmpty(current.event_data?._event),
              ...objectOrEmpty(eventData?._event),
            },
          };
          current.movement_seq = numberValue(row.seq, current.movement_seq);
          current.artifact_id = ledgerArtifactId(row) || current.artifact_id;
          current.content_hash = contentHashHex(row?.artifact?.content) || current.content_hash;
          current.latency_ms = latencyMs;
        } else {
          const streamed = {
            id: `${runId}:${incrementalKey}`,
            run_id: runId,
            role: "assistant",
            label,
            direction: "out",
            event_kind: eventKind,
            event_data: eventData,
            text,
            error: false,
            terminal: false,
            created_at_ms: createdAtMs,
            movement_seq: numberValue(row.seq, 0),
            artifact_id: ledgerArtifactId(row),
            content_hash: contentHashHex(row?.artifact?.content),
            interface_id: movementBoundaryInterface(row, "to"),
            latency_ms: latencyMs,
          };
          incrementalMessages.set(incrementalKey, streamed);
          messages.push(streamed);
        }
        continue;
      }
      const terminalError = Boolean(providerFailure) || label === protocol.errorOutputLabel;
      const controlOutcome = eventKind === "turn_control" ? operatorTurnControlOutcome(activityPayload, row) : null;
      const isError = terminalError || label === protocol.retryOutputLabel || controlOutcome?.status === "failed";
      const terminal = label === protocol.terminalOutputLabel
        || terminalError
        || eventKind === "interrupted_result"
        || operatorTurnControlEndsTurn(controlOutcome);
      appendOperatorChatAssistantMessage(messages, assistantMessages, {
        id: operatorChatProjectedRowId(row, `${runId}:out:${row.source_line}`),
        run_id: runId,
        role: "assistant",
        label,
        direction: "out",
        event_kind: eventKind,
        event_data: eventData,
        text,
        error: isError,
        terminal,
        will_retry: label === protocol.retryOutputLabel,
        created_at_ms: createdAtMs,
        movement_seq: numberValue(row.seq, 0),
        artifact_id: ledgerArtifactId(row),
        content_hash: contentHashHex(row?.artifact?.content),
        interface_id: movementBoundaryInterface(row, "to"),
        latency_ms: latencyMs,
      });
      if (controlOutcome?.status === "acknowledged" && controlOutcome.action === "resume") {
        turnStartedAtMs = createdAtMs;
        assistantMessages.clear();
        turnOpen = true;
      }
      if (operatorTurnControlEndsTurn(controlOutcome)) turnOpen = false;
      if (terminal) turnOpen = false;
    }
    return messages;
  }

  async function operatorReadOnlyRunMessages(projection, entry, protocol) {
    const presentedArtifactVersions = new Set();
    const sourceRows = [...entry.activity].sort(compareMovementOrder).filter((row) => {
      const artifactId = ledgerArtifactId(row);
      if (!artifactId) return true;
      const artifactVersion = `${artifactId}:${contentHashHex(row?.artifact?.content)}`;
      if (presentedArtifactVersions.has(artifactVersion)) return false;
      presentedArtifactVersions.add(artifactVersion);
      return true;
    });
    const payloads = await store.resolveLedgerArtifactPayloads(sourceRows, false, { projection: protocol.artifactReadProjection });
    const ordered = (await operatorChatExpandedStreamRows(sourceRows, payloads, entry.runId)).sort(compareMovementOrder);
    const interfaceRoles = new Map(baseInterfaceRows(projection).map((descriptor) => [
      stringValue(descriptor.rule_id ?? descriptor.id),
      stringValue(descriptor.role),
    ]).filter(([interfaceId]) => interfaceId));
    const toolCallOrigins = new Map();
    const messages = [];
    let turnOpen = false;
    let turnStartedAtMs = 0;
    const assistantMessages = new Map();
    const incrementalMessages = new Map();
    for (const row of ordered) {
      const artifactId = ledgerArtifactId(row);
      const contentHash = contentHashHex(row?.artifact?.content);
      const label = ledgerArtifactLabel(row);
      const payload = payloads.get(row);
      if (label === "interface.context.updated" && movementBoundaryInterface(row, "from") === operatorChatRuntimeProviderId(entry)
          && applyMessageRevision(messages, payload)) {
        assistantMessages.clear();
        incrementalMessages.clear();
        continue;
      }
      if (arrayValue(protocol.hiddenActivityLabels).includes(label)) continue;
      const createdAtMs = rowTimeMs(row);
      const sourceInterfaceId = movementBoundaryInterface(row, "from");
      const input = label === protocol.inputLabel && sourceInterfaceId === entry.chatInterfaceId;
      if (input) {
        const text = operatorChatTurnText(payload, "in");
        if (!text) continue;
        const steering = turnOpen;
        if (!turnOpen) {
          turnStartedAtMs = createdAtMs;
          assistantMessages.clear();
        }
        turnOpen = true;
        messages.push({
          id: stringValue(payload?.message_id) || artifactId || `${entry.runId}:in:${row.source_line}`,
          run_id: entry.runId,
          role: "user",
          label,
          direction: "in",
          event_kind: steering ? "steering" : "message",
          event_data: {
            ...operatorChatEventData(payload, steering ? "steering" : "message", row),
            read_only_run: true,
            ...(steering ? { steering_provider_interface: operatorChatRuntimeProviderId(entry) } : {}),
          },
          text,
          attachments: operatorChatAttachments(payload),
          error: false,
          terminal: true,
          created_at_ms: createdAtMs,
          movement_seq: numberValue(row.seq, 0),
          artifact_id: artifactId,
          content_hash: contentHash,
          interface_id: sourceInterfaceId,
          latency_ms: 0,
        });
        continue;
      }
      const providerFailure = operatorChatProviderFailureActivity(entry, row, payload);
      const projected = row.__stream?.event_kind === "input_recorded"
        ? { eventKind: "input_recorded", payload, text: "", error: false }
        : providerFailure ?? operatorChatActivityPayload(payload, label, protocol);
      const callId = stringValue(projected.payload?.call_id ?? projected.payload?.correlation_id);
      let activityOrigin = {
        interface_id: sourceInterfaceId,
        interface_role: interfaceRoles.get(sourceInterfaceId) ?? "",
      };
      if (projected.eventKind === "dynamic_tool" && callId) {
        if (stringValue(projected.payload?.status) === "started") {
          toolCallOrigins.set(callId, activityOrigin);
        } else {
          activityOrigin = toolCallOrigins.get(callId) ?? activityOrigin;
        }
      }
      const terminalError = Boolean(providerFailure) || label === protocol.errorOutputLabel;
      const controlOutcome = projected.eventKind === "turn_control"
        ? operatorTurnControlOutcome(payload, row)
        : null;
      const terminal = label === protocol.terminalOutputLabel
        || terminalError
        || projected.eventKind === "interrupted_result"
        || operatorTurnControlEndsTurn(controlOutcome);
      const eventData = {
        ...operatorChatEventData(projected.payload, projected.eventKind, row),
        _activity_origin: activityOrigin,
        ...(terminal ? {} : { _run_activity: true }),
        read_only_run: true,
      };
      const activityOriginRole = stringValue(activityOrigin.interface_role).toLowerCase();
      if (projected.eventKind === "dynamic_tool" && activityOriginRole && activityOriginRole !== "provider") {
        continue;
      }
      const projectedText = projected.eventKind.endsWith("_delta")
        ? projected.text
        : projected.eventKind === "compaction"
          ? ""
          : projected.eventKind === "message"
            ? projected.text || operatorChatTurnText(projected.payload, "out")
            : projected.text || operatorChatTurnText(projected.payload, "out") || label;
      if (projected.eventKind === "message_delta" || projected.eventKind === "commentary_delta") {
        if (!projectedText) continue;
        const responseEventData = { ...eventData, _run_activity: false };
        const streamed = {
          id: operatorChatProjectedRowId(row, `${entry.runId}:stream:${row.source_line}`),
          run_id: entry.runId,
          role: "assistant",
          label,
          direction: "out",
          event_kind: projected.eventKind,
          event_data: responseEventData,
          text: projectedText,
          attachments: [],
          error: false,
          terminal: false,
          created_at_ms: createdAtMs,
          movement_seq: numberValue(row.seq, 0),
          artifact_id: artifactId,
          content_hash: contentHash,
          interface_id: sourceInterfaceId,
          latency_ms: turnStartedAtMs ? Math.max(0, createdAtMs - turnStartedAtMs) : 0,
        };
        appendOperatorChatAssistantMessage(messages, assistantMessages, streamed);
        continue;
      }
      const incrementalKey = store.operatorChatIncrementalEventKey(projected.payload, projected.eventKind);
      if (incrementalKey) {
        const current = incrementalMessages.get(incrementalKey);
        if (current && operatorChatStreamPositionCovered(current.event_data, eventData)) continue;
        if (current) {
          if (projectedText) current.text += projectedText;
          current.event_data = {
            ...objectOrEmpty(current.event_data),
            ...eventData,
            _event: {
              ...objectOrEmpty(current.event_data?._event),
              ...objectOrEmpty(eventData?._event),
            },
          };
          current.movement_seq = numberValue(row.seq, current.movement_seq);
          current.artifact_id = artifactId || current.artifact_id;
          current.content_hash = contentHash || current.content_hash;
          current.latency_ms = turnStartedAtMs ? Math.max(0, createdAtMs - turnStartedAtMs) : 0;
        } else {
          const streamed = {
            id: `${entry.runId}:${incrementalKey}`,
            run_id: entry.runId,
            role: "assistant",
            label,
            direction: "out",
            event_kind: projected.eventKind,
            event_data: eventData,
            text: projectedText,
            attachments: [],
            error: false,
            terminal: false,
            created_at_ms: createdAtMs,
            movement_seq: numberValue(row.seq, 0),
            artifact_id: artifactId,
            content_hash: contentHash,
            interface_id: sourceInterfaceId,
            latency_ms: turnStartedAtMs ? Math.max(0, createdAtMs - turnStartedAtMs) : 0,
          };
          incrementalMessages.set(incrementalKey, streamed);
          messages.push(streamed);
        }
        continue;
      }
      appendOperatorChatAssistantMessage(messages, assistantMessages, {
        id: operatorChatProjectedRowId(row, `${entry.runId}:activity:${row.source_line}`),
        run_id: entry.runId,
        role: "assistant",
        label,
        direction: "out",
        event_kind: projected.eventKind,
        event_data: eventData,
        text: label === protocol.terminalOutputLabel && projectedText === label ? "" : projectedText,
        attachments: [],
        error: projected.error || terminalError || label === protocol.retryOutputLabel,
        terminal,
        will_retry: label === protocol.retryOutputLabel,
        created_at_ms: createdAtMs,
        movement_seq: numberValue(row.seq, 0),
        artifact_id: artifactId,
        content_hash: contentHash,
        interface_id: sourceInterfaceId,
        latency_ms: turnStartedAtMs ? Math.max(0, createdAtMs - turnStartedAtMs) : 0,
      });
      if (controlOutcome?.status === "acknowledged" && controlOutcome.action === "resume") {
        turnStartedAtMs = createdAtMs;
        assistantMessages.clear();
        turnOpen = true;
      }
      if (terminal) turnOpen = false;
    }
    return messages;
  }

  async function operatorProviderStatusRows(projection) {
    const latestByProvider = new Map();
    for (const entry of await store.operatorChatRunIndex(projection)) {
      const provider = operatorChatRunProvider(projection, entry);
      if (!provider.id) continue;
      for (const row of entry.outbound) {
        if (ledgerArtifactLabel(row) !== "account.rate_limits") continue;
        const movementSeq = numberValue(row.seq, 0);
        const observedAtMs = rowTimeMs(row);
        const previous = latestByProvider.get(provider.id);
        if (
          previous
          && (previous.observed_at_ms > observedAtMs
            || previous.observed_at_ms === observedAtMs && previous.movement_seq >= movementSeq)
        ) continue;
        const payload = objectOrEmpty(await store.resolveLedgerArtifactPayload(row));
        const snapshot = objectOrEmpty(payload.rate_limits ?? payload.rateLimits ?? payload);
        if (Object.keys(snapshot).length === 0) continue;
        latestByProvider.set(provider.id, {
          provider_interface_id: provider.id,
          run_id: entry.runId,
          movement_seq: movementSeq,
          observed_at_ms: observedAtMs,
          rate_limits: snapshot,
        });
      }
    }
    return Array.from(latestByProvider.values()).sort((left, right) => (
      right.observed_at_ms - left.observed_at_ms
      || right.movement_seq - left.movement_seq
      || left.provider_interface_id.localeCompare(right.provider_interface_id)
    ));
  }

  return { buildOperatorChatMessages, buildSingleOperatorChatMessages, operatorChatMessages, operatorProviderStatusRows, operatorReadOnlyRunMessages };
}
