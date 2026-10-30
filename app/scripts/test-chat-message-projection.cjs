const assert = require('node:assert/strict');
const { spawnSync } = require('node:child_process');
const { scheduledInterfacesProjection } = require('../electron/projection/scheduledInterfaces.cjs');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { test } = require('node:test');
const { blake3 } = require('@noble/hashes/blake3');
const { createProjectionContext } = require('../electron/projection.cjs');
const { operatorChatEventKind, operatorChatEventText, operatorChatProjectedRowId, operatorChatRunProvider } = require('../electron/projection/operatorChatEvents.cjs');
const { compareMovementOrder } = require('../electron/projection/movementRows.cjs');

const repoRoot = path.resolve(__dirname, '../..');
const protocol = { inputLabel: 'operator.message', terminalOutputLabel: 'interface.artifact.output', errorOutputLabel: 'interface.artifact.error',
  toolCallLabel: 'tool.call', hiddenActivityLabels: [] };

function projectionStore(overrides = {}) {
  return Object.assign(createProjectionContext({ resolveRepoRoot: () => repoRoot, octopusHome: () => os.tmpdir() }), overrides);
}
function projectionFixture(fields = {}) {
  return { octopusHome: os.tmpdir(), registryRoot: path.join(os.tmpdir(), 'registry'), operatorChatProtocol: protocol, descriptors: { interface: [{ id: 'provider', role: 'provider' }, { id: 'other-provider', role: 'provider' }], ee_template: [] },
    descriptorIndexes: { interface: new Map() }, eeInstances: [], runRecords: [], clientAgentCatalog: { document: { agents: [] } },
    structureMemo: new Map(), ledgerMemo: new Map(), ...fields };
}

test('durable metadata resolves exact binding refs and rejects ambiguous or changed content', (t) => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'octopus-metadata-'));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  fs.mkdirSync(path.join(root, 'metadata'));
  const refs = [{ id: 'bound', content: Array(32).fill(1) }];
  const bytes = Buffer.from(JSON.stringify(refs));
  const hash = Buffer.from(blake3(bytes)).toString('hex');
  const contentFile = path.join(root, 'metadata', hash);
  fs.writeFileSync(contentFile, bytes);
  const row = { seq: 0, lamport: 1, recorded_at_unix_ms: 1, trace: 'metadata', kind: 'InterfaceOutbound',
    from: { Interface: 'provider' }, to: { Interface: 'worker' }, artifact: null,
    invocation_metadata: { grant: {}, artifact_binding_refs_content: hash } };
  const store = projectionStore();
  const read = (value = row) => store.parseCanonicalMovement(JSON.stringify(value), path.join(root, 'movements.jsonl'), 1);
  assert.deepEqual(read().invocation_metadata.artifact_binding_refs, refs);
  assert.deepEqual(read().invocation_metadata.artifact_binding_refs, refs);
  assert.throws(() => read({ ...row, invocation_metadata: { ...row.invocation_metadata, artifact_binding_refs: refs } }), /both inline and content-addressed/);
  fs.writeFileSync(contentFile, Buffer.alloc(bytes.length, 32));
  assert.throws(() => read(), /integrity mismatch/);
  fs.rmSync(contentFile);
  assert.throws(() => read(), /content is invalid/);
});

test('Triggers includes bound event sensors without treating them as cron activations', async (t) => {
  const sensor = (id) => ({ id, interface_type: 'world', role: 'sensor', direction: 'inbound', source_names: ['issue.opened'] });
  const projection = projectionFixture({
    descriptors: { interface: [sensor('world_events'), sensor('world_unused'),
      { ...sensor('world_provider'), role: 'provider' },
      { ...sensor('world_mail'), role: 'service', direction: 'bidirectional', source_names: ['mail'],
        interface_driver_config: { private_config: { task_input_labels: ['mail.event'] } } },
      { ...sensor('world_drive'), role: 'service' }], ee_template: [] },
    runRecords: [
      { run_id: 'task', checkpoints: [{ status: 'projected', interface: 'world_events' }, { status: 'projected', interface: 'world_mail' },
        { status: 'projected', interface: 'world_drive' }] },
      { run_id: 'old', checkpoints: [{ status: 'made', interface: 'world_unused' }] },
    ],
  });
  const event = { ...movement(5, 'issue.opened', {}, { from: 'world_events', kind: 'InterfaceArrival' }),
    invocation_metadata: { runtime: { run: 'task' } } };
  const entry = { runId: 'task', chatInterfaceId: 'chat', surfaceRole: 'task' };
  const store = projectionStore({
    operatorOsScope: () => ({ client_runs: [{ run_id: 'task', interface_id: 'chat' }] }),
    operatorChatRunIndex: async () => [entry], operatorChatTitle: async () => 'Review new issues',
    readIndexedInterfaceArtifactRows: async (_projection, run, id, labels) => {
      assert.equal(run, '');
      if (id === 'world_mail') {
        assert.deepEqual([...labels], ['mail.event']);
        return [{ ...movement(7, 'mail.event', {}, { from: 'world_mail', kind: 'InterfaceArrival' }), invocation_metadata: { runtime: { run: 'task' } } },
          { ...movement(8, 'mail.result', {}, { from: 'world_mail', kind: 'InterfaceArrival' }), invocation_metadata: { runtime: { run: 'task' } } }];
      }
      assert.equal(id, 'world_events'); assert.equal(labels.size, 0);
      return [event, { ...event, seq: 6, kind: 'DeliveryRefused' }];
    },
  });
  t.after(() => store.close());
  const rows = await store.scheduledInterfaceRows(projection);
  assert.deepEqual(rows.map((row) => row.interface_id), ['world_events', 'world_mail']);
  assert.equal(rows[1].last_movement_seq, 7);
  assert.deepEqual(rows[1].event_source.sources, ['mail']);
  assert.equal(rows[0].valid, true);
  assert.equal(rows[0].clone_eligible, false);
  assert.equal(rows[0].schedule, undefined);
  assert.equal(rows[0].last_movement_seq, 5);
  assert.deepEqual(rows[0].event_source, { sources: ['issue.opened'], targets: [
    { run_id: 'task', chat_interface_id: 'chat', title: 'Review new issues' },
  ] });
});

test('trigger history groups delivery edges and uses the same task title as All tasks', async (t) => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'oct-trigger-history-'));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const entry = { runId: 'task', chatInterfaceId: 'chat', surfaceRole: 'task', writable: true, inbound: [], activity: [] };
  const projection = projectionFixture({ octopusHome: root, registryRoot: path.join(root, 'registry') });
  const store = projectionStore({ operatorOsScope: () => ({ client_runs: [{ run_id: 'task', interface_id: 'chat' }, { run_id: 'other', interface_id: 'other_chat' }] }),
    operatorChatRunIndex: async () => [entry, { ...entry, runId: 'other', chatInterfaceId: 'other_chat' }] });
  t.after(() => store.close());
  const edge = (seq, run, kind, artifact = 'firing') => ({ ...movement(seq, 'cron.current_time', {}, { from: 'trigger', kind }),
    artifact: { id: `cron.current_time:${artifact}` }, invocation_metadata: { runtime: { run } } });
  const edges = [edge(1, 'controller', 'InterfaceArrival'), edge(2, 'task', 'InterfaceArrival'), edge(3, 'task', 'InterfaceOutbound')];
  entry.activity = [edges[1]];
  projection.descriptorIndexes.interface.set('trigger', { activation_sidecars: [{ activations: [{ activation_driver_ref: 'cron_life', config: { run_template: { id: 'run_review' } } }] }] });
  const title = () => store.operatorChatTitle(projection, entry, new Map(), protocol);
  const read = (rows = edges) => store.scheduledInterfaceEvidence(projection, 'trigger', { schedule: {} }, rows);
  assert.equal(await title(), 'Review');
  assert.deepEqual((await read()).deliveries.map(row => [row.run_id, row.chat_interface_id, row.title]), [['task', 'chat', 'Review']]);
  const directory = path.join(root, 'state/change-journals/task'); fs.mkdirSync(directory, { recursive: true });
  fs.writeFileSync(path.join(directory, 'journal.json'), JSON.stringify({ schema_version: 1, run: 'task', outcome: 'unchanged', summary: 'Reviewed the activity window', changes: [] }));
  assert.equal((await read()).deliveries[0].title, await title());
  assert.equal(await title(), 'Reviewed the activity window');
  assert.equal((await read([...edges, edge(4, 'other', 'InterfaceArrival'), edge(5, 'other', 'InterfaceOutbound')])).deliveries.length, 2);
  assert.equal((await read([...edges, edge(6, 'task', 'InterfaceArrival', 'second')])).deliveries.length, 2);
  assert.equal((await read([edges[0]])).deliveries[0].chat_interface_id, '');
  assert.equal(require('../electron/projection/registryRows.cjs').projectionTruthChange('state/change-journals/task/journal.json'), 'runs');
});
const cronOwner = path.resolve(__dirname, "../../registry/interface/world_cron");
const cronDueMs = 1_800_000_000_000;
const prepareCronState = `
import importlib.util, json, os, pathlib, sys
root, cronOwner, due = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2]), int(sys.argv[3])
os.environ.update(
    OCTOPUS_INTERFACE_ID="world_cron", OCTOPUS_INTERFACE_DIR=str(root / "registry/interface/world_cron"),
    OCTOPUS_VAR_DIR=str(root / "interfaces/world_cron"),
    OCTOPUS_INTERFACE_DESCRIPTOR_JSON=(root / "registry/interface/world_cron/interface.json").read_text(),
    OCTOPUS_INTERFACE_HOST_IO_LIMITS=json.dumps({key: 1048576 for key in
        ("max_control_frame_bytes", "max_manifest_bytes", "max_payload_bytes", "max_stderr_bytes")}),
)
spec = importlib.util.spec_from_file_location("cron_projection_test", cronOwner / "cron_life.py")
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
schedule = module.load_schedule()
module.commit_interval_state(schedule, "waiting", due * 1_000_000)
print(schedule["fingerprint"])
`;

for (const mode of ["existing_run", "new_run"]) {
  test(`Triggers projects owner-written interval state for ${mode}`, async (t) => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), "oct-cron-projection-"));
    t.after(() => fs.rmSync(root, { recursive: true, force: true }));
    const directory = path.join(root, "registry/interface/world_cron");
    fs.mkdirSync(directory, { recursive: true });
    const descriptor = JSON.parse(fs.readFileSync(path.join(cronOwner, "interface.json")));
    const activation = JSON.parse(fs.readFileSync(path.join(cronOwner, "activation.json")));
    activation.activations[0].enabled = true;
    const config = activation.activations[0].config;
    config.schedule = { interval_seconds: 7200, fire_on_start: false, timezone: "UTC",
      reminder_env: "", reminder_default: "Review café / проверка 🐙\u007f" };
    if (mode === "new_run") config.target = { mode, ee_id: "ee_existence", worker_interface_id: "worker",
      provider_interface_id: "provider", reasoning_effort: "high", client_surface_role: "task",
      id_prefix: "review", door_host_slots: {} };
    const activationPath = path.join(directory, "activation.json");
    fs.writeFileSync(activationPath, JSON.stringify(activation));
    fs.writeFileSync(path.join(directory, "interface.json"), JSON.stringify(descriptor));
    const result = spawnSync("python3", ["-B", "-c", prepareCronState, root, cronOwner, String(cronDueMs)], { encoding: "utf8" });
    assert.equal(result.status, 0, result.stderr);
    const store = scheduledInterfacesProjection({
      indexedArtifactLabelRows: async () => [], operatorOsScope: () => ({ client_runs: [] }),
    });
    const projection = { octopusHome: root, structureMemo: new Map(), descriptors: { interface: [
      { ...descriptor, activation_sidecars: [{ ...activation, descriptor_path: activationPath }] },
    ] }, runRecords: [config.run_template] };
    const read = async () => { projection.structureMemo.clear(); return (await store.scheduledInterfaceRows(projection))[0]; };
    const current = await read();
    assert.equal(current.valid, true, current.error);
    assert.equal(current.next_run_at_ms, cronDueMs);
    assert.equal(current.next_run_source, "driver_state");
    config.schedule.cron = null;
    const explicitNull = await read();
    assert.equal(explicitNull.next_run_at_ms, cronDueMs, "omitted cron and null are the same owner configuration");
    if (mode === "new_run") {
      config.target.interface_private_state_root = fs.realpathSync(path.join(root, "interfaces"));
      assert.equal((await read()).next_run_at_ms, cronDueMs, "omitted private root resolves to this Home");
      const planner = descriptor.interface_driver_config.private_config.task_planner_executable;
      descriptor.interface_driver_config.private_config.task_planner_executable = "bin/changed-planner";
      assert.equal((await read()).next_run_at_ms, null, "different planner must not validate old state");
      descriptor.interface_driver_config.private_config.task_planner_executable = planner;
    } else {
      delete descriptor.interface_driver_config;
      delete projection.descriptors.interface[0].interface_driver_config;
      assert.equal((await read()).next_run_at_ms, cronDueMs, "existing-run triggers do not use a planner");
    }
    config.schedule.reminder_default = "Changed assignment";
    assert.equal((await read()).next_run_at_ms, null, "different assignment must not validate old state");
  });
}

function movement(seq, label, payload, { from = 'provider', to = 'chat', kind = 'InterfaceOutbound' } = {}) {
  const hex = seq.toString(16).padStart(64, '0');
  return { seq, recorded_at_unix_ms: seq * 100, kind, from: { Interface: from }, to: { Interface: to },
    artifact: { id: `${label}:${hex}`, content: hex }, invocation_metadata: { runtime: { run: 'run', interface: to } }, payload };
}

test('Stop aliases match actual tasks titles without reading terminal history', async () => {
  await require('./build-cli.cjs').buildCli();
  const { createClient } = require('../cli/client.cjs');
  const entries = ['home', 'task', 'named'].map((name, index) => ({
    runId: `run_${name}`, chatInterfaceId: `chat_${name}`, surfaceRole: name === 'home' ? 'home' : 'task', writable: true, activity: [], record: {},
    inbound: [movement(index + 1, protocol.inputLabel, { text: name === 'home' ? 'Review\n notes' : 'Not the authored title' }, { from: `chat_${name}`, to: 'provider', kind: 'InterfaceArrival' })],
    outbound: [movement(index + 20, protocol.terminalOutputLabel, { reply: `Answer for ${name}` }, { to: `chat_${name}` })],
  }));
  entries[0].inbound.unshift(movement(10, protocol.inputLabel, { text: 'Later input' }, { from: 'chat_home', to: 'provider', kind: 'InterfaceArrival' }));
  entries[0].outbound.push(movement(30, protocol.terminalOutputLabel, { reply: 'Delivered elsewhere' }, { to: 'chat_other' }));
  const sidecar = (name) => ({ workspace: 'octopus_os', presentation: { name } });
  const projection = projectionFixture({ descriptors: { ee_template: [], interface: [
    { id: 'chat_home', octopus_os_sidecar: sidecar('Home') }, { id: 'chat_task', octopus_os_sidecar: sidecar('Task title') },
    { id: 'chat_named', octopus_os_sidecar: sidecar('run_named') }] } });
  const scope = { status: 'ready', home_run_id: 'run_home', home_interface_id: 'chat_home',
    client_runs: entries.map(entry => ({ run_id: entry.runId, interface_id: entry.chatInterfaceId, surface_role: entry.surfaceRole, archived: false })) };
  let allowPayload = true, allowHistory = true, inputReads = 0;
  const projectionReads = projectionStore({
    operatorOsScope: () => scope, operatorChatRunIndex: async () => entries, operatorOsApplications: () => ({}),
    operatorChatTurnState: async () => { assert(allowHistory, 'control aliases must not read turn history'); return { open: false, state: 'idle' }; },
    resolveLedgerArtifactPayload: async row => {
      if (row.kind === 'InterfaceOutbound') { assert(allowHistory, 'control aliases must not read terminal history'); return row.payload; }
      assert(allowPayload, 'scope and last selectors must not read payloads'); inputReads++;
      assert.equal(row, entries[0].inbound[1], 'only the first Home input supplies a derived title'); return row.payload;
    },
  });
  const store = { async read({ method, params }) {
    if (method === 'projection.operator.os.scope') return scope;
    assert.equal(method, 'projection.operator.chats.list');
    return { chats: params?.control_targets_only
      ? await projectionReads.operatorChatControlTargets(projection, params.include_titles)
      : await projectionReads.buildOperatorChatsList(projection) };
  }, invalidate() {}, close() {} };
  const client = createClient({ home: os.tmpdir(), packageRoots: {}, status: async () => ({ runtimeReady: true }) }, {
    store, metadata: {}, loader: { load: async () => ({ submitOperatorTurnControl: async (_options, input) => ({ ...input, verdict: { admitted: { seq: 1 } } }) }) },
  });
  try {
    const tasks = await client.tasks();
    assert.deepEqual(Object.fromEntries(tasks.map(task => [task.id, task.title])), {
      run_home: 'Review notes', run_named: 'run_named', run_task: 'Task title',
    });
    assert.equal(inputReads, 1);
    allowHistory = false; allowPayload = false;
    for (const [selector, id] of [['home', 'run_home'], ['run_task', 'run_task'], ['last', 'run_named']]) {
      assert.equal((await client.stop(selector)).runId, id);
    }
    assert.equal(inputReads, 1);
    allowPayload = true;
    const aliases = await projectionReads.operatorChatControlTargets(projection, true);
    for (const task of tasks) {
      assert.equal(aliases.find(row => row.run_id === task.id).title, task.title);
      assert.equal((await client.stop(task.title)).runId, task.id);
    }
    projection.descriptors.interface[1].octopus_os_sidecar.presentation.name = tasks.find(task => task.isHome).title;
    await assert.rejects(client.stop('Review notes'), /ambiguous/);
    allowHistory = true;
    const listed = await projectionReads.buildOperatorChatsList(projection);
    assert.deepEqual(listed.map(chat => [chat.run_id, chat.last_terminal_message.text, chat.last_terminal_message.movement_seq]).sort(),
      [['run_home', 'Answer for home', 20], ['run_named', 'Answer for named', 22], ['run_task', 'Answer for task', 21]]);
  } finally { await client.close(); }
});

test('artifact cache separates raw bytes, projection contracts and exact artifact identities', async (t) => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'octopus-artifact-cache-')), blobs = path.join(root, 'blobs');
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  fs.mkdirSync(blobs);
  const blob = (bytes) => { const hex = Buffer.from(blake3(bytes)).toString('hex'); fs.writeFileSync(path.join(blobs, hex), bytes); return hex; };
  const artifact = (label, value) => {
    const payloadHex = blob(Buffer.from(JSON.stringify(value)));
    const manifestHex = blob(Buffer.from(JSON.stringify({ label, payload_path: path.join(blobs, payloadHex), payload_content_hash: payloadHex })));
    return { row: { source_file: path.join(root, 'movements.jsonl'), artifact: { id: `${label}:${manifestHex}`, content: manifestHex } }, payloadHex };
  };
  const contract = { schema_version: 1, json_object_labels: ['reply'], excluded_root_fields: ['private'], max_source_bytes: 100, max_projected_bytes: 100 };
  const store = projectionStore();
  const reply = artifact('reply', { text: 'Reply', private: 'original' }), binary = artifact('binary', { text: 'Reply', private: 'original' });
  assert.equal((await store.resolveLedgerArtifactPayload(reply.row)).private, 'original');
  assert.deepEqual(await store.resolveLedgerArtifactPayload(reply.row, { projection: contract }), { text: 'Reply' });
  assert.equal((await store.resolveLedgerArtifactPayload(binary.row, { projection: contract })).private, 'original');
  await assert.rejects(store.resolveLedgerArtifactPayload({ ...reply.row, artifact: { ...reply.row.artifact, id: `reply:${'f'.repeat(64)}` } }, { projection: contract }));
  await assert.rejects(store.resolveLedgerArtifactPayload(reply.row, { projection: { ...contract, max_source_bytes: 1 } }));
  await assert.rejects(store.resolveLedgerArtifactPayload(reply.row, { projection: undefined }));
  fs.rmSync(path.join(blobs, reply.payloadHex));
  assert.equal((await store.resolveLedgerArtifactPayload(reply.row)).private, 'original');
  assert.deepEqual(await store.resolveLedgerArtifactPayload(reply.row, { projection: contract }), { text: 'Reply' });
});

const delta = (seq, text, item, phase, from) => movement(seq, 'agent.message.delta', { text, item_id: item, turn_id: 'turn', phase, lifecycle: 'streaming' }, { from });
const commentary = (seq, text, item) => movement(seq, 'turn.agent_message', { text, item_id: item, turn_id: 'turn', phase: 'commentary', lifecycle: 'completed' });
const partialAnswer = (seq, text, item) => movement(seq, 'turn.agent_message', { text, item_id: item, turn_id: 'turn', phase: 'partial_answer', lifecycle: 'completed' });
const toolCall = (seq, status) => movement(seq, 'turn.dynamic_tool_call', { tool: 'Read', call_id: 'read', status, turn_id: 'turn' });
const finalAnswer = (seq, text) => movement(seq, protocol.terminalOutputLabel, { reply: text, item_id: 'final', turn_id: 'turn', phase: 'final_answer', lifecycle: 'completed' });

async function project(rows, writable) {
  const store = projectionStore({ resolveLedgerArtifactPayloads: async values => new Map(values.map(value => [value, value.payload])) });
  const entry = { runId: 'run', chatInterfaceId: 'chat', writable, inbound: [], outbound: rows, activity: rows,
    record: { interface_state: { chat: { provider_runtime_interface: 'provider' } } } };
  return JSON.parse(JSON.stringify(await store.buildSingleOperatorChatMessages(projectionFixture(), entry, protocol)));
}

test('Home voice delegations open separate evidenced user turns without accepting another voice interface', async () => {
  const inbound = [movement(1, protocol.inputLabel, { text: 'Typed request' }, { from: 'chat', kind: 'InterfaceArrival' }),
    movement(20, 'realtime.delegation', { input_text: 'Enable Insights', delegation_id: 'voice-request', correlation_id: 'call' }, { from: 'voice-input', kind: 'InterfaceArrival' })];
  const outbound = [finalAnswer(2, 'First reply'), finalAnswer(21, 'Enabled')];
  outbound[1].payload.in_reply_to = inbound[1].artifact;
  const entry = { runId: 'run', chatInterfaceId: 'chat', writable: true, inbound, outbound, activity: [...inbound, ...outbound],
    record: { interface_state: { chat: { client_surface_role: 'home', provider_runtime_interface: 'provider', voice_context: { delegation_interface_id: 'voice-input' } } } } };
  const store = projectionStore({ resolveLedgerArtifactPayloads: async rows => new Map(rows.map(row => [row, row.payload])) });
  const messages = await store.buildSingleOperatorChatMessages(projectionFixture(), entry, protocol);
  assert.deepEqual(messages.map(message => message.text), ['Typed request', 'First reply', 'Enable Insights', 'Enabled']);
  assert.equal(messages[2].event_kind, 'message');
  assert.equal(messages[2].event_data.voice.delegation_id, 'voice-request');
  assert.deepEqual(messages[3].event_data.voice, { correlation_id: 'call', delegation_id: 'voice-request' });
  for (const patch of [{ in_reply_to: { ...inbound[1].artifact, content: 'f'.repeat(64) } }, { in_reply_to: { ...inbound[1].artifact, id: 'another' } }]) {
    Object.assign(outbound[1].payload, patch, { voice: { correlation_id: 'unproven', delegation_id: 'unproven' } });
    assert.equal((await store.buildSingleOperatorChatMessages(projectionFixture(), entry, protocol)).at(-1).event_data.voice, undefined);
  }
  assert.equal(messages[3].latency_ms, 100);
  assert.equal(await store.operatorChatTurnOpening(entry, inbound[1], protocol), 'waiting');
  assert.equal(await store.operatorChatTurnOpening(entry, { ...inbound[1], from: { Interface: 'other-voice' } }, protocol), null);
});

test('both chat history paths preserve interleaved item identity and replace completed snapshots', async () => {
  for (const writable of [true, false]) for (const finalText of ['', 'Done!']) {
    const messages = await project([
      delta(1, 'Pro', 'a', 'commentary'), toolCall(2, 'started'),
      delta(3, 'First', 'b', 'commentary'), delta(4, 'gress', 'a', 'commentary'),
      commentary(5, 'Progress', 'a'), toolCall(6, 'completed'),
      delta(7, ' second', 'b', 'commentary'), commentary(8, 'First second', 'b'),
      delta(9, ' late', 'a', 'commentary'), delta(10, 'Done', 'final', 'final_answer'),
      finalAnswer(11, finalText),
    ], writable);
    assert.deepEqual(messages.map(message => [message.event_kind, message.text]), [
      ['commentary', 'Progress'], ['dynamic_tool', 'Read'], ['commentary', 'First second'], ['dynamic_tool', 'Read'], ['message', finalText || 'Done']]);
    assert.deepEqual(messages.filter(message => message.event_kind === 'commentary').map(message => message.event_data.item_id), ['a', 'b']);
    assert.equal(messages[0].created_at_ms, 100); assert.equal(messages[0].movement_seq, 5);
    assert.equal(messages.at(-1).event_data.item_id, 'final'); assert.equal(messages.at(-1).terminal, true);
  }
});

test('provider identity and missing item identity never merge unrelated text', async () => {
  for (const writable of [true, false]) {
    const messages = await project([
      delta(1, 'Same', 'a', null), delta(2, 'Same', 'a', null, 'other-provider'),
      delta(3, 'Unknown', '', null), delta(4, 'Unknown', '', null),
    ], writable);
    assert.deepEqual(messages.map(message => message.text), ['Same', 'Same', 'Unknown', 'Unknown']);
  }
});

test('completed answer items retain their text without completing the turn', async () => {
  for (const phase of ['partial_answer', 'final_answer']) for (const writable of [true, false]) {
    assert.equal(operatorChatEventKind('turn.agent_message', protocol, { phase }), 'message');
    const answer = partialAnswer(2, 'Answer so far', 'partial');
    answer.payload.phase = phase;
    const messages = await project([
      delta(1, 'Answer', 'partial', phase), answer,
      toolCall(3, 'started'), finalAnswer(4, 'Complete answer'),
    ], writable);
    assert.deepEqual(messages.map(message => message.text), ['Answer so far', 'Read', 'Complete answer']);
    assert.equal(messages[0].terminal, false);
    assert.equal(messages[0].event_data.phase, phase);
    assert.equal(messages.at(-1).terminal, true);
  }
});

test('declared chat outputs expose completed items before the turn stream and fold later delivery once', async (t) => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'oct-chat-progress-'));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const declared = await require('../electron/projection/providerDescriptors.cjs').readOperatorChatProtocol(path.join(repoRoot, 'registry'));
  const rows = [commentary(2, 'Working', 'progress'), movement(4, 'turn.agent_message', {
    text: 'Done', item_id: 'final', turn_id: 'turn', phase: 'final_answer', lifecycle: 'completed',
  })];
  const projection = projectionFixture({ durableRoot: root, operatorChatProtocol: declared, ledgerRows: rows,
    ledgerRowsByRunSources: [new Map([['run', rows]])], runRecords: [{ run_id: 'run', interface_state: { chat: { provider_runtime_interface: 'provider' } } }] });
  const store = projectionStore({ operatorOsScope: () => ({ client_runs: [{ run_id: 'run', interface_id: 'chat' }] }),
    refreshLedgerProjection: async () => {}, resolveLedgerArtifactPayloads: async values => new Map(values.map(value => [value, value.payload])) });
  t.after(() => store.close());
  const read = async () => {
    projection.operatorChatRunIndexCache = null;
    const [entry] = await store.operatorChatRunIndex(projection);
    return store.buildSingleOperatorChatMessages(projection, entry, declared);
  };
  const early = await read();
  assert.deepEqual(early.map(message => [message.event_kind, message.text, message.terminal]), [['commentary', 'Working', false], ['message', 'Done', false]]);
  const records = [
    { type: 'header', schema_version: 1, seq: 0, provider_interface: 'provider', run: 'run', thread_id: 'thread', turn_id: 'turn', opened_source_movement_seq: 0, opened_at_unix_ms: 10 },
    ...[delta(1, 'Working', 'progress', 'commentary'), delta(3, 'Done', 'final', 'final_answer')].map((row, index) => ({
      type: 'event', seq: index + 1, source_movement_seq: 0, recorded_at_unix_ms: row.recorded_at_unix_ms, label: 'agent.message.delta', payload: row.payload,
    })),
    { type: 'seal', seq: 3, source_movement_seq: 5, recorded_at_unix_ms: 500, terminal_label: 'turn.completed', terminal_artifact_payload_blake3: 'a'.repeat(64) },
  ];
  rows.push(movement(5, 'turn.stream', { __raw: records.map(record => JSON.stringify(record)).join('\n') + '\n' }), finalAnswer(6, 'Done'));
  const completed = await read();
  assert.deepEqual(completed.map(message => message.text), ['Working', 'Done']);
  assert.equal(completed.at(-1).terminal, true);
});

test('chat history and status use a caught-up ledger index without rescanning history', async (t) => {
  const { DatabaseSync } = require('node:sqlite');
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'oct-chat-index-'));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const row = movement(0, 'turn.agent_message', {}), bytes = JSON.stringify(row) + '\n';
  fs.writeFileSync(path.join(root, 'movements.jsonl'), bytes);
  const db = new DatabaseSync(path.join(root, 'ledger-index.sqlite'));
  db.exec(`CREATE TABLE index_meta(key TEXT PRIMARY KEY, value TEXT);
    INSERT INTO index_meta VALUES ('schema_version', '4'), ('watermark', '1'), ('durable_end_offset', '${Buffer.byteLength(bytes)}');
    CREATE TABLE movements(seq INTEGER PRIMARY KEY, lamport INTEGER, recorded_at_unix_ms INTEGER, trace TEXT, kind TEXT,
      from_kind TEXT, from_id TEXT, to_kind TEXT, to_id TEXT, artifact_id TEXT, artifact_content TEXT,
      runtime_run TEXT, runtime_checkpoint TEXT, runtime_interface TEXT);`);
  db.prepare('INSERT INTO movements VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)')
    .run(0, 0, 0, 'index-test', 'InterfaceOutbound', 'interface', 'provider', 'interface', 'chat', row.artifact.id, row.artifact.content, 'run', 'output', 'chat');
  db.close();
  const store = projectionStore({ refreshLedgerProjection: async () => { throw new Error('A valid, caught-up index must not rescan the ledger'); } });
  t.after(() => store.close());
  const projection = projectionFixture({ durableRoot: root, operatorChatStatusCursor: -1 });
  assert.deepEqual((await store.operatorChatLedgerRows(projection, new Set(['run']))).get('run').map(row => row.seq), [0]);
  assert.equal(projection.operatorChatStatusCursor, 0);
  const delta = await store.operatorChatLedgerDelta(projection, new Set(['run']), 0);
  assert.deepEqual(delta.rowsByRun.get('run'), []);
  assert.equal(delta.nextMovementSeq, 0);
});

test('a sealed stream folds each delta position once even when it is delivered twice', async () => {
  const opened = 1_000;
  const records = [
    { type: 'header', schema_version: 1, seq: 0, provider_interface: 'provider', run: 'run', thread_id: 'thread', turn_id: 'turn', opened_source_movement_seq: 1, opened_at_unix_ms: opened },
    ...['Hel', 'lo', '!'].map((text, index) => ({ type: 'event', seq: index + 1, source_movement_seq: 1, recorded_at_unix_ms: opened + index + 1,
      label: 'agent.message.delta', payload: { text, item_id: 'answer', turn_id: 'turn', phase: 'final_answer', lifecycle: 'streaming' } })),
    { type: 'seal', seq: 4, source_movement_seq: 2, recorded_at_unix_ms: opened + 9, terminal_label: 'turn.completed', terminal_artifact_payload_blake3: 'a'.repeat(64) },
  ];
  const raw = records.map(record => JSON.stringify(record)).join('\n') + '\n';
  for (const writable of [true, false]) {
    const messages = await project([movement(2, 'turn.stream', { __raw: raw }), movement(3, 'turn.stream', { __raw: raw })], writable);
    assert.deepEqual(messages.map(message => message.text), ['Hello!']);
  }
});

test('provider labels keep their transcript meaning, row identity and movement order', () => {
  assert.equal(operatorChatEventKind(protocol.terminalOutputLabel, protocol, {}), 'message');
  assert.equal(operatorChatEventKind(protocol.errorOutputLabel, protocol, {}), 'error');
  assert.equal(operatorChatEventKind('agent.message.delta', protocol, { phase: 'commentary' }), 'commentary_delta');
  assert.equal(operatorChatEventKind('agent.message.delta', protocol, { phase: 'final_answer' }), 'message_delta');
  assert.equal(operatorChatEventKind('turn.reasoning.delta', protocol, {}), 'reasoning_delta');
  assert.equal(operatorChatEventKind('turn.context.compaction', protocol, {}), 'compaction');
  assert.equal(operatorChatEventText({ text: 'summary' }, 'compaction'), '');
  assert.equal(operatorChatProjectedRowId({ __stream: { stream_id: 'stream', stream_seq: 3 }, artifact: { id: 'turn.stream:a' } }, 'fallback'), 'stream:3');
  assert.equal(operatorChatProjectedRowId({ artifact: { id: 'interface.artifact.output:b' } }, 'fallback'), 'interface.artifact.output:b');
  assert.equal(operatorChatProjectedRowId({}, 'fallback'), 'fallback');
  const ordered = [{ seq: 4, recorded_at_unix_ms: 20 }, { seq: 9, recorded_at_unix_ms: 10 }, { seq: 2, recorded_at_unix_ms: 20 }].sort(compareMovementOrder);
  assert.deepEqual(ordered.map(row => row.seq), [9, 2, 4]);
});

test('a run reports its provider only for the exact recorded selection and its projected runtime', () => {
  const projection = projectionFixture({ descriptorIndexes: { interface: new Map([['bound', { interface_driver_config: { private_config: { model: 'gpt', reasoning_effort: 'high' } } }]]) } });
  const entry = (choice, bound = 'bound', status = 'projected') => ({ chatInterfaceId: 'chat', providerChoiceInterfaceId: 'selected', record: {
    interface_state: { chat: { provider_choice_interface: choice, provider_runtime_interface: bound } },
    checkpoints: [{ checkpoint_id: 'output', status, interface: 'bound', out_interfaces: ['chat'] }] } });
  assert.deepEqual([operatorChatRunProvider(projection, entry('selected'))].map(({ id, boundId, model, reasoningEffort }) => ({ id, boundId, model, reasoningEffort })),
    [{ id: 'selected', boundId: 'bound', model: 'gpt', reasoningEffort: 'high' }]);
  for (const candidate of [entry('another'), entry('selected', ''), entry('selected', 'bound', 'retired')]) {
    assert.equal(operatorChatRunProvider(projection, candidate).id, '');
  }
});

test('turn state opens on input, pauses on interrupt, reopens on an acknowledged resume and fails on provider failure', async () => {
  const store = projectionStore({ resolveLedgerArtifactPayload: async row => row.payload });
  const entry = { runId: 'run', chatInterfaceId: 'chat', writable: true, record: { interface_state: { chat: { provider_runtime_interface: 'provider' } } } };
  const native = (turn) => ({ thread_id: 'thread', turn_id: turn });
  const rows = [
    movement(1, protocol.inputLabel, { text: 'Go', message_id: 'm1' }, { from: 'chat', to: 'provider', kind: 'InterfaceArrival' }),
    movement(2, 'turn.started', native('t1')),
    movement(3, 'turn.control.acknowledged', { ...native('t1'), control_id: 'c1', action: 'interrupt', status: 'acknowledged' }),
    movement(4, 'turn.control.acknowledged', { ...native('t2'), control_id: 'c2', action: 'resume', status: 'acknowledged' }),
    movement(5, protocol.terminalOutputLabel, { ...native('t2'), reply: 'Done' }),
    movement(6, protocol.inputLabel, { text: 'Again', message_id: 'm2' }, { from: 'chat', to: 'provider', kind: 'InterfaceArrival' }),
    movement(7, 'turn.started', native('t3')),
    movement(8, 'provider.failure', { failure_class: 'transport', source_movement_seq: 6, details: { error: 'Provider transport closed' } }),
  ];
  const states = [];
  for (let count = 1; count <= rows.length; count += 1) {
    const reduced = await store.reduceOperatorChatTurnState({ open: false, state: 'idle', lastMovementSeq: -1, updatedAtMs: 0 }, rows.slice(0, count), entry, protocol);
    states.push(`${reduced.open ? 'open' : 'closed'}:${reduced.state}`);
  }
  assert.deepEqual(states, ['open:waiting', 'open:working', 'closed:idle', 'open:working', 'closed:idle', 'open:waiting', 'open:working', 'closed:failed']);
  const messages = await project(rows.slice(5).filter(row => row.kind === 'InterfaceOutbound'), true);
  assert.deepEqual(messages.map(message => [message.event_kind, message.text, message.error]).at(-1), ['error', 'Provider transport closed', true]);
});

test('a sealed stream without its seal row is not projected', async () => {
  const header = { type: 'header', schema_version: 1, seq: 0, provider_interface: 'provider', run: 'run', thread_id: 'thread', turn_id: 'turn', opened_source_movement_seq: 1, opened_at_unix_ms: 1_000 };
  const event = { type: 'event', seq: 1, source_movement_seq: 1, recorded_at_unix_ms: 1_001, label: 'agent.message.delta', payload: { text: 'Hel', item_id: 'answer', turn_id: 'turn' } };
  await assert.rejects(project([movement(2, 'turn.stream', { __raw: [header, event].map(row => JSON.stringify(row)).join('\n') + '\n' })], true), /no seal row/);
});


test('message revision receipt replaces only the admitted latest turn', async () => {
  const old = movement(1, protocol.inputLabel, { message_id: "old", text: "Original", attachments: [] }, { from: "chat", kind: "InterfaceArrival" });
  const edit = movement(3, protocol.inputLabel, { message_id: "edit", text: "Corrected", attachments: [], replaces_message: old.artifact }, { from: "chat", kind: "InterfaceArrival" });
  const receipt = movement(4, 'interface.context.updated', { phase: 'message_revised', replaces_message: old.artifact, source_artifact: edit.artifact });
  const entry = { runId: 'run', chatInterfaceId: 'chat', writable: true, inbound: [old, edit], outbound: [finalAnswer(2, 'Original reply')],
    record: { interface_state: { chat: { provider_runtime_interface: 'provider' } } } };
  const store = projectionStore({ resolveLedgerArtifactPayloads: async rows => new Map(rows.map(row => [row, row.payload])) });
  const read = () => store.buildSingleOperatorChatMessages(projectionFixture(), entry, protocol);
  assert.deepEqual((await read()).map(row => row.text), ['Original', 'Original reply', 'Corrected']);
  entry.outbound.push(receipt, finalAnswer(5, 'Revised reply'));
  const messages = await read();
  assert.deepEqual(messages.map(row => row.text), ['Corrected', 'Revised reply']);
  assert.equal(messages[0].event_data.message_revision_receipt.from_movement_seq, 1);
  entry.outbound.push(receipt);
  assert.deepEqual((await read()).map(row => row.text), ['Corrected', 'Revised reply']);
  entry.writable = false; entry.activity = [...entry.inbound, ...entry.outbound];
  assert.deepEqual((await read()).map(row => row.text), ['Corrected', 'Revised reply']);
  assert.equal(old.payload.text, 'Original');
});


test('assistant display edits survive replay without changing ledger messages', () => {
  const { applyAssistantTextEdits } = require('../electron/projection/assistantTextEdits.cjs');
  const edit = { provider_interface: 'provider', turn_id: 'turn', item_id: 'item', original_text: 'original', text: 'edited', edited_at_ms: 1 };
  const document = { schema_version: 1, run_id: 'run', edits: [edit] };
  const original = { run_id: 'run', role: 'assistant', event_kind: 'message', text: 'original', artifact_id: 'original-artifact',
    event_data: { turn_id: 'turn', item_id: 'item', _activity_origin: { interface_id: 'provider' } } };
  const rows = [original, { ...original, movement_seq: 100 }, { ...original, run_id: 'neighbor' }];
  const result = applyAssistantTextEdits(rows, 'run', document);
  assert.deepEqual(result.map(row => row.text), ['edited', 'edited', 'original']);
  assert.equal(original.text, 'original');
  assert.equal(result[0].artifact_id, original.artifact_id);
  assert.equal(result[0].event_data._client_text_edit.original_text, 'original');
  assert.throws(() => applyAssistantTextEdits([{ ...original, text: 'changed' }], 'run', document), /needs review/);
  assert.throws(() => applyAssistantTextEdits(rows, 'other', document), /do not match/);
  assert.throws(() => applyAssistantTextEdits(rows, 'run', { ...document, edits: [edit, edit] }), /repeat/);
  assert.equal(applyAssistantTextEdits(rows, 'run', null), rows);
});
