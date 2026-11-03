module.exports = { assertOperatorChatLiveArtifactBatch, cancelChanges, laterStreamCursor, parseOperatorTurnStream, parseOperatorTurnStreamHeader, parseOperatorTurnStreamRecords, readOpenOperatorTurnStream, readOpenOperatorTurnStreamTail, readOperatorTurnStreamHeaderBytes, readOperatorTurnStreamRange, readRegularFile, requiredBuffer, selectOperatorTurnStreamEvents, streamIdentityHash, validateOperatorTurnInputRecorded, validateOperatorTurnStreamRow };

const crypto = require("node:crypto");
const fs = require("node:fs/promises");
const path = require("node:path");
const { LIVE_STREAM_ARTIFACT_LABEL, LIVE_STREAM_MAX_BYTES, LIVE_STREAM_MAX_HEADER_BYTES, LIVE_STREAM_MAX_MANIFEST_BYTES, LIVE_STREAM_SCHEMA_VERSION } = require("./constants.cjs");
const { watchLedgerChange } = require("./liveCursors.cjs");
const { absolutePath, contentHashHex, isObject, nonNegativeInteger, positiveInteger, requiredObject, requiredString, requiredTrimmedString } = require("./values.cjs");

async function readOpenOperatorTurnStream(input) {
  const runRoot = input.runRoot;
  let entries;
  try {
    entries = await fs.readdir(runRoot, { withFileTypes: true });
  } catch (error) {
    if (error?.code === "ENOENT") return { changes: [], cursors: input.cursors, events: [] };
    throw error;
  }
  const candidates = [];
  try {
    for (const entry of entries) {
      if (!entry.isDirectory()) continue;
      const turnRoot = path.join(runRoot, entry.name);
      const manifestPath = path.join(turnRoot, "turn-stream.artifact.json");
      let manifestBytes;
      try {
        manifestBytes = await readRegularFile(
          manifestPath,
          LIVE_STREAM_MAX_MANIFEST_BYTES,
          "open turn.stream manifest",
        );
      } catch (error) {
        if (error?.code === "ENOENT") continue;
        throw error;
      }
      let manifest;
      try {
        manifest = JSON.parse(manifestBytes.toString("utf8"));
      } catch (error) {
        throw new Error(`open turn.stream manifest is malformed JSON: ${error.message}`);
      }
      requiredObject(manifest, "open turn.stream manifest");
      if (requiredString(manifest.label, "open turn.stream manifest label") !== LIVE_STREAM_ARTIFACT_LABEL) {
        throw new Error("open turn stream run directory contains a non-turn.stream manifest");
      }
      if (manifest.payload_content_hash !== undefined && manifest.payload_content_hash !== null) {
        contentHashHex(manifest.payload_content_hash, "sealed turn.stream manifest payload_content_hash");
        continue;
      }
      const bodyPath = absolutePath(manifest.payload_path, "open turn.stream payload_path");
      const expectedBodyPath = path.join(turnRoot, "turn-stream.body.jsonl");
      if (bodyPath !== expectedBodyPath) {
        throw new Error("open turn.stream manifest points outside its exact turn directory");
      }
      const changes = [
        watchLedgerChange(bodyPath, input.waitMs),
        watchLedgerChange(manifestPath, input.waitMs),
      ];
      try {
        const parsed = await readOpenOperatorTurnStreamTail(bodyPath, input.cursors, {
          providerInterfaceId: input.liveContext.providerInterfaceId,
          runId: input.runId,
        });
        if (entry.name !== streamIdentityHash([parsed.header.threadId, parsed.header.turnId])) {
          throw new Error("open turn.stream directory differs from its header identity");
        }
        candidates.push({ parsed, changes });
      } catch (error) {
        cancelChanges(changes);
        throw error;
      }
    }
  } catch (error) {
    for (const candidate of candidates) cancelChanges(candidate.changes);
    throw error;
  }
  const events = candidates.flatMap(({ parsed }) => selectOperatorTurnStreamEvents(
    parsed, input.cursors.get(parsed.streamId) ?? null, input.limit,
  ).events).sort((left, right) => left.recordedAtUnixMs - right.recordedAtUnixMs
    || left.sourceMovementSeq - right.sourceMovementSeq
    || left.streamId.localeCompare(right.streamId) || left.seq - right.seq).slice(0, input.limit);
  const counts = new Map();
  for (const event of events) counts.set(event.streamId, (counts.get(event.streamId) ?? 0) + 1);
  const cursors = new Map(input.cursors);
  for (const { parsed } of candidates) {
    const selected = selectOperatorTurnStreamEvents(parsed, cursors.get(parsed.streamId) ?? null, counts.get(parsed.streamId) ?? 0);
    cursors.set(parsed.streamId, selected.cursor);
  }
  return { changes: candidates.flatMap((candidate) => candidate.changes), cursors, events };
}

function cancelChanges(changes) {
  for (const change of changes ?? []) change.cancel();
}

async function readRegularFile(filePath, maximumBytes, field) {
  const stat = await fs.lstat(filePath);
  if (stat.isSymbolicLink() || !stat.isFile()) throw new Error(`${field} is not a regular file`);
  if (stat.size > maximumBytes) throw new Error(`${field} exceeds ${maximumBytes} bytes`);
  const handle = await fs.open(filePath, "r");
  try {
    const before = await handle.stat();
    const bytes = Buffer.alloc(before.size);
    let filled = 0;
    while (filled < bytes.byteLength) {
      const { bytesRead } = await handle.read(bytes, filled, bytes.byteLength - filled, filled);
      if (bytesRead === 0) throw new Error(`${field} changed while being read`);
      filled += bytesRead;
    }
    const after = await handle.stat();
    if (after.size < before.size) {
      throw new Error(`${field} changed incompatibly while being read`);
    }
    return bytes;
  } finally {
    await handle.close();
  }
}

async function readOpenOperatorTurnStreamTail(filePath, cursor, expected) {
  const stat = await fs.lstat(filePath);
  if (stat.isSymbolicLink() || !stat.isFile()) throw new Error("open turn.stream body is not a regular file");
  if (stat.size > LIVE_STREAM_MAX_BYTES) {
    throw new Error(`open turn.stream body exceeds ${LIVE_STREAM_MAX_BYTES} bytes`);
  }
  const handle = await fs.open(filePath, "r");
  try {
    const before = await handle.stat();
    const headerBytes = await readOperatorTurnStreamHeaderBytes(handle, before.size);
    const headerRecord = parseOperatorTurnStreamRecords(headerBytes, 0, true)[0];
    const header = parseOperatorTurnStreamHeader(headerRecord, expected);
    cursor = cursor instanceof Map ? cursor.get(header.streamId) ?? null : cursor;
    let startOffset = headerRecord.endOffset;
    let state = {
      expectedSeq: 1,
      previousTime: header.openedAtUnixMs,
      previousSourceMovementSeq: header.openedSourceMovementSeq,
      sealed: false,
    };
    if (cursor?.stream_id === header.streamId) {
      startOffset = nonNegativeInteger(cursor.offset, "turn.stream cursor offset");
      if (startOffset < headerRecord.endOffset || startOffset > before.size) {
        throw new Error("turn.stream body shrank below its cursor");
      }
      state = {
        expectedSeq: nonNegativeInteger(cursor.seq, "turn.stream cursor seq"),
        previousTime: positiveInteger(
          cursor.recorded_at_unix_ms,
          "turn.stream cursor recorded_at_unix_ms",
        ),
        previousSourceMovementSeq: nonNegativeInteger(
          cursor.source_movement_seq,
          "turn.stream cursor source_movement_seq",
        ),
        sealed: cursor.sealed === true,
      };
    }
    const tailBytes = await readOperatorTurnStreamRange(handle, startOffset, before.size);
    const lastNewline = tailBytes.lastIndexOf(0x0a);
    const completeTail = lastNewline < 0 ? Buffer.alloc(0) : tailBytes.subarray(0, lastNewline + 1);
    const records = parseOperatorTurnStreamRecords(completeTail, startOffset, false);
    const rows = [];
    for (const record of records) {
      const validated = validateOperatorTurnStreamRow(record.value, record, header, state);
      rows.push(validated.row);
      state = validated.state;
    }
    const after = await handle.stat();
    if (after.size < before.size) throw new Error("open turn.stream body changed incompatibly while being read");
    return {
      header,
      headerEndOffset: headerRecord.endOffset,
      rows,
      streamId: header.streamId,
      nextSeq: state.expectedSeq,
      completeEndOffset: startOffset + completeTail.byteLength,
      tailStartOffset: startOffset,
      tailStartSeq: cursor?.stream_id === header.streamId ? state.expectedSeq - rows.length : 1,
      readBytes: headerBytes.byteLength + tailBytes.byteLength,
    };
  } finally {
    await handle.close();
  }
}

async function readOperatorTurnStreamHeaderBytes(handle, size) {
  const length = Math.min(size, LIVE_STREAM_MAX_HEADER_BYTES);
  if (length === 0) throw new Error("turn.stream body has no complete header row");
  const bytes = await readOperatorTurnStreamRange(handle, 0, length);
  const newline = bytes.indexOf(0x0a);
  if (newline < 0) throw new Error("turn.stream header exceeds its bounded complete row");
  return bytes.subarray(0, newline + 1);
}

async function readOperatorTurnStreamRange(handle, startOffset, endOffset) {
  const length = endOffset - startOffset;
  const bytes = Buffer.alloc(length);
  let filled = 0;
  while (filled < length) {
    const { bytesRead } = await handle.read(bytes, filled, length - filled, startOffset + filled);
    if (bytesRead === 0) throw new Error("turn.stream body changed while being read");
    filled += bytesRead;
  }
  return bytes;
}

function parseOperatorTurnStreamRecords(bytes, baseOffset, requireRecord) {
  const records = [];
  let startOffset = 0;
  for (let index = 0; index < bytes.byteLength; index += 1) {
    if (bytes[index] !== 0x0a) continue;
    const end = index > startOffset && bytes[index - 1] === 0x0d ? index - 1 : index;
    const raw = bytes.subarray(startOffset, end);
    if (raw.byteLength === 0) throw new Error("turn.stream body contains an empty complete row");
    let value;
    try {
      value = JSON.parse(raw.toString("utf8"));
    } catch (error) {
      throw new Error(`turn.stream body contains malformed complete JSON: ${error.message}`);
    }
    records.push({
      value: requiredObject(value, "turn.stream row"),
      startOffset: baseOffset + startOffset,
      endOffset: baseOffset + index + 1,
    });
    startOffset = index + 1;
  }
  if (requireRecord && records.length !== 1) throw new Error("turn.stream body has no exact header row");
  return records;
}

function parseOperatorTurnStreamHeader(headerRecord, expected) {
  const header = headerRecord?.value;
  if (
    header?.type !== "header"
    || header.schema_version !== LIVE_STREAM_SCHEMA_VERSION
    || header.seq !== 0
    || requiredString(header.provider_interface, "turn.stream header provider_interface") !== expected.providerInterfaceId
    || requiredString(header.run, "turn.stream header run") !== expected.runId
  ) {
    throw new Error("turn.stream header has the wrong provider/run identity");
  }
  const threadId = requiredString(header.thread_id, "turn.stream header thread_id");
  const turnId = requiredString(header.turn_id, "turn.stream header turn_id");
  const openedSourceMovementSeq = nonNegativeInteger(
    header.opened_source_movement_seq,
    "turn.stream header opened_source_movement_seq",
  );
  const openedAtUnixMs = positiveInteger(header.opened_at_unix_ms, "turn.stream header opened_at_unix_ms");
  return {
    providerInterfaceId: expected.providerInterfaceId,
    runId: expected.runId,
    threadId,
    turnId,
    openedAtUnixMs,
    openedSourceMovementSeq,
    streamId: streamIdentityHash([expected.providerInterfaceId, expected.runId, threadId, turnId]),
  };
}

function parseOperatorTurnStream(bytes, expected) {
  if (!Buffer.isBuffer(bytes) || bytes.byteLength === 0) {
    throw new Error("turn.stream body has no complete header row");
  }
  if (bytes.byteLength > LIVE_STREAM_MAX_BYTES) {
    throw new Error(`turn.stream body exceeds ${LIVE_STREAM_MAX_BYTES} bytes`);
  }
  if (expected.complete && bytes.at(-1) !== 0x0a) {
    throw new Error("sealed turn.stream body ends with an incomplete row");
  }
  const records = parseOperatorTurnStreamRecords(bytes, 0, false);
  const headerRecord = records.shift();
  if (!headerRecord) throw new Error("turn.stream body has no header row");
  const header = parseOperatorTurnStreamHeader(headerRecord, expected);
  const streamId = header.streamId;
  const rows = [];
  let state = {
    expectedSeq: 1,
    previousTime: header.openedAtUnixMs,
    previousSourceMovementSeq: header.openedSourceMovementSeq,
    sealed: false,
  };
  for (const record of records) {
    const validated = validateOperatorTurnStreamRow(record.value, record, header, state);
    rows.push(validated.row);
    state = validated.state;
  }
  if (expected.complete && !state.sealed) throw new Error("sealed turn.stream body has no seal row");
  return {
    header,
    headerEndOffset: headerRecord.endOffset,
    rows,
    streamId,
    nextSeq: state.expectedSeq,
    completeEndOffset: bytes.byteLength,
    tailStartOffset: headerRecord.endOffset,
    tailStartSeq: 1,
  };
}

function validateOperatorTurnStreamRow(row, record, header, state) {
  const seq = nonNegativeInteger(row.seq, "turn.stream row seq");
  if (seq !== state.expectedSeq) {
    throw new Error(`turn.stream row sequence ${seq} differs from expected ${state.expectedSeq}`);
  }
  const recordedAtUnixMs = positiveInteger(
    row.recorded_at_unix_ms,
    "turn.stream row recorded_at_unix_ms",
  );
  if (recordedAtUnixMs < state.previousTime) throw new Error("turn.stream row time moved backwards");
  const sourceMovementSeq = nonNegativeInteger(
    row.source_movement_seq,
    "turn.stream row source_movement_seq",
  );
  if (sourceMovementSeq < state.previousSourceMovementSeq) {
    throw new Error("turn.stream row source_movement_seq moved backwards");
  }
  const type = requiredString(row.type, "turn.stream row type");
  if (state.sealed) throw new Error("turn.stream contains data after its seal row");
  let sealed = false;
  if (type === "seal") {
    requiredString(row.terminal_label, "turn.stream seal terminal_label");
    contentHashHex(
      row.terminal_artifact_payload_blake3,
      "turn.stream seal terminal_artifact_payload_blake3",
    );
    sealed = true;
  } else if (type === "semantic_boundary") {
    requiredString(row.label, "turn.stream semantic boundary label");
    contentHashHex(row.artifact_payload_blake3, "turn.stream semantic boundary artifact_payload_blake3");
  } else if (type === "recovery") {
    positiveInteger(row.discarded_byte_count, "turn.stream recovery discarded_byte_count");
  } else if (type === "input_recorded") {
    validateOperatorTurnInputRecorded(row, header);
  } else if (type !== "event") {
    throw new Error(`turn.stream row has unknown type '${type}'`);
  }
  return {
    row: {
      type,
      seq,
      streamId: header.streamId,
      startOffset: record.startOffset,
      endOffset: record.endOffset,
      sourceMovementSeq,
      recordedAtUnixMs,
      sealedAfter: sealed,
      ...(type === "event" ? {
        label: requiredString(row.label, "turn.stream event label"),
        payload: row.payload,
      } : {}),
      ...(type === "input_recorded" ? { payload: { ...row } } : {}),
    },
    state: {
      expectedSeq: seq + 1,
      previousTime: recordedAtUnixMs,
      previousSourceMovementSeq: sourceMovementSeq,
      sealed,
    },
  };
}

function validateOperatorTurnInputRecorded(row, header) {
  const fields = new Set([
    "type", "seq", "recorded_at_unix_ms", "source_movement_seq",
    "provider_interface", "run", "thread_id", "turn_id", "item_id", "client_id",
    "stage", "source_event", "native_event_at_ms",
  ]);
  for (const field of Object.keys(row)) {
    if (!fields.has(field)) throw new Error(`turn.stream input_recorded has unknown field '${field}'`);
  }
  for (const field of ["provider_interface", "run", "thread_id", "turn_id", "item_id", "client_id"]) {
    const value = requiredTrimmedString(row[field], `turn.stream input_recorded ${field}`);
    if (/[\r\n\0]/.test(value)) throw new Error(`turn.stream input_recorded has invalid ${field}`);
  }
  if (
    row.provider_interface !== header.providerInterfaceId || row.run !== header.runId
    || row.thread_id !== header.threadId || row.turn_id !== header.turnId
    || row.stage !== "input_recorded_in_turn"
  ) throw new Error("turn.stream input_recorded differs from its turn identity or stage");
  if (row.source_event === "item/started" || row.source_event === "item/completed") {
    nonNegativeInteger(row.native_event_at_ms, "turn.stream input_recorded native_event_at_ms");
  } else if (row.source_event === "thread/resume") {
    if (Object.hasOwn(row, "native_event_at_ms")) {
      nonNegativeInteger(row.native_event_at_ms, "turn.stream history input_recorded native_event_at_ms");
    }
  } else {
    throw new Error("turn.stream input_recorded has an invalid source_event");
  }
}

function selectOperatorTurnStreamEvents(parsed, cursor, limit = Number.MAX_SAFE_INTEGER) {
  let offset = parsed.headerEndOffset;
  let seq = 1;
  let recordedAtUnixMs = parsed.header.openedAtUnixMs;
  let sourceMovementSeq = parsed.header.openedSourceMovementSeq;
  let sealed = false;
  if (cursor?.stream_id === parsed.streamId) {
    offset = nonNegativeInteger(cursor.offset, "turn.stream cursor offset");
    seq = nonNegativeInteger(cursor.seq, "turn.stream cursor seq");
    if (offset > parsed.completeEndOffset) throw new Error("turn.stream body shrank below its cursor");
    recordedAtUnixMs = positiveInteger(cursor.recorded_at_unix_ms, "turn.stream cursor recorded_at_unix_ms");
    sourceMovementSeq = nonNegativeInteger(cursor.source_movement_seq, "turn.stream cursor source_movement_seq");
    sealed = cursor.sealed === true;
    const boundary = offset === parsed.tailStartOffset && seq === parsed.tailStartSeq
      || offset === parsed.headerEndOffset && seq === 1
      || parsed.rows.some((row) => row.endOffset === offset && row.seq + 1 === seq)
      || offset === parsed.completeEndOffset && seq === parsed.nextSeq;
    if (!boundary) throw new Error("turn.stream cursor does not align to a complete row boundary");
  }
  const events = [];
  for (const row of parsed.rows) {
    if (row.seq < seq) continue;
    if (row.seq !== seq || row.startOffset !== offset) {
      throw new Error("turn.stream cursor encountered a sequence or byte-offset gap");
    }
    const projectsEvent = row.type === "event" || row.type === "input_recorded";
    if (projectsEvent && events.length >= limit) break;
    offset = row.endOffset;
    seq = row.seq + 1;
    recordedAtUnixMs = row.recordedAtUnixMs;
    sourceMovementSeq = row.sourceMovementSeq;
    sealed = row.sealedAfter;
    if (projectsEvent) events.push(row);
    if (events.length >= limit) break;
  }
  return {
    cursor: {
      stream_id: parsed.streamId,
      offset,
      seq,
      recorded_at_unix_ms: recordedAtUnixMs,
      source_movement_seq: sourceMovementSeq,
      ...(sealed ? { sealed: true } : {}),
    },
    events,
  };
}

function laterStreamCursor(current, candidate) {
  if (!current) return candidate;
  if (!candidate) return current;
  if (current.stream_id !== candidate.stream_id) return candidate;
  if (candidate.seq !== current.seq) return candidate.seq > current.seq ? candidate : current;
  return candidate.offset > current.offset ? candidate : current;
}

function streamIdentityHash(parts) {
  const hash = crypto.createHash("sha256");
  for (const part of parts) {
    const value = requiredString(part, "turn.stream identity part");
    const bytes = Buffer.from(value, "utf8");
    const length = Buffer.alloc(8);
    length.writeBigUInt64LE(BigInt(bytes.byteLength));
    hash.update(length);
    hash.update(bytes);
  }
  return hash.digest("hex");
}

function requiredBuffer(value, field) {
  if (!Buffer.isBuffer(value)) throw new Error(`${field} must be a Buffer`);
  return value;
}

function assertOperatorChatLiveArtifactBatch(batch) {
  if (!isObject(batch) || !Array.isArray(batch.artifacts)) {
    throw new Error("operator chat live artifact batch is invalid");
  }
  for (const artifact of batch.artifacts) {
    if (!isObject(artifact)) throw new Error("operator chat live artifact is invalid");
    requiredString(artifact.from_interface_id, "operator chat live artifact from_interface_id");
    requiredString(artifact.from_interface_role, "operator chat live artifact from_interface_role");
  }
  return batch;
}
