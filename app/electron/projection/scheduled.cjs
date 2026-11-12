module.exports = { applyScheduledDraft, armSetupTriggers, mutateScheduledInterface, setupTriggerEntries, setupTriggerProof, setupTriggerState, setupTriggerTaskPlanner, withScheduleEditLock };

const { parseScheduledDueAt } = require("../scheduledTime.cjs");
const fs = require("node:fs/promises");
const crypto = require("node:crypto");
const path = require("node:path");
const { undoChangeJournal } = require("./changeJournal.cjs");
const { scheduleEditTails } = require("./constants.cjs");
const { parseCronActivationDocument, validateCronExpression, validateCronSchedule } = require("./cronActivation.cjs");
const { readOptionalRegularJsonDocument, readRegularJsonDocument, requirePlainDirectory, requireUnchangedJsonDigest } = require("./jsonFiles.cjs");
const { requirePrivatePublicationDirectory, runInterfaceReferences } = require("./taskDeletion.cjs");
const { arrayValue, atomicWriteJson, exactObjectKeys, requiredDigest, requiredSafeRegistryId, stringValue } = require("./values.cjs");

async function mutateScheduledInterface(registryRoot, octopusHomeRoot, request, hooks = {}) {
  const action = stringValue(request?.action);
  if (!["update", "set_enabled", "delete", "undo_changes"].includes(action)) throw new Error("scheduled mutation action is invalid");
  if (action === "undo_changes") return undoChangeJournal(registryRoot, octopusHomeRoot, request, hooks.commitClientAuthoring);
  const interfaceRoot = await fs.realpath(path.join(registryRoot, "interface"));
  const interfaceId = requiredSafeRegistryId(request?.interfaceId, "interfaceId");
  const interfaceDirectory = path.join(interfaceRoot, interfaceId);
  await requirePlainDirectory(interfaceDirectory, "scheduled interface directory");
  return withScheduleEditLock(interfaceDirectory, async () => {
    const activationPath = path.join(interfaceDirectory, "activation.json");
    const { document, digest } = await readRegularJsonDocument(activationPath, "activation document");
    const expectedDigest = requiredDigest(request?.expectedDigest, "expectedDigest");
    if (digest !== expectedDigest) throw new Error("scheduled interface changed before the edit could be applied");
    const parsed = parseCronActivationDocument(document, interfaceId);
    if (action === "delete") {
      await requireUnchangedJsonDigest(activationPath, digest, "activation document");
      return deleteTrigger(interfaceDirectory, interfaceId, hooks.commitClientAuthoring);
    }
    const next = structuredClone(document);
    const nextActivation = next.activations[parsed.activationIndex];
    if (action === "set_enabled") {
      if (typeof request.enabled !== "boolean") throw new Error("enabled must be boolean");
      nextActivation.enabled = request.enabled;
    } else {
      applyScheduledDraft(nextActivation.config.schedule, request?.draft, false);
    }
    parseCronActivationDocument(next, interfaceId);
    await requireUnchangedJsonDigest(activationPath, digest, "activation document");
    await atomicWriteJson(activationPath, next);
    return {
      action,
      interfaceId,
      activationDigest: crypto.createHash("sha256").update(await fs.readFile(activationPath)).digest("hex"),
    };
  });
}

async function deleteTrigger(interfaceDirectory, interfaceId, commitClientAuthoring) {
  if (typeof commitClientAuthoring !== "function") throw new Error("client authoring transaction is unavailable");
  const writes = [];
  for (const entry of await fs.readdir(interfaceDirectory, { withFileTypes: true })) {
    if (entry.name === ".octopus-publication") { await requirePrivatePublicationDirectory(path.join(interfaceDirectory, entry.name)); continue; }
    if (!entry.isFile()) throw new Error(`trigger ${interfaceId} holds ${entry.name}, which is not a regular file`);
    const file = path.join(interfaceDirectory, entry.name);
    writes.push({ file, beforeDigest: crypto.createHash("sha256").update(await fs.readFile(file)).digest("hex"), next: null });
  }
  const barrier = path.join(interfaceDirectory, "interface.json");
  if (!writes.some((write) => write.file === barrier)) throw new Error(`trigger ${interfaceId} has no descriptor`);
  writes.sort((left, right) => Number(left.file === barrier) - Number(right.file === barrier));
  await commitClientAuthoring({ label: `delete trigger ${interfaceId}`, writes, removals: [], publishBarrierPath: barrier });
  return { action: "delete", interfaceId, deleted: true };
}

async function triggerCopyWrites(interfaceRoot, sourceInterfaceId, interfaceId, draft) {
  const sourceDirectory = path.join(interfaceRoot, sourceInterfaceId);
  await requirePlainDirectory(sourceDirectory, `trigger template ${sourceInterfaceId}`);
  const target = path.join(interfaceRoot, interfaceId);
  if (await fs.lstat(target).catch((error) => error?.code === "ENOENT" ? null : Promise.reject(error))) throw new Error(`interface already exists: ${interfaceId}`);
  const descriptor = await readRegularJsonDocument(path.join(sourceDirectory, "interface.json"), `trigger template ${sourceInterfaceId}`);
  if (descriptor.document?.id !== sourceInterfaceId) throw new Error(`trigger template ${sourceInterfaceId} does not match its folder`);
  const life = await readRegularJsonDocument(path.join(sourceDirectory, "life.json"), `trigger template ${sourceInterfaceId} life`);
  const activation = await readRegularJsonDocument(path.join(sourceDirectory, "activation.json"), `trigger template ${sourceInterfaceId} activation`);
  const parsed = parseCronActivationDocument(activation.document, sourceInterfaceId);
  const copy = structuredClone(activation.document.activations[parsed.activationIndex]);
  copy.enabled = true;
  applyScheduledDraft(copy.config.schedule, draft, true);
  const activationDocument = { schema_version: 1, interface: interfaceId, activations: [copy] };
  parseCronActivationDocument(activationDocument, interfaceId);
  return [
    { file: path.join(target, "interface.json"), beforeDigest: null, next: { ...descriptor.document, id: interfaceId, copy_of: descriptor.document.copy_of ?? sourceInterfaceId } },
    { file: path.join(target, "life.json"), beforeDigest: null, next: life.document },
    { file: path.join(target, "activation.json"), beforeDigest: null, next: activationDocument },
  ];
}

function setupTriggerEntries(pack, selectedRecommended) {
  const chosen = new Set(arrayValue(selectedRecommended));
  const selected = new Set(arrayValue(pack?.components)
    .filter((component) => component.selection !== "recommended" || chosen.has(component.package_id))
    .map((component) => component.package_id));
  return arrayValue(pack?.workspace_setup?.triggers)
    .filter((entry) => selected.has(entry.package_id))
    .map((entry) => ({
      package_id: entry.package_id,
      template_interface_id: entry.template_interface_id,
      interface_id: entry.interface_id,
      quiet_period_seconds: entry.quiet_period_seconds,
      task: entry.task,
    }))
    .sort((left, right) => left.interface_id.localeCompare(right.interface_id));
}

async function boundTriggerRuns(octopusHomeRoot, interfaceId) {
  const runRoot = path.join(octopusHomeRoot, "state", "runs");
  const runs = [];
  for (const entry of await fs.readdir(runRoot, { withFileTypes: true }).catch((error) => error?.code === "ENOENT" ? [] : Promise.reject(error))) {
    if (!entry.isFile() || !entry.name.endsWith(".json")) continue;
    const run = await readOptionalRegularJsonDocument(path.join(runRoot, entry.name), `run ${entry.name}`);
    if (run && runInterfaceReferences(run.document).has(interfaceId)) runs.push(stringValue(run.document.id));
  }
  return runs.sort();
}

async function setupTriggerState(interfaceRoot, octopusHomeRoot, entry) {
  const interfaceId = requiredSafeRegistryId(entry?.interface_id, "setup trigger interface id");
  const directory = await fs.lstat(path.join(interfaceRoot, interfaceId)).catch((error) => error?.code === "ENOENT" ? null : Promise.reject(error));
  if (!directory) return null;
  const activation = await readRegularJsonDocument(path.join(interfaceRoot, interfaceId, "activation.json"), `setup trigger ${interfaceId} activation`);
  const parsed = parseCronActivationDocument(activation.document, interfaceId);
  if (parsed.activation.enabled !== true || parsed.config.schedule.inactivity?.seconds !== entry.quiet_period_seconds) {
    throw new Error(`setup trigger ${interfaceId} exists but is not the enabled quiet-period trigger the pack declares`);
  }
  const runs = await boundTriggerRuns(octopusHomeRoot, interfaceId);
  if (runs.length === 0) return null;
  return {
    interface_id: interfaceId,
    template_interface_id: entry.template_interface_id,
    run_ids: runs,
    quiet_period_seconds: entry.quiet_period_seconds,
    activation_digest: activation.digest,
  };
}

async function setupTriggerProof(registryRoot, octopusHomeRoot, entries) {
  const interfaceRoot = await fs.realpath(path.join(registryRoot, "interface"));
  const proven = [];
  for (const entry of entries) {
    const state = await setupTriggerState(interfaceRoot, octopusHomeRoot, entry);
    if (!state) return null;
    proven.push(state);
  }
  return proven;
}

function setupTriggerTaskPlanner(planSurface, { eeInstancesRoot, rootEeId, home }) {
  const required = (value, field) => {
    const text = stringValue(value);
    if (!text) throw new Error(`${field} is required`);
    return text;
  };
  return async (entry, externalInput) => {
    const surface = await planSurface({
      eeInstancesRoot,
      request: {
        chatId: `trigger_${entry.interface_id}`,
        cycle: `trigger_${entry.interface_id}`,
        clientSurfaceRole: "task",
        eeId: required(rootEeId, "Home root EE"),
        providerInterfaceId: required(home?.providerInterfaceId, "Home provider interface"),
        providerModel: required(home?.providerModel, "Home provider model"),
        reasoningEffort: home?.reasoningEffort,
        serviceTier: home?.serviceTier,
        workerInterfaceId: entry.task.worker_interface_id,
        doorHostSlots: entry.task.door_host_slots ?? {},
        contextArtifactBindings: entry.task.context_artifact_bindings,
        selectedWorkerEnvironmentKinds: entry.task.selected_worker_environment_kinds,
        externalInputs: [externalInput],
      },
    });
    return {
      runId: surface.runId,
      writes: surface.writes.map((write) => ({ file: write.filePath, beforeDigest: write.expectedBeforeHash, content: write.content, label: write.tag })),
    };
  };
}

async function armSetupTriggers(registryRoot, octopusHomeRoot, entries, hooks = {}) {
  if (typeof hooks.commitClientAuthoring !== "function" || typeof hooks.planTriggerTask !== "function") {
    throw new Error("setup triggers require the client authoring transaction and the task planner");
  }
  const interfaceRoot = await fs.realpath(path.join(registryRoot, "interface"));
  const armed = [];
  for (const entry of entries) {
    if (await setupTriggerState(interfaceRoot, octopusHomeRoot, entry)) continue;
    const interfaceId = requiredSafeRegistryId(entry.interface_id, "setup trigger interface id");
    const sourceInterfaceId = requiredSafeRegistryId(entry.template_interface_id, "setup trigger template");
    if (!await fs.lstat(path.join(interfaceRoot, interfaceId)).catch((error) => error?.code === "ENOENT" ? null : Promise.reject(error))) {
      const source = await readRegularJsonDocument(path.join(interfaceRoot, sourceInterfaceId, "activation.json"), `setup trigger template ${sourceInterfaceId}`);
      const schedule = parseCronActivationDocument(source.document, sourceInterfaceId).config.schedule;
      if (!schedule.inactivity) throw new Error(`setup trigger template ${sourceInterfaceId} is not a quiet-period trigger`);
      if (!Number.isSafeInteger(entry.quiet_period_seconds) || entry.quiet_period_seconds < 60) throw new Error(`setup trigger ${interfaceId} quiet period must be at least one minute`);
      const writes = await triggerCopyWrites(interfaceRoot, sourceInterfaceId, interfaceId, {
        type: "inactivity",
        inactivity: { ...structuredClone(schedule.inactivity), seconds: entry.quiet_period_seconds },
        timezone: schedule.timezone || "local",
        fireOnStart: false,
        instructions: schedule.reminder_default,
      });
      await hooks.commitClientAuthoring({ label: `create trigger ${interfaceId}`, writes, removals: [], publishBarrierPath: writes.at(-1).file });
    }
    const task = await hooks.planTriggerTask(entry, {
      checkpointId: `cp_${interfaceId}`,
      interfaceId,
      artifactNames: ["cron.current_time"],
    });
    await hooks.commitClientAuthoring({ label: `create the task of trigger ${interfaceId}`, writes: task.writes, removals: [], publishBarrierPath: task.writes.at(-1).file });
    armed.push({ action: "create", interfaceId, runId: task.runId });
  }
  return armed;
}

function applyScheduledDraft(schedule, draft, creating) {
  exactObjectKeys(draft, ["type", "intervalSeconds", "inactivity", "cron", "dueAt", "timezone", "fireOnStart", "instructions"], ["type", "timezone", "fireOnStart", "instructions"], "scheduled draft");
  delete schedule.inactivity;
  if (draft.type === "inactivity") {
    schedule.inactivity = structuredClone(draft.inactivity);
    schedule.interval_seconds = null;
    schedule.cron = null;
    delete schedule.due_at;
  } else if (draft.type === "interval") {
    if (typeof draft.intervalSeconds !== "number" || !Number.isFinite(draft.intervalSeconds) || draft.intervalSeconds <= 0) throw new Error("intervalSeconds must be a finite positive number");
    schedule.interval_seconds = draft.intervalSeconds;
    schedule.cron = null;
    delete schedule.due_at;
  } else if (draft.type === "cron") {
    validateCronExpression(draft.cron);
    schedule.interval_seconds = null;
    schedule.cron = draft.cron.trim().replace(/\s+/g, " ");
    delete schedule.due_at;
  } else if (draft.type === "once") {
    parseScheduledDueAt(draft.dueAt);
    schedule.interval_seconds = null;
    schedule.cron = null;
    schedule.due_at = draft.dueAt;
  } else {
    throw new Error("scheduled draft type must be interval, cron, once or inactivity");
  }
  schedule.timezone = stringValue(draft.timezone);
  schedule.fire_on_start = draft.fireOnStart;
  schedule.reminder_default = draft.instructions;
  if (creating) schedule.reminder_env = "";
  validateCronSchedule(schedule);
}

async function withScheduleEditLock(directory, action) {
  const key = await fs.realpath(directory);
  const previous = scheduleEditTails.get(key) ?? Promise.resolve();
  const current = previous.then(action);
  const settled = current.then(() => undefined, () => undefined);
  scheduleEditTails.set(key, settled);
  try {
    return await current;
  } finally {
    if (scheduleEditTails.get(key) === settled) scheduleEditTails.delete(key);
  }
}
