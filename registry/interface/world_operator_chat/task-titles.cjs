const fs = require("node:fs/promises");
const path = require("node:path");
const crypto = require("node:crypto");
const { isDeepStrictEqual: equal } = require("node:util");
const { setTimeout: delay } = require("node:timers/promises");
const { planClientMaintenanceEe } = require("./client-maintenance-ee.cjs");

const digest = (bytes) => crypto.createHash("sha256").update(bytes).digest("hex");
const json = async (file) => JSON.parse(await fs.readFile(file, "utf8"));
async function optional(file) { try { return await fs.readFile(file, "utf8"); } catch (error) { if (error.code === "ENOENT") return null; throw error; } }
function segment(value) {
  if (typeof value !== "string" || !/^[A-Za-z0-9][A-Za-z0-9._-]*$/.test(value)) throw new Error("Invalid client title identity");
  return value;
}

function identityFor(config, runId, sourceProvider, ee) {
  const suffix = digest(JSON.stringify([segment(runId), segment(sourceProvider)])).slice(0, 24);
  return { runId, sourceProvider, ee, service: `${config.client_template}_title_input_${suffix}`,
    result: `${config.client_template}_title_result_${suffix}`, worker: `${config.worker_template}_title_${suffix}`,
    provider: `${config.provider_template}_title_${suffix}`, run: `run_client_title_${suffix}` };
}

function recordFor(config, identity, request, files, completed = false) {
  const binding = (interfaceId, label) => ({ interface: interfaceId, config: { artifact_policy: { mode: "allow", names: [label], namespaces: [] } } });
  const checkpoint = (name, from, to, label, artifacts = []) => ({ id: `cp_${name}`, run: identity.run,
    trace: `trace:${identity.run}:${name}`, epoch: 0, interface_binding: binding(from, label),
    outbound_interface_bindings: to ? [binding(to, label)] : [], artifact_bindings: artifacts, required_verified_artifacts: [] });
  const checkpoints = [checkpoint("input", identity.service, identity.provider, config.request_label, [{ id: request.source.id }]),
    checkpoint("result", identity.result, null, config.result_label)];
  return { id: identity.run, ee: identity.ee, worker: identity.worker, trace: `trace:${identity.run}`, status: completed ? "idle" : "running",
    interface_state: { [identity.service]: { task_title: { source_run: identity.runId, source_provider: identity.sourceProvider, request, files } } },
    checkpoints, active_checkpoints: completed ? [] : checkpoints.map((row) => row.id) };
}

async function descriptors(registry, config, identity, sourceProvider) {
  const files = new Map();
  for (const [template, id, names] of [
    [config.client_template, identity.service, ["interface.json", "activation.json", "life.json"]],
    [config.client_template, identity.result, ["interface.json", "activation.json", "life.json"]],
    [config.worker_template, identity.worker, ["interface.json", "policy.json"]],
    [sourceProvider.id, identity.provider, ["interface.json", "life.json"]],
  ]) {
    for (const name of names) {
      const value = name === "interface.json" && id === identity.provider ? structuredClone(sourceProvider)
        : await json(path.join(registry, "interface", template, name));
      if (name === "interface.json" || name === "policy.json") value.id = id;
      if (name === "interface.json") value.copy_of = template === sourceProvider.id ? sourceProvider.copy_of ?? config.provider_template : template;
      if (name === "activation.json") value.interface = id;
      if (name === "life.json") for (const process of value.processes) {
        if (process.bin.startsWith("owner://")) process.bin = `interface://${template}/${process.bin.slice("owner://".length)}`;
      }
      if (name === "policy.json") value.ceiling.interfaces = [identity.worker, identity.service, identity.result, identity.provider];
      files.set(path.join("interface", id, name), value);
    }
  }
  return files;
}

const titleRequests = new Map();

function requestTaskTitle(options, input) {
  const key = JSON.stringify([options.octopusHome(), segment(input?.runId)]);
  if (titleRequests.has(key)) return titleRequests.get(key);
  const operation = generateTaskTitle(options, input).finally(() => {
    if (titleRequests.get(key) === operation) titleRequests.delete(key);
  });
  titleRequests.set(key, operation);
  return operation;
}

async function generateTaskTitle(options, input) {
  const config = await json(path.join(__dirname, "task-titles.json"));
  const home = options.octopusHome(), runId = segment(input?.runId), registry = path.join(home, "registry");
  const source = await options.readSource(runId);
  if (!source) return null;
  const sourcePath = path.join(home, "state", "runs", `${runId}.json`);
  if (await optional(sourcePath) === null) return null;
  const provider = segment(source.chat.provider_bound_interface_id);
  const selected = await json(path.join(registry, "interface", provider, "interface.json"));
  if (selected.id !== provider || selected.interface_driver_ref !== config.provider_driver
      || selected.interface_driver_config?.private_config?.opencodex) return null;
  const identity = identityFor(config, runId, provider, `ee_client_maintenance_${segment(source.chat.os_root_ee_id)}`);
  const request = { request_id: `title-${source.input.movement_seq}`, reply_to: identity.result,
    source: { id: source.input.artifact_id, content: [...Buffer.from(source.input.content_hash, "hex")] },
    source_movement_seq: source.input.movement_seq, text_pointer: config.text_pointer,
    instructions: config.instructions, output_schema: config.output_schema };
  if (!/^[0-9a-f]{64}$/.test(source.input.content_hash) || !Number.isSafeInteger(request.source_movement_seq)
      || request.source_movement_seq < 0) throw new Error("Title source is malformed");
  const runPath = path.join(home, "state", "runs", `${identity.run}.json`);
  let recordBytes = await optional(runPath), record = recordBytes === null ? null : JSON.parse(recordBytes);
  const prior = record?.interface_state?.[identity.service]?.task_title;
  if (record && (!prior || !equal(prior.request, request)
      || !equal(record, recordFor(config, identity, request, prior.files, record.status === "idle")))) {
    throw new Error("Client title run has another source or owner");
  }
  if (!record) {
    const prepared = await options.withRunLock(`prediction_${runId}`, () => options.withTopologyLock(home, async () => {
      if (await optional(sourcePath) === null) return false;
      const writes = [];
      const owned = await descriptors(registry, config, identity, selected);
      const files = Object.fromEntries([...owned].map(([file, value]) => [file, digest(JSON.stringify(value))]));
      for (const [file, next] of owned) {
        const existing = await optional(path.join(registry, file));
        if (existing !== null && !equal(JSON.parse(existing), next)) throw new Error(`Client title interface has another owner: ${file}`);
        if (existing === null) writes.push({ file: path.join(registry, file), next, beforeDigest: null });
      }
      writes.push(...await planClientMaintenanceEe(home, identity.ee, config.ee_template));
      record = recordFor(config, identity, request, files);
      writes.push({ file: runPath, next: record, beforeDigest: null });
      options.assertCurrent();
      await options.commit({ label: "Prepare task title", writes, removals: [], publishBarrierPath: runPath });
      recordBytes = await fs.readFile(runPath, "utf8");
      return true;
    }));
    if (!prepared) return null;
  }
  const readResult = async () => {
    const rows = await options.readArtifacts({ run_id: identity.run, interface_id: identity.result, labels: [config.result_label], after_seq: request.source_movement_seq });
    for (const row of rows) {
      if (row.error) throw new Error(row.error);
      if (row.movement_kind !== "InterfaceOutbound" || row.payload?.request?.request_id !== request.request_id) continue;
      if (!equal(row.payload.request, request) || typeof row.payload.result?.ok !== "boolean") throw new Error("Client title result has another source or invalid shape");
      return row.payload.result;
    }
    return null;
  };
  let result;
  try {
    result = await readResult();
    if (!result) {
      if (record.status === "idle") throw new Error("Completed title run has no result evidence");
      const payloadPath = path.join(home, "interface-hand-in", identity.service, "client", "request.json");
      await fs.mkdir(path.dirname(payloadPath), { recursive: true });
      const bytes = JSON.stringify(request), existing = await optional(payloadPath);
      if (existing !== null && existing !== bytes) throw new Error("Client title request changed after preparation");
      if (existing === null) await fs.writeFile(payloadPath, bytes, { flag: "wx", mode: 0o600 });
      await options.waitForInterface(home, identity.service, 20_000);
      options.assertCurrent();
      const verdict = await options.handIn({ interfaceId: identity.service, manifest: { label: config.request_label, payload_path: payloadPath }, timeoutMs: 20_000 });
      if (!Number.isSafeInteger(verdict?.admitted?.seq)) throw new Error("Client title request was refused");
      const deadline = Date.now() + 90_000;
      while (!result && Date.now() < deadline) { options.assertCurrent(); result = await readResult(); if (!result) await delay(200); }
      if (!result) throw new Error("Client title did not finish in time");
    }
  } finally {
    if (record.status !== "idle" && await optional(runPath) !== null) {
      options.assertCurrent();
      await options.commit({ label: "Finish task title", writes: [{ file: runPath,
        next: recordFor(config, identity, request, record.interface_state[identity.service].task_title.files, true),
        beforeDigest: digest(recordBytes) }], removals: [], publishBarrierPath: runPath });
    }
  }
  if (await optional(sourcePath) === null || await optional(runPath) === null) return null;
  if (!result.ok) throw new Error(result.error || "Task title generation failed");
  const title = result.value?.title;
  if (!result.value || Object.keys(result.value).join() !== "title" || typeof title !== "string"
      || !title.trim() || [...title].length > 150 || /[\r\n\u0000-\u001f]/.test(title)) throw new Error("Generated task title is malformed");
  const latest = await options.readSource(runId);
  if (!latest || latest.input.artifact_id !== source.input.artifact_id) return null;
  const sidecar = path.join(registry, "interface", segment(latest.chat.chat_interface_id), "octopus_os.json");
  const before = await fs.readFile(sidecar, "utf8"), next = JSON.parse(before);
  if (next.presentation?.name) return null;
  next.presentation = { projects: [], ...next.presentation, name: title.trim() };
  options.assertCurrent();
  await options.commit({ label: "Name task", writes: [{ file: sidecar, next, beforeDigest: digest(before) }], removals: [], publishBarrierPath: sidecar });
  return { title: title.trim(), runId };
}

async function readTaskTitleTopology(options) {
  const source = options.sourceRecord, provider = source.interface_state?.[options.sourceClientInterface]?.provider_runtime_interface;
  if (!provider) return null;
  const config = await json(path.join(__dirname, "task-titles.json"));
  let identity = identityFor(config, source.id, provider);
  const file = path.join(options.runRecordsRoot, `${identity.run}.json`);
  if (await optional(file) === null) return null;
  const current = await options.readDocument(file, "client title Run"), record = current.document;
  identity = { ...identity, ee: segment(record.ee) };
  const state = record.interface_state?.[identity.service]?.task_title;
  if (!state || !equal(record, recordFor(config, identity, state.request, state.files, record.status === "idle"))) throw new Error("Client title Run has another owner");
  const interfaces = new Set([identity.service, identity.result, identity.worker, identity.provider]);
  const expected = new Set([[identity.service, ["interface.json", "life.json", "activation.json"]],
    [identity.result, ["interface.json", "life.json", "activation.json"]], [identity.worker, ["interface.json", "policy.json"]],
    [identity.provider, ["interface.json", "life.json"]]].flatMap(([id, names]) => names.map(name => path.join("interface", id, name))));
  if (!state.files || Object.keys(state.files).length !== expected.size) throw new Error("Client title ownership inventory changed");
  for (const [relative, hash] of Object.entries(state.files)) {
    if (!expected.has(relative)) throw new Error("Client title ownership escapes its private interfaces");
    const value = (await options.readDocument(path.join(options.registryRoot, relative), "client title interface")).document;
    if (digest(JSON.stringify(value)) !== hash) throw new Error(`Client title interface changed: ${relative}`);
  }
  return { ...current, file, role: "title", provider: identity.provider, sourceProvider: provider, targetRun: source.id, interfaces };
}

module.exports = { requestTaskTitle, readTaskTitleTopology };
