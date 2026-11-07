const assert = require("node:assert/strict");
const os = require("node:os");
const path = require("node:path");

function maintenanceFixture(registryRoot, runsRoot, original = {}) {
  const terminal = "interface.artifact.output", input = "memory.experience.input", outcome = "memory.capture.outcome", library = "artifact-system-library";
  const plan = { run_id: "source_run", model_interface_id: "source_provider", worker_interface_id: "source_worker",
    chat_interface_id: "source_client", interface_writes: [], ...structuredClone(original), run_writes: [] };
  const source = original.run_record_content ? JSON.parse(original.run_record_content) : {
    id: plan.run_id, ee: "ee_test", worker: plan.worker_interface_id, trace: "trace:source", status: "running", interface_state: {},
    checkpoints: [], active_checkpoints: [],
  };
  const binding = (id, label) => ({ interface: id, config: { artifact_policy: label
    ? { mode: "allow", names: [label], namespaces: [] } : { mode: "all" } } });
  const requirement = (label, params) => [{ artifact_label: label, mark: "memory.experience.admitted", verifier: "memory_experience", params }];
  const template = (role, from, to, requirements = [], label) => ({ name: `${role}_${from}_${to}`, role,
    interface_binding: binding(from, label), outbound_interface_bindings: [binding(to, label)],
    include_context: role === "stable", static_artifact_bindings: [], required_verified_artifacts: requirements });
  const record = (id, worker, provider, templates) => {
    const checkpoints = templates.map((row, n) => ({ id: `cp_${n}`, run: id, epoch: 0, trace: `trace:${id}:${n}`,
      interface_binding: row.interface_binding, outbound_interface_bindings: row.outbound_interface_bindings,
      artifact_bindings: [], required_verified_artifacts: row.required_verified_artifacts }));
    return { id, ee: source.ee, worker, trace: `trace:${id}`, status: "running", checkpoints, active_checkpoints: checkpoints.map((cp) => cp.id),
      interface_state: { [provider]: { run_authoring_binding: { schema_version: 2, run: id, worker, provider_interface: provider, checkpoints: templates } } } };
  };
  const stage = (kind, id, value, file = `${kind}.json`) => {
    const write = { path: path.join(registryRoot, kind, id, file), content: `${JSON.stringify(value)}\n`, tag: `${file === "policy.json" ? "policy" : kind}:${id}`, expected_before_hash: null };
    const index = plan.interface_writes.findIndex((row) => row.path === write.path);
    if (index < 0) plan.interface_writes.push(write); else plan.interface_writes[index] = write;
  };
  const runWrite = (run, content = `${JSON.stringify(run, null, 2)}\n`) => ({ path: path.join(runsRoot, `${run.id}.json`), content, tag: `run:${run.id}`, expected_before_hash: null });
  const provider = plan.model_interface_id, client = plan.chat_interface_id;
  const d = { schema_version: 1, run: "observer_run", ee: source.ee, worker_interface: "observer_worker", provider_interface: "observer_provider",
    source_run: source.id, source_worker_interface: source.worker, source_provider_interface: provider, source_client_interface: client,
    source_relay_interface: "terminal_relay", projection_interface: "projector", experience_interface: "experience_relay",
    terminal_label: terminal, provider_output_labels: [terminal, "turn.completed"], input_label: input, outcome_interface: "observer_outcome", capture_interface: "capture_relay", outcome_label: outcome, outcome_error_label: "memory.capture.error",
    collection_source: library, collection_entries_root: path.join(registryRoot, "artifact", library, "entries"),
    publication_door_interface: "publication_door", publication_tool: "memory.save.publication" };
  const sourceParams = { source_run: source.id, source_ee: source.ee, source_worker_interface: source.worker, source_provider_interface: provider, source_client_interface: client };
  const projected = requirement(input, { stage: "projection", ...sourceParams });
  const observerTemplates = [template("stable", d.experience_interface, d.provider_interface, projected),
    template("provider_artifact_output", d.provider_interface, d.outcome_interface, [], terminal),
    template("provider_tool_output", d.provider_interface, d.publication_door_interface), template("stable", d.publication_door_interface, d.provider_interface),
    template("authoring_call", "observer_authoring", "observer_door", requirement("tool.call", { tools: ["file.write.observer_run"] })), template("authoring_result", "observer_door", d.provider_interface)];
  const observer = record(d.run, d.worker_interface, d.provider_interface, observerTemplates);
  observer.checkpoints[0].artifact_bindings = [{ id: library }];
  const projectionTemplates = [template("stable", d.source_relay_interface, d.projection_interface,
    requirement(terminal, { stage: "terminal", source_provider_interface: provider }), terminal),
    template("provider_artifact_output", d.projection_interface, d.experience_interface, projected, input),
    template("independent_input", d.outcome_interface, d.projection_interface, requirement(terminal, { stage: "terminal", source_provider_interface: d.provider_interface }), terminal),
    template("provider_artifact_output", d.projection_interface, d.capture_interface, requirement(outcome, {
      source_run: source.id, source_ee: source.ee, provider_interface: d.provider_interface, publication_tool: d.publication_tool, entries_root: d.collection_entries_root }), outcome),
    template("authoring_call", "projector_authoring", "projector_door", requirement("tool.call", { tools: ["file.write.projection_run"] })), template("authoring_result", "projector_door", d.projection_interface)];
  projectionTemplates.push(template("provider_artifact_output", d.projection_interface, d.capture_interface, [], "memory.capture.error"));
  projectionTemplates[0].outbound_interface_bindings = [binding(d.projection_interface)];
  projectionTemplates[2].outbound_interface_bindings = [binding(d.projection_interface)];
  delete projectionTemplates[2].static_artifact_bindings;
  const projection = record("projection_run", "projection_worker", d.projection_interface, projectionTemplates);
  projection.status = "idle";
  for (const [run, owner, authoring, door] of [[observer, d.provider_interface, "observer_authoring", "observer_door"], [projection, d.projection_interface, "projector_authoring", "projector_door"]]) {
    const topology = run.interface_state[owner].run_authoring_binding;
    topology.authoring_source_interface = authoring;
    const writer = record(`${run.id}_writer`, run.worker, owner, [template("authoring_call", authoring, door,
      topology.checkpoints.find((cp) => cp.role === "authoring_call").required_verified_artifacts), template("authoring_result", door, owner)]);
    writer.status = "idle"; run.status = "idle";
    writer.interface_state[owner] = { file_writer_target: { run: run.id, provider: owner, source: authoring, door, generation: 0 } };
    run.interface_state[owner].run_authoring_binding.writer_run = { run: writer.id, initial_record: writer, initial_record_format: "pretty" };
    run.checkpoints = run.checkpoints.filter((cp) => ![authoring, door].includes(cp.interface_binding.interface));
    run.active_checkpoints = run.checkpoints.map((cp) => cp.id);
    plan.run_writes.push(runWrite(writer));
  }
  const observerWrite = runWrite(observer), projectionWrite = runWrite(projection);
  Object.assign(d, { initial_record: observer, initial_record_content: observerWrite.content,
    projection_run: { run: projection.id, ee: projection.ee, worker_interface: projection.worker, provider_interface: d.projection_interface,
      initial_record: projection, initial_record_content: projectionWrite.content } });
  plan.run_writes.push(observerWrite, projectionWrite);
  let output = source.checkpoints.find((cp) => cp.interface_binding?.interface === provider && cp.outbound_interface_bindings.some((b) => b.interface === client));
  if (!output) { output = record(source.id, source.worker, provider, [template("provider_artifact_output", provider, client)]).checkpoints[0]; source.checkpoints.push(output); source.active_checkpoints.push(output.id); }
  output.interface_binding = binding(provider, terminal); output.outbound_interface_bindings.push(binding(d.source_relay_interface, terminal));
  const sourceTemplate = { ...template("provider_artifact_output", provider, client, output.required_verified_artifacts, terminal), outbound_interface_bindings: output.outbound_interface_bindings };
  source.interface_state[provider] = { ...source.interface_state[provider], maintenance_observer: d,
    run_authoring_binding: { schema_version: 2, run: source.id, worker: source.worker, provider_interface: provider, checkpoints: [sourceTemplate] } };
  plan.run_record_content = `${JSON.stringify(source, null, 2)}\n`;
  const stagedProvider = plan.interface_writes.find((row) => row.path === path.join(registryRoot, "interface", provider, "interface.json"));
  const providerDescriptor = stagedProvider ? JSON.parse(stagedProvider.content) : { id: provider };
  providerDescriptor.interface_driver_config ??= {}; providerDescriptor.interface_driver_config.private_config ??= {};
  Object.assign(providerDescriptor.interface_driver_config.private_config, { client_projection_output_labels: d.provider_output_labels, session_snapshot_labels: [terminal] });
  stage("interface", provider, providerDescriptor);
  stage("artifact", library, { id: library, label: "context.collection", schema_version: 1, directory: "entries" });
  stage("interface", d.publication_door_interface, { id: d.publication_door_interface, interface_driver_config: { private_config: { memory_collection: library },
    host_slots: { registry_artifact_root: { path: path.join(registryRoot, "artifact") } } } });
  stage("tool", d.publication_tool, { id: d.publication_tool, interface: d.publication_door_interface, process: { authority: {
    write_root_slots: ["registry_artifact_root"] } } });
  stage("interface", d.projection_interface, { id: d.projection_interface, interface_type: "system", interface_driver_ref: "bound_context_projection",
    interface_driver_config: { private_config: { client_projection_output_labels: [input, outcome, "memory.capture.error"], context_projection: { schema_version: 1, ...sourceParams, projection_run: projection.id,
      observer_run: d.run, source_relay_interface: d.source_relay_interface, terminal_label: terminal, collection_source: library, publication_tool: d.publication_tool,
      outcome: { worker_interface: d.worker_interface, provider_interface: d.provider_interface, relay_interface: d.outcome_interface,
        experience_interface: d.experience_interface, input_routes: [{ interface_binding: observerTemplates[0].interface_binding }] } } } } });
  for (const id of [d.source_relay_interface, d.experience_interface, d.outcome_interface, d.capture_interface]) stage("interface", id, { id, interface_type: "system", role: "worker" });
  for (const [id, door] of [["file.write.observer_run", "observer_door"], ["file.write.projection_run", "projector_door"]]) {
    stage("tool", id, { id, interface: door });
  }
  for (const run of [source, observer, projection]) {
    const templates = run.interface_state[run.id === observer.id ? d.provider_interface : run.id === projection.id ? d.projection_interface : provider].run_authoring_binding.checkpoints;
    const interfaces = new Set([run.worker, ...templates.flatMap((cp) => [cp.interface_binding.interface, ...cp.outbound_interface_bindings.map((b) => b.interface)])]);
    for (const id of interfaces) if (!plan.interface_writes.some((row) => row.path === path.join(registryRoot, "interface", id, "interface.json"))) stage("interface", id, { id });
    const policyPath = path.join(registryRoot, "interface", run.worker, "policy.json"), existing = plan.interface_writes.find((row) => row.path === policyPath);
    const tools = run.id === observer.id ? [d.publication_tool, "file.write.observer_run"]
      : run.id === projection.id ? ["file.write.projection_run"] : [];
    const policy = existing ? JSON.parse(existing.content) : { id: run.worker, ceiling: { interfaces: [], tools } };
    policy.ceiling.interfaces = [...new Set([...policy.ceiling.interfaces, ...interfaces])].sort();
    stage("interface", run.worker, policy, "policy.json");
  }
  for (const id of ["projector_authoring", "projector_door"]) {
    plan.interface_writes.find((write) => write.path === path.join(registryRoot, "interface", id, "interface.json")).tag = `projection_authoring:${id}`;
  }
  const sourcePolicy = JSON.parse(plan.interface_writes.find((row) => row.path === path.join(registryRoot, "interface", source.worker, "policy.json")).content);
  const sourceIds = new Set([source.worker, ...sourcePolicy.ceiling.interfaces]); sourceIds.delete(d.source_relay_interface);
  d.lifecycle_ownership = { schema_version: 1,
    private_interface_ids: plan.interface_writes.filter((row) => row.path.endsWith(`${path.sep}interface.json`))
      .map((row) => JSON.parse(row.content).id).filter((id) => !sourceIds.has(id)).sort(),
    private_tool_ids: plan.interface_writes.filter((row) => row.path.endsWith(`${path.sep}tool.json`))
      .map((row) => JSON.parse(row.content).id).filter((id) => !sourcePolicy.ceiling.tools.includes(id)).sort() };
  d.observer_selection = { template_interface_id: "system_worker_memory_observer", config_file: "maintenance.json" };
  plan.run_record_content = `${JSON.stringify(source, null, 2)}\n`;
  return plan;
}

module.exports = { maintenanceFixture };

if (require.main === module) {
  const test = require("node:test"), { plannedAdditionalRunWrites } = require("./client/topologyPlan.cjs");
  const registry = path.join(os.tmpdir(), "octopus-maintenance-guard-registry"), runs = path.join(os.tmpdir(), "octopus-maintenance-guard-runs");
  const guard = (plan) => plannedAdditionalRunWrites(plan, runs, registry);
  const changeSource = (plan, change) => { const source = JSON.parse(plan.run_record_content); change(source, source.interface_state[plan.model_interface_id].maintenance_observer); plan.run_record_content = JSON.stringify(source); };
  test("retained declaration decode needs neither a live source frontier nor current companion bytes", () => {
    const { decodeMaintenanceDeclaration } = require("./maintenance-client.cjs"), plan = maintenanceFixture(registry, runs);
    const source = JSON.parse(plan.run_record_content); source.active_checkpoints = [];
    const decoded = decodeMaintenanceDeclaration({ ...plan, interface_writes: [], run_writes: [] }, source);
    assert.equal(decoded.observer.record.id, "observer_run"); assert.equal(decoded.projection.record.id, "projection_run");
    assert.equal(decodeMaintenanceDeclaration(plan, { ...source, ee: "moved_ee" }).observer.record.id, "observer_run");
    assert.throws(() => decodeMaintenanceDeclaration(plan, { ...source, worker: "foreign_worker" }), /exact source/);
    delete source.interface_state[plan.model_interface_id].maintenance_observer;
    assert.equal(decodeMaintenanceDeclaration(plan, source), null);
    delete source.interface_state; assert.equal(decodeMaintenanceDeclaration(plan, source), null);
  });
  test("maintenance creation admits exact companions and nested writers; retained plans create nothing", () => {
    const plan = maintenanceFixture(registry, runs), accepted = guard(plan);
    assert.equal(accepted.length, 4); assert.equal(accepted[2].content, plan.run_writes[2].content);
    assert.equal(guard({ ...plan, run_writes: [], interface_writes: [] }).length, 0);
    assert.throws(() => guard({ ...plan, run_writes: plan.run_writes.slice(0, 3) }), /both companions/);
    assert.throws(() => guard({ ...plan, run_writes: plan.run_writes.slice(1) }), /declared writers/);
  });
  test("outcome uses only the context-free Independent input; source and observer remain Stable", () => {
    for (const edit of [(cp) => { cp.role = "stable"; }, (cp) => { cp.include_context = true; },
      (cp) => { cp.static_artifact_bindings = ["unbound-context"]; }]) {
      const plan = maintenanceFixture(registry, runs);
      changeSource(plan, (_, d) => {
        const projection = d.projection_run;
        edit(projection.initial_record.interface_state[d.projection_interface].run_authoring_binding.checkpoints[2]);
        projection.initial_record_content = JSON.stringify(projection.initial_record);
        plan.run_writes.find((write) => write.tag === `run:${projection.run}`).content = projection.initial_record_content;
      });
      assert.throws(() => guard(plan), /maintenance.*independent|maintenance.*Independent/);
    }
  });
  test("lifecycle inventory matches new private descriptors and companion ceilings exactly", () => {
    const edits = [
      (p) => changeSource(p, (_, d) => { delete d.lifecycle_ownership; }),
      (p) => changeSource(p, (_, d) => { delete d.observer_selection; }),
      (p) => changeSource(p, (_, d) => { d.observer_selection.config_file = "../maintenance.json"; }),
      (p) => changeSource(p, (_, d) => { d.lifecycle_ownership.schema_version = 2; }),
      (p) => changeSource(p, (_, d) => { d.lifecycle_ownership.extra = true; }),
      (p) => changeSource(p, (_, d) => { d.lifecycle_ownership.private_interface_ids.push(d.worker_interface); }),
      (p) => changeSource(p, (_, d) => { d.lifecycle_ownership.private_interface_ids.push(d.source_worker_interface); }),
      (p) => changeSource(p, (_, d) => { d.lifecycle_ownership.private_tool_ids.pop(); }),
      (p) => { p.interface_writes = p.interface_writes.filter((write) => write.tag !== "interface:observer_door"); },
      (p) => { p.interface_writes.find((write) => write.tag === "tool:file.write.projection_run").expected_before_hash = "a".repeat(64); },
      (p) => { const write = p.interface_writes.find((write) => write.tag === "tool:file.write.projection_run");
        write.content = JSON.stringify({ id: "file.write.projection_run", interface: p.worker_interface_id }); },
      (p) => { const write = p.interface_writes.find((write) => write.tag === `policy:${p.worker_interface_id}`), policy = JSON.parse(write.content);
        policy.ceiling.interfaces.push("observer_door"); write.content = JSON.stringify(policy); },
      (p) => { p.interface_writes.push({ path: path.join(registry, "interface", "undeclared-owner", "interface.json"),
        tag: "interface:undeclared-owner", content: JSON.stringify({ id: "undeclared-owner" }), expected_before_hash: null }); },
    ];
    for (const edit of edits) { const plan = maintenanceFixture(registry, runs); edit(plan); assert.throws(() => guard(plan), /maintenance/); }
  });
  test("ordinary declared writer and voice Runs retain their existing creation contract", () => {
    const plan = maintenanceFixture(registry, runs), source = JSON.parse(plan.run_record_content), owner = plan.model_interface_id;
    delete source.interface_state[owner].maintenance_observer;
    const writer = { id: "source_writer", ee: source.ee, worker: source.worker, status: "idle", trace: "trace:writer",
      checkpoints: [], active_checkpoints: [], interface_state: { [owner]: { file_writer_target: { run: source.id } } } };
    source.interface_state[owner].run_authoring_binding.writer_run = { run: writer.id, initial_record: writer };
    source.interface_state[plan.chat_interface_id] = { voice_context: { run_id: "voice_run", control_interface_id: "voice_control",
      provider_id: "voice_provider", delegation_interface_id: "voice_return" } };
    const voice = { ...writer, id: "voice_run", interface_state: { voice_control: { home_run: source.id, home_interface: plan.chat_interface_id },
      voice_provider: { run_authoring_binding: { run: "voice_run", worker: source.worker, provider_interface: "voice_provider",
        checkpoints: [{ role: "provider_interface_output", outbound_interface_bindings: [{ interface: "voice_return" }] }] } } } };
    plan.run_record_content = JSON.stringify(source); plan.interface_writes = [];
    plan.run_writes = [writer, voice].map((record) => ({ path: path.join(runs, `${record.id}.json`), content: JSON.stringify(record), tag: `run:${record.id}`, expected_before_hash: null }));
    assert.equal(guard(plan).length, 2);
  });
  test("maintenance guard rejects changed identity, scope, routes, authority and exact planned bytes", () => {
    const edits = [
      (p) => changeSource(p, (_, d) => { d.source_provider_interface = "foreign_provider"; }),
      (p) => changeSource(p, (_, d) => { d.source_client_interface = "foreign_client"; }),
      (p) => changeSource(p, (_, d) => { d.projection_run.worker_interface = "foreign_worker"; }),
      (p) => changeSource(p, (source) => { source.checkpoints[0].outbound_interface_bindings.at(-1).config.artifact_policy.names.push("turn.completed"); }),
      (p) => changeSource(p, (_, d) => { d.collection_entries_root = path.join(registry, "artifact", "another", "entries"); }),
      (p) => changeSource(p, (_, d) => { d.initial_record.checkpoints[0].artifact_bindings[0].access = { read: false };
        d.initial_record_content = `${JSON.stringify(d.initial_record)}\n`; p.run_writes.find((w) => w.tag === `run:${d.run}`).content = d.initial_record_content; }),
      (p) => { const write = p.interface_writes.find((w) => w.tag === "interface:projector"), value = JSON.parse(write.content);
        value.interface_driver_config.private_config.context_projection.outcome.provider_interface = "foreign_observer"; write.content = JSON.stringify(value); },
      (p) => { const write = p.interface_writes.find((w) => w.tag === "interface:capture_relay"); write.content = JSON.stringify({ id: "capture_relay", interface_type: "world", role: "worker" }); },
      (p) => { const write = p.interface_writes.find((w) => w.tag === "tool:memory.save.publication"), value = JSON.parse(write.content); value.process.authority.optional_write_root_slots = ["repo_root"]; write.content = JSON.stringify(value); },
      (p) => { const write = p.interface_writes.find((w) => w.tag === "interface:publication_door"), value = JSON.parse(write.content); value.interface_driver_config.private_config.memory_collection = "another"; write.content = JSON.stringify(value); },
      (p) => { p.run_writes[2].content = JSON.stringify(JSON.parse(p.run_writes[2].content)); },
      (p) => { p.run_writes[0].path = path.join(runs, "escaped.json"); },
      (p) => { p.run_writes[0].expected_before_hash = "a".repeat(64); },
      (p) => { const record = JSON.parse(p.run_writes[0].content); record.id = "undeclared"; p.run_writes[0] = { ...p.run_writes[0], path: path.join(runs, "undeclared.json"), tag: "run:undeclared", content: JSON.stringify(record) }; },
      (p) => { p.run_writes.push({ path: path.join(runs, "unrelated.json"), tag: "run:unrelated", expected_before_hash: null, content: JSON.stringify({ id: "unrelated" }) }); },
    ];
    for (const edit of edits) { const plan = maintenanceFixture(registry, runs); edit(plan); assert.throws(() => guard(plan), /maintenance|additional Run|declared initial Run/); }
  });
}
