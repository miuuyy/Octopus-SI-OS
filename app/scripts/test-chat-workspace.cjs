const assert = require("node:assert/strict");
const path = require("node:path");
const { test, after } = require("node:test");
const { build } = require("esbuild");
const { JSDOM } = require("jsdom");

const dom = new JSDOM('<div id="root"></div>', { url: "http://fixture.invalid", pretendToBeVisual: true });
const w = dom.window;
for (const key of ["window", "document", "HTMLElement", "Element", "Node", "Event", "CustomEvent", "localStorage", "sessionStorage"]) {
  Object.defineProperty(globalThis, key, { configurable: true, value: key === "window" ? w : w[key] });
}
globalThis.requestAnimationFrame = w.requestAnimationFrame.bind(w);
w.matchMedia = () => ({ matches: false, addEventListener() {}, removeEventListener() {} });

function memoryIndexedDb() {
  const stores = new Map();
  const later = (callback) => setTimeout(callback, 0);
  const database = {
    createObjectStore: (name) => { stores.set(name, new Map()); },
    close() {},
    transaction(name) {
      const store = stores.get(name);
      const writes = [];
      const transaction = {
        aborted: false,
        error: null,
        abort() { transaction.aborted = true; later(() => transaction.onabort?.()); },
        objectStore: () => ({
          get(key) {
            const request = {};
            later(() => {
              request.result = store.has(key) ? structuredClone(store.get(key)) : undefined;
              request.onsuccess?.();
              later(() => {
                if (transaction.aborted) return;
                for (const [value, writeKey] of writes) store.set(writeKey, structuredClone(value));
                transaction.oncomplete?.();
              });
            });
            return request;
          },
          put(value, key) { writes.push([value, key]); },
        }),
      };
      return transaction;
    },
  };
  return {
    open() {
      const request = { result: database };
      later(() => {
        if (!stores.has("queues")) request.onupgradeneeded?.();
        request.onsuccess?.();
      });
      return request;
    },
  };
}
globalThis.indexedDB = memoryIndexedDb();
globalThis.BroadcastChannel = class extends BroadcastChannel { constructor(name) { super(name); this.unref(); } };
const heldLocks = new Set();
Object.defineProperty(globalThis, "navigator", { configurable: true, value: {
  userAgent: "fixture",
  locks: {
    request: async (name, callback) => { heldLocks.add(name); try { return await callback(); } finally { heldLocks.delete(name); } },
    query: async () => ({ held: [...heldLocks].map((name) => ({ name })) }),
  },
} });

const React = require("react");
const { createRoot } = require("react-dom/client");
const compiled = build({
  stdin: {
    contents: 'export { useChatWorkspace } from "./src/components/chat/useChatWorkspace"; export { ClientPreferencesProvider } from "./src/clientPreferences"; export { clientExtensionIncidentSnapshot } from "./src/extensions/incidents";',
    resolveDir: path.join(__dirname, ".."), loader: "ts",
  },
  bundle: true, platform: "node", format: "cjs", write: false, logLevel: "silent",
  define: { "import.meta.env.DEV": "false", "import.meta.env.PROD": "true" },
  loader: { ".css": "empty", ".svg": "dataurl", ".png": "dataurl" },
  external: ["react", "react-dom", "react-dom/client", "react/jsx-runtime", "motion/react"],
}).then((result) => {
  const module = { exports: {} };
  Function("require", "module", "exports", result.outputFiles[0].text)(require, module, module.exports);
  return module.exports;
});

const provider = {
  id: "provider.test", label: "Test provider", descriptorPath: "registry/interface/provider.test", sourceNames: [], supportedReasoningEfforts: ["medium"],
  models: [{ model: "model-a", displayName: "Model A", description: "", defaultReasoningEffort: "medium", supportedReasoningEfforts: ["medium"], showInPicker: true, isDefault: true }],
};
const runRow = (runId, extra = {}) => ({
  run_id: runId, title: `Task ${runId}`, chat_interface_id: `chat.${runId}`, client_surface_role: "task", writable: true, ee_id: "ee.root",
  provider_interface_id: provider.id, provider_bound_interface_id: provider.id, provider_model: "model-a", reasoning_effort: "medium", worker_interface_id: "worker.test",
  turn_open: false, turn_state: "idle", last_movement_seq: 2, updated_at_ms: 1_000, ...extra,
});
const messageRow = (runId, seq, role, text, extra = {}) => ({
  id: `${runId}:${seq}`, run_id: runId, role, text, created_at_ms: seq * 100, movement_seq: seq, artifact_id: `${runId}.artifact.${seq}`,
  direction: role === "user" ? "in" : "out", interface_id: `chat.${runId}`, event_kind: "message", terminal: true, ...extra,
});
const movementArtifact = (row) => ({
  source: "movement", recorded_at_unix_ms: row.created_at_ms, run_id: row.run_id, interface_id: row.interface_id,
  from_interface_id: row.role === "user" ? row.interface_id : provider.id, from_interface_role: row.role === "user" ? "operator" : "provider",
  label: row.role === "user" ? "operator.message" : "provider.message", event_kind: row.event_kind, event_data: row.event_data ?? {}, text: row.text,
  error: row.error === true, terminal: row.terminal !== false, will_retry: false, movement_seq: row.movement_seq, checkpoint_id: `checkpoint.${row.movement_seq}`,
  artifact_id: row.artifact_id, content_hash: "",
});
const control = (runId, action, controlId) => ({ role: "assistant", event_kind: "turn_control",
  event_data: { action, status: "acknowledged", control_id: controlId, _movement: { from: { Interface: provider.id }, to: { Interface: `chat.${runId}` } } } });

const projectionListeners = new Set();
let currentHost = null;
w.octopusTransport = new Proxy({}, { get: (_, name) => name === "onProjectionChanged"
  ? (listener) => { projectionListeners.add(listener); return () => projectionListeners.delete(listener); }
  : currentHost?.transport[name] });

function fakeHost() {
  const host = {
    seq: 2,
    runs: [runRow("run-1")],
    messages: { "run-1": [messageRow("run-1", 1, "user", "Draft the report"), messageRow("run-1", 2, "assistant", "Here is the report.")] },
    workers: [{ id: "worker.test" }], reads: [], turns: [], steers: [], controls: [], waiters: new Set(), holdMessages: null, failNextTurn: null, recoverNextTurn: false,
    run: (runId) => host.runs.find((run) => run.run_id === runId),
    changed(kind) { if (currentHost === host) for (const listener of [...projectionListeners]) listener({ kind }); },
    append(runId, role, text, extra = {}) {
      const seq = ++host.seq;
      host.messages[runId] = [...(host.messages[runId] ?? []), messageRow(runId, seq, role, text, extra)];
      host.run(runId).last_movement_seq = seq;
      setTimeout(() => {
        host.changed("ledger");
        for (const waiter of [...host.waiters]) if (waiter()) host.waiters.delete(waiter);
      }, 0);
      return seq;
    },
    reply(runId, text) {
      host.append(runId, "assistant", text);
      Object.assign(host.run(runId), { turn_open: false, turn_state: "idle" });
    },
  };
  host.transport = {
    projectionRead: async (request) => {
      host.reads.push(request.method);
      switch (request.method) {
        case "projection.operator.chats.list": return { chats: structuredClone(host.runs) };
        case "projection.operator.chats.status": return { chats: host.runs.filter((run) => request.params.run_ids.includes(run.run_id)).map((run) => ({ ...run })) };
        case "projection.operator.chat.messages":
          if (host.holdMessages) await host.holdMessages;
          return { messages: structuredClone(host.messages[request.params.run_id] ?? []) };
        case "projection.operator.chat.contexts": return { contexts: [{ ee_id: "ee.root", is_os_root: true, display_name: "Root", agent_catalog_digest: "catalog-1", source_folders: host.sourceFolders ?? [],
          workers: host.workers.map((worker) => ({ label: worker.id, provider_interface_ids: [provider.id], interface_policy: { [provider.id]: { models: ["model-a"] } }, ...worker })) }] };
        case "projection.worker_environments.list": return { worker_environments: [] };
        case "projection.tools.list": return { tools: [] };
        case "projection.operator.providers.status": return { providers: [] };
        default: return {};
      }
    },
    providerInterfaces: async () => ({ repoRoot: "/repo", providers: [structuredClone(provider)] }),
    defaults: async () => ({ endpoint: "http://runtime.invalid" }),
    readOperatorChatArtifacts: (request) => {
      const after = Math.max(request.afterSeq, request.cursor?.seq ?? -1);
      const batch = () => {
        const rows = (host.messages[request.runId] ?? []).filter((row) => row.movement_seq > after);
        return rows.length ? { artifacts: rows.map(movementArtifact), cursor: { offset: 0, seq: rows.at(-1).movement_seq } } : null;
      };
      const ready = batch();
      if (ready) return Promise.resolve(ready);
      return new Promise((resolve) => host.waiters.add(() => { const next = batch(); if (next) resolve(next); return Boolean(next); }));
    },
    submitChatTurn: async (request) => {
      if (request.clientContext !== undefined) require('../../registry/interface/world_operator_chat/client/operatorMessages.cjs').operatorClientContext(request.clientContext);
      if (host.holdTurn) await host.holdTurn;
      host.turns.push(structuredClone(request));
      const runId = request.runId || "run-new";
      if (!request.runId) {
        host.runs.push(runRow(runId, { title: request.text, ee_id: request.eeId, worker_interface_id: request.workerInterfaceId }));
        host.messages[runId] = [];
      }
      if (host.failNextTurn) {
        const { failure, admitted } = host.failNextTurn;
        host.failNextTurn = null;
        if (admitted) { host.append(runId, "user", request.text, { id: request.messageId }); Object.assign(host.run(runId), { turn_open: true, turn_state: "working" }); }
        throw failure;
      }
      const recovered = host.recoverNextTurn;
      host.recoverNextTurn = false;
      const seq = recovered ? host.messages[runId].find((row) => row.id === request.messageId).movement_seq : host.append(runId, "user", request.text, { id: request.messageId });
      if (!recovered) Object.assign(host.run(runId), { turn_open: true, turn_state: "working" });
      return { runId, chatInterfaceId: `chat.${runId}`, messageVerdict: { admitted: { seq } }, outboundCursor: null, messageRecovered: recovered };
    },
    submitChatSteer: async (request) => {
      host.steers.push(structuredClone(request));
      const seq = host.append(request.runId, "user", request.text, { id: request.messageId, event_kind: "steering" });
      return { runId: request.runId, messageVerdict: { admitted: { seq } } };
    },
    submitOperatorTurnControl: async (request) => { host.controls.push(structuredClone(request)); return { accepted: true }; },
  };
  return host;
}

const container = document.getElementById("root");
const root = createRoot(container);
after(() => { root.unmount(); w.close(); });
const settle = async (rounds = 6) => { for (let round = 0; round < rounds; round += 1) await new Promise((resolve) => setTimeout(resolve, 10)); };

async function mount(host, profile = "lab", home = `/home/${Math.random().toString(16).slice(2)}`) {
  const { useChatWorkspace, ClientPreferencesProvider } = await compiled;
  currentHost = host;
  const view = { current: null };
  function Harness() { view.current = useChatWorkspace(profile, true, true, "fixture", "task", home, host.clientContext); return null; }
  root.render(React.createElement(ClientPreferencesProvider, { profile }, React.createElement(Harness)));
  await settle(12);
  return view;
}

const until = async (predicate, message) => {
  for (let attempt = 0; attempt < 200; attempt += 1) {
    if (predicate()) return;
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
  assert.fail(typeof message === "function" ? message() : message);
};
async function openTask(host, profile) {
  const view = await mount(host, profile);
  view.current.selectRun("run-1");
  await until(() => !view.current.loadingMessages && view.current.renderedMessages.length >= 2 && view.current.canSubmit === false && view.current.submitBlockReason === "Type a message or attach a file.", `task never became ready: ${view.current.submitBlockReason}`);
  return view;
}
async function type(view, text) {
  view.current.setText(text);
  await until(() => view.current.text === text, "the composer never took the text");
}
const working = (view) => view.current.renderedMessages.filter((message) => message.pending && message.role === "assistant");

test("refreshing models re-reads the provider catalog only in the live client", { timeout: 15_000 }, async () => {
  for (const [profile, refreshes] of [["live", 1], ["lab", 0]]) {
    const host = fakeHost();
    let catalogReads = 0;
    const read = host.transport.providerInterfaces;
    host.transport.providerInterfaces = async () => { catalogReads += 1; return read(); };
    const view = await openTask(host, profile);
    const before = catalogReads;
    await view.current.refreshProviderModels();
    await settle();
    assert.equal(catalogReads - before, refreshes, profile);
  }
});

const writerAgent = {
  client_agent_id: "agent-writer", client_agent_name: "Writer", client_agent_icon: "default", client_agent_created_at_ms: 1, client_agent_updated_at_ms: 1,
  client_agent_deleted_at_ms: 0, agent_source_template: true, client_template_digest: "template-1", client_template_provider_interface_id: provider.id,
  client_template_provider_model: "model-a", client_template_reasoning_effort: "medium", client_template_application_ids: [], client_template_worker_environment_kinds: [],
};

test("refreshing the task list preserves the selected model and a ready new-task draft", { timeout: 15_000 }, async () => {
  const host = fakeHost();
  host.workers = [{ id: "worker.test", ...writerAgent }];
  host.runs.push(runRow("home", { client_surface_role: "home", title: "Home", os_root_ee_id: "ee.root" }));
  const view = await mount(host, "live");
  await type(view, "Create a report");
  await until(() => view.current.canSubmit, () => view.current.submitBlockReason);
  const read = host.transport.projectionRead;
  let release;
  host.transport.projectionRead = request => request.method === "projection.operator.chats.list"
    ? new Promise(resolve => { release = () => resolve({ chats: structuredClone(host.runs) }); }) : read(request);
  const refreshing = view.current.refreshRuns();
  await until(() => view.current.loadingRuns, "the refresh did not start");
  assert.equal(view.current.runsLoaded, true);
  assert.equal(view.current.canSubmit, true, view.current.submitBlockReason);
  assert.equal(view.current.providerModel, "model-a");
  release(); await refreshing;
});

test("model selection stays loading until the task capabilities arrive", { timeout: 15_000 }, async () => {
  const host = fakeHost();
  host.workers = [{ id: "worker.test", ...writerAgent }];
  host.runs.push(runRow("home", { client_surface_role: "home", os_root_ee_id: "ee.root" }));
  const read = host.transport.projectionRead;
  let release;
  const held = new Promise(resolve => { release = resolve; });
  host.transport.projectionRead = async request => {
    if (request.method === "projection.operator.chat.contexts") await held;
    return read(request);
  };
  const view = await mount(host, "live");
  await until(() => view.current.providerInterfacesLoaded && !view.current.providerInterfacesRefreshing, "catalog did not load");
  assert.equal(view.current.activeProvider, undefined);
  assert.equal(view.current.modelSelectionLoading, true);
  release();
  await until(() => view.current.activeProvider && !view.current.modelSelectionLoading, "model selection never became ready");
  assert.equal(view.current.providerModel, "model-a");
});

test("editing the latest message keeps the task and provider without selecting an Agent", { timeout: 15_000 }, async () => {
  const host = fakeHost();
  host.messages["run-1"][0].content_hash = "a".repeat(64);
  const view = await openTask(host, "live");
  const message = view.current.renderedMessages.find(message => message.role === "user");
  view.current.setText("Unsent composer text");
  await until(() => view.current.text === "Unsent composer text", "composer draft missing");
  const pending = view.current.editUserMessage(message, "Corrected");
  await until(() => host.turns.length === 1, "edited message never submitted");
  assert.equal(host.turns[0].runId, "run-1");
  assert.deepEqual(host.turns[0].replacesMessage, { id: message.artifactId, content: "a".repeat(64) });
  assert.equal(host.turns[0].providerModel, "model-a");
  assert.equal(view.current.activeRunId, "run-1");
  assert.equal(view.current.text, "Unsent composer text");
  host.append("run-1", "assistant", "Revised answer");
  Object.assign(host.run("run-1"), { turn_open: false, turn_state: "completed" });
  host.changed("ledger");
  await pending;
});

test("editing the latest message of an Agent task sends through the task's Agent template", { timeout: 15_000 }, async () => {
  const host = fakeHost();
  Object.assign(host.run("run-1"), { worker_interface_id: "worker.concrete", worker_choice_interface_id: "worker.test" });
  host.messages["run-1"][0].content_hash = "b".repeat(64);
  const view = await openTask(host, "live");
  const message = view.current.renderedMessages.find(message => message.role === "user");
  const pending = view.current.editUserMessage(message, "Corrected");
  await until(() => host.turns.length === 1, "the edit of an Agent task was refused before submission");
  assert.equal(host.turns[0].runId, "run-1");
  assert.equal(host.turns[0].workerInterfaceId, "worker.test");
  assert.deepEqual(host.turns[0].replacesMessage, { id: message.artifactId, content: "b".repeat(64) });
  host.append("run-1", "assistant", "Revised answer");
  Object.assign(host.run("run-1"), { turn_open: false, turn_state: "completed" });
  host.changed("ledger");
  await pending;
});

test("a new task adopts the one Agent matching the Home defaults, starts as submitting and moves to the run the host creates", { timeout: 15_000 }, async () => {
  const host = fakeHost();
  host.sourceFolders = ["/workspace/primary", "/workspace/secondary"];
  host.clientContext = [{ name: "client_observation", text: "{}" }];
  host.workers = [{ id: "worker.test", ...writerAgent }];
  host.runs.push(runRow("home", { client_surface_role: "home", title: "Home", os_root_ee_id: "ee.root" }));
  let admit;
  host.holdTurn = new Promise((resolve) => { admit = resolve; });
  let releaseList;
  const listReady = new Promise((resolve) => { releaseList = resolve; });
  const read = host.transport.projectionRead;
  host.transport.projectionRead = async (request) => {
    if (request.method === "projection.operator.chats.list" && host.turns.length) await listReady;
    return read(request);
  };
  const view = await mount(host, "lab");
  await type(view, "Draft the plan");
  await until(() => view.current.canSubmit, () => `never ready: ${view.current.submitBlockReason}`);
  assert.deepEqual([view.current.agentPresetId, view.current.eeId, view.current.workerInterfaceId, view.current.providerModel], ["agent-writer", "ee.root", "worker.test", "model-a"]);
  void view.current.submit();
  await until(() => working(view).length === 1, "no pending reply");
  assert.equal(working(view)[0].eventData.work_status, "submitting");
  assert.equal(view.current.activeRunId, "");
  admit();
  await until(() => host.turns.length === 1 && view.current.loadingRuns, "new task projection did not start");
  assert.equal(view.current.activeRunId, "");
  assert.equal(view.current.activeProvider.id, provider.id);
  assert.equal(view.current.providerModel, "model-a");
  releaseList();
  await until(() => host.turns.length === 1 && view.current.activeRunId === "run-new", () => `the new run was not adopted: ${view.current.activeRunId}`);
  assert.ok(view.current.activeProvider);
  assert.deepEqual([host.turns[0].runId, host.turns[0].eeId, host.turns[0].workerInterfaceId, host.turns[0].providerModel, host.turns[0].text],
    [undefined, "ee.root", "worker.test", "model-a", "Draft the plan"]);
  assert.deepEqual(host.turns[0].clientContext.map(entry => entry.name), ["client_observation", "project_source_folders"]);
  assert.deepEqual(JSON.parse(host.turns[0].clientContext[1].text), { primary: host.sourceFolders[0], folders: host.sourceFolders });
  host.reply("run-new", "Plan drafted.");
  await until(() => !view.current.busy && view.current.runs.some((run) => run.runId === "run-new"), "the new task never settled");
});

test("without an Agent on the Home worker a new task waits for the operator to choose one", { timeout: 15_000 }, async () => {
  const host = fakeHost();
  host.workers = [{ id: "worker.test" }, { id: "worker.writer", ...writerAgent }];
  host.runs.push(runRow("home", { client_surface_role: "home", title: "Home", os_root_ee_id: "ee.root" }));
  const view = await mount(host, "lab");
  await type(view, "Draft the plan");
  await until(() => view.current.submitBlockReason === "Choose an Agent before sending.", () => `unexpected block: ${view.current.submitBlockReason}`);
  assert.equal(view.current.canSubmit, false);
  view.current.setAgentPresetId("agent-writer");
  await until(() => view.current.canSubmit, () => `never ready: ${view.current.submitBlockReason}`);
  assert.deepEqual([view.current.agentPresetId, view.current.eeId, view.current.workerInterfaceId], ["agent-writer", "ee.root", "worker.writer"]);
});

test("a live client refreshes task status from ledger changes and ignores stale status", { timeout: 15_000 }, async () => {
  const host = fakeHost();
  const view = await openTask(host, "live");
  const reads = (method) => host.reads.filter((read) => read === method).length;
  const before = { status: reads("projection.operator.chats.status"), list: reads("projection.operator.chats.list") };
  Object.assign(host.run("run-1"), { turn_state: "waiting", turn_open: true, last_movement_seq: 3 });
  host.changed("ledger");
  await until(() => view.current.runs.find((run) => run.runId === "run-1")?.turnState === "waiting", "the ledger change did not refresh status");
  assert.ok(reads("projection.operator.chats.status") > before.status);
  assert.equal(reads("projection.operator.chats.list"), before.list, "a ledger change refreshes status without reloading the task list");
  Object.assign(host.run("run-1"), { turn_state: "idle", turn_open: false, last_movement_seq: 1 });
  const stale = reads("projection.operator.chats.status");
  host.changed("ledger");
  await until(() => reads("projection.operator.chats.status") > stale, "no status read");
  await new Promise((resolve) => setTimeout(resolve, 30));
  assert.equal(view.current.runs.find((run) => run.runId === "run-1").turnState, "waiting", "an older status never overrides a newer projection");
});

for (const profile of ["live", "lab"]) {
  test(`a follow-up is sent once under the task's binding, shows as working until the provider answers, and clears the draft (${profile})`, { timeout: 15_000 }, async () => {
    const host = fakeHost();
    const view = await openTask(host, profile);
    await type(view, "Add totals");
    void view.current.submit();
    await until(() => host.turns.length === 1 && view.current.busy && view.current.turnOpen, "the turn was not submitted");
    const [turn] = host.turns;
    assert.deepEqual([turn.runId, turn.providerInterfaceId, turn.providerModel, turn.reasoningEffort, turn.clientSurfaceRole, turn.text, turn.recoverPreparedMessage],
      ["run-1", provider.id, "model-a", "medium", "task", "Add totals", false]);
    assert.match(turn.messageId, /^msg_/);
    assert.equal(view.current.turnOpen, true);
    assert.deepEqual(working(view).map((message) => message.eventData.work_status), ["working"]);
    assert.equal(view.current.text, "");
    host.reply("run-1", "Totals added.");
    await until(() => !view.current.busy, "the reply never closed the turn");
    assert.equal(view.current.turnOpen, false);
    assert.deepEqual(working(view), []);
    assert.ok(view.current.renderedMessages.some((message) => message.text === "Totals added."));
    assert.equal(host.turns.length, 1);
  });

  test(`a message written while a reply is open is queued under its own identity and sent after the reply (${profile})`, { timeout: 15_000 }, async () => {
    const host = fakeHost();
    const view = await openTask(host, profile);
    await type(view, "Add totals");
    void view.current.submit();
    await until(() => view.current.busy && host.turns.length === 1, "first turn not sent");
    await type(view, "Also chart it");
    await view.current.submit();
    await until(() => view.current.queuedTurns.length === 1, "the follow-up was not queued");
    await until(() => view.current.text === "", "queueing did not clear the composer");
    const [queued] = view.current.queuedTurns;
    assert.match(queued.id, /^queued_/);
    assert.equal(queued.text, "Also chart it");
    assert.equal(host.turns.length, 1);
    assert.equal(view.current.paneTransferBlockReason("run-1"), "Send or remove queued follow-ups before moving this task.");
    const statusReads = host.reads.filter((method) => method === "projection.operator.chats.status").length;
    host.reply("run-1", "Totals added.");
    await until(() => host.turns.length === 2, "the queued follow-up was never dispatched");
    assert.equal(host.turns[1].messageId, queued.id.replace(/^queued_/, "msg_"), "the queued turn is delivered under the identity it was queued with");
    assert.equal(host.turns[1].text, "Also chart it");
    assert.ok(host.reads.filter((method) => method === "projection.operator.chats.status").length > statusReads, "dispatch rechecks the task status first");
    host.reply("run-1", "Chart added.");
    await until(() => !view.current.busy && view.current.queuedTurns.length === 0, "the queue never drained");
    assert.equal(view.current.paneTransferBlockReason("run-1"), "");
  });

  test(`steering sends into the open turn with its own message and binding (${profile})`, { timeout: 15_000 }, async () => {
    const host = fakeHost();
    const view = await openTask(host, profile);
    await type(view, "Add totals");
    void view.current.submit();
    await until(() => host.turns.length === 1, "first turn not sent");
    host.changed("runs");
    await until(() => view.current.canSteer, "steering never became available");
    await type(view, "Use euros");
    view.current.steerCurrentTurn();
    await until(() => host.steers.length === 1, "the steering message was not sent");
    const [steer] = host.steers;
    assert.deepEqual([steer.runId, steer.text, steer.providerInterfaceId, steer.clientSurfaceRole, steer.recoverPreparedMessage], ["run-1", "Use euros", provider.id, "task", false]);
    assert.match(steer.messageId, /^msg_/);
    assert.notEqual(steer.messageId, host.turns[0].messageId);
    await until(() => view.current.text === "", "the steering draft was not consumed");
    assert.equal(host.turns.length, 1);
    host.reply("run-1", "Totals in euros.");
    await until(() => !view.current.busy, "the reply never closed the turn");
  });

  test(`a delivery whose confirmation was lost is reconciled from the task history and never sent twice (${profile})`, { timeout: 15_000 }, async () => {
    const host = fakeHost();
    const view = await openTask(host, profile);
    await type(view, "Add totals");
    host.failNextTurn = { failure: new Error("transport closed"), admitted: true };
    await view.current.submit();
    await until(() => view.current.queuedTurns.length === 0 && view.current.busy, () => `history never confirmed the admitted message: ${JSON.stringify({ queued: view.current.queuedTurns, busy: view.current.busy, error: view.current.error })}`);
    assert.equal(view.current.error, "");
    host.reply("run-1", "Totals added.");
    await until(() => !view.current.busy, "the reply never closed the turn");
    await type(view, "Next");
    assert.equal(view.current.submitBlockReason, "");
    assert.equal(host.turns.length, 1);
  });

  test(`an unconfirmed manual delivery returns to the composer and retries the same identity (${profile})`, { timeout: 15_000 }, async () => {
    const host = fakeHost();
    const view = await openTask(host, profile);
    await type(view, "Add totals");
    host.failNextTurn = { failure: new Error("transport closed"), admitted: false };
    await view.current.submit();
    await until(() => view.current.text === "Add totals" && view.current.error === "transport closed", "the draft was not restored");
    assert.equal(view.current.queuedTurns.length, 0);
    assert.equal(view.current.canSubmit, true);
    const original = host.turns[0].messageId;
    host.changed("ledger");
    await settle();
    assert.equal(host.turns.length, 1, "uncertain delivery must not retry automatically");
    void view.current.submit();
    await until(() => host.turns.length === 2, "explicit retry did not submit");
    assert.equal(host.turns[1].messageId, original);
    host.reply("run-1", "Totals added.");
    await until(() => !view.current.busy, "retry did not settle");
  });

  test(`a recovered delivery refreshes the task and is never sent again (${profile})`, { timeout: 15_000 }, async () => {
    const host = fakeHost();
    const view = await openTask(host, profile);
    host.append("run-1", "user", "Add totals", { id: "msg_recovered" });
    host.recoverNextTurn = true;
    await type(view, "Add totals");
    const reads = host.reads.length;
    host.failNextTurn = null;
    host.transport.submitChatTurn = ((submit) => async (request) => submit({ ...request, messageId: "msg_recovered" }))(host.transport.submitChatTurn);
    await view.current.submit();
    await until(() => host.turns.length === 1 && !view.current.busy, "the recovered turn never settled");
    assert.ok(host.reads.slice(reads).includes("projection.operator.chat.messages"));
    assert.ok(host.reads.slice(reads).includes("projection.operator.chats.list"));
    assert.deepEqual(working(view), []);
    await new Promise((resolve) => setTimeout(resolve, 50));
    assert.equal(host.turns.length, 1);
  });

  test(`a draft blocks moving the task until it is sent or cleared (${profile})`, { timeout: 15_000 }, async () => {
    const host = fakeHost();
    const view = await openTask(host, profile);
    assert.equal(view.current.paneTransferBlockReason("run-1"), "");
    await type(view, "Unsent thought");
    assert.equal(view.current.paneTransferBlockReason("run-1"), "Send or clear the current draft before moving this task.");
    await type(view, "");
    assert.equal(view.current.paneTransferBlockReason("run-1"), "");
  });

  test(`sending waits until the task history has loaded (${profile})`, { timeout: 15_000 }, async () => {
    const host = fakeHost();
    let release;
    host.holdMessages = new Promise((resolve) => { release = resolve; });
    const view = await mount(host, profile);
    view.current.selectRun("run-1");
    await type(view, "Add totals");
    await until(() => view.current.submitBlockReason === "Task history is still loading.", `unexpected block: ${view.current.submitBlockReason}`);
    assert.equal(view.current.canSubmit, false);
    host.holdMessages = null;
    release();
    await until(() => view.current.canSubmit, `never ready: ${view.current.submitBlockReason}`);
  });

  test(`stop and resume stay pending until the provider acknowledges them (${profile})`, { timeout: 15_000 }, async () => {
    const host = fakeHost();
    const view = await openTask(host, profile);
    await type(view, "Add totals");
    void view.current.submit();
    await until(() => view.current.turnOpen, "turn never opened");
    await view.current.interruptActiveTurn();
    assert.deepEqual(host.controls.map((request) => [request.runId, request.action, request.interfaceId]), [["run-1", "stop", "chat.run-1"]]);
    assert.equal(view.current.turnControlPending, false, "Stop remains available after hand-in while the provider acknowledgement is pending");
    host.append("run-1", "assistant", "", control("run-1", "stop", host.controls[0].controlId));
    Object.assign(host.run("run-1"), { turn_open: false, turn_state: "idle" });
    host.changed("ledger");
    await until(() => !view.current.turnControlPending && !view.current.busy && view.current.resumeAvailable, "the acknowledged stop never settled into a resumable turn");
    await view.current.resumeActiveTurn();
    assert.deepEqual(host.controls.map((request) => request.action), ["stop", "resume"]);
    await until(() => view.current.turnControlPending, "the resume is not pending");
    host.append("run-1", "assistant", "", control("run-1", "resume", host.controls[1].controlId));
    Object.assign(host.run("run-1"), { turn_open: true, turn_state: "working" });
    host.changed("ledger");
    await until(() => !view.current.turnControlPending && view.current.turnOpen && !view.current.resumeAvailable, () => `the acknowledged resume never reopened the turn: ${JSON.stringify({ pending: view.current.turnControlPending, open: view.current.turnOpen, resume: view.current.resumeAvailable, run: view.current.runs[0]?.turnOpen, error: view.current.error })}`);
    host.reply("run-1", "Totals added.");
    await until(() => !view.current.turnOpen, "the reply never closed the turn");
  });

  test(`editing a queued message brings it back into the composer and keeps its place in the queue (${profile})`, { timeout: 15_000 }, async () => {
    const host = fakeHost();
    const view = await openTask(host, profile);
    await type(view, "Add totals");
    void view.current.submit();
    await until(() => view.current.busy, "first turn not sent");
    await type(view, "Also chart it");
    await view.current.submit();
    await until(() => view.current.queuedTurns.length === 1, "not queued");
    await until(() => view.current.text === "", "queueing did not clear the composer");
    const [queued] = view.current.queuedTurns;
    await view.current.editQueuedTurn(queued.id);
    await until(() => view.current.editingQueuedTurn && view.current.text === "Also chart it", "the queued text did not return to the composer");
    await type(view, "Also chart it in euros");
    await view.current.submit();
    await until(() => !view.current.editingQueuedTurn && view.current.queuedTurns[0]?.text === "Also chart it in euros", "the edit was not saved");
    assert.deepEqual([view.current.queuedTurns.length, view.current.queuedTurns[0].id], [1, queued.id]);
    host.reply("run-1", "Totals added.");
    await until(() => host.turns.length === 2, "the edited follow-up was never dispatched");
    assert.equal(host.turns[1].text, "Also chart it in euros");
    host.reply("run-1", "Chart added.");
    await until(() => !view.current.busy, "the queue never drained");
  });

  test(`stopping a turn pauses its queued follow-ups until the operator resumes them (${profile})`, { timeout: 15_000 }, async () => {
    const host = fakeHost();
    const view = await openTask(host, profile);
    await type(view, "Add totals");
    void view.current.submit();
    await until(() => view.current.turnOpen, "turn never opened");
    await type(view, "Also chart it");
    await view.current.submit();
    await until(() => view.current.queuedTurns.length === 1 && view.current.text === "", "not queued");
    await view.current.interruptActiveTurn();
    host.append("run-1", "assistant", "", control("run-1", "stop", host.controls[0].controlId));
    host.append("run-1", "assistant", "Interrupted before totals", { event_kind: "interrupted_result", event_data: { turn_interrupted: true } });
    Object.assign(host.run("run-1"), { turn_open: false, turn_state: "idle" });
    await until(() => !view.current.busy && view.current.queuePaused, () => `the queue did not pause: ${JSON.stringify({ busy: view.current.busy, paused: view.current.queuePaused, queued: view.current.queuedTurns })}`);
    await new Promise((resolve) => setTimeout(resolve, 40));
    assert.equal(host.turns.length, 1, "a stopped turn never releases its queued follow-up by itself");
    await view.current.resumeQueuedTurns();
    await until(() => host.turns.length === 2, "resuming the queue never sent the follow-up");
    assert.equal(host.turns[1].text, "Also chart it");
    host.reply("run-1", "Chart added.");
    await until(() => !view.current.busy && view.current.queuedTurns.length === 0, "the queue never drained");
  });

  test(`a stop the provider refuses is reported and released (${profile})`, { timeout: 15_000 }, async () => {
    const host = fakeHost();
    const view = await openTask(host, profile);
    await type(view, "Add totals");
    void view.current.submit();
    await until(() => view.current.turnOpen, "turn never opened");
    await view.current.interruptActiveTurn();
    assert.equal(view.current.turnControlPending, false, "Stop remains available after hand-in while the provider acknowledgement is pending");
    const refusal = control("run-1", "stop", host.controls[0].controlId);
    host.append("run-1", "assistant", "Provider busy", { ...refusal, event_data: { ...refusal.event_data, status: "failed", detail: { error: "provider busy" } } });
    await until(() => !view.current.turnControlPending && view.current.error === "Stop failed. provider busy", "the refused stop was not reported");
    assert.equal(view.current.error, "Stop failed. provider busy");
    host.reply("run-1", "Totals added.");
    await until(() => !view.current.busy, "the reply never closed the turn");
  });

  test(`resume is never offered while the task's turn is open (${profile})`, { timeout: 15_000 }, async () => {
    const host = fakeHost();
    host.append("run-1", "assistant", "", control("run-1", "interrupt", "control_earlier"));
    const view = await openTask(host, profile);
    assert.equal(view.current.resumeAvailable, true);
    Object.assign(host.run("run-1"), { turn_open: true, turn_state: "working" });
    host.changed("runs");
    await until(() => view.current.turnOpen, "the open turn was not projected");
    assert.equal(view.current.resumeAvailable, false);
  });
}

const stagedFile = (name) => ({ id: `stage-${name}`, name, size: 12, type: "text/markdown", label: "operator.file", lastModified: 1, contentHash: "c".repeat(64) });
const names = (attachments) => attachments.map((attachment) => attachment.name);
async function queueFollowUp(view, host, text, files = []) {
  await type(view, "Add totals");
  void view.current.submit();
  await until(() => view.current.busy && host.turns.length === 1, "first turn not sent");
  await type(view, text);
  if (files.length) {
    view.current.onFilesPicked(files);
    await until(() => view.current.attachments.length === files.length, "the files were not attached");
  }
  await view.current.submit();
  await until(() => view.current.queuedTurns.length === 1 && view.current.text === "" && view.current.attachments.length === 0, "the follow-up was not queued");
  return view.current.queuedTurns[0];
}

test("a follow-up queued with files keeps them through an unchanged edit and is delivered with them under its own identity", { timeout: 15_000 }, async () => {
  const host = fakeHost();
  const view = await openTask(host, "lab");
  const queued = await queueFollowUp(view, host, "Also chart it", [stagedFile("notes.md")]);
  assert.deepEqual(names(queued.attachments), ["notes.md"]);
  await view.current.editQueuedTurn(queued.id);
  await until(() => view.current.editingQueuedTurn && view.current.text === "Also chart it", "the queued message did not return to the composer");
  assert.deepEqual(names(view.current.attachments), ["notes.md"]);
  await view.current.submit();
  await until(() => !view.current.editingQueuedTurn && view.current.queuedTurns[0]?.status === "queued", "the edit was not saved");
  host.reply("run-1", "Totals added.");
  await until(() => host.turns.length === 2, "the follow-up was never dispatched");
  assert.deepEqual([host.turns[1].messageId, names(host.turns[1].attachments), host.turns[1].recoverPreparedMessage],
    [queued.id.replace(/^queued_/, "msg_"), ["notes.md"], false]);
  host.reply("run-1", "Chart added.");
  await until(() => !view.current.busy && view.current.queuedTurns.length === 0, "the queue never drained");
});

test("a queued message saved with a changed payload is delivered as a new message", { timeout: 15_000 }, async () => {
  const host = fakeHost();
  const view = await openTask(host, "lab");
  const queued = await queueFollowUp(view, host, "Also chart it");
  await view.current.editQueuedTurn(queued.id);
  await until(() => view.current.editingQueuedTurn && view.current.text === "Also chart it", "the queued message did not return to the composer");
  await type(view, "Also chart it in euros");
  await view.current.submit();
  await until(() => !view.current.editingQueuedTurn && view.current.queuedTurns[0]?.text === "Also chart it in euros", "the edit was not saved");
  host.reply("run-1", "Totals added.");
  await until(() => host.turns.length === 2, "the edited follow-up was never dispatched");
  assert.match(host.turns[1].messageId, /^msg_/);
  assert.notEqual(host.turns[1].messageId, queued.id.replace(/^queued_/, "msg_"));
  host.reply("run-1", "Chart added.");
  await until(() => !view.current.busy, "the queue never drained");
});

test("attached files alone keep a task from moving to another pane", { timeout: 15_000 }, async () => {
  const host = fakeHost();
  const view = await openTask(host, "lab");
  view.current.onFilesPicked([stagedFile("notes.md")]);
  await until(() => view.current.attachments.length === 1, "the file was not attached");
  assert.equal(view.current.paneTransferBlockReason("run-1"), "Send or remove attached files before moving this task.");
  view.current.removeAttachment(view.current.attachments[0].id);
  await until(() => view.current.attachments.length === 0, "the file was not removed");
  assert.equal(view.current.paneTransferBlockReason("run-1"), "");
});

test("sending after a terminal receipt cannot regress to an older working status", { timeout: 15_000 }, async () => {
  const host = fakeHost();
  const view = await openTask(host, "live");
  await type(view, "Add totals");
  void view.current.submit();
  await until(() => host.turns.length === 1 && view.current.turnOpen, "turn never opened");
  const stale = structuredClone(host.run("run-1"));
  host.append("run-1", "assistant", "Provider stopped", { event_kind: "error", error: true });
  Object.assign(host.run("run-1"), { turn_open: false, turn_state: "failed" });
  await until(() => !view.current.busy && !view.current.turnOpen, "terminal receipt never closed the turn");
  const read = host.transport.projectionRead;
  host.transport.projectionRead = (request) => request.method === "projection.operator.chats.status"
    ? Promise.resolve({ chats: [stale] }) : read(request);
  await type(view, "Continue with the remaining work");
  void view.current.submit();
  await until(() => host.turns.length === 2, () => `follow-up rejected: ${view.current.error}`);
  assert.equal(host.turns[1].text, "Continue with the remaining work");
  host.reply("run-1", "Continued.");
  await until(() => !view.current.busy, "follow-up did not finish");
});

test("a live provider failure with a failure code is reported once as an incident of the task's provider", { timeout: 15_000 }, async () => {
  const { clientExtensionIncidentSnapshot } = await compiled;
  const host = fakeHost();
  const read = host.transport.readOperatorChatArtifacts;
  host.transport.readOperatorChatArtifacts = async (request) => {
    const batch = await read(request);
    return batch && { ...batch, artifacts: batch.artifacts.map((artifact) => artifact.event_data.failure_code ? { ...artifact, from_interface_id: "provider.test.bound" } : artifact) };
  };
  const view = await openTask(host, "live");
  await type(view, "Add totals");
  void view.current.submit();
  await until(() => host.turns.length === 1 && view.current.turnOpen, "the turn was not submitted");
  host.append("run-1", "assistant", "Sign in again", { event_kind: "error", error: true, event_data: { failure_code: "provider_auth_expired" } });
  host.append("run-1", "assistant", "Sign in again", { event_kind: "error", error: true, event_data: { failure_code: "provider_auth_expired" } });
  Object.assign(host.run("run-1"), { turn_open: false, turn_state: "failed" });
  await until(() => !view.current.busy, "the failure never closed the turn");
  assert.deepEqual(clientExtensionIncidentSnapshot().filter((incident) => incident.code === "provider_auth_expired").map(({ interfaceId, message }) => [interfaceId, message]),
    [[provider.id, "Sign in again"]]);
});

for (const profile of ["live", "lab"]) {
  test(`an acknowledged resume reopens the turn before the task status catches up and sends no message (${profile})`, { timeout: 15_000 }, async () => {
    const host = fakeHost();
    const view = await openTask(host, profile);
    await type(view, "Add totals");
    void view.current.submit();
    await until(() => view.current.turnOpen, "turn never opened");
    await view.current.interruptActiveTurn();
    host.append("run-1", "assistant", "", control("run-1", "stop", host.controls[0].controlId));
    Object.assign(host.run("run-1"), { turn_open: false, turn_state: "idle" });
    await until(() => !view.current.busy && view.current.resumeAvailable && !view.current.turnControlPending, "the stop never settled");
    await type(view, "Keep this draft");
    await view.current.resumeActiveTurn();
    const [stop, resume] = host.controls;
    assert.deepEqual([resume.runId, resume.interfaceId, resume.action], ["run-1", "chat.run-1", "resume"]);
    assert.match(resume.controlId, /^control_/);
    assert.notEqual(resume.controlId, stop.controlId);
    const seq = host.append("run-1", "assistant", "", control("run-1", "resume", resume.controlId));
    host.run("run-1").last_movement_seq = seq - 1;
    await until(() => view.current.turnOpen && !view.current.turnControlPending && !view.current.resumeAvailable, () => `the acknowledged resume did not reopen the turn: ${JSON.stringify({ open: view.current.turnOpen, pending: view.current.turnControlPending })}`);
    assert.equal(host.run("run-1").turn_open, false);
    assert.deepEqual([host.turns.length, view.current.text], [1, "Keep this draft"]);
  });
}

for (const profile of ["live", "lab"]) {
  test(`a reply that ends the turn releases a stop that was never acknowledged (${profile})`, { timeout: 15_000 }, async () => {
    const host = fakeHost();
    const view = await openTask(host, profile);
    await type(view, "Add totals");
    void view.current.submit();
    await until(() => view.current.turnOpen, "turn never opened");
    await view.current.interruptActiveTurn();
    assert.equal(view.current.turnControlPending, false, "Stop remains available after hand-in while the provider acknowledgement is pending");
    host.reply("run-1", "Totals added.");
    await until(() => !view.current.busy && !view.current.turnOpen && !view.current.turnControlPending, () => `the reply did not release the stop: ${JSON.stringify({ busy: view.current.busy, open: view.current.turnOpen, pending: view.current.turnControlPending })}`);
  });
}

test("stopping the first turn of a new task reaches the run the host created before the task list shows it", { timeout: 15_000 }, async () => {
  const host = fakeHost();
  host.workers = [{ id: "worker.test", ...writerAgent }];
  host.runs.push(runRow("home", { client_surface_role: "home", title: "Home", os_root_ee_id: "ee.root" }));
  const read = host.transport.projectionRead;
  host.transport.projectionRead = async (request) => {
    const response = await read(request);
    return response.chats ? { ...response, chats: response.chats.filter((chat) => chat.run_id !== "run-new") } : response;
  };
  const view = await mount(host, "lab");
  await type(view, "Draft the plan");
  await until(() => view.current.canSubmit, () => `never ready: ${view.current.submitBlockReason}`);
  void view.current.submit();
  await until(() => host.turns.length === 1 && view.current.activeRunId === "run-new" && view.current.turnOpen, () => `the new run's turn never opened: ${view.current.activeRunId}`);
  assert.equal(view.current.runs.some((run) => run.runId === "run-new"), false);
  await view.current.interruptActiveTurn();
  assert.deepEqual(host.controls.map((request) => [request.runId, request.interfaceId, request.action]), [["run-new", "chat.run-new", "stop"]]);
});

test("returning to a task awaits current history, and reselecting the open task keeps its view", { timeout: 15_000 }, async () => {
  const host = fakeHost();
  host.runs.push(runRow("run-2"));
  host.messages["run-2"] = [messageRow("run-2", 1, "user", "Plan the trip"), messageRow("run-2", 2, "assistant", "Trip planned.")];
  const view = await openTask(host, "lab");
  view.current.setMessageQuery("report");
  await until(() => view.current.messageQuery === "report", "the find query was not set");
  view.current.selectRun("run-1");
  await settle(2);
  assert.equal(view.current.messageQuery, "report");
  view.current.selectRun("run-2");
  await until(() => !view.current.loadingMessages && view.current.renderedMessages.some((message) => message.text === "Trip planned."), "the second task never loaded");
  let release;
  host.holdMessages = new Promise(resolve => { release = resolve; });
  host.messages["run-1"].push(messageRow("run-1", 20, "assistant", "Updated report."));
  view.current.selectRun("run-1");
  await until(() => view.current.activeRunId === "run-1", "the first task was not selected again");
  assert.equal(view.current.loadingMessages, true);
  release();
  await until(() => !view.current.loadingMessages, "current history never loaded");
  assert.ok(view.current.renderedMessages.some((message) => message.text === "Updated report."));
});

test("a partial live stream is not treated as a loaded chat snapshot on navigation", { timeout: 15_000 }, async () => {
  const host = fakeHost();
  host.runs.push(runRow("run-partial", { turn_open: true, turn_state: "working" }));
  host.messages["run-partial"] = [messageRow("run-partial", 5, "assistant", "Latest live event", { terminal: false })];
  const view = await openTask(host, "live");
  let release;
  host.holdMessages = new Promise(resolve => { release = resolve; });
  view.current.selectRun("run-partial");
  await until(() => view.current.renderedMessages.some(message => message.text === "Latest live event"), "the live stream did not arrive");
  assert.equal(view.current.loadingMessages, true);
  view.current.selectRun("run-1"); await settle(2);
  view.current.selectRun("run-partial"); await settle(2);
  assert.equal(view.current.loadingMessages, true);
  release();
  await until(() => !view.current.loadingMessages, "snapshot did not finish");
});

test("a projection change re-reads the Agent catalog without dropping the current one and never reads the interface list", { timeout: 15_000 }, async () => {
  const host = fakeHost();
  const view = await openTask(host, "lab");
  await type(view, "Add totals");
  await until(() => view.current.canSubmit, () => `never ready: ${view.current.submitBlockReason}`);
  let release;
  const held = new Promise((resolve) => { release = resolve; });
  let catalogReads = 0;
  const read = host.transport.projectionRead;
  host.transport.projectionRead = async (request) => {
    if (request.method === "projection.operator.chat.contexts") { catalogReads += 1; await held; }
    return read(request);
  };
  host.changed("runs");
  await until(() => catalogReads === 1, "the catalog was not re-read");
  await settle(2);
  assert.deepEqual([view.current.executionContexts.map((context) => context.eeId), view.current.canSubmit], [["ee.root"], true]);
  release();
  await settle();
  assert.equal(host.reads.includes("projection.interfaces.list"), false);
});

test("a live client applies the provider's own receipts to the task before its status is re-read", { timeout: 15_000 }, async () => {
  const host = fakeHost();
  const view = await openTask(host, "live");
  const statusReads = () => host.reads.filter((method) => method === "projection.operator.chats.status").length;
  const before = statusReads();
  host.changed = () => {};
  host.append("run-1", "assistant", "", { event_kind: "turn_started", terminal: false });
  host.run("run-1").last_movement_seq = 2;
  await until(() => view.current.turnOpen && view.current.runs.find((run) => run.runId === "run-1")?.turnState === "working", "the provider's receipt did not open the turn");
  assert.equal(statusReads(), before);
});

const twoModels = ["model-a", "model-b"].map((model) => ({ ...provider.models[0], model, displayName: model, supportedReasoningEfforts: ["medium", "high"], isDefault: model === "model-a" }));
const capableProvider = { ...provider, supportedReasoningEfforts: ["medium", "high"], models: twoModels };
const otherProvider = { ...capableProvider, id: "provider.other", label: "Other provider", models: twoModels.slice(1) };
const policy = (models) => ({ [provider.id]: { models }, [otherProvider.id]: { models: ["model-b"] } });
const notesApp = { id: "app.notes", name: "Notes", category: "connector", interfaces: [], artifact_bindings: [], task_capabilities: { tools: [{ tool: "notes.read", interface: "iface.notes", worker_environment_kind: "system_shell" }] } };
const mailApp = { id: "app.mail", name: "Mail", category: "connector", interfaces: [], artifact_bindings: [], task_capabilities: { tools: [] } };
const drivers = [{ name: "system_shell", door_interfaces: ["door.shell"], root_slots: ["host_filesystem_root"], tools: [], host_slots: {} }, { name: "system_browser", door_interfaces: ["door.browser"], root_slots: [], tools: [], host_slots: {} }];
const researcher = { ...writerAgent, id: "worker.research", label: "Research", provider_interface_ids: [provider.id, otherProvider.id], interface_policy: policy(["model-a", "model-b"]),
  applications: [notesApp, mailApp], drivers, selected_tools: [{ tool: "shell.exec", interface: "iface.shell", worker_environment_kind: "system_shell" }, { tool: "browser.open", interface: "iface.browser", worker_environment_kind: "system_browser" }],
  client_agent_id: "agent-research", client_agent_name: "Researcher", client_template_provider_model: "model-b", client_template_reasoning_effort: "high",
  client_template_application_ids: ["app.notes"], client_template_worker_environment_kinds: ["system_shell"] };
const rootContext = (workers) => ({ ee_id: "ee.root", is_os_root: true, display_name: "Root", agent_catalog_digest: "catalog-1", full_access_root: "/", host_root_refs: { host_filesystem_root: "/" },
  workers: workers.map((worker) => ({ label: worker.id, provider_interface_ids: [provider.id, otherProvider.id], interface_policy: policy(["model-a", "model-b"]), ...worker })) });
const projectContext = { ee_id: "ee.proj", display_name: "Market study", agent_catalog_digest: "catalog-2", full_access_root: "/", host_root_refs: { host_filesystem_root: "/" }, workers: [researcher] };
function capableHost(contexts = [rootContext([{ id: "worker.test" }]), projectContext], { home = true } = {}) {
  localStorage.clear();
  const host = fakeHost();
  if (home) host.runs.push(runRow("home", { client_surface_role: "home", title: "Home", os_root_ee_id: "ee.root" }));
  host.mutations = [];
  host.transport.providerInterfaces = async () => ({ repoRoot: "/repo", providers: structuredClone([capableProvider, otherProvider]) });
  host.transport.mutateOctopusOsWorkspace = async (request) => { host.mutations.push(structuredClone(request)); return { ok: true }; };
  const read = host.transport.projectionRead;
  host.transport.projectionRead = async (request) => request.method === "projection.operator.chat.contexts"
    ? (host.reads.push(request.method), { contexts: structuredClone(contexts) }) : read(request);
  return host;
}
const choose = async (view, agentId) => {
  view.current.setAgentPresetId(agentId);
  await until(() => view.current.canSubmit, () => `the Agent never became ready: ${view.current.submitBlockReason}`);
};

test("a new task without a matching Agent sends nothing, then takes the chosen Agent's project, model and capabilities exactly", { timeout: 15_000 }, async () => {
  const host = capableHost();
  const view = await mount(host, "lab");
  await type(view, "Study the market");
  await until(() => view.current.submitBlockReason === "Choose an Agent before sending.", () => `unexpected block: ${view.current.submitBlockReason}`);
  assert.equal(view.current.workerInterfaceId, "", "an unmatched Home worker is not kept as the task's worker");
  await view.current.submit();
  await settle();
  assert.deepEqual([host.turns.length, view.current.error, view.current.text], [0, "", "Study the market"]);
  await choose(view, "agent-research");
  assert.deepEqual([view.current.eeId, view.current.workerInterfaceId, view.current.providerInterfaceId, view.current.providerModel, view.current.reasoningEffort, view.current.applicationIds, view.current.driverNames],
    ["ee.proj", "worker.research", provider.id, "model-b", "high", ["app.notes"], ["system_shell"]]);
  void view.current.submit();
  await until(() => host.turns.length === 1, "the task was not created");
  const [turn] = host.turns;
  assert.deepEqual([turn.runId, turn.eeId, turn.workerInterfaceId, turn.providerModel, turn.reasoningEffort, turn.selectedWorkerEnvironmentKinds, turn.selectedTools.map((tool) => tool.tool)],
    [undefined, "ee.proj", "worker.research", "model-b", "high", ["system_shell"], ["notes.read", "shell.exec"]]);
  assert.deepEqual(Object.keys(turn).filter((key) => /agent/i.test(key)), [], "the runtime receives the Agent's configuration, never a client Agent identity");
  assert.equal(turn.doorHostSlots["door.shell"]?.host_filesystem_root, "/", "a new task starts with full access");
  host.reply("run-new", "Studied.");
  await until(() => !view.current.busy, "the new task never settled");
});

test("an Agent's task choices stay inside its template, a missing Worker Environment blocks sending, and default full access carries the host root", { timeout: 15_000 }, async () => {
  const host = capableHost();
  const view = await mount(host, "lab");
  await type(view, "Study the market");
  await choose(view, "agent-research");
  assert.deepEqual([view.current.accessMode, view.current.fullAccessUnavailableReason], ["full_access", ""]);
  view.current.toggleApplication("app.mail");
  view.current.toggleDriver("system_browser");
  await settle();
  assert.deepEqual([view.current.applicationIds, view.current.driverNames], [["app.notes"], ["system_shell"]], "capabilities outside the Agent's template are not offered");
  view.current.setProviderModel(provider.id, "model-a", "medium");
  await until(() => view.current.providerModel === "model-a", "the model choice was not taken");
  assert.deepEqual([view.current.agentPresetId, view.current.canSubmit], ["agent-research", true], "another admitted model keeps the chosen Agent");
  view.current.toggleDriver("system_shell");
  await until(() => view.current.submitBlockReason === "Application 'Notes' requires Worker Environment 'system_shell'. Select that Worker Environment or remove the Application.",
    () => `unexpected block: ${view.current.submitBlockReason}`);
  assert.equal(view.current.agentPresetId, "agent-research");
  view.current.toggleApplication("app.notes");
  await until(() => view.current.canSubmit, () => `still blocked: ${view.current.submitBlockReason}`);
  assert.equal(view.current.fullAccessUnavailableReason, "", "full access without a host-root Worker Environment adds nothing and blocks nothing");
  view.current.toggleDriver("system_shell");
  await until(() => view.current.driverNames.includes("system_shell"), "the host-root Worker Environment was not selected again");
  assert.equal(view.current.accessMode, "full_access");
  void view.current.submit();
  await until(() => host.turns.length === 1, "the task was not created");
  assert.deepEqual([host.turns[0].providerModel, host.turns[0].reasoningEffort, host.turns[0].doorHostSlots], ["model-a", "medium", { "door.shell": { host_filesystem_root: "/" } }]);
  host.reply("run-new", "Studied.");
  await until(() => !view.current.busy, "the new task never settled");
});

test("an existing task keeps its bound provider, takes an exact model and effort, and never replaces an unknown model", { timeout: 15_000 }, async () => {
  const host = capableHost();
  host.runs.push(runRow("run-2", { provider_model: "retired-model" }));
  const view = await openTask(host, "lab");
  view.current.setProviderModel(otherProvider.id, "model-b");
  await settle();
  assert.deepEqual([view.current.providerInterfaceId, view.current.providerModel], [provider.id, "model-a"], "another provider's model is not taken into a bound task");
  view.current.setProviderModel(provider.id, "model-b", "high");
  await until(() => view.current.providerModel === "model-b", "the model choice was not taken");
  view.current.setProviderModel(provider.id, "model-a", "extreme");
  await settle();
  assert.deepEqual([view.current.providerModel, view.current.reasoningEffort], ["model-b", "high"], "an unsupported effort rejects the whole choice");
  await type(view, "Add totals");
  void view.current.submit();
  await until(() => host.turns.length === 1, "the turn was not sent");
  assert.deepEqual([host.turns[0].providerInterfaceId, host.turns[0].providerModel, host.turns[0].reasoningEffort], [provider.id, "model-b", "high"]);
  host.reply("run-1", "Totals added.");
  await until(() => !view.current.busy, "the reply never closed the turn");
  view.current.selectRun("run-2");
  await until(() => !view.current.loadingMessages && view.current.activeRunId === "run-2", "run-2 never opened");
  await type(view, "Continue");
  await until(() => view.current.submitBlockReason === "provider.test does not admit model 'retired-model' for this task.", () => `unexpected block: ${view.current.submitBlockReason}`);
  assert.equal(view.current.providerModel, "retired-model");
});

test("an open task is attributed to the one Agent of its worker choice, whatever model it runs", { timeout: 15_000 }, async () => {
  const host = capableHost();
  host.runs.push(runRow("run-research", { ee_id: "ee.proj", worker_interface_id: "worker.concrete", worker_choice_interface_id: "worker.research", provider_model: "model-a" }));
  const view = await openTask(host, "lab");
  assert.equal(view.current.agentPresetId, "");
  view.current.selectRun("run-research");
  await until(() => view.current.agentPresetId === "agent-research", () => `the task's Agent was not recognised: ${view.current.agentPresetId}`);
  assert.equal(view.current.providerModel, "model-a");
});

test("only Home's provider is preferred for new Agents, and the catalog reports itself loaded without a Home", { timeout: 15_000 }, async () => {
  const view = await mount(capableHost(), "lab");
  await until(() => view.current.declaredProviderInterfaces.length === 2, "the catalog never loaded");
  assert.deepEqual(view.current.declaredProviderInterfaces.filter((candidate) => candidate.preferred).map((candidate) => candidate.id), [provider.id]);
  const homeless = await mount(capableHost(undefined, { home: false }), "lab");
  await until(() => homeless.current.providerInterfacesLoaded && homeless.current.newTaskAuthoringError !== "", "the catalog never loaded");
  assert.deepEqual(homeless.current.declaredProviderInterfaces.filter((candidate) => candidate.preferred), []);
});

test("a Home default adopts no Agent that could not start the task", { timeout: 15_000 }, async () => {
  const view = await mount(capableHost([rootContext([{ id: "worker.test", ...writerAgent, interface_policy: policy(["model-b"]) }])]), "lab");
  await type(view, "Draft the plan");
  await until(() => view.current.submitBlockReason === "Choose an Agent before sending.", () => `unexpected block: ${view.current.submitBlockReason}`);
  assert.deepEqual([view.current.agentPresetId, view.current.workerInterfaceId], ["", ""]);
});

test("a Home default adopts its Agent and then takes that Agent's applications and Worker Environments", { timeout: 15_000 }, async () => {
  const worker = { id: "worker.test", ...writerAgent, applications: [notesApp, mailApp], drivers,
    client_template_application_ids: ["app.notes"], client_template_worker_environment_kinds: ["system_shell"] };
  const host = capableHost([rootContext([worker])]);
  const view = await mount(host, "lab");
  await type(view, "Draft the plan");
  await until(() => view.current.canSubmit, () => `the Home Agent was not adopted: ${view.current.submitBlockReason}`);
  assert.deepEqual([view.current.agentPresetId, view.current.workerInterfaceId, view.current.applicationIds, view.current.driverNames],
    ["agent-writer", "worker.test", ["app.notes"], ["system_shell"]]);
  void view.current.submit();
  await until(() => host.turns.length === 1, "the task was not created");
  assert.deepEqual([host.turns[0].workerInterfaceId, host.turns[0].selectedWorkerEnvironmentKinds], ["worker.test", ["system_shell"]]);
  host.reply("run-new", "Drafted.");
  await until(() => !view.current.busy, "the new task never settled");
});

test("a live task connects only applications its Agent's template provides", { timeout: 15_000 }, async () => {
  const host = capableHost();
  host.runs = [runRow("run-1", { ee_id: "ee.proj", worker_interface_id: "worker.research", record_digest: "digest-1", connected_application_ids: [] }), ...host.runs.slice(1)];
  const view = await openTask(host, "live");
  view.current.toggleApplication("app.mail");
  await until(() => view.current.error === "Application 'app.mail' is outside this Task's immutable Agent template.", () => `unexpected error: ${view.current.error}`);
  assert.deepEqual(host.mutations, []);
  view.current.toggleApplication("app.notes");
  await until(() => host.mutations.length === 1, "the template application was not connected");
  assert.deepEqual(host.mutations[0], { action: "configure_task_application", runId: "run-1", applicationId: "app.notes", connected: true, expectedDigest: "digest-1" });
});

test("text written in a starting task's composer follows that task once the host creates it", { timeout: 15_000 }, async () => {
  const host = fakeHost();
  host.workers = [{ id: "worker.test", ...writerAgent }];
  host.runs.push(runRow("home", { client_surface_role: "home", title: "Home", os_root_ee_id: "ee.root" }));
  let admit;
  host.holdTurn = new Promise((resolve) => { admit = resolve; });
  const view = await mount(host, "lab");
  await type(view, "Draft the plan");
  await until(() => view.current.canSubmit, () => `never ready: ${view.current.submitBlockReason}`);
  void view.current.submit();
  await until(() => working(view).length === 1, "no pending reply");
  await type(view, "Then the budget");
  view.current.selectRun("run-1");
  await until(() => view.current.activeRunId === "run-1" && view.current.text === "", "the other task did not open");
  admit();
  await until(() => view.current.runs.some((run) => run.runId === "run-new"), "the new task was never listed");
  view.current.selectRun("run-new");
  await until(() => view.current.activeRunId === "run-new", "the new task did not open");
  assert.equal(view.current.text, "Then the budget");
});

test("a structural change lists new tasks even when ledger changes arrive with it", { timeout: 15_000 }, async () => {
  const host = fakeHost();
  const view = await mount(host, "live");
  await until(() => view.current.runs.length === 1, "the first task never listed");
  host.runs.push(runRow("child-1"), runRow("child-2"));
  host.changed("structure");
  host.changed("ledger");
  await until(() => view.current.runs.length === 3, () => `structural discovery was lost: ${view.current.runs.map((run) => run.runId)}`);
  const lists = host.reads.filter((method) => method === "projection.operator.chats.list").length;
  host.changed("ledger");
  await until(() => host.reads.filter((method) => method === "projection.operator.chats.status").length > 0, "no status read");
  await settle();
  assert.equal(host.reads.filter((method) => method === "projection.operator.chats.list").length, lists, "a ledger change does not re-list tasks");
});


test("a manual message refused before delivery returns to the composer and the existing error surface", { timeout: 15000 }, async () => {
  const host = fakeHost();
  const view = await openTask(host, "live");
  await type(view, "Keep this draft");
  host.failNextTurn = { failure: Object.assign(new Error("Choose a project folder"), { deliveryState: "not_sent" }), admitted: false };
  await view.current.submit();
  await until(() => view.current.text === "Keep this draft" && view.current.error === "Choose a project folder", () => `draft/error: ${view.current.text} / ${view.current.error}`);
  assert.equal(view.current.error, "Choose a project folder");
  assert.equal(view.current.queuedTurns.length, 0);
  assert.equal(view.current.canSubmit, true);
});

test("Home storage blocking retains drafts, suspends sends and queue delivery, and keeps open turn evidence", async () => {
  const host = fakeHost();
  let state, emit;
  host.transport.bootStatus = async () => state;
  host.transport.onRuntimeHostState = (listener) => { emit = listener; return () => { emit = null; }; };
  const home = "/home/storage-fixture";
  state = { home, storageHealth: { status: "writable", message: null }, interfaceFailures: [] };
  const view = await mount(host, "live", home);
  view.current.selectRun("run-1");
  await until(() => !view.current.loadingMessages && view.current.activeRunId === "run-1", "task unavailable");
  await type(view, "Keep the disk-full draft");
  Object.assign(host.run("run-1"), { turn_open: true, turn_state: "working" });
  host.changed("ledger");
  await until(() => view.current.turnOpen, "open turn was not observed");
  await type(view, "Previously queued message");
  await view.current.queueCurrentTurn();
  await until(() => view.current.queuedTurns.length === 1, "follow-up was not queued");
  await type(view, "Keep the disk-full draft");
  state = { home, storageHealth: { status: "blocked", code: "ENOSPC", message: "Disk full. Sending is paused; your draft is retained." }, interfaceFailures: [] };
  emit(state);
  await until(() => Boolean(view.current.storageBlockReason), "storage warning missing");
  assert.equal(view.current.canSubmit, false);
  assert.equal(view.current.canQueue, false);
  assert.equal(view.current.canSteer, false);
  await view.current.submit();
  await view.current.queueCurrentTurn();
  await view.current.steerCurrentTurn();
  assert.equal(view.current.text, "Keep the disk-full draft");
  assert.equal(view.current.turnOpen, true);
  assert.equal(host.turns.length + host.steers.length, 0);
  assert.equal(view.current.queuedTurns.length, 1);
  host.reply("run-1", "Previous reply finished");
  await until(() => !view.current.turnOpen, "reply evidence was not observed");
  await settle();
  assert.equal(host.turns.length, 0);
  emit({ home: "/another-home", storageHealth: { status: "writable", message: null } });
  await settle();
  assert.ok(view.current.storageBlockReason);
  emit({ home, storageHealth: { status: "writable", message: null }, interfaceFailures: [] });
  await until(() => !view.current.storageBlockReason && host.turns.length === 1, "writable Home did not resume its saved queue");
  assert.equal(host.turns[0].text, "Previously queued message");
  host.reply("run-1", "Queued reply finished");
  await until(() => !view.current.busy, "queued reply did not finish");
  assert.equal(view.current.text, "Keep the disk-full draft");
});

test("a stopped bound provider blocks only its task and preserves the open turn and draft", async () => {
  const host = fakeHost();
  host.runs[0].provider_bound_interface_id = "provider.bound";
  host.runs.push(runRow("run-healthy", { provider_bound_interface_id: "provider.sibling" }));
  host.messages["run-healthy"] = [];
  let emit;
  const home = "/home/provider-health-fixture";
  host.transport.bootStatus = async () => ({ home, storageHealth: { status: "writable" }, interfaceFailures: [] });
  host.transport.onRuntimeHostState = (listener) => { emit = listener; return () => { emit = null; }; };
  const view = await mount(host, "live", home);
  view.current.selectRun("run-1");
  await until(() => !view.current.loadingMessages && view.current.activeRunId === "run-1", "task unavailable");
  await type(view, "Keep my provider draft");
  Object.assign(host.run("run-1"), { turn_open: true, turn_state: "working" }); host.changed("ledger");
  await until(() => view.current.turnOpen, "open turn missing");
  emit({ home, storageHealth: { status: "writable" }, interfaceFailures: [{ interfaceId: "provider.bound", message: "sandbox unavailable" }] });
  await until(() => Boolean(view.current.providerFailureReason), "provider warning missing");
  assert.match(view.current.runtimeBlockReason, /sandbox unavailable/);
  await view.current.submit();
  assert.equal(view.current.text, "Keep my provider draft"); assert.equal(view.current.turnOpen, true);
  assert.equal(view.current.canQueue, false); assert.equal(view.current.storageBlockReason, "");
  view.current.selectRun("run-healthy");
  await until(() => !view.current.loadingMessages && view.current.activeRunId === "run-healthy", "sibling unavailable");
  await type(view, "Healthy sibling message");
  await until(() => view.current.canSubmit, "unrelated life failure blocked a healthy task");
  assert.equal(view.current.runtimeBlockReason, ""); assert.equal(host.turns.length, 0);
});


test("a failed manual send preserves a newer composer draft and retains the refused message", async () => {
  const host = fakeHost();
  const view = await openTask(host, "live");
  await type(view, "Original message");
  let release;
  host.holdTurn = new Promise((resolve) => { release = resolve; });
  host.failNextTurn = { failure: Object.assign(new Error("Choose a project folder"), { deliveryState: "not_sent" }), admitted: false };
  const sending = view.current.submit();
  await until(() => view.current.text === "" && view.current.busy, "sending did not consume the original draft");
  await type(view, "New draft");
  release();
  await sending;
  await until(() => view.current.error === "Choose a project folder", "the refusal was not reported");
  assert.equal(view.current.text, "New draft");
  assert.equal(view.current.queuedTurns[0]?.text, "Original message");
  assert.equal(view.current.queuedTurns[0]?.status, "failed");
  view.current.setText("");
  await until(() => view.current.text === "Original message" && !view.current.queuedTurns.length, "the retained message did not return to the empty composer");
});

test("late admission clears a restored uncertain draft without resending it", async () => {
  const host = fakeHost();
  const view = await openTask(host, "live");
  await type(view, "Original message");
  host.failNextTurn = { failure: new Error("transport closed"), admitted: false };
  await view.current.submit();
  await until(() => view.current.text === "Original message", "the draft was not restored");
  host.append("run-1", "user", "Original message", { id: host.turns[0].messageId });
  host.reply("run-1", "Received.");
  await until(() => view.current.text === "" && !view.current.busy, "late admission left the original draft ready to duplicate");
  assert.equal(host.turns.length, 1);
});
