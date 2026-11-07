const fs = require("node:fs");
const path = require("node:path");

const MAX_DESCRIPTOR_BYTES = 2 * 1024 * 1024;

function decodeMaintenanceDeclaration(plan, target) {
  if (target.interface_state === undefined) return null;
  const declarations = Object.entries(object(target.interface_state, "source state"))
    .filter(([, state]) => state?.maintenance_observer !== undefined);
  if (!declarations.length) return null;
  if (declarations.length !== 1 || declarations[0][0] !== plan.model_interface_id) {
    fail("source has no unique selected maintenance provider");
  }
  const [owner, state] = declarations[0];
  const d = object(state.maintenance_observer, "declaration");
  fields(d, ["schema_version", "run", "worker_interface", "provider_interface", "source_run",
    "source_worker_interface", "source_provider_interface", "source_client_interface", "source_relay_interface",
    "projection_interface", "experience_interface", "terminal_label", "input_label", "outcome_interface", "capture_interface", "outcome_label",
    "projection_run", "collection_source", "collection_entries_root", "publication_door_interface", "publication_tool",
    "provider_output_labels", "initial_record", "initial_record_content", "lifecycle_ownership", "observer_selection"], ["ee", "outcome_error_label"]);
  const selection = object(d.observer_selection, "observer selection");
  fields(selection, ["template_interface_id", "config_file"]);
  component(selection.template_interface_id, "observer selection template");
  component(selection.config_file, "observer selection configuration file");
  if (d.schema_version !== 1 || target.id !== plan.run_id || target.worker !== plan.worker_interface_id
    || d.source_run !== target.id || d.source_worker_interface !== target.worker
    || d.source_provider_interface !== owner || d.source_client_interface !== plan.chat_interface_id) {
    fail("declaration differs from its exact source run/worker/provider/client");
  }
  for (const name of ["run", "worker_interface", "provider_interface", "source_run", "source_worker_interface",
    "source_provider_interface", "source_client_interface", "source_relay_interface", "projection_interface",
    "experience_interface", "outcome_interface", "capture_interface", "publication_door_interface"]) component(d[name], name);
  text(d.publication_tool, "publication tool");
  text(d.terminal_label, "terminal label"); text(d.input_label, "input label"); text(d.outcome_label, "outcome label");
  distinct([d.terminal_label, d.input_label, d.outcome_label], "maintenance labels");
  if (d.outcome_error_label !== undefined) {
    text(d.outcome_error_label, "outcome error label");
    distinct([d.terminal_label, d.input_label, d.outcome_label, d.outcome_error_label], "maintenance labels");
  }
  const labels = exactArray(d.provider_output_labels, "provider output labels");
  if (!labels.includes(d.terminal_label) || d.terminal_label === d.input_label) {
    fail("terminal and projected inputs must have distinct declared ownership");
  }
  const projection = object(d.projection_run, "projection run declaration");
  fields(projection, ["run", "worker_interface", "provider_interface", "initial_record", "initial_record_content"], ["ee"]);
  for (const name of ["run", "worker_interface", "provider_interface"]) component(projection[name], `projection ${name}`);
  if (projection.provider_interface !== d.projection_interface) fail("projection identity differs");
  distinct([target.id, d.run, projection.run], "run identities");
  distinct([target.worker, d.worker_interface, projection.worker_interface], "worker identities");
  distinct([owner, d.source_client_interface, d.source_relay_interface, d.projection_interface,
    d.experience_interface, d.provider_interface, d.outcome_interface, d.capture_interface, d.publication_door_interface], "interface identities");
  const observer = initial(d, "observer");
  const projected = initial(projection, "projection");
  topology(observer.record, d.provider_interface);
  topology(projected.record, d.projection_interface);
  const ownership = object(d.lifecycle_ownership, "lifecycle ownership");
  fields(ownership, ["schema_version", "private_interface_ids", "private_tool_ids"]);
  if (ownership.schema_version !== 1) fail("lifecycle ownership requires schema 1");
  const interfaces = exactArray(ownership.private_interface_ids, "private interface inventory").map((id) => component(id, "private interface"));
  const tools = exactArray(ownership.private_tool_ids, "private tool inventory").map((id) => component(id, "private tool"));
  const expected = new Set([d.source_relay_interface, d.experience_interface, d.outcome_interface, d.capture_interface,
    ...recordInterfaces(observer.record), ...recordInterfaces(projected.record)]);
  if (!same([...interfaces].sort(), [...expected].sort()) || [target.worker, owner, d.source_client_interface].some((id) => expected.has(id))
    || !tools.includes(d.publication_tool)) fail("lifecycle inventory differs from its exact private companion topology");
  return { declaration: d, observer, projection: projected };
}

function plannedMaintenanceRuns(plan, target, registryRoot, createsCompanions) {
  if (typeof createsCompanions !== "boolean") fail("planned companion validation requires an explicit creation mode");
  const retained = decodeMaintenanceDeclaration(plan, target);
  if (!retained) return [];
  const { declaration: d, observer, projection: projected } = retained;
  const owner = d.source_provider_interface;
  const labels = d.provider_output_labels;
  const sourceTopology = topology(target, owner);
  const observerTopology = topology(observer.record, d.provider_interface);
  const projectionTopology = topology(projected.record, d.projection_interface);
  const sourceOutput = route(target, sourceTopology, "provider_artifact_output", owner, d.source_client_interface, d.terminal_label);
  narrow(sourceOutput.interface_binding, owner, d.terminal_label);
  const relay = sourceOutput.outbound_interface_bindings.filter((binding) => binding.interface === d.source_relay_interface);
  if (relay.length !== 1) fail("source terminal has no unique declared relay");
  narrow(relay[0], d.source_relay_interface, d.terminal_label);
  if (sourceTopology.checkpoints.some((cp) => cp !== sourceOutput
    && cp.outbound_interface_bindings.some((binding) => binding.interface === d.source_relay_interface))) {
    fail("source relay is bound outside its exact terminal owner");
  }
  if (target.checkpoints.some((cp) => cp.outbound_interface_bindings?.some((binding) => binding.interface === d.source_relay_interface)
    && (!same(cp.interface_binding, sourceOutput.interface_binding)
      || !same(cp.outbound_interface_bindings, sourceOutput.outbound_interface_bindings)))) {
    fail("source relay checkpoint differs from its terminal rearm owner");
  }
  const projectionInput = route(projected.record, projectionTopology, "stable", d.source_relay_interface, d.projection_interface, d.terminal_label);
  narrow(projectionInput.interface_binding, d.source_relay_interface, d.terminal_label);
  onlyOutbound(projectionInput, d.projection_interface, d.terminal_label);
  const projectionOutput = route(projected.record, projectionTopology, "provider_artifact_output", d.projection_interface, d.experience_interface, d.input_label);
  narrow(projectionOutput.interface_binding, d.projection_interface, d.input_label);
  onlyOutbound(projectionOutput, d.experience_interface, d.input_label, true);
  const observerInput = route(observer.record, observerTopology, "stable", d.experience_interface, d.provider_interface, d.input_label);
  onlyOutbound(observerInput, d.provider_interface, d.input_label);
  const observerOutput = route(observer.record, observerTopology, "provider_artifact_output", d.provider_interface, d.outcome_interface, d.terminal_label);
  narrow(observerOutput.interface_binding, d.provider_interface, d.terminal_label);
  onlyOutbound(observerOutput, d.outcome_interface, d.terminal_label);
  const outcomeInput = route(projected.record, projectionTopology, "independent_input", d.outcome_interface, d.projection_interface, d.terminal_label);
  if (outcomeInput.include_context !== false || outcomeInput.static_artifact_bindings !== undefined
    && !same(outcomeInput.static_artifact_bindings, [])) fail("independent outcome input must remain context-free");
  narrow(outcomeInput.interface_binding, d.outcome_interface, d.terminal_label);
  onlyOutbound(outcomeInput, d.projection_interface, d.terminal_label);
  const captureOutput = route(projected.record, projectionTopology, "provider_artifact_output", d.projection_interface, d.capture_interface, d.outcome_label);
  narrow(captureOutput.interface_binding, d.projection_interface, d.outcome_label);
  onlyOutbound(captureOutput, d.capture_interface, d.outcome_label, true);
  if (d.outcome_error_label !== undefined) {
    const failure = route(projected.record, projectionTopology, "provider_artifact_output", d.projection_interface, d.capture_interface, d.outcome_error_label);
    narrow(failure.interface_binding, d.projection_interface, d.outcome_error_label);
    onlyOutbound(failure, d.capture_interface, d.outcome_error_label, true);
    if (failure.required_verified_artifacts?.length) fail("outcome diagnostics cannot carry a capture-success requirement");
  }

  const descriptors = descriptorReader(plan, registryRoot);
  if (createsCompanions) {
    validateCreationInventory(plan, target, d, observer.record, projected.record, registryRoot, descriptors);
  }
  for (const id of [d.source_relay_interface, d.experience_interface, d.outcome_interface, d.capture_interface]) {
    const receiver = descriptors("interface", id);
    if (receiver.interface_type !== "system" || receiver.role !== "worker" || Object.hasOwn(receiver, "interface_driver_ref")) {
      fail("maintenance receivers must remain their declared inert System pins");
    }
  }
  const sourceProvider = descriptors("interface", owner);
  const privateSource = object(sourceProvider.interface_driver_config?.private_config, "source provider config");
  if (!same(privateSource.client_projection_output_labels, labels)
    || !exactArray(privateSource.session_snapshot_labels, "source snapshot labels").includes(d.terminal_label)) {
    fail("source terminal/output labels differ from its selected provider");
  }
  const collectionId = component(d.collection_source, "collection source");
  const collection = descriptors("artifact", collectionId);
  if (collection.label !== "context.collection" || collection.schema_version !== 1 || collection.directory !== "entries") {
    fail("publication root requires a declared context.collection entries directory");
  }
  const entriesRoot = path.join(registryRoot, "artifact", collectionId, collection.directory);
  if (d.collection_entries_root !== entriesRoot) fail("publication root escapes its exact bound collection");
  if (!observerInput.include_context) {
    fail("observer collection is absent from its declared experience context");
  }
  for (const cp of observer.record.checkpoints.filter((cp) => observer.record.active_checkpoints.includes(cp.id)
    && cp.outbound_interface_bindings?.some((binding) => binding.interface === d.provider_interface)
    && cp.interface_binding?.interface === d.experience_interface)) {
    if (!cp.artifact_bindings?.some((binding) => binding.id === collectionId && binding.access?.read !== false)) {
      fail("observer input cannot read its bound collection");
    }
  }
  const publication = descriptors("interface", d.publication_door_interface);
  if (publication.interface_driver_config?.private_config?.memory_collection !== collectionId
    || publication.interface_driver_config?.host_slots?.registry_artifact_root?.path !== path.join(registryRoot, "artifact")) {
    fail("publication door differs from its declared collection");
  }
  const tool = descriptors("tool", component(d.publication_tool, "publication tool"));
  const authority = object(tool.process?.authority, "publication authority");
  if (tool.interface !== d.publication_door_interface || !same(authority.write_root_slots, ["registry_artifact_root"])
    || ["write_argument_paths", "optional_write_root_slots", "write_paths", "write_env_paths", "optional_write_env_paths", "write_env_subpaths"]
      .some((key) => authority[key] !== undefined && (!Array.isArray(authority[key]) || authority[key].length))) {
    fail("publication tool must write only through its door's collection backend");
  }
  const projector = descriptors("interface", d.projection_interface);
  if (projector.interface_type !== "system" || projector.interface_driver_ref !== "bound_context_projection") {
    fail("projection requires its declared System driver");
  }
  const config = object(projector.interface_driver_config?.private_config?.context_projection, "projection config");
  for (const [key, value] of Object.entries({ schema_version: 1, observer_run: d.run, projection_run: projected.record.id, source_run: target.id,
    source_worker_interface: target.worker, source_provider_interface: owner, source_client_interface: d.source_client_interface,
    source_relay_interface: d.source_relay_interface, terminal_label: d.terminal_label, collection_source: collectionId, publication_tool: d.publication_tool })) {
    if (config[key] !== value) fail(`projection ${key} differs from its declaration`);
  }
  const outcome = object(config.outcome, "outcome projection config");
  for (const [key, value] of Object.entries({ worker_interface: d.worker_interface, provider_interface: d.provider_interface,
    relay_interface: d.outcome_interface, experience_interface: d.experience_interface })) {
    if (outcome[key] !== value) fail(`outcome projection ${key} differs from its declaration`);
  }
  if (!same(outcome.input_routes, [{ interface_binding: observerInput.interface_binding }])
    || !same(projector.interface_driver_config.private_config.client_projection_output_labels,
      [d.input_label, d.outcome_label, ...(d.outcome_error_label === undefined ? [] : [d.outcome_error_label])])) {
    fail("outcome projection changes its exact experience input or output labels");
  }
  const sourceParams = { source_run: target.id, source_worker_interface: target.worker,
    source_provider_interface: owner, source_client_interface: d.source_client_interface };
  requirement(projectionInput, d.terminal_label, { stage: "terminal", source_provider_interface: owner });
  requirement(projectionOutput, d.input_label, { stage: "projection", ...sourceParams });
  requirement(observerInput, d.input_label, { stage: "projection", ...sourceParams });
  requirement(outcomeInput, d.terminal_label, { stage: "terminal", source_provider_interface: d.provider_interface });
  requirement(captureOutput, d.outcome_label, { source_run: target.id,
    provider_interface: d.provider_interface, publication_tool: d.publication_tool, entries_root: entriesRoot });
  return [observer, projected];
}

function recordInterfaces(record) {
  const ids = new Set([component(record.worker, "Run worker")]);
  const edges = (rows) => {
    if (!Array.isArray(rows)) fail("private topology checkpoints must be an array");
    for (const cp of rows) {
      if (cp.interface_binding) ids.add(component(cp.interface_binding.interface, "private checkpoint source"));
      if (!Array.isArray(cp.outbound_interface_bindings)) fail("private checkpoint returns must be an array");
      for (const binding of cp.outbound_interface_bindings) ids.add(component(binding.interface, "private checkpoint return"));
      if (cp.owner_authoring) for (const key of ["owner", "source", "door"]) ids.add(component(cp.owner_authoring[key], "private authoring owner"));
    }
  };
  edges(record.checkpoints);
  for (const [owner, state] of Object.entries(object(record.interface_state, "private Run state"))) {
    ids.add(component(owner, "private state owner"));
    if (state.run_authoring_binding) edges(state.run_authoring_binding.checkpoints);
    const writer = state.run_authoring_binding?.writer_run ?? state.writer_run;
    if (writer) {
      const initial = object(writer.initial_record, "private writer initial record");
      if (initial.id !== writer.run || initial.worker !== record.worker
        || initial.interface_state?.[owner]?.file_writer_target?.run !== record.id) fail("private writer changes its exact target/worker");
      edges(initial.checkpoints);
      for (const id of Object.keys(object(initial.interface_state, "private writer state"))) ids.add(component(id, "private writer owner"));
    }
  }
  return ids;
}

function validateCreationInventory(plan, source, declaration, observer, projection, root, descriptors) {
  const interfaces = new Set(declaration.lifecycle_ownership.private_interface_ids), tools = new Set(declaration.lifecycle_ownership.private_tool_ids);
  const declaredTools = new Set();
  for (const run of [observer, projection]) {
    const ceiling = object(descriptors("interface", run.worker, "policy.json").ceiling, "companion ceiling");
    for (const id of exactArray(ceiling.interfaces, "companion ceiling interfaces")) if (!interfaces.has(id)) fail("companion ceiling reaches outside its private inventory");
    if (!Array.isArray(ceiling.tools)) fail("companion ceiling tools must be an array");
    for (const id of ceiling.tools) declaredTools.add(component(id, "companion tool"));
  }
  if (!same([...tools].sort(), [...declaredTools].sort())) fail("private tool inventory differs from companion ceilings");
  const sourceCeiling = object(descriptors("interface", source.worker, "policy.json").ceiling, "source ceiling");
  if (!sourceCeiling.interfaces?.includes(declaration.source_relay_interface)) fail("source terminal relay is outside the source worker ceiling");
  const sourceInterfaces = new Set([...recordInterfaces(source), ...exactArray(sourceCeiling.interfaces, "source ceiling interfaces")]);
  if (!Array.isArray(sourceCeiling.tools)) fail("source ceiling tools must be an array");
  const sourceTools = new Set(sourceCeiling.tools.map((id) => component(id, "source tool")));
  const proposed = new Map(plan.interface_writes.map((write) => [write.path, write]));
  for (const [kind, ids] of [["interface", interfaces], ["tool", tools]]) {
    for (const id of ids) {
      const write = proposed.get(path.join(root, kind, id, `${kind}.json`));
      if (!write || write.expected_before_hash !== null) fail("private inventory owner lacks its exact new descriptor write");
      text(write.tag, "private owner write tag");
      const descriptor = descriptors(kind, id);
      if (kind === "tool" && !interfaces.has(descriptor.interface)) fail("private tool inventory has a foreign door");
      if (kind === "interface" && sourceInterfaces.has(id) && id !== declaration.source_relay_interface) fail("private inventory includes a source-owned interface");
      if (kind === "tool" && sourceTools.has(id)) fail("private inventory includes a source-owned tool");
    }
  }
  for (const write of plan.interface_writes) {
    const [kind, id, file, extra] = path.relative(root, write.path).split(path.sep);
    if (!["interface", "tool"].includes(kind) || !file || extra !== undefined) continue;
    const owned = (kind === "interface" ? interfaces : tools).has(id);
    if (owned && write.expected_before_hash !== null) fail("private companion write cannot replace an existing owner file");
    if (write.expected_before_hash === null && !owned && !(kind === "interface" ? sourceInterfaces : sourceTools).has(id)) {
      fail("new descriptor owner is absent from the exact source or companion inventory");
    }
  }
}

function initial(declaration, name) {
  const record = object(declaration.initial_record, `${name} initial record`);
  const content = declaration.initial_record_content;
  if (typeof content !== "string" || !content.trim()) fail(`${name} initial bytes must be a nonempty string`);
  if (record.id !== declaration.run || record.worker !== declaration.worker_interface
    || !same(JSON.parse(content), record)) fail(`${name} initial record changes its exact identity or bytes`);
  return { record, content };
}

function topology(record, provider) {
  const value = object(record.interface_state?.[provider]?.run_authoring_binding, "run authoring topology");
  if (value.schema_version !== 2 || value.run !== record.id || value.worker !== record.worker
    || value.provider_interface !== provider || !Array.isArray(value.checkpoints)) fail("run authoring topology changes its exact identity");
  const ids = exactArray(record.active_checkpoints, "active checkpoints");
  if (!Array.isArray(record.checkpoints) || new Set(record.checkpoints.map((cp) => cp.id)).size !== record.checkpoints.length
    || record.checkpoints.some((cp) => cp.run !== record.id) || ids.some((id) => !record.checkpoints.some((cp) => cp.id === id))) {
    fail("initial checkpoint identities differ from their exact run");
  }
  return value;
}

function route(record, topology, role, from, to, label) {
  const candidates = topology.checkpoints.filter((cp) => cp.role === role && cp.interface_binding?.interface === from
    && admits(cp.interface_binding, label) && cp.outbound_interface_bindings?.some((binding) => binding.interface === to && admits(binding, label)));
  if (candidates.length !== 1) fail(`no unique ${role} route from ${from} to ${to}`);
  const template = candidates[0];
  const active = record.checkpoints.filter((cp) => record.active_checkpoints.includes(cp.id)
    && same(cp.interface_binding, template.interface_binding)
    && same(cp.outbound_interface_bindings, template.outbound_interface_bindings)
    && same(cp.required_verified_artifacts ?? [], template.required_verified_artifacts ?? []));
  if (active.length !== 1) fail("active checkpoint differs from its exact rearm route");
  return template;
}

function requirement(template, label, params) {
  if (!template.required_verified_artifacts?.some((item) => item.artifact_label === label
    && typeof item.mark === "string" && item.mark && typeof item.verifier === "string" && item.verifier
    && Object.entries(params).every(([key, value]) => item.params?.[key] === value))) fail("maintenance route lacks its scoped verification requirement");
}

function onlyOutbound(template, id, label, restricted = false) {
  if (template.outbound_interface_bindings.length !== 1 || template.outbound_interface_bindings[0].interface !== id
    || !admits(template.outbound_interface_bindings[0], label)) fail("maintenance route has an unrelated outbound receiver");
  if (restricted) narrow(template.outbound_interface_bindings[0], id, label);
}

function admits(binding, label) {
  const policy = binding?.config?.artifact_policy ?? { mode: "all" };
  return policy.mode === "all" || policy.mode === "allow" && (policy.names?.includes(label)
    || policy.namespaces?.some((prefix) => label === prefix || label.startsWith(`${prefix}.`)));
}

function narrow(binding, id, label) {
  if (binding?.interface !== id || !same(binding.config?.artifact_policy, { mode: "allow", names: [label], namespaces: [] })) {
    fail("maintenance binding is not confined to its exact label");
  }
}

function descriptorReader(plan, root) {
  if (!path.isAbsolute(root) || path.normalize(root) !== root) fail("registry root must be an absolute normalized coordinate");
  const staged = new Map();
  for (const write of plan.interface_writes) {
    if (staged.has(write.path)) fail("planned descriptors repeat an exact path");
    staged.set(write.path, write.content);
  }
  return (kind, id, name = `${kind}.json`) => {
    const file = path.join(root, kind, component(id, "descriptor owner"), name);
    let content;
    if (staged.has(file)) content = staged.get(file);
    else {
      const realRoot = fs.realpathSync(root);
      let selected = realRoot;
      for (const part of [kind, id, name]) {
        selected = path.join(selected, part);
        if (fs.lstatSync(selected).isSymbolicLink()) fail("registered maintenance descriptor contains a symlink");
      }
      const fd = fs.openSync(selected, fs.constants.O_RDONLY | fs.constants.O_NOFOLLOW);
      try {
        const before = fs.fstatSync(fd);
        if (!before.isFile() || before.size > MAX_DESCRIPTOR_BYTES) fail("registered maintenance descriptor exceeds its regular-file bound");
        content = fs.readFileSync(fd, "utf8");
        const after = fs.fstatSync(fd), named = fs.lstatSync(selected);
        if (![after, named].every((stat) => ["dev", "ino", "size", "mtimeMs", "ctimeMs"].every((key) => stat[key] === before[key]))) {
          fail("registered maintenance descriptor changed during read");
        }
      } finally { fs.closeSync(fd); }
    }
    if (typeof content !== "string" || Buffer.byteLength(content) > MAX_DESCRIPTOR_BYTES) fail("maintenance descriptor exceeds its byte bound");
    const value = object(JSON.parse(content), "descriptor");
    if (value.id !== id) fail("maintenance descriptor owner differs from its exact path");
    return value;
  };
}

function object(value, name) {
  if (!value || typeof value !== "object" || Array.isArray(value)) fail(`${name} must be an object`);
  return value;
}
function text(value, name) {
  if (typeof value !== "string" || !value || value.trim() !== value || /[\u0000-\u001f\u007f]/.test(value)) fail(`${name} must be an exact nonempty string`);
  return value;
}
function component(value, name) {
  text(value, name);
  if (value.trim() !== value || value === "." || value === ".." || /[/\\\0]/.test(value)) fail(`${name} must be an exact path component`);
  return value;
}
function fields(value, keys, predecessors = []) {
  if (Object.keys(value).some((key) => !keys.includes(key) && !predecessors.includes(key)) || keys.some((key) => !Object.hasOwn(value, key))) fail("maintenance declaration has missing or unknown fields");
}
function distinct(values, name) { if (new Set(values).size !== values.length) fail(`${name} must be distinct`); }
function exactArray(value, name) {
  if (!Array.isArray(value) || !value.length) fail(`${name} must be a nonempty array`);
  for (const item of value) if (text(item, name).trim() !== item) fail(`${name} must contain exact strings`);
  distinct(value, name); return value;
}
function same(a, b) { return canonical(a) === canonical(b); }
function canonical(value) {
  if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`;
  if (value && typeof value === "object") return `{${Object.keys(value).sort().map((key) => `${JSON.stringify(key)}:${canonical(value[key])}`).join(",")}}`;
  return JSON.stringify(value);
}
function fail(message) { throw new Error(`maintenance ${message}`); }

module.exports = { decodeMaintenanceDeclaration, plannedMaintenanceRuns };
