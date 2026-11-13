const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { once } = require("node:events");
const { test } = require("node:test");
const ts = require("typescript");
const { WebSocketServer } = require("ws");
const { captureArtifactDelivery } = require("../electron/interfaceTransport.cjs");
const root = path.resolve(__dirname, "../..");
function source(file) { return require("./splitSource.cjs").readSource(path.join(root, file)); }
function declaration(file, name) {
  const ast = ts.createSourceFile(file, source(file), ts.ScriptTarget.Latest, true);
  let found;
  function visit(node) {
    if (ts.isFunctionDeclaration(node) && node.name?.text === name) found = node.getText(ast);
    ts.forEachChild(node, visit);
  }
  visit(ast); assert.ok(found, name);
  return ts.transpileModule(found, { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText;
}
function callbackDeclaration(file, name) {
  const ast = ts.createSourceFile(file, source(file), ts.ScriptTarget.Latest, true);
  let callback;
  function visit(node) {
    if (ts.isVariableDeclaration(node) && node.name.getText(ast) === name) callback = node.initializer.arguments[0].getText(ast);
    ts.forEachChild(node, visit);
  }
  visit(ast); assert.ok(callback);
  return ts.transpileModule(`const ${name} = ${callback};`, {compilerOptions:{target:ts.ScriptTarget.ES2022}}).outputText;
}

test("actual socket admission distinguishes pre-dispatch failure, unknown outcome and verdict", async (t) => {
  const scratch = fs.mkdtempSync(path.join(os.tmpdir(), "oc-dlv-"));
  const endpoint = path.join(scratch, "s");
  const server = require("node:net").createServer();
  const http = require("node:http").createServer();
  const wss = new WebSocketServer({ server: http });
  let mode = "admitted", received = 0;
  wss.on("connection", (socket) => socket.once("message", () => {
    received += 1;
    if (mode === "unknown") return socket.terminate();
    if (mode === "timeout") return;
    if (mode === "malformed") return socket.send("not JSON");
    if (mode === "binary") return socket.send(Buffer.from("{}"));
    socket.send(JSON.stringify({ [mode]: mode === "admitted" ? { seq: 5 } : { reason: "denied", detail: {} } }));
  }));
  http.listen(endpoint); await once(http, "listening");
  t.after(async () => {
    for (const socket of wss.clients) socket.terminate();
    await new Promise((resolve) => wss.close(resolve));
    await new Promise((resolve) => http.close(resolve));
    if (server.listening) await new Promise((resolve) => server.close(resolve));
    fs.rmSync(scratch, { recursive: true });
  });
  const request = { endpoint: `unix://${endpoint}`, interfaceId: "test", octopusHome: scratch,
    manifest: { label: "test.control", payload_path: path.join(scratch, "payload") }, timeoutMs: 300 };
  const { assertAdmitted } = require("../../registry/interface/world_operator_chat/client/values.cjs");
  const send = (input = request) => captureArtifactDelivery(async (deliver) => {
    const verdict = await deliver(input); assertAdmitted(verdict, "test.control"); return { verdict };
  });
  assert.deepEqual((await send()).verdict, { admitted: { seq: 5 } });
  for (const next of ["unknown", "timeout", "malformed", "binary", "refused"]) {
    mode = next;
    assert.equal((await send()).deliveryError.stage, next === "refused" ? "refused" : "outcome-unknown");
  }
  assert.equal(received, 6);
  assert.equal((await send({ ...request, endpoint: `unix://${scratch}/absent` })).deliveryError.stage, "not-sent");
  assert.equal((await send({ ...request, manifest: {} })).deliveryError.stage, "not-sent");
  const handshake = path.join(scratch, "h"); const connections = new Set();
  server.on("connection", (socket) => { connections.add(socket); socket.on("close", () => connections.delete(socket)); });
  server.listen(handshake); await once(server, "listening");
  assert.equal((await send({ ...request, endpoint: `unix://${handshake}` })).deliveryError.stage, "not-sent");
  for (const socket of connections) socket.destroy();
  mode = "admitted";
  const asleep = path.join(scratch, "z"), staged = path.join(scratch, "p");
  const gone = require("node:net").createServer();
  gone.listen(staged); await once(gone, "listening");
  fs.renameSync(staged, asleep); await new Promise((resolve) => gone.close(resolve));
  const woken = require("node:http").createServer();
  const wokenWss = new WebSocketServer({ server: woken });
  wokenWss.on("connection", (socket) => socket.once("message", () => socket.send(JSON.stringify({ admitted: { seq: 6 } }))));
  t.after(async () => {
    for (const socket of wokenWss.clients) socket.terminate();
    await new Promise((resolve) => wokenWss.close(resolve));
    if (woken.listening) await new Promise((resolve) => woken.close(resolve));
  });
  setTimeout(() => woken.listen(staged, () => fs.renameSync(staged, asleep)), 120);
  assert.deepEqual((await send({ ...request, endpoint: `unix://${asleep}`, timeoutMs: 3000 })).verdict, { admitted: { seq: 6 } });
  const afterAdmission = await captureArtifactDelivery(async (deliver) => { await deliver(request); throw new Error("read failed"); });
  assert.equal(afterAdmission.deliveryError.stage, "admitted");
});

test("actual Stop and Resume retain unknown identity, release known rejection, fence old Home callbacks", async () => {
  const file = "app/src/components/chat/useChatWorkspace.ts";
  const actions = declaration(file, "interruptActiveTurn") + declaration(file, "resumeActiveTurn")
    + declaration(file, "clearPendingTurnControl") + declaration(file, "knownMovementSeq") + callbackDeclaration(file, "setPendingTurnControls");
  const create = Function("submitOperatorTurnControl", "options", `${actions}
    let activeTurnControlTarget = {runId:'r',interfaceId:'i'}, activeRun = {...activeTurnControlTarget,writable:true,chatInterfaceId:'i'};
    const resumeAvailable=true,queuedTurnsRef={current:options?.queued??[]},chatDraftKey=x=>x;
    const deliveryContextRef={current:{storageBlockReason:options?.storageBlockReason??'',runtimeReady:options?.runtimeReady??true}},interfaceFailureReason=()=>options?.providerFailureReason??'';
    const activeRunIdRef={get current(){return activeRun.runId}};
    const replacePausedQueueRunKeys=options?.pause??(async()=>{});
    const randomId=()=> 'fixed',formatRuntimeError=e=>e.message,projectedRunsRef={current:[]},messageCacheRef={current:new Map()};
    let pendingTurnControls={},error='',current=true,reads=0;
    const pendingTurnControlsRef={current:{}},setPendingTurnControlsState=value=>pendingTurnControls=value,setError=e=>error=e;
    const deliveryScopeCurrent=()=>current,refreshMessages=async()=>{reads++},refreshRuns=async()=>{reads++};
    return {stop:interruptActiveTurn,resume:resumeActiveTurn,state:()=>({pendingTurnControl:pendingTurnControls[activeRun.runId]??null,error,reads}),
      all:()=>pendingTurnControls,select:runId=>{activeTurnControlTarget={runId,interfaceId:runId};activeRun={...activeTurnControlTarget,writable:true,chatInterfaceId:runId}},
      switchHome:()=>{current=false;setPendingTurnControls({});error=''}};
  `);
  for (const action of ["stop", "resume"]) {
    for (const stage of ["not-sent", "refused", "admitted", "outcome-unknown"]) {
      let calls = 0;
      const hook = create(async () => { calls++; return { deliveryError: { stage, message: "detail" } }; });
      await hook[action]();
      const known = ["not-sent", "refused"].includes(stage);
      assert.equal(Boolean(hook.state().pendingTurnControl), !known);
      if (!known) {
        assert.equal(hook.state().pendingTurnControl.controlId, "control_fixed");
        await hook[action](); assert.equal(calls, action === "stop" ? 2 : 1);
        assert.equal(hook.state().pendingTurnControl.controlId, "control_fixed", "retry preserves the admitted operation identity");
      }
      assert.equal(hook.state().reads, 0);
    }
    const disconnected = create(async () => { throw new Error("IPC disconnected"); });
    await disconnected[action](); assert.ok(disconnected.state().pendingTurnControl);
    const accepted = create(async () => ({ verdict: { admitted: { seq: 1 } } }));
    await accepted[action](); assert.equal(accepted.state().reads, 2); assert.ok(accepted.state().pendingTurnControl);
    let resolve; const switched = create(() => new Promise((done) => { resolve = done; }));
    const pending = switched[action](); switched.switchHome(); resolve({deliveryError:{stage:"not-sent",message:"old"}}); await pending;
    assert.deepEqual(switched.state(), {pendingTurnControl:null,error:"",reads:0});
  }
  for (const action of ["stop", "resume"]) {
    const stopped = create(async () => assert.fail("a stopped runtime must not be asked"), { runtimeReady: false });
    await stopped[action]();
    assert.equal(stopped.state().pendingTurnControl, null);
    assert.match(stopped.state().error, /runtime is not running, so (Stop|Resume) was not sent/);
  }
  let calls=0;
  const multiple=create(async()=>{calls++;return{deliveryError:{stage:"outcome-unknown",message:"lost"}}});
  await multiple.stop(); multiple.select("b"); await multiple.stop(); multiple.select("r"); await multiple.stop();
  assert.equal(calls,3); assert.deepEqual(Object.keys(multiple.all()),["r","b"]);
  let pauseDone;
  const preparing=create(async()=>{assert.fail("old Home must not submit")},{queued:[{runKey:"r"}],pause:()=>new Promise(resolve=>pauseDone=resolve)});
  const resumed=preparing.resume(); await preparing.resume(); preparing.switchHome(); pauseDone(); await resumed;
  assert.deepEqual(preparing.state(),{pendingTurnControl:null,error:"",reads:0});
  const refusedPreparation=create(async()=>assert.fail("preparation failed"),{queued:[{runKey:"r"}],pause:async()=>{throw new Error("IDB denied")}});
  await refusedPreparation.resume(); assert.equal(refusedPreparation.state().pendingTurnControl,null);
  assert.match(refusedPreparation.state().error,/was not sent.*IDB denied/);
});

test("actual main handler serializes pre-dispatch errors as data through preload forwarding", async () => {
  const file = "app/electron/main.cjs", ast = ts.createSourceFile(file, source(file), ts.ScriptTarget.Latest, true);
  let callback;
  function visit(node) {
    if (ts.isCallExpression(node) && node.expression.getText(ast) === "ipcMain.handle"
      && node.arguments[0]?.text === "octopus:submit-operator-turn-control") callback = node.arguments[1].getText(ast);
    ts.forEachChild(node, visit);
  }
  visit(ast); assert.ok(callback);
  let authorized = false, loaded = 0;
  const handler = Function("captureArtifactDelivery", "getClientAccessController", "resolveOctopusHome", "loadOperatorChatClient", "path", `return (${callback})`)(
    captureArtifactDelivery,
    () => ({ requireSession: () => { if (!authorized) throw new Error("expired access"); } }),
    () => "/unused-test-home",
    async () => { loaded++; return { submitOperatorTurnControl: async () => { throw new Error("invalid run"); } }; }, path,
  );
  assert.deepEqual(JSON.parse(JSON.stringify(await handler({}, "token", {}))), { deliveryError: { stage: "not-sent", message: "expired access" } });
  assert.equal(loaded, 0);
  authorized = true;
  assert.deepEqual(await handler({}, "token", {}), { deliveryError: { stage: "not-sent", message: "invalid run" } });
  assert.match(source("app/electron/preload.cjs"), /submitOperatorTurnControl\(request\)\s*\{\s*return invokeAccess\("octopus:submit-operator-turn-control", request\);/);
});
