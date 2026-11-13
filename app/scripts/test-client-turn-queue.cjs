const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const ts = require("typescript");
const vm = require("node:vm");
const root = path.resolve(__dirname, "..");
function load(relative, dependencies = {}) {
  const filename = path.join(root, relative);
  const source = fs.readFileSync(filename, "utf8");
  const compiled = ts.transpileModule(source, { fileName: filename, reportDiagnostics: true,
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX } });
  assert.deepEqual(compiled.diagnostics, [], relative);
  const module = { exports: {} };
  vm.runInThisContext(`(function(require,module,exports){${compiled.outputText}\n})`, { filename })((id) => {
    if (id in dependencies) return dependencies[id];
    if (id.endsWith(".css")) return {};
    return require(id);
  }, module, module.exports);
  return module.exports;
}
function atomicMemory() {
  const records = new Map(); let sequence = Promise.resolve();
  return {
    read: async (key) => { await sequence; return structuredClone(records.get(key) ?? { turns: [], pausedRunKeys: [] }); },
    update: (key, change) => {
      const operation = sequence.then(() => {
        const next = change(structuredClone(records.get(key) ?? { turns: [], pausedRunKeys: [] }));
        records.set(key, structuredClone(next)); return structuredClone(next);
      });
      sequence = operation.catch(() => {}); return operation;
    },
  };
}
function workspaceActions(names, bindings) {
  const source = fs.readFileSync(path.join(root, "src/components/chat/useChatWorkspace.ts"), "utf8");
  const tree = ts.createSourceFile("workspace.ts", source, ts.ScriptTarget.Latest, true);
  const functions = [];
  function visit(node) {
    if (ts.isFunctionDeclaration(node) && names.includes(node.name?.text)) functions.push(node.getText(tree));
    else ts.forEachChild(node, visit);
  }
  visit(tree);
  assert.equal(functions.length, names.length);
  const compiled = ts.transpileModule(functions.join("\n"), { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText;
  const scope = { DEFAULT_TASK_ACCESS_MODE: taskAccess.DEFAULT_TASK_ACCESS_MODE, ...bindings };
  return vm.compileFunction(`${compiled}\nreturn { ${names.join(",")} };`, Object.keys(scope))(...Object.values(scope));
}
const taskAccess = load("src/components/chat/taskAccess.ts");
const { ClientTurnQueue, indexedQueuePersistence, queueNamespace, queueHead, queueDispatchCandidates, queueSubmissionFailureStatus, queueTurnReady, visibleQueueTurns, pendingQueueSteering } = load("src/components/chat/clientTurnQueue.ts", { "./chatTurnControl": load("src/components/chat/chatTurnControl.ts") });
const { operatorChatSubmitFailure, unwrapOperatorChatSubmitResult } = require("../electron/operatorChatSubmitResult.cjs");
for (const submissionStarted of [false, true]) {
  const response = operatorChatSubmitFailure({ code: "operator_chat_client_contract_mismatch" }, { submissionStarted });
  assert.throws(() => unwrapOperatorChatSubmitResult(response), error => {
    assert.equal(queueSubmissionFailureStatus(error, true), submissionStarted ? "unknown" : "failed");
    assert.equal(error.code, "chat_package_repair_required");
    return true;
  });
}
assert.equal(operatorChatSubmitFailure(Object.assign(new Error("Configuration refused"), { deliveryState: "not_sent" })).error.deliveryState, "not_sent");
assert.equal(queueSubmissionFailureStatus(new Error("IPC connection lost"), true), "unknown");
assert.equal(queueSubmissionFailureStatus(new Error("Local validation failed"), false), "failed");
assert.throws(() => unwrapOperatorChatSubmitResult({ error: { deliveryState: "not_sent" } }), error => {
  assert.equal(queueSubmissionFailureStatus(error, true), "unknown");
  return true;
});
const item = (id, runKey = "run-a") => ({ id, messageId: `msg_${id}`, runKey, text: id, attachments: [],
  hadAttachments: false, eeId: "ee", workerInterfaceId: "worker", providerInterfaceId: "provider", providerModel: "model", reasoningEffort: "high",
  endpoint: "ws://localhost:8000", createdAtMs: id === "first" ? 1 : 2, status: "queued" });
const { queuedSubmission } = workspaceActions(["queuedSubmission"], { chatDraftKey: (id) => id || "new" });
for (const [hadAttachments, attachments, recoverPreparedMessage] of [
  [false, [], false], [false, [{ id: "retained-file" }], false],
  [true, [], true], [true, [{ id: "retained-file" }], false],
]) {
  const queued = { ...item("recovery"), clientSurfaceRole: "task", hadAttachments, attachments };
  const submission = queuedSubmission(queued);
  assert.equal(submission.recoverPreparedMessage, recoverPreparedMessage);
  assert.equal(submission.messageId, queued.messageId);
  assert.equal(submission.queueId, queued.id);
  assert.equal(submission.runId, queued.runKey);
  assert.equal(submission.attachments, attachments);
  assert.equal(submission.accessMode, "full_access");
}
(async () => {
  const editedMessage = { id: "latest", runId: "run-a", role: "user", eventKind: "message", text: "original" };
  const { latestSteerableUserMessageId } = workspaceActions(["latestSteerableUserMessageId"], {});
  assert.equal(latestSteerableUserMessageId([editedMessage, { ...editedMessage, id: "steer", eventKind: "steering" }], "run-a", true), "latest");
  for (const [patch, allowed] of [[{}, false], [{ pending: true }, true], [{ error: true }, true], [{ runId: "other" }, true], [{ eventKind: "control" }, true]]) {
    assert.equal(latestSteerableUserMessageId([{ ...editedMessage, ...patch }], "run-a", allowed), "");
  }
  const editTarget = { current: "latest" }, editCalls = [], editErrors = [];
  const { steerUserMessage } = workspaceActions(["steerUserMessage"], { canSteer: true, steerableMessageIdRef: editTarget,
    activeRunIdRef: { current: "run-a" }, chatDraftKey: id => id, setError: value => editErrors.push(value),
    submitSteeringMessage: async input => { editCalls.push(input); return true; } });
  assert.equal(await steerUserMessage(editedMessage, " revised "), true);
  assert.deepEqual(editCalls[0], { attachments: [], fromComposer: false, editedMessageId: "latest", runKey: "run-a", text: "revised" });
  for (const target of ["", "next-turn"]) { editTarget.current = target; assert.equal(await steerUserMessage(editedMessage, "keep my draft"), false); }
  assert.equal(editCalls.length, 1); assert.equal(editErrors.length, 2);
  const settledEdits = [];
  editTarget.current = "latest";
  const { submitSteeringMessage: raceEdit } = workspaceActions(["submitSteeringMessage"], {
    deliveryContextRef: { current: { storageBlockReason: "" } }, interfaceFailureReason: () => "", readDraft: () => undefined,
    activeRun: { runId: "run-a", turnOpen: true, chatInterfaceId: "chat" }, queueOwner: { active: true, id: "owner" },
    queueReady: true, enabled: true, runtimeReady: true, canSteer: true, chatDraftKey: id => id,
    activeRunIdRef: { current: "run-a" }, steeringComposerRef: { current: new Set() }, steerableMessageIdRef: editTarget,
    randomId: () => "edit", setSteeringComposerPendingRunKeys() {}, replaceQueuedTurns: async () => {},
    queueStore: { claim: async () => ({ text: "keep my draft", attachments: [] }), settle: async (...args) => settledEdits.push(args) },
    reasoningEffort: "high", endpoint: "local", fileToBridgeAttachment() {}, deliveryStillAllowed: () => true, deliveryScopeCurrent: () => true,
    loadChatRunStatuses: async () => { editTarget.current = "next-turn"; return [{ runId: "run-a", turnOpen: true }]; },
    submitOperatorSteer: () => assert.fail("an edit must not steer a later turn"), setError: value => assert.fail(value),
  });
  assert.equal(await raceEdit({ attachments: [], fromComposer: false, editedMessageId: "latest", runKey: "run-a", text: "keep my draft" }), false);
  assert.equal(settledEdits[0][2], "failed");
  const submissions = [];
  const busyDraft = workspaceActions(["submitDraft"], {
    deliveryContextRef: { current: { storageBlockReason: "" } }, interfaceFailureReason: () => "", resolvedProviderInterfaceId: "provider",
    runtimeReady: true, queueReady: true, editingQueuedTurn: false, replyOpen: true,
    queuedTurnsRef: { current: [] }, activeRunIdRef: { current: "run-a" }, chatDraftKey: (id) => id,
    text: "follow-up", attachments: [{ id: "file", name: "proof.txt" }], draftMessageId: "", draftMessageHadAttachments: false,
    randomId: () => "stable", canQueue: true,
    enqueueCurrentTurn: async (body) => submissions.push(["queue", body]),
    submitSteeringMessage: async (input) => submissions.push(["steer", input.text]),
    setError: (message) => assert.fail(message),
  }).submitDraft;
  await busyDraft(undefined, undefined, "send", () => {});
  assert.deepEqual(submissions, [["queue", "follow-up"]]);
  await busyDraft(undefined, "explicit steering", "steer", () => {});
  assert.deepEqual(submissions.at(-1), ["steer", "explicit steering"]);
  const controlApi = load("src/components/chat/chatTurnControl.ts");
  const controlRun = { runId: "run-a", chatInterfaceId: "client-a", providerBoundInterfaceId: "provider-a" };
  const receipt = { runId: "run-a", interfaceId: "client-a", direction: "out", artifactId: "receipt", movementSeq: 12,
    eventKind: "turn_control", eventData: { control_id: "stop-a", action: "stop", status: "failed",
      _movement: { from: { Interface: "provider-a" }, to: { Interface: "client-a" } } } };
  assert.equal(controlApi.operatorTurnControlReceipt(controlRun, [receipt], { controlId: "stop-a", action: "stop" }), receipt);
  assert.equal(controlApi.operatorTurnControlReceipt(controlRun, [receipt], { controlId: "other", action: "stop" }), undefined);
  assert.equal(controlApi.operatorTurnControlReceipt({ ...controlRun, providerBoundInterfaceId: "other" }, [receipt], { controlId: "stop-a", action: "stop" }), undefined);
  const db = atomicMemory();
  const key = queueNamespace("lab", "/exact/Home");
  const openings = [], persisted = { turns: [item("retained")], pausedRunKeys: [] };
  const queuePersistence = indexedQueuePersistence({ open: () => { const request = {}; openings.push(request); return request; } });
  const blocked = queuePersistence.read(key); openings[0].onblocked();
  await assert.rejects(blocked, /blocked by another client window/);
  let staleClosed = false;
  openings[0].result = { close: () => { staleClosed = true; } }; openings[0].onsuccess();
  assert.equal(staleClosed, true);
  const recovered = queuePersistence.read(key); assert.equal(openings.length, 2);
  openings[0].error = new Error("old opening failed late"); openings[0].onerror();
  const concurrent = queuePersistence.read(key); assert.equal(openings.length, 2);
  const connection = { close() {}, transaction() {
    const request = { result: persisted }, transaction = { objectStore: () => ({ get: () => request }) };
    queueMicrotask(() => { request.onsuccess(); transaction.oncomplete(); }); return transaction;
  } };
  openings[1].result = connection; openings[1].onsuccess();
  assert.deepEqual(await recovered, persisted);
  assert.deepEqual(await concurrent, persisted);
  connection.onclose();
  const reopened = queuePersistence.read(key); assert.equal(openings.length, 3);
  openings[2].error = new Error("storage denied"); openings[2].onerror();
  await assert.rejects(reopened, /storage denied/);
  assert.throws(() => queueNamespace("lab", ""), /Home/);
  assert.notEqual(key, queueNamespace("live", "/exact/Home"));
  assert.notEqual(key, queueNamespace("lab", "/other/Home"));
  const a = new ClientTurnQueue(key, db), b = new ClientTurnQueue(key, db);
  await a.change(() => ({ turns: [item("first"), item("second"), item("other", "run-b")], pausedRunKeys: [] }));
  // Reordering survives a new controller, preserving the other run and full bytes.
  await a.change((state) => ({ ...state, turns: [state.turns[1], { ...state.turns[0], attachments: [{ id: "file", file: new Blob(["exact bytes"]), name: "proof.txt" }] }, state.turns[2]] }));
  await b.refresh();
  assert.equal(queueHead(b.snapshot, "run-a").id, "second");
  assert.equal(await b.snapshot.turns[1].attachments[0].file.text(), "exact bytes");
  assert.equal(queueHead(b.snapshot, "run-b").id, "other");
  assert.equal((await new ClientTurnQueue(queueNamespace("lab", "/other/Home"), db).getSnapshot()).turns.length, 0);
  // Independent windows race the same head. Exactly one acquires delivery.
  const claims = await Promise.all([a.claim("second", "owner-a", false), b.claim("second", "owner-b", false)]);
  assert.equal(claims.filter(Boolean).length, 1);
  const owner = claims[0] ? "owner-a" : "owner-b";
  await a.refresh();
  assert.equal(a.snapshot.turns[0].status, "sending");
  assert.equal(queueHead(a.snapshot, "run-a"), null);
  assert.equal(await b.claim("first", "owner-b", false), null);
  // Unmount/crash cannot turn sending into queued. Late acknowledgement may settle that exact owner.
  await a.retire(owner);
  const remount = new ClientTurnQueue(key, db); await remount.refresh();
  assert.equal(remount.snapshot.turns[0].status, "unknown");
  assert.equal(await remount.claim("second", "new-owner", true), null);
  await a.settle("second", "wrong-owner", "accepted", undefined, 5);
  assert.equal(a.snapshot.turns[0].status, "unknown");
  await a.settle("second", owner, "accepted", undefined, 5);
  assert.equal(a.snapshot.turns[0].status, "accepted");
  await a.retire(owner);
  assert.equal(a.snapshot.turns[0].status, "accepted");
  assert.equal(queueHead(a.snapshot, "run-a"), null);
  // A failed or editing head blocks subsequent work, including after reload.
  for (const status of ["failed", "editing", "unknown"]) {
    await a.change((state) => ({ ...state, turns: state.turns.map((turn, index) => index ? turn : { ...turn, status }) }));
    await b.refresh(); assert.equal(queueHead(b.snapshot, "run-a"), null);
  }
  // Paused, manual and immediate-steer records never become automatic follow-ups.
  for (const intent of ["steer", "turn"]) {
    await a.change((state) => ({ ...state, turns: [{ ...item("manual"), intent }] }));
    assert.equal(queueHead(a.snapshot, "run-a"), null);
  }
  await a.change(() => ({ turns: [item("first")], pausedRunKeys: ["run-a"] }));
  assert.equal(await a.claim("first", "owner", false), null);
  assert.equal((await a.claim("first", "owner", true)).id, "first");
  // The live-generation query must not overwrite a claim acquired after that query's snapshot.
  await a.change(() => ({ turns: [{ ...item("late"), owner: "late-owner", status: "sending" }], pausedRunKeys: [] }));
  await a.recoverOwners(new Set(), new Set(["old-owner"]));
  assert.equal(a.snapshot.turns[0].status, "sending");
  await a.recoverOwners(new Set(), new Set(["late-owner"]));
  assert.equal(a.snapshot.turns[0].status, "unknown");
  const failing = new ClientTurnQueue(key, { read: db.read, update: async () => { throw new Error("disk full"); } });
  await assert.rejects(() => failing.claim("late", "owner", false), /disk full/);
  assert.equal(failing.snapshot.ready, false);

  // A task finishes after its composer has been replaced by another task's view.
  await a.change(() => ({ turns: [item("background", "run-a")], pausedRunKeys: [] }));
  const projected = [{ runId: "run-a", writable: true, turnOpen: true, turnState: "working" }, { runId: "run-b", writable: true, turnOpen: false, turnState: "idle" }];
  assert.equal(queueDispatchCandidates(a.snapshot, projected).length, 0);
  projected[0] = { ...projected[0], turnOpen: false, turnState: "idle" };
  await b.refresh();
  let deliveredRuns = [];
  await Promise.all([a, b].map(async (view) => {
    for (const turn of queueDispatchCandidates(view.snapshot, projected)) {
      if (await view.claim(turn.id, "background-owner", false)) deliveredRuns.push(turn.runKey);
    }
  }));
  assert.deepEqual(deliveredRuns, ["run-a"]);
  const pendingTurn = { ...item("ready"), queuedAfterSeq: 10 };
  const idle = { runId: "run-a", turnOpen: false, turnState: "idle", lastMovementSeq: 12, updatedAtMs: 1 };
  const final = { id: "reply", runId: "run-a", direction: "out", role: "assistant", eventKind: "message", terminal: true, movementSeq: 12, createdAtMs: 1 };
  assert.equal(queueTurnReady(pendingTurn, idle, []), false);
  assert.equal(queueTurnReady(pendingTurn, idle, [{ ...final, movementSeq: 9 }]), false);
  assert.equal(queueTurnReady(pendingTurn, idle, [{ ...final, eventKind: "rate_limits" }]), false);
  assert.equal(queueTurnReady(pendingTurn, idle, [final]), true);
  assert.equal(queueTurnReady(pendingTurn, { ...idle, turnOpen: true }, [final]), false);
  assert.equal(queueTurnReady(pendingTurn, { ...idle, runId: "run-b" }, [final]), false);
  const interrupted = { ...final, eventKind: "interrupted_result" };
  assert.equal(queueTurnReady(pendingTurn, idle, [interrupted]), false);
  assert.equal(queueTurnReady({ ...pendingTurn, resumeAfterInterrupt: true }, idle, [interrupted]), true);
  await a.change(() => ({ turns: [pendingTurn, item("next")], pausedRunKeys: [] }));
  await a.claim("ready", "sender", false);
  await a.settle("ready", "sender", "queued");
  assert.equal(queueHead(a.snapshot, "run-a").id, "ready");
  assert.equal(queueHead({ turns: [{ ...pendingTurn, intent: "turn" }], pausedRunKeys: [] }, "run-a").id, "ready");
  await a.claim("ready", "sender", false);
  await a.settle("ready", "sender", "accepted", undefined, 14);
  assert.deepEqual(visibleQueueTurns(a.snapshot.turns, "run-a").map((turn) => turn.id), ["next"]);
  assert.equal(queueHead(a.snapshot, "run-a"), null);
  const steering = { ...item("steering"), intent: "steer", status: "sending", owner: "sender" };
  assert.equal(visibleQueueTurns([steering], "run-a").length, 0);
  assert.equal(pendingQueueSteering([steering], "run-a")[0].eventData.steering_status, "pending");
  assert.equal(pendingQueueSteering([{ ...steering, status: "accepted" }], "run-a")[0].eventData.steering_status, "submitted");
  assert.equal(pendingQueueSteering([steering], "run-b").length, 0);
  assert.equal(pendingQueueSteering([{ ...steering, status: "unknown" }], "run-a").length, 0);
  assert.equal(visibleQueueTurns([{ ...steering, status: "unknown" }], "run-a").length, 1);
  await a.change(() => ({ turns: [{ ...steering, status: "queued" }], pausedRunKeys: [] }));
  assert.equal(visibleQueueTurns(a.snapshot.turns, "run-a").length, 0);
  await a.retire("sender");
  assert.equal(visibleQueueTurns(a.snapshot.turns, "run-a")[0].status, "failed");
  await a.change(() => ({ turns: [{ ...steering, status: "queued" }], pausedRunKeys: [] }));
  await a.recoverOwners(new Set(), new Set(["sender"]));
  assert.equal(a.snapshot.turns[0].status, "failed");
  assert.equal(visibleQueueTurns([{ ...steering, status: "queued", owner: undefined }], "run-a").length, 1);
  let records, dispatched, restoredDraft;
  const activeRunIdRef = { current: "run-a" };
  const actionBindings = {
    deliveryContextRef: { current: { storageBlockReason: "" } }, interfaceFailureReason: () => "",
    activeRunIdRef, chatDraftKey: (id) => id || "new", queueOwner: { id: "sender", active: true }, canSendQueued: true,
    deliveryScopeCurrent: () => true, queuedTurnsRef: { get current() { return records; } },
    replaceQueuedTurns: async (update) => { records = update(records); return records; },
    submitSteeringMessage: async (input) => { dispatched = ["steer", input]; return true; },
    executeTurnSequence: async (input) => { dispatched = ["turn", input]; },
    formatRuntimeError: (failure) => failure.message,
    queueAttemptedProjection: new Map(),
    queueStore: { change: async (update) => { const result = update({ turns: records, pausedRunKeys: ["run-a"] }); records = result.turns; return result; } },
    writeDraft: (runId, draft) => { restoredDraft = [runId, draft]; }, deleteDraft: noopForActions,
    applyDraft: noopForActions, setDraftMessageId: noopForActions, setText: noopForActions, setAttachments: noopForActions,
  };
  function noopForActions() {}
  let finishTurn, finishQueue, queuedWhileReplying = 0;
  const pendingReply = new Promise((resolve) => { finishTurn = resolve; });
  const pendingQueueSave = new Promise((resolve) => { finishQueue = resolve; });
  const composerActionRef = { current: new Set() }; records = [];
  const composerActions = workspaceActions(["submit", "beginSubmission"], { ...actionBindings, composerActionRef,
    consumeDraft: noopForActions, resetInputHistory: noopForActions, setError: assert.fail, executeTurnSequence: () => pendingReply,
    submitDraft: async (_event, _text, mode, release) => mode === "queue"
      ? (++queuedWhileReplying, await pendingQueueSave) : composerActions.beginSubmission({ ...item("base"), runId: "run-a" }, release),
  });
  const replying = composerActions.submit(); await Promise.resolve();
  const queueing = composerActions.submit(undefined, undefined, "queue"); assert.equal(queuedWhileReplying, 1);
  finishTurn(); await replying; assert.equal(composerActionRef.current.has("run-a"), true);
  finishQueue(); await queueing; assert.equal(composerActionRef.current.size, 0);
  for (const replyOpen of [true, false]) {
    records = [{ ...pendingTurn, status: "failed", owner: "previous-owner", intent: replyOpen ? "turn" : "steer", clientSurfaceRole: "task" }];
    dispatched = null;
    const actions = workspaceActions(["steerQueuedTurn", "retryQueuedTurn", "queuedSubmission"], { ...actionBindings, replyOpen });
    await actions.retryQueuedTurn("ready");
    assert.equal(dispatched[0], replyOpen ? "steer" : "turn");
    assert.equal(dispatched[1].messageId, pendingTurn.messageId);
    assert.equal(dispatched[1].text, pendingTurn.text);
    records = [{ ...pendingTurn, status: "unknown" }]; dispatched = null;
    await actions.retryQueuedTurn("ready");
    assert.equal(dispatched, null); assert.equal(records[0].status, "unknown");
  }
  records = [{ ...pendingTurn, status: "failed", intent: "steer" }];
  const idleActions = workspaceActions(["steerQueuedTurn", "retryQueuedTurn", "queuedSubmission"], { ...actionBindings, replyOpen: false });
  await assert.rejects(() => idleActions.retryQueuedTurn("ready"), /no exact client surface identity/);
  assert.equal(records[0].status, "failed");
  records = [{ ...pendingTurn, status: "editing", editOwner: "sender", beforeEdit: { status: "failed", error: "Refused exactly" }, draftBeforeEdit: { text: "original composer" } }];
  const editActions = workspaceActions(["cancelQueuedEdit", "restoreQueueDraft", "resumeQueuedTurns"], actionBindings);
  await editActions.cancelQueuedEdit();
  assert.equal(records[0].status, "failed"); assert.equal(records[0].error, "Refused exactly");
  assert.deepEqual(restoredDraft, ["run-a", { text: "original composer" }]);
  records = [{ ...item("accepted"), status: "accepted" }, pendingTurn];
  await editActions.resumeQueuedTurns();
  assert.equal(records[1].resumeAfterInterrupt, true);
  assert.equal(queueTurnReady(records[1], idle, [interrupted]), true);
  let undo, restoredEdit;
  const queuedActions = workspaceActions(["reorderQueuedTurns", "deleteQueuedTurn", "editQueuedTurn"], {
    ...actionBindings, appToast: { info: (_title, _duration, options) => { undo = options.action.onSelect; } }, setError: noopForActions,
    saveCurrentDraft: noopForActions, readDraft: () => ({ messageId: "msg_first", text: "edited", attachments: [{ id: "new-attachment" }] }),
    composerDraftRef: { current: { runId: "run-a", text: "edited", attachments: [] } },
    sameAttachmentSelection: (left, right) => left === right, applyDraft: (draft) => { restoredEdit = draft; }, resetInputHistory: noopForActions,
  });
  records = [item("first"), item("other", "run-b"), item("second")];
  await queuedActions.reorderQueuedTurns("second", "first");
  assert.deepEqual(records.map((record) => record.id), ["second", "other", "first"]);
  await queuedActions.deleteQueuedTurn("second");
  records.push(item("later")); undo();
  assert.deepEqual(records.filter((record) => record.runKey === "run-a").map((record) => record.id), ["second", "first", "later"]);
  records = [{ ...item("first"), status: "editing", editOwner: "sender", beforeEdit: { status: "failed", error: "original refusal" } }];
  await queuedActions.editQueuedTurn("first");
  assert.deepEqual(restoredEdit.attachments, [{ id: "new-attachment" }]);
  assert.equal(records[0].beforeEdit.error, "original refusal");
  const { deliveryScopeCurrent: _scope, ...scopeBindings } = actionBindings;
  for (const action of ["editQueuedTurn", "cancelQueuedEdit"]) {
    for (const drift of ["home", "retired", "generation", "replacement-edit"]) {
      if (action === "cancelQueuedEdit" && drift === "replacement-edit") continue;
      const owner = { id: "old-owner", active: true };
      const context = { current: { accessProfile: "lab", runtimeHome: "/Home-a" } };
      const sameDraft = { runId: "run-a", text: "identical draft", attachments: [] };
      const original = { ...item("scope-race"), ...(action === "cancelQueuedEdit" ? {
        status: "editing", editOwner: owner.id, beforeEdit: { status: "queued" }, draftBeforeEdit: { text: "prior draft" },
      } : {}) };
      records = [original];
      let release, awaited = false, draftEffects = 0;
      const fenceActions = workspaceActions(["editQueuedTurn", "cancelQueuedEdit", "restoreQueueDraft", "deliveryScopeCurrent"], {
        ...scopeBindings, accessProfile: "lab", runtimeHome: "/Home-a", deliveryContextRef: context, queueOwner: owner,
        composerDraftRef: { current: sameDraft }, sameAttachmentSelection: (left, right) => left === right,
        saveCurrentDraft: noopForActions, readDraft: () => ({ text: sameDraft.text, attachments: sameDraft.attachments }),
        applyDraft: () => draftEffects++, writeDraft: () => draftEffects++, deleteDraft: () => draftEffects++, resetInputHistory: () => draftEffects++,
        replaceQueuedTurns: async (update) => {
          records = update(records);
          if (!awaited) { awaited = true; await new Promise((resolve) => { release = resolve; }); }
          return records;
        },
      });
      const pending = fenceActions[action]("scope-race");
      assert.equal(typeof release, "function");
      if (drift === "home") context.current = { ...context.current, runtimeHome: "/Home-b" };
      if (drift === "retired") owner.active = false;
      if (drift === "generation" || drift === "replacement-edit") owner.id = "new-owner";
      if (drift === "replacement-edit") records = [{ ...records[0], editOwner: "new-owner", text: "new owner's edit" }];
      release(); await pending;
      assert.equal(draftEffects, 0, `${action}/${drift} must not restore or apply a stale draft`);
      if (drift === "replacement-edit") assert.equal(records[0].text, "new owner's edit");
      else if (action === "editQueuedTurn") assert.deepEqual(records, [original], "rollback is confined to the old exact edit owner");
    }
  }
  const reserved = { ...pendingTurn, owner: "other-live-owner", intent: "turn" };
  records = [reserved]; dispatched = null;
  const reservedActions = workspaceActions(["steerQueuedTurn", "queuedSubmission"], { ...actionBindings, replyOpen: true });
  await reservedActions.steerQueuedTurn(reserved.id);
  assert.deepEqual(records, [reserved]); assert.equal(dispatched, null);
  await a.change(() => ({ turns: [reserved], pausedRunKeys: [] }));
  assert.equal(await b.claim(reserved.id, "sender", true), null);
  assert.equal(await b.claim(reserved.id, "sender", false, true), null);
  assert.equal((await a.claim(reserved.id, "other-live-owner", false, true)).owner, "other-live-owner");
  // Completion callbacks may arrive out of order; neither a stale read nor a stale write may repaint newer data.
  let releaseWrite, releaseRead;
  const storage = atomicMemory();
  const delayed = new ClientTurnQueue(key, { read: async (key) => { const state = await storage.read(key); await new Promise((resolve) => { releaseRead = resolve; }); return state; },
    update: async (key, change) => { const state = await storage.update(key, change); if (state.revision === 1) await new Promise((resolve) => { releaseWrite = resolve; }); return state; } });
  const oldRead = delayed.refresh(); await new Promise(setImmediate);
  const oldWrite = delayed.change(() => ({ turns: [item("old")], pausedRunKeys: [] })); await new Promise(setImmediate);
  await delayed.change(() => ({ turns: [item("new")], pausedRunKeys: [] }));
  releaseWrite(); releaseRead(); await Promise.all([oldWrite, oldRead]);
  assert.equal(delayed.snapshot.turns[0].id, "new");

  const React = require("react");
  const { JSDOM } = require("jsdom");
  const dom = new JSDOM('<!doctype html><div id="root"></div>', { url: "http://localhost/" });
  global.window = dom.window; global.document = dom.window.document; global.navigator = dom.window.navigator;
  global.localStorage = dom.window.localStorage; global.CustomEvent = dom.window.CustomEvent;
  global.Element = dom.window.Element; global.HTMLElement = dom.window.HTMLElement; global.SVGElement = dom.window.SVGElement;
  global.requestAnimationFrame = (callback) => setTimeout(() => callback(performance.now()), 16);
  global.cancelAnimationFrame = clearTimeout;
  window.scrollTo = () => {};
  global.IS_REACT_ACT_ENVIRONMENT = true;
  const { createRoot } = require("react-dom/client");
  const { act } = React;
  const noop = () => {};
  const host = createRoot(document.getElementById("root"));
  const empty = () => null;
  {
    const originalChannel = global.BroadcastChannel, originalDatabase = global.indexedDB;
    const ownerStorage = atomicMemory(); let failRecovery = true, lockName, ownerQueue, queueView;
    global.BroadcastChannel = class { postMessage() {} };
    global.indexedDB = {};
    navigator.locks = { request: async (name, callback) => { lockName = name; await callback(); }, query: async () => ({ held: [{ name: lockName }] }) };
    const queueApi = load("src/components/chat/useChatTurnQueue.ts", {
      "./clientTurnQueue": { ClientTurnQueue: class extends ClientTurnQueue { constructor(key) {
        super(key, { ...ownerStorage, update: async (key, change) => { if (failRecovery) { failRecovery = false; throw new Error("storage interrupted"); } return ownerStorage.update(key, change); } }); ownerQueue = this;
      } }, queueNamespace, queueHead, indexedQueuePersistence: () => ({}) },
    });
    function QueueProbe() { queueView = queueApi.useChatTurnQueue("live", "/queue-recovery-fixture"); return null; }
    try {
      await act(async () => host.render(React.createElement(QueueProbe)));
      const owner = queueView.queueOwner.id;
      assert.equal(queueView.queueReady, false); assert.match(queueView.queueError, /storage interrupted/);
      await act(async () => { await new Promise(resolve => setTimeout(resolve, 1_050)); });
      assert.equal(queueView.queueReady, true); assert.equal(queueView.queueOwner.id, owner);
      await act(async () => ownerQueue.reportError(new Error("later read interrupted")));
      await act(async () => { await new Promise(resolve => setTimeout(resolve, 1_050)); });
      assert.equal(queueView.queueReady, true); assert.deepEqual(queueView.queuedTurns, []);
    } finally {
      await act(async () => host.render(null));
      global.BroadcastChannel = originalChannel; global.indexedDB = originalDatabase; delete navigator.locks;
    }
  }
  Object.assign(global, { Event: window.Event, HTMLInputElement: window.HTMLInputElement, HTMLTextAreaElement: window.HTMLTextAreaElement,
    ResizeObserver: class { observe() {} disconnect() {} } });
  document.hasFocus = () => true; // JSDOM has no window focus; model a focused client for Escape restoration.
  const modalFocus = load("src/design-system/modalFocus.ts");
  const shellTooltip = load("src/components/shell/ShellTooltip.tsx", {
    "../chat/MessageTooltip": load("src/components/chat/MessageTooltip.tsx"),
  });
  const menuComponents = load("src/design-system/components.tsx", { "./OcDropdown": {}, "./modalFocus": modalFocus,
    "../components/IconGlyphs": { Glyph: empty }, "../components/navigation/OctopusSpinner": { OctopusSpinner: empty },
    "../components/shell/ShellTooltip": shellTooltip });
  const dropdown = load("src/design-system/OcDropdown.tsx", { "./components": menuComponents, "./modalFocus": modalFocus });
  const keydown = async (target, key, modifiers = {}) => {
    const event = new window.KeyboardEvent("keydown", { key, bubbles: true, cancelable: true, ...modifiers });
    await act(async () => target.dispatchEvent(event)); return event;
  };
  const markdownDependencies = Object.fromEntries(await Promise.all(["micromark", "micromark-core-commonmark", "remark-parse", "remark-gfm", "remark-math", "unified"].map(async (id) => [id, await import(id)])));
  const markdown = load("src/components/chat/markdownPresentation.ts", markdownDependencies);
  const messageContent = load("src/components/chat/MessageContent.tsx", {
    "../../clientBrowser": load("src/clientBrowser.ts", { "./toasts": { appToast: { danger: noop } } }),
    "react-markdown": await import("react-markdown"), "rehype-katex": await import("rehype-katex"),
    "../../appearance": { useAppearance: () => ({}) }, "../../runtimeFormat": { copyText: noop },
    "../../design-system/components": { OcButton: empty, OcDialog: empty, OcDialogFooter: empty, OcDialogHeader: empty },
    "../IconGlyphs": { Glyph: empty }, "./markdownPresentation": markdown,
    "../shell/ShellTooltip": shellTooltip,
  });
  for (const compactHeadings of [false, true, false]) {
    await act(async () => host.render(React.createElement(messageContent.MessageContent, { text: "# Heading", compactHeadings })));
    assert.equal(Boolean(document.querySelector("h1.octopus-markdown-heading")), !compactHeadings, "compact headings are opt-in and invalidate the memo");
  }
  const openedFiles = [];
  for (const [href, expected] of [["/workspace/report%20one.md", "/workspace/report one.md"],
    ["file:///workspace/report%20one.md", "/workspace/report one.md"], ["report.md", "report.md"]]) {
    const text = `[Report](${href})`;
    for (const owner of ["first", "replacement"]) {
      await act(async () => host.render(React.createElement(messageContent.MessageContent, { text,
        onOpenFile: value => openedFiles.push([owner, value]) })));
      const event = new window.MouseEvent("click", { bubbles: true, cancelable: true });
      await act(async () => document.querySelector("a").dispatchEvent(event));
      assert.equal(event.defaultPrevented, true, "local links never navigate the app document");
      assert.deepEqual(openedFiles.at(-1), [owner, expected], "link uses the current Files owner and decoded path");
    }
  }
  for (const href of ["javascript:alert(1)", "//remote.test/file", "file://remote.test/file", "/report%00.md", "/bad%zz.md"]) {
    await act(async () => host.render(React.createElement(messageContent.MessageContent, { text: `[Blocked](${href})`, onOpenFile: noop })));
    assert.ok(document.querySelector("a") === null);
  }
  await act(async () => host.render(React.createElement(messageContent.MessageContent, { text: "[Report](/workspace/report.md)" })));
  assert.ok(document.querySelector("a") === null, "a surface without a Files owner does not navigate a local link");
  await act(async () => host.render(React.createElement(messageContent.MessageContent, { text: "[Website](https://example.com/)" })));
  const webLinks = [];
  const openWebLink = event => { event.preventDefault(); webLinks.push(event.detail.url); };
  window.addEventListener("octopus:open-web-link", openWebLink);
  const externalClick = new window.MouseEvent("click", { bubbles: true, cancelable: true });
  await act(async () => document.querySelector("a").dispatchEvent(externalClick));
  assert.equal(externalClick.defaultPrevented, true, "web links never navigate the app document");
  assert.deepEqual(webLinks, ["https://example.com/"], "web links open in the client browser");
  assert.equal(document.querySelector('[role="dialog"]'), null, "ordinary links do not prompt for an external browser");
  window.removeEventListener("octopus:open-web-link", openWebLink);
  const realNow = performance.now;
  let streamNow = 1_000;
  Object.defineProperty(performance, "now", { configurable: true, value: () => streamNow });
  try {
    const streamed = (text, streaming) => act(async () => host.render(React.createElement(messageContent.MessageContent, {
      streamKey: "reply", streaming, text, variant: "assistant" })));
    const delayOf = (element) => Number.parseFloat(element.style.getPropertyValue("--fade-delay"));
    const startOf = (element) => 1_000 + delayOf(element);
    await streamed("1. First item\n2. `artifact.*` second\n3.", true);
    const items = [...document.querySelectorAll("li")];
    const ordered = [...document.querySelectorAll("li, .octopus-markdown-fade-in")];
    const starts = ordered.filter((element) => element.tagName !== "LI" || element.style.getPropertyValue("--fade-delay") !== "600000ms").map(startOf);
    assert.deepEqual(starts, [...starts].sort((left, right) => left - right), "streamed list markers, words and inline code reveal in document order");
    assert.equal(startOf(items[1]), startOf(items[1].querySelector("code")), "a list item appears with its first unit, even when that unit is inline code");
    assert.equal(items[2].style.getPropertyValue("--fade-delay"), "600000ms", "an empty streamed list item keeps its marker hidden");
    streamNow = 2_000;
    await streamed("1. First item\n2. `artifact.*` second\n3. Third", true);
    assert.equal(document.querySelectorAll("li")[2], items[2]);
    assert.equal(1_000 + delayOf(items[2]), 2_000 + delayOf(items[2].querySelector(".octopus-markdown-fade-in")), "the held item appears with its first word");
    await streamed("1. First item\n2. `artifact.*` second\n3. Third", false);
    assert.deepEqual([...document.querySelectorAll("li")], items, "completing the reply keeps the streamed markdown mounted");
    assert.ok(document.querySelector("[data-markdown-animated], .octopus-markdown-fade-in") === null);
  } finally {
    Object.defineProperty(performance, "now", { configurable: true, value: realNow });
  }
  const preferencesApi = load("src/clientPreferences.ts"); let loadedPreferences;
  const obsoletePreference = JSON.stringify({ activeRunSubmitBehavior: "queue", sendShortcut: "cmd-always" });
  localStorage.setItem("octopus.client-preferences.v1.test", obsoletePreference);
  function PreferencesProbe() { loadedPreferences = preferencesApi.useClientPreferences(); return null; }
  await act(async () => host.render(React.createElement(preferencesApi.ClientPreferencesProvider, { profile: "lab" }, React.createElement(PreferencesProbe))));
  assert.equal("activeRunSubmitBehavior" in loadedPreferences, false); assert.equal(loadedPreferences.sendShortcut, "cmd-always");
  assert.equal(localStorage.getItem("octopus.client-preferences.v1.test"), obsoletePreference, "reading an obsolete queue default does not migrate storage");
  let reduceQueueMotion = false;
  const queueComponent = load("src/components/chat/QueuedMessageList.tsx", {
    "../../appearance": { useShouldReduceMotion: () => reduceQueueMotion },
    "../IconGlyphs": { Glyph: empty }, "./MessageContent": messageContent,
    "./markdownPresentation": markdown, "./MessageTooltip": { MessageTooltip: ({ children }) => children },
    "../../design-system/components": menuComponents,
    "../../design-system/OcDropdown": dropdown,
  }).QueuedMessageList;
  let queueActions = [];
  const queueProps = { messages: [item("row")], paused: false, sendNowAvailable: true,
    onDelete: (id) => queueActions.push(["delete", id]), onEdit: (id) => queueActions.push(["edit", id]),
    onSendNow: (id) => queueActions.push(["steer", id]), onRetry: (id) => queueActions.push(["retry", id]), onReorder: noop, onResume: noop };
  await act(async () => host.render(React.createElement(queueComponent, queueProps)));
  assert.ok(document.querySelector(".composer-queue-status") === null);
  assert.ok(document.querySelector('[aria-label="Reorder queued message"]') === null);
  await act(async () => document.querySelector('[aria-label="Steer with queued message"]').click());
  await act(async () => document.querySelector('[aria-label="Queued message actions"]').click());
  await act(async () => document.querySelector('[aria-label="Queued message actions"]').click());
  assert.ok(document.querySelector('[role="menu"]') === null, "More toggles shut");
  await keydown(document.querySelector('[aria-label="Queued message actions"]'), "ArrowDown");
  assert.equal(document.activeElement.getAttribute("data-text-value"), "Edit message");
  assert.equal((await keydown(document.activeElement, "Tab")).defaultPrevented, true);
  await keydown(document.activeElement, "Enter");
  assert.deepEqual(queueActions, [["steer", "row"], ["edit", "row"]]);
  await act(async () => host.render(React.createElement(queueComponent, { ...queueProps, messages: [item("row"), item("second")] })));
  assert.equal(document.querySelectorAll('[aria-label="Reorder queued message"]').length, 2);
  await act(async () => host.render(React.createElement(queueComponent, { ...queueProps, messages: [{ ...item("row"), status: "failed", error: "Exact refusal" }], sendNowAvailable: false })));
  assert.equal(document.querySelector('[aria-label="Retry queued message"]').disabled, true);
  assert.match(document.querySelector(".composer-queue-error").textContent, /Exact refusal/);
  await act(async () => host.render(React.createElement(queueComponent, { ...queueProps, paused: true, messages: [{ ...item("row"), status: "unknown" }] })));
  assert.ok(document.querySelector('[aria-label="Retry queued message"]') === null);
  assert.ok(document.querySelector('[data-message-status="unknown"] [aria-label="Queued message actions"]') === null);
  assert.equal(document.querySelector(".composer-queue-paused button").disabled, true);
  await act(async () => document.querySelector('[aria-label="Remove unconfirmed message locally"]').click());
  assert.deepEqual(queueActions.at(-1), ["delete", "row"]);
  await act(async () => host.render(React.createElement(queueComponent, { ...queueProps, messages: [{ ...item("row"), text: "# **Review** `answer`" }] })));
  assert.equal(document.querySelector(".composer-queue-message strong").textContent, "Review");
  assert.equal(document.querySelector(".composer-queue-message code").textContent, "answer");
  assert.ok(document.querySelector(".composer-queue-message h1") === null);
  await act(async () => host.render(React.createElement(queueComponent, { ...queueProps, messages: [{ ...item("row"), text: "```js\nconst answer = 42;\n```" }] })));
  assert.equal(document.querySelector(".composer-queue-message").textContent, "const answer = 42;");
  let releasedPreview;
  const createObjectURL = URL.createObjectURL, revokeObjectURL = URL.revokeObjectURL;
  URL.createObjectURL = () => "blob:queue-test"; URL.revokeObjectURL = (src) => { releasedPreview = src; };
  await act(async () => host.render(React.createElement(queueComponent, { ...queueProps, messages: [{ ...item("row"), text: "", attachments: [{ name: "image.png", type: "image/png", file: new Blob(["bytes"]) }] }] })));
  assert.equal(document.querySelector(".composer-queue-image").getAttribute("src"), "blob:queue-test");
  assert.equal(document.querySelector(".composer-queue-message").textContent, "1 image");
  await act(async () => host.render(React.createElement(queueComponent, { ...queueProps, messages: [{ ...item("row"), text: " ", attachments: [1, 2].map((id) => ({ name: `${id}.png`, type: "image/png" })) }] })));
  assert.equal(document.querySelector(".composer-queue-message").textContent, "2 images");
  await act(async () => host.render(null));
  assert.equal(releasedPreview, "blob:queue-test");
  URL.createObjectURL = createObjectURL; URL.revokeObjectURL = revokeObjectURL;
  const exitRows = [item("departing"), item("last")];
  await act(async () => host.render(React.createElement(queueComponent, { ...queueProps, messages: exitRows })));
  await act(async () => document.querySelector('[aria-label="Queued message actions"]').click());
  assert.ok(document.querySelector('[role="menu"]'));
  await act(async () => host.render(React.createElement(queueComponent, { ...queueProps, messages: exitRows.slice(1) })));
  const departing = document.querySelector('.composer-queue-motion[inert]');
  assert.ok(departing, "a nonlast row remains mounted for its exit");
  assert.ok(document.querySelector('[role="menu"]') === null);
  assert.ok([...departing.querySelectorAll("button")].every((button) => button.disabled));
  const actionCount = queueActions.length;
  await act(async () => departing.querySelector("button").click());
  assert.equal(queueActions.length, actionCount);
  await act(async () => { await new Promise((resolve) => setTimeout(resolve, 280)); });
  assert.ok(document.querySelector('.composer-queue-motion[inert]') === null);
  await act(async () => host.render(React.createElement(queueComponent, { ...queueProps, messages: [] })));
  const exitingRail = document.querySelector('[data-composer-rail-item="exiting"]');
  assert.ok(exitingRail, "last-row removal keeps the outer rail mounted for its own exit");
  assert.ok(exitingRail.hasAttribute("inert"));
  assert.ok(exitingRail.querySelector('.composer-queue-motion[inert]'), "the last row participates in the propagated exit");
  assert.ok([...exitingRail.querySelectorAll("button")].every((button) => button.disabled));
  await act(async () => { await new Promise((resolve) => setTimeout(resolve, 420)); });
  assert.ok(document.querySelector('.composer-queue-surface') === null);
  reduceQueueMotion = true;
  await act(async () => host.render(React.createElement(queueComponent, queueProps)));
  assert.equal(document.querySelector('.composer-queue-surface').getAttribute("data-reduced-motion"), "true");
  await act(async () => host.render(React.createElement(queueComponent, { ...queueProps, messages: [] })));
  await act(async () => { await new Promise((resolve) => setTimeout(resolve, 60)); });
  assert.ok(document.querySelector('.composer-queue-surface') === null, "reduced motion does not wait for full rail or row duration");
  reduceQueueMotion = false;
  const commands = load("src/clientActions.ts");
  let sendShortcut = "enter";
  const composer = load("src/components/chat/ChatComposer.tsx", {
    "./taskAccess": taskAccess,
    "./DictationIcon": load("src/components/chat/DictationIcon.tsx"),
    "../IconGlyphs": { Glyph: empty }, "./ProviderModelPicker": { ProviderModelPicker: empty },
    "./ChatAttachmentCard": { ChatAttachmentStrip: empty },
    "../icon-glyphs/geometric-primitives": load("src/components/icon-glyphs/geometric-primitives.tsx"),
    "./useLocalDictation": { useLocalDictation: () => ({}) }, "../../toasts": { appToast: {} },
    "./useComposerSuggestion": load("src/components/chat/useComposerSuggestion.ts"),
    "../../design-system/components": { ...menuComponents, OcDialog: empty },
    "../../design-system/OcDropdown": dropdown,
    "../../clientActions": commands,
    "../../clientPreferences": { useClientPreferences: () => ({ sendShortcut }) },
    "./ContextUsageIndicator": { ContextUsageIndicator: empty },
    "./ProjectedFilePicker": { ProjectedFilePicker: empty }, "./ComposerUtilityBar": { ComposerUtilityBar: empty },
    "../shell/ShellTooltip": shellTooltip,
    "./RichComposerEditor": { RichComposerEditor: ({ onKeyDown }) => React.createElement("textarea", { onKeyDown: (event) => onKeyDown(event.nativeEvent, event.currentTarget) }) },
  }).ChatComposer;
  let sends = 0, steers = 0, stops = 0, queued = 0;
  const props = { accessProfile: "lab", text: "steer this", attachments: [], providerInterfaces: [], providerInterfaceId: "p", providerModel: "m", reasoningEffort: "high",
    submissionMode: "queue", canSubmit: true, canQueue: true, canSteer: true, onSteer: () => steers++, onQueue: () => queued++, onSubmit: () => sends++, onInterrupt: () => stops++, turnOpen: true,
    onKeyDown: noop, onTextChange: noop, onFilesPicked: noop, onRemoveAttachment: noop, onProviderInterfaceChange: noop, onProviderModelChange: noop, onReasoningEffortChange: noop, fileInputRef: { current: null } };
  await act(async () => host.render(React.createElement(composer, props)));
  const send = document.querySelector('button[aria-label="Send"]'); assert.ok(send); assert.ok(document.querySelector('button[aria-label="Stop"]'));
  await act(async () => send.click()); assert.equal(sends, 1); assert.equal(stops, 0); assert.equal(queued, 0);
  await act(async () => send.dispatchEvent(new window.MouseEvent("contextmenu", { bubbles: true, cancelable: true })));
  await keydown(document.activeElement, "Home");
  assert.equal(document.activeElement.getAttribute("data-text-value"), "Send now");
  await keydown(document.activeElement, "q");
  assert.equal(document.activeElement.getAttribute("data-text-value"), "Queue message");
  await keydown(document.activeElement, "Enter"); assert.equal(queued, 1); assert.equal(sends, 1);
  await act(async () => host.render(React.createElement(composer, { ...props, canSubmit: false, canSteer: false, isSubmitting: true })));
  const pendingSend = document.querySelector('button[aria-label="Send"]');
  assert.equal(pendingSend.disabled, false); assert.equal(pendingSend.getAttribute("aria-disabled"), "true"); assert.equal(pendingSend.getAttribute("aria-busy"), "true");
  pendingSend.focus(); assert.equal(document.activeElement, pendingSend);
  await act(async () => pendingSend.click()); assert.equal(sends, 1);
  await act(async () => pendingSend.dispatchEvent(new window.KeyboardEvent("keydown", { key: "F10", shiftKey: true, bubbles: true, cancelable: true })));
  assert.ok(document.querySelector('[role="menu"]'));
  assert.equal(document.activeElement.getAttribute("data-text-value"), "Queue message", "keyboard entry skips disabled Send now");
  await keydown(document.activeElement, "Escape");
  assert.ok(document.querySelector('[role="menu"]') === null); assert.equal(document.activeElement, pendingSend);
  await act(async () => pendingSend.dispatchEvent(new window.MouseEvent("contextmenu", { bubbles: true, cancelable: true })));
  const pendingQueueButton = document.querySelector('[data-text-value="Queue message"]');
  assert.equal(pendingQueueButton.disabled, false);
  await act(async () => pendingQueueButton.click()); assert.equal(queued, 2); assert.equal(sends, 1);
  await act(async () => host.render(React.createElement(composer, { ...props, text: "" })));
  assert.ok(document.querySelector('button[aria-label="Send"]') === null); assert.ok(document.querySelector('button[aria-label="Stop"]'));
  const primary = () => document.querySelector(".composer-primary-action");
  const tooltipFor = async (button) => {
    const measure = window.HTMLElement.prototype.getBoundingClientRect;
    window.HTMLElement.prototype.getBoundingClientRect = () => ({ x: 100, y: 100, left: 100, top: 100, right: 200, bottom: 120, width: 100, height: 20 });
    try { return await openTooltip(button); } finally { window.HTMLElement.prototype.getBoundingClientRect = measure; }
  };
  const openTooltip = async (button) => {
    await act(async () => { button.dispatchEvent(new window.MouseEvent("pointermove", { bubbles: true })); await new Promise((resolve) => setTimeout(resolve, 260)); });
    const text = document.querySelector('[role="tooltip"]')?.textContent ?? "";
    await act(async () => button.dispatchEvent(new window.MouseEvent("pointerout", { bubbles: true, relatedTarget: document.body })));
    return text;
  };
  await act(async () => primary().click()); assert.equal(stops, 1, "an empty composer on an open turn stops it");
  await act(async () => host.render(React.createElement(composer, { ...props, text: "", interruptPending: true })));
  assert.deepEqual([primary().getAttribute("aria-label"), primary().getAttribute("aria-busy"), primary().getAttribute("aria-disabled")], ["Stop", "true", "true"]);
  await act(async () => primary().click()); assert.equal(stops, 1, "a pending stop is not sent twice");
  let resumes = 0;
  const resumable = { ...props, text: "", turnOpen: false, resumeAvailable: true, onResume: () => resumes++, canSubmit: false, submitBlockReason: "Type a message or attach a file." };
  await act(async () => host.render(React.createElement(composer, resumable)));
  assert.equal(primary().getAttribute("aria-label"), "Resume");
  await act(async () => primary().click()); assert.deepEqual([resumes, sends], [1, 1]);
  await act(async () => host.render(React.createElement(composer, { ...resumable, submitBlockReason: "The runtime is unavailable. Your message has not been sent." })));
  assert.equal(primary().getAttribute("aria-disabled"), "true");
  await act(async () => primary().click()); assert.equal(resumes, 1, "a blocked resume does nothing");
  assert.equal(await tooltipFor(primary()), "The runtime is unavailable. Your message has not been sent.");
  await act(async () => host.render(React.createElement(composer, { ...resumable, text: "Continue with totals", canSubmit: true, submitBlockReason: "" })));
  assert.equal(primary().getAttribute("aria-label"), "Send", "a typed message replaces Resume with Send");
  await act(async () => host.render(React.createElement(composer, { ...props, editingQueuedTurn: true })));
  assert.equal(primary().getAttribute("aria-label"), "Save queued message");
  await act(async () => host.render(React.createElement(composer, { ...props, turnOpen: false, canSubmit: false, submitBlockReason: "Task history is still loading." })));
  assert.equal(primary().getAttribute("aria-label"), "Send");
  assert.equal(primary().disabled, true);
  assert.equal(await tooltipFor(primary().parentElement), "Task history is still loading.");
  for (sendShortcut of ["enter", "cmd-if-multiline", "cmd-always"]) {
    let normal = 0; queued = 0;
    const shortcutProps = { ...props, canSubmit: false, onKeyDown: () => normal++ };
    await act(async () => host.render(React.createElement(composer, shortcutProps)));
    const editor = document.querySelector("textarea");
    await keydown(editor, "Enter"); assert.equal(normal, 1); assert.equal(queued, 0);
    const modifiers = { metaKey: true, shiftKey: sendShortcut !== "enter" };
    await keydown(editor, "Enter", modifiers); assert.equal(queued, 1); assert.equal(normal, 1);
    await keydown(editor, "Enter", { ...modifiers, metaKey: false, ctrlKey: true }); assert.equal(queued, 2);
    await keydown(editor, "Enter", { ...modifiers, repeat: true }); assert.equal(queued, 2);
    await keydown(editor, "Enter", { ...modifiers, isComposing: true }); assert.equal(normal, 1); assert.equal(queued, 2);
    editor.addEventListener("keydown", (event) => event.preventDefault(), { capture: true, once: true });
    await keydown(editor, "Enter", modifiers); assert.equal(normal, 1); assert.equal(queued, 2);
    await act(async () => host.render(React.createElement(composer, { ...shortcutProps, canQueue: false })));
    await keydown(editor, "Enter", modifiers); assert.equal(queued, 2); assert.equal(normal, 1, "blocked queue must not fall through to steer");
  }
  queued = 0; sends = 0;
  await act(async () => host.render(React.createElement(React.Fragment, null,
    React.createElement(composer, { ...props, key: "inactive", listenToGlobalCommands: false }),
    React.createElement(composer, { ...props, key: "active", listenToGlobalCommands: true }))));
  await act(async () => document.querySelectorAll("textarea")[1].focus());
  await act(async () => { commands.dispatchClientCommand("composer.queue", "keyboard"); commands.dispatchClientCommand("composer.steer", "surface"); });
  assert.equal(queued, 1); assert.equal(steers, 1, "only the active pane handles explicit steering"); assert.equal(sends, 0);
  await act(async () => host.render(React.createElement(React.Fragment, null,
    React.createElement(composer, { ...props, key: "inactive", listenToGlobalCommands: true }),
    React.createElement(composer, { ...props, key: "active", listenToGlobalCommands: true }))));
  await act(async () => commands.dispatchClientCommand("composer.queue", "surface"));
  assert.equal(queued, 2, "focus also prevents dispatch to a different composer");
  await act(async () => host.unmount());
  const storageApi = load("src/components/chat/storage.ts", { "./constants": { CHAT_SETTINGS_KEY: "settings" }, "./taskAccess": load("src/components/chat/taskAccess.ts") });
  const useDraft = load("src/components/chat/useChatDraftState.ts", {
    "./taskAccess": taskAccess,
    "./storage": storageApi, "./artifacts": { attachmentLabel: () => "operator.file" },
    "./attachmentDrop": {}, "./constants": { MAX_ATTACHMENT_BYTES: 1024 },
  }).useChatDraftState;
  const draftHost = createRoot(document.getElementById("root")); let draft;
  function DraftProbe({ home }) {
    draft = useDraft({ accessProfile: "lab", activeRunId: "run-a", storageScope: JSON.stringify([home, "main"]), onError: noop, preserveSubmissionIntentOnEdit: () => false });
    return null;
  }
  await act(async () => draftHost.render(React.createElement(DraftProbe, { home: "/Home-a" })));
  await act(async () => draft.setText("one"));
  const oldDraft = draft;
  await act(async () => draftHost.render(React.createElement(DraftProbe, { home: "/Home-b" })));
  assert.equal(draft.text, "");
  await act(async () => draft.setText("two"));
  await act(async () => oldDraft.consumeDraft("run-a", "one", []));
  assert.equal(draft.text, "two");
  await act(async () => draftHost.render(React.createElement(DraftProbe, { home: "/Home-a" })));
  assert.equal(draft.text, "one");
  await act(async () => draft.setText("newer text"));
  await act(async () => oldDraft.consumeDraft("run-a", "one", []));
  assert.equal(draft.text, "newer text");
  let preserveEdits = false;
  function IntentProbe({ home }) {
    draft = useDraft({ accessProfile: "lab", activeRunId: "run-a", storageScope: JSON.stringify([home, "main"]), onError: noop, preserveSubmissionIntentOnEdit: () => preserveEdits });
    return null;
  }
  await act(async () => draftHost.render(React.createElement(IntentProbe, { key: "intent-1", home: "/Home-intent" })));
  await act(async () => draft.setText("Prepared totals"));
  await act(async () => draft.persistSubmissionIntent("run-a", "msg_prepared", false));
  assert.equal(draft.messageId, "msg_prepared");
  await act(async () => draftHost.render(React.createElement(IntentProbe, { key: "intent-2", home: "/Home-intent" })));
  assert.deepEqual([draft.text, draft.messageId], ["Prepared totals", "msg_prepared"], "a prepared submission survives a reload with its identity");
  await act(async () => draft.setText("Prepared totals, edited"));
  await act(async () => draft.saveCurrentDraft("run-a"));
  assert.deepEqual([draft.messageId, draft.readDraft("run-a").messageId, draft.readDraft("run-a").text], ["", "", "Prepared totals, edited"],
    "editing a prepared message abandons its identity so the edit is a new message");
  preserveEdits = true;
  await act(async () => draft.persistSubmissionIntent("run-a", "msg_prepared", false));
  await act(async () => draft.setText("Prepared totals, edited again"));
  assert.equal(draft.messageId, "msg_prepared", "an edit the workspace preserves keeps the prepared identity");
  await act(async () => draft.completeSubmissionIntent("run-a", "msg_other"));
  assert.equal(draft.readDraft("run-a").messageId, "msg_prepared", "another message never completes this intent");
  await act(async () => draft.completeSubmissionIntent("run-a", "msg_prepared"));
  assert.deepEqual([draft.messageId, draft.readDraft("run-a").messageId], ["", ""], "a completed intent releases the identity");
  await act(async () => draftHost.unmount());
  dom.window.close();
  console.log("PASS client queue: exact identity, order/bytes, concurrent claims, remount/unknown, owner recovery, head gates, storage failure, Send/Stop/context-menu actions");
})().catch((error) => { console.error(error); process.exitCode = 1; });
