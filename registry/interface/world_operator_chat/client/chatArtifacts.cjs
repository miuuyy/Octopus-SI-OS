module.exports = { deduplicateOperatorTurnStreamArtifacts, operatorChatArtifactOrder, operatorChatMovementArtifact, operatorChatProviderFailureActivity, operatorChatStreamArtifact, readCompleteLedgerEndOffset, readCompleteLedgerRecords, readCompleteLedgerTail, readLedgerMovementAtSeq, readLedgerMovementsAtSeqs, readOperatorChatArtifacts };

const fs = require("node:fs/promises");
const path = require("node:path");
const { assertExactOperatorClientInterface, readExactInterfaceRole, readOperatorChatArtifactBatch } = require("./artifactBatches.cjs");
const { LIVE_STREAM_ARTIFACT_LABEL } = require("./constants.cjs");
const { operatorChatLiveContext } = require("./conversation.cjs");
const { mergeOperatorStreamCursors, normalizeLiveCursor, operatorStreamCursors, watchLedgerChange, withOperatorStreamCursors } = require("./liveCursors.cjs");
const { compactOperatorChatLiveEventData, operatorChatLiveActivityPayload, operatorChatLiveEventEndsTurn } = require("./liveEvents.cjs");
const { readConfig } = require("./roots.cjs");
const { assertOperatorChatLiveArtifactBatch, cancelChanges, parseOperatorTurnStream, readOpenOperatorTurnStream, requiredBuffer, selectOperatorTurnStreamEvents, streamIdentityHash } = require("./turnStreams.cjs");
const { absolutePath, artifactLabel, boundedPositiveInteger, contentHashHex, endpointInterfaceId, integerAtLeast, nonNegativeInteger, requiredFunction, requiredObject, requiredString, safeSegment, stringValue } = require("./values.cjs");

async function readLedgerMovementAtSeq(movementsPath, seq) {
  return (await readLedgerMovementsAtSeqs(movementsPath, [seq])).get(seq);
}

async function readLedgerMovementsAtSeqs(movementsPath, seqs) {
  const requested = new Set(seqs.map((seq) => nonNegativeInteger(seq, "requested movement seq")));
  if (requested.size !== seqs.length) throw new Error("requested movement sequences repeat");
  const tail = await readCompleteLedgerTail(movementsPath, 0);
  const found = new Map();
  let expectedSeq = null;
  for (let index = 0; index < tail.records.length; index += 1) {
    let movement;
    try {
      movement = JSON.parse(tail.records[index].text);
    } catch (error) {
      throw new Error(`movement ledger contains malformed complete JSON at line ${index + 1}: ${error.message}`);
    }
    const seq = nonNegativeInteger(movement?.seq, "movement seq");
    if (expectedSeq !== null && seq !== expectedSeq) {
      throw new Error(`movement ledger is non-contiguous at seq ${seq}; expected ${expectedSeq}`);
    }
    expectedSeq = seq + 1;
    if (requested.has(seq)) found.set(seq, movement);
    if (found.size === requested.size) break;
  }
  for (const seq of requested) {
    if (!found.has(seq)) throw new Error(`movement ledger has no complete row at seq ${seq}`);
  }
  return found;
}

async function readCompleteLedgerEndOffset(movementsPath) {
  let handle;
  try {
    handle = await fs.open(movementsPath, "r");
  } catch (error) {
    if (error?.code === "ENOENT") return 0;
    throw error;
  }
  try {
    const stat = await handle.stat();
    let end = stat.size;
    while (end > 0) {
      const start = Math.max(0, end - 64 * 1024);
      const bytes = Buffer.alloc(end - start);
      let filled = 0;
      while (filled < bytes.length) {
        const { bytesRead } = await handle.read(
          bytes,
          filled,
          bytes.length - filled,
          start + filled,
        );
        if (bytesRead === 0) {
          throw new Error("movement ledger changed while resolving its complete tail");
        }
        filled += bytesRead;
      }
      const lastNewline = bytes.lastIndexOf(0x0a);
      if (lastNewline >= 0) return start + lastNewline + 1;
      end = start;
    }
    return 0;
  } finally {
    await handle.close();
  }
}

async function* readCompleteLedgerRecords(movementsPath, startOffset) {
  let handle;
  try {
    handle = await fs.open(movementsPath, "r");
  } catch (error) {
    if (error?.code === "ENOENT" && startOffset === 0) return;
    throw error;
  }
  try {
    const stat = await handle.stat();
    if (stat.size < startOffset) throw new Error("movement ledger shrank below the interface cursor");
    if (stat.size === startOffset) return;
    let offset = startOffset;
    let pending = [];
    let pendingLength = 0;
    for (let position = startOffset; position < stat.size;) {
      const buffer = Buffer.allocUnsafe(Math.min(64 * 1024, stat.size - position));
      const { bytesRead } = await handle.read(buffer, 0, buffer.length, position);
      if (bytesRead === 0) throw new Error("movement ledger changed while reading its complete tail");
      position += bytesRead;
      const chunk = buffer.subarray(0, bytesRead);
      let start = 0;
      for (;;) {
        const newline = chunk.indexOf(0x0a, start);
        if (newline < 0) break;
        const fragment = chunk.subarray(start, newline);
        const bytes = pendingLength ? Buffer.concat([...pending, fragment], pendingLength + fragment.length) : fragment;
        offset += bytes.length + 1;
        const text = bytes.subarray(0, bytes.at(-1) === 0x0d ? -1 : undefined).toString("utf8");
        pending = [];
        pendingLength = 0;
        start = newline + 1;
        yield { text, nextOffset: offset };
      }
      if (start < chunk.length) {
        const remainder = chunk.subarray(start);
        pending.push(remainder);
        pendingLength += remainder.length;
      }
    }
  } finally {
    await handle.close();
  }
}

async function readCompleteLedgerTail(movementsPath, startOffset) {
  const records = [];
  let nextOffset = startOffset;
  for await (const record of readCompleteLedgerRecords(movementsPath, startOffset)) {
    nextOffset = record.nextOffset;
    if (record.text.length > 0) records.push(record);
  }
  return { lines: records.map(record => record.text), records, nextOffset };
}

async function readOperatorChatArtifacts(options, request) {
  const config = await readConfig();
  const octopusHome = absolutePath(options.octopusHome(), "octopusHome");
  const interfaceId = safeSegment(request?.interfaceId, "interfaceId");
  const runId = safeSegment(request?.runId, "runId");
  const afterSeq = integerAtLeast(request?.afterSeq, -1, "afterSeq");
  const waitMs = boundedPositiveInteger(request?.waitMs, 30_000, 60_000, "waitMs");
  const limit = boundedPositiveInteger(request?.limit, 256, 1_024, "limit");
  const resolveArtifactPayload = requiredFunction(
    options.readArtifactPayload,
    "readArtifactPayload",
  );
  await assertExactOperatorClientInterface(octopusHome, interfaceId);
  const liveContext = await operatorChatLiveContext(
    config,
    octopusHome,
    runId,
    interfaceId,
  );
  const movementsPath = path.join(octopusHome, "ledger", "movements.jsonl");
  let cursor = normalizeLiveCursor(request?.cursor) ?? { offset: 0, seq: null };
  const deadline = Date.now() + waitMs;
  const streamRunRoot = path.join(
    liveContext.streamsRoot,
    streamIdentityHash([liveContext.providerInterfaceId, runId]),
  );
  for (;;) {
    const remaining = Math.max(0, deadline - Date.now());
    const priorStreamCursors = operatorStreamCursors(cursor);
    const ledgerChange = watchLedgerChange(movementsPath, remaining);
    const streamChange = watchLedgerChange(streamRunRoot, remaining);
    let ledgerBatch;
    let openStreamBatch;
    const openStreamLimit = Math.max(1, Math.ceil(limit / 2));
    const ledgerLimit = limit - openStreamLimit;
    try {
      const reads = await Promise.allSettled([
        readOperatorChatArtifactBatch({
          afterSeq,
          cursor,
          interfaceId,
          limit: ledgerLimit,
          movementsPath,
          octopusHome,
          runId,
        }),
        readOpenOperatorTurnStream({
          cursors: priorStreamCursors,
          limit: openStreamLimit,
          liveContext,
          runId,
          runRoot: streamRunRoot,
          waitMs: remaining,
        }),
      ]);
      if (reads[1].status === "fulfilled") openStreamBatch = reads[1].value;
      const failed = reads.find((result) => result.status === "rejected");
      if (failed) throw failed.reason;
      ledgerBatch = reads[0].value;
      if (ledgerLimit === 0 && openStreamBatch.events.length === 0) {
        ledgerBatch = await readOperatorChatArtifactBatch({
          afterSeq,
          cursor,
          interfaceId,
          limit,
          movementsPath,
          octopusHome,
          runId,
        });
      }
    } catch (error) {
      ledgerChange.cancel();
      streamChange.cancel();
      cancelChanges(openStreamBatch?.changes);
      throw error;
    }
    const sealedStreamPending = ledgerBatch.rows.some((row) => (
      artifactLabel(requiredString(row?.artifact?.id, "outbound artifact.id"))
        === LIVE_STREAM_ARTIFACT_LABEL
    ));
    const openStreamEvents = sealedStreamPending ? [] : openStreamBatch.events;
    let streamCursors = sealedStreamPending
      ? priorStreamCursors
      : mergeOperatorStreamCursors(priorStreamCursors, openStreamBatch.cursors);
    let ledgerCursor = ledgerBatch.cursor;
    cursor = withOperatorStreamCursors(ledgerCursor, streamCursors);
    if (ledgerBatch.rows.length > 0 || openStreamBatch.events.length > 0) {
      ledgerChange.cancel();
      streamChange.cancel();
      cancelChanges(openStreamBatch.changes);
      const sourceRoles = new Map(await Promise.all(
        [...new Set(ledgerBatch.rows.map((row) => endpointInterfaceId(row.from)))].map(async (sourceInterfaceId) => (
          [sourceInterfaceId, await readExactInterfaceRole(octopusHome, sourceInterfaceId)]
        )),
      ));
      const movementArtifacts = [];
      const sealedStreamArtifacts = [];
      for (const row of ledgerBatch.rows) {
        const artifactId = requiredString(row?.artifact?.id, "outbound artifact.id");
        const contentHash = contentHashHex(row?.artifact?.content, "outbound artifact.content");
        const label = artifactLabel(artifactId);
        const resolved = await resolveArtifactPayload({
          artifactId,
          manifestHash: contentHash,
          label,
          ...(config.artifact_read_projection.json_object_labels.includes(label) ? {
            projection: config.artifact_read_projection,
          } : {}),
        });
        if (label === LIVE_STREAM_ARTIFACT_LABEL) {
          const sourceInterfaceId = endpointInterfaceId(row.from);
          const sourceRole = sourceRoles.get(sourceInterfaceId);
          if (sourceRole !== "provider") {
            throw new Error(`sealed turn.stream source '${sourceInterfaceId}' is not a provider interface`);
          }
          const parsed = parseOperatorTurnStream(
            requiredBuffer(resolved?.payloadBytes, "sealed turn.stream payloadBytes"),
            {
              complete: true,
              providerInterfaceId: sourceInterfaceId,
              runId,
            },
          );
          const remainingBudget = Math.max(
            0,
            limit - openStreamEvents.length - movementArtifacts.length - sealedStreamArtifacts.length,
          );
          const selected = selectOperatorTurnStreamEvents(
            parsed,
            streamCursors.get(parsed.streamId) ?? null,
            remainingBudget,
          );
          streamCursors = mergeOperatorStreamCursors(streamCursors, new Map([[parsed.streamId, selected.cursor]]));
          sealedStreamArtifacts.push(...selected.events.map((event) => operatorChatStreamArtifact({
            config,
            event,
            interfaceId,
            providerInterfaceId: sourceInterfaceId,
            providerRole: sourceRole,
            runId,
          })));
          const fullyConsumed = selected.cursor.offset === parsed.completeEndOffset
            && selected.cursor.seq === parsed.nextSeq;
          ledgerCursor = fullyConsumed
            ? row.__ledger_cursor_after
            : row.__ledger_cursor_before;
          continue;
        }
        movementArtifacts.push(operatorChatMovementArtifact({
          config,
          contentHash,
          interfaceId,
          label,
          payload: resolved?.payload,
          providerInterfaceId: liveContext.providerInterfaceId,
          role: sourceRoles.get(endpointInterfaceId(row.from)),
          row,
          runId,
        }));
        ledgerCursor = row.__ledger_cursor_after;
      }
      cursor = withOperatorStreamCursors(ledgerCursor, streamCursors);
      const streamArtifacts = deduplicateOperatorTurnStreamArtifacts([
        ...openStreamEvents.map((event) => operatorChatStreamArtifact({
          config,
          event,
          interfaceId,
          providerInterfaceId: liveContext.providerInterfaceId,
          providerRole: liveContext.providerRole,
          runId,
        })),
        ...sealedStreamArtifacts,
      ]);
      const artifacts = [...movementArtifacts, ...streamArtifacts].sort((left, right) => (
        left.recorded_at_unix_ms - right.recorded_at_unix_ms
        || operatorChatArtifactOrder(left) - operatorChatArtifactOrder(right)
        || (left.source === "stream" ? left.stream_seq : 0) - (right.source === "stream" ? right.stream_seq : 0)
      ));
      assertOperatorChatLiveArtifactBatch({ artifacts, cursor });
      return { artifacts, cursor };
    }
    cursor = withOperatorStreamCursors(ledgerBatch.cursor, streamCursors);
    const changed = await Promise.race([
      ledgerChange.promise,
      streamChange.promise,
      ...openStreamBatch.changes.map((change) => change.promise),
    ]);
    ledgerChange.cancel();
    streamChange.cancel();
    cancelChanges(openStreamBatch.changes);
    if (remaining === 0 || !changed) {
      return { artifacts: [], cursor };
    }
  }
}

function operatorChatMovementArtifact({ config, contentHash, interfaceId, label, payload, providerInterfaceId, role, row, runId }) {
  const providerFailure = operatorChatProviderFailureActivity({
    label,
    payload,
    providerInterfaceId,
    sourceInterfaceId: endpointInterfaceId(row.from),
  });
  const activity = providerFailure ?? operatorChatLiveActivityPayload(label, config, payload);
  const eventKind = activity.eventKind;
  const terminalError = label === config.artifact_labels.error_output;
  const willRetry = label === config.artifact_labels.retry_output;
  const eventData = compactOperatorChatLiveEventData(activity.payload);
  delete eventData.continuation;
  if (eventKind === "compaction") delete eventData.text;
  return {
    source: "movement",
    movement_seq: nonNegativeInteger(row.seq, "outbound movement seq"),
    recorded_at_unix_ms: nonNegativeInteger(
      row.recorded_at_unix_ms,
      "outbound movement recorded_at_unix_ms",
    ),
    run_id: runId,
    interface_id: interfaceId,
    checkpoint_id: stringValue(row.runtime?.checkpoint),
    from_interface_id: endpointInterfaceId(row.from),
    from_interface_role: requiredString(role, "outbound movement source interface role"),
    artifact_id: requiredString(row?.artifact?.id, "outbound artifact.id"),
    content_hash: contentHash,
    label,
    event_kind: eventKind,
    event_data: eventData,
    text: activity.text,
    error: Boolean(providerFailure) || terminalError || willRetry || activity.error || label === "turn.control.failed",
    terminal: Boolean(providerFailure) || operatorChatLiveEventEndsTurn({
      config,
      eventKind,
      label,
      payload,
    }),
    will_retry: willRetry,
  };
}

function operatorChatProviderFailureActivity({ label, payload, providerInterfaceId, sourceInterfaceId }) {
  if (!providerInterfaceId || sourceInterfaceId !== providerInterfaceId) return null;
  if (label !== `${providerInterfaceId}.failure`) return null;
  const failure = requiredObject(payload, "provider failure payload");
  const failureClass = requiredString(failure.failure_class, "provider failure class");
  nonNegativeInteger(failure.source_movement_seq, "provider failure source_movement_seq");
  const details = requiredObject(failure.details, "provider failure details");
  const text = stringValue(details.error)
    || stringValue(details.message)
    || `Provider failed before completing the turn (${failureClass}).`;
  return {
    eventKind: "error",
    payload: { ...failure, failure_code: failureClass },
    text,
    error: true,
  };
}

function operatorChatStreamArtifact({ config, event, interfaceId, providerInterfaceId, providerRole, runId }) {
  const inputRecorded = event.type === "input_recorded";
  const activity = inputRecorded
    ? { eventKind: "input_recorded", payload: event.payload, text: "", error: false }
    : operatorChatLiveActivityPayload(event.label, config, event.payload);
  const eventData = inputRecorded ? {
    ...activity.payload,
    _stream: {
      stream_id: event.streamId,
      stream_seq: event.seq,
      byte_start_offset: event.startOffset,
      byte_end_offset: event.endOffset,
      recorded_at_unix_ms: event.recordedAtUnixMs,
    },
  } : compactOperatorChatLiveEventData(activity.payload);
  delete eventData.continuation;
  if (activity.eventKind === "compaction") delete eventData.text;
  return {
    source: "stream",
    stream_id: event.streamId,
    stream_seq: event.seq,
    byte_start_offset: event.startOffset,
    byte_end_offset: event.endOffset,
    source_movement_seq: event.sourceMovementSeq,
    recorded_at_unix_ms: event.recordedAtUnixMs,
    run_id: runId,
    interface_id: interfaceId,
    from_interface_id: providerInterfaceId,
    from_interface_role: providerRole,
    label: inputRecorded ? LIVE_STREAM_ARTIFACT_LABEL : event.label,
    event_kind: activity.eventKind,
    event_data: eventData,
    text: activity.text,
    error: activity.error,
    terminal: false,
    will_retry: false,
  };
}

function operatorChatArtifactOrder(artifact) {
  return artifact.source === "movement" ? artifact.movement_seq : artifact.source_movement_seq;
}

function deduplicateOperatorTurnStreamArtifacts(artifacts) {
  const seen = new Set();
  return artifacts.filter((artifact) => {
    const key = `${artifact.stream_id}\0${artifact.stream_seq}`;
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  });
}
