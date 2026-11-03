const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");
const { EventEmitter } = require("node:events");
const { createNativeBrowserSurfaceAdapter, createBrowserPresentationHost } = require("../electron/browserSurface.cjs");

const rustBrowserToolEvidence = {
  "schema_version": 1,
  "producer": "native_transport_authenticates_admitted_body_and_preserves_partial_and_chunked_frames",
  "crate_version": "0.1.0",
  "receipt_sha256": "48d60fd491802d247e8be7b9ad3870e7ed054a16f37eda316f5f217fd8392176",
  "source_abi": {
    "crates/octopus-tool-registry/src/lib.rs": "ae3ee84f5ce0f282df49c9af80d75368b8523d843c237f5cdcc054ca5b85097a",
    "crates/octopus-descriptors/src/process_authority.rs": "555cffef87f28ffb2e58e01a2f6b2980c460a7f59bbee69e3f4f0408baa3e037",
    "crates/octopus-descriptors/src/lib.rs": "cd7b628f8b5c293df0fbaa6707dba87790e35d21e0b74a80f2e195601ba51423",
    "crates/octopus-worker-env/src/formation.rs": "52a72e240780e32e412abeb943109fc8a4dc21425570ea4a9ff7c08ca7e76f32",
    "registry/worker_environment_kind/browser_session/packages/octopus-browser-session-env/src/lib.rs": "1305469ffead6a52f33167113753404f656b23aca5e5fc23126b424ece540a7a",
    "registry/worker_environment_kind/browser_session/packages/octopus-browser-session-env/src/tests.rs": "6288ad4e85f0b84a213e9b83528e7f212eb0a111766cc1abea7be5ec5f5fc7e4"
  },
  "pairs": [
    {
      "source_id": "browser.screenshot",
      "source_path": "registry/tool/browser.screenshot/tool.json",
      "source_sha256": "4fc25594f9c8c36a84cdcce60d16055bf3173c6a9c102b5caed5c2c4756ab16d",
      "exact_id": "browser.screenshot__chat_fixture",
      "door": "system_browser__chat_fixture",
      "formation_pin": "0e713f801a4cfbef9680f52a71c50cf405f58045f75ff7bb822ddb920e7dd964",
      "tool_descriptor_hash": "d6a11848f09369724d73790753a24278e7f52f0ed6201fdd69e8ba06d69a3b71",
      "tool_descriptor_contract_version": 3,
      "normalized_canonical_base64": "eyJhcmdzX3NjaGVtYSI6eyJhZGRpdGlvbmFsUHJvcGVydGllcyI6ZmFsc2UsInByb3BlcnRpZXMiOnsiY2xpcCI6eyJhZGRpdGlvbmFsUHJvcGVydGllcyI6ZmFsc2UsInByb3BlcnRpZXMiOnsiaGVpZ2h0Ijp7ImV4Y2x1c2l2ZU1pbmltdW0iOjAsInR5cGUiOiJudW1iZXIifSwid2lkdGgiOnsiZXhjbHVzaXZlTWluaW11bSI6MCwidHlwZSI6Im51bWJlciJ9LCJ4Ijp7InR5cGUiOiJudW1iZXIifSwieSI6eyJ0eXBlIjoibnVtYmVyIn19LCJyZXF1aXJlZCI6WyJ4IiwieSIsIndpZHRoIiwiaGVpZ2h0Il0sInR5cGUiOiJvYmplY3QifSwiZm9ybWF0Ijp7ImVudW0iOlsicG5nIiwianBlZyIsIndlYnAiXSwidHlwZSI6InN0cmluZyJ9LCJmdWxsX3BhZ2UiOnsidHlwZSI6ImJvb2xlYW4ifSwicGl4ZWxfbW9kZSI6eyJlbnVtIjpbImNzcyIsImRldmljZSJdLCJ0eXBlIjoic3RyaW5nIn0sInF1YWxpdHkiOnsibWF4aW11bSI6MTAwLCJtaW5pbXVtIjowLCJ0eXBlIjoiaW50ZWdlciJ9LCJ0YXJnZXRfaWQiOnsiZGVzY3JpcHRpb24iOiJFeGFjdCBhbHJlYWR5LW93bmVkIHBhZ2UgcmV0dXJuZWQgYnkgYnJvd3Nlci50YWJzIG9yIGJyb3dzZXIudGFiX25ldy4gVGhpcyBkb2VzIG5vdCBzZWxlY3QsIHNob3csIGNyZWF0ZSwgb3IgZm9jdXMgdGhhdCBwYWdlLiIsIm1heExlbmd0aCI6MjU2LCJtaW5MZW5ndGgiOjEsInBhdHRlcm4iOiJeW0EtWmEtejAtOV8tXSskIiwidHlwZSI6InN0cmluZyJ9fSwidHlwZSI6Im9iamVjdCJ9LCJkZXNjcmlwdGlvbiI6IkNhcHR1cmUgdGhlIGN1cnJlbnQgcGFnZSBpbnRvIHRoaXMgd29ya2VyIGVudmlyb25tZW50J3MgcHJpdmF0ZSBjYXB0dXJlcyBkaXJlY3RvcnkgYW5kIHJldHVybiBpdHMgcGF0aCwgYnl0ZSBjb3VudCwgYW5kIGNvbnRlbnQgaGFzaCBhcyBldmlkZW5jZS4gU3VwcGx5IHRhcmdldF9pZCB0byBhY3Qgb24gdGhhdCBleGFjdCBhbHJlYWR5LW93bmVkIHBhZ2UgaW5kZXBlbmRlbnRseSBvZiB0aGUgc2VsZWN0ZWQgb3IgZGlzcGxheWVkIHRhYi4gT21pdCB0YXJnZXRfaWQgdG8gdXNlIHRoZSBzZWxlY3RlZCB0YWIuIEEgY29tcGxldGUgZG9jdW1lbnQtQ1NTIGNsaXAgdGFrZXMgcHJlY2VkZW5jZSBvdmVyIGZ1bGxfcGFnZS4gcGl4ZWxfbW9kZT1jc3MgdXNlcyAxL2RldmljZVBpeGVsUmF0aW8gQ0RQIHNjYWxpbmc7IGRldmljZSByZW1haW5zIHRoZSBleGlzdGluZyBkZWZhdWx0LiBSZXR1cm5lZCBhcnRpZmFjdCBnZW9tZXRyeSByZWNvcmRzIGFjdHVhbCBkZWNvZGVkIGRpbWVuc2lvbnMgYW5kIG1hcHBpbmcuIiwiaGFuZGxlcl9jb25maWciOnsib3BlcmF0aW9uIjoic2NyZWVuc2hvdCJ9LCJpZCI6ImJyb3dzZXIuc2NyZWVuc2hvdF9fY2hhdF9maXh0dXJlIiwiaW50ZXJmYWNlIjoic3lzdGVtX2Jyb3dzZXJfX2NoYXRfZml4dHVyZSIsImlzX3dvcmxkX2VmZmVjdCI6ZmFsc2UsInByb2Nlc3MiOnsiYXJncyI6W10sImF1dGhvcml0eSI6eyJhbGxvd19hcHBsaWNhdGlvbl9sYXVuY2giOmZhbHNlLCJhbGxvd19sb2NhbF9pcGMiOnRydWUsImFsbG93X25ldHdvcmsiOnRydWUsImFsbG93X3NhbmRib3hfZGVsZWdhdGlvbiI6ZmFsc2UsImV4ZWN1dGFibGVfYXJndW1lbnRfcGF0aHMiOltdLCJleGVjdXRhYmxlX2NvbW1hbmRzIjpbXSwiZXhlY3V0YWJsZV9lbnZfcGF0aHMiOltdLCJleGVjdXRhYmxlX2Vudl9zdWJwYXRocyI6W10sImV4ZWN1dGFibGVfcGF0aHMiOltdLCJleGVjdXRhYmxlX3Jvb3Rfc2xvdHMiOltdLCJleGVjdXRhYmxlX3Jvb3Rfc3VicGF0aHMiOltdLCJvcHRpb25hbF9leGVjdXRhYmxlX3Jvb3Rfc2xvdHMiOltdLCJvcHRpb25hbF9yZWFkX2Vudl9wYXRocyI6W10sIm9wdGlvbmFsX3JlYWRfcm9vdF9zbG90cyI6W10sIm9wdGlvbmFsX3dyaXRlX2Vudl9wYXRocyI6W10sIm9wdGlvbmFsX3dyaXRlX3Jvb3Rfc2xvdHMiOltdLCJyZWFkX2FyZ3VtZW50X3BhdGhzIjpbXSwicmVhZF9lbnZfcGF0aHMiOltdLCJyZWFkX2Vudl9zdWJwYXRocyI6W10sInJlYWRfcGF0aHMiOltdLCJyZWFkX3Jvb3Rfc2xvdHMiOltdLCJ3cml0ZV9hcmd1bWVudF9wYXRocyI6W10sIndyaXRlX2Vudl9wYXRocyI6W10sIndyaXRlX2Vudl9zdWJwYXRocyI6W10sIndyaXRlX3BhdGhzIjpbXSwid3JpdGVfcm9vdF9zbG90cyI6W119LCJiaW4iOiJ0YXJnZXQvZGVidWcvb2N0b3B1cy1icm93c2VyLXNlc3Npb24tdG9vbCIsImNvb3JkaW5hdGVfcm9vdF9zbG90cyI6W10sImVudiI6e30sImluaGVyaXRfZW52IjpbXSwiaW5oZXJpdF9lbnZfZnJvbV9pbnRlcmZhY2UiOltdLCJpbnRlcnJ1cHRlZF9lZmZlY3RfcmVjb3ZlcnkiOiJpbmRldGVybWluYXRlIiwicHJpdmF0ZV9yZWFkX3N1YnBhdGhzIjpbeyJwYXRoIjoic2VjcmV0cy9icm93c2VyLW5hdGl2ZS1ob3N0LmF1dGgifV0sInJlcXVpcmVkX2ZpbGVzIjpbXX0sInN1cHBvcnRlZF93b3JrZXJfZW52aXJvbm1lbnRfa2luZHMiOlsiYnJvd3Nlcl9zZXNzaW9uIl19"
    },
    {
      "source_id": "browser.download_wait",
      "source_path": "registry/tool/browser.download_wait/tool.json",
      "source_sha256": "a3de19dbac731972d3834e03471076f6ce06bd7acc0758f04f1e44bf849f7364",
      "exact_id": "browser.download_wait__chat_fixture",
      "door": "door",
      "formation_pin": "1b388aa7197e6e6a6a5a48b0330cb3ae223061af9af696984b78185949f0fab4",
      "tool_descriptor_hash": "75ad19b85c24be2ba1783448919c7531fecb9b4642b61bd8619743a00b90ab49",
      "tool_descriptor_contract_version": 3,
      "normalized_canonical_base64": "eyJhcmdzX3NjaGVtYSI6eyJhZGRpdGlvbmFsUHJvcGVydGllcyI6ZmFsc2UsInByb3BlcnRpZXMiOnsiZG93bmxvYWRfaWQiOnsicGF0dGVybiI6Il5bMC05YS1mXXszMn0kIiwidHlwZSI6InN0cmluZyJ9LCJ0aW1lb3V0X21zIjp7Im1heGltdW0iOjMwMDAwLCJtaW5pbXVtIjoxLCJ0eXBlIjoiaW50ZWdlciJ9fSwicmVxdWlyZWQiOlsiZG93bmxvYWRfaWQiXSwidHlwZSI6Im9iamVjdCJ9LCJkZXNjcmlwdGlvbiI6Ik9ic2VydmUgYW4gZXhhY3QgcHJldmlvdXNseSBhcm1lZCBkb3dubG9hZCB3aXRoaW4gdGhpcyBmb3JtZWQgcm9vbSwgaW5kZXBlbmRlbnRseSBvZiB0aGUgc2VsZWN0ZWQgb3IgZGlzcGxheWVkIHRhYi4gVGhlIG9yaWdpbmFsIG5hdGl2ZSBEb3dubG9hZEl0ZW0gcmVtYWlucyBib3VuZCB0byBpdHMgYWRtaXR0ZWQgcm9vbSBhbmQgdGFyZ2V0LCBpbmNsdWRpbmcgYWZ0ZXIgdGhhdCB0YWIgY2xvc2VzLiBUZXJtaW5hbCBjb21wbGV0ZWQgRG93bmxvYWRJdGVtIGJ5dGVzIGFyZSBpbXBvcnRlZCBhcyBhbiBhcnRpZmFjdDsgcGVuZGluZyBzdGF0dXMgaXMgbm90IGNvbXBsZXRpb24uIHRpbWVvdXRfbXMgYm91bmRzIHRoaXMgb2JzZXJ2YXRpb24gb25seTsgY2FsbCBhZ2FpbiB3aXRoIHRoZSBzYW1lIGRvd25sb2FkX2lkIGlmIHN0aWxsIHBlbmRpbmcuIFRoaXMgZG9lcyBub3Qgc2VsZWN0IGEgdGFiLCB0cmlnZ2VyIG9yIHJlcGxheSBhbiBhY3Rpb24uIiwiaGFuZGxlcl9jb25maWciOnsib3BlcmF0aW9uIjoiZG93bmxvYWRfd2FpdCJ9LCJpZCI6ImJyb3dzZXIuZG93bmxvYWRfd2FpdF9fY2hhdF9maXh0dXJlIiwiaW50ZXJmYWNlIjoiZG9vciIsImlzX3dvcmxkX2VmZmVjdCI6ZmFsc2UsInByb2Nlc3MiOnsiYXJncyI6W10sImF1dGhvcml0eSI6eyJhbGxvd19hcHBsaWNhdGlvbl9sYXVuY2giOmZhbHNlLCJhbGxvd19sb2NhbF9pcGMiOnRydWUsImFsbG93X25ldHdvcmsiOnRydWUsImFsbG93X3NhbmRib3hfZGVsZWdhdGlvbiI6ZmFsc2UsImV4ZWN1dGFibGVfYXJndW1lbnRfcGF0aHMiOltdLCJleGVjdXRhYmxlX2NvbW1hbmRzIjpbXSwiZXhlY3V0YWJsZV9lbnZfcGF0aHMiOltdLCJleGVjdXRhYmxlX2Vudl9zdWJwYXRocyI6W10sImV4ZWN1dGFibGVfcGF0aHMiOltdLCJleGVjdXRhYmxlX3Jvb3Rfc2xvdHMiOltdLCJleGVjdXRhYmxlX3Jvb3Rfc3VicGF0aHMiOltdLCJvcHRpb25hbF9leGVjdXRhYmxlX3Jvb3Rfc2xvdHMiOltdLCJvcHRpb25hbF9yZWFkX2Vudl9wYXRocyI6W10sIm9wdGlvbmFsX3JlYWRfcm9vdF9zbG90cyI6W10sIm9wdGlvbmFsX3dyaXRlX2Vudl9wYXRocyI6W10sIm9wdGlvbmFsX3dyaXRlX3Jvb3Rfc2xvdHMiOltdLCJyZWFkX2FyZ3VtZW50X3BhdGhzIjpbXSwicmVhZF9lbnZfcGF0aHMiOltdLCJyZWFkX2Vudl9zdWJwYXRocyI6W10sInJlYWRfcGF0aHMiOltdLCJyZWFkX3Jvb3Rfc2xvdHMiOltdLCJ3cml0ZV9hcmd1bWVudF9wYXRocyI6W10sIndyaXRlX2Vudl9wYXRocyI6W10sIndyaXRlX2Vudl9zdWJwYXRocyI6W10sIndyaXRlX3BhdGhzIjpbXSwid3JpdGVfcm9vdF9zbG90cyI6W119LCJiaW4iOiJ0YXJnZXQvZGVidWcvb2N0b3B1cy1icm93c2VyLXNlc3Npb24tdG9vbCIsImNvb3JkaW5hdGVfcm9vdF9zbG90cyI6W10sImVudiI6e30sImluaGVyaXRfZW52IjpbXSwiaW5oZXJpdF9lbnZfZnJvbV9pbnRlcmZhY2UiOltdLCJpbnRlcnJ1cHRlZF9lZmZlY3RfcmVjb3ZlcnkiOiJpbmRldGVybWluYXRlIiwicHJpdmF0ZV9yZWFkX3N1YnBhdGhzIjpbeyJwYXRoIjoic2VjcmV0cy9icm93c2VyLW5hdGl2ZS1ob3N0LmF1dGgifV0sInJlcXVpcmVkX2ZpbGVzIjpbXX0sInN1cHBvcnRlZF93b3JrZXJfZW52aXJvbm1lbnRfa2luZHMiOlsiYnJvd3Nlcl9zZXNzaW9uIl19"
    }
  ]
};

function browserToolFixture(sourceId) {
  const { blake3 } = require("@noble/hashes/blake3");
  const { browserToolDescriptorPin } = require("../electron/browserSurface.cjs");
  const evidence = rustBrowserToolEvidence.pairs.find(pair => pair.source_id === sourceId);
  assert.ok(evidence);
  const raw = fs.readFileSync(path.resolve(__dirname, "../..", evidence.source_path));
  assert.equal(require("node:crypto").createHash("sha256").update(raw).digest("hex"), evidence.source_sha256, "source descriptor changed since the retained Rust evidence");
  const canonical = Buffer.from(evidence.normalized_canonical_base64, "base64");
  const descriptor = { ...JSON.parse(raw), id: evidence.exact_id, interface: evidence.door };
  assert.equal(Buffer.from(blake3(canonical)).toString("hex"), evidence.tool_descriptor_hash);
  assert.equal(browserToolDescriptorPin(JSON.parse(canonical)), evidence.tool_descriptor_hash, "Rust-normalized descriptor retains its captured formation pin");
  assert.equal(browserToolDescriptorPin(descriptor), evidence.tool_descriptor_hash, "raw source clone matches independent Rust normalization/formation evidence");
  return { ...evidence, descriptor };
}

async function adapterChecks() {
  const pin = "a".repeat(64);
  const data = { formation_pin: pin, engine_handle: "engine", host_generation: "generation", target_id: "target", browser_context_id: "context" };
  const surface = { id: "surface", kind: "browser", mode: "direct", protocol: "browser.native.v1", formationPin: pin, readOnly: false, data };
  const calls = [], events = [], errors = [], states = new Map();
  let missing = false, prepareWait = null, home = "/test/home", changeHomeDuringResolve = false, nextGeneration = 10;
  let rows = [{ worker_environment_id: "room", formation_pin: pin, lifecycle: "active", client_surfaces: [surface] }];
  const sender = Object.assign(new EventEmitter(), { isDestroyed: () => false, send: (_channel, event) => events.push(event) });
  const win = { webContents: sender, isDestroyed: () => false }, guest = {};
  const host = {
    async savedPermissions(request, check) { await check(); calls.push(["permissions", request]); return { hostGeneration: "generation", entries: [], ...(request.operation === "remove" ? { removed: true } : {}) }; },
    getTarget(projection) { assert.equal(projection.target_id, "target"); },
    async downloadAction(projection, action, id) { calls.push(["download", projection, action, id]); assert.equal("target_id" in projection, false); return { sequence: 1, downloads: [], unacknowledgedIds: [], error: null }; },
    async preparePresentation(request) {
      calls.push(["prepare", request]); const generation = ++nextGeneration;
      if (prepareWait) await prepareWait;
      const state = { attachmentId: request.attachmentId, hostGeneration: "generation", mountGeneration: generation, protocol: "browser.native.v1", presentationProtocol: "electron.ownedGuest.v1", presentationPhase: "prepared", viewportSequence: 0, modelViewport: { revision: 0, emulatedSize: null }, modelActive: false, modelTurnKey: "0", cursor: null, workerEnvironmentId: "room", surfaceId: "surface", targetId: "target", engineHandle: "engine", browserContextId: "context" };
      states.set(request.attachmentId, state); return { adoptionToken: "lease", hostGeneration: "generation", mountGeneration: generation, state };
    },
    validatePreparedGuest(request) { return request.guest === guest; },
    async acknowledgePresentation(request) { calls.push(["ack", request]); const state = { ...states.get(request.attachmentId), presentationPhase: "ready", viewportSequence: request.viewportSequence }; states.set(request.attachmentId, state); return state; },
    async syncPresentation(request) { calls.push(["sync", request]); const previous = states.get(request.attachmentId); if (request.viewportSequence <= previous.viewportSequence) throw new Error("stale viewport"); const state = { ...previous, viewportSequence: request.viewportSequence }; states.set(request.attachmentId, state); return state; },
    releasePresentation(request) { calls.push(["release", request]); states.delete(request.attachmentId); },
    viewerAction(request) { calls.push(["action", request]); return states.get(request.attachmentId); },
    cursorArrived(request) { calls.push(["cursor", request]); return { accepted: true }; },
    revokeFormation(request) { calls.push(["revoke", request]); },
    closeTarget(request) { calls.push(["closeTarget", request]); },
  };
  const adapter = createNativeBrowserSurfaceAdapter({ getHost: () => host, getHome: () => home, requireHome: (expected) => { if (expected !== home || expected !== "/test/home") throw new Error("Home changed"); }, resolveSurface: async () => { if (missing) throw new Error("Surface missing"); if (changeHomeDuringResolve) home = "/another/home"; return surface; }, resolveEnvironments: async () => rows, windowForSender: () => win, requireAccess: (token) => { if (token !== "owner") throw new Error("Access denied"); }, reportError: (error) => { errors.push(error); } });
  const prepare = (attachmentId = "attachment") => adapter.prepare("owner", sender, { attachmentId, workerEnvironmentId: "room", surfaceId: "surface", frameToken: "real-frame" });
  const epoch = { attachmentId: "attachment", hostGeneration: "generation", mountGeneration: 11 };
  const packet = (viewportSequence) => ({ ...epoch, viewportSequence, metricsRevision: 0, stageBounds: { x: 1, y: 2, width: 900, height: 600 }, pageBounds: { x: 1, y: 2, width: 900, height: 600 }, visualBounds: { x: 1, y: 2, width: 900, height: 600 }, scale: 1, visible: true });
  await assert.rejects(adapter.prepare("owner", sender, { attachmentId: "injected", workerEnvironmentId: "room", surfaceId: "surface", frameToken: "real-frame", projection: data }), /Invalid/);
  await prepare(); assert.equal(calls[0][1].frameToken, "real-frame"); assert.equal("bounds" in calls[0][1], false);
  assert.equal(adapter.validatePreparedGuest(sender, {}), false); assert.equal(adapter.validatePreparedGuest(sender, guest), true);
  await assert.rejects(adapter.action("owner", sender, { ...epoch, action: "reload" }), /acknowledged/);
  await adapter.acknowledge("owner", sender, packet(1));
  assert.equal((await adapter.cursorArrived("owner", sender, { ...epoch, moveSequence: 1, metricsRevision: 0 })).accepted, true);
  assert.equal(calls.at(-1)[1].projection.target_id, "target");
  await assert.rejects(adapter.cursorArrived("owner", sender, { ...epoch, mountGeneration: 12, moveSequence: 1, metricsRevision: 0 }), /generation/);
  await adapter.sync("owner", sender, packet(2)); await assert.rejects(adapter.sync("owner", sender, packet(1)), /stale/);
  await assert.rejects(adapter.action("owner", sender, { ...epoch, mountGeneration: 12, action: "reload" }), /generation/);
  await assert.rejects(adapter.action("owner", { isDestroyed: () => false }, { ...epoch, action: "reload" }), /unavailable/);
  await assert.rejects(adapter.action("owner", sender, { ...epoch, action: "claim" }), /Unsupported/);
  await adapter.action("owner", sender, { ...epoch, action: "reload" }); assert.equal(calls.at(-1)[0], "action");
  adapter.onEvent({ type: "state", attachmentId: "attachment", hostGeneration: "old", mountGeneration: 11, state: {} }); assert.equal(events.length, 0);
  await adapter.close("owner", sender, { ...epoch, mountGeneration: 10 }); assert.equal(calls.filter(([name]) => name === "release").length, 0);
  await adapter.revalidate(); missing = true; rows = []; await adapter.revalidate();
  assert.equal(calls.filter(([name]) => name === "release").length, 1); assert.equal(calls.filter(([name]) => name === "revoke").length, 0);
  rows = [{ worker_environment_id: "room", formation_pin: pin, lifecycle: "released", client_surfaces: [] }]; await adapter.revalidate();
  assert.equal(calls.filter(([name]) => name === "revoke").length, 1); missing = false;
  let finish; prepareWait = new Promise(resolve => { finish = resolve; }); const pending = prepare("cancelled");
  await new Promise(setImmediate); sender.emit("render-process-gone"); finish(); await assert.rejects(pending, /cancelled/);
  assert.equal(calls.filter(([name]) => name === "release").length, 2);
  await adapter.closePage("owner", sender, { home: "/test/home", workerEnvironmentId: "room", surfaceId: "surface" }); assert.equal(calls.at(-1)[0], "closeTarget");
  await assert.rejects(adapter.closePage("owner", sender, { home: "/test/home", workerEnvironmentId: "room", surfaceId: "surface", target_id: "arbitrary" }), /Invalid/);
  const savedSettings = await adapter.permissions("owner", sender, { operation: "list" }); assert.equal(savedSettings.home, "/test/home");
  assert.equal((await adapter.permissions("owner", sender, { operation: "remove", home: savedSettings.home, hostGeneration: savedSettings.hostGeneration, id: "d".repeat(64) })).removed, true);
  const permissionCalls = calls.filter(([name]) => name === "permissions").length;
  await assert.rejects(adapter.permissions("other", sender, { operation: "list" }), /Access/);
  await assert.rejects(adapter.permissions("owner", sender, { operation: "remove", home: "/other", hostGeneration: "generation", id: "d".repeat(64) }), /another Home/);
  assert.equal(calls.filter(([name]) => name === "permissions").length, permissionCalls);
  const downloads = { workerEnvironmentId: "room", surfaceId: "surface", hostGeneration: "generation", action: "snapshot" };
  assert.equal((await adapter.downloads("owner", sender, downloads)).sequence, 1); assert.equal(calls.at(-1)[2], "snapshot");
  await assert.rejects(adapter.downloads("owner", sender, { ...downloads, hostGeneration: "stale" }), /generation/);
  await assert.rejects(adapter.downloads("owner", sender, { ...downloads, path: "/arbitrary" }), /Invalid/);
  await assert.rejects(adapter.downloads("other", sender, downloads), /Access/);
  const downloadCalls = calls.filter(([name]) => name === "download").length;
  const closes = calls.filter(([name]) => name === "closeTarget").length; changeHomeDuringResolve = true;
  await assert.rejects(adapter.downloads("owner", sender, downloads), /Home changed/);
  assert.equal(calls.filter(([name]) => name === "download").length, downloadCalls);
  await assert.rejects(adapter.closePage("owner", sender, { home: "/test/home", workerEnvironmentId: "room", surfaceId: "surface" }), /Home changed/);
  assert.equal(calls.filter(([name]) => name === "closeTarget").length, closes);
  home = "/test/home"; prepareWait = null;
  const preparedCount = calls.filter(([name]) => name === "prepare").length;
  await assert.rejects(prepare("home-switch"), /Home changed/); assert.equal(calls.filter(([name]) => name === "prepare").length, preparedCount);
}

function packagedEngineChecks() {
  const crypto = require("node:crypto"), os = require("node:os");
  const file = path.resolve(__dirname, "../../registry/worker_environment_kind/browser_session/packages/octopus-browser-session-env/native/macos/engine-artifact.cjs");
  const verifier = require(file), contract = verifier.sourceContract();
  const sha = bytes => crypto.createHash("sha256").update(bytes).digest("hex");
  const temporary = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "octopus-package-proof-fixture-")));
  try {
    const bundle = path.join(temporary, "Fixture.app"), resources = path.join(bundle, "Contents/Resources");
    const executable = path.join(bundle, "Contents/MacOS/Octopus"), native = path.join(bundle, "Contents/Frameworks/Electron Framework.framework/Versions/A/Electron Framework");
    for (const directory of [resources, path.dirname(executable), path.dirname(native)]) fs.mkdirSync(directory, { recursive: true });
    fs.writeFileSync(executable, "injected launcher fixture"); fs.writeFileSync(native, "injected engine bytes; never loaded");
    const retained = verifier.inventory(bundle, ["Contents/Frameworks"]);
    const source = { electron_commit: contract.foundation.electron.commit, chromium_commit: contract.foundation.chromium.commit,
      electron_version: contract.foundation.electron.version, chromium_version: contract.foundation.chromium.version, node_version: contract.foundation.node_version,
      foundation_patch_sha256: contract.foundation.patch.sha256, production_patch_sha256: contract.production.patch.sha256,
      manifest_sha256: contract.manifest_sha256, production_manifest_sha256: contract.production_sha256,
      download_history_patch_sha256: contract.history.patch.sha256, download_history_manifest_sha256: contract.history_sha256,
      navigation_history_patch_sha256: contract.navigation.patch.sha256, navigation_history_manifest_sha256: contract.navigation_sha256 };
    const record = { schema_version: 2, producer: "octopus.browser.engine-package", producer_sha256: sha(fs.readFileSync(file)),
      engine_api: "electron.ownedGuest.v1", history_api: "electron.ownedNavigationHistory.v1", readiness: "native-accepted", architecture: "arm64", source,
      source_receipt_sha256: "a".repeat(64), source_inventory_sha256: "b".repeat(64), native_acceptance_sha256: "c".repeat(64),
      retained_engine: { root: "Contents/Frameworks", sha256: retained.sha256, member_count: retained.members.length },
      client_transformation: { executable: "Contents/MacOS/Octopus", source_executable: "Contents/MacOS/Electron", engine_frameworks: "preserve-byte-identical", signing: "client-resources-and-outer-bundle-only" } };
    const receipt = path.join(resources, "owned-browser-engine.json");
    const options = { resourcesRoot: resources, executablePath: executable, isPackaged: true, architecture: "arm64", nodeVersion: source.node_version, electronVersion: source.electron_version };
    fs.writeFileSync(receipt, JSON.stringify(record));
    const proof = verifier.verifyPackagedEngine(options);
    assert.equal(verifier.requirePackagedEngine(proof), proof);
    assert.throws(() => verifier.requirePackagedEngine({ ...proof }), /main-owned verified/);
    assert.throws(() => verifier.beginPackagedEngine(proof, bundle), /actual accepted engine result/);
    assert.throws(() => verifier.verifyPackagedEngine({ ...options, isPackaged: false }), /packaged implementation receipt/);
    assert.throws(() => verifier.verifyPackagedEngine({ ...options, resourcesRoot: temporary }), /this running packaged client/);
    fs.writeFileSync(receipt, JSON.stringify({ schema_version: 1, source }));
    assert.throws(() => verifier.verifyPackagedEngine(options), /incomplete or differs/);
    fs.writeFileSync(receipt, JSON.stringify({ ...record, source: { ...source, navigation_history_patch_sha256: "d".repeat(64) } }));
    assert.throws(() => verifier.verifyPackagedEngine(options), /pinned current source/);
    fs.writeFileSync(receipt, JSON.stringify(record)); fs.appendFileSync(native, "changed");
    assert.throws(() => verifier.verifyPackagedEngine(options), /accepted engine inventory/);
    assert.throws(() => verifier.requirePackagedEngine(proof), /changed/);
  } finally { fs.rmSync(temporary, { recursive: true, force: true }); }
}

async function startupChecks() {
  packagedEngineChecks();
  const clientRuntime = require("../native/runtime.cjs").clientRuntime();
  const ts = require("typescript");
  const main = ts.createSourceFile("main.cjs", fs.readFileSync(path.join(__dirname, "../electron/main.cjs"), "utf8"), ts.ScriptTarget.ES2022, true, ts.ScriptKind.JS);
  const names = new Set(["guardNativeBrowserPackage", "requireNativeBrowserPackageOwner", "startNativeBrowserEngine", "planBrowserNativeHost", "requireNativeBrowserHost", "nativeBrowserTargetsForProjection"]);
  const declarations = main.statements.filter((node) => ts.isFunctionDeclaration(node) && names.has(node.name?.text));
  assert.equal(declarations.length, names.size);
  const functions = declarations.map((node) => node.getText(main)).join("\n");
  const calls = [];
  const machine = { root_refs: {} };
  const door = { id: "system_browser", interface_driver_config: { private_config: { browser_room: { transport: { wire: "browser.native.v1", host_root_slot: "browser_native_host_root" } } } } };
  const installation = { available: true, installed: true, receipt_state: "valid", receipt_digest: "a".repeat(64), member_states: [{ registry_kind: "worker_environment_kind", owner: "browser_session", materialized: true, source_digest: "b".repeat(64), installed_digest: "b".repeat(64) }] };
  let guard, engineAvailable = false;
  const engineProof = Object.freeze({ receipt: "packaged-source-proof" });
  const context = vm.createContext({ path, console, process: { resourcesPath: "/client/Octopus.app/Contents/Resources", execPath: "/client/Octopus.app/Contents/MacOS/Octopus", arch: "arm64", versions: { node: clientRuntime.node, electron: clientRuntime.electron } }, resolveOctopusHome: () => "/test/home", initializeRuntimeHost: async () => {}, app: { whenReady: async () => {}, isPackaged: true }, clientSetupPackageRoots: () => ({ activeRegistryRoot: "/test/home/registry", stateRoot: "/test/home/client/package-state" }),
    registerLocalPackageMutationGuard({ guard: callback }) { calls.push("guard"); guard = callback; return () => {}; },
    async withLocalPackageInstallation(_options, callback) { return callback(installation); },
    fs: { promises: { async lstat() { return { isFile: () => true, isSymbolicLink: () => false, size: 256 }; }, async readFile(file) { return JSON.stringify(file.endsWith("host.json") ? machine : door); }, async realpath(file) { return file; } } },
    getNativeBrowserSurface: () => ({ onEvent() {} }),
    require(modulePath) {
      if (modulePath === "electron") return {};
      calls.push(modulePath);
      if (modulePath.endsWith("engine-mode.cjs")) return { verifyBrowserEngine(options) {
        assert.equal(options.executablePath, "/client/Octopus.app/Contents/MacOS/Octopus"); assert.equal(options.isPackaged, true);
        assert.equal(options.resourcesRoot, "/client/Octopus.app/Contents/Resources"); assert.equal(Object.hasOwn(options, "receiptPath"), false);
        if (!engineAvailable) throw new Error("Packaged engine proof unavailable"); return { mode: "owned", engineProof };
      } };
      if (modulePath.endsWith("setup-host.cjs")) return { planBrowserNativeHost: async (request) => ({ request }) };
      return { createBrowserEngineHost(options) { assert.equal(options.engineMode, "owned"); assert.equal(options.engineProof, engineProof); calls.push(options.hostRoot); return { async start() { calls.push("start"); }, async stop() { calls.push("stop"); }, listTargetsForProjection(request) { calls.push(request); return { targets: [], openIntent: null }; } }; } };
    },
  });
  vm.runInContext(`let nativeBrowserHost = null, nativeBrowserStartup = null, nativeBrowserStartupError = null, nativeBrowserHome = '', nativeBrowserModuleLoaded = false, nativeBrowserMutationGuard = null;\n${functions}`, context);
  installation.installed = false;
  installation.member_states[0].materialized = false;
  installation.receipt_state = "stale";
  installation.member_states[0].installed_digest = "c".repeat(64);
  await assert.rejects(vm.runInContext("startNativeBrowserEngine()", context), /installed browser owner is missing/);
  installation.member_states[0].installed_digest = "b".repeat(64);
  installation.member_states[0].materialized = true;
  await assert.rejects(vm.runInContext("startNativeBrowserEngine()", context), /explicit canonical absolute/);
  assert.equal(calls.includes("start"), false);
  assert.equal(vm.runInContext("nativeBrowserStartup", context), null, "explicit setup must be able to retry failed startup");
  machine.root_refs.browser_native_host_root = "/test/home/client/browser-native-host";
  door.interface_driver_config.host_slots = { browser_native_host_root: { path: "/test/custom-native-host" } };
  await assert.rejects(vm.runInContext("startNativeBrowserEngine()", context), /Packaged engine proof unavailable/);
  assert.equal(calls.includes("start"), false, "native host publication cannot precede package proof");
  engineAvailable = true;
  await Promise.all([vm.runInContext("startNativeBrowserEngine()", context), vm.runInContext("startNativeBrowserEngine()", context)]);
  assert.equal(calls.filter((call) => call === "start").length, 1);
  assert.equal(calls.filter((call) => call === "guard").length, 1);
  assert.ok(calls.includes("/test/custom-native-host"));
  const observed = vm.runInContext("nativeBrowserTargetsForProjection({octopusHome:'/test/home',worker_environment_id:'room',formation_pin:'pin',engine_handle:'engine',host_generation:'generation',browser_context_id:'context'})", context);
  assert.equal(observed.targets.length, 0, "empty live inventory must remain authoritative");
  assert.equal(calls.at(-1).protocol, "browser.native.v1");
  assert.equal(Object.hasOwn(calls.at(-1), "octopusHome"), false);
  assert.equal(Object.hasOwn(calls.at(-1), "target_id"), false, "closed seed target must not prevent room inventory");
  assert.throws(() => vm.runInContext("nativeBrowserTargetsForProjection({octopusHome:'/another/home'})", context), /another Octopus Home/);
  assert.throws(() => guard(["driver.browser_session"]), /Restart/);
  installation.member_states[0].installed_digest = "c".repeat(64);
  installation.member_states[0].materialized = false;
  await assert.rejects(vm.runInContext("planBrowserNativeHost({})", context), /installed browser owner is missing/);
  assert.equal(calls.some((call) => call.endsWith?.("setup-host.cjs")), false);
}

async function browserPreviewChecks() {
  const { blake3 } = require("@noble/hashes/blake3");
  const image = Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aN5cAAAAASUVORK5CYII=", "base64"), calls = [], live = new Set(["A", "B"]);
  const { browserToolDescriptorPin } = require("../electron/browserSurface.cjs");
  const evidence = browserToolFixture("browser.screenshot"), clone = evidence.descriptor;
  let currentDescriptor = structuredClone(clone), payloadTool = clone.id;
  let home = "/test/home", changeHome = false;
  const row = (target, seq) => ({ run_id: "run", movement_seq: seq + 1, from: { kind: "interface", id: clone.interface }, label: "tool.result", artifact_id: `tool.result:${String(seq).padStart(64, "0")}`, content_hash: String(seq).padStart(64, "0"), target, seq });
  const rows = [{ ...row("hidden", 9), hidden: true }, row("A", 11), row("B", 12)];
  const surface = target => ({ id: `surface:${target}`, protocol: "browser.native.v1", mode: "direct", data: { target_id: target, host_generation: "host", engine_handle: "engine", browser_context_id: "context" } });
  const owner = { room: { worker_environment_id: "room", formation_pin: evidence.formation_pin, run_id: "run", lifecycle: "active", opened_at_movement_seq: 1, door: clone.interface, tools: [clone.id], tool_descriptor_hashes: { [clone.id]: evidence.tool_descriptor_hash }, client_surfaces: [surface("A"), surface("B")] }, surface: surface("B") };
  const host = createBrowserPresentationHost({ homePath: () => home, requireAccess() {},
    async currentRoom(expected) { assert.equal(expected.room.worker_environment_id, "room"); return owner.room; },
    async readToolDescriptor(id) { assert.equal(id, clone.id); return structuredClone(currentDescriptor); },
    async readActivity(run, after) { assert.equal(run, "run"); const available = rows.filter(value => value.movement_seq > after), selected = available.slice(0, 1); return { activity: selected.filter(value => !value.hidden), latest_seq: selected.at(-1)?.movement_seq ?? null, has_newer: available.length > selected.length }; },
    async readArtifact(value) { return { payload: { tool: payloadTool, ok: true, body: { result: "browser.screenshot", worker_environment_id: "room", target_id: value.target, source_movement_seq: value.seq, bytes: image.length, media_type: "image/png", content_hash: Buffer.from(blake3(image)).toString("hex"), media: [{ mime_type: "image/png", payload_base64: image.toString("base64") }] } } }; },
    async currentTarget(record) { assert.equal(record.projection.formation_pin, evidence.formation_pin); if (!live.has(record.projection.target_id)) throw new Error("Target closed"); if (changeHome) home = "/other/home"; },
    async getNativeHost() { return { upsertBrowserImage(request) { calls.push(["image", request]); }, invalidateBrowserImage(request) { calls.push(["remove", request]); } }; },
    async focusTarget(record) { calls.push(["focus", record.projection.target_id]); },
  });
  const turn = { run_id: "run", turn_open: true, turn_opened_movement_seq: 10, last_movement_seq: 20 };
  await host.reconcile([owner], [turn]);
  const published = calls.filter(([name]) => name === "image"); assert.equal(published.length, 2);
  assert.deepEqual(published.map(([,value]) => value.image), [image, image]);
  assert.deepEqual(host.snapshot().map(value => value.target_id), ["A", "B"], "preview target is independent of selected tab");
  await host.reconcile([owner], [turn]); assert.equal(calls.length, 2, "same durable movement cannot republish an image");
  await host.onEvent({ type: "focus-browser", identity: published[0][1].identity, groupId: "run", batchId: "10" }); assert.deepEqual(calls.at(-1), ["focus", "A"]);
  live.delete("A"); owner.room.client_surfaces = [surface("B")];
  await host.reconcile([owner], [turn]); assert.equal(host.snapshot().length, 1); assert.equal(calls.at(-1)[0], "remove");
  await host.reconcile([owner], [{ ...turn, turn_open: false }]); assert.equal(host.snapshot().length, 0); assert.equal(calls.filter(([name]) => name === "remove").length, 2);
  rows.push(row("B", 31));
  const nextTurn = { ...turn, turn_opened_movement_seq: 30, last_movement_seq: 40 };
  for (const altered of [{ ...clone, handler_config: { operation: "snapshot" } }, { ...clone, interface: "another-exact-door" }]) {
    currentDescriptor = altered; owner.room.tool_descriptor_hashes[clone.id] = browserToolDescriptorPin(altered);
    await assert.rejects(host.reconcile([owner], [nextTurn]), /exact declared tool operation, door and formation pin/);
    assert.equal(calls.filter(([name]) => name === "image").length, 2);
  }
  currentDescriptor = structuredClone(clone); owner.room.tool_descriptor_hashes[clone.id] = "f".repeat(64);
  await assert.rejects(host.reconcile([owner], [nextTurn]), /exact declared tool operation, door and formation pin/);
  owner.room.tool_descriptor_hashes[clone.id] = evidence.tool_descriptor_hash; payloadTool = "browser.screenshot";
  await assert.rejects(host.reconcile([owner], [nextTurn]), /not pinned in its current formed grant/);
  assert.equal(calls.filter(([name]) => name === "image").length, 2, "canonical id cannot substitute for the actual formed clone");
  payloadTool = clone.id; changeHome = true;
  await host.reconcile([owner], [{ ...turn, turn_opened_movement_seq: 30, last_movement_seq: 40 }]);
  assert.equal(calls.filter(([name]) => name === "image").length, 2, "Home change during source admission cannot publish");
  assert.equal(host.snapshot()[0].phase, "failed"); host.stop();
}

async function rendererChecks(mode = "owned") {
  const { engineInfo } = require("../../registry/worker_environment_kind/browser_session/packages/octopus-browser-session-env/native/engine-mode.cjs");
  const engine = engineInfo(mode);
  const { JSDOM } = require("jsdom");
  const React = require("react");
  const ts = require("typescript");
  const dom = new JSDOM("<input id='composer'><div id='root'></div>", { url: "http://client.test", pretendToBeVisual: true });
  const saved = Object.fromEntries(["window", "document", "navigator", "IS_REACT_ACT_ENVIRONMENT"].map((key) => [key, Object.getOwnPropertyDescriptor(globalThis, key)]));
  for (const [key, value] of Object.entries({ window: dom.window, document: dom.window.document, navigator: dom.window.navigator, IS_REACT_ACT_ENVIRONMENT: true })) Object.defineProperty(globalThis, key, { configurable: true, writable: true, value });
  const { createRoot } = require("react-dom/client");
  const calls = [], listeners = new Set(), menus = [];
  const data = { target_id: "target", engine_handle: "engine", host_generation: "generation", browser_context_id: "context", formation_pin: "pin", url: "https://page.test/" };
  let current, holdAcknowledgement = null;
  let downloads = { sequence: 0, downloads: [], unacknowledgedIds: [], error: null, preferences: { directory: null, promptForLocation: false } };
  let pageLeft = 30;
  const visualViewport = { offsetLeft: 0, offsetTop: 0, width: dom.window.innerWidth, height: dom.window.innerHeight }; Object.defineProperty(dom.window, "visualViewport", { value: visualViewport });
  dom.window.HTMLElement.prototype.getBoundingClientRect = () => ({ x: pageLeft, y: 100, left: pageLeft, top: 100, right: pageLeft + 640, bottom: 580, width: 640, height: 480 });
  const styles = dom.window.document.createElement("style");
  styles.textContent = fs.readFileSync(path.join(__dirname, "../src/components/worker-environment/worker-environment.css"), "utf8");
  dom.window.document.head.append(styles);
  let nativeAttached = false;
  if (mode === "owned") {
    dom.window.HTMLElement.prototype.getOwnedGuestFrameToken = function () { assert.equal(this.tagName, "WEBVIEW"); assert.equal(this.hasAttribute("src"), false); return "native-frame"; };
    dom.window.HTMLElement.prototype.adoptOwnedGuest = async function (token, generation) { assert.equal(token, "lease"); assert.equal(generation, 11); assert.equal(this.style.width, "640px"); assert.equal(this.style.height, "480px"); nativeAttached = true; calls.push(["native-adopt"]); };
    dom.window.HTMLElement.prototype.releaseOwnedGuest = function () { nativeAttached = false; calls.push(["native-release"]); };
  }
  dom.window.octopusTransport = {
    showClientContextMenu(request) { calls.push(["menu", request]); return new Promise((resolve, reject) => menus.push({ request, resolve, reject })); },
    async cancelClientContextMenu(request) { calls.push(["menu-cancel", request]); return { cancelled: true }; },
    async prepareNativeBrowserViewer(request) { calls.push(["prepare", request]); if (mode === "owned") assert.equal(request.frameToken, "native-frame"); else assert.match(request.frameToken, /^[a-f0-9-]{36}$/); current = { engine, attachmentId: request.attachmentId, mountGeneration: 11, hostGeneration: "generation", protocol: "browser.native.v1", presentationProtocol: engine.presentationProtocol, presentationPhase: mode === "owned" ? "prepared" : "attached", viewportSequence: 0, modelViewport: { revision: 0, emulatedSize: null }, modelActive: false, modelTurnKey: "0", cursor: null, workerEnvironmentId: "room", run: "run", ee: "ee", surfaceId: "surface", targetId: "target", engineHandle: "engine", browserContextId: "context", connected: true, visible: false, readOnly: false, loading: false, url: data.url, title: "Page", canGoBack: true, canGoForward: false, zoomFactor: 1 }; return { adoptionToken: mode === "owned" ? "lease" : null, hostGeneration: "generation", mountGeneration: 11, state: current }; },
    async acknowledgeNativeBrowserViewer(request) { assert.equal(nativeAttached, mode === "owned"); calls.push(["ack", request]); if (holdAcknowledgement) return holdAcknowledgement(request); return (current = { ...current, viewportSequence: request.viewportSequence, presentationPhase: "ready", visible: request.visible }); },
    async syncNativeBrowserViewer(request) { assert.equal(request.mountGeneration, 11); assert.equal(request.hostGeneration, "generation"); calls.push(["sync", request]); return (current = { ...current, viewportSequence: request.viewportSequence, visible: request.visible }); },
    async nativeBrowserViewerAction(request) { calls.push(["action", request]); if (request.action === "find") current = { ...current, findMatches: request.params.query ? 2 : 0, findActiveOrdinal: request.params.query ? 1 : 0 }; return current; },
    async nativeBrowserCursorArrived(request) { calls.push(["cursor", request]); return { accepted: true }; },
    async nativeBrowserDownloads(request) {
      assert.equal(request.workerEnvironmentId, "room"); assert.equal(request.hostGeneration, "generation");
      calls.push(["download", request]);
      if (request.action === "acknowledge") downloads = { ...downloads, sequence: downloads.sequence + 1, unacknowledgedIds: [] };
      if (request.action === "pause") downloads = { ...downloads, sequence: downloads.sequence + 1, downloads: downloads.downloads.map(record => ({ ...record, status: "paused", canPause: false, canResume: true, sequence: downloads.sequence + 1 })) };
      if (request.action === "search") return { ...downloads, history: { text: request.query.text, offset: request.query.offset, hasMore: false, downloads: downloads.downloads } };
      return downloads;
    },
    async closeNativeBrowserViewer(request) { assert.equal(request.hostGeneration, "generation"); assert.equal(request.mountGeneration, 11); calls.push(["close", request]); },
    onNativeBrowserViewerEvent(listener) { listeners.add(listener); return () => listeners.delete(listener); },
  };
  const cursorModule = { exports: {} };
  const cursorSource = ts.transpileModule(fs.readFileSync(path.join(__dirname, "../src/components/worker-environment/browser-cursor.ts"), "utf8"), { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS }, reportDiagnostics: true });
  assert.equal(cursorSource.diagnostics.length, 0);
  vm.runInNewContext(cursorSource.outputText, { exports: cursorModule.exports, window: dom.window, document: dom.window.document, performance: dom.window.performance });
  const motionModule = { exports: {} }, frames = new Map(), arrivals = []; let clock = 0, frameId = 0;
  const motionWindow = { requestAnimationFrame(callback) { const id = ++frameId; frames.set(id, callback); return id; }, cancelAnimationFrame(id) { frames.delete(id); } };
  vm.runInNewContext(cursorSource.outputText, { exports: motionModule.exports, window: motionWindow, document: dom.window.document, performance: { now: () => clock } });
  const motionLayer = dom.window.document.createElement("div");
  const motion = motionModule.exports.createBrowserCursor(motionLayer, "cursor.png", seq => arrivals.push(seq));
  const advance = () => { for (let i = 0; i < 300 && frames.size; i++) { clock += 1000 / 60; const pending = [...frames.values()]; frames.clear(); pending.forEach(callback => callback(clock)); } };
  const setCursor = (moveSequence, x, y) => motion.setState({ cursor: { moveSequence, x, y }, visible: true, turnKey: "turn1", viewportSize: { width: 1000, height: 800 } });
  setCursor(1, 100, 80); assert.deepEqual(arrivals, [1]);
  setCursor(2, 900, 600); assert.deepEqual(arrivals, [1], "arrival waits for the actual outer cursor motion");
  advance(); assert.deepEqual(arrivals, [1, 2]);
  assert.match(motionLayer.querySelector("[data-testid='browser-model-cursor']").style.transform, /^translate3d\(888px, 588px/);
  setCursor(3, 940, 610); advance(); assert.deepEqual(arrivals, [1, 2, 3], "long curve and short scoot each settle once");
  setCursor(3, 940, 610); advance(); assert.deepEqual(arrivals, [1, 2, 3], "repeated presentation cannot duplicate arrival");
  motion.setState({ cursor: null, visible: false, turnKey: "turn1", viewportSize: { width: 1000, height: 800 } }); advance();
  assert.equal(motionLayer.querySelector("[data-testid='browser-model-cursor']").style.opacity, "0");
  motion.destroy(); assert.equal(motionLayer.childElementCount, 0); assert.equal(frames.size, 0);
  const formatModule = { exports: {} }, downloadModule = { exports: {} };
  vm.runInNewContext(ts.transpileModule(fs.readFileSync(path.join(__dirname, "../src/components/chat/format.ts"), "utf8"), { compilerOptions: { module: ts.ModuleKind.CommonJS } }).outputText, { exports: formatModule.exports });
  const downloadSource = ts.transpileModule(fs.readFileSync(path.join(__dirname, "../src/components/worker-environment/BrowserDownloads.tsx"), "utf8"), { compilerOptions: { jsx: ts.JsxEmit.ReactJSX, target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS }, reportDiagnostics: true });
  assert.equal(downloadSource.diagnostics.length, 0);
  vm.runInNewContext(downloadSource.outputText, { exports: downloadModule.exports, window: dom.window, document: dom.window.document, Node: dom.window.Node, setTimeout, clearTimeout, crypto: require("node:crypto").webcrypto,
    require: name => name.startsWith("react") ? require(name) : name.endsWith("format") ? formatModule.exports : name.endsWith("IconGlyphs") ? { Glyph: () => null } : { OcButton: "button", OcMenuSurface: "div" },
  });
  const module = { exports: {} };
  const source = ts.transpileModule(fs.readFileSync(path.join(__dirname, "../src/components/worker-environment/RunEnvironmentBrowserSurfaces.tsx"), "utf8"), { compilerOptions: { jsx: ts.JsxEmit.ReactJSX, target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS }, reportDiagnostics: true });
  assert.equal(source.diagnostics.length, 0);
  vm.runInNewContext(source.outputText + "\nexports.nativeBrowserStageFits = nativeBrowserStageFits;", { module, exports: module.exports, window: dom.window, document: dom.window.document, URL, Node: dom.window.Node, MutationObserver: dom.window.MutationObserver, ResizeObserver: class { observe() {} disconnect() {} }, crypto: require("node:crypto").webcrypto,
    require: (name) => name.startsWith("react") ? require(name) : name.endsWith(".css") || name.endsWith("browserCoordinates.mjs") ? {}
      : name.endsWith("MessageTooltip") ? { MessageTooltip: ({ children }) => React.createElement("span", null, children) }
      : name.endsWith("BrowserDownloads") ? downloadModule.exports
      : name.endsWith("browser-cursor") ? cursorModule.exports
      : name.endsWith("browser-cursor.png") ? { default: "data:image/png;base64,iVBORw0KGgo=" }
      : name.endsWith("projectionClient") ? { formatProjectionError: (error) => error.message }
      : name.endsWith("IconGlyphs") ? { Glyph: () => null }
      : name.endsWith("components") ? { OcIconButton: ({ label, glyph, appearance, ...props }) => React.createElement("button", { ...props, "aria-label": label }), OcButton: "button", OcMenuItem: ({ label, leadingIcon, ...props }) => React.createElement("button", props, label), OcMenuSurface: "div" }
      : name.endsWith("OctopusSurface") ? { OctopusEmptyState: () => null }
      : { LiveSurfaceEmpty: ({ title }) => React.createElement("div", null, title), ManualRoomControlWarning: () => { throw new Error("Native viewing must not mount a manual gate"); }, dataText: (data, key) => typeof data[key] === "string" ? data[key] : "", isViewerUrl: (value) => /^https?:/.test(value) },
  });
  const root = createRoot(dom.window.document.getElementById("root"));
  const props = { active: true, environment: { id: "room", runId: "run", lifecycle: "active" }, surface: { id: "surface", mode: "direct", protocol: "browser.native.v1", readOnly: false, data } };
  const settleLayout = () => React.act(async () => { await new Promise((resolve) => dom.window.requestAnimationFrame(() => dom.window.requestAnimationFrame(() => dom.window.requestAnimationFrame(resolve)))); });
  const openBrowserMenu = async () => {
    await React.act(async () => dom.window.document.querySelector('[aria-label="More browser actions"]').click()); await settleLayout();
    assert.equal(current.visible, true, "native popup must preserve the rendered page");
    assert.equal(dom.window.document.querySelector('[role="menu"][aria-label="Browser actions"]'), null);
    assert.equal(dom.window.document.querySelector('[aria-label="More browser actions"]').getAttribute("aria-expanded"), "true");
    return menus.at(-1);
  };
  const selectBrowserMenu = (pending, id) => React.act(async () => pending.resolve({ requestId: pending.request.requestId, id }));
  const publish = () => { for (const listener of listeners) listener({ type: "state", attachmentId: current.attachmentId, hostGeneration: current.hostGeneration, mountGeneration: current.mountGeneration, state: current }); };
  const checkClosedPage = async () => {
    const pendingMenu = await openBrowserMenu(), actionCount = calls.filter(([type]) => type === "action").length;
    await React.act(async () => { for (const listener of listeners) listener({ type: "error", attachmentId: current.attachmentId, hostGeneration: current.hostGeneration, mountGeneration: current.mountGeneration, error: { code: "presentation_unavailable", message: "Connection failed" } }); });
    assert.ok(calls.some(([type, request]) => type === "menu-cancel" && request.requestId === pendingMenu.request.requestId));
    await selectBrowserMenu(pendingMenu, "zoom-in"); assert.equal(calls.filter(([type]) => type === "action").length, actionCount);
    assert.match(dom.window.document.querySelector('[role="status"]').textContent, /Browser unavailable/);
    await React.act(async () => [...dom.window.document.querySelectorAll('button')].find(button => button.textContent === 'Try again').click()); await settleLayout();
    const before = calls.filter(([type]) => type === "prepare").length;
    await React.act(async () => { for (const listener of listeners) listener({ type: "closed", attachmentId: current.attachmentId, hostGeneration: current.hostGeneration, mountGeneration: current.mountGeneration, error: { code: "target_unavailable", message: "Browser page closed by the user" } }); });
    assert.match(dom.window.document.querySelector('[role="status"]').textContent, /Browser page closed/); assert.equal([...dom.window.document.querySelectorAll('button')].some(button => button.textContent === 'Try again'), false);
    assert.equal(calls.filter(([type]) => type === "prepare").length, before, 'closed target is never recreated');
  };
  try {
    const composer = dom.window.document.getElementById("composer"); composer.focus();
    await React.act(async () => root.render(React.createElement(module.exports.BrowserSurface, props)));
    await settleLayout();
    assert.equal(dom.window.document.activeElement, composer, "tool opening must preserve composer focus");
    const guestElement = dom.window.document.querySelector("webview"); assert.ok(guestElement);
    assert.equal(dom.window.document.querySelector("iframe, canvas"), null);
    const layout = calls.find(([type]) => type === "ack")[1];
    current = { ...current, modelActive: true }; await React.act(async () => publish()); await settleLayout();
    assert.equal(dom.window.document.querySelector(".we-browser-status"), null, "model activity must not add a browser banner");
    const staleMenu = await openBrowserMenu(), actionsBefore = calls.filter(([type]) => type === "action").length;
    assert.ok(staleMenu.request.items.find(item => item.id === "zoom").submenu.every(item => item.enabled));
    current = { ...current, readOnly: true }; await React.act(async () => publish());
    assert.ok(calls.some(([type, request]) => type === "menu-cancel" && request.requestId === staleMenu.request.requestId));
    await selectBrowserMenu(staleMenu, "zoom-in"); assert.equal(calls.filter(([type]) => type === "action").length, actionsBefore);
    assert.match(dom.window.document.querySelector(".we-browser-status").textContent, /Read-only browser/);
    const lockedMenu = await openBrowserMenu();
    assert.ok(lockedMenu.request.items.find(item => item.id === "zoom").submenu.every(item => !item.enabled));
    assert.equal(lockedMenu.request.items.find(item => item.id === "find").enabled, true);
    await selectBrowserMenu(lockedMenu, "zoom-in"); assert.equal(calls.filter(([type]) => type === "action").length, actionsBefore);
    await React.act(async () => [...dom.window.document.querySelectorAll("button")].find(button => button.textContent === "Dismiss").click());
    current = { ...current, readOnly: false }; await React.act(async () => publish()); await settleLayout();
    const failedMenu = await openBrowserMenu(); await React.act(async () => failedMenu.reject(new Error("Popup unavailable")));
    assert.equal(dom.window.document.querySelector('[aria-label="More browser actions"]').getAttribute("aria-expanded"), "false");
    assert.match(dom.window.document.querySelector('[role="alert"]').textContent, /Popup unavailable/);
    await React.act(async () => [...dom.window.document.querySelectorAll("button")].find(button => button.textContent === "Dismiss").click());
    await selectBrowserMenu(await openBrowserMenu(), "zoom-out");
    assert.equal(calls.at(-1)[1].action, "zoom"); assert.equal(calls.at(-1)[1].params.zoomFactor, 0.9);
    assert.equal(calls.at(-1)[1].attachmentId, current.attachmentId);
    if (mode === "standard") {
      const clipped = dom.window.document.createElement("div"), stage = clipped.appendChild(dom.window.document.createElement("div")); clipped.style.overflowX = "hidden";
      const measured = { x: 478.5714416503906, y: 200.68080139160156, width: 431.0044860839844, height: 366.02679443359375 }; clipped.getBoundingClientRect = () => ({ left: 477.8571472167969, right: 909.5758972167969, top: 83.99553680419922, bottom: 566.7076034545898 }); dom.window.devicePixelRatio = 1.4;
      assert.equal(module.exports.nativeBrowserStageFits(stage, measured), true); assert.equal(module.exports.nativeBrowserStageFits(stage, { ...measured, width: measured.width + 1 }), false);
      dom.window.innerHeight = 514; visualViewport.height = 514.2857055664062;
      assert.equal(module.exports.nativeBrowserStageFits(clipped, { x: 0, y: 0, width: 800, height: visualViewport.height }), true); assert.equal(module.exports.nativeBrowserStageFits(clipped, { x: 0, y: 0, width: 800, height: visualViewport.height + 1 }), false); dom.window.innerHeight = visualViewport.height = 768;
      dom.window.devicePixelRatio = 1;
      assert.equal(typeof guestElement.adoptOwnedGuest, "undefined"); assert.equal(layout.scale, 1); assert.equal(layout.visible, true);
      assert.equal(layout.pageBounds.width, 640); assert.equal(layout.visualBounds.width, 640);
      current = { ...current, presentationPhase: "attached", modelViewport: { revision: 1, emulatedSize: { width: 1000, height: 800 } } };
      await React.act(async () => { for (const listener of listeners) listener({ type: "state", attachmentId: current.attachmentId, hostGeneration: current.hostGeneration, mountGeneration: current.mountGeneration, state: current }); });
      await settleLayout();
      const emulatedLayout = calls.findLast(([type]) => type === "ack")[1];
      assert.equal(emulatedLayout.metricsRevision, 1); assert.equal(emulatedLayout.scale, 1);
      assert.equal(emulatedLayout.pageBounds.width, 640); assert.equal(emulatedLayout.pageBounds.height, 480);
      assert.equal(calls.some(([type]) => type === "native-adopt" || type === "native-release"), false);
      assert.ok(calls.some(([type, request]) => type === "download" && request.action === "snapshot"));
      assert.equal(dom.window.document.querySelector('button[aria-label^="Downloads"]'), null);
      await selectBrowserMenu(await openBrowserMenu(), "details");
      assert.ok(dom.window.document.querySelector("details").textContent.includes("Standard Chromium"));
      await React.act(async () => { dom.window.document.querySelector("details").open = false; await new Promise(resolve => setTimeout(resolve, 0)); });
      for (const role of ["menu", "dialog"]) {
        const overlay = dom.window.document.createElement("div"); overlay.setAttribute("role", role);
        await React.act(async () => dom.window.document.body.append(overlay)); await settleLayout();
        assert.equal(current.visible, false, `${role} must hide the native view`);
        await React.act(async () => overlay.remove()); await settleLayout(); assert.equal(current.visible, true);
      }
      const clip = dom.window.document.getElementById("root");
      await React.act(async () => clip.setAttribute("inert", "")); await settleLayout(); assert.equal(current.visible, false);
      await React.act(async () => clip.removeAttribute("inert")); await settleLayout(); assert.equal(current.visible, true);
      await selectBrowserMenu(await openBrowserMenu(), "find"); await settleLayout();
      const find = dom.window.document.querySelector('input[aria-label="Find in page"]');
      assert.equal(dom.window.getComputedStyle(find.form).position, "static"); assert.equal(dom.window.document.activeElement, find); assert.equal(current.visible, true);
      await React.act(async () => { Object.getOwnPropertyDescriptor(dom.window.HTMLInputElement.prototype, "value").set.call(find, "needle"); find.dispatchEvent(new dom.window.Event("input", { bubbles: true })); });
      assert.equal(calls.findLast(([type, request]) => type === "action" && request.action === "find")[1].params.query, "needle"); assert.ok(find.form.textContent.includes("1 / 2")); assert.equal(current.visible, true);
      await React.act(async () => find.dispatchEvent(new dom.window.KeyboardEvent("keydown", { key: "Escape", bubbles: true })));
      assert.equal(dom.window.document.querySelector('input[aria-label="Find in page"]'), null); assert.equal(current.visible, true); assert.equal(calls.at(-1)[1].action, "focus");
      await React.act(async () => root.render(React.createElement(module.exports.BrowserSurface, { ...props, active: false })));
      assert.equal(calls.at(-1)[0], "close"); assert.equal(listeners.size, 0);
      assert.equal(calls.some(([type]) => type === "native-release"), false);
      const acknowledgements = calls.filter(([type]) => type === "ack").length; pageLeft = dom.window.innerWidth;
      await React.act(async () => root.render(React.createElement(module.exports.BrowserSurface, props))); await settleLayout();
      assert.equal(current.visible, false); assert.equal(calls.filter(([type]) => type === "ack").length, acknowledgements);
      pageLeft = 30; clip.style.overflowX = "hidden"; clip.getBoundingClientRect = () => ({ left: 30, right: 350, top: 100, bottom: 580 });
      await settleLayout(); assert.equal(calls.filter(([type]) => type === "ack").length, acknowledgements);
      await React.act(async () => { clip.style.overflowX = "visible"; }); await settleLayout();
      assert.equal(calls.filter(([type]) => type === "ack").length, acknowledgements + 1); assert.equal(current.visible, true);
      assert.equal(calls.findLast(([type]) => type === "ack")[1].stageBounds.width, 640); assert.equal(dom.window.document.querySelector('[role="alert"]'), null);
      pageLeft = dom.window.innerWidth; dom.window.dispatchEvent(new dom.window.Event("resize")); await settleLayout(); assert.equal(current.visible, false); assert.equal(calls.findLast(([type]) => type === "sync")[1].stageBounds.width, 0);
      pageLeft = 30; dom.window.dispatchEvent(new dom.window.Event("resize")); await settleLayout(); assert.equal(current.visible, true);
      await selectBrowserMenu(await openBrowserMenu(), "downloads");
      await settleLayout();
      const panel = dom.window.document.querySelector('[role="dialog"][aria-label="Downloads"]');
      assert.ok(panel); assert.ok(panel.textContent.includes("No downloads yet"));
      assert.equal(current.visible, false);
      await React.act(async () => [...panel.querySelectorAll('button')].find(button => button.textContent === "View all downloads").click());
      assert.ok(calls.some(([type, request]) => type === "download" && request.action === "search"));
      assert.ok(panel.querySelector('input[type="search"]'));
      await React.act(async () => panel.dispatchEvent(new dom.window.KeyboardEvent("keydown", { key: "Escape", bubbles: true })));
      await settleLayout(); assert.equal(current.visible, true);
      await checkClosedPage();
      return;
    }
    assert.equal(calls.findIndex(([type]) => type === "native-adopt") < calls.findIndex(([type]) => type === "ack"), true);
    assert.equal(layout.mountGeneration, 11); assert.equal(layout.viewportSequence, 1); assert.equal(layout.visible, true); assert.equal(layout.pageBounds.width, 640);
    const clip = dom.window.document.getElementById("root");
    clip.getBoundingClientRect = () => ({ x: 30, y: 100, left: 30, top: 100, right: 350, bottom: 580, width: 320, height: 480 });
    await React.act(async () => { clip.style.overflowX = "hidden"; }); await settleLayout();
    assert.equal(guestElement.style.width, "640px");
    assert.equal(current.visible, true, "actual DOM clip does not hide or resize the full page");
    await React.act(async () => { clip.setAttribute("inert", ""); }); await settleLayout();
    assert.equal(calls.at(-1)[1].visible, false);
    await React.act(async () => { clip.removeAttribute("inert"); clip.style.overflowX = ""; }); await settleLayout();
    await React.act(async () => dom.window.document.querySelector('button[aria-label="Reload page"]').click());
    assert.equal(calls.at(-1)[1].action, "reload");
    const address = dom.window.document.querySelector('input[aria-label="Browser address"]');
    await React.act(async () => address.focus());
    assert.equal(address.selectionStart, 0); assert.equal(address.selectionEnd, data.url.length);
    await React.act(async () => { address.focus(); Object.getOwnPropertyDescriptor(dom.window.HTMLInputElement.prototype, "value").set.call(address, ""); address.dispatchEvent(new dom.window.Event("input", { bubbles: true })); });
    assert.equal(address.value, "");
    await React.act(async () => address.dispatchEvent(new dom.window.KeyboardEvent("keydown", { key: "Escape", bubbles: true })));
    assert.equal(address.value, data.url);
    composer.focus();
    await React.act(async () => { for (const listener of listeners) listener({ type: "command", attachmentId: "another-view", hostGeneration: current.hostGeneration, mountGeneration: current.mountGeneration, command: "focus-address" }); });
    assert.equal(dom.window.document.activeElement, composer);
    await React.act(async () => { for (const listener of listeners) listener({ type: "command", attachmentId: current.attachmentId, hostGeneration: current.hostGeneration, mountGeneration: current.mountGeneration, command: "focus-address" }); });
    assert.equal(dom.window.document.activeElement, address);
    await React.act(async () => { Object.getOwnPropertyDescriptor(dom.window.HTMLInputElement.prototype, "value").set.call(address, "example.com"); address.dispatchEvent(new dom.window.Event("input", { bubbles: true })); });
    await React.act(async () => address.dispatchEvent(new dom.window.KeyboardEvent("keydown", { key: "Enter", bubbles: true })));
    assert.equal(calls.findLast(([type, request]) => type === "action" && request.action === "navigate")[1].params.url, "https://example.com/");
    const openFind = async () => { await React.act(async () => { for (const listener of listeners) listener({ type: "command", attachmentId: current.attachmentId, hostGeneration: current.hostGeneration, mountGeneration: current.mountGeneration, command: "find" }); }); await settleLayout(); };
    await openFind();
    const find = dom.window.document.querySelector('input[aria-label="Find in page"]');
    assert.equal(dom.window.getComputedStyle(find.form).position, "absolute");
    const enterQuery = value => React.act(async () => { Object.getOwnPropertyDescriptor(dom.window.HTMLInputElement.prototype, "value").set.call(find, value); find.dispatchEvent(new dom.window.Event("input", { bubbles: true })); });
    await enterQuery("needle");
    assert.equal(calls.findLast(([type, request]) => type === "action" && request.action === "find")[1].params.query, "needle", "typing searches without submitting");
    const key = (key, options = {}) => React.act(async () => find.dispatchEvent(new dom.window.KeyboardEvent("keydown", { key, bubbles: true, cancelable: true, ...options })));
    await key("Enter", { shiftKey: true }); assert.equal(calls.at(-1)[1].params.backwards, true);
    await key("Enter"); assert.equal(calls.at(-1)[1].params.backwards, false);
    const beforeComposition = calls.filter(([type]) => type === "action").length;
    await key("Enter", { isComposing: true }); assert.equal(calls.filter(([type]) => type === "action").length, beforeComposition);
    await React.act(async () => address.focus()); await openFind();
    assert.equal(dom.window.document.activeElement, find); assert.equal(find.selectionStart, 0); assert.equal(find.selectionEnd, 6);
    await enterQuery(""); assert.equal(calls.at(-1)[1].params.query, "", "clearing the query clears native highlights");
    await enterQuery("needle"); await key("Escape");
    assert.equal(dom.window.document.querySelector('input[aria-label="Find in page"]'), null);
    assert.equal(calls.findLast(([type, request]) => type === "action" && request.action === "find")[1].params.query, "");
    assert.equal(calls.at(-1)[1].action, "focus", "closing find returns focus to the exact browser page");
    const dialog = dom.window.document.createElement("div"); dialog.setAttribute("role", "dialog");
    await React.act(async () => dom.window.document.body.append(dialog)); await settleLayout();
    assert.equal(current.visible, true, "DOM menu/dialog z-order needs no WCV visibility workaround");
    await React.act(async () => dialog.remove()); await settleLayout();
    assert.equal(current.visible, true);
    const pendingAcks = [];
    holdAcknowledgement = (request) => new Promise((resolve) => pendingAcks.push({ request, resolve, snapshot: current }));
    const publishState = () => { for (const listener of listeners) listener({ type: "state", attachmentId: current.attachmentId, hostGeneration: current.hostGeneration, mountGeneration: current.mountGeneration, state: current }); };
    current = { ...current, presentationPhase: "attached", visible: false, modelViewport: { revision: 1, emulatedSize: { width: 1000, height: 800 } } };
    await React.act(async () => publishState()); await settleLayout();
    const scaled = pendingAcks[0].request;
    assert.equal(scaled.metricsRevision, 1); assert.equal(scaled.pageBounds.width, 1000); assert.equal(scaled.stageBounds.width, 640); assert.equal(scaled.scale, 0.6);
    assert.equal(guestElement.style.width, "1000px"); assert.equal(guestElement.style.transform, "scale(0.6)");
    const cursorLayer = dom.window.document.querySelector(".we-native-browser-cursor");
    assert.equal(cursorLayer.style.width, "1000px"); assert.equal(cursorLayer.style.transform, "scale(0.6)");
    assert.equal(guestElement.contains(cursorLayer), false, "model cursor is client DOM, never injected in the guest");
    assert.equal(guestElement.style.visibility, "hidden");
    current = { ...current, modelViewport: { ...current.modelViewport, revision: 2 } };
    await React.act(async () => publishState());
    await React.act(async () => pendingAcks[0].resolve({ ...pendingAcks[0].snapshot, viewportSequence: scaled.viewportSequence, presentationPhase: "ready", visible: true }));
    await settleLayout();
    assert.equal(guestElement.style.visibility, "hidden", "stale previous-metrics ACK must not reveal the guest");
    assert.equal(pendingAcks.length, 2);
    const fresh = pendingAcks[1];
    assert.equal(fresh.request.metricsRevision, 2); assert.equal(fresh.request.mountGeneration, scaled.mountGeneration);
    assert.ok(fresh.request.viewportSequence > scaled.viewportSequence);
    current = { ...current, viewportSequence: fresh.request.viewportSequence, presentationPhase: "ready", visible: true };
    await React.act(async () => fresh.resolve(current)); await settleLayout();
    assert.equal(guestElement.style.visibility, "visible");
    holdAcknowledgement = null;
    const ackCount = calls.filter(([type]) => type === "ack").length;
    current = { ...current, presentationPhase: "attached", visible: false };
    await React.act(async () => publishState()); await settleLayout();
    assert.equal(calls.filter(([type]) => type === "ack").length, ackCount + 1, "same geometry still requires ACK after native demotion");
    assert.equal(current.presentationPhase, "ready"); assert.equal(guestElement.style.visibility, "visible");
    current = { ...current, modelActive: true, modelTurnKey: "41", cursor: { moveSequence: 9, x: 580, y: 440, metricsRevision: 2, sourceMovementSeq: 42 } };
    await React.act(async () => publishState()); await settleLayout();
    assert.equal(calls.findLast(([kind]) => kind === "cursor")[1].moveSequence, 9);
    assert.equal(calls.findLast(([kind]) => kind === "cursor")[1].metricsRevision, 2);
    assert.equal(cursorLayer.style.transform, "scale(0.6)");
    current = { ...current, modelActive: false, cursor: null };
    await React.act(async () => publishState());
    assert.equal(Boolean(dom.window.document.querySelector(".we-browser-status")?.textContent.includes("Octopus is using this page")), false);
    const record = { id: "download-one", targetId: "target", filename: "report.pdf", url: "https://page.test/report.pdf", status: "in_progress", receivedBytes: 512, totalBytes: 1024, startedAtMs: 1000, updatedAtMs: 2000, fileExists: false, canCancel: true, canPause: true, canResume: false, userInitiated: true, sequence: 1 };
    downloads = { ...downloads, sequence: 1, downloads: [record] };
    await React.act(async () => root.render(React.createElement(module.exports.BrowserSurface, { ...props, surface: { ...props.surface, data: { ...data, downloads: { sequence: 1 } } } })));
    const downloadButton = dom.window.document.querySelector('button[aria-label="Downloads, 1 active download, 50% complete"]'); assert.ok(downloadButton);
    await React.act(async () => downloadButton.click());
    assert.equal(dom.window.document.querySelector(".we-download-list").textContent.includes("report.pdf"), true);
    await React.act(async () => dom.window.document.querySelector('button[aria-label="Pause download"]').click());
    assert.equal(calls.at(-1)[1].id, "download-one"); assert.equal(calls.at(-1)[1].action, "pause");
    assert.ok(dom.window.document.querySelector('button[aria-label="Resume download"]'));
    downloads = { ...downloads, sequence: 3, downloads: [{ ...record, status: "complete", fileExists: true, receivedBytes: 1024, canCancel: false, canPause: false, sequence: 3 }], unacknowledgedIds: [record.id], error: null };
    await React.act(async () => root.render(React.createElement(module.exports.BrowserSurface, { ...props, surface: { ...props.surface, data: { ...data, downloads: { sequence: 3 } } } })));
    assert.equal(calls.at(-1)[1].action, "acknowledge");
    assert.equal(dom.window.document.querySelector(".we-download-announcement").textContent, "report.pdf downloaded");
    await React.act(async () => dom.window.document.querySelector('button[aria-label="Open report.pdf"]').click());
    assert.equal(calls.at(-1)[1].action, "open"); assert.equal(calls.at(-1)[1].id, record.id);
    await checkClosedPage();
    await React.act(async () => root.render(React.createElement(module.exports.BrowserSurface, { ...props, active: false })));
    assert.equal(calls.at(-1)[0], "close");
    const preparations = calls.filter(([type]) => type === "prepare").length;
    await React.act(async () => root.render(React.createElement(module.exports.BrowserSurface, { ...props, surface: { ...props.surface, id: "room:downloads", protocol: "browser.native.downloads.v1", data: { ...data, downloads: { sequence: 3 } } } })));
    assert.equal(dom.window.document.querySelector("webview"), null);
    assert.equal(calls.filter(([type]) => type === "prepare").length, preparations, "history survives with no page creation or attachment");
    assert.ok(dom.window.document.querySelector('button[aria-label="Open report.pdf"]'));
    const beforeUnavailable = calls.length;
    const unavailable = { ...props.surface, mode: "evidence", status: "unavailable", readOnly: true, data: { reason_code: "host_unavailable", detail: "Native browser host generation is unavailable", last_known_url: "https://page.test/", last_known_title: "Page" } };
    await React.act(async () => root.render(React.createElement(module.exports.BrowserSurface, { ...props, surface: unavailable })));
    assert.equal(calls.length, beforeUnavailable, "unavailable evidence never invokes the native host");
    assert.equal(dom.window.document.querySelector("webview, iframe, canvas"), null);
    assert.equal(dom.window.document.querySelectorAll('[role="toolbar"] button:disabled').length, 5);
    const lastAddress = dom.window.document.querySelector('[aria-label="Last known browser address"]');
    assert.equal(lastAddress.value, "https://page.test/"); assert.ok(lastAddress.readOnly); assert.equal(lastAddress.disabled, false);
    assert.match(dom.window.document.querySelector('[role="status"]').textContent, /Browser session disconnected/);
    assert.doesNotMatch(dom.window.document.querySelector('[role="status"]').textContent, /host generation|ready/);
    assert.equal(dom.window.document.querySelector("details").open, false);
    assert.match(dom.window.document.querySelector("details").textContent, /Native browser host generation/);
  } finally {
    await React.act(async () => root.unmount()); dom.window.close();
    for (const [key, descriptor] of Object.entries(saved)) { if (descriptor) Object.defineProperty(globalThis, key, descriptor); else delete globalThis[key]; }
  }
}

async function downloadHistoryChecks() {
  const { createBrowserDownloadHistory, browserToolDescriptorPin } = require('../electron/browserSurface.cjs');
  const evidence = browserToolFixture('browser.download_wait'), clone = evidence.descriptor;
  let currentDescriptor = structuredClone(clone);
  let home = '/download-home', swapHome = false; const accepted = [], reads = [];
  const owner = { room: { worker_environment_id: 'room', run_id: 'run', door: 'door', lifecycle: 'failed', formation_pin: evidence.formation_pin, opened_at_movement_seq: 10 },
    surface: { id: 'room:downloads', protocol: 'browser.native.downloads.v1', data: { host_generation: 'new-history-generation' } } };
  const row = seq => ({ movement_seq: seq, run_id: 'run', from: { kind: 'interface', id: 'door' }, label: 'tool.result', content_hash: 'a'.repeat(64), artifact_id: `tool.result:${'a'.repeat(64)}` });
  owner.room.tools = [clone.id]; owner.room.tool_descriptor_hashes = { [clone.id]: evidence.tool_descriptor_hash };
  const body = { result: clone.id, status: 'completed', worker_environment_id: 'room', source_movement_seq: 12, target_id: 'closed-target',
    arm: { formation_pin: evidence.formation_pin }, host_generation: 'generation', browser_context_id: 'context', artifact: { label: 'browser.download', payload_path: '/admitted-file', payload_content_hash: 'b'.repeat(64) } };
  const history = createBrowserDownloadHistory({ homePath: () => home, requireAccess() {}, currentRoom: async (_owner, projection) => { assert.equal('target_id' in projection, false); return owner.room; },
    readToolDescriptor: async id => { assert.equal(id, clone.id); return structuredClone(currentDescriptor); },
    readActivity: async (_run, after) => { reads.push(after); return after < 11 ? { activity: [], latest_seq: 11, has_newer: true }
      : { activity: [row(13)], latest_seq: 13, has_newer: false }; },
    readArtifact: async () => { if (swapHome) home = '/another-home'; return { payload: { tool: clone.id, ok: true, body } }; },
    acceptArtifact: async (projection, value, validateCurrent) => { await validateCurrent(); accepted.push([projection, value]); },
  });
  const turn = { run_id: 'run', turn_open: false, last_movement_seq: 13 };
  await history.reconcile([owner], [turn]); assert.deepEqual(reads, [9, 11]); assert.equal(accepted.length, 1);
  assert.equal(accepted[0][1].target_id, 'closed-target'); assert.equal('browser_context_id' in accepted[0][0], false);
  assert.equal(accepted[0][0].host_generation, 'new-history-generation'); assert.equal(accepted[0][1].host_generation, 'generation');
  await history.reconcile([owner], [turn]); assert.equal(accepted.length, 1);
  for (const altered of [{ ...clone, handler_config: { operation: 'snapshot' } }, { ...clone, interface: 'another-exact-door' }]) {
    history.revokeAll(); currentDescriptor = altered; owner.room.tool_descriptor_hashes[clone.id] = browserToolDescriptorPin(altered);
    await assert.rejects(history.reconcile([owner], [turn]), /exact declared tool operation, door and formation pin/);
    assert.equal(accepted.length, 1, 'a pinned descriptor with another operation or door cannot collect this download');
  }
  currentDescriptor = structuredClone(clone); owner.room.tool_descriptor_hashes[clone.id] = 'f'.repeat(64); history.revokeAll();
  await assert.rejects(history.reconcile([owner], [turn]), /exact declared tool operation, door and formation pin/);
  assert.equal(accepted.length, 1); owner.room.tool_descriptor_hashes[clone.id] = evidence.tool_descriptor_hash;
  history.revokeAll(); swapHome = true; await assert.rejects(history.reconcile([owner], [turn]), /Home changed/); assert.equal(accepted.length, 1);
  history.stop();
}

async function expectNavigationChecks() {
  const base = path.resolve(__dirname, '../..');
  const sdk = JSON.parse(fs.readFileSync(path.join(base, 'registry/artifact/artifact-code-mode-browser-prelude/artifact.json'), 'utf8'));
  const descriptor = id => JSON.parse(fs.readFileSync(path.join(base, 'registry/tool', id, 'tool.json'), 'utf8'));
  const request = descriptor('browser.wait'), complete = descriptor('browser.wait_complete');
  assert.deepEqual(sdk.tool_roles.wait_complete, { tool: complete.id, interface: complete.interface });
  assert.equal(request.interface, complete.interface); assert.notEqual(request.process.bin, complete.process.bin);
  const Ajv = require('ajv'), ajv = new Ajv({ strict: false });
  const checkStart = ajv.compile(request.args_schema), checkComplete = ajv.compile(complete.args_schema);
  const bindings = { tabs: { name: 'browser_tabs__sdk_fixture', tool: 'browser.tabs__sdk_fixture' },
    wait: { name: 'browser_wait__sdk_fixture', tool: 'browser.wait__sdk_fixture' }, wait_complete: { name: 'browser_wait_complete__sdk_fixture', tool: 'browser.wait_complete__sdk_fixture' } };
  const defer = () => { let resolve; const promise = new Promise(done => { resolve = done; }); return { promise, resolve }; };
  const flush = () => new Promise(resolve => setImmediate(resolve));
  const factory = (roles, tools) => vm.runInNewContext(sdk.module_source + '\ncreateBrowser(bindings)', { bindings: roles, tools });
  const handle = { wait_id: 'a'.repeat(32), host_generation: 'b'.repeat(32), worker_environment_id: 'room', formation_pin: 'c'.repeat(64),
    browser_context_id: 'CONTEXT', target_id: 'PAGE', debugger_epoch: 2, start_source_movement_seq: 102, start_artifact_content_hash: 'd'.repeat(64), condition_digest: 'e'.repeat(64) };
  const modes = ['pending', 'already_loaded', 'invalid_wait', 'start_refused', 'sync_throw', 'async_throw', 'wait_refused', 'changed_waiter', 'changed_target'];
  for (const mode of modes) {
    const calls = [], start = defer(), end = defer(), action = defer(), value = { mode }, failure = new Error(mode);
    const refused = { tool: mode === 'start_refused' ? bindings.wait.tool : bindings.wait_complete.tool, ok: false, error: { code: 'refused', message: mode } };
    const reply = (role, body) => ({ tool: bindings[role].tool, ok: true, body });
    const tools = {
      [bindings.tabs.name]: async () => reply('tabs', { worker_environment_id: 'room', browser_context_id: 'CONTEXT', current_target_id: 'PAGE', tabs: [{ target_id: 'PAGE', browser_context_id: 'CONTEXT' }] }),
      [bindings.wait.name]: args => { calls.push(['start', args]); assert.equal(checkStart(args), true, JSON.stringify(checkStart.errors)); return start.promise; },
      [bindings.wait_complete.name]: args => { calls.push(['complete', args]); assert.equal(checkComplete(args), true, JSON.stringify(checkComplete.errors)); assert.deepEqual(JSON.parse(JSON.stringify(args)), { waiter: handle }); return end.promise; },
    };
    const tab = await factory(bindings, tools).tabs.get('PAGE'); let outcome;
    const options = mode === 'already_loaded' ? {} : { url: 'https://example.test/next*', timeoutMs: mode === 'invalid_wait' ? -1 : 1234, waitUntil: 'commit' };
    const result = tab.playwright.expectNavigation(() => { calls.push(['action']); if (mode === 'sync_throw') throw failure;
      return mode === 'async_throw' ? Promise.reject(failure) : mode === 'pending' ? action.promise : value; }, options);
    const settled = result.then(value => { outcome = { value }; }, error => { outcome = { error }; });
    assert.equal(calls.filter(row => row[0] === 'action').length, 1);
    assert.deepEqual(calls.map(row => row[0]), mode === 'invalid_wait' ? ['action'] : ['start', 'action']);
    if (mode !== 'invalid_wait') assert.deepEqual(JSON.parse(JSON.stringify(calls[0][1])), mode === 'already_loaded' ? { phase: 'start', target_id: 'PAGE', load_state: 'load' }
      : { phase: 'start', target_id: 'PAGE', timeout_ms: 1234, url_pattern: options.url, wait_until: 'commit' });
    await flush();
    if (['sync_throw', 'async_throw'].includes(mode)) assert.equal(outcome.error, failure);
    else if (mode === 'invalid_wait') assert.equal(outcome.error.name, 'TypeError'); else assert.equal(outcome, undefined);
    start.resolve(mode === 'start_refused' ? refused : reply('wait', { waiter: { ...handle, ...(mode === 'changed_target' ? { target_id: 'OTHER' } : {}) }, status: mode === 'already_loaded' ? 'satisfied' : 'pending', effective_timeout_ms: 1234 }));
    await flush();
    assert.equal(calls.filter(row => row[0] === 'complete').length, ['invalid_wait', 'start_refused', 'changed_target'].includes(mode) ? 0 : 1);
    const observation = mode === 'already_loaded' ? { state: 'load', match_event: 'ready_state' } : { url: 'https://example.test/next', wait_until: 'commit', match_event: 'frame_navigated' };
    end.resolve(['sync_throw', 'async_throw', 'wait_refused'].includes(mode) ? refused : reply('wait_complete', { waiter: { ...handle, ...(mode === 'changed_waiter' ? { host_generation: 'f'.repeat(32) } : {}) }, status: 'satisfied', observation, completion_source_movement_seq: 104 }));
    await flush(); if (mode === 'pending') { assert.equal(outcome, undefined); action.resolve(value); }
    await settled; await flush();
    if (['sync_throw', 'async_throw'].includes(mode)) assert.equal(outcome.error, failure);
    else if (['start_refused', 'wait_refused'].includes(mode)) assert.equal(outcome.error.result, refused);
    else if (['changed_waiter', 'changed_target'].includes(mode)) assert.match(outcome.error.message, /changed/);
    else if (mode !== 'invalid_wait') assert.equal(outcome.value, value);
    assert.equal(calls.filter(row => row[0] === 'action').length, 1); assert.equal(calls.some(row => row[1]?.phase === 'cancel'), false);
    const partial = { ...bindings }; delete partial.wait_complete;
    assert.equal((await factory(partial, tools).tabs.get('PAGE')).playwright.expectNavigation, undefined);
  }
  console.log(JSON.stringify({ check: 'expectNavigation', scenarios: modes.length, scope: 'pure declared SDK; no admitted CodeMode or native acceptance' }));
  for (const previous of ['CONTEXT', 'unproven']) {
    const roles = { tabs: { name: 'tabs', tool: 'tabs' }, tab_new: { name: 'tab_new', tool: 'tab_new' }, navigate: { name: 'navigate', tool: 'navigate' } }; let context = 'CONTEXT', target = 'PAGE'; const reply = (tool, body) => ({ tool, ok: true, body });
    const browser = factory(roles, { tabs: async () => reply('tabs', { browser_context_id: context, current_target_id: target, tabs: [{ target_id: target, browser_context_id: context }] }), tab_new: async () => { context = 'FRESH'; target = 'NEW'; return reply('tab_new', { target_id: target, browser_context_id: context, previous_context: { browser_context_id: previous } }); } }); const old = await browser.tabs.get('PAGE');
    if (previous === 'unproven') await assert.rejects(browser.tabs.new(), /retained context/); else { assert.equal((await browser.tabs.new()).id, 'NEW'); assert.equal((await browser.tabs.get('NEW')).id, 'NEW'); await assert.rejects(browser.tabs.get(old.id)); await assert.rejects(old.goto('https://old.test/'), /previous browser context/); }
  }
}

if (process.argv[2] === '--expect-navigation') expectNavigationChecks().catch(error => { console.error(error); process.exitCode = 1; });
else adapterChecks().then(startupChecks).then(browserPreviewChecks).then(downloadHistoryChecks).then(() => rendererChecks("owned")).then(() => rendererChecks("standard")).then(() => console.log("Native browser startup, presentation adapter and both renderer contracts passed (injected host; no native acceptance)."));
