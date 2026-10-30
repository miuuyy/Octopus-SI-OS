module.exports = { artifactAttachmentBytes, artifactMovementCoordinates, artifactPayloadFilename, artifactPayloadMimeType, compareArtifactMovementNewest, compareMovementOrder, escapeSqlLike, filterByEnvelopeKind, filterInterfaceArtifactTransferTargets, formatCanonicalSchemaErrors, immutableClientAction, indexedEndpoint, indexedMovementRunId, indexedUnsignedInteger, interfaceArtifactTransferTargetRow, interfaceEndpointName, isClientSurfaceDocumentPayload, ledgerRowsForRun, loadLedgerArtifactPayload, matchesMovementPage, movementBaseKind, movementBelongsToRun, movementBoundaryInterface, movementEndpoint, movementKindText, movementPageRequiresIndex, movementPayloadPreview, movementRows, movementRunId, nestedArtifactMedia, normalizeIndexedProjectionMovement, normalizedMovementKindFilter, openValidatedLedgerIndex, optionalActivityAfterSeq, optionalUnsignedInteger, pendingArtifactRows, projectMovementRows, projectedTextFile, projectionLedgerSourceCursor, projectionRequiresFullLedger, rawMovement, readChangeJournal, readIndexedTaskSearchCandidates, readStableArtifactBytes, readStableContentAddressedArtifactBytes, recentMovementRows, summarizeInterfaceArtifactTransferTargets, taskSearchSnippet, validateCanonicalMovement };

const { readContentAddressedArtifactProjection } = require("../contentAddressedArtifact.cjs");
const { setImmediate: yieldToClient } = require("node:timers/promises");
const fsSync = require("node:fs");
const fs = require("node:fs/promises");
const crypto = require("node:crypto");
const path = require("node:path");
const { blake3 } = require("@noble/hashes/blake3");
const { changeJournalPlan } = require("./changeJournal.cjs");
const { ARTIFACT_MANIFEST_PREVIEW_MAX_BYTES, ARTIFACT_PREVIEW_MAX_BYTES, FULL_LEDGER_PROJECTION_METHODS, MOVEMENT_PAYLOAD_PREVIEW_BYTES, ProjectionEvidenceError, movementSchemaValidator } = require("./constants.cjs");
const { envelopeRows, preserveProjectionEvidenceError } = require("./registryRows.cjs");
const { arrayValue, contentHashHex, endpointId, filterByString, isPathInside, ledgerArtifactId, ledgerArtifactLabel, numberValue, objectOrEmpty, requiredContentHash, requiredString, rowTimeMs, stringValue, timestampOf } = require("./values.cjs");

function projectionRequiresFullLedger(method, params) {
  if (params.include_ledger === false && [
    "projection.runtime.topology",
    "projection.runtime.snapshot",
    "projection.ee_instances.list",
    "projection.runs.list",
    "projection.runs.checkpoints.list",
    "projection.runs.get",
  ].includes(method)) return false;
  return FULL_LEDGER_PROJECTION_METHODS.has(method);
}

async function movementRows(projection) {
  return projectMovementRows(projection, projection.ledgerRows);
}

async function recentMovementRows(projection, limit) {
  const count = Math.max(1, numberValue(limit, 1_000));
  return projectMovementRows(projection, projection.ledgerRows.slice(-count)).slice(0, count);
}

function projectMovementRows(projection, sourceRows) {
  const rows = sourceRows.map((row) => {
    const contentHex = contentHashHex(row?.artifact?.content);
    const runtime = objectOrEmpty(objectOrEmpty(row.invocation_metadata).runtime);
    const blobPath = contentHex && row.source_file
      ? path.join(path.dirname(row.source_file), "blobs", contentHex)
      : "";
    return {
      movement_id: `${path.basename(stringValue(row.source_file) || "ledger", ".jsonl")}:${row.source_line}`,
      seq: numberValue(row.seq, null),
      lamport: numberValue(row.lamport, null),
      trace: stringValue(row.trace ?? row.trace_id),
      kind: movementKindText(row.kind),
      recorded_at_unix_ms: numberValue(row.recorded_at_unix_ms ?? row.wall_clock_unix_ms, 0),
      raw_run_id: movementRunId(row),
      raw_ee_id: stringValue(runtime.ee),
      raw_worker: stringValue(runtime.worker),
      artifact_id: ledgerArtifactId(row),
      artifact_label: ledgerArtifactLabel(row),
      content_hash: contentHex,
      blob_path: blobPath,
      blob_size: null,
      from: movementEndpoint(row.from),
      to: movementEndpoint(row.to),
      source_file: stringValue(row.source_file),
      source_line: numberValue(row.source_line, 0),
      raw: rawMovement(row),
    };
  });
  return rows.sort((a, b) => (b.seq ?? -1) - (a.seq ?? -1) || b.recorded_at_unix_ms - a.recorded_at_unix_ms);
}

function movementPageRequiresIndex(params) {
  return optionalUnsignedInteger(params?.before_seq) !== null
    || optionalUnsignedInteger(params?.after_seq) !== null
    || Boolean(stringValue(params?.query))
    || Number.isFinite(numberValue(params?.recorded_after_unix_ms, NaN))
    || Number.isFinite(numberValue(params?.recorded_before_unix_ms, NaN));
}

function matchesMovementPage(movement, seq, filter) {
  if (filter.beforeSeq !== null && seq >= filter.beforeSeq) return false;
  if (filter.afterSeq !== null && seq <= filter.afterSeq) return false;
  const recordedAt = indexedUnsignedInteger(movement?.recorded_at_unix_ms);
  if (recordedAt === null) return false;
  if (filter.recordedAfter !== null && recordedAt < filter.recordedAfter) return false;
  if (filter.recordedBefore !== null && recordedAt > filter.recordedBefore) return false;
  if (filter.kinds.size > 0 && !filter.kinds.has(movementBaseKind(movement))) return false;
  if (!filter.query) return true;
  const needle = filter.query;
  return [
    movementKindText(movement?.kind),
    stringValue(movement?.trace),
    ledgerArtifactId(movement),
    ledgerArtifactLabel(movement),
    ...Object.values(objectOrEmpty(movement?.from)).map(stringValue),
    ...Object.values(objectOrEmpty(movement?.to)).map(stringValue),
  ].some((value) => value.toLocaleLowerCase().includes(needle));
}

function escapeSqlLike(value) {
  return value.replace(/[\\%_]/g, (character) => `\\${character}`);
}

function normalizedMovementKindFilter(params) {
  return [...new Set(arrayValue(params?.kinds).map(stringValue).map((kind) => kind.replace(/\(.+\)$/, "")).filter((kind) => (
    kind
    && kind.length <= 128
    && /^[A-Za-z][A-Za-z0-9_.-]*$/.test(kind)
  )))].sort();
}

function movementBaseKind(row) {
  return movementKindText(row?.kind).replace(/\(.+\)$/, "");
}

function isClientSurfaceDocumentPayload(value) {
  return Boolean(
    value
    && typeof value === "object"
    && !Array.isArray(value)
    && value.schema === "octopus.client_surface"
    && value.schema_version === 1,
  );
}

async function immutableClientAction(row) {
  const manifestHash = contentHashHex(row?.artifact?.content);
  if (!manifestHash || !row.source_file) throw new Error("Client action has no immutable manifest.");
  const blobRoot = path.join(path.dirname(row.source_file), "blobs");
  const manifest = JSON.parse((await readStableContentAddressedArtifactBytes(
    path.join(blobRoot, manifestHash), manifestHash, ARTIFACT_MANIFEST_PREVIEW_MAX_BYTES, "client action manifest",
  )).toString("utf8"));
  if (!manifest || typeof manifest !== "object" || Array.isArray(manifest)
    || manifest.label !== "client.action" || row.artifact.id !== `client.action:${manifestHash}`) {
    throw new Error("Client action identity does not match its immutable manifest.");
  }
  const payloadHash = requiredContentHash(manifest.payload_content_hash, "client action payload_content_hash");
  if (typeof manifest.payload_path !== "string" || !path.isAbsolute(manifest.payload_path)) {
    throw new Error("Client action payload_path must be absolute.");
  }
  const bytes = await readStableContentAddressedArtifactBytes(
    path.join(blobRoot, payloadHash), payloadHash, ARTIFACT_PREVIEW_MAX_BYTES, "client action payload",
  );
  const text = bytes.toString("utf8");
  if (!Buffer.from(text, "utf8").equals(bytes)) throw new Error("Client action payload must be UTF-8 JSON.");
  const { parseClientActionRequest } = await import("../clientActionSemantics.mjs");
  return parseClientActionRequest(JSON.parse(text));
}

async function openValidatedLedgerIndex(projection) {
  let DatabaseSync;
  try {
    ({ DatabaseSync } = require("node:sqlite"));
  } catch (error) {
    preserveProjectionEvidenceError(error);
    return null;
  }
  const ledgerPath = path.join(projection.durableRoot, "movements.jsonl");
  const indexPath = path.join(projection.durableRoot, "ledger-index.sqlite");
  let database;
  try {
    database = new DatabaseSync(indexPath, { readOnly: true });
    database.exec("PRAGMA query_only = ON; BEGIN");
    const meta = database.prepare(
      "SELECT key, value FROM index_meta WHERE key IN ('schema_version', 'watermark', 'durable_end_offset')",
    ).all();
    const values = new Map(meta.map((row) => [String(row.key), String(row.value)]));
    if (values.get("schema_version") !== "4") {
      database.close();
      return null;
    }
    const watermark = indexedUnsignedInteger(values.get("watermark"));
    const durableEndOffset = indexedUnsignedInteger(values.get("durable_end_offset"));
    const ledgerStat = await fs.stat(ledgerPath);
    if (watermark === null || durableEndOffset === null || durableEndOffset > ledgerStat.size) {
      database.close();
      return null;
    }
    const last = database.prepare("SELECT seq FROM movements ORDER BY seq DESC LIMIT 1").get();
    if (
      watermark === 0
        ? last !== undefined
        : indexedUnsignedInteger(last?.seq) !== watermark - 1
    ) {
      database.close();
      return null;
    }
    return { database, durableEndOffset, ledgerPath, ledgerSize: ledgerStat.size, watermark };
  } catch {
    database?.close();
    return null;
  }
}

async function projectionLedgerSourceCursor(projection, primarySeq) {
  const sources = [];
  for (const [name, file] of [
    ["movements", path.join(projection.durableRoot, "movements.jsonl")],
  ]) {
    try {
      const stat = await fs.stat(file);
      sources.push({
        name,
        identity: `${stat.dev}:${stat.ino}`,
        mtimeMs: stat.mtimeMs,
        size: stat.size,
      });
    } catch (error) {
      if (error?.code !== "ENOENT") throw error;
      sources.push({ name, identity: "", mtimeMs: 0, size: 0 });
    }
  }
  return {
    identity: JSON.stringify(sources.map((source) => [source.name, source.identity])),
    mtimes: sources.map((source) => source.mtimeMs),
    primary_seq: primarySeq,
    sizes: sources.map((source) => source.size),
  };
}

function movementBelongsToRun(row, runId) {
  return movementRunId(row) === runId;
}

function movementRunId(row) {
  const metadata = objectOrEmpty(row.invocation_metadata);
  return movementBaseKind(row) === "MarkOutcomeAdmitted"
    ? stringValue(metadata.run)
    : stringValue(objectOrEmpty(metadata.runtime).run);
}

async function artifactAttachmentBytes(payloadBytes, attachmentId) {
  if (typeof attachmentId !== "string" || !/^[0-9a-f]{64}$/.test(attachmentId)) {
    throw new Error("attachment_id must be an exact SHA-256 digest.");
  }
  let payload;
  try {
    payload = JSON.parse(payloadBytes.toString("utf8"));
  } catch {
    throw new Error("The admitted artifact has no attachment manifest.");
  }
  const matches = Array.isArray(payload?.attachments)
    ? payload.attachments.filter((entry) => entry?.payload_content_hash === attachmentId)
    : [];
  if (matches.length !== 1) throw new Error("The requested attachment must match exactly one admitted manifest entry.");
  const attachment = matches[0];
  const filename = requiredString(attachment.name, "attachment.name");
  const mimeType = attachment.mime_type;
  if (typeof mimeType !== "string" || mimeType.length > 160 || !/^[A-Za-z0-9!#$&^_.+-]+\/[A-Za-z0-9!#$&^_.+-]+$/.test(mimeType)) {
    throw new Error("Attachment mime_type is invalid.");
  }
  const payloadPath = requiredString(attachment.payload_path, "attachment.payload_path");
  if (!path.isAbsolute(payloadPath)) throw new Error("Attachment payload_path must be absolute.");
  const size = attachment.size;
  if (!Number.isSafeInteger(size) || size < 0 || size > ARTIFACT_PREVIEW_MAX_BYTES) {
    throw new Error(`Attachment size exceeds the ${ARTIFACT_PREVIEW_MAX_BYTES} byte preview boundary or is invalid.`);
  }
  const stat = await fs.lstat(payloadPath);
  if (stat.isSymbolicLink() || !stat.isFile()) throw new Error("Attachment payload is not a regular file.");
  const handle = await fs.open(payloadPath, fsSync.constants.O_RDONLY | fsSync.constants.O_NOFOLLOW | fsSync.constants.O_NONBLOCK);
  try {
    const before = await handle.stat();
    if (!before.isFile() || before.dev !== stat.dev || before.ino !== stat.ino || before.size !== size) {
      throw new Error("Attachment size or file identity does not match its admitted manifest.");
    }
    const buffer = Buffer.alloc(size + 1);
    let offset = 0;
    while (offset < buffer.length) {
      const { bytesRead } = await handle.read(buffer, offset, buffer.length - offset, offset);
      if (!bytesRead) break;
      offset += bytesRead;
    }
    const after = await handle.stat();
    if (offset !== size || after.size !== size || before.mtimeMs !== after.mtimeMs || before.ctimeMs !== after.ctimeMs) {
      throw new Error("Attachment changed while it was being read.");
    }
    const bytes = buffer.subarray(0, size);
    const contentHash = crypto.createHash("sha256").update(bytes).digest("hex");
    if (contentHash !== attachmentId) throw new Error("Attachment content hash does not match its admitted manifest.");
    return { bytes, contentHash, filename: path.basename(filename), mimeType };
  } finally {
    await handle.close();
  }
}

function artifactMovementCoordinates(row) {
  const runtime = objectOrEmpty(objectOrEmpty(row.invocation_metadata).runtime);
  const from = objectOrEmpty(row.from);
  const to = objectOrEmpty(row.to);
  return {
    runId: movementRunId(row),
    interfaceId: stringValue(runtime.interface ?? from.Interface ?? from.interface ?? to.Interface ?? to.interface),
  };
}

async function nestedArtifactMedia(payloadBytes) {
  let payload;
  try {
    payload = JSON.parse(payloadBytes.toString("utf8"));
  } catch {
    return null;
  }
  const media = objectOrEmpty(objectOrEmpty(payload).media);
  const mediaPath = stringValue(media.path);
  const mimeType = stringValue(media.mime_type ?? media.mimeType);
  const contentHash = stringValue(media.content_hash ?? media.contentHash).toLowerCase();
  const expectedSize = numberValue(media.size, -1);
  if (!mediaPath && !mimeType && !contentHash && expectedSize < 0) return null;
  if (!path.isAbsolute(mediaPath)) throw new Error("Artifact media.path must be absolute.");
  if (!mimeType || mimeType.length > 160 || /[\u0000-\u001f\u007f]/.test(mimeType)) {
    throw new Error("Artifact media.mime_type is invalid.");
  }
  if (!/^[0-9a-f]{64}$/.test(contentHash)) throw new Error("Artifact media.content_hash must be a SHA-256 digest.");
  const bytes = await readStableArtifactBytes(mediaPath, ARTIFACT_PREVIEW_MAX_BYTES, "artifact media");
  if (expectedSize !== bytes.byteLength) throw new Error("Artifact media.size does not match the admitted file.");
  const actualHash = crypto.createHash("sha256").update(bytes).digest("hex");
  if (actualHash !== contentHash) throw new Error("Artifact media.content_hash does not match the admitted file.");
  return {
    bytes,
    contentHash: actualHash,
    filename: path.basename(mediaPath),
    mimeType,
  };
}

async function readStableArtifactBytes(filePath, maximumBytes, label, onStableFile) {
  const stat = await fs.lstat(filePath);
  if (stat.isSymbolicLink() || !stat.isFile()) throw new Error(`${label} is not a regular file.`);
  if (stat.size < 0 || stat.size > maximumBytes) throw new Error(`${label} exceeds the ${maximumBytes} byte preview boundary.`);
  const handle = await fs.open(filePath, "r");
  try {
    const before = await handle.stat();
    if (!before.isFile() || before.dev !== stat.dev || before.ino !== stat.ino || before.size !== stat.size) {
      throw new Error(`${label} changed before it was read.`);
    }
    const bytes = await handle.readFile();
    const after = await handle.stat();
    if (bytes.byteLength !== before.size || before.size !== after.size || before.mtimeMs !== after.mtimeMs || before.ctimeMs !== after.ctimeMs) {
      throw new Error(`${label} changed while it was being read.`);
    }
    onStableFile?.({ path: filePath, identity: [after.dev, after.ino, after.size, after.mtimeMs, after.ctimeMs] });
    return bytes;
  } finally {
    await handle.close();
  }
}

async function readStableContentAddressedArtifactBytes(filePath, expectedHash, maximumBytes, label, onVerifiedFile) {
  let evidence;
  const bytes = await readStableArtifactBytes(filePath, maximumBytes, label, (value) => { evidence = value; });
  const hash = blake3.create();
  for (let offset = 0; offset < bytes.length; offset += 64 * 1024) {
    hash.update(bytes.subarray(offset, offset + 64 * 1024));
    await yieldToClient();
  }
  const actualHash = Buffer.from(hash.digest()).toString("hex");
  if (actualHash !== expectedHash) throw new Error(`${label} does not match its content-addressed digest.`);
  onVerifiedFile?.(evidence);
  return bytes;
}

function artifactPayloadFilename(label, payloadPath, payloadBytes) {
  const basename = path.basename(payloadPath);
  if (basename && basename !== "." && basename !== path.sep) return basename;
  const stem = label.replace(/[^A-Za-z0-9._-]+/g, "-").replace(/^-+|-+$/g, "") || "artifact";
  const mimeType = artifactPayloadMimeType(payloadBytes);
  return `${stem}${mimeType === "application/json" ? ".json" : mimeType.startsWith("text/") ? ".txt" : ".bin"}`;
}

function artifactPayloadMimeType(payloadBytes) {
  const text = payloadBytes.toString("utf8");
  if (Buffer.from(text, "utf8").equals(payloadBytes)) {
    try {
      JSON.parse(text);
      return "application/json";
    } catch {
      return "text/plain;charset=utf-8";
    }
  }
  return "application/octet-stream";
}

function movementKindText(kind) {
  if (typeof kind === "string") return kind;
  if (kind && typeof kind === "object") {
    for (const [variant, value] of Object.entries(kind)) {
      const text = stringValue(value);
      return text ? `${variant}(${text})` : variant;
    }
  }
  return "";
}

function interfaceEndpointName(endpoint) {
  return endpointId(endpoint, "Interface");
}

function movementEndpoint(endpoint) {
  const id = stringValue(endpoint?.Interface);
  if (!id || Object.keys(endpoint).length !== 1) {
    throw new ProjectionEvidenceError("ledger movement contains a non-interface endpoint");
  }
  return { kind: "interface", id };
}

function rawMovement(row) {
  const { source_file, source_line, envelope_id, ...rest } = row;
  if (rest.artifact && Array.isArray(rest.artifact.content)) {
    rest.artifact = { ...rest.artifact, content: contentHashHex(rest.artifact.content) };
  }
  return rest;
}

async function movementPayloadPreview(params) {
  const blobPath = requiredString(params.blob_path, "blob_path");
  const hex = path.basename(blobPath);
  if (!/^[0-9a-f]{64}$/.test(hex) || path.basename(path.dirname(blobPath)) !== "blobs") {
    throw new Error("blob_path must point into a ledger blobs/ directory by content hash");
  }
  let bytes;
  try {
    bytes = await fs.readFile(blobPath);
  } catch (error) {
    if (error?.code === "ENOENT") {
      return { resolved: false, blob_path: blobPath, size: 0, text: "", truncated: false };
    }
    throw error;
  }
  const slice = bytes.subarray(0, MOVEMENT_PAYLOAD_PREVIEW_BYTES);
  const text = slice.toString("utf8");
  const binary = text.includes("\uFFFD");
  return {
    resolved: true,
    blob_path: blobPath,
    size: bytes.byteLength,
    truncated: bytes.byteLength > slice.byteLength,
    text: binary ? "" : text,
    binary,
  };
}

async function projectedTextFile(file, interfaceVarDir, warnings, label) {
  if (!file || !path.isAbsolute(file) || !isPathInside(interfaceVarDir, file)) {
    if (file) warnings.push(`${label} path escapes its interface var directory: ${file}`);
    return "";
  }
  try {
    const [resolvedInterfaceVarDir, resolvedFile] = await Promise.all([
      fs.realpath(interfaceVarDir),
      fs.realpath(file),
    ]);
    if (!isPathInside(resolvedInterfaceVarDir, resolvedFile)) {
      warnings.push(`${label} resolves outside its interface var directory: ${file}`);
      return "";
    }
    const stat = await fs.stat(resolvedFile);
    if (!stat.isFile()) return "";
    const maxBytes = 512 * 1024;
    const start = Math.max(0, stat.size - maxBytes);
    const handle = await fs.open(resolvedFile, "r");
    try {
      const bytes = Buffer.alloc(Math.min(maxBytes, stat.size));
      const { bytesRead } = await handle.read(bytes, 0, bytes.length, start);
      return bytes.subarray(0, bytesRead).toString("utf8");
    } finally {
      await handle.close();
    }
  } catch (error) {
    if (error?.code !== "ENOENT") warnings.push(`${label} cannot be read: ${file}: ${error.message}`);
    return "";
  }
}

function validateCanonicalMovement(row, file, lineNumber) {
  if (!movementSchemaValidator(row)) {
    throw new ProjectionEvidenceError(
      `ledger movement violates the generated movement schema: ${file}:${lineNumber}: ${formatCanonicalSchemaErrors(movementSchemaValidator.errors)}`,
    );
  }
  if (!stringValue(row.trace)) {
    throw new ProjectionEvidenceError(`ledger movement trace is empty: ${file}:${lineNumber}`);
  }
  return row;
}

function formatCanonicalSchemaErrors(errors) {
  return (errors ?? []).map((error) => `${error.instancePath || "/"} ${error.message}`).join("; ");
}

async function loadLedgerArtifactPayload(row, hex, required = false, projection = null) {
  const files = [];
  const onVerifiedFile = (file) => files.push(file);
  if (projection) {
    const resolved = await readContentAddressedArtifactProjection({
      blobsRoot: path.join(path.dirname(row.source_file), "blobs"), manifestHash: hex,
      artifactId: ledgerArtifactId(row), label: ledgerArtifactLabel(row), projection, onVerifiedFile,
    });
    return { payload: resolved.payload, cacheable: true, retainedBytes: resolved.projection.projected_bytes, files };
  }
  const blobPath = path.join(path.dirname(row.source_file), "blobs", hex);
  let bytes;
  try {
    bytes = await readStableContentAddressedArtifactBytes(
      blobPath,
      hex,
      ARTIFACT_MANIFEST_PREVIEW_MAX_BYTES,
      "ledger artifact manifest",
      onVerifiedFile,
    );
  } catch (error) {
    if (error?.code === "ENOENT" && !required) return { payload: null, cacheable: false, retainedBytes: 0 };
    throw error;
  }
  const text = bytes.toString("utf8");
  let manifest;
  try {
    manifest = JSON.parse(text);
  } catch {
    return { payload: { __raw: text }, cacheable: true, retainedBytes: bytes.byteLength, files };
  }
  const payloadPath = stringValue(manifest?.payload_path);
  if (!payloadPath) return { payload: manifest, cacheable: true, retainedBytes: bytes.byteLength, files };
  if (!path.isAbsolute(payloadPath)) {
    throw new Error(`ledger artifact manifest payload_path must be absolute: ${payloadPath}`);
  }
  const expectedPayloadHash = requiredContentHash(
    manifest?.payload_content_hash,
    "ledger artifact manifest payload_content_hash",
  );
  let payloadBytes;
  try {
    payloadBytes = await readStableContentAddressedArtifactBytes(
      path.join(path.dirname(row.source_file), "blobs", expectedPayloadHash),
      expectedPayloadHash,
      ledgerArtifactLabel(row) === "turn.stream" ? 256 * 1024 * 1024 : ARTIFACT_PREVIEW_MAX_BYTES,
      "ledger artifact payload",
      onVerifiedFile,
    );
  } catch (error) {
    if (error?.code === "ENOENT" && !required) return { payload: manifest, cacheable: false, retainedBytes: 0 };
    throw error;
  }
  const payloadText = payloadBytes.toString("utf8");
  try {
    return { payload: withoutEmbeddedMedia(JSON.parse(payloadText)), cacheable: true, retainedBytes: bytes.byteLength + payloadBytes.byteLength, files };
  } catch {
    return { payload: { __raw: payloadText }, cacheable: true, retainedBytes: bytes.byteLength + payloadBytes.byteLength, files };
  }
}

function withoutEmbeddedMedia(payload) {
  if (!Array.isArray(payload?.media)) return payload;
  return { ...payload, media: payload.media.map((entry) => {
    if (!entry || typeof entry !== "object" || Array.isArray(entry)) return entry;
    const { payload_base64: _bytes, ...summary } = entry;
    return summary;
  }) };
}

function movementBoundaryInterface(row, endpointKey) {
  return endpointId(row?.[endpointKey], "Interface");
}

function normalizeIndexedProjectionMovement(row, ledgerPath) {
  const seq = indexedUnsignedInteger(row.seq);
  const lamport = indexedUnsignedInteger(row.lamport);
  const recordedAt = indexedUnsignedInteger(row.recorded_at_unix_ms);
  if (seq === null || lamport === null || recordedAt === null) {
    throw new Error("ledger index contains a non-integer movement coordinate");
  }
  const artifactId = stringValue(row.artifact_id);
  const content = contentHashHex(row.artifact_content);
  if (Boolean(artifactId) !== Boolean(content)) {
    throw new ProjectionEvidenceError(`ledger index movement ${seq} carries an incomplete artifact reference`);
  }
  const runtime = {
    run: stringValue(row.runtime_run),
    checkpoint: stringValue(row.runtime_checkpoint),
    interface: stringValue(row.runtime_interface),
  };
  const mark = stringValue(row.mark);
  const from = indexedEndpoint(row.from_kind, row.from_id);
  const to = indexedEndpoint(row.to_kind, row.to_id);
  const canonical = validateCanonicalMovement({
    seq,
    lamport,
    recorded_at_unix_ms: recordedAt,
    trace: stringValue(row.trace),
    kind: mark ? { MarkOutcomeAdmitted: mark } : stringValue(row.kind),
    from,
    to,
    ...(artifactId ? { artifact: { id: artifactId, content: [...Buffer.from(content, "hex")] } } : {}),
  }, ledgerPath, seq + 1);
  return {
    seq,
    lamport,
    recorded_at_unix_ms: recordedAt,
    trace: canonical.trace,
    kind: movementKindText(canonical.kind),
    from,
    to,
    ...(artifactId ? { artifact: { id: artifactId, content } } : {}),
    invocation_metadata: mark ? { run: runtime.run, name: mark } : { runtime },
    runtime,
    envelope_id: `movements:${seq + 1}`,
    source_file: ledgerPath,
    source_line: seq + 1,
  };
}

function indexedEndpoint(kindValue, idValue) {
  const id = stringValue(idValue);
  if (kindValue !== "interface" || !id) {
    throw new ProjectionEvidenceError("ledger index contains a non-interface endpoint");
  }
  return { Interface: id };
}

function indexedMovementRunId(row) {
  return stringValue(row.runtime_run);
}

function indexedUnsignedInteger(value) {
  const number = typeof value === "bigint" ? Number(value) : Number(value);
  return Number.isSafeInteger(number) && number >= 0 ? number : null;
}

function optionalUnsignedInteger(value) {
  if (value === undefined || value === null || value === "") return null;
  const parsed = indexedUnsignedInteger(value);
  if (parsed === null) throw new Error("movement cursor and timestamp parameters must be unsigned integers");
  return parsed;
}

function optionalActivityAfterSeq(value) {
  return value === -1 ? null : optionalUnsignedInteger(value);
}

function compareMovementOrder(left, right) {
  return rowTimeMs(left) - rowTimeMs(right)
    || numberValue(left.seq, 0) - numberValue(right.seq, 0);
}

function ledgerRowsForRun(projection, runId) {
  const rows = [];
  for (const source of projection.ledgerRowsByRunSources ?? []) {
    const matching = source instanceof Map ? source.get(runId) : null;
    if (matching) rows.push(...matching);
  }
  return rows;
}

function readIndexedTaskSearchCandidates(index, targetsBySourceRun, beforeSeq, limit, protocolValue) {
  if (beforeSeq <= 0 || targetsBySourceRun.size === 0) return [];
  const protocol = objectOrEmpty(protocolValue);
  const labels = [protocol.inputLabel, protocol.terminalOutputLabel, protocol.errorOutputLabel]
    .map(stringValue)
    .filter(Boolean);
  if (labels.length !== 3) throw new Error("operator chat protocol does not declare the searchable transcript labels");
  const runIds = [...targetsBySourceRun.keys()];
  const rows = [];
  for (let offset = 0; offset < runIds.length; offset += 96) {
    const chunk = runIds.slice(offset, offset + 96);
    const targetPredicates = [];
    const targetValues = [];
    for (const runId of chunk) {
      const target = targetsBySourceRun.get(runId);
      targetPredicates.push(`(
        (
          runtime_run = ?
        )
        AND (
          (kind = 'InterfaceArrival' AND artifact_label = ? AND from_kind = 'interface' AND from_id = ?)
          OR (kind = 'InterfaceOutbound' AND artifact_label IN (?, ?) AND to_kind = 'interface' AND to_id = ?)
        )
      )`);
      targetValues.push(
        runId,
        protocol.inputLabel,
        target.chatInterfaceId,
        protocol.terminalOutputLabel,
        protocol.errorOutputLabel,
        target.chatInterfaceId,
      );
    }
    const selected = index.database.prepare(
      `SELECT
         seq, lamport, recorded_at_unix_ms, trace, kind, mark,
         from_kind, from_id, to_kind, to_id,
         artifact_id, artifact_label, artifact_content,
         runtime_run, runtime_checkpoint, runtime_interface
       FROM movements
       WHERE seq < ?
         AND artifact_id IS NOT NULL
         AND (${targetPredicates.join("\nOR ")})
       ORDER BY seq DESC
       LIMIT ?`,
    ).all(beforeSeq, ...targetValues, limit);
    rows.push(...selected);
  }
  const bySeq = new Map();
  for (const row of rows) bySeq.set(numberValue(row.seq, -1), row);
  return [...bySeq.values()]
    .sort((left, right) => numberValue(right.seq, 0) - numberValue(left.seq, 0))
    .flatMap((row) => {
      const sourceRunId = indexedMovementRunId(row);
      const target = targetsBySourceRun.get(sourceRunId);
      if (!target) return [];
      const label = stringValue(row.artifact_label);
      const fromInterface = stringValue(row.from_kind).toLowerCase() === "interface" ? stringValue(row.from_id) : "";
      const toInterface = stringValue(row.to_kind).toLowerCase() === "interface" ? stringValue(row.to_id) : "";
      const inbound = stringValue(row.kind) === "InterfaceArrival"
        && label === protocol.inputLabel
        && fromInterface === target.chatInterfaceId;
      const outbound = stringValue(row.kind) === "InterfaceOutbound"
        && [protocol.terminalOutputLabel, protocol.errorOutputLabel].includes(label)
        && toInterface === target.chatInterfaceId;
      return inbound || outbound
        ? [{ row: normalizeIndexedProjectionMovement(row, index.ledgerPath), target }]
        : [];
    })
    .slice(0, limit);
}

function taskSearchSnippet(value, normalizedIndex, needleLength) {
  const compact = value.replace(/\s+/g, " ").trim();
  const start = Math.max(0, normalizedIndex - 54);
  const end = Math.min(compact.length, normalizedIndex + needleLength + 90);
  return `${start ? "…" : ""}${compact.slice(start, end)}${end < compact.length ? "…" : ""}`;
}

async function readChangeJournal(projection, runId) {
  try {
    const plan = await changeJournalPlan(projection.octopusHome, projection.registryRoot, runId);
    if (!plan) return null;
    const { writes, ...journal } = plan;
    return journal;
  } catch (error) {
    return { run_id: runId, error: String(error?.message ?? error) };
  }
}

function interfaceArtifactTransferTargetRow(args) {
  return {
    target_id: [
      args.interfaceId,
      stringValue(args.activation?.ref) || "no_transport",
      stringValue(args.activation?.descriptor_path) || "no_descriptor",
    ].join(":"),
    interface_id: args.interfaceId,
    interface_type: stringValue(args.iface?.interface_type),
    role: stringValue(args.iface?.role),
    direction: stringValue(args.iface?.direction),
    activation_transport_ref: stringValue(args.activation?.ref),
    transfer_path: stringValue(args.transferPath),
    status: stringValue(args.status) || "unknown",
    reason: stringValue(args.reason),
    source: "interface_artifact_transfer_protocol",
  };
}

function summarizeInterfaceArtifactTransferTargets(targets, transfer) {
  const ready = targets.filter((row) => stringValue(row.status) === "ready").length;
  if (ready > 0) {
    return {
      status: "ready",
      target_count: targets.length,
      ready_targets: ready,
      reason: `${ready} artifact hand-in socket target${ready === 1 ? "" : "s"} ready`,
    };
  }
  const first = targets[0];
  if (first) {
    return {
      status: stringValue(first.status) || "unknown",
      target_count: targets.length,
      ready_targets: 0,
      reason: stringValue(first.reason),
    };
  }
  return {
    status: stringValue(transfer.status) || "unknown",
    target_count: 0,
    ready_targets: 0,
    reason: stringValue(transfer.reason),
  };
}

function compareArtifactMovementNewest(left, right) {
  return numberValue(right?.seq, -1) - numberValue(left?.seq, -1)
    || timestampOf(right) - timestampOf(left)
    || numberValue(right?.source_line, 0) - numberValue(left?.source_line, 0)
    || stringValue(left?.source_file).localeCompare(stringValue(right?.source_file));
}

function pendingArtifactRows(projection) {
  return envelopeRows(projection)
    .filter((row) => ["pending", "open", "in_flight"].includes(stringValue(row.status).toLowerCase()))
    .map((row) => ({
      pending_id: stringValue(row.pending_id ?? row.envelope_id),
      template_name: stringValue(row.interface_id ?? row.template_name ?? row.source_endpoint),
      artifact_id: stringValue(row.artifact_id ?? row.artifact?.artifact_id ?? row.payload?.artifact_id),
      trace_id: stringValue(row.trace_id),
      created_at: timestampOf(row),
      raw: row,
    }));
}

function filterInterfaceArtifactTransferTargets(rows, params, projection) {
  let scopedRows = rows;
  if (params.run_id !== undefined) {
    const runId = requiredString(params.run_id, "run_id");
    const run = arrayValue(projection.runRecords).find((record) => stringValue(record.run_id) === runId);
    if (!run) throw new Error(`run is not present in file projection: ${runId}`);
    const activeInterfaces = new Set();
    for (const checkpoint of arrayValue(run.checkpoints)) {
      if (stringValue(checkpoint.status) !== "projected") continue;
      const inbound = stringValue(checkpoint.interface);
      if (inbound) activeInterfaces.add(inbound);
      for (const outbound of arrayValue(checkpoint.out_interfaces).map(stringValue).filter(Boolean)) {
        activeInterfaces.add(outbound);
      }
    }
    scopedRows = scopedRows
      .filter((row) => activeInterfaces.has(stringValue(row.interface_id)))
      .map((row) => ({ ...row, run_id: runId }));
  }
  return filterByString(
    filterByString(scopedRows, "interface_id", params.interface_id ?? params.interface),
    "status",
    params.status,
  );
}

function filterByEnvelopeKind(rows, kind) {
  return filterByString(rows, "kind", kind);
}
