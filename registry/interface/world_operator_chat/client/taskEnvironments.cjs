module.exports = { exactWorkerRoleArtifactBindings, lifecycleCheckpoint, nextLifecycleCheckpointEpoch, operatorTaskWorkerEnvironmentRecords, ownedOperatorTaskInterfaces, releaseOperatorTaskWorkerEnvironments, validateTaskWriterFormation, waitForWorkerEnvironmentRelease };

const fs = require("node:fs/promises");
const path = require("node:path");
const { readCompleteLedgerEndOffset, readCompleteLedgerTail } = require("./chatArtifacts.cjs");
const { WORKER_ENVIRONMENT_RELEASED_LABEL, WORKER_ENVIRONMENT_RELEASE_LABEL } = require("./constants.cjs");
const { chatIdentityFromRunRecord } = require("./conversation.cjs");
const { watchLedgerChange } = require("./liveCursors.cjs");
const { resolveRunRecordsRoot, writeContentAddressedPayload } = require("./operatorMessages.cjs");
const { commitClientWrites, waitForInterfaceSocket, withRunAuthoringLock } = require("./publication.cjs");
const { readConfig } = require("./roots.cjs");
const { canonicalJson, interfaceEndpoint } = require("./topologyPlan.cjs");
const { absolutePath, artifactLabel, assertAdmitted, contentHashHex, endpointInterfaceId, exactOutboundRuntime, numberValue, randomToken, readJsonDocumentIfPresent, readJsonIfPresent, requiredArray, requiredObject, requiredString, safeSegment, sha256Hex, stringValue } = require("./values.cjs");

async function releaseOperatorTaskWorkerEnvironments(options, request) {
  const config = await readConfig();
  const octopusHome = absolutePath(options.octopusHome(), "octopusHome");
  const runId = safeSegment(request?.runId, "runId");
  const expectedDigest = contentHashHex(request?.expectedDigest, "expectedDigest");
  const timeoutMs = numberValue(request?.timeoutMs, 45_000);
  const runsDir = await resolveRunRecordsRoot(octopusHome);
  const runPath = path.join(runsDir, `${runId}.json`);

  return withRunAuthoringLock(runId, async () => {
    const initial = await readJsonDocumentIfPresent(runPath);
    if (!initial) throw new Error(`operator chat task '${runId}' does not exist`);
    const initialDigest = sha256Hex(Buffer.from(initial.text));
    if (initialDigest !== expectedDigest) {
      throw new Error(`operator chat task '${runId}' changed before worker environments could be released`);
    }
    const identity = chatIdentityFromRunRecord(config, runId, initial.value);
    const clientState = requiredObject(
      initial.value.interface_state?.[identity.interfaceId],
      `operator chat task '${runId}' client state`,
    );
    if (clientState.client_surface_role !== "task") {
      throw new Error(`operator chat run '${runId}' is not a deletable task surface`);
    }
    const exactInterfaces = await ownedOperatorTaskInterfaces(octopusHome, identity.interfaceId);
    const formations = await operatorTaskWorkerEnvironmentRecords(
      octopusHome,
      initial.value,
      runId,
      exactInterfaces,
    );
    const pending = formations.filter((entry) => (
      entry.record.formation.requires_release === true
      && entry.record.lifecycle !== "released"
    ));
    if (pending.length === 0) {
      return {
        runId,
        expectedDigest: initialDigest,
        releasedWorkerEnvironmentIds: [],
      };
    }

    const movementsPath = path.join(octopusHome, "ledger", "movements.jsonl");
    const artifactRoot = path.join(
      absolutePath(options.clientArtifactsRoot(identity.interfaceId), "clientArtifactsRoot"),
      "operator-chat",
      "worker-environment-lifecycle",
    );
    await fs.mkdir(artifactRoot, { recursive: true });
    await waitForInterfaceSocket(octopusHome, identity.interfaceId, timeoutMs);
    const mandatoryRoleBindings = await exactWorkerRoleArtifactBindings(
      octopusHome,
      initial.value.worker,
    );

    let current = initial;
    let currentDigest = initialDigest;
    const releasedWorkerEnvironmentIds = [];
    for (const entry of pending) {
      const formation = entry.record.formation;
      const cycle = randomToken(20);
      const inputCheckpointId = `cp_task_release_input_${cycle}`;
      const outputCheckpointId = `cp_task_release_output_${cycle}`;
      const next = structuredClone(current.value);
      const retainedCheckpoints = requiredArray(next.checkpoints, "task checkpoints")
        .filter((checkpoint) => !stringValue(checkpoint?.id).startsWith("cp_task_release_"));
      const checkpointEpoch = nextLifecycleCheckpointEpoch(retainedCheckpoints);
      retainedCheckpoints.push(
        lifecycleCheckpoint(
          runId,
          inputCheckpointId,
          identity.interfaceId,
          formation.door_interface,
          checkpointEpoch,
          [WORKER_ENVIRONMENT_RELEASE_LABEL],
          mandatoryRoleBindings,
        ),
        lifecycleCheckpoint(
          runId,
          outputCheckpointId,
          formation.door_interface,
          identity.interfaceId,
          checkpointEpoch,
          [
            WORKER_ENVIRONMENT_RELEASED_LABEL,
            `${formation.door_interface}.failure`,
          ],
          mandatoryRoleBindings,
        ),
      );
      next.checkpoints = retainedCheckpoints;
      next.active_checkpoints = [inputCheckpointId, outputCheckpointId];
      requiredObject(
        next.interface_state?.[identity.interfaceId],
        `operator chat task '${runId}' client state`,
      ).cycle_checkpoint_ids = [inputCheckpointId, outputCheckpointId];
      const nextText = `${JSON.stringify(next, null, 2)}\n`;
      await commitClientWrites([{
        filePath: runPath,
        content: nextText,
        tag: `run-release:${runId}:${formation.worker_environment_id}`,
        expectedBeforeHash: currentDigest,
      }], options);
      current = { value: next, text: nextText };
      currentDigest = sha256Hex(Buffer.from(nextText));

      const ledgerOffset = await readCompleteLedgerEndOffset(movementsPath);
      const payloadPath = await writeContentAddressedPayload(
        artifactRoot,
        WORKER_ENVIRONMENT_RELEASE_LABEL,
        Buffer.from(JSON.stringify({
          worker_environment_id: formation.worker_environment_id,
        })),
      );
      const verdict = await options.handInArtifact({
        endpoint: interfaceEndpoint(octopusHome, identity.interfaceId),
        interfaceId: identity.interfaceId,
        manifest: {
          label: WORKER_ENVIRONMENT_RELEASE_LABEL,
          payload_path: payloadPath,
        },
        timeoutMs,
      });
      assertAdmitted(verdict, WORKER_ENVIRONMENT_RELEASE_LABEL);
      await waitForWorkerEnvironmentRelease({
        clientInterfaceId: identity.interfaceId,
        doorInterfaceId: formation.door_interface,
        formationPath: entry.path,
        movementsPath,
        outputCheckpointId,
        runId,
        startOffset: ledgerOffset,
        timeoutMs,
        workerEnvironmentId: formation.worker_environment_id,
      });
      releasedWorkerEnvironmentIds.push(formation.worker_environment_id);
    }

    return {
      runId,
      expectedDigest: currentDigest,
      releasedWorkerEnvironmentIds,
    };
  });
}

function lifecycleCheckpoint(
  runId,
  checkpointId,
  inboundInterfaceId,
  outboundInterfaceId,
  epoch,
  allowedArtifactNames,
  mandatoryRoleBindings,
) {
  const artifactPolicy = {
    mode: "allow",
    names: allowedArtifactNames,
    namespaces: [],
  };
  const binding = (interfaceId) => ({
    interface: interfaceId,
    config: { artifact_policy: artifactPolicy },
  });
  return {
    id: checkpointId,
    run: runId,
    trace: `trace:${runId}:${checkpointId}`,
    epoch,
    interface_binding: binding(inboundInterfaceId),
    outbound_interface_bindings: [binding(outboundInterfaceId)],
    artifact_bindings: mandatoryRoleBindings.map((binding) => ({ ...binding })),
    required_verified_artifacts: [],
  };
}

async function exactWorkerRoleArtifactBindings(octopusHome, workerId) {
  const worker = safeSegment(workerId, "operator task worker interface");
  const interfaceRoot = await fs.realpath(path.join(octopusHome, "registry", "interface"));
  const workerRoot = path.join(interfaceRoot, worker);
  const workerRootStat = await fs.lstat(workerRoot);
  if (workerRootStat.isSymbolicLink() || !workerRootStat.isDirectory()) {
    throw new Error(`operator task worker interface '${worker}' must be a plain registry directory`);
  }
  const policyPath = path.join(workerRoot, "policy.json");
  const policyStat = await fs.lstat(policyPath);
  if (policyStat.isSymbolicLink() || !policyStat.isFile()) {
    throw new Error(`operator task worker policy '${worker}' must be a plain file`);
  }
  const policy = await readJsonIfPresent(policyPath);
  if (!policy || policy.id !== worker) {
    throw new Error(`operator task worker policy '${worker}' does not match its exact registry folder`);
  }
  const seen = new Set();
  return requiredArray(policy.role_artifacts, `operator task worker policy '${worker}'.role_artifacts`)
    .map((artifact, index) => safeSegment(artifact, `operator task worker role_artifacts[${index}]`))
    .filter((artifact) => {
      if (seen.has(artifact)) return false;
      seen.add(artifact);
      return true;
    })
    .map((id) => ({ id }));
}

function nextLifecycleCheckpointEpoch(checkpoints) {
  let maximum = -1;
  for (const checkpoint of checkpoints) {
    const epoch = checkpoint?.epoch;
    if (!Number.isSafeInteger(epoch) || epoch < 0) {
      throw new Error("task checkpoint epoch must be a non-negative safe integer");
    }
    maximum = Math.max(maximum, epoch);
  }
  if (maximum >= Number.MAX_SAFE_INTEGER) throw new Error("task checkpoint epoch overflowed");
  return maximum + 1;
}

async function ownedOperatorTaskInterfaces(octopusHome, ownerInterfaceId) {
  const root = path.join(octopusHome, "registry", "interface");
  const rootStat = await fs.lstat(root);
  if (rootStat.isSymbolicLink() || !rootStat.isDirectory()) {
    throw new Error("operator task interface registry must be a plain directory");
  }
  const entries = await fs.readdir(root, { withFileTypes: true });
  const owned = new Set();
  for (const entry of entries) {
    if (!entry.isDirectory() || !/^[A-Za-z0-9_-]+$/.test(entry.name)) continue;
    const sidecarPath = path.join(root, entry.name, "octopus_os.json");
    const sidecarStat = await fs.lstat(sidecarPath).catch((error) => {
      if (error?.code === "ENOENT") return null;
      throw error;
    });
    if (!sidecarStat) continue;
    if (sidecarStat.isSymbolicLink() || !sidecarStat.isFile()) {
      throw new Error(`Octopus OS interface '${entry.name}' has a non-regular sidecar`);
    }
    const sidecar = await readJsonIfPresent(sidecarPath);
    if (!sidecar || sidecar.workspace !== "octopus_os") continue;
    if (sidecar.interface !== entry.name) {
      throw new Error(`Octopus OS interface '${entry.name}' has drifted sidecar identity`);
    }
    if (sidecar.owner_interface === ownerInterfaceId) owned.add(entry.name);
  }
  if (!owned.has(ownerInterfaceId)) {
    throw new Error(`operator chat task interface '${ownerInterfaceId}' owns no exact topology`);
  }
  return owned;
}

async function operatorTaskWorkerEnvironmentRecords(octopusHome, run, runId, exactInterfaces) {
  const worker = requiredString(run.worker, `operator chat task '${runId}' worker`);
  const records = [];
  for (const interfaceId of [...exactInterfaces].sort()) {
    const root = path.join(octopusHome, "interfaces", interfaceId, "worker-environments");
    let entries;
    try {
      const rootStat = await fs.lstat(root);
      if (rootStat.isSymbolicLink() || !rootStat.isDirectory()) {
        throw new Error(`worker-environment root for '${interfaceId}' must be a plain directory`);
      }
      entries = await fs.readdir(root, { withFileTypes: true });
    } catch (error) {
      if (error?.code === "ENOENT") continue;
      throw error;
    }
    for (const entry of entries) {
      if (!entry.isDirectory() || !/^we_[0-9a-f]{32}$/.test(entry.name)) {
        throw new Error(`worker-environment state under '${interfaceId}' has an invalid identity`);
      }
      const recordPath = path.join(root, entry.name, "worker_environment.json");
      const recordStat = await fs.lstat(recordPath);
      if (recordStat.isSymbolicLink() || !recordStat.isFile()) {
        throw new Error(`worker-environment record '${recordPath}' must be a regular file`);
      }
      const record = await readJsonIfPresent(recordPath);
      if (!record) throw new Error(`worker-environment record '${recordPath}' is absent`);
      const formation = requiredObject(record.formation, `worker-environment '${entry.name}' formation`);
      if (
        record.schema_version !== 5
        || formation.worker_environment_id !== entry.name
        || formation.door_interface !== interfaceId
        || !["active", "failed", "released"].includes(record.lifecycle)
      ) {
        throw new Error(`worker-environment record '${recordPath}' violates its lifecycle identity`);
      }
      if (formation.run !== runId) {
        await validateTaskWriterFormation(octopusHome, run, formation, exactInterfaces);
        if (formation.requires_release && record.lifecycle !== "released") {
          throw new Error(`task writer '${formation.run}' retains an unreleased worker environment`);
        }
      }
      if (formation.worker !== worker) {
        throw new Error(`worker-environment '${entry.name}' differs from task '${runId}' coordinates`);
      }
      if (typeof formation.requires_release !== "boolean") {
        throw new Error(`worker-environment '${entry.name}' has no release contract`);
      }
      records.push({ path: recordPath, record });
    }
  }
  return records.sort((left, right) => (
    left.record.formation.worker_environment_id
      .localeCompare(right.record.formation.worker_environment_id)
  ));
}

async function validateTaskWriterFormation(octopusHome, run, formation, exactInterfaces) {
  const writerId = safeSegment(formation.run, "task writer run");
  const declarations = Object.entries(requiredObject(run.interface_state, "task interface state"))
    .flatMap(([owner, state]) => {
      const binding = state?.run_authoring_binding?.writer_run ?? state?.writer_run;
      return binding?.run === writerId ? [{ owner, binding }] : [];
    });
  if (declarations.length !== 1) throw new Error(`task-owned interface '${formation.door_interface}' retains a formation for another run`);
  const { owner, binding } = declarations[0];
  if (Object.hasOwn(binding, "initial_record_format") && !["pretty", "compact"].includes(binding.initial_record_format)) {
    throw new Error("task writer Run declaration has invalid initial record format");
  }
  const initial = requiredObject(binding.initial_record, "task writer initial record");
  const target = initial.interface_state?.[owner]?.file_writer_target;
  if (!exactInterfaces.has(owner) || initial.id !== writerId
    || initial.worker !== run.worker || target?.run !== run.id || target.provider !== owner
    || target.generation !== 0 || target.door !== formation.door_interface
    || !exactInterfaces.has(target.source) || !exactInterfaces.has(target.door) || target.source === target.door) {
    throw new Error(`task writer '${writerId}' differs from its exact task declaration`);
  }
  const file = path.join(await resolveRunRecordsRoot(octopusHome), `${writerId}.json`);
  const stat = await fs.lstat(file);
  if (!stat.isFile() || stat.isSymbolicLink()) throw new Error(`task writer '${writerId}' must be a plain file`);
  const current = await readJsonIfPresent(file);
  const generation = current?.interface_state?.[owner]?.file_writer_target?.generation;
  if (!Number.isSafeInteger(generation) || generation < 0) throw new Error(`task writer '${writerId}' has invalid generation`);
  const expected = structuredClone(initial);
  const checkpoints = requiredArray(expected.checkpoints, "task writer checkpoints");
  const activeCheckpoints = requiredArray(expected.active_checkpoints, "task writer active checkpoints");
  const initialIds = checkpoints.map((checkpoint) => requiredString(checkpoint?.id, "task writer checkpoint id"));
  const activeIds = activeCheckpoints.map((checkpointId) => requiredString(checkpointId, "task writer active checkpoint id"));
  if (new Set(initialIds).size !== initialIds.length
    || canonicalJson([...initialIds].sort()) !== canonicalJson([...activeIds].sort())) {
    throw new Error(`task writer '${writerId}' has an unrelated active checkpoint`);
  }
  if (generation > 0) {
    expected.interface_state[owner].file_writer_target.generation = generation;
    for (const checkpoint of checkpoints) {
      checkpoint.id = `${checkpoint.id}_${generation}`;
      checkpoint.trace = `trace:${writerId}:${checkpoint.id}`;
    }
    expected.active_checkpoints = activeIds.map((checkpointId) => `${checkpointId}_${generation}`);
  }
  if (canonicalJson(current) !== canonicalJson(expected)) throw new Error(`task writer '${writerId}' drifted from its declared generation`);
}

async function waitForWorkerEnvironmentRelease(input) {
  const deadline = Date.now() + input.timeoutMs;
  let cursor = { offset: input.startOffset, seq: null };
  for (;;) {
    const remaining = Math.max(0, deadline - Date.now());
    const change = watchLedgerChange(input.movementsPath, remaining);
    const tail = await readCompleteLedgerTail(input.movementsPath, cursor.offset);
    cursor = { offset: tail.nextOffset, seq: null };
    for (const line of tail.lines) {
      let movement;
      try {
        movement = JSON.parse(line);
      } catch (error) {
        change.cancel();
        throw new Error(`movement ledger contains malformed JSON while awaiting release: ${error.message}`);
      }
      const runtime = exactOutboundRuntime(movement, input.clientInterfaceId);
      if (
        !runtime
        || runtime.run !== input.runId
        || runtime.checkpoint !== input.outputCheckpointId
        || endpointInterfaceId(movement.from) !== input.doorInterfaceId
      ) continue;
      const label = artifactLabel(requiredString(movement?.artifact?.id, "release artifact id"));
      change.cancel();
      if (label !== WORKER_ENVIRONMENT_RELEASED_LABEL) {
        throw new Error(
          `worker environment '${input.workerEnvironmentId}' release returned '${label}'`,
        );
      }
      const released = await readJsonIfPresent(input.formationPath);
      if (
        !released
        || released.formation?.worker_environment_id !== input.workerEnvironmentId
        || released.lifecycle !== "released"
      ) {
        throw new Error(
          `worker environment '${input.workerEnvironmentId}' emitted release without durable Released state`,
        );
      }
      return;
    }
    if (remaining === 0 || !(await change.promise)) {
      change.cancel();
      throw new Error(`worker environment '${input.workerEnvironmentId}' release timed out`);
    }
  }
}
