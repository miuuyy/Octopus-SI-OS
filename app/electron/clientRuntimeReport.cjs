const path = require("node:path");
const { blake3 } = require("@noble/hashes/blake3");
const { readBounded, supportEntries, sourceFailure } = require("./clientSupportFiles.cjs");

const LABELS = new Set(["operator.input", "operator.message", "operator.turn.control", "tool.call", "tool.result", "turn.error", "turn.completed", "turn.final", "turn.stream", "turn.dynamic_tool_call", "turn.interrupted.result", "turn.control.acknowledged", "turn.control.failed", "turn.input.acknowledged", "interface.artifact.output", "interface.context.updated", "turn.context.compaction", "turn.agent_message", "turn.reasoning", "turn.plan.update", "operator.approval.request", "operator.elicitation.request", "operator.user_input.request", "provider.transport.dispatch", "provider.operation.error", "workspace.exec.result", "workspace.write_stdin.result", "workspace.fs.result", "memory.capture.error"]);
const PAYLOAD_LABELS = new Set([...LABELS].filter(label => !["operator.input", "operator.message", "turn.stream", "provider.transport.dispatch"].includes(label)));
const ERROR_LABELS = new Set(["turn.error", "turn.control.failed", "provider.operation.error", "memory.capture.error"]);
const KINDS = new Set(["ArtifactMoved", "MarkOutcomeAdmitted", "VerificationFailed", "DeliveryRefused", "InterfaceArrival", "InterfaceOutbound"]);
const REASONS = new Set(["no_binding", "policy", "unknown_interface", "checkpoint_spent", "duplicate_artifact", "verification_failed", "stale_epoch", "registry_unstable", "malformed"]);
const CODES = new Set(["transport_turn_interrupted", "tool_invocation_refused", "tool_process_failure", "tool_effect_indeterminate", "indeterminate_external_effect", "protocol_error", "provider_context_invalid", "checkpoint_spent"]);
const SAFE_SEGMENT = /^[A-Za-z0-9][A-Za-z0-9._-]{0,191}$/;
const HASH = /^[a-f0-9]{64}$/;
const LIMITS = { ledger_bytes: 32 * 1024 * 1024, blob_bytes: 32 * 1024 * 1024, movements: 240, artifacts: 96, context_bindings: 24, records: 24, runs: 8, interfaces: 16, stderr_files: 4, stderr_bytes: 8192 };
const object = value => value && typeof value === "object" && !Array.isArray(value) ? value : {};
const array = value => Array.isArray(value) ? value : [];
const integer = value => Number.isSafeInteger(value) && value >= 0 ? value : null;
const firstAndLast = (values, first, last) => [...new Set([...values.slice(0, first), ...values.slice(-last)])];
const kind = row => typeof row.kind === "string" ? row.kind : Object.keys(object(row.kind))[0];
const artifactLabel = ref => typeof ref?.id === "string" && HASH.test(ref?.content) && ref.id.endsWith(`:${ref.content}`) ? ref.id.slice(0, -65) : null;
const sessionEntries = value => firstAndLast(array(value.session_context?.artifacts), 4, 20);

function projection(includeDetails) {
  const identities = new Map();
  function id(type, value) {
    if (typeof value !== "string" || !value) return null;
    if (!identities.has(type)) identities.set(type, new Map());
    const table = identities.get(type);
    if (!table.has(value)) table.set(value, `${type}_${table.size + 1}`);
    return table.get(value);
  }
  const label = value => LABELS.has(value) ? value : id("label", value);
  const ref = value => value && typeof value.id === "string" ? { artifact: id("artifact", value.id), content: id("content", value.content), label: label(artifactLabel(value)) } : null;
  const errorText = value => includeDetails && typeof value === "string" ? { message: value.slice(0, 3000), text_truncated: value.length > 3000 } : {};
  function coordinates(value) {
    if (!value) return null;
    return { run: id("run", value.run), interface: id("interface", value.interface), checkpoint: id("checkpoint", value.checkpoint), epoch: integer(value.epoch), record: id("record", value.run_record_hash) };
  }
  function pending(value) {
    if (!value) return null;
    return { kind: ["dynamic_tool", "interface_operation", "operator"].includes(value.kind) ? value.kind : "unknown", turn: id("turn", value.turn_id), call: id("call", value.call_id), request: id("request", value.request_key), tool: id("tool", value.tool), interface: id("interface", value.interface), operation: id("operation", value.operation), output_label: label(value.output_label), native_operation: Boolean(value.native_operation) };
  }
  function continuity(value) {
    if (!value) return null;
    return { schema_version: integer(value.schema_version), run: id("run", value.run), provider: id("interface", value.provider_interface), thread: id("thread", value.thread_id), shape: id("shape", value.thread_shape_fingerprint), static_context: id("static", value.static_context_fingerprint), context_generation: integer(value.context_generation), turn_input: ref(value.turn_input), pending: pending(value.pending), interrupted_pending: pending(value.interrupted_pending), parallel_pending: Object.values(object(value.parallel_pending)).slice(0, 16).map(item => pending(item.pending)), parallel_pending_omitted: Math.max(0, Object.keys(object(value.parallel_pending)).length - 16), native_terminal: value.native_terminal ? { thread: id("thread", value.native_terminal.thread_id), turn: id("turn", value.native_terminal.turn?.id), status: ["completed", "failed", "interrupted", "inProgress"].includes(value.native_terminal.turn?.status) ? value.native_terminal.turn.status : "unknown" } : null, context_refresh: Boolean(value.context_refresh), native_history_retirement: Boolean(value.native_history_retirement) };
  }
  function technicalPayload(value, sourceLabel) {
    return { ok: typeof value.ok === "boolean" ? value.ok : null, will_retry: typeof value.will_retry === "boolean" ? value.will_retry : null, source_movement_seq: integer(value.source_movement_seq ?? value.additional_details?.source_movement_seq), correlation: id("call", value.correlation_id), control: id("control", value.control_id), thread: id("thread", value.thread_id), turn: id("turn", value.turn_id), source_artifact: ref(value.source_artifact), returned_to_model: typeof value.returned_to_model === "boolean" ? value.returned_to_model : null,
      continuity: continuity(value.codex_continuity ?? value.provider_continuity), native_terminal_source_continuity: continuity(value.native_terminal_source_continuity),
      error: { code: CODES.has(value.error?.code ?? value.additional_details?.code) ? value.error?.code ?? value.additional_details.code : id("error", value.error?.code ?? value.additional_details?.code), ...errorText(ERROR_LABELS.has(sourceLabel) ? value.message ?? value.error?.message : value.error?.message) } };
  }
  function payload(value, sourceLabel) {
    const result = technicalPayload(value, sourceLabel);
    const entries = array(value.session_context?.artifacts);
    if (value.session_context) result.session_context = { parent: ref(value.session_context.parent), entries: sessionEntries(value).map(entry => ({ ...technicalPayload(object(entry.payload), artifactLabel(entry.source_artifact)), artifact: ref(entry.source_artifact), source_movement_seq: integer(entry.source_movement_seq) })), omitted_entries: entries.length - sessionEntries(value).length };
    return result;
  }
  function checkpoint(value) {
    return { checkpoint: id("checkpoint", value.id), epoch: integer(value.epoch), interface: id("interface", value.interface_binding?.interface), outbound: array(value.outbound_interface_bindings).slice(0, 16).map(binding => id("interface", binding.interface)), required: array(value.required_verified_artifacts).slice(0, 16).map(item => ({ label: label(item.artifact_label), verifier: id("verifier", item.verifier), mark: id("mark", item.mark) })) };
  }
  function handoff(value) {
    if (!value) return null;
    return { source_movement_seq: integer(value.source_movement_seq), terminal: value.terminal === true, deferred_publication: value.deferred_publication === true, source_without_context: value.source_without_context === true, dispatch_fenced: value.dispatch_fenced === true, settlement_of: ref(value.settlement_of), continuation_of: ref(value.continuation_of), context_publication: array(value.context_publication).slice(0, 16).map(value => id("artifact", value)), rebase: array(value.rebase).slice(0, 16).map(ref), items: [...array(value.standing), ...array(value.items)].slice(-24).map(item => ({ artifact: ref({ id: item.artifact_id, content: item.artifact_content_hash }), ...checkpoint(object(item.checkpoint)) })), omitted_items: Math.max(0, array(value.standing).length + array(value.items).length - 24) };
  }
  function run(value) {
    const active = array(value.active_checkpoints);
    const states = Object.entries(object(value.interface_state));
    return { run: id("run", value.id), status: ["idle", "running"].includes(value.status) ? value.status : "unknown", active_checkpoints: active.slice(0, 64).map(value => id("checkpoint", value)), omitted_checkpoints: Math.max(0, active.length - 64), checkpoints: array(value.checkpoints).filter(cp => active.includes(cp.id)).slice(0, 64).map(checkpoint), interface_state: states.slice(0, 16).map(([name, state]) => ({ interface: id("interface", name), context_generation: integer(state?.context_generation), pending_handoff: handoff(state?.pending_handoff) })), omitted_interface_states: Math.max(0, states.length - 16) };
  }
  function movement(row) {
    const meta = object(row.invocation_metadata), refused = meta.delivery_verdict?.refused;
    return { seq: row.seq, at_unix_ms: integer(row.recorded_at_unix_ms), kind: KINDS.has(kind(row)) ? kind(row) : "unknown", from: id("interface", row.from?.Interface), to: id("interface", row.to?.Interface), artifact: ref(row.artifact), runtime: coordinates(meta.runtime), source_movement_seq: integer(meta.source_movement_seq), delivery_scope_seq: integer(meta.delivery_scope_seq), replaces_arrival_seq: integer(meta.replaces_arrival_seq), arrival_seq: integer(meta.arrival_seq), mark_run: id("run", meta.run), checkpoint_admission: meta.checkpoint_admission ? { label: label(meta.checkpoint_admission.artifact_label), complete: meta.checkpoint_admission.complete === true, required: array(meta.checkpoint_admission.required_artifact_labels).slice(0, 16).map(label) } : null, refusal: refused ? { reason: REASONS.has(refused.reason) ? refused.reason : "unknown", ...errorText(refused.detail?.message) } : null, verification_failure: meta.verification_failure ? { verifier: id("verifier", meta.verification_failure.verifier), ...errorText(meta.verification_failure.error) } : null };
  }
  return { id, ref, errorText, continuity, payload, run, handoff, movement };
}

async function collectRuntimeReport({ home, includeDetails, sourceSequences = [], interfaceFailures = [] }) {
  const view = projection(includeDetails);
  const report = { source: "observed", limits: LIMITS, interface_failures: interfaceFailures.slice(0, 32).map(failure => ({ interface: view.id("interface", failure.interfaceId), ...view.errorText(failure.message) })), omitted_interface_failures: Math.max(0, interfaceFailures.length - 32), ledger: {}, context_bindings: [], artifacts: [], records: [], interfaces: [] };
  try {
    const host = JSON.parse((await readBounded(home, "host.json", 1024 * 1024)).text);
    const roots = Object.entries(object(host.root_refs));
    report.host = { source: "observed", schema_version: integer(host.host_manifest_version), roots: roots.slice(0, 64).map(([slot, root]) => ({ slot: ["repo_root", "workspace_root", "run_records_root", "registry_interface_root", "registry_tool_root", "registry_worker_environment_kind_root", "platform_toolchain_root", "host_filesystem_root"].includes(slot) ? slot : view.id("slot", slot), path: view.id("path", root), inside_selected_home: typeof root === "string" && (root === home || root.startsWith(home + path.sep)) })), omitted_roots: Math.max(0, roots.length - 64) };
  } catch (error) { report.host = { source: sourceFailure(error) }; }
  try {
    const instance = JSON.parse((await readBounded(home, "state/runtime-instance/state.json", 16384)).text);
    report.saved_runtime = { source: "observed", live_status: "not_probed", schema_version: integer(instance.schema_version), phase: ["starting", "ready", "stopping", "stopped"].includes(instance.phase) ? instance.phase : "unknown", started_at_unix_ms: integer(instance.started_at_unix_ms), updated_at_unix_ms: integer(instance.updated_at_unix_ms) };
  } catch (error) { report.saved_runtime = { source: sourceFailure(error) }; }
  let rows = [];
  try {
    const source = await readBounded(home, "ledger/movements.jsonl", LIMITS.ledger_bytes, true);
    const complete = source.text.lastIndexOf("\n");
    let malformed = 0, sequenceGaps = 0, previous = null;
    for (const line of source.text.slice(0, complete < 0 ? 0 : complete).split("\n").filter(Boolean)) {
      let row;
      try { row = JSON.parse(line); } catch { malformed++; continue; }
      if (integer(row?.seq) === null) { malformed++; continue; }
      if (previous !== null && row.seq !== previous + 1) sequenceGaps++;
      previous = row.seq;
      rows.push(row);
    }
    report.ledger = { source: "observed", truncated: source.truncated, observed_bytes: source.observed_bytes, changed_during_read: source.changed_during_read, incomplete_tail: !source.text.endsWith("\n"), malformed_rows: malformed, sequence_gaps: sequenceGaps, observed_rows: rows.length, first_seq: rows[0]?.seq ?? null, last_seq: rows.at(-1)?.seq ?? null };
  } catch (error) { report.ledger = { source: sourceFailure(error) }; }

  const failures = rows.filter(row => ["DeliveryRefused", "VerificationFailed"].includes(kind(row)) || ERROR_LABELS.has(artifactLabel(row.artifact)));
  const requested = firstAndLast(sourceSequences, 2, 6);
  const anchors = new Set([...requested, ...firstAndLast(failures, 4, 8).map(row => row.seq)]);
  const bySeq = new Map(rows.map(row => [row.seq, row]));
  const anchorRows = [...anchors].flatMap(seq => bySeq.has(seq) ? [bySeq.get(seq)] : []);
  const prioritize = values => [...new Set([...anchorRows.filter(row => values.includes(row)), ...values.slice(0, 4), ...values.slice().reverse()])];
  const relatedRuns = new Set([...anchors].flatMap(seq => bySeq.get(seq)?.invocation_metadata?.runtime?.run ? [bySeq.get(seq).invocation_metadata.runtime.run] : []));
  const selected = new Set([...anchors].filter(seq => bySeq.has(seq)));
  for (const seq of anchors) {
    const index = rows.findIndex(row => row.seq === seq);
    if (index < 0) continue;
    for (const row of rows.slice(Math.max(0, index - 4), index + 5)) selected.add(row.seq);
  }
  for (const run of relatedRuns) {
    const own = rows.filter(row => row.invocation_metadata?.runtime?.run === run);
    for (const row of firstAndLast(own, 8, 24)) selected.add(row.seq);
  }
  if (!selected.size) for (const row of rows.slice(-48)) selected.add(row.seq);
  let chosen = rows.filter(row => selected.has(row.seq));
  if (chosen.length > LIMITS.movements) {
    const required = chosen.filter(row => anchors.has(row.seq));
    chosen = [...required, ...chosen.filter(row => !anchors.has(row.seq)).slice(-(LIMITS.movements - required.length))].sort((a, b) => a.seq - b.seq);
  }
  report.ledger.requested_sources = requested;
  report.ledger.missing_requested_sources = requested.filter(seq => !bySeq.has(seq));
  report.ledger.movements = chosen.map(view.movement);
  report.ledger.omitted_rows = rows.length - chosen.length;

  let blobBytes = 0;
  const blobs = new Map();
  async function addressed(directory, hash) {
    if (!HASH.test(hash)) throw new Error("invalid_identity");
    const key = `${directory}/${hash}`;
    if (blobs.has(key)) return blobs.get(key);
    const remaining = LIMITS.blob_bytes - blobBytes;
    if (remaining <= 0) throw new Error("read_budget_exhausted");
    const source = await readBounded(home, path.join(directory, hash), Math.min(4 * 1024 * 1024, remaining));
    blobBytes += source.bytes.length;
    if (Buffer.from(blake3(source.bytes)).toString("hex") !== hash) throw new Error("digest_mismatch");
    let value;
    try { value = JSON.parse(source.text); } catch { throw new Error("invalid_json"); }
    blobs.set(key, value);
    return value;
  }
  const blob = hash => addressed("ledger/blobs", hash);
  const boundArtifacts = new Map();
  const bindingRows = prioritize(chosen.filter(row => row.invocation_metadata?.artifact_binding_refs_content || row.invocation_metadata?.artifact_binding_refs?.length));
  for (const row of bindingRows.slice(0, LIMITS.context_bindings)) {
    const item = { source_movement_seq: row.seq };
    try {
      const meta = row.invocation_metadata;
      if (meta.artifact_binding_refs_content && meta.artifact_binding_refs) throw new Error("invalid_identity");
      const refs = meta.artifact_binding_refs_content ? await addressed("ledger/metadata", meta.artifact_binding_refs_content) : meta.artifact_binding_refs;
      if (!Array.isArray(refs)) throw new Error("invalid_json");
      const selectedRefs = firstAndLast(refs, 4, 28);
      Object.assign(item, { source: "observed", refs: selectedRefs.map(view.ref), omitted_refs: refs.length - selectedRefs.length });
      for (const ref of selectedRefs) if (PAYLOAD_LABELS.has(artifactLabel(ref))) boundArtifacts.set(ref.id, ref);
    } catch (error) { item.source = sourceFailure(error); }
    report.context_bindings.push(item);
  }
  report.omitted_context_bindings = Math.max(0, bindingRows.length - LIMITS.context_bindings);
  const artifacts = new Map();
  const primaryArtifacts = new Set(), secondaryArtifacts = new Set(), capturedArtifacts = new Set();
  const addArtifact = (ref, primary = true) => {
    if (!PAYLOAD_LABELS.has(artifactLabel(ref))) return;
    artifacts.set(ref.id, ref);
    (primary ? primaryArtifacts : secondaryArtifacts).add(ref.id);
  };
  for (const row of anchorRows) addArtifact(row.artifact);
  for (const ref of boundArtifacts.values()) addArtifact(ref);
  for (const row of chosen.slice().reverse()) addArtifact(row.artifact, false);
  while (report.artifacts.length < LIMITS.artifacts && (primaryArtifacts.size || secondaryArtifacts.size)) {
    const queue = primaryArtifacts.size ? primaryArtifacts : secondaryArtifacts;
    const id = queue.values().next().value;
    queue.delete(id);
    if (capturedArtifacts.has(id)) continue;
    capturedArtifacts.add(id);
    const artifact = artifacts.get(id);
    const item = { artifact: view.ref(artifact) };
    try {
      const manifest = await blob(artifact.content);
      if (manifest.label !== artifactLabel(artifact)) throw new Error("invalid_identity");
      const value = object(await blob(manifest.payload_content_hash));
      item.payload = view.payload(value, manifest.label);
      for (const ref of [value.session_context?.parent, value.source_artifact, ...sessionEntries(value).flatMap(entry => [entry.source_artifact, entry.payload?.source_artifact])]) addArtifact(ref);
      item.source = "observed";
    } catch (error) { item.source = sourceFailure(error); }
    report.artifacts.push(item);
  }
  report.omitted_artifacts = Math.max(0, artifacts.size - LIMITS.artifacts);
  const linked = rows.filter(row => artifacts.has(row.artifact?.id) && !chosen.some(selected => selected.seq === row.seq));
  report.ledger.omitted_linked_rows = Math.max(0, linked.length - Math.max(0, LIMITS.movements - chosen.length));
  chosen.push(...linked.slice(0, Math.max(0, LIMITS.movements - chosen.length)));
  chosen.sort((left, right) => left.seq - right.seq);
  report.ledger.movements = chosen.map(view.movement);
  report.ledger.omitted_rows = rows.length - chosen.length;

  const recordRows = prioritize(chosen.filter(row => HASH.test(row.invocation_metadata?.runtime?.run_record_hash)));
  const records = new Map(recordRows.map(row => [row.invocation_metadata.runtime.run_record_hash, row.invocation_metadata.runtime.run]));
  report.omitted_records = Math.max(0, records.size - LIMITS.records);
  for (const [hash, run] of [...records].slice(0, LIMITS.records)) {
    const item = { record: view.id("record", hash), run: view.id("run", run), current: false };
    try {
      const record = await blob(hash);
      if (record.id !== run) throw new Error("invalid_identity");
      item.state = view.run(record); item.source = "observed";
    } catch (error) { item.source = sourceFailure(error); }
    report.records.push(item);
  }
  const runs = [...new Set([...relatedRuns, ...chosen.slice().reverse().flatMap(row => row.invocation_metadata?.runtime?.run ? [row.invocation_metadata.runtime.run] : [])])];
  const interfaces = new Set(interfaceFailures.map(failure => failure.interfaceId).filter(value => typeof value === "string"));
  const runProviders = [];
  for (const run of runs.slice(0, LIMITS.runs)) {
    const item = { run: view.id("run", run), current: true };
    try {
      if (!SAFE_SEGMENT.test(run)) throw new Error("invalid_identity");
      const record = JSON.parse((await readBounded(home, `state/runs/${run}.json`, 4 * 1024 * 1024)).text);
      if (record.id !== run) throw new Error("invalid_identity");
      item.state = view.run(record); item.source = "observed";
      for (const [name, state] of Object.entries(object(record.interface_state))) if (integer(state?.context_generation) !== null) { interfaces.add(name); runProviders.push([run, name]); }
      for (const cp of array(record.checkpoints).filter(cp => array(record.active_checkpoints).includes(cp.id))) {
        if (cp.interface_binding?.interface) interfaces.add(cp.interface_binding.interface);
      }
    } catch (error) { item.source = sourceFailure(error); }
    report.records.push(item);
  }
  report.omitted_runs = Math.max(0, runs.length - LIMITS.runs);
  for (const row of chosen.slice().reverse()) for (const name of [row.invocation_metadata?.runtime?.interface, row.from?.Interface, row.to?.Interface]) if (typeof name === "string") interfaces.add(name);
  if (!interfaces.size) {
    try {
      const source = await supportEntries(home, "interfaces", 128);
      report.interface_discovery = { source: "observed", truncated: source.truncated };
      for (const entry of source.entries) if (entry.isDirectory() && !entry.isSymbolicLink()) interfaces.add(entry.name);
    } catch (error) { report.interface_discovery = { source: sourceFailure(error) }; }
  }
  for (const name of [...interfaces].slice(0, LIMITS.interfaces)) {
    const item = { interface: view.id("interface", name), stderr: [], standing_handoffs: [] };
    try {
      if (!SAFE_SEGMENT.test(name)) throw new Error("invalid_identity");
      const source = await supportEntries(home, `interfaces/${name}`, 256);
      item.source = "observed"; item.directory_truncated = source.truncated;
      const logs = source.entries.filter(entry => /^interface-life\.stderr\.[A-Za-z0-9._-]+\.log$/.test(entry.name)).map(entry => entry.name).sort();
      const selectedLogs = new Set(firstAndLast(logs, 1, 1));
      for (const row of chosen.filter(row => anchors.has(row.seq))) {
        const preceding = logs.filter(log => Number(log.split(".")[2]) <= row.recorded_at_unix_ms).at(-1);
        if (preceding && selectedLogs.size < LIMITS.stderr_files) selectedLogs.add(preceding);
      }
      item.omitted_stderr_files = logs.length - selectedLogs.size;
      for (const log of [...selectedLogs].sort()) {
        const file = { file: view.id("stderr", `${name}/${log}`) };
        try {
          const captured = await readBounded(home, `interfaces/${name}/${log}`, LIMITS.stderr_bytes, true);
          Object.assign(file, { source: "observed", observed_bytes: captured.observed_bytes, truncated: captured.truncated, changed_during_read: captured.changed_during_read, ...(includeDetails ? { text: captured.text } : {}) });
        } catch (error) { file.source = sourceFailure(error); }
        item.stderr.push(file);
      }
    } catch (error) { item.source = sourceFailure(error); }
    for (const [run, provider] of runProviders.filter(([, provider]) => provider === name)) {
      const journal = { run: view.id("run", run) };
      try {
        const hash = Buffer.from(blake3(Buffer.from(run))).toString("hex");
        const source = await readBounded(home, `interfaces/${provider}/handoff-standing/${hash}.jsonl`, 128 * 1024, true);
        const lines = source.text.slice(0, source.text.lastIndexOf("\n") + 1).split("\n").filter(Boolean);
        Object.assign(journal, { source: "observed", truncated: source.truncated, entries: lines.slice(-24).map(line => {
          const value = JSON.parse(line);
          return { source_movement_seq: integer(value.source_movement_seq), boundary: view.id("handoff", value.boundary), context_generations: integer(value.context_generations), handoff: view.handoff(value) };
        }), omitted_entries: Math.max(0, lines.length - 24) });
      } catch (error) { journal.source = sourceFailure(error); }
      item.standing_handoffs.push(journal);
    }
    report.interfaces.push(item);
  }
  report.omitted_interfaces = Math.max(0, interfaces.size - LIMITS.interfaces);
  report.blob_bytes_read = blobBytes;
  try {
    const source = await readBounded(home, "state/runtime-instance/output.log", 16384, true);
    report.runtime_log = { source: "observed", observed_bytes: source.observed_bytes, truncated: source.truncated, ...(includeDetails ? { text: source.text } : {}) };
  } catch (error) { report.runtime_log = { source: sourceFailure(error) }; }
  return report;
}

module.exports = { collectRuntimeReport };
