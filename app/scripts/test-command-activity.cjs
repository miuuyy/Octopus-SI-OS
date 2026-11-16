const assert = require("node:assert/strict");
const path = require("node:path");
const { test, after } = require("node:test");
const { build } = require("esbuild");
const { JSDOM } = require("jsdom");

const appRoot = path.resolve(__dirname, "..");
const dom = new JSDOM('<div id="root"></div>', { url: "http://fixture.invalid", pretendToBeVisual: true });
const w = dom.window;
for (const key of ["window", "document", "HTMLElement", "Element", "SVGElement", "Node", "Event", "CustomEvent", "MouseEvent", "KeyboardEvent", "WheelEvent", "navigator", "getComputedStyle", "localStorage", "sessionStorage"]) {
  Object.defineProperty(globalThis, key, { configurable: true, value: key === "getComputedStyle" ? w[key].bind(w) : key === "window" ? w : w[key] });
}
globalThis.requestAnimationFrame = w.requestAnimationFrame.bind(w);
globalThis.cancelAnimationFrame = w.cancelAnimationFrame.bind(w);
globalThis.IntersectionObserver = class { observe() {} unobserve() {} disconnect() {} };
globalThis.ResizeObserver = w.ResizeObserver = class { observe() {} unobserve() {} disconnect() {} };
w.matchMedia = (query) => ({ matches: query.includes("reduce"), addEventListener() {}, removeEventListener() {} });
w.scrollTo = () => {};
w.HTMLElement.prototype.scrollTo = function () {};
w.HTMLElement.prototype.scrollIntoView = function () {};
const copies = [], projectionListeners = new Set();
let currentHost = null;
w.octopusTransport = new Proxy({}, { get: (_, name) => name === "copyText" ? async (value) => { copies.push(value); }
  : name === "onProjectionChanged" ? (listener) => { projectionListeners.add(listener); return () => projectionListeners.delete(listener); }
    : currentHost?.transport[name] });
globalThis.indexedDB = (() => {
  const stores = new Map(), later = (callback) => setTimeout(callback, 0);
  // Transactions run one after another in the order they were created, as IndexedDB orders read-write transactions
  // over one store: a later one reads only what the earlier one committed, so two writers cannot lose an update.
  let settled = Promise.resolve();
  const database = { createObjectStore: (name) => { stores.set(name, new Map()); }, close() {}, transaction(name) {
    const store = stores.get(name), writes = [];
    const transaction = { aborted: false, error: null, abort() { transaction.aborted = true; later(() => transaction.onabort?.()); }, objectStore: () => ({
      get(key) {
        const request = {};
        settled = settled.then(() => new Promise((finish) => later(() => {
          request.result = store.has(key) ? structuredClone(store.get(key)) : undefined;
          request.onsuccess?.();
          later(() => { if (!transaction.aborted) { for (const [value, writeKey] of writes) store.set(writeKey, structuredClone(value)); transaction.oncomplete?.(); } finish(); });
        })));
        return request;
      },
      put(value, key) { writes.push([value, key]); },
    }) };
    return transaction;
  } };
  return { open() { const request = { result: database }; later(() => { if (!stores.has("queues")) request.onupgradeneeded?.(); request.onsuccess?.(); }); return request; } };
})();
globalThis.BroadcastChannel = class extends BroadcastChannel { constructor(name) { super(name); this.unref(); } };
const heldLocks = new Set();
Object.defineProperty(w.navigator, "locks", { configurable: true, value: {
  request: async (name, callback) => { heldLocks.add(name); try { return await callback(); } finally { heldLocks.delete(name); } },
  query: async () => ({ held: [...heldLocks].map((name) => ({ name })) }),
} });

const React = require("react");
const { createRoot } = require("react-dom/client");
const heavy = /^(@pierre\/diffs|shiki|@shikijs|mermaid|katex)/;
const stub = new Proxy({}, { get: (_, name) => name === "__esModule" ? true : name === "preloadHighlighter" ? async () => {} : () => null });
const fixtureModule = (name, filter, contents) => ({ name, setup(bundle) {
  bundle.onResolve({ filter }, () => ({ path: name, namespace: name }));
  bundle.onLoad({ filter: /.*/, namespace: name }, () => ({ loader: "js", contents }));
} });
const bundle = (contents, plugins = []) => build({
  stdin: { contents, resolveDir: appRoot, loader: "tsx" }, bundle: true, platform: "node", format: "cjs", write: false, logLevel: "silent", plugins,
  define: { "import.meta.env.DEV": "false", "import.meta.env.PROD": "true" },
  loader: { ".css": "empty", ".svg": "dataurl", ".png": "dataurl" },
  external: ["react", "react-dom", "react-dom/client", "react/jsx-runtime", "motion/react", "@pierre/diffs", "@pierre/diffs/*", "shiki", "@shikijs/*", "mermaid", "katex"],
}).then((result) => {
  const module = { exports: {} };
  Function("require", "module", "exports", result.outputFiles[0].text)((id) => heavy.test(id) ? stub : require(id), module, module.exports);
  return module.exports;
});
const ui = bundle(`export { ChatMessageList } from "./src/components/chat/ChatMessageList";
export { commandActivityModel } from "./src/components/chat/commandActivityModel";
export * from "./src/components/chat/toolPresentation";`, [fixtureModule("message-content", /\/MessageContent$/,
  `import { createElement } from "react"; export function MessageContent({ text }) { return createElement("span", { "data-content": "" }, text); }`)]);
const observationModule = () => bundle(`export * from "./src/components/chat/chatTurnObservation";`, [fixtureModule("observation-host", /\/(chatRuns|projectionClient|projectionLive)$/,
  ["loadChatRunMessages", "readOperatorChatArtifacts", "currentProjectionGeneration", "nextProjectionChange"]
    .map((name) => `export function ${name}(...args) { return globalThis.observationHost.${name}(...args); }`).join("\n"))]);

const container = document.getElementById("root");
const root = createRoot(container);
after(() => root.unmount());
const settle = async () => { for (let pass = 0; pass < 3; pass += 1) await new Promise((resolve) => setTimeout(resolve, 15)); };
const noop = () => {};
let renders = 0;
async function show(messages, { streaming = false, presentation = "conversation", toolPresentations = {} } = {}) {
  const { ChatMessageList } = await ui;
  root.render(React.createElement(ChatMessageList, {
    key: ++renders, activityPresentation: presentation, activeMessageCount: messages.length, conversationId: "task", findQuery: "", findActiveIndex: 0, onFindMatchCountChange: noop,
    dragOver: false, onDragOver: noop, onDragLeave: noop, onDrop: noop, footer: null, onOperatorRequestResponse: async () => {}, onOpenAssistantReview: noop,
    onOpenAssistantVisualization: noop, onOpenFile: noop, resolveAssistantVisualization: () => null, pinnedArtifactIds: [], pinningArtifactIds: [], toolPresentations,
    onTogglePinnedMessage: async () => {}, onPinMessageExcerpt: async () => {}, onContinueInNewTask: noop, onEditUserMessage: () => true, onSteerUserMessage: async () => true,
    steerableMessageId: "", revealPinnedContext: null, responseStreaming: streaming, onPinnedContextRevealed: noop, messages,
  }));
  await settle();
}
const all = (selector, scope = container) => [...scope.querySelectorAll(selector)];
const groupToggles = (scope = container) => all(".octopus-agent-activity-toggle", scope).filter((toggle) => !toggle.closest(".octopus-agent-activity-viewport"));
const summaries = (scope = container) => groupToggles(scope).map((toggle) => toggle.getAttribute("aria-label"));
async function openGroups(scope = container) {
  for (const toggle of groupToggles(scope)) if (toggle.getAttribute("aria-expanded") === "false") toggle.click();
  await settle();
}
const turn = (key) => container.querySelector(`[data-thread-turn-key="${key}"]`);
const rows = (scope = container) => all(".octopus-agent-activity-row", scope);
const exec = (scope = container) => all(".octopus-exec-activity", scope);
const spoken = (node) => { const copy = node.cloneNode(true); copy.querySelectorAll("[aria-hidden='true']").forEach((hidden) => hidden.remove()); return copy.textContent; };
const execSummary = (node) => spoken(node.querySelector(".octopus-exec-activity-summary"));
const button = (label, scope = container) => all("button", scope).find((node) => node.getAttribute("aria-label") === label);

const origin = { interface_id: "provider", interface_role: "provider" };
const ask = (id, createdAtMs = 1) => ({ id, role: "user", text: `Ask ${id}`, createdAtMs });
const event = (id, eventKind, eventData = {}, extra = {}) => ({ id, role: "assistant", runId: "run", interfaceId: "client", text: "", createdAtMs: 2, eventKind, eventData, ...extra });
const tool = (id, eventData, extra) => event(id, "dynamic_tool", { _activity_origin: origin, ...eventData }, extra);
const declaredCommand = { "unrelated.custom.execute": { conversationVisible: true, kind: "command", label: "Execute", queryArguments: [], targetArguments: ["command"] } };
const custom = (id, body, extra = {}) => tool(id, { tool: "unrelated.custom.execute", call_id: id, ok: true, status: "completed", arguments: { command: "arbitrary executable" }, body, ...extra });
const native = (id, output, status = "completed") => event(id, "tool", { call_id: id, command: "arbitrary executable", status, output });

test("process polling settles the original command and retains its output without merging another owner", async () => {
  const started = tool("exec-start", { tool: "workspace.exec__room", call_id: "exec", status: "started", arguments: { command: "long-command" } });
  const yielded = tool("exec-yield", { tool: "workspace.exec__room", call_id: "exec", status: "completed", ok: true,
    body: { process_id: "process", completed: false, output: { running: true, chunks: [{ source_seq: 1, stream: "stdout", content: { text: "first output" } }] } } });
  const poll = tool("poll", { tool: "workspace.write_stdin__room", call_id: "poll", status: "started", arguments: { process_id: "process", chars: "" } });
  const done = tool("poll-done", { tool: "workspace.write_stdin__room", call_id: "poll", status: "completed", ok: true,
    body: { process_id: "process", completed: true, output: { running: false, exited: true, exit_code: 0, chunks: [{ source_seq: 2, stream: "stdout", content: { text: "last output" } }] } } });
  await show([ask("u"), started, yielded, poll, done]); await openGroups();
  assert.equal(exec().length, 1);
  assert.equal(exec()[0].dataset.commandState, "exited");
  assert.match(exec()[0].textContent, /long-command/);
  assert.match(exec()[0].textContent, /first output/);
  assert.match(exec()[0].textContent, /last output/);
  await show([ask("u"), started, yielded, { ...done, runId: "other" }]); await openGroups();
  assert.equal(exec()[0].dataset.commandState, "running", "a different run cannot settle this process");
});

test("a tool result returns to the turn that started its exact call while unrelated results stay where they arrived", async () => {
  const user = ask("first"), queued = ask("queued", 3);
  const started = tool("call", { tool: "code_mode.run", call_id: "exact-call", status: "started", thread_id: "thread", turn_id: "native-turn", arguments: { script: "broken();" } });
  const failed = { ...started, id: "result", createdAtMs: 4, eventData: { ...started.eventData, status: "failed", ok: false, arguments: undefined, error: { message: "recorded native failure" } } };
  const activityIn = (key) => turn(key).querySelector(".octopus-turn-activity .octopus-agent-activity") !== null;
  await show([user, started, queued, failed]);
  assert.deepEqual([activityIn("first"), activityIn("queued")], [true, false]);
  await openGroups(turn("first"));
  const [row] = rows(turn("first"));
  assert.equal(row.dataset.state, "error", "a failed result settles the call it belongs to");
  assert.match(row.textContent, /broken\(\);/, "the result keeps the arguments its call started with");
  assert.match(row.textContent, /recorded native failure/);
  await show([user, started, failed], { streaming: true });
  await openGroups();
  assert.deepEqual(rows().map((node) => node.dataset.state), ["error"], "a settled call is no longer active while its turn works");
  await show([user, started, queued, failed], { presentation: "ledger" });
  assert.deepEqual([activityIn("first"), activityIn("queued")], [true, true], "the ledger keeps each movement where it arrived");
  const closure = { ...failed, id: "closed", createdAtMs: 5, eventKind: "interrupted_result", terminal: true, eventData: { _activity_origin: origin,
    correlation_id: "exact-call", thread_id: "thread", turn_id: "native-turn", returned_to_model: false, turn_interrupted: true } };
  await show([user, started, queued, failed, closure]);
  assert.deepEqual([/Interrupted/.test(turn("first").textContent), /Interrupted/.test(turn("queued").textContent)], [true, false]);
  await show([user, started, queued, { ...closure, eventData: { ...closure.eventData, turn_id: "other" } }]);
  assert.deepEqual([/Interrupted/.test(turn("first").textContent), /Interrupted/.test(turn("queued").textContent)], [false, true]);
  for (const foreign of [{ ...failed, runId: "other" }, { ...failed, interfaceId: "other" },
    { ...failed, eventData: { ...failed.eventData, _activity_origin: { ...origin, interface_id: "other" } } },
    { ...failed, eventData: { ...failed.eventData, call_id: "other" } }, { ...failed, eventData: { ...failed.eventData, tool: "other" } }]) {
    await show([user, started, queued, foreign]);
    assert.equal(activityIn("queued"), true, JSON.stringify(foreign.eventData));
  }
});

test("finished turns stop activity animation without inventing a tool outcome", async () => {
  const pending = tool("widget", { tool: "client.widgets", call_id: "widget", status: "started" });
  await show([ask("u"), pending], { streaming: true });
  await openGroups();
  assert.ok(container.querySelector(".octopus-cadenced-shimmer"));
  for (const end of [event("error", "turn_error", {}, { text: "Provider disconnected", error: true, terminal: true }),
    event("done", "message", { phase: "final_answer" }, { text: "Done", terminal: true })]) {
    await show([ask("u"), pending, end]);
    const history = container.querySelector(".octopus-worked-for-toggle");
    if (history) { history.click(); await settle(); }
    await openGroups();
    assert.equal(container.querySelector(".octopus-cadenced-shimmer"), null);
  }
  const { commandActivityModel } = await ui;
  const result = custom("exited", { process_id: "p", output: { running: true, exit_code: 0 } });
  assert.equal(commandActivityModel(result).phase, "exited");
  assert.equal(commandActivityModel(result).running, false);
});

test("every supplied command shape shows its command, an absent one says so, and code runs as code", async () => {
  const { dynamicToolPresentation } = await ui;
  for (const shape of [{ command: "printf 'hello world'" }, { arguments: { cmd: "printf 'hello world'" } }, { arguments: { argv: ["printf", "hello world"] } }, { parsed_cmd: { cmd: "printf 'hello world'" } }]) {
    await show([ask("u"), tool("run", { tool: "workspace.exec", call_id: "run", status: "completed", ok: true, ...shape })]);
    await openGroups();
    const [activity] = exec();
    assert.ok(activity, JSON.stringify(shape));
    assert.match(activity.textContent, /hello world/);
    assert.doesNotMatch(activity.textContent, /\$ Run/);
  }
  await show([ask("u"), tool("run", { tool: "workspace.exec", call_id: "run", status: "completed", ok: true })]);
  await openGroups();
  assert.match(exec()[0].textContent, /Command unavailable/);
  assert.ok(!button("Copy command"));
  const script = "const d = await prelude.cua.documentation(); return d;";
  const code = tool("code", { tool: "code_mode.run__42450f1bf38856e1fb9743bf91929c06", call_id: "code", status: "started", arguments: { script, yield_time_ms: 1000 } }, { createdAtMs: Date.now() });
  assert.equal(dynamicToolPresentation(code, {}).target, script);
  await show([ask("u"), code], { streaming: true });
  assert.deepEqual(summaries(), [`Running ${script}`]);
  await openGroups();
  const [running] = exec();
  assert.equal(execSummary(running), "Running code");
  assert.equal(running.querySelector(".octopus-exec-shell-command").textContent, script);
  assert.ok(button("Copy code", running));
  assert.equal(running.querySelector(".octopus-exec-shell-label").textContent, "Code");
  assert.doesNotMatch(running.textContent, /\$ Run|\$ const/);
});

test("code that reports an error fails, and yielded code stays active", async () => {
  const { commandActivityModel } = await ui;
  const script = "const d = await prelude.cua.documentation(); return d;";
  for (const errorText of ["SyntaxError: Illegal return statement", "TypeError: prelude.cua.documentation is not a function"]) {
    const result = tool("code", { tool: "code_mode.run__42450f1bf38856e1fb9743bf91929c06", call_id: "code", status: "completed", ok: true, arguments: { script },
      body: { cell_id: "1", state: "completed", output: [], error_text: errorText } });
    assert.equal(commandActivityModel(result).phase, "failed");
    await show([ask("u"), result], { streaming: true });
    await openGroups();
    const [row] = rows();
    assert.equal(row.dataset.state, "error");
    assert.match(execSummary(exec()[0]), /^Failed/);
    assert.ok(row.textContent.includes(errorText));
    assert.doesNotMatch(row.textContent, /Outcome unknown|Running code/);
  }
  const codeResult = custom("done", { cell_id: "2", state: "completed", output: [{ type: "text", text: "NEXT_CODE_OK" }] });
  assert.equal(commandActivityModel(codeResult).phase, "exited");
  assert.deepEqual(commandActivityModel(codeResult).sections, [{ stream: "", text: "NEXT_CODE_OK" }]);
  const codeYield = custom("yield", { cell_id: "3", state: "yielded", output: [] });
  assert.equal(commandActivityModel(codeYield).phase, "running");
  assert.equal(commandActivityModel(codeYield).yielded, true);
  await show([ask("u"), codeYield], { streaming: true, toolPresentations: declaredCommand });
  await openGroups();
  assert.equal(rows()[0].dataset.state, "active");
});

test("a yielded code cell and the wait that finishes it read as one completed cell", async () => {
  const script = "text(await prelude.cua.getState());";
  const run = tool("code", { tool: "code_mode.run__42450f1bf38856e1fb9743bf91929c06", call_id: "code", status: "completed", ok: true, arguments: { script },
    body: { cell_id: "1", state: "yielded", output: [], pending_tool_calls: ["tool-1"] } });
  const waitStarted = tool("wait-start", { tool: "code_mode.wait__42450f1bf38856e1fb9743bf91929c06", call_id: "wait", status: "started", arguments: { cell_id: "1", yield_time_ms: 20000 } });
  const waitDone = tool("wait-end", { tool: "code_mode.wait__42450f1bf38856e1fb9743bf91929c06", call_id: "wait", status: "completed", ok: true, arguments: { cell_id: "1" },
    body: { cell_id: "1", state: "completed", output: [{ type: "text", text: "CELL_STATE_OK" }], pending_tool_calls: [] } });
  await show([ask("u"), run, waitStarted], { streaming: true });
  await openGroups();
  assert.equal(exec().length, 1, "the wait joins the cell it waits on");
  assert.equal(exec()[0].dataset.state, "active");
  await show([ask("u"), run, waitStarted, waitDone]);
  await openGroups();
  const [cell] = exec();
  assert.equal(exec().length, 1);
  assert.equal(cell.dataset.state, "complete");
  assert.equal(cell.querySelector(".octopus-exec-shell-command").textContent, script);
  assert.match(cell.textContent, /CELL_STATE_OK/);
  assert.doesNotMatch(cell.textContent, /still running/i);
  const other = { ...waitDone, id: "other-wait", eventData: { ...waitDone.eventData, call_id: "other", arguments: { cell_id: "9" }, body: { ...waitDone.eventData.body, cell_id: "9" } } };
  await show([ask("u"), run, other]);
  await openGroups();
  assert.equal(exec()[0].dataset.state, "active", "a wait on another cell leaves this cell running");
});

test("a command call shows its started arguments with its completed result", async () => {
  const begin = tool("start", { call_id: "command-call", status: "started", tool: "workspace.build__0123456789abcdef0123456789abcdef", arguments: { command: ["/bin/sh", "-c", "pwd"] } });
  const complete = { ...begin, id: "end", eventData: { _activity_origin: origin, tool: begin.eventData.tool, call_id: "command-call", status: "completed", ok: true,
    body: { output: { exit_code: 0, stdout: { text: "/work" } } } } };
  await show([ask("u"), begin], { streaming: true });
  assert.deepEqual(summaries(), ["Running /bin/sh -c pwd"]);
  await show([ask("u"), begin, complete]);
  await openGroups();
  const [activity] = exec();
  assert.equal(activity.querySelector(".octopus-exec-shell-command code").textContent, "/bin/sh -c pwd");
  assert.match(activity.querySelector(".octopus-exec-shell-output").textContent, /\/work/);
  assert.equal(activity.dataset.commandState, "exited");
  assert.match(activity.textContent, /Shell/);
  assert.match(activity.textContent, /Success/);
  assert.equal(activity.closest(".structured-activity, [data-testid='multi-agent-action-rows']"), null);
});

test("file and artifact reads name their exact target", async () => {
  const { dynamicToolPresentation, dynamicToolReadLabel } = await ui;
  for (const suffix of ["0123456789abcdef01234567", "0123456789abcdef0123456789abcdef"]) {
    for (const [name, args, label] of [
      ["workspace.read_file", { path: "/work/README.md" }, "Read README.md"],
      ["workspace.list_dir", { path: "/work" }, "Listed files in /work"],
      ["artifact.read", { id: "docs.README.md:db13113ae69d4a945ec05fd595cbf83a41dd9decd3e96ca6899bc2f6e5149a6c" }, "Read artifact docs.README.md"],
      ["artifact.list", {}, "Listed artifacts"],
    ]) {
      const started = tool("start", { tool: `${name}__${suffix}`, call_id: "read-call", status: "started", arguments: args });
      const completed = { ...started, id: "end", eventData: { _activity_origin: origin, tool: started.eventData.tool, call_id: "read-call", status: "completed", ok: true, body: { result: "read" } } };
      await show([ask("u"), started, completed]);
      await openGroups();
      assert.equal(rows()[0].textContent, label, `${name}__${suffix}`);
      if (name.startsWith("artifact.")) {
        const presentation = dynamicToolPresentation({ eventData: { tool: started.eventData.tool, arguments: args } }, {});
        assert.equal(presentation.path, "");
        if (args.id) assert.equal(presentation.target, args.id);
      }
    }
  }
  assert.equal(dynamicToolPresentation({ eventData: { tool: "artifact.read", arguments: { id: "opaque:artifact-id" } } }, {}).pathLabel, "opaque:artifact-id");
  assert.equal(dynamicToolReadLabel(dynamicToolPresentation({ eventData: { tool: "workspace.list_dir", arguments: { limit: 50 }, body: { path: "." } } }, {}), false), "Listed files in .");
  assert.equal(dynamicToolPresentation({ eventData: { tool: "unknown_tool", body: { path: "/unrelated" } } }, {}).target, "");
});

test("process listings, run inspection and continuation receipts are operations, not shell commands", async () => {
  const { dynamicToolPresentation, dynamicToolOperationLabel, nativeRunOperationPresentation } = await ui;
  const processList = tool("process", { tool: "process.list", call_id: "process", arguments: {}, status: "completed", body: { process_id: null, exit_code: null, status: "idle" } });
  assert.equal(dynamicToolPresentation(processList, {}).family, "tool");
  await show([ask("u"), processList]);
  await openGroups();
  assert.equal(container.querySelector(".octopus-exec-shell, .glyph-terminal"), null);
  const inspected = tool("inspect", { namespace: "formed_interfaces", tool: "run_list", call_id: "inspect", arguments: {}, status: "completed",
    body: { source_run: { run_id: "run-a" }, declared_peer_interfaces: ["peer-interface"] } });
  await show([ask("u"), inspected]);
  assert.ok(container.querySelector(".octopus-agent-activity-header .glyph-plugins"), "Octopus interface operations carry the plugin mark");
  await openGroups();
  const operation = rows()[0].querySelector(".octopus-agent-activity-toggle");
  assert.match(operation.getAttribute("aria-label"), /^Inspected run/);
  assert.equal(operation.getAttribute("aria-expanded"), "false");
  assert.doesNotMatch(rows()[0].textContent, /peer-interface/);
  assert.equal(rows()[0].querySelector(".glyph-tool, .glyph-terminal"), null);
  assert.doesNotMatch(rows()[0].textContent, /Ran a command|Success/);
  operation.click(); await settle();
  assert.equal(operation.getAttribute("aria-expanded"), "true");
  assert.match(rows()[0].textContent, /peer-interface/);
  const continued = { namespace: "formed_interfaces", tool: "run_continue", call_id: "continue", status: "completed",
    arguments: { interface: "peer-interface", text: "Continue the assigned task" }, body: { admitted: true, recipient_action_confirmed: false, work_completed: false } };
  const continuation = dynamicToolPresentation({ eventData: continued }, {});
  assert.equal(continuation.family, "tool");
  assert.equal(continuation.target, "peer-interface");
  assert.equal(dynamicToolOperationLabel(continuation), "Requested run continuation");
  assert.deepEqual(continuation.operation.details.slice(2, 5), [["Admission", "Accepted"], ["Recipient action", "Not confirmed"], ["Work", "Not completed"]]);
  const refused = { ...continued, status: "failed", error: { message: "Request failed" }, body: { ok: false, error: "Peer did not admit continuation" } };
  const refusal = nativeRunOperationPresentation({ eventKind: "dynamic_tool", eventData: refused });
  assert.equal(refusal.label, "Continuation request failed");
  assert.deepEqual(refusal.details.at(-1), ["Error", "Request failed\nPeer did not admit continuation"]);
  assert.equal(nativeRunOperationPresentation({ eventData: { ...continued, namespace: "unrelated" } }), null);
  for (const eventData of [continued, refused]) {
    await show([ask("u"), tool("continue", eventData)]);
    await openGroups();
    rows()[0].querySelector(".octopus-agent-activity-toggle").click(); await settle();
    const text = rows()[0].textContent;
    assert.equal(rows()[0].querySelector(".glyph-terminal, .octopus-exec-shell, [data-testid='multi-agent-action-rows']"), null);
    assert.doesNotMatch(text, /Arguments|Operation|work_completed/);
    assert.match(text, eventData === refused ? /Peer did not admit continuation/ : /Admission[\s\S]*Accepted[\s\S]*Recipient action[\s\S]*Not confirmed[\s\S]*Work[\s\S]*Not completed/);
  }
});

test("web searches read as searches whether the provider or a tool performs them", async () => {
  const { webSearchActivityDetail, dynamicToolPresentation } = await ui;
  for (const [action, query, expected] of [
    [null, "", ""], [{ kind: "other" }, "", ""],
    [{ kind: "search", queries: ["", "contract site:example.org", "second query"] }, "stale", "contract | example.org ..."],
    [{ kind: "open_page", url: "https://example.org/page" }, "stale", "https://example.org/page"],
    [{ kind: "find_in_page", url: "https://example.org/page", pattern: "term" }, "stale", "'term' in https://example.org/page"],
  ]) assert.equal(webSearchActivityDetail({ text: "turn.web_search", eventData: { action, query } }), expected);
  const query = "requirements site:example.org";
  for (const name of ["web.search", "web.search__0123456789abcdef0123456789abcdef"]) {
    const started = tool("search", { tool: name, call_id: "search", status: "started", arguments: { query } });
    const ended = { ...started, id: "searched", eventData: { _activity_origin: origin, tool: name, call_id: "search", status: "completed", ok: true, body: { results: [] } } };
    assert.equal(dynamicToolPresentation(started, {}).family, "web");
    assert.equal(dynamicToolPresentation(started, {}).path, "");
    await show([ask("u"), started], { streaming: true });
    assert.deepEqual(summaries(), ["Searching the web for requirements | example.org"]);
    await show([ask("u"), started, ended]);
    assert.deepEqual(summaries(), ["Searched the web"]);
    assert.ok(container.querySelector(".octopus-agent-activity-header .glyph-search"));
    await openGroups();
    const searchActivity = () => container.querySelector(".octopus-turn-activity .octopus-inline-activity").outerHTML;
    const viaTool = searchActivity();
    assert.equal(viaTool.split("requirements").length - 1, 1);
    await show([ask("u"), event("native-search", "web_search", { status: "completed", query, action: { kind: "search", query } })]);
    await openGroups();
    assert.equal(searchActivity(), viaTool);
  }
});

test("a group names each kind of work it settled and run operations keep their plugin mark", async () => {
  const runCreate = tool("create", { tool: "run_create", call_id: "create-call", status: "failed" });
  const begin = tool("start", { call_id: "command-call", status: "started", tool: "workspace.build__0123456789abcdef0123456789abcdef", arguments: { command: ["/bin/sh", "-c", "pwd"] } });
  const complete = { ...begin, id: "end", eventData: { ...begin.eventData, status: "completed", ok: true, body: { output: { exit_code: 0, stdout: { text: "/work" } } } } };
  await show([ask("u"), runCreate, begin, complete]);
  assert.deepEqual(summaries(), ["Ran a command, run create"]);
  await show([ask("u"), { ...runCreate, eventData: { ...runCreate.eventData, status: "started" } }], { streaming: true });
  assert.deepEqual(summaries(), ["Run create"]);
  await show([ask("u"), runCreate]);
  assert.ok(container.querySelector(".octopus-agent-activity-header .glyph-plugins"));
  const { dynamicToolPresentation, dynamicToolGlyph } = await ui;
  assert.equal(dynamicToolPresentation({ eventData: { tool: "custom_tool", arguments: { command: "pwd" } } }, {}).family, "tool");
  assert.equal(dynamicToolGlyph(dynamicToolPresentation({ eventData: { namespace: "formed_interfaces", tool: "run_list", arguments: {} } }, {})), "plugins");
});

test("Thinking shows only while nothing else reports progress", async () => {
  const progress = () => all(".octopus-thinking-fallback").map(spoken);
  const activeGroups = () => all(".octopus-turn-activity-content > .octopus-agent-activity[data-state='active']").length;
  const commentary = event("commentary", "commentary", { status: "completed" }, { text: "Progress explained" });
  const settledTool = tool("settled", { tool: "run_list", call_id: "settled", arguments: {}, status: "completed" });
  const request = event("approval", "approval", {}, { text: "Allow?" });
  for (const [label, activities, streaming, expected, groups = 0] of [
    ["nothing yet", [], true, ["Thinking"]], ["nothing yet, settled", [], false, []],
    ["after commentary", [commentary], true, ["Thinking"]], ["after commentary, settled", [commentary], false, []],
    ["a group then commentary", [settledTool, commentary], true, ["Thinking"]], ["a trailing group", [settledTool], true, [], 1],
    ["streamed reasoning", [event("reasoning", "reasoning_delta", { summary_index: 0 }, { text: "**Planning**" })], true, ["Planning"]],
    ["streamed commentary", [event("commentary", "commentary_delta", {}, { text: "Working through it" })], true, []],
    ["a running command", [native("running", { running: true, exited: false, exit_code: null }, "started"), commentary], true, [], 1],
    ["an open request", [request], true, []], ["a resolved request", [{ ...request, eventData: { resolved: true } }], true, ["Thinking"]],
    ["a reply not yet final", [commentary, event("pending", "message_delta", {}, { text: "" })], true, ["Thinking"]],
    ["a final reply", [commentary, event("final", "message_delta", { phase: "final_answer" }, { text: "The answer" })], true, []],
    ["summarized reasoning", [commentary, event("reasoning", "reasoning", { status: "completed", summary: ["**Next step**"] }, { text: "**Next step**" })], true, ["Next step"]],
  ]) {
    await show([ask("u"), ...activities], { streaming });
    assert.deepEqual([progress(), activeGroups()], [expected, groups], label);
  }
});

test("commands report every outcome from their own evidence", async () => {
  const { commandActivityModel } = await ui;
  const shown = async (message) => {
    await show([ask("u"), message], { toolPresentations: declaredCommand });
    await openGroups();
    return exec()[0];
  };
  const success = native("command", { exit_code: 0, aggregated_output: "exact output\n", duration_ms: 1200 });
  assert.equal(commandActivityModel(success).phase, "exited");
  const ran = await shown(success);
  assert.equal(execSummary(ran), "Ran arbitrary executable for 1s");
  assert.equal(ran.dataset.commandState, "exited");
  assert.equal(ran.querySelector(".octopus-exec-shell-label").textContent, "Shell");
  assert.match(ran.querySelector(".octopus-exec-shell-success").textContent, /Success/);
  button("Copy output", ran).click(); await settle();
  assert.equal(copies.at(-1), "exact output\n");
  const nonzero = custom("nonzero", { completed: true, output: { exit_code: 7, stdout: { text: "prefix\n" } } });
  const failedRun = await shown(nonzero);
  assert.match(failedRun.textContent, /Exit code 7/);
  assert.match(execSummary(failedRun), /^Ran arbitrary executable/);
  assert.equal(rows()[0].dataset.state, "error");
  const outputDelta = event("delta", "tool_output_delta", { call_id: "command" }, { text: "more" });
  await show([ask("u"), success, outputDelta], { streaming: true });
  await openGroups();
  assert.deepEqual(rows().map((row) => row.dataset.state), ["complete"], "output after an exit cannot reopen a finished command");
  const yielded = custom("yielded", { process_id: "p1", completed: false, continuation_available: true,
    output: { running: true, exited: false, closed: false, exit_code: null, chunks: [{ stream: "stdout", content: { text: "  prefix\n" } }] } });
  const running = commandActivityModel(yielded);
  assert.deepEqual([running.invocationStatus, running.phase, running.yielded], ["completed", "running", true]);
  await show([ask("u"), yielded], { streaming: true, toolPresentations: declaredCommand });
  await openGroups();
  assert.equal(rows()[0].dataset.state, "complete", "the status read completed; the process remains explicitly running at its last observation");
  assert.match(execSummary(exec()[0]), /^Command running at last check$/);
  assert.equal(exec()[0].querySelector(".octopus-cadenced-shimmer"), null, "a returned process snapshot does not pretend to be live telemetry");
  assert.doesNotMatch(exec()[0].textContent, /Exit code|Success/);
  const controlled = custom("controlled", { ...yielded.eventData.body, control: { action: "interrupt", delivered: true } });
  assert.equal(commandActivityModel(controlled).phase, "running");
  assert.match((await shown(controlled)).textContent, /Process control delivered: interrupt/);
  const timedOut = custom("timed", { completed: false, output: { timed_out: true, exit_code: -1, stdout: { text: "retained" }, stdout_truncated: true, stdout_omitted_bytes: 17, stdout_retention: "head_tail" } });
  assert.equal(commandActivityModel(timedOut).exitCode, null);
  const timed = await shown(timedOut);
  assert.match(execSummary(timed), /^Timed out/);
  assert.match(timed.textContent, /17 bytes omitted/);
  assert.doesNotMatch(timed.textContent, /Exit code -1|Success/);
  const exitedWithOutput = custom("more", { completed: false, continuation_available: true, output: { exited: true, closed: false, running: false, exit_code: 0 } });
  assert.equal(commandActivityModel(exitedWithOutput).phase, "exited");
  assert.match((await shown(exitedWithOutput)).textContent, /More output is available/);
  assert.equal(commandActivityModel(native("stopped", { aggregated_output: "partial" }, "interrupted")).phase, "interrupted");
  assert.equal(commandActivityModel(custom("closed", { completed: false, output: { closed: true, running: false } })).phase, "unknown");
  assert.match((await shown(native("failed", { exit_code: 7 }, "failed"))).textContent, /Failed · exit code 7/);
  assert.match(execSummary(await shown(custom("unknown", {}))), /^Outcome unknown/);
  const failed = custom("reader", undefined, { ok: false, status: "failed", error: { code: "tool_execution_failed", message: "output reader failed", details: {
    process_id: "p1", completed: false, output: { running: true, failure: "output reader failed", chunks: [
      { stream: "stdout", source_seq: 1, content: { text: " first\n" } },
      { stream: "stderr", source_seq: 2, content: { encoding: "base64", retained_bytes: 3, base64: "/wph" }, preview: { text: "�\na", lossy: true } },
    ] },
  } } });
  const failedModel = commandActivityModel(failed);
  assert.equal(failedModel.phase, "failed");
  assert.deepEqual(failedModel.sections.map(({ stream, text }) => [stream, text]), [["stdout", " first\n"], ["stderr", "�\na"]]);
  const reader = await shown(failed);
  for (const notice of [/output reader failed/, /process is still running/, /replacement characters/]) assert.match(reader.textContent, notice);
  assert.equal(commandActivityModel({ ...yielded, eventData: { ...yielded.eventData, tool: "renamed.tool", arguments: { command: "completely different" } } }).phase, "running");
  await show([ask("u"), tool("started", { tool: "unrelated.custom.execute", call_id: "started", status: "started", body: { unrelated: true } })], { streaming: true, toolPresentations: declaredCommand });
  await openGroups();
  assert.equal(rows()[0].dataset.state, "active");
  const preview = { ...success, eventData: { ...success.eventData, _payload_projection: { truncated: true, contains_process_output: true } } };
  assert.match((await shown(preview)).textContent, /partial preview/);
  assert.doesNotMatch((await shown({ ...preview, eventData: { ...preview.eventData, _payload_projection: { truncated: true, resolved: true } } })).textContent, /partial preview/);
});

test("live and historical projections mark the same process output and drop the same overflow", () => {
  const markers = require("../electron/projection/operatorChatEvents.cjs");
  const failure = { ok: false, status: "failed", error: { code: "tool_execution_failed", message: "output reader failed", details: { completed: false, output: { running: true, chunks: [{ stream: "stdout", content: { text: " first\n" } }] } } } };
  assert.equal(markers.clientEventHasProcessOutput(failure), true);
  assert.equal(markers.clientEventHasProcessOutput({ body: { unrelated: "value" } }), false);
  const historical = markers.clientEventDataProjection;
  const live = require("../../registry/interface/world_operator_chat/client/liveEvents.cjs").compactOperatorChatLiveEventData;
  for (const wrap of [
    (output) => ({ body: { completed: false, output } }), (output) => ({ result: { completed: false, output } }), (output) => ({ output }),
    (output) => ({ error: { code: "capture_failed", message: "partial", details: { completed: false, output } } }),
  ]) {
    const small = wrap({ stdout: { text: "exact" }, running: true, exit_code: null });
    for (const compact of [live, historical]) assert.deepEqual(compact(small), small);
    for (const output of [{ stdout: { text: "x".repeat(12_000) } }, { stderr: "x".repeat(12_000) }, { chunks: [{ stream: "stdout", content: { text: "x".repeat(12_000) } }] }]) {
      for (const overflow of [false, true]) {
        const payload = { ...wrap(output), ...(overflow ? { padding: Object.fromEntries(Array.from({ length: 12 }, (_, i) => [`field${i}`, "y".repeat(1500)])) } : {}) };
        const [l, h] = [live(payload), historical(payload)];
        assert.deepEqual(l._payload_projection, h._payload_projection);
        assert.equal(l._payload_projection.contains_process_output, true);
        if (overflow) assert.equal(l.padding, undefined);
      }
    }
    for (const output of [{}, { stdout: "", stderr: {}, chunks: [] }, { unrelated: "x".repeat(12_000) }]) {
      const payload = { ...wrap(output), padding: "z".repeat(12_000) };
      for (const compact of [live, historical]) assert.equal(compact(payload)._payload_projection.contains_process_output, undefined);
    }
  }
});

test("a reply observer reads past a failed stop to the answer, gives way to a retired view, and recovers a dropped read", async () => {
  const { observeChatReply, observeChatArtifacts } = await observationModule();
  const request = (accessProfile, isCurrent, onArtifacts = () => {}) => ({ accessProfile, knownRunId: "run-a", interfaceId: "interface-a", afterSeq: 10, cursor: {}, isCurrent, onArtifacts });
  for (const accessProfile of ["live", "lab"]) {
    let reads = 0;
    const failed = { source: "movement", direction: "out", movementSeq: 11, terminal: true, error: true,
      event_kind: "turn_control", event_data: { action: "stop", status: "failed" }, eventKind: "turn_control", eventData: { action: "stop", status: "failed" } };
    const completed = { ...failed, movementSeq: 12, artifact_id: "answer", artifactId: "answer", error: false, event_kind: "message", event_data: {}, eventKind: "message", eventData: {} };
    globalThis.observationHost = {
      readOperatorChatArtifacts: async () => ({ cursor: {}, artifacts: [++reads === 1 ? failed : completed] }),
      loadChatRunMessages: async () => ++reads === 1 ? [failed] : [failed, completed],
      currentProjectionGeneration: () => 1, nextProjectionChange: async () => {},
    };
    const result = await observeChatReply(request(accessProfile, () => true));
    assert.equal(reads, 2, accessProfile);
    assert.equal(result.terminal.artifactId, "answer");
  }
  globalThis.observationHost = { readOperatorChatArtifacts: async () => { throw new Error("connection closed before the next result"); }, currentProjectionGeneration: () => 1 };
  await assert.rejects(observeChatReply(request("live", () => true, () => assert.fail("no receipt was delivered"))), /connection closed/);
  for (const accessProfile of ["live", "lab"]) for (const reject of [false, true]) {
    let current = true, reads = 0;
    globalThis.observationHost = {
      readOperatorChatArtifacts: async () => { reads++; current = false; if (reject) throw new Error("retired reader"); return { cursor: {}, artifacts: [{ source: "movement", terminal: true }] }; },
      loadChatRunMessages: async () => { reads++; current = false; if (reject) throw new Error("retired reader"); return [{ direction: "out", movementSeq: 11, terminal: true }]; },
      currentProjectionGeneration: () => 1,
    };
    const result = await observeChatReply(request(accessProfile, () => current, () => assert.fail("a retired view must not consume receipts")));
    assert.equal(result, null); assert.equal(reads, 1);
  }
  let current = true, reads = 0, waits = 0, failures = 0, deliveries = 0;
  globalThis.observationHost = {
    readOperatorChatArtifacts: async (input) => {
      reads++; assert.equal(input.afterSeq, 10); assert.equal(input.cursor, undefined);
      if (reads === 1) throw new Error("read disconnected");
      return { cursor: { movement_seq: 11 }, artifacts: [{ source: "movement", movement_seq: 11 }] };
    },
    currentProjectionGeneration: () => 1, nextProjectionChange: async () => { waits++; },
  };
  await observeChatArtifacts({ runId: "run-a", interfaceId: "interface-a", afterSeq: 10, isCurrent: () => current,
    onFailure: () => { failures++; }, onRecovered: () => { current = false; },
    onArtifacts: () => { deliveries++; if (deliveries === 1) throw new Error("receipt not consumed"); } });
  assert.deepEqual({ reads, waits, failures, deliveries }, { reads: 3, waits: 2, failures: 2, deliveries: 2 });
});

const provider = { id: "provider.test", label: "Test provider", descriptorPath: "registry/interface/provider.test", sourceNames: [], supportedReasoningEfforts: ["medium"],
  models: [{ model: "model-a", displayName: "Model A", description: "", defaultReasoningEffort: "medium", supportedReasoningEfforts: ["medium"], showInPicker: true, isDefault: true }] };
function workspaceHost() {
  const row = (seq, role, text, extra = {}) => ({ id: `run-1:${seq}`, run_id: "run-1", role, text, created_at_ms: seq * 100, movement_seq: seq, artifact_id: `run-1.artifact.${seq}`,
    direction: role === "user" ? "in" : "out", interface_id: "chat.run-1", event_kind: "message", event_data: { _movement: { seq } }, terminal: true, ...extra });
  const host = {
    seq: 2, turns: [], holdMessages: null, replyOnSubmit: false, messages: [row(1, "user", "Draft the report"), row(2, "assistant", "Here is the report.")],
    run: { run_id: "run-1", title: "Task", chat_interface_id: "chat.run-1", client_surface_role: "task", writable: true, ee_id: "ee.root", provider_interface_id: provider.id,
      provider_bound_interface_id: provider.id, provider_model: "model-a", reasoning_effort: "medium", worker_interface_id: "worker.test", turn_open: false, turn_state: "idle",
      last_movement_seq: 2, updated_at_ms: 1_000 },
    append(role, text, extra) {
      const seq = ++host.seq;
      host.messages.push(row(seq, role, text, extra));
      host.run.last_movement_seq = seq;
      setTimeout(() => { if (currentHost === host) for (const listener of [...projectionListeners]) listener({ kind: "ledger" }); }, 0);
      return seq;
    },
  };
  host.transport = {
    projectionRead: async ({ method }) => {
      if (method === "projection.operator.chats.list" || method === "projection.operator.chats.status") return { chats: [{ ...host.run }] };
      if (method === "projection.operator.chat.messages") { if (host.holdMessages) await host.holdMessages; return { messages: structuredClone(host.messages) }; }
      if (method === "projection.operator.chat.contexts") return { contexts: [{ ee_id: "ee.root", is_os_root: true, display_name: "Root", agent_catalog_digest: "catalog-1",
        workers: [{ id: "worker.test", label: "worker.test", provider_interface_ids: [provider.id], interface_policy: { [provider.id]: { models: ["model-a"] } } }] }] };
      if (method === "projection.worker_environments.list") return { worker_environments: [] };
      if (method === "projection.tools.list") return { tools: [] };
      if (method === "projection.operator.providers.status") return { providers: [] };
      return {};
    },
    providerInterfaces: async () => ({ repoRoot: "/repo", providers: [structuredClone(provider)] }),
    defaults: async () => ({ endpoint: "http://runtime.invalid" }),
    readOperatorChatArtifacts: async () => { throw new Error("connection closed before the next result"); },
    submitChatTurn: async (request) => {
      host.turns.push(structuredClone(request));
      const seq = host.append("user", request.text, { id: request.messageId });
      if (host.replyOnSubmit) host.append("assistant", "Already answered.");
      return { runId: "run-1", chatInterfaceId: "chat.run-1", messageVerdict: { admitted: { seq } }, outboundCursor: null, messageRecovered: false };
    },
    submitOperatorTurnControl: async () => ({ accepted: true }),
  };
  return host;
}
const workspaceModule = bundle(`export { useChatWorkspace } from "./src/components/chat/useChatWorkspace"; export { ClientPreferencesProvider } from "./src/clientPreferences";`);
const workspaceRoot = createRoot(document.createElement("div"));
after(async () => { workspaceRoot.unmount(); currentHost = null; await new Promise((resolve) => setTimeout(resolve, 50)); w.close(); });
const until = async (predicate, message) => {
  for (let attempt = 0; attempt < 400; attempt += 1) { if (predicate()) return; await new Promise((resolve) => setTimeout(resolve, 5)); }
  assert.fail(typeof message === "function" ? message() : message);
};
async function sendUnobserved(host) {
  const { useChatWorkspace, ClientPreferencesProvider } = await workspaceModule;
  currentHost = host;
  const view = { current: null };
  function Harness({ home }) { view.current = useChatWorkspace("live", true, true, "fixture", "task", home); return null; }
  view.render = (home) => workspaceRoot.render(React.createElement(ClientPreferencesProvider, { profile: "live" }, React.createElement(Harness, { home })));
  view.render(`/home/${Math.random().toString(16).slice(2)}`);
  await until(() => view.current, "the workspace never mounted");
  view.current.selectRun("run-1");
  await until(() => !view.current.loadingMessages && view.current.renderedMessages.length >= 2, "the task never loaded");
  view.current.setText("question");
  await until(() => view.current.canSubmit, () => `never ready: ${view.current.submitBlockReason}`);
  return view;
}
const notices = (view) => view.current.renderedMessages.filter((message) => message.eventData?._observation_error === true);

test("an accepted turn whose reply cannot be observed stays sent, stoppable and marked unconfirmed until a newer receipt arrives", { timeout: 20_000 }, async () => {
  const host = workspaceHost();
  const view = await sendUnobserved(host);
  await view.current.submit();
  await until(() => notices(view).length === 1, "the lost observation was never shown");
  assert.equal(host.turns.length, 1, "an accepted turn is never resubmitted");
  const [notice] = notices(view);
  assert.equal(notice.terminal, false);
  assert.match(notice.text, /connection closed/);
  assert.ok(view.current.renderedMessages.some((message) => message.role === "user" && message.text === "question"));
  assert.equal(view.current.turnOpen, true, "an unconfirmed turn can still be stopped");
  host.append("assistant", "Late answer.");
  await until(() => view.current.renderedMessages.some((message) => message.text === "Late answer."), "the newer receipt never arrived");
  assert.deepEqual(notices(view), [], "a newer receipt on the run supersedes the notice");
});

test("a reply already recorded when observation fails leaves nothing pending", { timeout: 20_000 }, async () => {
  const host = workspaceHost();
  host.replyOnSubmit = true;
  const view = await sendUnobserved(host);
  await view.current.submit();
  await until(() => view.current.renderedMessages.some((message) => message.text === "Already answered.") && !view.current.busy, "the recorded reply never showed");
  assert.deepEqual(notices(view), []);
  assert.equal(view.current.turnOpen, false);
  assert.equal(host.turns.length, 1);
});

test("a Home switch while the failed turn is re-read leaves the new Home untouched", { timeout: 20_000 }, async () => {
  const host = workspaceHost();
  const view = await sendUnobserved(host);
  let release, observed = false;
  const held = new Promise((resolve) => { release = resolve; });
  host.transport.readOperatorChatArtifacts = async () => { observed = true; host.holdMessages = held; throw new Error("connection closed before the next result"); };
  const sent = view.current.submit();
  await until(() => observed, "the reply was never observed");
  view.render(`/home/${Math.random().toString(16).slice(2)}`);
  await new Promise((resolve) => setTimeout(resolve, 30));
  host.holdMessages = null; release();
  await sent;
  view.current.selectRun("run-1");
  await until(() => !view.current.loadingMessages && view.current.renderedMessages.some((message) => message.text === "question"), "the new Home never read the task");
  await new Promise((resolve) => setTimeout(resolve, 50));
  assert.deepEqual(notices(view), []);
  assert.equal(view.current.turnOpen, false);
});
