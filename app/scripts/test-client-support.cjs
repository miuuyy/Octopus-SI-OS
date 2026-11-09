const assert = require("node:assert/strict");
const fs = require("node:fs/promises");
const os = require("node:os");
const path = require("node:path");
const { test } = require("node:test");
const { collectSupportReport, supportReportText, supportIssueUrl } = require("../electron/clientSupportReport.cjs");

test("reports disclose only selected technical evidence; error text requires explicit inclusion", async () => {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "octopus-support-"));
  try {
    await fs.mkdir(path.join(root, "logs"));
    await fs.mkdir(path.join(root, "setup-state/operations"), { recursive: true });
    const operation_id = "workspace_setup_test";
    await fs.writeFile(path.join(root, "setup-state/current.json"), JSON.stringify({ operation_id, plan_digest: "same" }));
    await fs.writeFile(path.join(root, `setup-state/operations/${operation_id}.json`), JSON.stringify({ operation_id, plan_digest: "same", plan: { secret: "PRIVATE_PLAN" }, steps: [{ step: "provider", state: "pending" }], last_error: { step: "provider", message: "PRIVATE_ERROR" } }));
    await fs.writeFile(path.join(root, "logs/octopus-client.jsonl"), [
      JSON.stringify({ at: "2026-10-26T00:00:00.000Z", event: "setup.progress", detail: { step: "provider", phase: "verifying", percent: 96, operationId: "PRIVATE_ID" } }),
      "broken line",
      JSON.stringify({ event: "PRIVATE_EVENT", level: "error", detail: { message: "PRIVATE_ERROR", profile: "PRIVATE_PROFILE", stack: "PRIVATE_STACK" } }),
    ].join("\n"));
    const input = { clientRoot: root, profile: "live", application: { version: "0.1.0", revision: null, platform: "darwin", arch: "arm64" }, runtime: { phase: "ready", ready: true, home: "PRIVATE_HOME", launchEnv: { secret: "PRIVATE_ENV" } } };
    const safe = await collectSupportReport(input);
    assert.doesNotMatch(JSON.stringify(safe), /PRIVATE_/);
    assert.equal(safe.setup.journal.steps[0].step, "provider");
    assert.equal(safe.client_logs.sources[1].malformed_lines, 1);
    const detailed = await collectSupportReport({ ...input, includeDetails: true });
    assert.match(supportReportText(detailed, "My description"), /PRIVATE_ERROR/);
    assert.doesNotMatch(JSON.stringify(detailed), /PRIVATE_(PLAN|ENV|HOME|STACK|ID|PROFILE)/);
    assert.doesNotMatch(supportIssueUrl(detailed, "My description"), /PRIVATE_ERROR/);
    for (const profile of [null, "lab"]) {
      const report = await collectSupportReport({ ...input, profile, clientRoot: "/does-not-exist", includeDetails: false });
      assert.equal(report.setup, undefined);
      assert.equal(report.client_logs, undefined);
      assert.equal(report.runtime, undefined);
    }
    await fs.unlink(path.join(root, "logs/octopus-client.jsonl"));
    await fs.symlink(path.join(root, "setup-state/current.json"), path.join(root, "logs/octopus-client.jsonl"));
    assert.equal((await collectSupportReport(input)).client_logs.sources[1].state, "symlink_refused");
    await fs.unlink(path.join(root, "logs/octopus-client.jsonl"));
    await fs.writeFile(path.join(root, "logs/octopus-client.jsonl"), "x".repeat(300_000) + "\n" + JSON.stringify({ event: "setup.failed", level: "error" }));
    assert.equal((await collectSupportReport(input)).client_logs.sources[1].truncated, true);
  } finally { await fs.rm(root, { recursive: true, force: true }); }
});

test("runtime reports retain the failure, exact call coordinates and authored context without exporting content or touching a preserved Home", async () => {
  const { blake3 } = require("@noble/hashes/blake3");
  const home = await fs.mkdtemp(path.join(os.tmpdir(), "octopus-old-support-"));
  const hash = bytes => Buffer.from(blake3(bytes)).toString("hex");
  const write = async (relative, bytes) => { const file = path.join(home, relative); await fs.mkdir(path.dirname(file), { recursive: true }); await fs.writeFile(file, bytes); };
  const store = async (value, directory = "ledger/blobs") => { const bytes = Buffer.from(JSON.stringify(value)), digest = hash(bytes); await write(`${directory}/${digest}`, bytes); return digest; };
  const artifact = async (label, value) => { const payload = await store(value); const content = await store({ label, payload_path: `/PRIVATE_ORIGINAL_HOME/ledger/blobs/${payload}`, payload_content_hash: payload }); return { id: `${label}:${content}`, content }; };
  try {
    const run = "PRIVATE_RUN", provider = "PRIVATE_PROVIDER", door = "PRIVATE_DOOR";
    const continuity = { schema_version: 1, run, provider_interface: provider, thread_id: "PRIVATE_THREAD", thread_shape_fingerprint: "PRIVATE_SHAPE", static_context_fingerprint: "PRIVATE_STATIC", context_generation: 7, pending: { kind: "dynamic_tool", turn_id: "PRIVATE_TURN", call_id: "PRIVATE_CALL", tool: "PRIVATE_TOOL", native_operation: { secret: "PRIVATE_OPERATION" } } };
    const call = await artifact("tool.call", { arguments: { command: "PRIVATE_COMMAND" }, correlation_id: "PRIVATE_CALL", codex_continuity: continuity });
    const result = await artifact("workspace.exec.result", { correlation_id: "PRIVATE_CALL", ok: false, body: "PRIVATE_RESULT", error: { code: "tool_process_failure", message: "PRIVATE_ERROR" } });
    const parent = await artifact("turn.agent_message", { message: "PRIVATE_CONVERSATION", session_context: { schema_version: 3, artifacts: [{ label: "tool.call", payload: { arguments: "PRIVATE_COMMAND", correlation_id: "PRIVATE_CALL", codex_continuity: continuity }, source_artifact: call, source_movement_seq: 1 }] } });
    const terminal = await artifact("turn.error", { message: "PRIVATE_ERROR", codex_continuity: continuity, session_context: { schema_version: 3, parent, artifacts: [{ payload: { message: "PRIVATE_CONVERSATION", codex_continuity: continuity }, source_artifact: call, source_movement_seq: 1 }] } });
    const unrelated = await artifact("operator.message", { text: "PRIVATE_PROMPT", credential: "PRIVATE_CREDENTIAL" });
    const record = { id: run, goal: "PRIVATE_GOAL", status: "running", active_checkpoints: ["PRIVATE_CHECKPOINT"], checkpoints: [{ id: "PRIVATE_CHECKPOINT", epoch: 2, interface_binding: { interface: provider }, artifact_bindings: ["PRIVATE_INSTRUCTIONS"], required_verified_artifacts: [{ artifact_label: "tool.result", verifier: "PRIVATE_VERIFIER", mark: "PRIVATE_MARK", params: { secret: "PRIVATE_PARAMS" } }] }], interface_state: { [provider]: { context_generation: 4, pending_handoff: { source_movement_seq: 4, terminal: false, dispatch_fenced: true, items: [] }, model: "PRIVATE_MODEL", auth: "PRIVATE_AUTH" } } };
    const recordHash = await store(record), refsHash = await store([call], "ledger/metadata");
    const runtime = { run, interface: provider, checkpoint: "PRIVATE_CHECKPOINT", epoch: 2, run_record_hash: recordHash };
    const rows = [call, call, result, result, terminal, unrelated].map((artifact, index) => ({ seq: index + 1, kind: index === 1 ? "InterfaceArrival" : "InterfaceOutbound", recorded_at_unix_ms: 1_700_000_000_000 + index, from: { Interface: index < 2 ? provider : door }, to: { Interface: provider }, artifact, invocation_metadata: { runtime, source_movement_seq: index === 4 ? 4 : null, artifact_binding_refs_content: refsHash, grant: { secret: "PRIVATE_GRANT" } } }));
    await write("ledger/movements.jsonl", rows.map(JSON.stringify).join("\n") + "\n");
    await write(`state/runs/${run}.json`, JSON.stringify(record));
    await write(`interfaces/${provider}/handoff-standing/${hash(Buffer.from(run))}.jsonl`, JSON.stringify({ schema_version: 1, boundary: "PRIVATE_BOUNDARY", source_movement_seq: 4, context_generations: 2, items: [] }) + "\n");
    await write(`interfaces/${provider}/interface-life.stderr.1700000000000.123.1.log`, "PRIVATE_STDERR\n");
    await write("state/runtime-instance/output.log", "PRIVATE_RUNTIME_LOG\n");
    await write(".env", "PRIVATE_CREDENTIAL");
    await write("client/logs/octopus-client.jsonl", [JSON.stringify({ event: "renderer.toast", level: "error", detail: { message: "First failure (movement seq 4): PRIVATE_ERROR" } }), ...Array.from({ length: 100 }, () => JSON.stringify({ event: "setup.progress", level: "info", detail: { step: "workspace", phase: "verifying", percent: 90 } }))].join("\n"));
    const input = { home, clientRoot: path.join(home, "client"), application: { version: "test" }, profile: "live", runtime: { home, phase: "ready", ready: true, interfaceFailures: [{ interfaceId: provider, message: "PRIVATE_LIFE_ERROR" }] } };
    const before = await fs.stat(path.join(home, "ledger/movements.jsonl"));
    const safe = await collectSupportReport(input);
    assert.equal(safe.schema_version, 2);
    assert.doesNotMatch(JSON.stringify(safe), /PRIVATE_/);
    assert.equal(safe.client_logs.events[0].source_movement_seq, 4, "setup progress cannot evict the first failure");
    const evidence = safe.runtime_evidence;
    assert.deepEqual(evidence.ledger.missing_requested_sources, []);
    assert.equal(evidence.interface_failures.length, 1, "a ready runtime can contain failed lives");
    const otherHome = await collectSupportReport({ ...input, includeDetails: true, runtime: { ...input.runtime, home: "/another-home", error: "PRIVATE_OTHER_HOME" } });
    assert.equal(otherHome.runtime.phase, "not_observed"); assert.deepEqual(otherHome.runtime_evidence.interface_failures, []);
    assert.doesNotMatch(JSON.stringify(otherHome), /PRIVATE_OTHER_HOME/);
    assert.ok(evidence.context_bindings.some(binding => binding.refs[0].artifact === evidence.artifacts.find(item => item.artifact.label === "tool.call").artifact.artifact));
    assert.equal(evidence.artifacts.find(item => item.artifact.label === "turn.error").payload.continuity.context_generation, 7);
    assert.equal(evidence.artifacts.find(item => item.artifact.label === "turn.agent_message").payload.session_context.entries[0].correlation, evidence.artifacts.find(item => item.artifact.label === "tool.call").payload.correlation);
    assert.equal(evidence.records.find(item => item.current).state.interface_state[0].context_generation, 4);
    assert.equal(evidence.interfaces.find(item => item.standing_handoffs.length).standing_handoffs[0].entries[0].context_generations, 2);
    const detailed = await collectSupportReport({ ...input, includeDetails: true });
    assert.match(JSON.stringify(detailed), /PRIVATE_STDERR/);
    assert.match(JSON.stringify(detailed), /PRIVATE_LIFE_ERROR/);
    assert.doesNotMatch(JSON.stringify(detailed), /PRIVATE_(COMMAND|RESULT|CONVERSATION|PROMPT|CREDENTIAL|AUTH|GRANT|GOAL|PARAMS|OPERATION|ORIGINAL_HOME)/);
    assert.ok(Buffer.byteLength(JSON.stringify(detailed)) < 2 * 1024 * 1024);
    assert.equal((await fs.stat(path.join(home, "ledger/movements.jsonl"))).mtimeMs, before.mtimeMs);
    assert.equal(await fs.readFile(path.join(home, `state/runs/${run}.json`), "utf8"), JSON.stringify(record));
    for (const profile of [null, "lab"]) assert.equal((await collectSupportReport({ ...input, home: "/unreadable", profile })).runtime_evidence, undefined);
    const noisyRows = [...rows];
    for (let batch = 0; batch < 4; batch++) {
      const refs = await Promise.all(Array.from({ length: 32 }, (_, index) => artifact("turn.reasoning", { message: "PRIVATE_CONVERSATION", noise: batch * 32 + index })));
      noisyRows.push({ ...rows[0], seq: 7 + batch, invocation_metadata: { runtime, artifact_binding_refs_content: await store(refs, "ledger/metadata") } });
    }
    await write("ledger/movements.jsonl", noisyRows.map(JSON.stringify).join("\n") + "\n");
    const bounded = (await collectSupportReport({ ...input, includeDetails: true })).runtime_evidence;
    assert.ok(bounded.omitted_artifacts > 0);
    assert.equal(bounded.artifacts.find(item => item.artifact.label === "turn.error").source, "observed", "binding volume cannot evict the selected failure");
    assert.doesNotMatch(JSON.stringify(bounded), /PRIVATE_CONVERSATION/);
    await write("ledger/movements.jsonl", rows.map(JSON.stringify).join("\n") + "\n");
    const payloadHash = JSON.parse(await fs.readFile(path.join(home, "ledger/blobs", terminal.content))).payload_content_hash;
    await fs.unlink(path.join(home, "ledger/blobs", payloadHash));
    await fs.symlink(path.join(home, ".env"), path.join(home, "ledger/blobs", payloadHash));
    assert.equal((await collectSupportReport(input)).runtime_evidence.artifacts.find(item => item.artifact.label === "turn.error").source, "symlink_refused");
    await fs.unlink(path.join(home, "ledger/blobs", payloadHash));
    await write(`ledger/blobs/${payloadHash}`, "{}");
    assert.equal((await collectSupportReport(input)).runtime_evidence.artifacts.find(item => item.artifact.label === "turn.error").source, "digest_mismatch");
    await write("ledger/movements.jsonl", JSON.stringify(rows[0]) + "\n{broken\n{\"seq\":2");
    const incomplete = (await collectSupportReport(input)).runtime_evidence.ledger;
    assert.equal(incomplete.malformed_rows, 1); assert.equal(incomplete.incomplete_tail, true); assert.deepEqual(incomplete.missing_requested_sources, [4]);
  } finally { await fs.rm(home, { recursive: true, force: true }); }
});
