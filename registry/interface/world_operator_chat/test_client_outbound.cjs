const assert = require("node:assert/strict");
const crypto = require("node:crypto");
const { getEventListeners } = require("node:events");
const fs = require("node:fs");
const moduleApi = require("node:module");
const net = require("node:net");
const os = require("node:os");
const path = require("node:path");
const test = require("node:test");

const sourcePath = path.join(__dirname, "client.cjs");
const { readConfig, validateArtifactReadProjection } = require("./client/roots.cjs");
const { interfaceSocketPath, interfaceSocketIdentity } = require("./client/topologyPlan.cjs");
const { waitForInterfaceSocket } = require("./client/publication.cjs");
const { readLedgerOperatorRows } = require("./client/artifactBatches.cjs");
const { assertPayloadInputCycleReady, assertPreparedOperatorPayloadSelection, prepareOperatorPayloadArtifactBindings,
  selectedProviderDescriptorContent } = require("./client/operatorTurns.cjs");
const { operatorMessageRequestDigest, readOrCreateOperatorMessageIntent } = require("./client/messageAdmission.cjs");
const { assertProviderModelCatalogActiveRoute, providerOperationRequestCoordinates } = require("./client/providerRoutes.cjs");
const { existingRunSubmission } = require("./client/conversation.cjs");
const { exactOutboundRuntime } = require("./client/values.cjs");
const { ledgerChangeHubs } = require("./client/constants.cjs");
const { operatorChatLiveActivityPayload, operatorChatLiveEventEndsTurn } = require("./client/liveEvents.cjs");
const { providerAccountOperationError, providerAccountTurnError } = require("./client/providerObservations.cjs");
const { readOpenOperatorTurnStreamTail, selectOperatorTurnStreamEvents, streamIdentityHash } = require("./client/turnStreams.cjs");
const { watchLedgerChange } = require("./client/liveCursors.cjs");
const {
  assertGenericOperatorChatArtifactAllowed,
  loginProviderAccount,
  planOperatorChatSurface,
  readOperatorChatArtifacts,
  refreshProviderModels,
  releaseOperatorTaskWorkerEnvironments,
  submitOperatorTurn,
  submitOperatorTurnControl,
  validateProviderAccountObservation,
  verifyProviderAccount,
  withOperatorWorkspaceTopologyLock,
} = require(sourcePath);

const ACTIVE_RUN = "run_active_task";
const ACTIVE_CLIENT = "world_operator_chat_active";
const ACTIVE_PROVIDER = "world_provider_codex_active";
const ACTIVE_WORKER = "system_worker_coder_max_active";
const ACTIVE_WORKER_ROLE = "artifact-worker-role-active";
const SUMMARY_ARTIFACT = "interface.artifact.output:task-summary";
const CONTINUATION_ARTIFACT = "coder.worktree.snapshot:task-continuation";

test("full client configuration validates the explicit artifact projection contract", async () => {
  const canonical = await readConfig();
  assert.deepEqual(Array.from(canonical.artifact_read_projection.excluded_root_fields), ["session_context", "codex_continuity"]);
  assert.equal(canonical.artifact_read_projection.max_source_bytes, 268435456);
  for (const contract of [undefined, null, {}, { ...canonical.artifact_read_projection, schema_version: 2 },
    { ...canonical.artifact_read_projection, max_source_bytes: 0 }, { ...canonical.artifact_read_projection, max_projected_bytes: -1 },
    { ...canonical.artifact_read_projection, extra: true }, { ...canonical.artifact_read_projection, excluded_root_fields: ["x", "x"] },
    { ...canonical.artifact_read_projection, excluded_root_fields: [" x "] }, { ...canonical.artifact_read_projection, json_object_labels: undefined },
    { ...canonical.artifact_read_projection, json_object_labels: ["turn.stream"] }]) {
    assert.throws(() => validateArtifactReadProjection(contract), /artifact_read_projection/);
  }
});

test("operator message and turn-control labels cannot bypass their owned admission paths", async () => {
  for (const label of ["operator.message", "operator.turn.control"]) {
    await assert.rejects(
      assertGenericOperatorChatArtifactAllowed({
        interfaceId: "world_operator_chat_private",
        label,
      }),
      /requires its interface-owned admission path/,
    );
  }
  await assert.doesNotReject(assertGenericOperatorChatArtifactAllowed({
    interfaceId: "world_operator_chat_private",
    label: "operator.approval.response",
  }));
  await assert.doesNotReject(assertGenericOperatorChatArtifactAllowed({
    interfaceId: "world_microphone_private",
    label: "operator.message",
  }));
});

test("live operator activity settles on stop acknowledgement and interrupted-result closure", () => {
  const config = {
    artifact_labels: {
      terminal_output: "assistant.message",
      error_output: "assistant.error",
    },
  };
  assert.equal(operatorChatLiveEventEndsTurn({
    config,
    eventKind: "turn_control",
    label: "turn.control.acknowledged",
    payload: { action: "stop", status: "acknowledged" },
    willRetry: false,
  }), true);
  assert.equal(operatorChatLiveEventEndsTurn({
    config,
    eventKind: "interrupted_result",
    label: "turn.interrupted.result",
    payload: {},
    willRetry: false,
  }), true);
  assert.equal(operatorChatLiveEventEndsTurn({
    config,
    eventKind: "turn_control",
    label: "turn.control.failed",
    payload: { action: "stop", status: "failed" },
    willRetry: false,
  }), false);
});

test("native compaction projects as non-terminal activity", () => {
  const config = {
    artifact_labels: {
      terminal_output: "interface.artifact.output",
      error_output: "interface.artifact.error",
      tool_call: "tool.call",
    },
  };
  const payload = { id: "compact-one", type: "contextCompaction" };
  const activity = operatorChatLiveActivityPayload("turn.context.compaction", config, payload);
  assert.equal(activity.eventKind, "compaction");
  assert.equal(activity.text, "");
  assert.equal(operatorChatLiveActivityPayload("interface.artifact.output", config, { text: "Done." }).eventKind, "message");
  assert.equal(operatorChatLiveActivityPayload("interface.artifact.output", config, { text: "" }).text, "");
  assert.equal(operatorChatLiveEventEndsTurn({
    config,
    eventKind: activity.eventKind,
    label: "turn.context.compaction",
    payload,
    willRetry: false,
  }), false);
});

test("answer items retain their message meaning in the live transcript", () => {
  const config = { artifact_labels: { terminal_output: "interface.artifact.output", error_output: "interface.artifact.error", tool_call: "tool.call" } };
  for (const phase of ["partial_answer", "final_answer"]) {
    const payload = { phase, text: "The first result." };
    const activity = operatorChatLiveActivityPayload("turn.agent_message", config, payload);
    assert.equal(activity.eventKind, "message");
    assert.equal(activity.text, payload.text);
    assert.equal(operatorChatLiveEventEndsTurn({ config, eventKind: activity.eventKind, label: "turn.agent_message", payload, willRetry: false }), false);
  }
});

test("provider authentication recovery stays visible and non-terminal", () => {
  const config = {
    artifact_labels: {
      terminal_output: "interface.artifact.output",
      error_output: "interface.artifact.error",
      tool_call: "tool.call",
    },
  };
  for (const [label, eventKind] of [
    ["turn.auth_recovery.started", "auth_recovery_started"],
    ["turn.auth_recovery.completed", "auth_recovery_completed"],
  ]) {
    const payload = { provider: "example", message: "Refreshing authentication" };
    const activity = operatorChatLiveActivityPayload(label, config, payload);
    assert.equal(activity.eventKind, eventKind);
    assert.equal(activity.text, payload.message);
    assert.equal(operatorChatLiveEventEndsTurn({
      config,
      eventKind,
      label,
      payload,
      willRetry: false,
    }), false);
  }
});

test("live compaction batch stays on the active turn", async (t) => {
  const root = await fs.promises.mkdtemp(path.join("/tmp", "octopus-native-compaction-"));
  const runId = "run_native_compaction";
  const interfaceId = "world_operator_chat_nativecompaction";
  const providerInterfaceId = "world_provider_codex_nativecompaction";
  const runsRoot = path.join(root, "state", "runs");
  const interfaceRoot = path.join(root, "registry", "interface", interfaceId);
  const providerInterfaceRoot = path.join(root, "registry", "interface", providerInterfaceId);
  const ledgerRoot = path.join(root, "ledger");
  await Promise.all([runsRoot, interfaceRoot, providerInterfaceRoot, ledgerRoot]
    .map((directory) => fs.promises.mkdir(directory, { recursive: true })));
  const writeJson = (file, value) => fs.promises.writeFile(file, JSON.stringify(value));
  await Promise.all([
    writeJson(path.join(root, "host.json"), { host_manifest_version: 1, root_refs: { run_records_root: runsRoot } }),
    writeJson(path.join(interfaceRoot, "interface.json"), {
      id: interfaceId, interface_type: "world", role: "user", source_names: ["operator.chat"],
    }),
    writeJson(path.join(interfaceRoot, "octopus_os.json"), {
      interface: interfaceId, owner_interface: interfaceId, component: "client", workspace: "octopus_os",
    }),
    writeJson(path.join(providerInterfaceRoot, "interface.json"), {
      id: providerInterfaceId, interface_type: "world", role: "provider",
    }),
    writeJson(path.join(runsRoot, `${runId}.json`), {
      id: runId,
      ee: "ee_operator_space",
      worker: "system_worker_nativecompaction",
      trace: `trace:${runId}`,
      status: "running",
      checkpoints: [],
      active_checkpoints: [],
      interface_state: {
        [interfaceId]: { client_surface_role: "task", provider_runtime_interface: providerInterfaceId },
        [providerInterfaceId]: {},
      },
    }),
  ]);
  const contentHash = "b".repeat(64);
  await fs.promises.writeFile(path.join(ledgerRoot, "movements.jsonl"), `${JSON.stringify({
    seq: 0,
    lamport: 1,
    recorded_at_unix_ms: 1_700_000_000_000,
    trace: `trace:${runId}:compaction`,
    kind: "InterfaceOutbound",
    from: { Interface: providerInterfaceId },
    to: { Interface: interfaceId },
    artifact: { id: `turn.context.compaction:${contentHash}`, content: contentHash },
    invocation_metadata: { runtime: {
      run: runId, ee: "ee_operator_space", worker: "system_worker_nativecompaction",
      checkpoint: "cp_provider_output_nativecompaction", interface: interfaceId, epoch: 0,
    } },
  })}\n`);
  t.after(async () => fs.promises.rm(root, { recursive: true, force: true }));
  const batch = await readOperatorChatArtifacts({
    octopusHome: () => root,
    readArtifactPayload: async input => {
      assert.equal(input.projection.schema_version, 1);
      assert.equal(input.label, "turn.context.compaction");
      return { payload: { id: "compact-one", type: "contextCompaction" } };
    },
  }, {
    interfaceId, runId, afterSeq: -1, cursor: { offset: 0, seq: null }, waitMs: 1, limit: 16,
  });
  assert.equal(batch.artifacts.length, 1);
  assert.equal(batch.artifacts[0].event_kind, "compaction");
  assert.equal(batch.artifacts[0].text, "");
  assert.equal(batch.artifacts[0].event_data.text, undefined);
  assert.equal(batch.artifacts[0].event_data.continuation, undefined);
  assert.equal(batch.artifacts[0].terminal, false);
  const readerPath = path.join(__dirname, "../../../app/electron/contentAddressedArtifact.cjs");
  const { readContentAddressedArtifactPayload: raw, readContentAddressedArtifactProjection: projected } = require(readerPath);
  const blake3 = moduleApi.createRequire(readerPath)("@noble/hashes/blake3").blake3;
  const blobsRoot = path.join(ledgerRoot, "blobs"), ledger = path.join(ledgerRoot, "movements.jsonl");
  const base = JSON.parse((await fs.promises.readFile(ledger, "utf8")).trim());
  const decoded = new Map();
  const append = async (seq, label, bytes) => {
    await fs.promises.mkdir(blobsRoot, { recursive: true });
    const payloadHash = Buffer.from(blake3(bytes)).toString("hex");
    const manifest = Buffer.from(JSON.stringify({ label, payload_path: path.join(blobsRoot, payloadHash), payload_content_hash: payloadHash }));
    const hash = Buffer.from(blake3(manifest)).toString("hex");
    await fs.promises.writeFile(path.join(blobsRoot, payloadHash), bytes); await fs.promises.writeFile(path.join(blobsRoot, hash), manifest);
    await fs.promises.appendFile(ledger, `${JSON.stringify({ ...base, seq, lamport: seq + 1, artifact: { id: `${label}:${hash}`, content: hash } })}\n`);
  };
  for (const [index, [label, bytes]] of [["plain.note", Buffer.from("plain text")], ["binary.data", Buffer.from([255, 0, 1])],
    ["json.array", Buffer.from("[1,2]")], ["json.scalar", Buffer.from("3")]].entries()) await append(index + 1, label, bytes);
  const readOptions = { octopusHome: () => root, readArtifactPayload: async input => {
    const mode = Object.hasOwn(input, "projection");
    assert.equal(mode, input.label === "turn.context.compaction");
    const result = await (mode ? projected : raw)({ ...input, blobsRoot }); decoded.set(input.label, result.payload); return result;
  } };
  const rawBatch = await readOperatorChatArtifacts(readOptions, { interfaceId, runId, afterSeq: 0, cursor: { offset: 0, seq: null }, waitMs: 1, limit: 16 });
  assert.equal(rawBatch.artifacts.length, 4); assert.equal(decoded.get("plain.note").__raw, "plain text");
  assert.equal(decoded.get("binary.data").__binary, true); assert.deepEqual(decoded.get("json.array"), [1, 2]); assert.equal(decoded.get("json.scalar"), 3);
  await append(5, "turn.context.compaction", Buffer.from('{"private":['));
  await assert.rejects(readOperatorChatArtifacts(readOptions, { interfaceId, runId, afterSeq: 4, cursor: { offset: 0, seq: null }, waitMs: 1, limit: 16 }), /malformed/);
});

test("exact provider failure terminates the live turn with diagnostic evidence", async (t) => {
  const root = await fs.promises.mkdtemp(path.join("/tmp", "octopus-provider-turn-failure-"));
  const runId = "run_provider_failure";
  const interfaceId = "world_operator_chat_providerfailure";
  const providerInterfaceId = "world_provider_codex_providerfailure";
  const runsRoot = path.join(root, "state", "runs");
  const interfaceRoot = path.join(root, "registry", "interface", interfaceId);
  const providerInterfaceRoot = path.join(root, "registry", "interface", providerInterfaceId);
  const ledgerRoot = path.join(root, "ledger");
  await Promise.all([runsRoot, interfaceRoot, providerInterfaceRoot, ledgerRoot]
    .map((directory) => fs.promises.mkdir(directory, { recursive: true })));
  const writeJson = (file, value) => fs.promises.writeFile(file, JSON.stringify(value));
  await Promise.all([
    writeJson(path.join(root, "host.json"), { host_manifest_version: 1, root_refs: { run_records_root: runsRoot } }),
    writeJson(path.join(interfaceRoot, "interface.json"), {
      id: interfaceId, interface_type: "world", role: "user", source_names: ["operator.chat"],
    }),
    writeJson(path.join(interfaceRoot, "octopus_os.json"), {
      interface: interfaceId, owner_interface: interfaceId, component: "client", workspace: "octopus_os",
    }),
    writeJson(path.join(providerInterfaceRoot, "interface.json"), {
      id: providerInterfaceId, interface_type: "world", role: "provider",
    }),
    writeJson(path.join(runsRoot, `${runId}.json`), {
      id: runId,
      ee: "ee_operator_space",
      worker: "system_worker_providerfailure",
      trace: `trace:${runId}`,
      status: "idle",
      checkpoints: [],
      active_checkpoints: [],
      interface_state: {
        [interfaceId]: { client_surface_role: "task", provider_runtime_interface: providerInterfaceId },
        [providerInterfaceId]: {},
      },
    }),
  ]);
  const contentHash = "e".repeat(64);
  await fs.promises.writeFile(path.join(ledgerRoot, "movements.jsonl"), `${JSON.stringify({
    seq: 7,
    lamport: 8,
    recorded_at_unix_ms: 1_700_000_000_007,
    trace: `trace:${runId}:failure`,
    kind: "InterfaceOutbound",
    from: { Interface: providerInterfaceId },
    to: { Interface: interfaceId },
    artifact: { id: `${providerInterfaceId}.failure:${contentHash}`, content: contentHash },
    invocation_metadata: { runtime: {
      run: runId,
      ee: "ee_operator_space",
      worker: "system_worker_providerfailure",
      checkpoint: "cp_provider_client_failure",
      interface: interfaceId,
      epoch: 0,
    } },
  })}\n`);
  t.after(async () => fs.promises.rm(root, { recursive: true, force: true }));
  const batch = await readOperatorChatArtifacts({
    octopusHome: () => root,
    readArtifactPayload: async () => ({ payload: {
      source_movement_seq: 6,
      failure_class: "invalid_provider_context",
      details: { error: "frontier does not match the authored topology" },
    } }),
  }, {
    interfaceId, runId, afterSeq: -1, cursor: { offset: 0, seq: null }, waitMs: 1, limit: 16,
  });
  assert.equal(batch.artifacts.length, 1);
  assert.equal(batch.artifacts[0].event_kind, "error");
  assert.equal(batch.artifacts[0].error, true);
  assert.equal(batch.artifacts[0].terminal, true);
  assert.equal(batch.artifacts[0].event_data.failure_code, "invalid_provider_context");
  assert.equal(batch.artifacts[0].text, "frontier does not match the authored topology");
});

test("live turn.stream keeps one bounded cursor budget, wakes on nested append, and reads append tails linearly", async (t) => {
  const root = await fs.promises.mkdtemp(path.join("/tmp", "octopus-live-turn-stream-"));
  t.after(async () => fs.promises.rm(root, { recursive: true, force: true }));
  const runId = "run_live_stream_contract";
  const interfaceId = "world_operator_chat_livestream";
  const providerInterfaceId = "world_provider_codex_livestream";
  const runsRoot = path.join(root, "runs");
  const ledgerRoot = path.join(root, "ledger");
  const clientRoot = path.join(root, "registry", "interface", interfaceId);
  const providerRoot = path.join(root, "registry", "interface", providerInterfaceId);
  const streamsRoot = path.join(root, "interfaces", providerInterfaceId, "streams");
  await Promise.all([runsRoot, ledgerRoot, clientRoot, providerRoot, streamsRoot]
    .map((directory) => fs.promises.mkdir(directory, { recursive: true })));
  const writeJson = (file, value) => fs.promises.writeFile(file, `${JSON.stringify(value)}\n`);
  await Promise.all([
    writeJson(path.join(root, "host.json"), { host_manifest_version: 1, root_refs: { run_records_root: runsRoot } }),
    writeJson(path.join(clientRoot, "interface.json"), {
      id: interfaceId, interface_type: "world", role: "user", source_names: ["operator.chat"],
    }),
    writeJson(path.join(clientRoot, "octopus_os.json"), {
      interface: interfaceId, owner_interface: interfaceId, component: "client", workspace: "octopus_os",
    }),
    writeJson(path.join(providerRoot, "interface.json"), {
      id: providerInterfaceId, interface_type: "world", role: "provider",
    }),
    writeJson(path.join(runsRoot, `${runId}.json`), {
      id: runId, ee: "ee_live", worker: "system_worker_live", trace: `trace:${runId}`, status: "running",
      checkpoints: [], active_checkpoints: [],
      interface_state: {
        [interfaceId]: { client_surface_role: "task", provider_runtime_interface: providerInterfaceId },
        [providerInterfaceId]: {},
      },
    }),
  ]);
  const movementRows = [0, 1].map((seq) => ({
    seq,
    recorded_at_unix_ms: 1_700_000_000_100 + seq,
    trace: `trace:${runId}:${seq}`,
    kind: "InterfaceOutbound",
    from: { Interface: providerInterfaceId },
    to: { Interface: interfaceId },
    artifact: { id: `turn.reasoning:${String(seq + 1).repeat(64)}`, content: String(seq + 1).repeat(64) },
    invocation_metadata: { runtime: {
      run: runId, ee: "ee_live", worker: "system_worker_live", checkpoint: `cp_${seq}`,
      interface: interfaceId, epoch: 0,
    } },
  }));
  await fs.promises.writeFile(path.join(ledgerRoot, "movements.jsonl"), `${movementRows.map(JSON.stringify).join("\n")}\n`);
  const runStreamRoot = path.join(streamsRoot, streamIdentityHash([providerInterfaceId, runId]));
  const threadId = "thread_live";
  const turnId = "turn_live";
  const turnRoot = path.join(runStreamRoot, streamIdentityHash([threadId, turnId]));
  await fs.promises.mkdir(turnRoot, { recursive: true });
  const bodyPath = path.join(turnRoot, "turn-stream.body.jsonl");
  const manifestPath = path.join(turnRoot, "turn-stream.artifact.json");
  const header = {
    type: "header", schema_version: 1, seq: 0, provider_interface: providerInterfaceId,
    run: runId, thread_id: threadId, turn_id: turnId,
    opened_source_movement_seq: 0, opened_at_unix_ms: 1_700_000_000_000,
  };
  const eventRow = (seq) => ({
    type: "event", seq, source_movement_seq: seq - 1,
    recorded_at_unix_ms: 1_700_000_000_000 + seq,
    label: "agent.message.delta", payload: { text: `stream-${seq}` },
  });
  await fs.promises.writeFile(bodyPath, `${JSON.stringify(header)}\n${JSON.stringify(eventRow(1))}\n${JSON.stringify(eventRow(2))}\n`);
  await writeJson(manifestPath, { label: "turn.stream", payload_path: bodyPath });
  const options = {
    octopusHome: () => root,
    readArtifactPayload: async (input) => {
      if (input.label === "turn.stream") {
        const payloadBytes = await fs.promises.readFile(bodyPath);
        return { payload: { __raw: payloadBytes.toString("utf8") }, payloadBytes };
      }
      return { payload: { summary: input.label }, payloadBytes: Buffer.alloc(0) };
    },
  };
  const first = await readOperatorChatArtifacts(options, {
    interfaceId, runId, afterSeq: -1, cursor: { offset: 0, seq: null }, waitMs: 1, limit: 2,
  });
  assert.equal(first.artifacts.length, 2);
  const second = await readOperatorChatArtifacts(options, {
    interfaceId, runId, afterSeq: -1, cursor: first.cursor, waitMs: 1, limit: 2,
  });
  assert.equal(second.artifacts.length, 2);
  assert.deepEqual(
    [...first.artifacts, ...second.artifacts].map((artifact) => artifact.source).sort(),
    ["movement", "movement", "stream", "stream"],
  );
  const independent = await readOperatorChatArtifacts(options, {
    interfaceId, runId, afterSeq: -1, waitMs: 1, limit: 2,
  });
  assert.deepEqual(independent.artifacts, first.artifacts,
    "another observer must receive the call start instead of inheriting the first observer's cursor");
  const wakeStarted = Date.now();
  const waiting = readOperatorChatArtifacts(options, {
    interfaceId, runId, afterSeq: -1, cursor: second.cursor, waitMs: 2_000, limit: 2,
  });
  setTimeout(() => {
    void fs.promises.appendFile(bodyPath, `${JSON.stringify(eventRow(3))}\n`);
  }, 50);
  const awakened = await waiting;
  assert.equal(awakened.artifacts.length, 1);
  assert.equal(awakened.artifacts[0].text, "stream-3");
  assert.ok(Date.now() - wakeStarted < 1_000);

  let streamCursor = awakened.cursor.stream;
  let totalReadBytes = 0;
  const headerBytes = Buffer.byteLength(`${JSON.stringify(header)}\n`);
  for (let seq = 4; seq <= 36; seq += 1) {
    const row = `${JSON.stringify(eventRow(seq))}\n`;
    await fs.promises.appendFile(bodyPath, row);
    const parsed = await readOpenOperatorTurnStreamTail(bodyPath, streamCursor, { providerInterfaceId, runId });
    totalReadBytes += parsed.readBytes;
    const selected = selectOperatorTurnStreamEvents(parsed, streamCursor, 1);
    assert.equal(selected.events.length, 1);
    assert.equal(selected.events[0].seq, seq);
    assert.ok(parsed.readBytes <= headerBytes + Buffer.byteLength(row));
    streamCursor = selected.cursor;
  }
  assert.ok(totalReadBytes < headerBytes * 40 + 32 * 1024);

  const terminalHash = "f".repeat(64);
  await fs.promises.appendFile(bodyPath, [
    JSON.stringify({
      type: "semantic_boundary", seq: 37, source_movement_seq: 36,
      recorded_at_unix_ms: 1_700_000_000_037,
      label: "turn.completed", artifact_payload_blake3: terminalHash,
    }),
    JSON.stringify({
      type: "seal", seq: 38, source_movement_seq: 36,
      recorded_at_unix_ms: 1_700_000_000_038,
      terminal_label: "turn.completed", terminal_artifact_payload_blake3: terminalHash,
    }),
  ].join("\n") + "\n");
  await writeJson(manifestPath, {
    label: "turn.stream", payload_path: bodyPath, payload_content_hash: "d".repeat(64),
  });
  await fs.promises.appendFile(path.join(ledgerRoot, "movements.jsonl"), `${JSON.stringify({
    seq: 2,
    recorded_at_unix_ms: 1_700_000_000_039,
    trace: `trace:${runId}:2`,
    kind: "InterfaceOutbound",
    from: { Interface: providerInterfaceId },
    to: { Interface: interfaceId },
    artifact: { id: `turn.stream:${"d".repeat(64)}`, content: "d".repeat(64) },
    invocation_metadata: { runtime: {
      run: runId, ee: "ee_live", worker: "system_worker_live", checkpoint: "cp_stream",
      interface: interfaceId, epoch: 0,
    } },
  })}\n`);

  const nextThreadId = "thread_live_next";
  const nextTurnId = "turn_live_next";
  const nextTurnRoot = path.join(runStreamRoot, streamIdentityHash([nextThreadId, nextTurnId]));
  await fs.promises.mkdir(nextTurnRoot, { recursive: true });
  const nextBodyPath = path.join(nextTurnRoot, "turn-stream.body.jsonl");
  const nextManifestPath = path.join(nextTurnRoot, "turn-stream.artifact.json");
  const nextHeader = {
    ...header,
    thread_id: nextThreadId,
    turn_id: nextTurnId,
    opened_source_movement_seq: 3,
    opened_at_unix_ms: 1_700_000_000_040,
  };
  const nextEvent = {
    type: "event", seq: 1, source_movement_seq: 3,
    recorded_at_unix_ms: 1_700_000_000_041,
    label: "agent.message.delta", payload: { text: "next-turn" },
  };
  await fs.promises.writeFile(nextBodyPath, `${JSON.stringify(nextHeader)}\n${JSON.stringify(nextEvent)}\n`);
  await writeJson(nextManifestPath, { label: "turn.stream", payload_path: nextBodyPath });

  const transitionCursor = { ...awakened.cursor, stream: streamCursor };
  const sealed = await readOperatorChatArtifacts(options, {
    interfaceId, runId, afterSeq: -1, cursor: transitionCursor, waitMs: 1, limit: 2,
  });
  assert.deepEqual(sealed.artifacts, []);
  assert.equal(sealed.cursor.seq, 3);
  assert.equal(sealed.cursor.stream.stream_id, streamCursor.stream_id);
  const next = await readOperatorChatArtifacts(options, {
    interfaceId, runId, afterSeq: -1, cursor: sealed.cursor, waitMs: 1, limit: 2,
  });
  assert.deepEqual(next.artifacts.map((artifact) => artifact.text), ["next-turn"]);
  assert.equal(Object.keys(next.cursor.streams).length, 2);
  assert.equal(next.cursor.streams[streamCursor.stream_id].sealed, true);

  const orphanRoot = path.join(runStreamRoot, streamIdentityHash([threadId, "interrupted-old"]));
  await fs.promises.mkdir(orphanRoot);
  const orphanBody = path.join(orphanRoot, "turn-stream.body.jsonl");
  await fs.promises.writeFile(orphanBody, `${JSON.stringify({ ...header, turn_id: "interrupted-old" })}\n${JSON.stringify(eventRow(1))}\n`);
  await writeJson(path.join(orphanRoot, "turn-stream.artifact.json"), { label: "turn.stream", payload_path: orphanBody });
  let resumedCursor = JSON.parse(JSON.stringify(next.cursor));
  const observed = [];
  for (let index = 0; index < 2; index += 1) {
    const batch = await readOperatorChatArtifacts(options, {
      interfaceId, runId, afterSeq: -1, cursor: resumedCursor, waitMs: 1, limit: 1,
    });
    observed.push(...batch.artifacts);
    resumedCursor = JSON.parse(JSON.stringify(batch.cursor));
  }
  assert.deepEqual(observed.map((artifact) => artifact.text), ["stream-1"]);
  assert.ok(resumedCursor.streams);
  await fs.promises.appendFile(orphanBody, `${JSON.stringify(eventRow(2))}\n`);
  await fs.promises.appendFile(nextBodyPath, `${JSON.stringify({ ...nextEvent, seq: 2, payload: { text: "current-continues" } })}\n`);
  const continued = [];
  for (let index = 0; index < 3; index += 1) {
    const batch = await readOperatorChatArtifacts(options, { interfaceId, runId, afterSeq: -1, cursor: resumedCursor, waitMs: 1, limit: 1 });
    continued.push(...batch.artifacts);
    resumedCursor = JSON.parse(JSON.stringify(batch.cursor));
  }
  assert.deepEqual(continued.map((artifact) => artifact.text).sort(), ["current-continues", "stream-2"]);
  assert.equal(new Set(continued.map((artifact) => artifact.stream_id)).size, 2);
  assert.ok(continued.every((artifact) => !artifact.terminal));
});

test("turn control uses the exact operator socket without a projection read", async (t) => {
  const root = await fs.promises.mkdtemp(path.join("/tmp", "octopus-turn-control-"));
  const runId = "run_operator_chat_controltest";
  const interfaceId = "world_operator_chat_controltest";
  const providerInterfaceId = "world_provider_codex_controltest";
  const runsRoot = path.join(root, "state", "runs");
  const interfaceRoot = path.join(root, "registry", "interface", interfaceId);
  const providerInterfaceRoot = path.join(root, "registry", "interface", providerInterfaceId);
  const ledgerRoot = path.join(root, "ledger");
  await Promise.all([runsRoot, interfaceRoot, providerInterfaceRoot, ledgerRoot]
    .map((directory) => fs.promises.mkdir(directory, { recursive: true })));
  const writeJson = (file, value) => fs.promises.writeFile(file, JSON.stringify(value));
  await Promise.all([
    writeJson(path.join(root, "host.json"), { host_manifest_version: 1, root_refs: { run_records_root: runsRoot } }),
    writeJson(path.join(runsRoot, `${runId}.json`), {
      id: runId, ee: "ee_operator_space", worker: "system_worker_controltest", trace: `trace:${runId}`, status: "running",
      checkpoints: [], active_checkpoints: [], interface_state: {
        [interfaceId]: { client_surface_role: "task", provider_runtime_interface: providerInterfaceId },
        [providerInterfaceId]: {},
      },
    }),
    writeJson(path.join(interfaceRoot, "interface.json"), {
      id: interfaceId, interface_type: "world", role: "user", source_names: ["operator.chat"],
    }),
    writeJson(path.join(interfaceRoot, "octopus_os.json"), {
      interface: interfaceId, owner_interface: interfaceId, component: "client", workspace: "octopus_os",
    }),
    writeJson(path.join(providerInterfaceRoot, "interface.json"), {
      id: providerInterfaceId, interface_type: "world", role: "provider",
    }),
    fs.promises.writeFile(path.join(ledgerRoot, "movements.jsonl"), ""),
  ]);
  const server = await listenForInterface(root, interfaceId);
  t.after(async () => { await closeServer(server); await fs.promises.rm(root, { recursive: true, force: true }); });
  let handed = null;
  await assert.rejects(submitOperatorTurnControl({ octopusHome: () => root }, {
    interfaceId, runId, action: "pause", controlId: "control_test_00", timeoutMs: 1_000,
  }), /must be 'interrupt', 'resume', or 'stop'/);
  const outcome = await submitOperatorTurnControl({
    octopusHome: () => root,
    clientArtifactsRoot: () => path.join(root, "interface-hand-in", interfaceId, "client"),
    handInArtifact: async (input) => {
      handed = input;
      return { admitted: { seq: 41 } };
    },
  }, {
    interfaceId, runId, action: "stop", controlId: "control_test_01", timeoutMs: 1_000,
  });
  assert.equal(outcome.verdict.admitted.seq, 41);
  assert.equal(handed.interfaceId, interfaceId);
  assert.match(handed.endpoint, /^unix:\/\//);
  assert.deepEqual(JSON.parse(await fs.promises.readFile(handed.manifest.payload_path, "utf8")), { action: "stop", control_id: "control_test_01" });
  const artifactHash = "a".repeat(64);
  await fs.promises.appendFile(path.join(ledgerRoot, "movements.jsonl"), `${JSON.stringify({
    seq: 42, recorded_at_unix_ms: 1_700_000_000_000, kind: "InterfaceOutbound",
    from: { Interface: providerInterfaceId },
    to: { Interface: interfaceId },
    artifact: { id: `turn.dynamic_tool_call:${artifactHash}`, content: artifactHash },
    invocation_metadata: { runtime: {
      run: runId, ee: "ee_operator_space", worker: "system_worker_controltest",
      checkpoint: "cp_provider_output_controltest", interface: interfaceId, epoch: 0,
    } },
  })}\n`);
  const live = await readOperatorChatArtifacts({
    octopusHome: () => root,
    readArtifactPayload: async () => ({ payload: {
      call_id: "call_controltest", status: "started", tool: "workspace.read_file__controltest",
    } }),
  }, {
    interfaceId, runId, afterSeq: -1, cursor: { offset: 0, seq: null }, waitMs: 1, limit: 16,
  });
  assert.deepEqual([live.artifacts[0].from_interface_id, live.artifacts[0].from_interface_role], [providerInterfaceId, "provider"]);
});

function authoredBinding(interfaceId) {
  return {
    interface: interfaceId,
    config: { artifact_policy: { mode: "all" } },
  };
}

async function installFakeTopologyTransport(installationRoot, handler = 'throw new Error("unsupported fake topology mode");') {
  const executable = path.join(installationRoot, "target", "debug", "octopus-operator-chat-topology");
  await fs.promises.mkdir(path.dirname(executable), { recursive: true });
  await fs.promises.writeFile(executable, `#!/usr/bin/env node
const assert = require("node:assert/strict"), crypto = require("node:crypto");
const fs = require("node:fs"), path = require("node:path");
let input = "";
process.stdin.setEncoding("utf8");
process.stdin.on("data", chunk => { input += chunk; });
process.stdin.on("end", () => {
  const request = JSON.parse(input);
  if (process.argv[2] === "--publish-files") {
    assert.deepEqual(Object.keys(request), ["writes"]);
    assert.ok(Array.isArray(request.writes) && request.writes.length <= 1024 && Buffer.byteLength(input) <= 64 * 1024 * 1024);
    for (const write of request.writes) {
      assert.deepEqual(Object.keys(write).sort(), ["content", "expected_before_hash", "operation_id", "path"]);
      assert.ok(typeof write.path === "string" && path.isAbsolute(write.path));
      assert.ok(typeof write.operation_id === "string" && write.operation_id.length > 0);
      assert.equal(typeof write.content, "string");
      assert.ok(write.expected_before_hash === null || (typeof write.expected_before_hash === "string" && /^[0-9a-f]{64}$/.test(write.expected_before_hash)));
    }
    const hash = bytes => crypto.createHash("sha256").update(bytes).digest("hex");
    const result = { receipts: [], error: null };
    for (const write of request.writes) {
      const receipt = { digest_algorithm: "sha256", operation_id: write.operation_id, path: write.path,
        before_hash: write.expected_before_hash, after_hash: hash(write.content),
        bytes: Buffer.byteLength(write.content), outcome: "applied", error: null };
      try {
        const before = fs.existsSync(write.path) ? hash(fs.readFileSync(write.path)) : null;
        if (before !== write.expected_before_hash) {
          result.receipts.push({ ...receipt, outcome: "not_applied", error: "fixture preimage changed" });
          break;
        }
        fs.mkdirSync(path.dirname(write.path), { recursive: true });
        fs.writeFileSync(write.path, write.content);
        result.receipts.push(receipt);
      } catch (error) { result.error = error.message; break; }
    }
    process.stdout.write(JSON.stringify(result)); return;
  }
  ${handler}
});
`);
  await fs.promises.chmod(executable, 0o755);
  return executable;
}

async function writeAuthoringFixtureJson(file, value) {
  await fs.promises.mkdir(path.dirname(file), { recursive: true });
  await fs.promises.writeFile(file, JSON.stringify(value));
}

async function createAuthoringFixture(root, eeId = "ee_operator_space") {
  const installationRoot = path.join(root, "installation");
  const registryRoot = path.join(root, "registry");
  const runsRoot = path.join(root, "runs");
  const eeInstancesRoot = path.join(root, "ee-overlay");
  const ledgerRoot = path.join(root, "ledger");
  const capturePath = path.join(root, "planner-request.json");
  await Promise.all([
    fs.promises.mkdir(path.join(registryRoot, "interface"), { recursive: true }),
    fs.promises.mkdir(path.join(registryRoot, "tool"), { recursive: true }),
    fs.promises.mkdir(runsRoot, { recursive: true }),
    fs.promises.mkdir(eeInstancesRoot, { recursive: true }),
    fs.promises.mkdir(ledgerRoot, { recursive: true }),
  ]);
  await fs.promises.writeFile(path.join(root, "host.json"), JSON.stringify({
    host_manifest_version: 1,
    root_refs: { run_records_root: runsRoot, workspace_root: root },
  }));
  const room = JSON.parse(await fs.promises.readFile(path.join(__dirname, "../system_workspace_env/interface.json"), "utf8"));
  room.id = "fixture_room";
  await writeAuthoringFixtureJson(path.join(registryRoot, "interface/fixture_room/interface.json"), room);
  await writeAuthoringFixtureJson(path.join(registryRoot, "interface/fixture_room/life.json"), JSON.parse(await fs.promises.readFile(path.join(__dirname, "../system_workspace_env/life.json"), "utf8")));
  await writeAuthoringFixtureJson(path.join(registryRoot, "worker_environment_kind/filesystem_workspace/worker_environment_kind.json"), JSON.parse(await fs.promises.readFile(path.join(__dirname, "../../worker_environment_kind/filesystem_workspace/worker_environment_kind.json"), "utf8")));
  const fixtureTool = JSON.parse(await fs.promises.readFile(path.join(__dirname, "../../tool/workspace.read_file/tool.json"), "utf8"));
  fixtureTool.interface = "fixture_room";
  await fs.promises.writeFile(path.join(eeInstancesRoot, `${eeId}.json`), `${JSON.stringify({
    ee_id: eeId,
    template_name: "project",
    parent_ee_id: "ee_operator_space",
    goal: "Test Project",
    config: {},
  }, null, 2)}\n`);
  const ledgerBytes = "{}\n";
  await fs.promises.writeFile(path.join(ledgerRoot, "movements.jsonl"), ledgerBytes);
  await installFakeTopologyTransport(installationRoot, `
  if (!fs.existsSync(path.join(request.ee_policy_root, \`\${request.ee_id}.json\`))) {
    process.stderr.write(\`planner missing EE '\${request.ee_id}'\`);
    process.exit(2);
  }
  const runId = \`run_operator_chat_\${request.chat_id}\`;
  const chatInterfaceId = \`world_operator_chat_\${request.chat_id}\`;
  const modelInterfaceId = \`\${request.provider_interface_id}_\${request.chat_id}\`;
  const workerInterfaceId = \`\${request.worker_interface_id}_\${request.chat_id}\`;
  const authoringSourceId = \`system_interface_authoring_\${request.chat_id}\`;
  const authoringEnvironmentId = \`system_room_authoring_\${request.chat_id}\`;
  const authoringToolId = \`file.write__authoring_\${request.chat_id}\`;
  const profile = {
    ee_id: request.ee_id,
    worker_choice_interface: request.worker_interface_id,
    door_host_slots: request.door_host_slots,
    selected_worker_environment_kinds: request.selected_worker_environment_kinds,
  };
  const run = {
    id: runId,
    ee: request.ee_id,
    worker: workerInterfaceId,
    trace: \`trace:\${runId}\`,
    status: "idle",
    checkpoints: [],
    active_checkpoints: [],
    interface_state: {
      [chatInterfaceId]: {
        client_surface_role: request.client_surface_role,
        provider_runtime_interface: modelInterfaceId,
        provider_choice_interface: request.provider_interface_id,
        worker_choice_interface: request.worker_interface_id,
      },
      [modelInterfaceId]: {},
    },
  };
  const requiredInterfaces = [
    chatInterfaceId,
    modelInterfaceId,
    workerInterfaceId,
    authoringSourceId,
    authoringEnvironmentId,
  ].sort();
  const writes = [
    {
      path: path.join(request.registry_root, "interface", chatInterfaceId, "interface.json"),
      content: "{}\\n",
      tag: \`interface:\${chatInterfaceId}\`,
      expected_before_hash: null,
    },
    {
      path: path.join(request.registry_root, "tool", authoringToolId, "tool.json"),
      content: JSON.stringify({ ...${JSON.stringify(fixtureTool)}, id: authoringToolId }) + "\\n",
      tag: \`tool:\${authoringToolId}\`,
      expected_before_hash: null,
    },
  ];
  fs.writeFileSync(path.join(path.dirname(request.registry_root), "planner-request.json"), input);
  process.stdout.write(JSON.stringify({
    run_id: runId,
    chat_interface_id: chatInterfaceId,
    model_interface_id: modelInterfaceId,
    worker_interface_id: workerInterfaceId,
    client_surface_role: request.client_surface_role,
    model: request.model,
    reasoning_effort: request.reasoning_effort || "",
    expected_before_hash_algorithm: request.write_hash_algorithm,
    authoring_profile: profile,
    required_interface_ids: requiredInterfaces,
    required_tool_ids: [authoringToolId],
    interface_writes: writes,
    run_writes: [],
    run_record_content: JSON.stringify(run, null, 2) + "\\n",
  }));
`);
  return {
    capturePath,
    eeId,
    eeInstancesRoot,
    ledgerBytes,
    ledgerRoot,
    registryRoot,
    root,
    runsRoot,
    options: {
      octopusHome: () => root,
      installationRoot: () => installationRoot,
      registryRoot: () => registryRoot,
      eeInstancesRoot: () => eeInstancesRoot,
    },
  };
}

function newTaskRequest(eeId) {
  return {
    providerInterfaceId: "world_provider_codex",
    providerModel: "gpt-5.6-sol",
    eeId,
    workerInterfaceId: "system_worker_coder_max",
    clientSurfaceRole: "task",
    doorHostSlots: {},
    selectedWorkerEnvironmentKinds: ["git_worktree"],
    text: "Start the task.",
    attachments: [],
    timeoutMs: 3_000,
  };
}

async function writeActiveProviderRegistry(registryRoot) {
  const providerRoot = path.join(registryRoot, "interface", ACTIVE_PROVIDER);
  await fs.promises.mkdir(providerRoot, { recursive: true });
  await fs.promises.writeFile(path.join(providerRoot, "interface.json"), JSON.stringify({
    id: ACTIVE_PROVIDER,
    interface_type: "world",
    role: "provider",
    interface_driver_config: {
      private_config: { model: "gpt-5.6-sol", reasoning_effort: "high" },
    },
  }));
}

function newTaskOptions(fixture, handed) {
  return {
    ...fixture.options,
    clientArtifactsRoot: (interfaceId) => path.join(fixture.root, "hand-in", interfaceId, "client"),
    handInArtifact: async (input) => {
      handed.push(input);
      return { admitted: { seq: 1 } };
    },
  };
}

async function projectDeleteThroughTopologyGate(fixture) {
  return withOperatorWorkspaceTopologyLock(fixture.root, async () => {
    for (const entry of await fs.promises.readdir(fixture.runsRoot, { withFileTypes: true })) {
      if (!entry.isFile() || !entry.name.endsWith(".json")) continue;
      const run = JSON.parse(await fs.promises.readFile(path.join(fixture.runsRoot, entry.name), "utf8"));
      if (run.ee === fixture.eeId) {
        throw new Error(`Project EE '${fixture.eeId}' is still referenced by run '${String(run.id)}'`);
      }
    }
    await fs.promises.unlink(path.join(fixture.eeInstancesRoot, `${fixture.eeId}.json`));
  });
}

async function waitForRunRecord(runsRoot) {
  const deadline = Date.now() + 3_000;
  while (Date.now() < deadline) {
    const entries = (await fs.promises.readdir(runsRoot)).filter((entry) => entry.endsWith(".json"));
    if (entries.length === 1) return path.join(runsRoot, entries[0]);
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
  throw new Error("new task run was not committed before the test deadline");
}

async function waitForDirectoryEntry(directory) {
  const deadline = Date.now() + 3_000;
  while (Date.now() < deadline) {
    const entries = await fs.promises.readdir(directory).catch((error) => {
      if (error?.code === "ENOENT") return [];
      throw error;
    });
    if (entries.length) return;
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
  throw new Error(`directory '${directory}' stayed empty before the test deadline`);
}

async function listenForInterface(root, interfaceId) {
  const controlRoot = path.join(root, "app-server-control");
  await fs.promises.mkdir(controlRoot, { recursive: true });
  const socketPath = path.join(
    controlRoot,
    `${crypto.createHash("sha256").update(interfaceId).digest("hex").slice(0, 24)}.sock`,
  );
  const server = net.createServer((socket) => socket.destroy());
  let serverListening = true;
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(socketPath, resolve);
  });
  return server;
}

function closeServer(server) {
  return new Promise((resolve) => server.close(resolve));
}

test("interface socket replacement wait refuses a live prior listener until its generation changes", async (t) => {
  const root = await fs.promises.mkdtemp("/tmp/octo-socket-");
  const wait = waitForInterfaceSocket;
  assert.equal(await interfaceSocketIdentity(root, ACTIVE_CLIENT), null);
  let server = await listenForInterface(root, ACTIVE_CLIENT);
  t.after(async () => { await closeServer(server); await fs.promises.rm(root, { recursive: true, force: true }); });
  assert.equal(server.address(), interfaceSocketPath(root, ACTIVE_CLIENT));
  const previous = await interfaceSocketIdentity(root, ACTIVE_CLIENT);
  assert.equal(typeof previous, "string");
  await wait(root, ACTIVE_CLIENT, 1_000);
  await assert.rejects(wait(root, ACTIVE_CLIENT, 100, previous));
  let released = false;
  const replacement = wait(root, ACTIVE_CLIENT, 2_000, previous).then(
    () => { released = true; }, error => error,
  );
  await fs.promises.chmod(interfaceSocketPath(root, ACTIVE_CLIENT), 0o600);
  assert.equal(await interfaceSocketIdentity(root, ACTIVE_CLIENT), previous);
  await wait(root, ACTIVE_CLIENT, 1_000);
  assert.equal(released, false);
  await closeServer(server);
  server = await listenForInterface(root, ACTIVE_CLIENT);
  assert.notEqual(await interfaceSocketIdentity(root, ACTIVE_CLIENT), previous);
  assert.equal(await replacement, undefined);
  assert.equal(released, true);
  await wait(root, ACTIVE_CLIENT, 1_000);
});

test("workspace topology authoring is serialized per Octopus home", async () => {
  const root = await fs.promises.mkdtemp(path.join("/tmp", "octopus-topology-lock-"));
  let releaseFirst;
  const firstMayFinish = new Promise((resolve) => { releaseFirst = resolve; });
  const entered = [];
  const first = withOperatorWorkspaceTopologyLock(root, async () => {
    entered.push("first");
    await firstMayFinish;
    entered.push("first-finished");
  });
  await new Promise((resolve) => setImmediate(resolve));
  const second = withOperatorWorkspaceTopologyLock(root, async () => {
    entered.push("second");
  });
  await new Promise((resolve) => setImmediate(resolve));
  assert.deepEqual(entered, ["first"]);
  releaseFirst();
  await Promise.all([first, second]);
  assert.deepEqual(entered, ["first", "first-finished", "second"]);
  await fs.promises.rm(root, { recursive: true, force: true });
});

function activeRun() {
  const carriedCheckpoint = {
    id: "cp_active_project_result",
    run: ACTIVE_RUN,
    epoch: 0,
    interface_binding: authoredBinding("system_project_result_octopus"),
    outbound_interface_bindings: [authoredBinding("system_worker_home")],
    artifact_bindings: [],
  };
  const cycleCheckpoints = [
    {
      id: "cp_active_client_input",
      run: ACTIVE_RUN,
      epoch: 0,
      interface_binding: authoredBinding(ACTIVE_CLIENT),
      outbound_interface_bindings: [authoredBinding(ACTIVE_PROVIDER)],
      artifact_bindings: [{ id: SUMMARY_ARTIFACT }, { id: CONTINUATION_ARTIFACT }],
    },
    {
      id: "cp_active_client_control",
      run: ACTIVE_RUN,
      epoch: 0,
      interface_binding: authoredBinding(ACTIVE_CLIENT),
      outbound_interface_bindings: [authoredBinding(ACTIVE_PROVIDER)],
      artifact_bindings: [],
    },
    {
      id: "cp_active_provider_output",
      run: ACTIVE_RUN,
      epoch: 0,
      interface_binding: authoredBinding(ACTIVE_PROVIDER),
      outbound_interface_bindings: [authoredBinding(ACTIVE_CLIENT)],
      artifact_bindings: [],
    },
  ];
  cycleCheckpoints[1].interface_binding.config.artifact_policy = { mode: "allow", names: ["operator.turn.control"], namespaces: [] };
  cycleCheckpoints[1].outbound_interface_bindings[0].config.artifact_policy = { mode: "allow", names: ["operator.turn.control"], namespaces: [] };
  const checkpoints = [carriedCheckpoint, ...cycleCheckpoints];
  const cycleIds = cycleCheckpoints.map((checkpoint) => checkpoint.id);
  return {
    id: ACTIVE_RUN,
    ee: "ee_operator_interaction",
    worker: ACTIVE_WORKER,
    trace: `trace:${ACTIVE_RUN}`,
    status: "recorded",
    checkpoints,
    active_checkpoints: [carriedCheckpoint.id, ...cycleIds],
    interface_state: {
      [ACTIVE_CLIENT]: {
        schema_version: 1,
        client_surface_role: "task",
        provider_choice_interface: "world_provider_codex",
        provider_runtime_interface: ACTIVE_PROVIDER,
        worker_choice_interface: "system_worker_coder_max",
        ledger_offset: 0,
        conversation_artifact_bindings: [
          { id: SUMMARY_ARTIFACT },
          { id: CONTINUATION_ARTIFACT },
        ],
        pinned_artifact_bindings: [],
        pinned_context_items: [],
        cycle_checkpoint_ids: cycleIds,
      },
      [ACTIVE_PROVIDER]: {
        context_generation: 0,
        authoring_generation: 0,
        frontier_checkpoint_ids: cycleIds,
        run_authoring_binding: {
          schema_version: 2,
          run: ACTIVE_RUN,
          worker: ACTIVE_WORKER,
          provider_interface: ACTIVE_PROVIDER,
          authoring_source_interface: "system_interface_authoring_active",
          checkpoints: [{ name: "operator_input" }, {
            name: "provider_output", role: "provider_artifact_output",
            interface_binding: authoredBinding(ACTIVE_PROVIDER),
            outbound_interface_bindings: [authoredBinding(ACTIVE_CLIENT)],
            required_verified_artifacts: [],
          }],
        },
      },
    },
  };
}

function activeRunRequest() {
  return {
    providerChoiceInterfaceId: "world_provider_codex",
    clientSurfaceRole: "task",
    eeId: "",
    workerInterfaceId: "",
  };
}

function runtime(overrides = {}) {
  return {
    run: "run_operator_chat",
    ee: "ee_operator_chat",
    worker: "system_worker_operator_chat",
    checkpoint: "cp_operator_chat",
    interface: "world_operator_chat_exact",
    epoch: 0,
    ...overrides,
  };
}

function movement(overrides = {}) {
  return {
    kind: "InterfaceOutbound",
    from: { Interface: "world_provider_test" },
    to: { Interface: "world_operator_chat_exact" },
    invocation_metadata: { runtime: runtime() },
    ...overrides,
  };
}

test("provider model request proof uses interface endpoints and exact run provenance", async () => {
  const label = "provider.models.list", hash = "a".repeat(64), metadata = runtime();
  const row = { seq: 243, kind: "InterfaceArrival", from: { Interface: metadata.interface }, to: { Interface: metadata.worker },
    artifact: { id: `${label}:${hash}`, content: hash }, invocation_metadata: { runtime: metadata } };
  const read = async () => ({ label, manifestHash: hash, payloadHash: "b".repeat(64), payload: {} });
  const prove = (overrides) => providerOperationRequestCoordinates({ ...row, ...overrides },
    { runId: metadata.run, interfaceId: metadata.interface }, label, {}, read, "provider model catalog request");
  assert.equal((await prove({})).requestMovementSeq, 243);
  for (const overrides of [
    { from: { Interface: "world_other" } },
    { to: { Run: metadata.run } },
    { to: { Interface: "system_worker_other" } },
    { invocation_metadata: null },
    ...["run", "interface", "worker"].map(field => ({ invocation_metadata: { runtime: { ...metadata, [field]: "other" } } })),
  ]) await assert.rejects(prove(overrides), /Home\/run coordinates|runtime provenance|invocation_metadata/);
  await assert.rejects(prove({ artifact: { ...row.artifact, content: "c".repeat(64) } }), /identity does not match/);
});

test("operator chat accepts only its exact canonical outbound", () => {
  assert.equal(
    exactOutboundRuntime(movement(), "world_operator_chat_exact").run,
    "run_operator_chat",
  );
  assert.equal(
    exactOutboundRuntime(
      movement({
        to: { Interface: "world_other" },
        invocation_metadata: { runtime: 7 },
      }),
      "world_operator_chat_exact",
    ),
    null,
  );
  assert.equal(
    exactOutboundRuntime(
      movement({
        from: { Run: "run_legacy" },
        invocation_metadata: { runtime: 7 },
      }),
      "world_operator_chat_exact",
    ),
    null,
  );
});

test("operator chat fails closed on exact-target runtime drift", () => {
  assert.throws(
    () => exactOutboundRuntime(
      movement({ invocation_metadata: { runtime: { interface: "world_operator_chat_exact" } } }),
      "world_operator_chat_exact",
    ),
    /movement runtime\.run/,
  );
  assert.throws(
    () => exactOutboundRuntime(
      movement({ invocation_metadata: { runtime: runtime({ interface: "world_other" }) } }),
      "world_operator_chat_exact",
    ),
    /exact target/,
  );
});

test("operator chat shares one ledger watcher across concurrent waits", async (t) => {
  const root = await fs.promises.mkdtemp(path.join("/tmp", "octopus-ledger-watch-"));
  const movementsPath = path.join(root, "movements.jsonl");
  await fs.promises.writeFile(movementsPath, "");
  const waits = Array.from({ length: 12 }, () => watchLedgerChange(movementsPath, 2_000));
  t.after(async () => {
    for (const wait of waits) wait.cancel();
    await fs.promises.rm(root, { recursive: true, force: true });
  });

  const hub = ledgerChangeHubs.get(movementsPath);
  assert.ok(hub);
  assert.equal(hub.waiters.size, 12);
  assert.equal(getEventListeners(hub.statWatcher, "change").length, 1);

  await fs.promises.appendFile(movementsPath, "{}\n");
  assert.deepEqual(await Promise.all(waits.map((wait) => wait.promise)), new Array(12).fill(true));
  assert.equal(ledgerChangeHubs.has(movementsPath), false);
});

test("ordinary existing run reuses its provider-authored exact client input", () => {
  const ordinary = activeRun();
  const providerState = ordinary.interface_state[ACTIVE_PROVIDER];
  delete providerState.run_authoring_binding;
  providerState.authoring_generation = 0;
  const submission = existingRunSubmission(
    { run: { interface_prefix: "world_operator_chat_" }, artifact_labels: { message: "operator.message" } },
    ACTIVE_RUN,
    ordinary,
    ACTIVE_CLIENT,
    activeRunRequest(),
  );
  assert.deepEqual(JSON.parse(JSON.stringify(submission)), {
    modelInterfaceId: ACTIVE_PROVIDER,
    workerInterfaceId: ACTIVE_WORKER,
    clientSurfaceRole: "task",
  });
});

test("existing private provider selection adopts exact formed models without importing template drift", () => {
  const current = {
    id: ACTIVE_PROVIDER,
    interface_policy: {
      variables: { models: { reduce: "intersection", default: ["gpt-5.6-sol"] } },
      values: {},
    },
    interface_driver_config: {
      supported_reasoning_efforts: ["high"],
      transport: { family: "current-private-transport" },
      private_config: {
        model: "gpt-5.6-sol",
        reasoning_effort: "high",
        model_catalog: [{ model: "gpt-5.6-sol" }],
        task_private_state: "preserve-me",
      },
    },
  };
  const planned = {
    id: ACTIVE_PROVIDER,
    interface_policy: {
      variables: {
        models: {
          reduce: "intersection",
          default: ["gpt-5.6-sol", "gpt-5.6-terra"],
        },
      },
      values: { models: ["gpt-5.6-sol", "gpt-5.6-terra"] },
    },
    interface_driver_config: {
      supported_reasoning_efforts: ["low", "high", "ultra"],
      transport: { family: "template-drift-must-not-enter" },
      private_config: {
        model: "gpt-5.6-terra",
        reasoning_effort: "ultra",
        model_catalog: [
          { model: "gpt-5.6-sol" },
          { model: "gpt-5.6-terra" },
        ],
        task_private_state: "template-drift-must-not-enter",
      },
    },
  };

  const selected = JSON.parse(selectedProviderDescriptorContent(
    `${JSON.stringify(current)}\n`,
    `${JSON.stringify(planned)}\n`,
    ACTIVE_PROVIDER,
    "gpt-5.6-terra",
    "ultra",
  ));
  assert.deepEqual(selected.interface_policy, planned.interface_policy);
  assert.deepEqual(
    selected.interface_driver_config.supported_reasoning_efforts,
    planned.interface_driver_config.supported_reasoning_efforts,
  );
  assert.deepEqual(
    selected.interface_driver_config.private_config.model_catalog,
    planned.interface_driver_config.private_config.model_catalog,
  );
  assert.equal(selected.interface_driver_config.private_config.model, "gpt-5.6-terra");
  assert.equal(selected.interface_driver_config.private_config.reasoning_effort, "ultra");
  assert.equal(selected.interface_driver_config.private_config.task_private_state, "preserve-me");
  assert.deepEqual(selected.interface_driver_config.transport, current.interface_driver_config.transport);
});

test("provider catalog uses authored output ownership after a label-specific reply", () => {
  const context = { requestLabel: "provider.models.list", outputLabel: "provider.models.catalog",
    operationErrorLabel: "provider.operation.error", turnErrorLabel: "turn.error" };
  const check = (run) => assertProviderModelCatalogActiveRoute(run, ACTIVE_RUN, ACTIVE_CLIENT, ACTIVE_PROVIDER, context);
  const run = activeRun();
  run.checkpoints.find((cp) => cp.id === "cp_active_provider_output").interface_binding.config.artifact_policy = {
    mode: "allow", names: ["interface.artifact.output"], namespaces: [],
  };
  assert.doesNotThrow(() => check(run));
  for (const mutate of [
    (r) => { delete r.interface_state[ACTIVE_PROVIDER].run_authoring_binding; },
    (r) => { r.interface_state[ACTIVE_PROVIDER].run_authoring_binding.run = "other"; },
    (r) => { r.interface_state[ACTIVE_PROVIDER].run_authoring_binding.provider_interface = "other"; },
    (r) => { r.interface_state[ACTIVE_PROVIDER].run_authoring_binding.checkpoints.pop(); },
    (r) => { const cps = r.interface_state[ACTIVE_PROVIDER].run_authoring_binding.checkpoints; cps.push(cps.at(-1)); },
    (r) => { r.interface_state[ACTIVE_PROVIDER].run_authoring_binding.checkpoints.at(-1).outbound_interface_bindings = [authoredBinding("other")]; },
    (r) => { r.interface_state[ACTIVE_PROVIDER].run_authoring_binding.checkpoints.at(-1).interface_binding = authoredBinding("other"); },
    (r) => { r.interface_state[ACTIVE_PROVIDER].run_authoring_binding.checkpoints.at(-1).outbound_interface_bindings[0].config.artifact_policy = { mode: "allow", names: ["turn.error"], namespaces: [] }; },
    (r) => { r.active_checkpoints = r.active_checkpoints.filter((id) => id !== "cp_active_client_input"); },
    (r) => { r.checkpoints.find((cp) => cp.id === "cp_active_client_input").outbound_interface_bindings = [authoredBinding("other")]; },
  ]) {
    const invalid = structuredClone(run); mutate(invalid);
    assert.throws(() => check(invalid));
  }
});

async function createProviderLoginFixture(root, scenario = {}) {
  const runsRoot = path.join(root, "runs");
  const ledgerPath = path.join(root, "ledger", "movements.jsonl");
  const homeRoot = path.join(root, "registry", "interface", ACTIVE_CLIENT);
  const providerRoot = path.join(root, "registry", "interface", ACTIVE_PROVIDER);
  await Promise.all([
    fs.promises.mkdir(runsRoot, { recursive: true }),
    fs.promises.mkdir(path.dirname(ledgerPath), { recursive: true }),
    fs.promises.mkdir(homeRoot, { recursive: true }),
    fs.promises.mkdir(providerRoot, { recursive: true }),
  ]);
  await fs.promises.writeFile(path.join(root, "host.json"), JSON.stringify({
    host_manifest_version: 1,
    root_refs: { run_records_root: runsRoot },
  }));
  const run = activeRun();
  run.interface_state[ACTIVE_CLIENT].client_surface_role = "home";
  await fs.promises.writeFile(path.join(runsRoot, `${ACTIVE_RUN}.json`), JSON.stringify(run));
  await fs.promises.writeFile(path.join(homeRoot, "interface.json"), JSON.stringify({
    id: ACTIVE_CLIENT, interface_type: "world", role: "user", direction: "bidirectional", source_names: ["operator.chat"],
  }));
  await fs.promises.writeFile(path.join(homeRoot, "octopus_os.json"), JSON.stringify({
    interface: ACTIVE_CLIENT, owner_interface: ACTIVE_CLIENT, component: "client", workspace: "octopus_os",
  }));
  const projectionLabels = [
    "provider.account.snapshot",
    "provider.operation.error",
    "turn.error",
    "provider.account.login.started",
    "provider.account.login.status",
    "provider.account.login.completed",
    "provider.account.updated",
  ];
  await fs.promises.writeFile(path.join(providerRoot, "interface.json"), JSON.stringify({
    id: ACTIVE_PROVIDER,
    interface_type: "world",
    role: "provider",
    interface_driver_config: { private_config: {
      provider_operations: {
        error_output_label: "provider.operation.error",
        account_login_completed_output_label: "provider.account.login.completed",
        account_updated_output_label: "provider.account.updated",
        account_login_start: {
          request_label: "provider.account.login.start",
          output_label: "provider.account.login.started",
        },
        account_login_status_read: {
          request_label: "provider.account.login.status.read",
          output_label: "provider.account.login.status",
        },
        account_read: { request_label: "provider.account.read", output_label: "provider.account.snapshot" },
      },
      client_projection_output_labels: projectionLabels,
    } },
  }));
  await fs.promises.writeFile(ledgerPath, "");
  const artifacts = new Map();
  const submissions = [];
  const opened = [];
  let nextSeq = 80;
  let statusPoll = 0;
  const artifact = (label, payload) => {
    const manifestHash = crypto.createHash("sha256").update(`${nextSeq}:${label}:${JSON.stringify(payload)}`).digest("hex");
    const artifactId = `${label}:${manifestHash}`;
    artifacts.set(artifactId, {
      label,
      manifestHash,
      payload,
      payloadHash: crypto.createHash("sha256").update(JSON.stringify(payload)).digest("hex"),
    });
    return { id: artifactId, content: manifestHash };
  };
  const outputMovement = (label, payload) => ({
    seq: nextSeq++,
    kind: "InterfaceOutbound",
    from: { Interface: ACTIVE_PROVIDER },
    to: { Interface: ACTIVE_CLIENT },
    artifact: artifact(label, payload),
    recorded_at_unix_ms: 1700000000000 + nextSeq,
    invocation_metadata: { runtime: runtime({ run: ACTIVE_RUN, worker: ACTIVE_WORKER, checkpoint: "cp_active_provider_output", interface: ACTIVE_CLIENT }) },
  });
  const submitArtifact = async (request) => {
    submissions.push({ label: request.label, payload: request.payload });
    const requestSeq = nextSeq++;
    const rows = [{
      seq: requestSeq,
      kind: "InterfaceArrival",
      from: { Interface: ACTIVE_CLIENT },
      to: { Interface: ACTIVE_WORKER },
      artifact: artifact(request.label, request.payload),
      invocation_metadata: { runtime: runtime({ run: ACTIVE_RUN, worker: ACTIVE_WORKER, checkpoint: "cp_active_client_input", interface: ACTIVE_CLIENT }) },
    }];
    if (request.label === "provider.account.login.start") {
      const started = scenario.startedPayload ?? {
        operation: "provider.account.login.start",
        method: "chatgpt_browser",
        status: "pending",
        login_id: "login-exact-01",
        auth_url: "https://auth.example.test/login?attempt=1",
      };
      const startOutputs = scenario.startOutputs ?? [["provider.account.login.started", started]];
      rows.push(...startOutputs.map(([label, payload]) => outputMovement(label, payload)));
    } else if (request.label === "provider.account.login.status.read") {
      statusPoll += 1;
      const defaultOutputs = [
        ["provider.account.login.completed", { login_id: "login-exact-01", status: "completed", success: true, failure: null }],
        ["provider.account.updated", { auth_mode: "chatgpt" }],
        ["provider.account.login.status", {
          operation: "provider.account.login.status.read",
          login_id: "login-exact-01",
          state: { status: "completed", success: true },
        }],
      ];
      const statusOutputs = typeof scenario.statusOutputs === "function"
        ? scenario.statusOutputs(statusPoll)
        : scenario.statusOutputs ?? defaultOutputs;
      rows.push(...statusOutputs.map(([label, payload]) => outputMovement(label, payload)));
    } else if (request.label === "provider.account.read") {
      rows.push(outputMovement("provider.account.snapshot", {
        account: { type: "chatgpt", email: "private@example.test" },
        requiresOpenaiAuth: false,
      }));
    } else {
      throw new Error(`unexpected provider test request '${request.label}'`);
    }
    await fs.promises.appendFile(ledgerPath, `${rows.map(JSON.stringify).join("\n")}\n`);
    return {
      interfaceId: request.interfaceId,
      runId: request.runId,
      label: request.label,
      contentHash: crypto.createHash("sha256").update(JSON.stringify(request.payload)).digest("hex"),
      verdict: { admitted: { seq: requestSeq } },
    };
  };
  return {
    coordinates: {
      operationId: "setup_login_01",
      runId: ACTIVE_RUN,
      interfaceId: ACTIVE_CLIENT,
      providerInterfaceId: "world_provider_codex",
    },
    opened,
    submissions,
    statusPolls: () => statusPoll,
    options: {
      octopusHome: () => root,
      submitArtifact,
      openExternalUrl: async (url) => { opened.push(url); },
      readArtifactPayload: async ({ artifactId }) => artifacts.get(artifactId),
    },
  };
}

test("model refresh observes its generation, waits for rearm, and works without account operations", { timeout: 4_000 }, async (t) => {
  const root = await fs.promises.mkdtemp(path.join("/tmp", "octopus-catalog-recovery-"));
  t.after(() => fs.promises.rm(root, { recursive: true, force: true }));
  const fixture = await createProviderLoginFixture(root);
  const runPath = path.join(root, "runs", `${ACTIVE_RUN}.json`), ledgerPath = path.join(root, "ledger", "movements.jsonl");
  const descriptorPath = path.join(root, "registry", "interface", ACTIVE_PROVIDER, "interface.json");
  const descriptor = JSON.parse(await fs.promises.readFile(descriptorPath, "utf8"));
  Object.assign(descriptor.interface_driver_config.private_config, {
    model: "model", model_catalog_operation: { request_label: "provider.models.list", output_label: "provider.models.catalog" },
    client_projection_output_labels: ["provider.models.catalog", "provider.operation.error", "turn.error"],
  });
  await fs.promises.writeFile(descriptorPath, JSON.stringify(descriptor));
  const artifacts = new Map(); let seq = 70, submissions = 0, allowSubmit = false, settled = false;
  let requestRead, outputRead;
  const requestSeen = new Promise(resolve => { requestRead = resolve; });
  const outputSeen = new Promise(resolve => { outputRead = resolve; });
  const artifact = (label, payload) => {
    const hash = crypto.createHash("sha256").update(`${seq}:${label}:${JSON.stringify(payload)}`).digest("hex"), id = `${label}:${hash}`;
    artifacts.set(id, { label, payload, manifestHash: hash, payloadHash: crypto.createHash("sha256").update(JSON.stringify(payload)).digest("hex") });
    return { id, content: hash };
  };
  const metadata = epoch => ({ runtime: runtime({ run: ACTIVE_RUN, ee: "ee_operator_interaction", worker: ACTIVE_WORKER,
    checkpoint: "cp_active_client_input", interface: ACTIVE_CLIENT, epoch }),
    checkpoint_admission: { artifact_label: "provider.models.list", complete: true, required_artifact_labels: [] }, outbound_interface_bindings: [ACTIVE_PROVIDER] });
  const append = rows => fs.promises.appendFile(ledgerPath, rows.map(JSON.stringify).join("\n") + "\n");
  const admit = async epoch => {
    const ref = artifact("provider.models.list", {}), admittedSeq = seq++;
    await append([{ seq: admittedSeq, kind: "InterfaceArrival", from: { Interface: ACTIVE_CLIENT }, to: { Interface: ACTIVE_WORKER }, artifact: ref, invocation_metadata: metadata(epoch) },
      { seq: seq++, kind: "InterfaceOutbound", from: { Interface: ACTIVE_CLIENT }, to: { Interface: ACTIVE_PROVIDER }, artifact: ref, invocation_metadata: { ...metadata(epoch), runtime: { ...metadata(epoch).runtime, interface: ACTIVE_PROVIDER } } }]);
    return admittedSeq;
  };
  const output = displayName => append([{ seq: seq++, kind: "InterfaceOutbound", from: { Interface: ACTIVE_PROVIDER }, to: { Interface: ACTIVE_CLIENT },
    artifact: artifact("provider.models.catalog", { label: "provider.models.catalog", next_cursor: null,
      models: [{ model: "model", displayName, defaultReasoningEffort: "high", supportedReasoningEfforts: [{ reasoningEffort: "high" }] }] }),
    recorded_at_unix_ms: 1700000000000 + seq, invocation_metadata: { runtime: runtime({ run: ACTIVE_RUN, worker: ACTIVE_WORKER, interface: ACTIVE_CLIENT }) } }]);
  const rearm = async epoch => {
    const run = JSON.parse(await fs.promises.readFile(runPath, "utf8"));
    run.checkpoints.find(cp => cp.id === "cp_active_client_input").epoch = epoch;
    await fs.promises.writeFile(runPath, JSON.stringify(run));
  };
  const options = { ...fixture.options, readArtifactPayload: async ({ artifactId, label }) => {
    if (label === "provider.models.list") requestRead(); else outputRead();
    return artifacts.get(artifactId);
  }, submitArtifact: async request => {
    assert.equal(allowSubmit, true, "an admitted operation must not be resubmitted"); submissions += 1;
    const admittedSeq = await admit(1); await rearm(2); await output("Fresh");
    return { ...request, contentHash: crypto.createHash("sha256").update(JSON.stringify(request.payload)).digest("hex"), verdict: { admitted: { seq: admittedSeq } } };
  } };
  const request = { runId: ACTIVE_RUN, interfaceId: ACTIVE_CLIENT, providerInterfaceId: "world_provider_codex", timeoutMs: 1_000 };
  await admit(0);
  const pending = refreshProviderModels(options, request).then(value => { settled = true; return value; });
  await Promise.race([requestSeen, pending.then(() => assert.fail("pending operation returned before observation"))]);
  assert.equal(submissions, 0); await output("Recovered"); await outputSeen;
  await new Promise(resolve => setImmediate(resolve)); assert.equal(settled, false);
  await rearm(1); assert.equal((await pending).models[0].displayName, "Recovered");
  assert.deepEqual((await pending).models[0].supportedReasoningEfforts, ["high"]);
  delete descriptor.interface_driver_config.private_config.provider_operations;
  await fs.promises.writeFile(descriptorPath, JSON.stringify(descriptor));
  allowSubmit = true;
  assert.equal((await refreshProviderModels(options, request)).models[0].displayName, "Fresh");
  assert.equal(submissions, 1);
});

test("provider account proof crosses exact Home, redacts identity, advances its cursor, and revalidates", async (t) => {
  const root = await fs.promises.mkdtemp(path.join("/tmp", "octopus-provider-account-"));
  t.after(() => fs.promises.rm(root, { recursive: true, force: true }));
  const runsRoot = path.join(root, "runs");
  const ledgerPath = path.join(root, "ledger", "movements.jsonl");
  const homeRoot = path.join(root, "registry", "interface", ACTIVE_CLIENT);
  const providerRoot = path.join(root, "registry", "interface", ACTIVE_PROVIDER);
  await Promise.all([
    fs.promises.mkdir(runsRoot, { recursive: true }),
    fs.promises.mkdir(path.dirname(ledgerPath), { recursive: true }),
    fs.promises.mkdir(homeRoot, { recursive: true }),
    fs.promises.mkdir(providerRoot, { recursive: true }),
  ]);
  await fs.promises.writeFile(path.join(root, "host.json"), JSON.stringify({
    host_manifest_version: 1,
    root_refs: { run_records_root: runsRoot },
  }));
  const run = activeRun();
  run.interface_state[ACTIVE_CLIENT].client_surface_role = "home";
  await fs.promises.writeFile(path.join(runsRoot, `${ACTIVE_RUN}.json`), JSON.stringify(run));
  await fs.promises.writeFile(path.join(homeRoot, "interface.json"), JSON.stringify({
    id: ACTIVE_CLIENT, interface_type: "world", role: "user", direction: "bidirectional", source_names: ["operator.chat"],
  }));
  await fs.promises.writeFile(path.join(homeRoot, "octopus_os.json"), JSON.stringify({
    interface: ACTIVE_CLIENT, owner_interface: ACTIVE_CLIENT, component: "client", workspace: "octopus_os",
  }));
  await fs.promises.writeFile(path.join(providerRoot, "interface.json"), JSON.stringify({
    id: ACTIVE_PROVIDER,
    interface_type: "world",
    role: "provider",
    interface_driver_config: { private_config: {
      provider_operations: {
        error_output_label: "provider.operation.error",
        account_read: { request_label: "provider.account.read", output_label: "provider.account.snapshot" },
      },
      client_projection_output_labels: ["provider.account.snapshot", "provider.operation.error", "turn.error"],
    } },
  }));
  await fs.promises.writeFile(ledgerPath, "");
  const requestHash = "a".repeat(64);
  const snapshotHash = "b".repeat(64);
  const payloadHashes = { "provider.account.read": "c".repeat(64), "provider.account.snapshot": "d".repeat(64) };
  const payloads = {
    "provider.account.read": { refresh_token: true },
    "provider.account.snapshot": { account: { type: "chatgpt", email: "private@example.test", planType: "pro" }, requiresOpenaiAuth: true },
  };
  const readArtifactPayload = async ({ label, manifestHash }) => ({
    label, manifestHash, payloadHash: payloadHashes[label], payload: payloads[label],
  });
  const options = {
    octopusHome: () => root,
    readArtifactPayload,
    submitArtifact: async (request) => {
      assert.deepEqual(request.payload, { refresh_token: true });
      assert.equal(Object.hasOwn(request.payload, "operationId"), false);
      const rows = [
        {
          seq: 70, kind: "InterfaceArrival", from: { Interface: ACTIVE_CLIENT }, to: { Interface: ACTIVE_WORKER },
          artifact: { id: `provider.account.read:${requestHash}`, content: requestHash },
          invocation_metadata: { runtime: runtime({ run: ACTIVE_RUN, worker: ACTIVE_WORKER, checkpoint: "cp_active_client_input", interface: ACTIVE_CLIENT }) },
        },
        {
          seq: 71, kind: "InterfaceOutbound", from: { Interface: ACTIVE_PROVIDER }, to: { Interface: ACTIVE_CLIENT },
          artifact: { id: `provider.account.snapshot:${snapshotHash}`, content: snapshotHash }, recorded_at_unix_ms: 1700000000000,
          invocation_metadata: { runtime: runtime({ run: ACTIVE_RUN, worker: ACTIVE_WORKER, checkpoint: "cp_active_provider_output", interface: ACTIVE_CLIENT }) },
        },
      ];
      await fs.promises.appendFile(ledgerPath, `${rows.map(JSON.stringify).join("\n")}\n`);
      return {
        interfaceId: request.interfaceId, runId: request.runId, label: request.label,
        contentHash: crypto.createHash("sha256").update(JSON.stringify(request.payload)).digest("hex"),
        verdict: { admitted: { seq: 70 } },
      };
    },
  };
  const coordinates = { operationId: "setup_account_01", runId: ACTIVE_RUN, interfaceId: ACTIVE_CLIENT, providerInterfaceId: "world_provider_codex" };
  const evidence = await verifyProviderAccount(options, { ...coordinates, timeoutMs: 1_000 });
  assert.equal(evidence.runtime_provider_interface_id, ACTIVE_PROVIDER);
  assert.equal(JSON.stringify(evidence).includes("private@example.test"), false);
  assert.deepEqual(await validateProviderAccountObservation(options, { ...coordinates, evidence }), evidence);
  for (const requiresOpenaiAuth of [false, "true", "false", null, undefined, 0, {}]) {
    const variedOptions = { ...options, readArtifactPayload: async (input) => ({
      ...(await readArtifactPayload(input)),
      payload: input.label === "provider.account.snapshot"
        ? { ...payloads[input.label], requiresOpenaiAuth }
        : payloads[input.label],
    }) };
    if (requiresOpenaiAuth === false) {
      assert.deepEqual(await validateProviderAccountObservation(variedOptions, { ...coordinates, evidence }), evidence);
    } else {
      await assert.rejects(
        validateProviderAccountObservation(variedOptions, { ...coordinates, evidence }),
        (error) => error.code === "provider_account_snapshot_malformed" && error.retryable === false && error.repairable === false,
      );
    }
  }
  const replayed = await readOperatorChatArtifacts(options, { runId: ACTIVE_RUN, interfaceId: ACTIVE_CLIENT, afterSeq: -1, waitMs: 1, limit: 16 });
  assert.deepEqual(replayed.artifacts.map((artifact) => artifact.label), ["provider.account.snapshot"]);
  assert.equal((await readOperatorChatArtifacts(options, { runId: ACTIVE_RUN, interfaceId: ACTIVE_CLIENT, afterSeq: -1, cursor: replayed.cursor, waitMs: 1, limit: 16 })).artifacts.length, 0);
  await assert.rejects(
    validateProviderAccountObservation(options, { ...coordinates, operationId: "setup_account_02", evidence }),
    /expected setup coordinates/,
  );
  await assert.rejects(
    validateProviderAccountObservation(options, { ...coordinates, evidence: { ...evidence, snapshot_manifest_hash: "e".repeat(64) } }),
    /durable ledger observation/,
  );
  await assert.rejects(
    validateProviderAccountObservation({ ...options, readArtifactPayload: async (input) => ({ ...(await readArtifactPayload(input)), payload: input.label === "provider.account.snapshot" ? { account: null, requiresOpenaiAuth: true } : payloads[input.label] }) }, { ...coordinates, evidence }),
    (error) => error.code === "provider_account_auth_required" && error.retryable === true && error.repairable === true,
  );
});

test("ordinary provider browser sign-in uses exact Home, safe URL callback, matching status, and final account proof", async (t) => {
  const root = await fs.promises.mkdtemp(path.join("/tmp", "octopus-provider-login-"));
  t.after(() => fs.promises.rm(root, { recursive: true, force: true }));
  const fixture = await createProviderLoginFixture(root, {
    statusOutputs: (poll) => poll === 1
      ? [["provider.account.login.status", {
          operation: "provider.account.login.status.read",
          login_id: "login-exact-01",
          state: { status: "pending", method: "chatgpt_browser" },
        }]]
      : [
          ["provider.account.login.completed", { login_id: "login-exact-01", status: "completed", success: true, failure: null }],
          ["provider.account.updated", { auth_mode: "chatgpt" }],
          ["provider.account.login.status", {
            operation: "provider.account.login.status.read",
            login_id: "login-exact-01",
            state: { status: "completed", success: true },
          }],
        ],
  });
  const result = await loginProviderAccount(fixture.options, {
    ...fixture.coordinates,
    timeoutMs: 1_000,
    statusPollMs: 5,
  });
  assert.deepEqual(result, { status: "completed" });
  assert.deepEqual(fixture.opened, ["https://auth.example.test/login?attempt=1"]);
  assert.equal(fixture.statusPolls(), 2);
  assert.deepEqual(fixture.submissions.slice(0, 2), [
    {
      label: "provider.account.login.start",
      payload: { method: "chatgpt_browser", codex_streamlined_login: true },
    },
    {
      label: "provider.account.login.status.read",
      payload: { login_id: "login-exact-01" },
    },
  ]);
  const evidence = await verifyProviderAccount(fixture.options, {
    ...fixture.coordinates,
    timeoutMs: 1_000,
  });
  assert.equal(evidence.runtime_provider_interface_id, ACTIVE_PROVIDER);
  assert.equal(fixture.submissions.at(-1).label, "provider.account.read");
  assert.equal(JSON.stringify(evidence).includes("private@example.test"), false);
});

test("provider browser sign-in fails closed on malformed, unsafe, foreign, failed, and uncompleted observations", async (t) => {
  const cases = [
    {
      name: "malformed started payload",
      scenario: {
        startedPayload: {
          operation: "provider.account.login.start",
          method: "chatgpt_browser",
          status: "pending",
          login_id: "login-exact-01",
        },
      },
      verify: (error) => error.code === "provider_account_login_payload_malformed",
    },
    {
      name: "unsafe browser URL",
      scenario: {
        startedPayload: {
          operation: "provider.account.login.start",
          method: "chatgpt_browser",
          status: "pending",
          login_id: "login-exact-01",
          auth_url: "http://auth.example.test/login",
        },
      },
      verify: (error) => error.code === "provider_account_login_url_invalid",
    },
    {
      name: "mismatched login identity",
      scenario: {
        statusOutputs: [
          ["provider.account.login.completed", { login_id: "login-other", status: "completed", success: true, failure: null }],
          ["provider.account.login.status", {
            operation: "provider.account.login.status.read",
            login_id: "login-exact-01",
            state: { status: "completed", success: true },
          }],
        ],
      },
      verify: (error) => error.code === "provider_account_login_id_mismatch",
    },
    {
      name: "failed login",
      scenario: {
        statusOutputs: [
          ["provider.account.login.completed", {
            login_id: "login-exact-01",
            status: "failed",
            success: false,
            failure: { code: "upstream_login_failed", redacted: true },
          }],
          ["provider.account.login.status", {
            operation: "provider.account.login.status.read",
            login_id: "login-exact-01",
            state: { status: "completed", success: false },
          }],
        ],
      },
      verify: (error) => error.code === "provider_account_login_failed" && error.retryable === true,
    },
    {
      name: "missing exact completion observation",
      scenario: {
        statusOutputs: [["provider.account.login.status", {
          operation: "provider.account.login.status.read",
          login_id: "login-exact-01",
          state: { status: "completed", success: true },
        }]],
      },
      verify: (error) => error.code === "provider_account_login_completion_missing",
    },
    {
      name: "unrelated Home output",
      scenario: { startOutputs: [["interface.artifact.output", { text: "not login" }]] },
      verify: (error) => /unrelated Home output/.test(error.message),
    },
    {
      name: "bounded pending timeout",
      scenario: {
        statusOutputs: () => [["provider.account.login.status", {
          operation: "provider.account.login.status.read",
          login_id: "login-exact-01",
          state: { status: "pending", method: "chatgpt_browser" },
        }]],
      },
      timeoutMs: 30,
      verify: (error) => error.code === "provider_account_login_timeout" && error.retryable === true,
    },
  ];
  for (const item of cases) {
    await t.test(item.name, async () => {
      const root = await fs.promises.mkdtemp(path.join("/tmp", "octopus-provider-login-refusal-"));
      try {
        const fixture = await createProviderLoginFixture(root, item.scenario);
        await assert.rejects(
          loginProviderAccount(fixture.options, {
            ...fixture.coordinates,
            timeoutMs: item.timeoutMs ?? 1_000,
            statusPollMs: 5,
          }),
          item.verify,
        );
        if (item.name === "unsafe browser URL" || item.name === "malformed started payload" || item.name === "unrelated Home output") {
          assert.equal(fixture.opened.length, 0);
        }
        const cursorPath = path.join(root, "interfaces", ACTIVE_CLIENT, "ledger-cursor.client.json");
        assert.equal(fs.existsSync(cursorPath), true);
      } finally {
        await fs.promises.rm(root, { recursive: true, force: true });
      }
    });
  }
});

test("provider account typed failures repair only explicit authentication evidence", () => {
  const upstream = providerAccountOperationError({
    operation: "provider.account.read",
    status: "error",
    error: { code: "upstream_transport_failed", category: "upstream", retryable: true },
  }, "provider.account.read");
  assert.deepEqual(
    { code: upstream.code, retryable: upstream.retryable, repairable: upstream.repairable },
    { code: "provider_account_operation_error", retryable: true, repairable: false },
  );
  const authentication = providerAccountTurnError({
    failure_code: "provider.authentication.invalid",
    will_retry: false,
    message: "private upstream detail",
  });
  assert.deepEqual(
    { code: authentication.code, retryable: authentication.retryable, repairable: authentication.repairable },
    { code: "provider_account_auth_invalid", retryable: true, repairable: true },
  );
  assert.equal(authentication.message.includes("private upstream detail"), false);
  for (const failure_code of ["provider_quota_exhausted", "provider_rate_limited", "provider_overloaded"]) {
    const nonAuth = providerAccountTurnError({ failure_code, will_retry: false });
    assert.notEqual(nonAuth?.repairable, true, failure_code);
  }
});

test("author-only surface planning returns validated topology without committing or handing in", async (t) => {
  const root = await fs.promises.mkdtemp(path.join("/tmp", "octopus-author-only-plan-"));
  t.after(async () => {
    await fs.promises.rm(root, { recursive: true, force: true });
  });
  const fixture = await createAuthoringFixture(root);

  const request = {
    chatId: "homeauthoring000000000001",
    cycle: "homecycle0000000000000001",
    providerInterfaceId: "world_provider_codex",
    providerModel: "gpt-5.6-sol",
    eeId: "ee_operator_space",
    workerInterfaceId: "system_worker_coder_max",
    clientSurfaceRole: "home",
    doorHostSlots: {
      system_coder_env: { repo_root: root },
    },
    selectedWorkerEnvironmentKinds: ["git_worktree"],
  };
  const plan = await planOperatorChatSurface(fixture.options, request);

  assert.equal(plan.runId, `run_operator_chat_${request.chatId}`);
  assert.equal(plan.clientSurfaceRole, "home");
  assert.equal(plan.authoringProfile.ee_id, request.eeId);
  assert.equal(plan.authoringProfile.worker_choice_interface, request.workerInterfaceId);
  assert.ok(plan.requiredInterfaceIds.includes(plan.chatInterfaceId));
  assert.ok(plan.requiredInterfaceIds.includes(plan.modelInterfaceId));
  assert.ok(plan.requiredInterfaceIds.includes(plan.workerInterfaceId));
  assert.equal(plan.requiredToolIds.length, 1);
  assert.equal(plan.ledgerOffset, Buffer.byteLength(fixture.ledgerBytes));
  assert.equal(plan.writes.length, 3);
  for (const write of plan.writes) {
    assert.equal(fs.existsSync(write.filePath), false);
  }
  assert.equal(
    await fs.promises.readFile(path.join(fixture.ledgerRoot, "movements.jsonl"), "utf8"),
    fixture.ledgerBytes,
  );
  const plannerRequest = JSON.parse(await fs.promises.readFile(fixture.capturePath, "utf8"));
  assert.equal(plannerRequest.ee_policy_root, fixture.eeInstancesRoot);
  assert.equal(plannerRequest.ee_instances_root, path.join(root, "state", "ee_instances"));
  assert.equal(plannerRequest.existing_run_record, null);
  assert.equal(Object.hasOwn(plannerRequest, "text"), false);
  assert.equal(Object.hasOwn(plannerRequest, "attachments"), false);
  await assert.rejects(
    planOperatorChatSurface(fixture.options, { ...request, text: "invented bootstrap message" }),
    /unknown fields: text/,
  );
});

test("new task authoring and Project deletion enforce both topology orderings", async (t) => {
  const taskFirstRoot = await fs.promises.mkdtemp(path.join("/tmp", "octopus-task-first-topology-"));
  const deleteFirstRoot = await fs.promises.mkdtemp(path.join("/tmp", "octopus-delete-first-topology-"));
  t.after(async () => {
    await Promise.all([
      fs.promises.rm(taskFirstRoot, { recursive: true, force: true }),
      fs.promises.rm(deleteFirstRoot, { recursive: true, force: true }),
    ]);
  });

  const taskFirst = await createAuthoringFixture(taskFirstRoot, "ee_project_task_first");
  const taskFirstHandIns = [];
  let taskFirstSettled = false;
  const taskFirstSubmission = submitOperatorTurn(
    newTaskOptions(taskFirst, taskFirstHandIns),
    newTaskRequest(taskFirst.eeId),
  );
  taskFirstSubmission.then(
    () => { taskFirstSettled = true; },
    () => { taskFirstSettled = true; },
  );
  const taskFirstRunPath = await waitForRunRecord(taskFirst.runsRoot);
  assert.equal(
    JSON.parse(await fs.promises.readFile(taskFirstRunPath, "utf8")).status,
    "idle",
  );
  await assert.rejects(
    projectDeleteThroughTopologyGate(taskFirst),
    /is still referenced by run/,
  );
  assert.equal(taskFirstSettled, false);
  const taskFirstRun = JSON.parse(await fs.promises.readFile(taskFirstRunPath, "utf8"));
  const taskFirstInterface = Object.keys(taskFirstRun.interface_state)[0];
  const taskFirstServer = await listenForInterface(taskFirst.root, taskFirstInterface);
  try {
    const submitted = await taskFirstSubmission;
    assert.equal(submitted.runId, taskFirstRun.id);
    assert.equal(taskFirstHandIns.length, 1);
  } finally {
    await closeServer(taskFirstServer);
  }
  assert.equal(fs.existsSync(path.join(taskFirst.eeInstancesRoot, `${taskFirst.eeId}.json`)), true);

  const deleteFirst = await createAuthoringFixture(deleteFirstRoot, "ee_project_delete_first");
  let releaseDelete;
  let markDeleteEntered;
  const deleteMayFinish = new Promise((resolve) => { releaseDelete = resolve; });
  const deleteEntered = new Promise((resolve) => { markDeleteEntered = resolve; });
  const deleteFirstOperation = withOperatorWorkspaceTopologyLock(deleteFirst.root, async () => {
    markDeleteEntered();
    await deleteMayFinish;
    await fs.promises.unlink(path.join(deleteFirst.eeInstancesRoot, `${deleteFirst.eeId}.json`));
  });
  await deleteEntered;
  const deleteFirstSubmission = submitOperatorTurn(
    newTaskOptions(deleteFirst, []),
    newTaskRequest(deleteFirst.eeId),
  );
  const deleteFirstRefusal = assert.rejects(deleteFirstSubmission, /planner missing EE/);
  await waitForDirectoryEntry(path.join(deleteFirst.root, "hand-in"));
  assert.equal(fs.existsSync(deleteFirst.capturePath), false);
  releaseDelete();
  await deleteFirstOperation;
  await deleteFirstRefusal;
  assert.deepEqual(await fs.promises.readdir(deleteFirst.runsRoot), []);
});

test("new task remains idle when operator input hand-in is refused", async (t) => {
  const root = await fs.promises.mkdtemp(path.join("/tmp", "octopus-new-task-refused-"));
  t.after(async () => {
    await fs.promises.rm(root, { recursive: true, force: true });
  });
  const fixture = await createAuthoringFixture(root, "ee_project_refused_input");
  const request = { ...newTaskRequest(fixture.eeId), messageId: "msg_refused_input_01" };
  const submission = submitOperatorTurn({
    ...newTaskOptions(fixture, []),
    handInArtifact: async () => {
      throw new Error("refused before ledger admission");
    },
  }, request);
  const runPath = await waitForRunRecord(fixture.runsRoot);
  const run = JSON.parse(await fs.promises.readFile(runPath, "utf8"));
  const server = await listenForInterface(root, Object.keys(run.interface_state)[0]);
  t.after(() => closeServer(server));

  await assert.rejects(submission, /refused before ledger admission/);
  assert.equal(JSON.parse(await fs.promises.readFile(runPath, "utf8")).status, "idle");
  assert.equal(
    await fs.promises.readFile(path.join(fixture.ledgerRoot, "movements.jsonl"), "utf8"),
    fixture.ledgerBytes,
  );
});

test("new task retry after a lost verdict reuses one exact authored run and admission", async (t) => {
  const root = await fs.promises.mkdtemp(path.join("/tmp", "octopus-new-task-admission-"));
  t.after(async () => {
    await fs.promises.rm(root, { recursive: true, force: true });
  });
  const fixture = await createAuthoringFixture(root, "ee_project_new_task_recovery");
  const ledgerPath = path.join(fixture.ledgerRoot, "movements.jsonl");
  await fs.promises.writeFile(ledgerPath, "");
  const resolvedArtifacts = new Map();
  let handIns = 0;
  const request = {
    ...newTaskRequest(fixture.eeId),
    messageId: "msg_new_task_recovery_01",
  };
  const options = {
    ...fixture.options,
    clientArtifactsRoot: (interfaceId) => path.join(root, "hand-in", interfaceId, "client"),
    readArtifactPayload: async ({ artifactId, label, manifestHash }) => {
      const resolved = resolvedArtifacts.get(artifactId);
      assert.ok(resolved);
      assert.equal(resolved.label, label);
      assert.equal(resolved.manifestHash, manifestHash);
      return resolved;
    },
    handInArtifact: async (input) => {
      handIns += 1;
      const payloadBytes = await fs.promises.readFile(input.manifest.payload_path);
      const payload = JSON.parse(payloadBytes.toString("utf8"));
      const manifestHash = crypto.createHash("sha256").update(`manifest:${payloadBytes}`).digest("hex");
      const artifactId = `${input.manifest.label}:${manifestHash}`;
      const runPath = await waitForRunRecord(fixture.runsRoot);
      const run = JSON.parse(await fs.promises.readFile(runPath, "utf8"));
      resolvedArtifacts.set(artifactId, {
        label: input.manifest.label,
        manifestHash,
        payloadHash: "d".repeat(64),
        payloadBytes,
        payloadPath: input.manifest.payload_path,
        payload,
      });
      await fs.promises.appendFile(ledgerPath, `${JSON.stringify({
        seq: 501,
        kind: "InterfaceArrival",
        from: { Interface: input.interfaceId },
        to: { Interface: run.worker },
        artifact: { id: artifactId, content: manifestHash },
        invocation_metadata: {
          runtime: runtime({
            run: run.id,
            ee: run.ee,
            worker: run.worker,
            checkpoint: "cp_new_task_client_input",
            interface: input.interfaceId,
            run_record_hash: "e".repeat(64),
          }),
        },
      })}\n`);
      throw new Error("new task verdict was lost after ledger admission");
    },
  };

  const first = submitOperatorTurn(options, request);
  const runPath = await waitForRunRecord(fixture.runsRoot);
  const run = JSON.parse(await fs.promises.readFile(runPath, "utf8"));
  const interfaceId = Object.keys(run.interface_state)[0];
  const server = await listenForInterface(root, interfaceId);
  await assert.rejects(first, /new task verdict was lost after ledger admission/);
  await closeServer(server);

  const recovered = await submitOperatorTurn(options, request);
  assert.equal(recovered.messageRecovered, true);
  assert.equal(recovered.runId, run.id);
  assert.equal(recovered.chatInterfaceId, interfaceId);
  assert.equal(recovered.messageVerdict.admitted.seq, 501);
  assert.equal(handIns, 1);
  assert.deepEqual(await fs.promises.readdir(fixture.runsRoot), [path.basename(runPath)]);
  const repeated = await submitOperatorTurn(options, request);
  assert.equal(repeated.messageRecovered, true);
  assert.equal(repeated.runId, run.id);
  assert.equal(handIns, 1);
});

test("ordinary existing submission hands into its active client without topology writes", async (t) => {
  const root = await fs.promises.mkdtemp(path.join("/tmp", "octopus-existing-run-"));
  t.after(async () => {
    await fs.promises.rm(root, { recursive: true, force: true });
  });
  await installFakeTopologyTransport(root);
  const runsRoot = path.join(root, "runs");
  const registryRoot = path.join(root, "registry");
  const controlRoot = path.join(root, "app-server-control");
  await Promise.all([
    fs.promises.mkdir(runsRoot, { recursive: true }),
    fs.promises.mkdir(registryRoot, { recursive: true }),
    fs.promises.mkdir(controlRoot, { recursive: true }),
  ]);
  await fs.promises.writeFile(path.join(root, "host.json"), JSON.stringify({
    host_manifest_version: 1,
    root_refs: { run_records_root: runsRoot },
  }));
  await writeActiveProviderRegistry(registryRoot);
  const providerPath = path.join(registryRoot, "interface", ACTIVE_PROVIDER, "interface.json");
  const providerBytes = await fs.promises.readFile(providerPath, "utf8");
  const runPath = path.join(runsRoot, `${ACTIVE_RUN}.json`);
  const ordinary = activeRun();
  const providerState = ordinary.interface_state[ACTIVE_PROVIDER];
  providerState.authoring_generation = 7;
  providerState.pending_handoff = {
    terminal: true,
    source_movement_seq: 41,
    items: [{ artifact_id: SUMMARY_ARTIFACT }],
  };
  const runBytes = `${JSON.stringify(ordinary, null, 2)}\n`;
  await fs.promises.writeFile(runPath, runBytes);

  const socketPath = path.join(
    controlRoot,
    `${crypto.createHash("sha256").update(ACTIVE_CLIENT).digest("hex").slice(0, 24)}.sock`,
  );
  const server = net.createServer((socket) => socket.destroy());
  let serverListening = true;
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(socketPath, resolve);
  });
  t.after(async () => {
    if (serverListening) await new Promise((resolve) => server.close(resolve));
  });

  const handed = [];
  const options = {
    clientArtifactsRoot: (interfaceId) => path.join(root, "hand-in", interfaceId, "client"),
    octopusHome: () => root,
    installationRoot: () => root,
    registryRoot: () => registryRoot,
    handInArtifact: async (input) => {
      handed.push(input);
      return { admitted: { seq: 73 } };
    },
  };
  const result = await submitOperatorTurn(options, {
    messageId: "msg_existing_continue",
    runId: ACTIVE_RUN,
    providerInterfaceId: "world_provider_codex",
    providerModel: "gpt-5.6-sol",
    clientSurfaceRole: "task",
    text: "Continue the existing task.",
    attachments: [],
    timeoutMs: 2_000,
  });

  assert.equal(result.runId, ACTIVE_RUN);
  assert.equal(result.chatInterfaceId, ACTIVE_CLIENT);
  assert.equal(result.modelInterfaceId, ACTIVE_PROVIDER);
  assert.equal(result.workerInterfaceId, ACTIVE_WORKER);
  assert.deepEqual(result.clientWrites, []);
  assert.equal(result.messageRecovered, false);
  assert.equal(handed.length, 1);
  assert.equal(handed[0].interfaceId, ACTIVE_CLIENT);
  assert.deepEqual(
    JSON.parse(await fs.promises.readFile(handed[0].manifest.payload_path, "utf8")),
    {
      text: "Continue the existing task.",
      message_id: "msg_existing_continue",
      attachments: [],
    },
  );
  assert.equal(await fs.promises.readFile(runPath, "utf8"), runBytes);
  assert.equal(await fs.promises.readFile(providerPath, "utf8"), providerBytes);

  const ledgerRoot = path.join(root, "ledger");
  await fs.promises.mkdir(ledgerRoot, { recursive: true });
  const clientRegistryRoot = path.join(registryRoot, "interface", ACTIVE_CLIENT);
  await fs.promises.mkdir(clientRegistryRoot, { recursive: true });
  await fs.promises.writeFile(path.join(clientRegistryRoot, "octopus_os.json"), JSON.stringify({
    schema_version: 1, workspace: "octopus_os", component: "client", interface: ACTIVE_CLIENT,
    owner_interface: ACTIVE_CLIENT, surface_role: "task",
    provider_choice_interface: "world_provider_codex", worker_choice_interface: "system_worker_coder_max",
    authoring_profile: {
      ee_id: ordinary.ee, worker_choice_interface: "system_worker_coder_max", door_host_slots: {},
    },
  }));
  const plannerPath = await installFakeTopologyTransport(root, `
  const data = input;
  fs.appendFileSync(path.join(path.dirname(__filename), "revision-requests.jsonl"), data + "\\n");
  if (process.argv[2] === "--inspect-input-payloads") {
    process.stdout.write(JSON.stringify({ selected_payloads: [] })); return;
  }
  if (process.argv[2]) {
    const root = path.join(request.artifact_root, "input-revisions");
    fs.mkdirSync(root, { recursive: true });
    const file = path.join(root, request.key + ".artifact.json");
    if (process.argv[2] === "--prepare-input-revision") {
      if (request.operation.kind === "owner_inputs") {
        process.stdout.write(JSON.stringify({ status: "unchanged" })); return;
      }
      if (!fs.existsSync(file)) {
        const current = JSON.parse(request.expected_before_content), next = structuredClone(current);
        const old = next.checkpoints.find(cp => next.active_checkpoints.includes(cp.id) && cp.interface_binding.interface === request.source);
        const id = old.id + "_checked";
        next.active_checkpoints = next.active_checkpoints.map(item => item === old.id ? id : item);
        next.interface_state[request.provider].frontier_checkpoint_ids = next.interface_state[request.provider].frontier_checkpoint_ids.map(item => item === old.id ? id : item);
        old.id = id;
        fs.writeFileSync(file, JSON.stringify({ key: request.key, path: request.run_path, before: request.expected_before_content, after: JSON.stringify(next) }));
      }
      process.stdout.write(JSON.stringify({ status: "prepared" })); return;
    }
    if (process.argv[2] !== "--verify-input-revision") throw new Error("unknown revision flag");
    const pending = JSON.parse(fs.readFileSync(file)), sent = fs.existsSync(file + ".sent"), done = fs.existsSync(file + ".result");
    if (done) process.stdout.write(JSON.stringify({ status: "installed", path: request.run_path, before_hash: "a".repeat(64), after_hash: "b".repeat(64), install: { call: { movement_seq: 76 }, result: { movement_seq: 78 } } }));
    else if (sent) process.stdout.write(JSON.stringify({ status: "awaiting_result" }));
    else {
      if (fs.readFileSync(request.run_path, "utf8") !== pending.before) throw new Error("changed before writer hand-in");
      process.stdout.write(JSON.stringify({ status: "ready", source_interface: "system_checked_writer", manifest: { label: "tool.call", payload_path: file } }));
    }
    return;
  }
  const run = request.existing_run_record;
  const client = \`world_operator_chat_\${request.chat_id}\`;
  const provider = run.interface_state[client].provider_runtime_interface;
  const previous = new Set(run.interface_state[client].cycle_checkpoint_ids);
  const checkpoint = \`cp_rearmed_\${request.cycle}\`;
  run.checkpoints = run.checkpoints.filter(item => !previous.has(item.id));
  run.active_checkpoints = run.active_checkpoints.filter(id => !previous.has(id));
  run.checkpoints.push({ id: checkpoint, run: run.id, epoch: 0, interface_binding: { interface: client, config: { artifact_policy: { mode: "allow", names: ["operator.message"], namespaces: [] } } }, outbound_interface_bindings: [{ interface: provider, config: { artifact_policy: { mode: "all" } } }], artifact_bindings: [] });
  run.active_checkpoints.push(checkpoint);
  run.interface_state[client].cycle_checkpoint_ids = [checkpoint];
  run.interface_state[client].ledger_offset = request.conversation.ledger_offset;
  run.interface_state[provider].frontier_checkpoint_ids = [checkpoint];
  run.interface_state[provider].unselected_new_default = "must-not-replace-retained-state";
  const profile = { ee_id: request.ee_id, worker_choice_interface: request.worker_interface_id, door_host_slots: request.door_host_slots };
  process.stdout.write(JSON.stringify({ run_id: run.id, chat_interface_id: client, model_interface_id: provider, worker_interface_id: run.worker, client_surface_role: "task", model: request.model, reasoning_effort: "high", expected_before_hash_algorithm: "sha256", authoring_profile: profile, required_interface_ids: [client, provider], required_tool_ids: ["file.write"], interface_writes: [], run_writes: [], removals: [], run_record_content: JSON.stringify(run, null, 2) + "\\n" }));
`);
  const initialRun = activeRun(); initialRun.status = "idle";
  const initialBytes = `${JSON.stringify(initialRun, null, 2)}\n`;
  await fs.promises.writeFile(runPath, initialBytes);
  await fs.promises.writeFile(path.join(ledgerRoot, "movements.jsonl"), "");
  const initialRequest = { messageId: "msg_existing_initial", runId: ACTIVE_RUN,
    providerInterfaceId: "world_provider_codex", providerModel: "gpt-5.6-sol",
    clientSurfaceRole: "task", text: "First input after updating the app.", attachments: [], timeoutMs: 2_000 };
  const initialResult = await submitOperatorTurn(options, initialRequest);
  assert.deepEqual(initialResult.clientWrites, []);
  assert.equal(await fs.promises.readFile(runPath, "utf8"), initialBytes);
  assert.equal(await fs.promises.readFile(providerPath, "utf8"), providerBytes);
  const initialRevisionRoot = path.join(options.clientArtifactsRoot(ACTIVE_CLIENT), "operator-chat", "input-revisions");
  const retainedDefinition = path.join(initialRevisionRoot, "msg_existing_initial_retained-definition.artifact.json");
  await fs.promises.writeFile(retainedDefinition, JSON.stringify({ key: "msg_existing_initial_retained-definition", path: runPath,
    before: initialBytes, after: initialBytes }));
  await fs.promises.writeFile(retainedDefinition + ".result", "checked");
  const initialRetried = await submitOperatorTurn(options, { ...initialRequest, messageId: "msg_existing_initial_retained" });
  assert.equal(initialRetried.clientWrites.length, 1);
  assert.equal(await fs.promises.readFile(runPath, "utf8"), initialBytes);
  const initialCommands = (await fs.promises.readFile(path.join(path.dirname(plannerPath), "revision-requests.jsonl"), "utf8"))
    .trim().split("\n").map(JSON.parse);
  assert.deepEqual(initialCommands.filter(row => row.operation).map(row => row.operation.kind), ["owner_inputs"]);
  assert.equal(initialCommands.some(row => row.key?.endsWith("-rearm")), false);
  assert.equal(initialCommands.filter(row => row.existing_run_record).every(row => row.reasoning_effort === "high"), true);
  assert.equal(handed.length, 3);
  await fs.promises.writeFile(runPath, runBytes);
  await fs.promises.writeFile(path.join(ledgerRoot, "movements.jsonl"), [
    {
      seq: 74, kind: "InterfaceArrival", from: { Interface: ACTIVE_CLIENT }, to: { Interface: ACTIVE_WORKER },
      artifact: { id: `operator.message:${"a".repeat(64)}`, content: "a".repeat(64) },
      invocation_metadata: { runtime: runtime({ run: ACTIVE_RUN, worker: ACTIVE_WORKER, checkpoint: "cp_active_client_input", interface: ACTIVE_CLIENT }) },
    },
    {
      seq: 75, kind: "InterfaceOutbound", from: { Interface: ACTIVE_PROVIDER }, to: { Interface: ACTIVE_CLIENT },
      artifact: { id: `turn.error:${"b".repeat(64)}`, content: "b".repeat(64) },
      invocation_metadata: { runtime: runtime({ run: ACTIVE_RUN, worker: ACTIVE_WORKER, checkpoint: "cp_active_provider_output", interface: ACTIVE_CLIENT }) },
    },
  ].map(JSON.stringify).join("\n") + "\n");
  const writerServer = await listenForInterface(root, "system_checked_writer");
  t.after(() => closeServer(writerServer));
  let writerCalls = 0;
  let retainedCall;
  options.handInArtifact = async (input) => {
    if (input.interfaceId === "system_checked_writer") {
      writerCalls++;
      retainedCall = input.manifest.payload_path;
      const edit = JSON.parse(await fs.promises.readFile(retainedCall, "utf8"));
      assert.equal(await fs.promises.readFile(edit.path, "utf8"), runBytes);
      await fs.promises.writeFile(edit.path, edit.after);
      await fs.promises.writeFile(retainedCall + ".sent", "accepted");
      throw new Error("lost writer verdict before checked result");
    }
    assert.equal(fs.existsSync(retainedCall + ".result"), true);
    handed.push(input);
    return { admitted: { seq: 79 } };
  };
  const nextRequest = {
    messageId: "msg_existing_after_provider_failure",
    runId: ACTIVE_RUN,
    providerInterfaceId: "world_provider_codex",
    providerModel: "gpt-5.6-sol",
    clientSurfaceRole: "task",
    text: "Continue after the terminal provider failure.",
    attachments: [],
    timeoutMs: 1_000,
  };
  await assert.rejects(submitOperatorTurn(options, nextRequest), /awaits its checked writer result/);
  assert.equal(writerCalls, 1);
  assert.equal(handed.length, 3);
  await fs.promises.writeFile(retainedCall + ".result", "checked");
  for (const cached of Object.keys(require.cache)) {
    if (cached === sourcePath || cached.startsWith(path.join(__dirname, "client") + path.sep)) delete require.cache[cached];
  }
  const resumed = await require(sourcePath).submitOperatorTurn(options, nextRequest);
  const reauthored = JSON.parse(await fs.promises.readFile(runPath, "utf8"));
  assert.equal(resumed.clientWrites.length, 1);
  assert.equal(writerCalls, 1);
  assert.equal(reauthored.active_checkpoints.includes("cp_active_client_input_checked"), true);
  assert.deepEqual(reauthored.interface_state[ACTIVE_PROVIDER].pending_handoff, providerState.pending_handoff);
  assert.deepEqual(reauthored.interface_state[ACTIVE_CLIENT], ordinary.interface_state[ACTIVE_CLIENT]);
  assert.equal(reauthored.interface_state[ACTIVE_PROVIDER].unselected_new_default, undefined);
  assert.equal(handed.length, 4);
  const progressed = structuredClone(reauthored);
  progressed.interface_state[ACTIVE_PROVIDER].authoring_generation += 1;
  const progressedBytes = JSON.stringify(progressed);
  const steered = await submitOperatorTurn({ ...options, preparePayloadArtifactBindings: async ({ payloadArtifactBindings }) => {
    await fs.promises.writeFile(runPath, progressedBytes); return payloadArtifactBindings;
  } }, { ...nextRequest, messageId: "msg_existing_during_progress" });
  assert.deepEqual(steered.clientWrites, []);
  assert.equal(handed.length, 5);
  assert.equal(await fs.promises.readFile(runPath, "utf8"), progressedBytes);
  const sidecarPath = path.join(clientRegistryRoot, "octopus_os.json"), sidecar = JSON.parse(await fs.promises.readFile(sidecarPath));
  sidecar.authoring_profile.payload_artifact_bindings = [{ id: "fixture.stale", content: Array(32).fill(1) }];
  await fs.promises.writeFile(sidecarPath, JSON.stringify(sidecar));
  await assert.rejects(submitOperatorTurn({ ...options, preparePayloadArtifactBindings: async s => s.payloadArtifactBindings },
    { ...nextRequest, messageId: "msg_stale_payload_publication" }), /awaits publication of its installed input selection/);
  assert.equal(handed.length, 5); assert.equal(await fs.promises.readFile(runPath, "utf8"), progressedBytes);
});

test("attachment crash before admission resumes only from an exact reattachment", async (t) => {
  const root = await fs.promises.mkdtemp(path.join("/tmp", "octopus-attach-cut-"));
  t.after(async () => {
    await fs.promises.rm(root, { recursive: true, force: true });
  });
  await installFakeTopologyTransport(root);
  const runsRoot = path.join(root, "runs");
  const registryRoot = path.join(root, "registry");
  const controlRoot = path.join(root, "app-server-control");
  await Promise.all([
    fs.promises.mkdir(runsRoot, { recursive: true }),
    fs.promises.mkdir(registryRoot, { recursive: true }),
    fs.promises.mkdir(controlRoot, { recursive: true }),
  ]);
  await fs.promises.writeFile(path.join(root, "host.json"), JSON.stringify({
    host_manifest_version: 1,
    root_refs: { run_records_root: runsRoot },
  }));
  await writeActiveProviderRegistry(registryRoot);
  await fs.promises.writeFile(
    path.join(runsRoot, `${ACTIVE_RUN}.json`),
    `${JSON.stringify(activeRun(), null, 2)}\n`,
  );
  const server = await listenForInterface(root, ACTIVE_CLIENT);
  t.after(() => closeServer(server));

  const attachmentBytes = Buffer.from("exact attachment crash cut");
  const attachmentPath = path.join(root, "attachment.stage");
  const attachmentHash = crypto.createHash("sha256").update(attachmentBytes).digest("hex");
  await fs.promises.writeFile(attachmentPath, attachmentBytes);
  const messageId = "msg_attachment_crash_cut_01";
  const clientRoot = path.join(root, "hand-in", ACTIVE_CLIENT, "client", "operator-chat");
  const admissionPath = path.join(clientRoot, "message-admissions", `${messageId}.json`);
  const payloadPath = path.join(clientRoot, "payloads", `${messageId}.payload`);
  const intentPath = path.join(clientRoot, "message-intents", `${messageId}.json`);
  const attachmentRoot = path.join(clientRoot, "attachments");
  const handed = [];
  const options = {
    clientArtifactsRoot: (interfaceId) => path.join(root, "hand-in", interfaceId, "client"),
    octopusHome: () => root,
    installationRoot: () => root,
    registryRoot: () => registryRoot,
    handInArtifact: async (input) => {
      handed.push(input);
      return { admitted: { seq: 611 } };
    },
  };
  const request = {
    messageId,
    runId: ACTIVE_RUN,
    providerInterfaceId: "world_provider_codex",
    providerModel: "gpt-5.6-sol",
    clientSurfaceRole: "task",
    text: "Keep this attachment exactly once.",
    attachments: [{
      name: "crash-cut.txt",
      label: "operator.file.text",
      type: "text/plain",
      size: attachmentBytes.length,
      payloadPath: attachmentPath,
      contentHash: attachmentHash,
      lastModified: 77,
    }],
    timeoutMs: 2_000,
  };
  const fsPromises = require("node:fs/promises");
  const link = fsPromises.link;
  let interrupted = false;
  fsPromises.link = async (...args) => {
    if (!interrupted && path.resolve(args[1]).startsWith(`${path.resolve(attachmentRoot)}${path.sep}`)) {
      interrupted = true;
      throw new Error("simulated crash before durable attachment admission");
    }
    return link(...args);
  };
  try {
    await assert.rejects(
      submitOperatorTurn(options, request),
      /simulated crash before durable attachment admission/,
    );
  } finally {
    fsPromises.link = link;
  }

  assert.equal(interrupted, true);
  assert.equal(fs.existsSync(attachmentPath), true);
  assert.equal(fs.existsSync(payloadPath), true);
  assert.equal(fs.existsSync(intentPath), true);
  assert.equal(fs.existsSync(admissionPath), false);
  assert.equal(handed.length, 0);
  await fs.promises.writeFile(attachmentPath, Buffer.alloc(attachmentBytes.length, 0x78));
  await assert.rejects(
    submitOperatorTurn(options, request),
    /no longer matches its declared content hash/,
  );
  assert.equal(fs.existsSync(admissionPath), false);
  assert.equal(handed.length, 0);
  await fs.promises.writeFile(attachmentPath, attachmentBytes);

  const resumed = await submitOperatorTurn(options, request);
  assert.equal(resumed.messageRecovered, false);
  assert.equal(resumed.messageVerdict.admitted.seq, 611);
  assert.equal(resumed.attachmentCount, 1);
  assert.equal(handed.length, 1);
  assert.deepEqual(await fs.promises.readFile(attachmentPath), attachmentBytes);
  await fs.promises.unlink(attachmentPath);
  const payload = JSON.parse(await fs.promises.readFile(payloadPath, "utf8"));
  assert.equal(payload.attachments.length, 1);
  assert.equal(payload.attachments[0].payload_content_hash, attachmentHash);
  assert.equal(await fs.promises.readFile(payload.attachments[0].payload_path, "utf8"), attachmentBytes.toString("utf8"));
  const admission = JSON.parse(await fs.promises.readFile(admissionPath, "utf8"));
  assert.equal(admission.phase, "admitted");
  assert.equal(admission.movement_seq, 611);
});

test("operator message admission recovers an accepted turn after the delivery verdict is lost", async (t) => {
  const root = await fs.promises.mkdtemp(path.join("/tmp", "octopus-message-admission-"));
  t.after(async () => {
    await fs.promises.rm(root, { recursive: true, force: true });
  });
  await installFakeTopologyTransport(root);
  const runsRoot = path.join(root, "runs");
  const registryRoot = path.join(root, "registry");
  const controlRoot = path.join(root, "app-server-control");
  const ledgerRoot = path.join(root, "ledger");
  await Promise.all([
    fs.promises.mkdir(runsRoot, { recursive: true }),
    fs.promises.mkdir(registryRoot, { recursive: true }),
    fs.promises.mkdir(controlRoot, { recursive: true }),
    fs.promises.mkdir(ledgerRoot, { recursive: true }),
  ]);
  await fs.promises.writeFile(path.join(root, "host.json"), JSON.stringify({
    host_manifest_version: 1,
    root_refs: { run_records_root: runsRoot },
  }));
  await writeActiveProviderRegistry(registryRoot);
  await fs.promises.writeFile(
    path.join(runsRoot, `${ACTIVE_RUN}.json`),
    `${JSON.stringify(activeRun(), null, 2)}\n`,
  );
  const ledgerPath = path.join(ledgerRoot, "movements.jsonl");
  await fs.promises.writeFile(ledgerPath, "");
  const socketPath = path.join(
    controlRoot,
    `${crypto.createHash("sha256").update(ACTIVE_CLIENT).digest("hex").slice(0, 24)}.sock`,
  );
  const server = net.createServer((socket) => socket.destroy());
  let serverListening = true;
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(socketPath, resolve);
  });
  t.after(async () => {
    if (serverListening) await new Promise((resolve) => server.close(resolve));
  });

  const attachmentBytes = Buffer.from("durable queued attachment");
  const attachmentPath = path.join(root, "queued-attachment.ready");
  await fs.promises.writeFile(attachmentPath, attachmentBytes);

  const resolvedArtifacts = new Map();
  let handIns = 0;
  const options = {
    clientArtifactsRoot: (interfaceId) => path.join(root, "hand-in", interfaceId, "client"),
    octopusHome: () => root,
    installationRoot: () => root,
    registryRoot: () => registryRoot,
    readArtifactPayload: async ({ artifactId, label, manifestHash }) => {
      const resolved = resolvedArtifacts.get(artifactId);
      assert.ok(resolved);
      assert.equal(resolved.label, label);
      assert.equal(resolved.manifestHash, manifestHash);
      return resolved;
    },
    handInArtifact: async (input) => {
      handIns += 1;
      const payloadBytes = await fs.promises.readFile(input.manifest.payload_path);
      const payload = JSON.parse(payloadBytes.toString("utf8"));
      const manifestHash = crypto.createHash("sha256").update(`manifest:${payloadBytes}`).digest("hex");
      const artifactId = `${input.manifest.label}:${manifestHash}`;
      resolvedArtifacts.set(artifactId, {
        label: input.manifest.label,
        manifestHash,
        payloadHash: "b".repeat(64),
        payloadBytes,
        payloadPath: input.manifest.payload_path,
        payload,
      });
      await fs.promises.appendFile(ledgerPath, `${JSON.stringify({
        seq: 73,
        kind: "InterfaceArrival",
        from: { Interface: ACTIVE_CLIENT },
        to: { Interface: ACTIVE_WORKER },
        artifact: { id: artifactId, content: manifestHash },
        invocation_metadata: {
          runtime: runtime({
            run: ACTIVE_RUN,
            ee: "ee_operator_interaction",
            worker: ACTIVE_WORKER,
            checkpoint: "cp_active_client_input",
            interface: ACTIVE_CLIENT,
            run_record_hash: "a".repeat(64),
          }),
        },
      })}\n`);
      await fs.promises.appendFile(ledgerPath, `${JSON.stringify({
        seq: 74,
        kind: "InterfaceArrival",
        from: { Interface: ACTIVE_CLIENT },
        to: { Interface: ACTIVE_WORKER },
        artifact: { id: artifactId, content: manifestHash },
        invocation_metadata: {
          runtime: runtime({
            run: ACTIVE_RUN,
            ee: "ee_operator_interaction",
            worker: ACTIVE_WORKER,
            checkpoint: "cp_active_client_input_fanout",
            interface: ACTIVE_CLIENT,
            run_record_hash: "a".repeat(64),
          }),
        },
      })}\n`);
      throw new Error("delivery verdict was lost after ledger admission");
    },
  };
  const request = {
    messageId: "msg_recover_exact_01",
    runId: ACTIVE_RUN,
    providerInterfaceId: "world_provider_codex",
    providerModel: "gpt-5.6-sol",
    clientSurfaceRole: "task",
    text: "Continue exactly once.",
    attachments: [{
      name: "evidence.txt",
      label: "operator.file.text",
      type: "text/plain",
      size: attachmentBytes.length,
      payloadPath: attachmentPath,
      contentHash: crypto.createHash("sha256").update(attachmentBytes).digest("hex"),
      lastModified: 42,
    }],
    timeoutMs: 2_000,
  };

  await assert.rejects(
    submitOperatorTurn(options, request),
    /delivery verdict was lost after ledger admission/,
  );
  assert.deepEqual(await fs.promises.readFile(attachmentPath), attachmentBytes);
  await fs.promises.unlink(attachmentPath);
  const preparedAdmissionPath = path.join(
    root,
    "hand-in",
    ACTIVE_CLIENT,
    "client",
    "operator-chat",
    "message-admissions",
    `${request.messageId}.json`,
  );
  const preparedAdmission = JSON.parse(await fs.promises.readFile(preparedAdmissionPath, "utf8"));
  assert.equal(preparedAdmission.phase, "prepared");
  assert.equal(preparedAdmission.movement_seq, null);
  const preparedPayload = JSON.parse(await fs.promises.readFile(preparedAdmission.payload_path, "utf8"));
  assert.equal(preparedPayload.attachments.length, 1);
  assert.equal(
    await fs.promises.readFile(preparedPayload.attachments[0].payload_path, "utf8"),
    attachmentBytes.toString("utf8"),
  );
  await new Promise((resolve) => server.close(resolve));
  serverListening = false;
  const settledRun = activeRun();
  settledRun.active_checkpoints = [];
  await fs.promises.writeFile(
    path.join(runsRoot, `${ACTIVE_RUN}.json`),
    `${JSON.stringify(settledRun, null, 2)}\n`,
  );
  const recoveryRequest = {
    ...request,
    attachments: [],
    recoverPreparedMessage: true,
  };
  const fsPromises = require("node:fs/promises");
  const open = fsPromises.open;
  fsPromises.open = async (...args) => {
    const handle = await open(...args);
    if (path.resolve(args[0]) !== path.resolve(ledgerPath)) return handle;
    return {
      close: (...closeArgs) => handle.close(...closeArgs),
      stat: (...statArgs) => handle.stat(...statArgs),
      read: (buffer, offset, length, position) => handle.read(
        buffer,
        offset,
        Math.min(length, 11),
        position,
      ),
    };
  };
  let recovered;
  try {
    recovered = await submitOperatorTurn(options, recoveryRequest);
  } finally {
    fsPromises.open = open;
  }
  assert.equal(recovered.messageRecovered, true);
  assert.equal(recovered.attachmentCount, 1);
  assert.equal(recovered.messageVerdict.admitted.seq, 73);
  assert.equal(recovered.outboundCursor.offset, 0);
  assert.equal(handIns, 1);
  const admission = JSON.parse(await fs.promises.readFile(preparedAdmissionPath, "utf8"));
  assert.equal(admission.phase, "admitted");
  assert.equal(admission.movement_seq, 73);

  const repeated = await submitOperatorTurn(options, recoveryRequest);
  assert.equal(repeated.messageRecovered, true);
  assert.equal(repeated.messageVerdict.admitted.seq, 73);
  assert.equal(handIns, 1);
  await assert.rejects(
    submitOperatorTurn(options, { ...recoveryRequest, text: "Conflicting replay." }),
    /conflicts with its durable prepared envelope/,
  );
  assert.equal(handIns, 1);
});

test("task deletion releases its exact worker environment through an allow-only artifact cycle", async (t) => {
  const root = await fs.promises.mkdtemp(path.join("/tmp", "octopus-task-release-"));
  const runsRoot = path.join(root, "state", "runs");
  const registryRoot = path.join(root, "registry", "interface");
  const controlRoot = path.join(root, "app-server-control");
  const ledgerRoot = path.join(root, "ledger");
  const door = "system_coder_env_active";
  const workerEnvironmentId = `we_${"a".repeat(32)}`;
  const formationPath = path.join(
    root,
    "interfaces",
    door,
    "worker-environments",
    workerEnvironmentId,
    "worker_environment.json",
  );
  await Promise.all([
    fs.promises.mkdir(runsRoot, { recursive: true }),
    fs.promises.mkdir(controlRoot, { recursive: true }),
    fs.promises.mkdir(ledgerRoot, { recursive: true }),
    fs.promises.mkdir(path.dirname(formationPath), { recursive: true }),
    ...[ACTIVE_CLIENT, ACTIVE_PROVIDER, door, ACTIVE_WORKER].map((interfaceId) => (
      fs.promises.mkdir(path.join(registryRoot, interfaceId), { recursive: true })
    )),
  ]);
  t.after(async () => {
    await fs.promises.rm(root, { recursive: true, force: true });
  });
  await installFakeTopologyTransport(root);
  await fs.promises.writeFile(path.join(root, "host.json"), JSON.stringify({
    host_manifest_version: 1,
    root_refs: { run_records_root: runsRoot },
  }));
  await fs.promises.writeFile(path.join(ledgerRoot, "movements.jsonl"), "");
  for (const interfaceId of [ACTIVE_CLIENT, ACTIVE_PROVIDER, door]) {
    await fs.promises.writeFile(
      path.join(registryRoot, interfaceId, "octopus_os.json"),
      JSON.stringify({
        workspace: "octopus_os",
        interface: interfaceId,
        owner_interface: ACTIVE_CLIENT,
      }),
    );
  }
  await fs.promises.writeFile(
    path.join(registryRoot, ACTIVE_WORKER, "policy.json"),
    JSON.stringify({ id: ACTIVE_WORKER, ceiling: {}, role_artifacts: [ACTIVE_WORKER_ROLE] }),
  );

  const run = activeRun();
  for (const checkpoint of run.checkpoints) {
    checkpoint.epoch = 4;
    checkpoint.trace = `trace:${ACTIVE_RUN}:${checkpoint.id}`;
    checkpoint.required_verified_artifacts = [];
  }
  const writerId = "declared_task_writer";
  const initialWriter = {
    id: writerId, ee: run.ee, worker: run.worker, trace: `trace:${writerId}`, status: "idle",
    interface_state: { [ACTIVE_PROVIDER]: { file_writer_target: {
      run: ACTIVE_RUN, provider: ACTIVE_PROVIDER, source: ACTIVE_CLIENT, door, generation: 0,
    } } },
    checkpoints: [
      { id: "cp_provider_authoring_1", run: writerId, epoch: 0, trace: `trace:${writerId}:cp_provider_authoring_1`,
        interface_binding: authoredBinding(ACTIVE_CLIENT), outbound_interface_bindings: [authoredBinding(door)], artifact_bindings: [] },
      { id: "cp_authoring_provider_1", run: writerId, epoch: 0, trace: `trace:${writerId}:cp_authoring_provider_1`,
        interface_binding: authoredBinding(door), outbound_interface_bindings: [authoredBinding(ACTIVE_PROVIDER)], artifact_bindings: [] },
    ],
    active_checkpoints: ["cp_authoring_provider_1", "cp_provider_authoring_1"],
  };
  run.interface_state[ACTIVE_PROVIDER].run_authoring_binding.writer_run = {
    run: writerId, initial_record: initialWriter, role_artifact_bindings: [],
  };
  const currentWriter = structuredClone(initialWriter);
  currentWriter.interface_state[ACTIVE_PROVIDER].file_writer_target.generation = 2;
  currentWriter.checkpoints.forEach((checkpoint) => {
    checkpoint.id += "_2";
    checkpoint.trace = `trace:${writerId}:${checkpoint.id}`;
  });
  currentWriter.active_checkpoints = initialWriter.active_checkpoints.map((checkpointId) => `${checkpointId}_2`);
  const writerPath = path.join(runsRoot, `${writerId}.json`);
  await fs.promises.writeFile(writerPath, JSON.stringify(currentWriter));
  const runPath = path.join(runsRoot, `${ACTIVE_RUN}.json`);
  const runText = `${JSON.stringify(run, null, 2)}\n`;
  await fs.promises.writeFile(runPath, runText);
  const formationRecord = {
    schema_version: 5,
    formation: {
      worker_environment_id: workerEnvironmentId,
      run: ACTIVE_RUN,
      ee: run.ee,
      worker: run.worker,
      door_interface: door,
      requires_release: true,
    },
    lifecycle: "active",
    opened_at_movement_seq: 11,
    last_activity_movement_seq: 11,
  };
  await fs.promises.writeFile(formationPath, `${JSON.stringify(formationRecord, null, 2)}\n`);
  const writerEnvironmentId = `we_${"b".repeat(32)}`;
  const writerFormationPath = path.join(path.dirname(path.dirname(formationPath)), writerEnvironmentId, "worker_environment.json");
  const writerFormation = structuredClone(formationRecord);
  Object.assign(writerFormation.formation, { worker_environment_id: writerEnvironmentId, run: writerId, requires_release: false });
  await fs.promises.mkdir(path.dirname(writerFormationPath));
  const preflightRelease = () => releaseOperatorTaskWorkerEnvironments({ octopusHome: () => root }, {
    runId: ACTIVE_RUN, expectedDigest: crypto.createHash("sha256").update(runText).digest("hex"),
  });
  writerFormation.formation.run = "unrelated_run";
  await fs.promises.writeFile(writerFormationPath, JSON.stringify(writerFormation));
  await assert.rejects(preflightRelease(), /formation for another run/);
  writerFormation.formation.run = writerId;
  await fs.promises.writeFile(writerFormationPath, JSON.stringify(writerFormation));
  await fs.promises.writeFile(writerPath, JSON.stringify({ ...currentWriter, ee: "unrelated_ee" }));
  await assert.rejects(preflightRelease(), /drifted from its declared generation/);
  await fs.promises.writeFile(writerPath, JSON.stringify({ ...currentWriter, active_checkpoints: currentWriter.checkpoints.map((checkpoint) => checkpoint.id) }));
  await assert.rejects(preflightRelease(), /drifted from its declared generation/);
  await fs.promises.writeFile(writerPath, JSON.stringify({ ...currentWriter, active_checkpoints: [...currentWriter.active_checkpoints, "cp_extra_2"] }));
  await assert.rejects(preflightRelease(), /drifted from its declared generation/);
  await fs.promises.writeFile(writerPath, JSON.stringify(currentWriter));
  assert.equal(await fs.promises.readFile(runPath, "utf8"), runText);

  const socketPath = path.join(
    controlRoot,
    `${crypto.createHash("sha256").update(ACTIVE_CLIENT).digest("hex").slice(0, 24)}.sock`,
  );
  const server = net.createServer((socket) => socket.destroy());
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(socketPath, resolve);
  });
  t.after(async () => {
    await new Promise((resolve) => server.close(resolve));
  });

  const outcome = await releaseOperatorTaskWorkerEnvironments({
    clientArtifactsRoot: (interfaceId) => path.join(root, "hand-in", interfaceId, "client"),
    octopusHome: () => root,
    installationRoot: () => root,
    handInArtifact: async (input) => {
      assert.equal(input.interfaceId, ACTIVE_CLIENT);
      assert.equal(input.manifest.label, "worker_environment.release");
      assert.deepEqual(
        JSON.parse(await fs.promises.readFile(input.manifest.payload_path, "utf8")),
        { worker_environment_id: workerEnvironmentId },
      );
      const authoredRun = JSON.parse(await fs.promises.readFile(runPath, "utf8"));
      const active = authoredRun.active_checkpoints.map((id) => (
        authoredRun.checkpoints.find((checkpoint) => checkpoint.id === id)
      ));
      assert.equal(active.length, 2);
      assert.deepEqual(active.map((checkpoint) => checkpoint.epoch), [5, 5]);
      assert.deepEqual(
        active.map((checkpoint) => checkpoint.artifact_bindings),
        [[{ id: ACTIVE_WORKER_ROLE }], [{ id: ACTIVE_WORKER_ROLE }]],
      );
      assert.deepEqual(
        active[0].interface_binding.config.artifact_policy,
        { mode: "allow", names: ["worker_environment.release"], namespaces: [] },
      );
      assert.deepEqual(
        active[1].interface_binding.config.artifact_policy,
        {
          mode: "allow",
          names: ["worker_environment.released", `${door}.failure`],
          namespaces: [],
        },
      );
      formationRecord.lifecycle = "released";
      formationRecord.last_activity_movement_seq = 12;
      await fs.promises.writeFile(formationPath, `${JSON.stringify(formationRecord, null, 2)}\n`);
      await fs.promises.appendFile(path.join(ledgerRoot, "movements.jsonl"), `${JSON.stringify({
        seq: 12,
        kind: "InterfaceOutbound",
        recorded_at_unix_ms: Date.now(),
        from: { Interface: door },
        to: { Interface: ACTIVE_CLIENT },
        artifact: { id: `worker_environment.released:${"b".repeat(64)}`, content: "b".repeat(64) },
        invocation_metadata: {
          runtime: {
            run: ACTIVE_RUN,
            ee: run.ee,
            worker: run.worker,
            checkpoint: active[1].id,
            interface: ACTIVE_CLIENT,
            epoch: 5,
          },
        },
      })}\n`);
      return { admitted: { seq: 11 } };
    },
  }, {
    runId: ACTIVE_RUN,
    expectedDigest: crypto.createHash("sha256").update(runText).digest("hex"),
    timeoutMs: 2_000,
  });

  assert.deepEqual(outcome.releasedWorkerEnvironmentIds, [workerEnvironmentId]);
  assert.equal(
    outcome.expectedDigest,
    crypto.createHash("sha256").update(await fs.promises.readFile(runPath)).digest("hex"),
  );
  assert.equal(JSON.parse(await fs.promises.readFile(formationPath, "utf8")).lifecycle, "released");
});


test("rearmed idle payload staging preserves inherited refs, explicit clearing and immutable preparation", async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "octopus-payload-selection-"));
  const registryRoot = path.join(root, "registry");
  const sidecarPath = path.join(registryRoot, "interface", ACTIVE_CLIENT, "octopus_os.json");
  fs.mkdirSync(path.dirname(sidecarPath), { recursive: true });
  const config = JSON.parse(fs.readFileSync(path.join(__dirname, "client.json"), "utf8"));
  const run = activeRun();
  run.status = "idle";
  const ready = assertPayloadInputCycleReady;
  const provider = run.interface_state[ACTIVE_PROVIDER];
  provider.authoring_generation = 7;
  provider.pending_handoff = { terminal: true };
  const fresh = { operatorMessageCheckpointAdmitted: false, terminalAfterOperatorMessage: false };
  const spent = { operatorMessageCheckpointAdmitted: true, terminalAfterOperatorMessage: true };
  for (const conversation of [fresh, spent]) {
    for (const publication of [undefined, null]) {
      if (publication === undefined) delete provider.pending_handoff.context_publication;
      else provider.pending_handoff.context_publication = publication;
      const before = JSON.stringify(run);
      assert.doesNotThrow(() => ready(run, ACTIVE_PROVIDER, conversation));
      assert.equal(JSON.stringify(run), before);
    }
    for (const change of [
      { status: "running" }, { interface_state: {} },
      ...[undefined, null, "7", -1].map(authoring_generation => ({ provider: { authoring_generation } })),
      ...[...(conversation.operatorMessageCheckpointAdmitted ? [undefined] : []), null, {}, [], { terminal: false }, { terminal: "true" },
        { terminal: true, context_publication: {} }].map(pending_handoff => ({ provider: { pending_handoff } })),
    ]) {
      const invalid = JSON.parse(JSON.stringify(run));
      if (change.provider) Object.assign(invalid.interface_state[ACTIVE_PROVIDER], change.provider);
      else Object.assign(invalid, change);
      assert.throws(() => ready(invalid, ACTIVE_PROVIDER, conversation), undefined, JSON.stringify(change));
    }
  }
  assert.throws(() => ready(run, ACTIVE_PROVIDER, { ...fresh, operatorMessageCheckpointAdmitted: true }));
  const untouched = activeRun();
  untouched.status = "idle";
  assert.doesNotThrow(() => ready(untouched, ACTIVE_PROVIDER, fresh));
  untouched.interface_state[ACTIVE_PROVIDER].authoring_generation = 7;
  assert.doesNotThrow(() => ready(untouched, ACTIVE_PROVIDER, fresh));
  assert.throws(() => ready(untouched, ACTIVE_PROVIDER, spent), /completed provider handoff evidence/);
  run.interface_state[ACTIVE_CLIENT].client_surface_role = "home";
  const document = { value: run, text: JSON.stringify(run) };
  const first = { id: "fixture.first", content: Array(32).fill(1) };
  const second = { id: "fixture.second", content: Array(32).fill(2) };
  const sidecar = {
    schema_version: 1, workspace: "octopus_os", component: "client", interface: ACTIVE_CLIENT,
    owner_interface: ACTIVE_CLIENT, surface_role: "home", authoring_profile: {
      ee_id: run.ee, worker_choice_interface: run.interface_state[ACTIVE_CLIENT].worker_choice_interface,
      door_host_slots: {}, payload_artifact_bindings: [first],
      publication_evidence: [{ pin: { collection: "artifact-system-library", entry_id: "fixture-context", revision: "a".repeat(64) }, entry: { id: "fixture-context" } }],
    },
  };
  fs.writeFileSync(sidecarPath, JSON.stringify(sidecar));
  const [prepare, check, intent, digest] = [prepareOperatorPayloadArtifactBindings, assertPreparedOperatorPayloadSelection,
    readOrCreateOperatorMessageIntent, operatorMessageRequestDigest];
  const request = { providerModel: "gpt-5.6-sol" };
  const resolved = { clientSurfaceRole: "home", providerReference: "world_provider_codex" };
  const input = { config, request, existingRunDocument: document, artifactRoot: path.join(root, "artifacts"),
    chatInterfaceId: ACTIVE_CLIENT, runId: ACTIVE_RUN, messageId: "msg_payload_second", clientSurfaceRole: "home",
    sourceRequestDigest: digest(config, request, resolved) };
  let calls = 0;
  const options = { registryRoot: () => registryRoot, preparePayloadArtifactBindings: async (selection) => {
    calls += 1;
    assert.equal(selection.requestedPayloadArtifactBindings, undefined);
    assert.equal(JSON.stringify(selection.payloadArtifactBindings), JSON.stringify([first]));
    return [...selection.payloadArtifactBindings, second];
  } };
  const prepared = await prepare(options, input);
  assert.equal(JSON.stringify(prepared.bindings), JSON.stringify([first, second]));
  await check(prepared, document);
  const finalDigest = digest(config, { ...request, payloadArtifactBindings: prepared.bindings }, resolved);
  const intentInput = { ...input, messageBytes: Buffer.from("same message"), requestDigest: finalDigest,
    payloadArtifactBindings: prepared.bindings };
  const intentPath = await intent(intentInput);
  const intentBytes = fs.readFileSync(intentPath);
  sidecar.authoring_profile.payload_artifact_bindings = [first, second];
  fs.writeFileSync(sidecarPath, JSON.stringify(sidecar));
  const replay = await prepare(options, input);
  assert.equal(calls, 1, "durable preparation does not consume staged files twice");
  assert.equal(JSON.stringify(replay.bindings), JSON.stringify(prepared.bindings));
  await intent({ ...intentInput, payloadArtifactBindings: replay.bindings });
  assert.deepEqual(fs.readFileSync(intentPath), intentBytes);
  const cleared = await prepare({ ...options, preparePayloadArtifactBindings: async (selection) => {
    assert.equal(JSON.stringify(selection.payloadArtifactBindings), "[]");
    assert.equal(selection.previousPayloadArtifactBindings.length, 2);
    return [];
  } }, { ...input, messageId: "msg_payload_clear", request: { ...request, payloadArtifactBindings: [] } });
  assert.equal(JSON.stringify(cleared.bindings), "[]");
  await assert.rejects(prepare({ ...options, preparePayloadArtifactBindings: async (selection) => [
    ...selection.payloadArtifactBindings, { ...first, content: Array(32).fill(3) },
  ] }, { ...input, messageId: "msg_payload_conflict" }), /repeated exact artifact identity/);
  await assert.rejects(prepare({ ...options, preparePayloadArtifactBindings: async () => undefined },
    { ...input, messageId: "msg_payload_missing" }), /explicit ArtifactRef array/);
  await assert.rejects(prepare({ ...options, preparePayloadArtifactBindings: async () => [] },
    { ...input, messageId: "msg_payload_dropped" }), /changed its explicitly selected base/);
  sidecar.owner_interface = config.interface;
  fs.writeFileSync(sidecarPath, JSON.stringify(sidecar));
  await assert.rejects(prepare(options, { ...input, messageId: "msg_payload_wrong_owner" }),
    /profile differs from its exact run, client or authoring owner/);
  sidecar.owner_interface = ACTIVE_CLIENT;
  fs.writeFileSync(sidecarPath, JSON.stringify(sidecar));
  const changed = { ...document, text: `${document.text}\n` };
  await check(replay, changed);
  for (const field of ["id", "ee", "worker", "client", "declaration"]) {
    const value = structuredClone(document.value);
    if (field === "client") value.interface_state[ACTIVE_CLIENT].provider_runtime_interface = ACTIVE_CLIENT;
    else if (field === "declaration") value.interface_state[ACTIVE_PROVIDER].run_authoring_binding.checkpoints = [];
    else value[field] += field === "worker" ? " " : "_changed";
    await assert.rejects(check(replay, { value, text: JSON.stringify(value) }), /identity or input authority changed|trimmed/);
  }
  fs.writeFileSync(sidecarPath, `${JSON.stringify(sidecar)}\n`);
  await assert.rejects(check(replay, document), /private profile preimage changed/);
  console.log(`payload staging preparation retained at ${root}`);
});


test("operator ledger pages stop reading at their page while retaining exact replay offsets", async t => {
  const root = await fs.promises.mkdtemp(path.join("/tmp", "octopus-ledger-page-"));
  t.after(() => fs.promises.rm(root, { recursive: true, force: true }));
  const file = path.join(root, "movements.jsonl");
  const rows = Array.from({ length: 2000 }, (_, seq) => movement({ seq, trace: "🎙".repeat(128), artifact: { id: `message:${seq}`, content: "a".repeat(64) } }));
  const bytes = Buffer.from(rows.map(JSON.stringify).join("\r\n") + "\r\n");
  await fs.promises.writeFile(file, bytes);
  const open = fs.promises.open;
  let requestedBytes = 0;
  fs.promises.open = async (...args) => {
    const handle = await open(...args);
    if (args[0] !== file) return handle;
    return { stat: () => handle.stat(), close: () => handle.close(), read: (buffer, offset, length, position) => {
      requestedBytes += length;
      return handle.read(buffer, offset, Math.min(length, 4093), position);
    } };
  };
  try {
    const input = { afterSeq: -1, cursor: { offset: 0, seq: null }, interfaceId: runtime().interface, runId: runtime().run, limit: 2, movementsPath: file };
    const read = readLedgerOperatorRows;
    const first = await read(input);
    assert.equal(JSON.stringify(first.rows.map(row => row.seq)), "[0,1]");
    assert.ok(requestedBytes <= 64 * 1024, `bounded first page requested ${requestedBytes} bytes of ${bytes.length}`);
    const second = await read({ ...input, cursor: first.cursor });
    assert.equal(JSON.stringify(second.rows.map(row => row.seq)), "[2,3]");
    assert.equal(second.cursor.offset, Buffer.byteLength(rows.slice(0, 4).map(JSON.stringify).join("\r\n") + "\r\n"));
  } finally { fs.promises.open = open; }
});


test("empty Project planning stays available while executable Task publication requires host bindings", async t => {
  const root = await fs.promises.mkdtemp(path.join("/tmp", "octopus-project-host-preflight-"));
  t.after(() => fs.promises.rm(root, { recursive: true, force: true }));
  const fixture = await createAuthoringFixture(root), handed = [];
  await writeAuthoringFixtureJson(path.join(root, "host.json"), { host_manifest_version: 1, root_refs: { run_records_root: fixture.runsRoot, host_filesystem_root: "/" } });
  await planOperatorChatSurface(fixture.options, { chatId: "emptyproject", cycle: "emptyproject", eeId: fixture.eeId,
    providerInterfaceId: "world_provider_codex", providerModel: "gpt-5.6-sol", workerInterfaceId: "system_worker_coder_max",
    clientSurfaceRole: "task", selectedWorkerEnvironmentKinds: ["filesystem_workspace"], doorHostSlots: {} });
  const request = { ...newTaskRequest(fixture.eeId), messageId: "hostpreflight001", selectedWorkerEnvironmentKinds: ["filesystem_workspace"] };
  await assert.rejects(submitOperatorTurn(newTaskOptions(fixture, handed), request), /unbound host root slot 'workspace_root'/);
  assert.deepEqual(await fs.promises.readdir(fixture.runsRoot), []);
  assert.deepEqual(await fs.promises.readdir(path.join(fixture.registryRoot, "tool")), []);
  assert.deepEqual(await fs.promises.readdir(path.join(fixture.registryRoot, "interface")), ["fixture_room"]);
  assert.equal(handed.length, 0);
  const doorPath = path.join(fixture.registryRoot, "interface/fixture_room/interface.json");
  const door = JSON.parse(await fs.promises.readFile(doorPath, "utf8"));
  door.interface_driver_config = { host_slots: { workspace_root: { path: root } } };
  await writeAuthoringFixtureJson(doorPath, door);
  const submitted = submitOperatorTurn(newTaskOptions(fixture, handed), request);
  const run = JSON.parse(await fs.promises.readFile(await waitForRunRecord(fixture.runsRoot), "utf8"));
  const server = await listenForInterface(root, Object.keys(run.interface_state)[0]);
  try { assert.equal((await submitted).runId, run.id); assert.equal(handed.length, 1); }
  finally { await closeServer(server); }
});

test("task host preflight refuses missing required slots before publication and preserves valid rooms", async t => {
  const fs = require("node:fs/promises"), os = require("node:os");
  const { validatePlannedHostBindings } = require("./client/hostBindings.cjs");
  const registryRoot = path.resolve(__dirname, "../..");
  const transport = await fs.readFile(path.join(registryRoot, "../crates/octopus-interface-transport/src/lib.rs"), "utf8");
  const formation = transport.slice(transport.indexOf("fn life_root_slots("), transport.indexOf("fn prepare_interface_local_ipc_root("));
  const formedSlots = [...formation.matchAll(/slots\.insert\(\s*(?:"([^"]+)"|([A-Z_]+))\.to_string\(\)/g)].map(([, literal, constant]) =>
    literal ?? transport.match(new RegExp(`const ${constant}: &str = "([^"]+)"`))[1]);
  const preflightSource = await fs.readFile(path.join(__dirname, "client/hostBindings.cjs"), "utf8");
  const clientSlots = JSON.parse(`[${preflightSource.match(/const hostFormedSlots = new Set\(\[([\s\S]*?)\]\)/)[1].replace(/,\s*$/, "")}]`);
  assert.deepEqual(clientSlots.sort(), [...new Set(formedSlots)].sort());
  const home = await fs.mkdtemp(path.join(os.tmpdir(), "octopus-host-preflight-"));
  t.after(() => fs.rm(home, { recursive: true, force: true }));
  const host = roots => fs.writeFile(path.join(home, "host.json"), JSON.stringify({ host_manifest_version: 1, root_refs: roots }));
  const read = async (kind, id, file) => JSON.parse(await fs.readFile(path.join(registryRoot, kind, id, file), "utf8"));
  const original = await read("interface", "system_workspace_env", "interface.json");
  const life = await read("interface", "system_workspace_env", "life.json");
  const tool = await read("tool", "workspace.read_file", "tool.json");
  const door = "preflight_private_door", toolId = "preflight.read_file";
  const plan = { requiredToolIds: [toolId], writes: [] };
  const write = (kind, id, file, document) => ({ filePath: path.join(registryRoot, kind, id, file), content: JSON.stringify(document) });
  function stage(overrides, selectedLife = life, selectedTool = tool) {
    plan.writes = [write("interface", door, "interface.json", { ...original, id: door, interface_driver_config: { host_slots: overrides } }),
      write("interface", door, "life.json", selectedLife), write("tool", toolId, "tool.json", { ...selectedTool, id: toolId, interface: door })];
  }
  const check = () => validatePlannedHostBindings({ octopusHome: home, registryRoot, plan });
  await host({});
  stage({ host_filesystem_root: { path: "/" } });
  await assert.rejects(check(), /requires unbound host root slot 'workspace_root'/);
  stage({ workspace_root: { path: home } });
  await check();
  await host({ workspace_root: home }); stage({}); await check();
  await host({}); stage({ workspace_root: { path: "relative" } }); await assert.rejects(check(), /must be absolute/);
  const optionalLife = structuredClone(life);
  for (const process of optionalLife.processes) {
    for (const field of ["read_root_slots", "write_root_slots", "executable_root_slots"]) {
      process.authority[field] = process.authority[field].filter(slot => slot !== "workspace_root");
      process.authority[`optional_${field}`].push("workspace_root");
    }
    process.coordinate_root_slots = process.coordinate_root_slots.filter(slot => slot !== "workspace_root");
  }
  stage({}, optionalLife); await assert.rejects(check(), /workspace_root.*tool preflight.read_file/);
  stage({}, JSON.parse(JSON.stringify(optionalLife).replaceAll("workspace_root", "analysis.root")),
    JSON.parse(JSON.stringify(tool).replaceAll("workspace_root", "analysis.root")));
  await assert.rejects(check(), /analysis\.root.*tool preflight.read_file/);
  const optionalTool = structuredClone(tool); optionalTool.process.authority.read_root_slots = ["registry_worker_environment_kind_root"];
  stage({}, optionalLife, optionalTool); await check();
  const renamedLife = JSON.parse(JSON.stringify(life).replaceAll("workspace_root", "analysis_root"));
  stage({}, renamedLife); await assert.rejects(check(), /unbound host root slot 'analysis_root'/);
  stage({ analysis_root: { path: home } }, renamedLife, optionalTool); await check();
  for (const [id, toolName, roots] of [
    ["system_coder_env", "coder.read_file", { repo_root: home, worktree_root: home }],
    ["system_room", "file.read", { repo_root: home, run_records_root: home, ee_instances_root: home }],
    ["system_web_search", "web.search", { repo_root: home }],
  ]) {
    await host(roots);
    const neighbour = { ...plan, requiredToolIds: [toolName], writes: [] };
    await validatePlannedHostBindings({ octopusHome: home, registryRoot, plan: neighbour });
  }
});
