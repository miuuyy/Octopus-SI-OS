module.exports = { applicationPrivateConfigurationProjection, baseInterfaceRows, bindingRows, descendantEnvironmentIds, descriptorInventoryRows, descriptorKindRows, descriptorRows, eeInstanceRows, eeTemplateDescriptorRows, eeTemplateRows, envelopeRows, environmentLineage, filterInterfaces, goalRows, interfaceActivationEntries, isExactGitWorktreeRoot, isOperatorChatProviderFailureRow, isOperatorChatProviderMovement, lowestCommonEnvironment, markKindRows, markRows, normalizeDescriptorRow, octopusOsInterfaceSidecars, operatorOsProjectRows, policyRows, preserveProjectionEvidenceError, projectedFilesystemRootSurfaces, projectedGitSurfaces, projectedRunInterfaceIds, projectedWorktreeStatusSurfaces, projectionMemo, projectionSnapshot, projectionTruthChange, readEeInstanceRecords, readHostRootRefs, readRunRecords, recordCheckpointRowsFor, requiredVerifiedArtifactOutcome, requiredVerifiedArtifactOutcomeIndex, rootSlotLabel, runCheckpointRows, runRows, toolRows, unavailableViewerSurface, validAddressWithProtocols, validHttpAddress, validWebSocketAddress, validWorkerEnvironmentLiveState, validateEeInstancePolicyNarrowing, validateEePolicyProjectionSnapshot, validateProjectedEePolicy, validateProjectedRunRecordSemantics, validateProjectedWorkerInterfacePolicies, verifierRows, withoutRawProjectionEvidence, workerEnvironmentCapabilities, workerEnvironmentLiveAvailable, workerEnvironmentRows, workerEnvironmentSession, workerEnvironmentWorkspace, workerRows };

const fs = require("node:fs/promises");
const path = require("node:path");
const { DESCRIPTOR_REGISTRIES, INTERFACE_PRIVATE_ENVIRONMENT_FILE, ProjectionEvidenceError, runRecordSchemaValidator } = require("./constants.cjs");
const { readRegularJsonDocument } = require("./jsonFiles.cjs");
const { formatCanonicalSchemaErrors, interfaceEndpointName, ledgerRowsForRun, movementBaseKind, movementBoundaryInterface, movementRunId } = require("./movementRows.cjs");
const { operatorChatRuntimeProviderId } = require("./operatorChatEvents.cjs");
const { listDirectFiles } = require("./providerDescriptors.cjs");
const { resolveAuthoredApplicationSetupFields } = require("./sidecars.cjs");
const { readClientTaskArchive } = require("./taskArchive.cjs");
const { arrayValue, capSetStrings, descriptorIdentity, exactObjectKeys, filterByString, interfaceBindingArtifactNames, interfaceBindingId, isEeInstanceJsonPath, isPlainRecord, isRunRecordJsonPath, isSortedUniqueStrings, ledgerArtifactId, ledgerArtifactLabel, numberValue, objectOrEmpty, requiredSafeRegistryId, rowTimeMs, stringValue, timestampOf, validateAuthoredInterfaceBinding } = require("./values.cjs");

function projectionTruthChange(filename) {
  if (filename === null || filename === undefined) return "structure";
  const relative = Buffer.isBuffer(filename) ? filename.toString("utf8") : String(filename);
  const segments = relative.split(/[\\/]/).filter(Boolean);
  if (segments.length === 0) return "structure";
  if (segments.length === 1 && segments[0] === "host.json") return "structure";
  if (segments[0] === "registry") {
    return segments.length <= 3 || (segments.length === 4 && segments[3].endsWith(".json")) ? "policy" : "files";
  }
  if (segments[0] === "state" && segments[1] === "runs") {
    return segments.length === 3 && segments[2].endsWith(".json") ? "runs" : null;
  }
  if (segments[0] === "state" && segments[1] === "change-journals") {
    return segments.length === 4 && segments[3] === "journal.json" ? "runs" : null;
  }
  if (segments[0] === "state" && segments[1] === "ee_instances") {
    return segments.length === 3 && segments[2].endsWith(".json") ? "policy" : null;
  }
  if (segments[0] === "ledger") {
    return segments.length <= 2 && segments[1] === "movements.jsonl"
      ? "ledger"
      : null;
  }
  if (segments[0] === "app-server-control") {
    return segments.length === 2 ? "runs" : null;
  }
  if (segments[0] === "client" && segments[1] === "package-state") {
    if (segments.length === 3 && ["maintenance-lock.sqlite", "maintenance-lock.sqlite-journal", "maintenance-lock.sqlite-wal", "maintenance-lock.sqlite-shm"].includes(segments[2])) return null;
    return "structure";
  }
  if (segments[0] === "client" && segments[1] === "agents") {
    return segments.length === 3 && segments[2] === "catalog.json" ? "structure" : null;
  }
  if (segments[0] === "client-state" && segments[1] === "workspace") {
    if (segments.length === 3 && /^transaction-lock\.sqlite(?:-journal|-wal|-shm)?$/.test(segments[2])) return null;
    return "runs";
  }
  if (segments[0] === "interfaces") {
    if (
      segments.length === 3
      && /^ledger-cursor(?:\.[A-Za-z0-9_-]+)?\.json$/.test(segments[2])
    ) return null;
    if (segments.length === 3 && segments[2].endsWith(".json")) {
      return segments[2] === INTERFACE_PRIVATE_ENVIRONMENT_FILE ? "structure" : "runs";
    }
    return segments[2] === "worker-environments"
      && segments.length === 5
      && ["worker_environment.json", "live_state.json"].includes(segments[4])
      ? "runs"
      : null;
  }
  return null;
}

function projectionMemo(projection, tier, key, create) {
  const cache = tier === "structure" ? projection.structureMemo : projection.ledgerMemo;
  if (cache.has(key)) return cache.get(key);
  const value = create();
  cache.set(key, value);
  return value;
}

function recordCheckpointRowsFor(
  projection,
  runId,
  outcomeIndex = requiredVerifiedArtifactOutcomeIndex(projection),
  recordByRun = null,
) {
  const record = recordByRun?.get(runId)
    ?? arrayValue(projection.runRecords).find((row) => row.run_id === runId);
  if (!record) return null;
  return arrayValue(record.checkpoints).map((checkpoint) => ({
    ...checkpoint,
    required_verified_artifacts: arrayValue(checkpoint.required_verified_artifacts).map((required) => ({
      ...required,
      outcome: requiredVerifiedArtifactOutcome(outcomeIndex, runId, required.mark),
    })),
  }));
}

function requiredVerifiedArtifactOutcomeIndex(projection) {
  return projectionMemo(projection, "ledger", "requiredVerifiedArtifactOutcomeIndex", () => {
    const outcomes = new Map();
    for (const row of projection.ledgerRows) {
      const runId = movementRunId(row);
      if (!runId) continue;
      const admitted = /^MarkOutcomeAdmitted\((.+)\)$/.exec(stringValue(row.kind));
      if (admitted?.[1]) {
        outcomes.set(`${runId}\u0000${admitted[1]}`, "closed");
        continue;
      }
      if (row.kind !== "VerificationFailed") continue;
      const markId = stringValue(objectOrEmpty(objectOrEmpty(row.invocation_metadata).verification_failure).mark);
      if (!markId) continue;
      const key = `${runId}\u0000${markId}`;
      if (outcomes.get(key) !== "closed") outcomes.set(key, "failed");
    }
    return outcomes;
  });
}

function requiredVerifiedArtifactOutcome(outcomeIndex, runId, markId) {
  if (!markId) return "declared";
  return outcomeIndex.get(`${runId}\u0000${markId}`) ?? "declared";
}

function runCheckpointRows(projection, runId) {
  return projectionMemo(projection, "ledger", `runCheckpointRows:${runId}`, () => {
    const rows = ledgerRowsForRun(projection, runId).sort((a, b) => rowTimeMs(a) - rowTimeMs(b));
    const checkpoints = new Map();
    for (const row of rows) {
      const runtime = objectOrEmpty(objectOrEmpty(row.invocation_metadata).runtime);
      if (stringValue(runtime.run) !== runId) continue;
      const checkpointId = stringValue(runtime.checkpoint);
      if (!checkpointId) continue;
      const current = checkpoints.get(checkpointId) ?? {
        checkpoint_id: checkpointId,
        label: checkpointId,
        status: "made",
        interface: "",
        out_interfaces: [],
        artifact_label: "",
        artifact_bindings: 0,
        interface_bindings: 0,
        recorded_at_unix_ms: rowTimeMs(row),
      };
      current.artifact_bindings += row.artifact ? 1 : 0;
      if (movementBaseKind(row) === "InterfaceArrival") {
        const interfaceId = interfaceEndpointName(row.from);
        current.interface = interfaceId || current.interface;
        current.artifact_label = ledgerArtifactLabel(row) || current.artifact_label;
      }
      if (movementBaseKind(row) === "InterfaceOutbound") {
        const interfaceId = interfaceEndpointName(row.to);
        if (interfaceId && !current.out_interfaces.includes(interfaceId)) current.out_interfaces.push(interfaceId);
      }
      current.interface_bindings = (current.interface ? 1 : 0) + current.out_interfaces.length;
      checkpoints.set(checkpointId, current);
    }
    return Array.from(checkpoints.values());
  });
}

async function readHostRootRefs(homeRoot) {
  const file = path.join(homeRoot, "host.json");
  const { document } = await readRegularJsonDocument(file, `host manifest ${file}`);
  const keys = Object.keys(document).sort();
  if (keys.length !== 2 || keys[0] !== "host_manifest_version" || keys[1] !== "root_refs") {
    throw new Error(`host manifest has unknown or missing fields: ${file}`);
  }
  if (document.host_manifest_version !== 1) {
    throw new Error(`host manifest has unsupported host_manifest_version '${String(document.host_manifest_version)}': ${file}`);
  }
  if (!isPlainRecord(document.root_refs)) {
    throw new Error(`host manifest root_refs is not an object: ${file}`);
  }
  const entries = Object.entries(document.root_refs).sort(([left], [right]) => left.localeCompare(right));
  for (const [slot, root] of entries) {
    requiredSafeRegistryId(slot, `host manifest root_refs slot in ${file}`);
    if (typeof root !== "string" || !path.isAbsolute(root)) {
      throw new Error(`host manifest root_refs.${slot} is not an absolute path: ${file}`);
    }
  }
  return Object.fromEntries(entries);
}

function validateEePolicyProjectionSnapshot(value) {
  exactObjectKeys(value, ["environments", "issues"], ["environments", "issues"], "EE policy projection");
  if (!Array.isArray(value.environments) || !Array.isArray(value.issues)) {
    throw new Error("EE policy projection fields must be arrays");
  }
  if (value.issues.some((issue) => typeof issue !== "string" || !issue.trim())) {
    throw new Error("EE policy projection issues must be non-empty strings");
  }
  const seen = new Set();
  const environments = value.environments.map((row) => {
    exactObjectKeys(
      row,
      ["ee_id", "effective_policy", "policy_ceiling", "workers"],
      ["ee_id", "effective_policy", "policy_ceiling", "workers"],
      "EE policy projection row",
    );
    const eeId = requiredSafeRegistryId(row.ee_id, "EE policy projection ee_id");
    if (seen.has(eeId)) throw new Error(`EE policy projection repeats '${eeId}'`);
    seen.add(eeId);
    return {
      ee_id: eeId,
      formed_envelope: validateProjectedEePolicy(row.effective_policy, `${eeId}.effective_policy`),
      boundary_envelope: validateProjectedEePolicy(row.policy_ceiling, `${eeId}.policy_ceiling`),
      workers: validateProjectedWorkerInterfacePolicies(row.workers, `${eeId}.workers`),
    };
  });
  return { environments, issues: [...value.issues] };
}

function validateProjectedWorkerInterfacePolicies(value, label) {
  if (!Array.isArray(value)) throw new Error(`${label} must be an array`);
  let previous = "";
  return value.map((row, index) => {
    exactObjectKeys(
      row,
      ["worker_interface_id", "interface_policy"],
      ["worker_interface_id", "interface_policy"],
      `${label}[${index}]`,
    );
    const workerInterfaceId = requiredSafeRegistryId(
      row.worker_interface_id,
      `${label}[${index}].worker_interface_id`,
    );
    if (previous && previous >= workerInterfaceId) {
      throw new Error(`${label} must be sorted by unique worker_interface_id`);
    }
    previous = workerInterfaceId;
    if (!isPlainRecord(row.interface_policy)) {
      throw new Error(`${label}[${index}].interface_policy must be a JSON object`);
    }
    const interfacePolicy = {};
    for (const [interfaceId, policy] of Object.entries(row.interface_policy)) {
      const exactInterfaceId = requiredSafeRegistryId(
        interfaceId,
        `${label}[${index}].interface_policy interface id`,
      );
      if (!isPlainRecord(policy)) {
        throw new Error(`${label}[${index}].interface_policy.${exactInterfaceId} must be a JSON object`);
      }
      interfacePolicy[exactInterfaceId] = structuredClone(policy);
    }
    return {
      worker_interface_id: workerInterfaceId,
      interface_policy: interfacePolicy,
    };
  });
}

function validateProjectedEePolicy(value, label) {
  const fields = [
    "allowed_interfaces",
    "allowed_tools",
    "allowed_worker_environment_kinds",
    "allowed_root_slots",
    "allowed_artifact_verbs",
    "hard_blocked_tools",
  ];
  exactObjectKeys(value, [...fields, "allowed_mark_outcomes"], fields, label);
  for (const field of fields) {
    if (field === "allowed_root_slots" && value[field] === null) continue;
    if (!isSortedUniqueStrings(value[field], false)) {
      throw new Error(`${label}.${field} must be a sorted unique string array`);
    }
  }
  if (value.allowed_artifact_verbs.some((verb) => !["read", "change", "create", "delete"].includes(verb))) {
    throw new Error(`${label}.allowed_artifact_verbs contains an unknown verb`);
  }
  if (Object.prototype.hasOwnProperty.call(value, "allowed_mark_outcomes")
    && value.allowed_mark_outcomes !== null
    && !isSortedUniqueStrings(value.allowed_mark_outcomes, false)) {
    throw new Error(`${label}.allowed_mark_outcomes must be a sorted unique string array`);
  }
  return structuredClone(value);
}

async function readRunRecords(runRecordsRoot, warnings = []) {
  const files = await listDirectFiles(runRecordsRoot, isRunRecordJsonPath);
  const records = [];
  const invalidRecords = [];
  for (const file of Array.from(new Set(files)).sort()) {
    let record;
    let recordDigest;
    try {
      const document = await readRegularJsonDocument(file, `run record ${file}`);
      record = document.document;
      recordDigest = document.digest;
    } catch (error) {
      const message = String(error?.message ?? error);
      warnings.push(message);
      invalidRecords.push({ source_path: file, error: message });
      continue;
    }
    const schemaPassed = runRecordSchemaValidator(record);
    let semanticError = "";
    if (schemaPassed) {
      try {
        validateProjectedRunRecordSemantics(record);
      } catch (error) {
        semanticError = String(error?.message ?? error);
      }
    }
    if (!schemaPassed || semanticError) {
      const schemaDetail = schemaPassed ? "" : formatCanonicalSchemaErrors(runRecordSchemaValidator.errors);
      const details = [schemaDetail, semanticError].filter(Boolean).join("; ");
      const detail = details ? `: ${details}` : "";
      const error = `run record does not satisfy the current run_record schema: ${file}${detail}`;
      warnings.push(error);
      invalidRecords.push({ source_path: file, source_digest: recordDigest, error });
      continue;
    }
    const active = new Set(arrayValue(record.active_checkpoints).map(stringValue).filter(Boolean));
    const checkpoints = arrayValue(record.checkpoints).map((checkpoint, index) => {
      const inboundInterface = interfaceBindingId(checkpoint?.interface_binding);
      const outbound = arrayValue(checkpoint?.outbound_interface_bindings)
        .map(interfaceBindingId)
        .filter(Boolean);
      const artifactBindings = arrayValue(checkpoint?.artifact_bindings);
      const grant = objectOrEmpty(checkpoint?.grant);
      const contextGrant = objectOrEmpty(grant.context);
      const checkpointId = stringValue(checkpoint?.id) || `${stringValue(record.id)}:cp:${index + 1}`;
      return {
        checkpoint_id: checkpointId,
        label: `Checkpoint ${index + 1}`,
        status: active.has(stringValue(checkpoint?.id)) ? "projected" : "made",
        interface: inboundInterface,
        interface_artifact_names: interfaceBindingArtifactNames(checkpoint?.interface_binding),
        epoch: numberValue(checkpoint?.epoch, 0),
        out_interfaces: outbound,
        artifact_label: stringValue(artifactBindings[0]?.label ?? artifactBindings[0]?.id),
        artifact_bindings: artifactBindings.length,
        artifact_binding_ids: artifactBindings
          .map((binding) => stringValue(binding?.artifact_id ?? binding?.id))
          .filter(Boolean),
        interface_bindings: (inboundInterface ? 1 : 0) + outbound.length,
        grant_interfaces: capSetStrings(contextGrant.interfaces),
        awaits: objectOrEmpty(checkpoint?.awaits),
        required_verified_artifacts: arrayValue(checkpoint?.required_verified_artifacts).map((row) => ({
          mark: stringValue(row?.mark),
          artifact_label: stringValue(row?.artifact_label),
          verifier: stringValue(row?.verifier),
          params: row?.params ?? {},
        })).filter((row) => row.mark),
      };
    });
    const recordStatus = stringValue(record.status) || "recorded";
    records.push({
      run_id: stringValue(record.id),
      ee_id: stringValue(record.ee),
      worker: stringValue(record.worker),
      status: active.size === 0 ? "settled" : recordStatus,
      record_status: recordStatus,
      active_checkpoint_count: active.size,
      record_path: file,
      record_digest: recordDigest,
      interface_state: objectOrEmpty(record.interface_state),
      archived_client_interface_ids: Object.entries(objectOrEmpty(record.interface_state))
        .filter(([, state]) => state?.client_surface_role === "task" && state.client_archive !== undefined)
        .map(([interfaceId]) => interfaceId),
      checkpoints,
    });
  }
  return { records, invalidRecords };
}

function validateProjectedRunRecordSemantics(record) {
  for (const [interfaceId, state] of Object.entries(objectOrEmpty(record.interface_state))) {
    if (state?.client_surface_role !== undefined && state.client_archive !== undefined) {
      readClientTaskArchive(record, interfaceId);
    }
  }
  for (const field of ["id", "ee", "worker", "trace"]) {
    if (!stringValue(record[field])) throw new Error(`/${field} must be a non-empty string`);
  }
  for (const [index, checkpoint] of record.checkpoints.entries()) {
    for (const field of ["id", "run", "trace"]) {
      if (!stringValue(checkpoint[field])) {
        throw new Error(`/checkpoints/${index}/${field} must be a non-empty string`);
      }
    }
    if (checkpoint.interface_binding !== undefined && checkpoint.interface_binding !== null) {
      validateAuthoredInterfaceBinding(checkpoint.interface_binding, `/checkpoints/${index}/interface_binding`);
    }
    arrayValue(checkpoint.outbound_interface_bindings).forEach((binding, bindingIndex) => {
      validateAuthoredInterfaceBinding(
        binding,
        `/checkpoints/${index}/outbound_interface_bindings/${bindingIndex}`,
      );
    });
  }
}

function validateEeInstancePolicyNarrowing(value, label) {
  if (value === undefined) return;
  exactObjectKeys(
    value,
    [
      "allowed_tools",
      "allowed_interfaces",
      "allowed_worker_environment_kinds",
      "allowed_mark_outcomes",
      "allowed_root_slots",
      "allowed_artifact_verbs",
      "hard_blocked_tools",
      "interface_policy_overrides",
    ],
    [],
    label,
  );
  for (const field of [
    "allowed_tools",
    "allowed_interfaces",
    "allowed_worker_environment_kinds",
    "allowed_mark_outcomes",
    "allowed_root_slots",
    "hard_blocked_tools",
  ]) {
    if (!Object.prototype.hasOwnProperty.call(value, field)) continue;
    if (value[field] === null && field !== "hard_blocked_tools") continue;
    if (
      !Array.isArray(value[field])
      || value[field].some((item) => typeof item !== "string" || !item || item.trim() !== item)
      || new Set(value[field]).size !== value[field].length
    ) {
      throw new Error(`${label}.${field} must be a unique string array`);
    }
  }
  if (Object.prototype.hasOwnProperty.call(value, "allowed_artifact_verbs")) {
    const verbs = value.allowed_artifact_verbs;
    if (verbs !== null && (
      !Array.isArray(verbs)
      || verbs.some((verb) => !["read", "change", "create", "delete"].includes(verb))
      || new Set(verbs).size !== verbs.length
    )) {
      throw new Error(`${label}.allowed_artifact_verbs is invalid`);
    }
  }
  if (Object.prototype.hasOwnProperty.call(value, "interface_policy_overrides")) {
    if (!isPlainRecord(value.interface_policy_overrides)) {
      throw new Error(`${label}.interface_policy_overrides must be a JSON object`);
    }
    for (const [interfaceId, policy] of Object.entries(value.interface_policy_overrides)) {
      if (!interfaceId || interfaceId.trim() !== interfaceId || !isPlainRecord(policy)) {
        throw new Error(`${label}.interface_policy_overrides contains an invalid interface policy`);
      }
    }
  }
}

async function readEeInstanceRecords(eeInstancesRoot, warnings = []) {
  const files = await listDirectFiles(eeInstancesRoot, isEeInstanceJsonPath);
  const records = [];
  for (const file of Array.from(new Set(files)).sort()) {
    let record;
    let recordDigest;
    try {
      const document = await readRegularJsonDocument(file, `EE instance ${file}`);
      record = document.document;
      recordDigest = document.digest;
    } catch (error) {
      warnings.push(String(error?.message ?? error));
      continue;
    }
    if (
      !record
      || typeof record !== "object"
      || Array.isArray(record)
      || !stringValue(record.ee_id)
      || !stringValue(record.template_name)
      || !stringValue(record.goal)
      || (record.name != null && (typeof record.name !== "string" || !record.name.trim()))
      || (
        Object.prototype.hasOwnProperty.call(record, "config")
        && (!record.config || typeof record.config !== "object" || Array.isArray(record.config))
      )
    ) {
      warnings.push(`EE instance does not satisfy the current state schema: ${file}`);
      continue;
    }
    try {
      exactObjectKeys(
        record,
        ["ee_id", "name", "template_name", "parent_ee_id", "goal", "config", "policy_narrowing"],
        ["ee_id", "template_name", "parent_ee_id", "goal"],
        `EE instance ${file}`,
      );
      validateEeInstancePolicyNarrowing(record.policy_narrowing, `EE instance ${file}.policy_narrowing`);
    } catch (error) {
      warnings.push(String(error?.message ?? error));
      continue;
    }
    records.push({ ...record, record_path: file, record_digest: recordDigest });
  }
  return records;
}

function projectedFilesystemRootSurfaces({ formation, id, lifecycle, rootBindings, status }) {
  if (lifecycle !== "active") return [];
  const bindings = new Map(rootBindings.map(([name, value]) => [stringValue(name), stringValue(value)]));
  const writableSlots = new Set(arrayValue(formation.writable_root_slots).map(stringValue).filter(Boolean));
  const projectedSlots = [...new Set([
    ...arrayValue(formation.readable_root_slots).map(stringValue).filter(Boolean),
    ...writableSlots,
  ])].sort((left, right) => Number(writableSlots.has(right)) - Number(writableSlots.has(left)) || left.localeCompare(right));
  const seenRoots = new Set();
  let projectedIndex = 0;
  return projectedSlots.flatMap((slot) => [bindings.get(slot), ...arrayValue(formation.door_additional_root_bindings?.[slot])].flatMap((root, index) => {
      if (!root || !path.isAbsolute(root) || seenRoots.has(root)) return [];
      seenRoots.add(root);
      const writable = writableSlots.has(slot);
      const first = projectedIndex === 0;
      projectedIndex += 1;
      return [{
        id: `${id}:files:${slot}${index ? `:${index}` : ""}`,
        kind: "files",
        label: first ? "Files" : index ? path.basename(root) || root : rootSlotLabel(slot),
        mode: "direct",
        status,
        protocol: writable ? "filesystem.readwrite" : "filesystem.readonly",
        root,
        read_only: !writable,
        data: { slot, writable },
      }];
    }));
}

async function projectedGitSurfaces({ id, kindCapabilities, lifecycle, rootBindings, status }) {
  if (lifecycle !== "active" || !kindCapabilities.includes("git")) return [];
  const seenRoots = new Set();
  const candidates = rootBindings.flatMap(([rawSlot, rawRoot]) => {
    const slot = stringValue(rawSlot);
    const root = stringValue(rawRoot);
    if (!slot || !path.isAbsolute(root) || seenRoots.has(root)) return [];
    seenRoots.add(root);
    return [{ root, slot }];
  });
  const exactWorktrees = (await Promise.all(candidates.map(async (candidate) => (
    await isExactGitWorktreeRoot(candidate.root) ? candidate : null
  )))).filter(Boolean);
  return exactWorktrees.map(({ root, slot }, index) => ({
      id: `${id}:changes:${slot}`,
      kind: "changes",
      label: index === 0 ? "Changes" : `Changes · ${rootSlotLabel(slot)}`,
      mode: "direct",
      status,
      protocol: "git.worktree",
      root,
      read_only: false,
      data: { slot },
    }));
}

async function isExactGitWorktreeRoot(root) {
  try {
    const marker = await fs.lstat(path.join(root, ".git"));
    return !marker.isSymbolicLink() && (marker.isDirectory() || marker.isFile());
  } catch {
    return false;
  }
}

function projectedWorktreeStatusSurfaces({ id, liveState, status }) {
  const phase = stringValue(liveState.phase).toLowerCase();
  if (!["queued", "creating", "setting-up", "worktree-ready", "failed"].includes(phase)) return [];
  return [{
    id: `${id}:worktree`,
    kind: "worktree_status",
    label: "Worktree",
    mode: "evidence",
    status,
    protocol: "worktree.status.v1",
    read_only: true,
    data: {
      phase,
      label: stringValue(liveState.label ?? liveState.name) || id,
      description: stringValue(liveState.description),
      branch: stringValue(liveState.branch),
      worktree_git_root: stringValue(liveState.worktree_git_root),
      worktree_workspace_root: stringValue(liveState.worktree_workspace_root ?? liveState.workspace),
      local_environment_config_path: stringValue(liveState.local_environment_config_path),
      error_message: stringValue(liveState.error_message ?? liveState.error),
      worktree_output_text: stringValue(liveState.worktree_output_text),
      setup_output_text: stringValue(liveState.setup_output_text),
    },
  }];
}

function rootSlotLabel(slot) {
  const label = slot.replace(/[_-]+/g, " ").trim();
  return label ? `${label[0].toUpperCase()}${label.slice(1)}` : "Files";
}

function validHttpAddress(value) {
  return validAddressWithProtocols(value, ["http:", "https:"]);
}

function validWebSocketAddress(value) {
  return validAddressWithProtocols(value, ["ws:", "wss:"]);
}

function validAddressWithProtocols(value, protocols) {
  try {
    const parsed = new URL(value);
    return protocols.includes(parsed.protocol)
      && Boolean(parsed.hostname)
      && !parsed.username
      && !parsed.password;
  } catch {
    return false;
  }
}

function unavailableViewerSurface({ id, label, protocol, reason, status }) {
  return {
    id: `${id}:${label.toLowerCase().replaceAll(" ", "-")}-unavailable`,
    kind: "runtime",
    label,
    mode: "evidence",
    status,
    protocol,
    read_only: true,
    data: { detail: reason },
  };
}

function validWorkerEnvironmentLiveState(value, id, kind) {
  if (!value || typeof value !== "object" || Array.isArray(value) || value.schema_version !== 1) return false;
  if (stringValue(value.worker_environment_id) !== id) return false;
  if (!stringValue(value.status)) return false;
  const projectedKind = stringValue(value.worker_environment_kind);
  return !projectedKind || projectedKind === kind;
}

function workerEnvironmentLiveAvailable(lifecycle, status) {
  return lifecycle === "active"
    && ["active", "running", "online", "ready", "available", "connected"].includes(status);
}

function workerEnvironmentCapabilities(formation, kindCapabilities = []) {
  const output = new Set(kindCapabilities);
  for (const slot of arrayValue(formation.readable_root_slots).map(stringValue).filter(Boolean)) output.add(`read:${slot}`);
  for (const slot of arrayValue(formation.writable_root_slots).map(stringValue).filter(Boolean)) output.add(`write:${slot}`);
  const network = stringValue(formation.network_ceiling);
  if (network) output.add(`network:${network}`);
  return [...output];
}

function workerEnvironmentWorkspace(liveState, rootBindings) {
  const projected = stringValue(liveState.workspace);
  if (projected) return projected;
  for (const preferred of ["workspace_root", "worktree_root", "workspace", "worktree", "project", "root"]) {
    const match = rootBindings.find(([name]) => name === preferred);
    if (match) return stringValue(match[1]);
  }
  return stringValue(rootBindings[0]?.[1]);
}

function workerEnvironmentSession(liveState) {
  const viewer = objectOrEmpty(liveState.viewer);
  const container = objectOrEmpty(liveState.container);
  return stringValue(
    liveState.target_id
    ?? liveState.process_id
    ?? viewer.target_id
    ?? viewer.container_name
    ?? container.id
    ?? container.name,
  );
}

function normalizeDescriptorRow(registryRoot, registry, dirName, descriptorPath, descriptor) {
  const identity = descriptorIdentity(descriptor, registry.identity, dirName);
  const row = {
    ...descriptor,
    id: stringValue(descriptor.id) || identity,
    name: stringValue(descriptor.name) || stringValue(descriptor.id) || identity,
    descriptor_id: `${registry.kind}:${identity}`,
    descriptor_kind: registry.kind,
    registry: registry.kind,
    registry_dir: dirName,
    descriptor_path: descriptorPath,
    source: "registry",
    source_file: path.relative(path.dirname(registryRoot), descriptorPath),
  };
  if (registry.kind === "interface") {
    row.rule_id = stringValue(descriptor.rule_id ?? descriptor.id) || identity;
    row.template_name = row.rule_id;
    row.source = arrayValue(descriptor.source_names).map(stringValue).find(Boolean) || "registry";
    row.status = "registered";
  }
  if (registry.kind === "worker_environment_kind") {
    row.worker_environment_kind = stringValue(descriptor.kind ?? descriptor.worker_environment_kind_id) || identity;
  }
  return row;
}

function preserveProjectionEvidenceError(error) {
  if (error instanceof ProjectionEvidenceError) throw error;
}

function octopusOsInterfaceSidecars(projection) {
  return projectionMemo(projection, "structure", "octopusOsInterfaceSidecars", () => new Map(
    descriptorRows(projection, "interface").flatMap((descriptor) => {
    const interfaceId = stringValue(descriptor.id ?? descriptor.rule_id);
    const sidecar = objectOrEmpty(descriptor.octopus_os_sidecar);
    return interfaceId && sidecar.workspace === "octopus_os"
      ? [[interfaceId, sidecar]]
      : [];
    }),
  ));
}

function projectedRunInterfaceIds(record) {
  const ids = new Set();
  for (const checkpoint of arrayValue(record?.checkpoints)) {
    if (checkpoint.status !== "projected") continue;
    const inbound = stringValue(checkpoint.interface);
    if (inbound) ids.add(inbound);
    for (const outbound of arrayValue(checkpoint.out_interfaces).map(stringValue).filter(Boolean)) ids.add(outbound);
  }
  return ids;
}

function lowestCommonEnvironment(eeIds, eeById, errors) {
  if (!eeIds.length) return "";
  const lineages = eeIds.map((eeId) => environmentLineage(eeId, eeById, errors));
  if (lineages.some((lineage) => lineage.length === 0)) return "";
  const remaining = lineages.slice(1).map((lineage) => new Set(lineage));
  return lineages[0].find((candidate) => remaining.every((lineage) => lineage.has(candidate))) ?? "";
}

function environmentLineage(eeId, eeById, errors) {
  const lineage = [];
  const seen = new Set();
  let cursor = eeId;
  while (cursor) {
    if (seen.has(cursor)) {
      errors.push(`EE hierarchy cycles at '${cursor}'`);
      return [];
    }
    seen.add(cursor);
    const row = eeById.get(cursor);
    if (!row) {
      errors.push(`Octopus OS client binding references missing EE '${cursor}'`);
      return [];
    }
    lineage.push(cursor);
    cursor = stringValue(row.parent_ee_id);
  }
  return lineage;
}

function descendantEnvironmentIds(rootEeId, eeById) {
  const children = new Map();
  for (const [eeId, row] of eeById) {
    const parent = stringValue(row.parent_ee_id);
    if (!parent) continue;
    const entries = children.get(parent) ?? [];
    entries.push(eeId);
    children.set(parent, entries);
  }
  const descendants = new Set();
  const visit = (eeId) => {
    if (descendants.has(eeId)) return;
    descendants.add(eeId);
    for (const child of children.get(eeId) ?? []) visit(child);
  };
  visit(rootEeId);
  return descendants;
}

function operatorOsProjectRows(scope, environments) {
  const presentations = new Map(arrayValue(scope.project_presentations).map((presentation) => [
    stringValue(presentation.ee_id),
    presentation,
  ]).filter(([eeId]) => eeId));
  return environments.map((environment) => {
    const presentation = objectOrEmpty(presentations.get(stringValue(environment.ee_id)));
    return {
      ee_id: stringValue(environment.ee_id),
      name: stringValue(environment.name) || stringValue(environment.ee_id),
      icon: stringValue(presentation.icon),
      color: stringValue(presentation.color),
      goal: stringValue(environment.goal),
      parent_ee_id: stringValue(environment.parent_ee_id),
      record_digest: stringValue(environment.record_digest),
    };
  });
}

function applicationPrivateConfigurationProjection(manifest, interfaceIds, descriptorsByInterface, installed) {
  const authoredFields = arrayValue(manifest?.setup?.fields);
  if (!installed) {
    return { status: "not_installed", fields: [], documents: [], error: "" };
  }
  const fields = [];
  const documents = [];
  const errors = [];
  for (const interfaceId of interfaceIds) {
    const configuration = objectOrEmpty(descriptorsByInterface.get(interfaceId)?.interface_private_configuration);
    const status = stringValue(configuration.status);
    if (!configuration.interface_id || status === "invalid") {
      errors.push(stringValue(configuration.error) || `Configuration for '${interfaceId}' could not be verified.`);
      continue;
    }
    try {
      const declarations = arrayValue(configuration.fields);
      const presented = authoredFields.filter((field) => stringValue(field?.interface) === interfaceId);
      const resolved = presented.length
        ? resolveAuthoredApplicationSetupFields(
            authoredFields,
            interfaceId,
            declarations,
            `application '${stringValue(manifest?.id)}' setup`,
          )
        : declarations.map((declaration) => ({
            presentation: {
              label: stringValue(declaration?.name),
              kind: "secret",
              required: declaration?.required === true,
            },
            declaration,
          }));
      if (resolved.length === 0) continue;
      documents.push({
        interface_id: interfaceId,
        status: stringValue(configuration.document_status) || "unavailable",
        digest: stringValue(configuration.document_digest),
      });
      for (const { presentation, declaration: projected } of resolved) {
        const authoredIndex = authoredFields.indexOf(presentation);
        fields.push({
          order: authoredIndex >= 0 ? authoredIndex : authoredFields.length,
          interface_id: interfaceId,
          name: stringValue(projected.name),
          label: stringValue(presentation.label),
          kind: stringValue(presentation.kind),
          required: presentation.required === true,
          authored: presented.length > 0,
          placeholder: stringValue(presentation.placeholder),
          help: stringValue(presentation.help),
          configured: projected.configured === true,
          private_saved: projected.private_saved === true,
          source: stringValue(projected.source) || "unknown",
        });
      }
    } catch (error) {
      errors.push(error.message);
    }
  }
  fields.sort((left, right) => left.order - right.order
    || left.interface_id.localeCompare(right.interface_id)
    || left.name.localeCompare(right.name));
  for (const field of fields) delete field.order;
  const requiredFields = fields.filter((field) => field.required);
  const conflict = fields.some((field) => field.source === "conflict");
  const provenMissing = requiredFields.some((field) => field.source === "missing");
  const unknown = requiredFields.some((field) => field.source === "unknown");
  if (conflict) errors.push("Saved configuration conflicts with the managed runtime environment. Clear the saved value or update the external configuration.");
  return {
    status: errors.length || conflict
      ? "issue"
      : provenMissing ? "required" : unknown ? "unknown" : "configured",
    fields,
    documents,
    error: errors.join("\n"),
  };
}

function isOperatorChatProviderFailureRow(entry, row) {
  const provider = operatorChatRuntimeProviderId(entry);
  return Boolean(provider)
    && stringValue(row?.kind) === "InterfaceOutbound"
    && movementBoundaryInterface(row, "from") === provider
    && movementBoundaryInterface(row, "to") === entry.chatInterfaceId
    && ledgerArtifactLabel(row) === `${provider}.failure`;
}

function isOperatorChatProviderMovement(entry, row) {
  const provider = operatorChatRuntimeProviderId(entry);
  return Boolean(provider)
    && movementRunId(row) === entry.runId
    && movementBoundaryInterface(row, "from") === provider
    && (row.kind === "InterfaceArrival"
      || row.kind === "InterfaceOutbound" && movementBoundaryInterface(row, "to") === entry.chatInterfaceId);
}

function projectionSnapshot(projection) {
  return projectionMemo(projection, "ledger", "projectionSnapshot", () => {
    const counts = {};
    for (const registry of DESCRIPTOR_REGISTRIES) {
      counts[registry.kind] = descriptorRows(projection, registry.kind).length;
    }
    const latestMovement = projection.ledgerRows.reduce((latest, row) => {
      const at = numberValue(row.recorded_at_unix_ms ?? row.wall_clock_unix_ms, 0);
      if (!latest || at > latest.recorded_at_unix_ms || (at === latest.recorded_at_unix_ms && numberValue(row.seq, 0) > latest.seq)) {
        return { seq: numberValue(row.seq, 0), recorded_at_unix_ms: at };
      }
      return latest;
    }, null);
    return {
      projection: {
        generated_at: projection.generatedAt,
        generation: projection.generation,
        repo_root: projection.repoRoot,
        octopus_home: projection.octopusHome,
        registry_root: projection.registryRoot,
        state_root: projection.stateRoot,
        runs_root: projection.runRecordsRoot,
        durable_root: projection.durableRoot,
        ledger_path: path.join(projection.durableRoot, "movements.jsonl"),
        descriptor_counts: counts,
        ledger_loaded: Boolean(projection.ledgerRevision),
        ledger_rows: projection.ledgerRows.length,
        latest_movement_seq: latestMovement?.seq ?? null,
        latest_movement_recorded_at_unix_ms: latestMovement?.recorded_at_unix_ms ?? null,
        invalid_run_records: projection.invalidRunRecords,
        invalid_ledger_rows: projection.invalidLedgerRows,
        warnings: arrayValue(projection.warnings),
      },
    };
  });
}

function baseInterfaceRows(projection) {
  return descriptorRows(projection, "interface");
}

function interfaceActivationEntries(sidecars) {
  const rows = [];
  for (const sidecar of arrayValue(sidecars)) {
    for (const activation of arrayValue(sidecar?.activations)) {
      const ref = stringValue(
        activation?.activation_transport_ref
        ?? activation?.activation_driver_ref
        ?? activation?.transport_ref,
      );
      rows.push({
        ref,
        run: stringValue(activation?.run),
        descriptor_path: stringValue(sidecar?.descriptor_path),
        config: objectOrEmpty(activation?.config),
      });
    }
  }
  return rows.filter((row) => row.ref);
}

function eeTemplateRows(projection) {
  return descriptorRows(projection, "ee_template").map((row) => ({
    ...row,
    name: stringValue(row.id ?? row.name),
    source: stringValue(row.source) || "registry",
    goal: stringValue(row.goal ?? row.goal_class ?? row.id),
  }));
}

function eeTemplateDescriptorRows(projection) {
  return eeTemplateRows(projection).map((row) => ({
    ...row,
    descriptor_id: stringValue(row.descriptor_id) || `ee_template:${row.name}`,
  }));
}

function eeInstanceRows(projection, includeLedger = true) {
  return projectionMemo(
    projection,
    includeLedger ? "ledger" : "structure",
    includeLedger ? "eeInstanceRows" : "eeInstanceRows.structure",
    () => {
  const byEe = new Map();
  const templates = new Map(descriptorRows(projection, "ee_template").map((candidate) => [
    stringValue(candidate.id ?? candidate.name),
    candidate,
  ]));
  for (const record of arrayValue(projection.eeInstances)) {
    const eeId = stringValue(record.ee_id);
    if (!eeId) continue;
    const templateName = stringValue(record.template_name);
    const template = templates.get(templateName);
    byEe.set(eeId, {
      ee_id: eeId,
      name: stringValue(record.name),
      template_name: templateName,
      status: "projected",
      goal_id: "",
      goal: stringValue(record.goal),
      parent_ee_id: stringValue(record.parent_ee_id),
      created_at: "",
      projection_kind: "state.ee_instance",
      runtime_state: "recorded",
      run_count: 0,
      source: "ee_instance",
      record_path: stringValue(record.record_path),
      record_digest: stringValue(record.record_digest),
      config: objectOrEmpty(record.config),
      policy_narrowing: objectOrEmpty(record.policy_narrowing),
      ee_template_descriptor: template ?? {},
    });
  }
  for (const run of runRows(projection, includeLedger)) {
    const eeId = stringValue(run.ee_id);
    if (!eeId) continue;
    const current = byEe.get(eeId) ?? {
      ee_id: eeId,
      template_name: "",
      status: "projected",
      goal_id: "",
      goal: "",
      parent_ee_id: "",
      created_at: stringValue(run.created_at),
      projection_kind: "run.ee_reference",
      runtime_state: "projected_from_run_evidence",
      run_count: 0,
      source: "run_evidence",
    };
    current.run_count += 1;
    if (current.source !== "ee_instance" && run.source === "run_record") current.status = "projected";
    if (!current.created_at) current.created_at = stringValue(run.created_at);
    byEe.set(eeId, current);
  }
  return Array.from(byEe.values());
    },
  );
}

function goalRows() {
  return [];
}

function workerRows(projection) {
  return descriptorRows(projection, "interface")
    .filter((row) => (
      stringValue(row.interface_type).toLowerCase() === "system"
      && stringValue(row.role).toLowerCase() === "worker"
    ))
    .map((row) => {
      const policy = objectOrEmpty(row.worker_policy);
      const ceiling = objectOrEmpty(policy.ceiling);
      const policyStatus = stringValue(row.worker_policy_status);
      return {
        worker: stringValue(row.rule_id ?? row.id),
        status: policyStatus === "registered" ? "registered" : `${policyStatus || "missing"}_policy`,
        ceiling_interfaces_count: arrayValue(ceiling.interfaces).length,
        ceiling_tools_count: arrayValue(ceiling.tools).length,
        ceiling_worker_environment_kinds_count: arrayValue(ceiling.wek).length,
        ceiling_mark_outcomes_count: arrayValue(ceiling.mark_outcomes).length,
        role_artifacts_count: arrayValue(policy.role_artifacts).length,
        policy_path: stringValue(row.worker_policy_path),
        interface_descriptor_path: stringValue(row.descriptor_path),
        source: "registry.interface",
        interface: row,
        policy: row.worker_policy ?? null,
      };
    });
}

function workerEnvironmentRows(projection) {
  const rows = arrayValue(projection.workerEnvironments);
  if (!arrayValue(projection.ledgerRows).length) return rows;
  return projectionMemo(projection, "ledger", "workerEnvironmentRows", () => {
    const wanted = new Set(rows.flatMap((row) => [row.opened_at_movement_seq, row.last_activity_movement_seq])
      .filter(Number.isSafeInteger));
    const recordedAt = new Map();
    for (const movement of projection.ledgerRows) {
      const seq = movement?.seq;
      if (wanted.has(seq)) recordedAt.set(seq, numberValue(movement.recorded_at_unix_ms ?? movement.wall_clock_unix_ms, 0) || null);
    }
    return rows.map((row) => ({
      ...row,
      opened_at_unix_ms: recordedAt.get(row.opened_at_movement_seq) ?? null,
      last_activity_at_unix_ms: recordedAt.get(row.last_activity_movement_seq) ?? null,
    }));
  });
}

function bindingRows(projection) {
  return projectionMemo(projection, "structure", "bindingRows", () => {
    const rows = [];
    for (const record of arrayValue(projection.runRecords)) {
      for (const checkpoint of arrayValue(record.checkpoints)) {
        const common = {
          run_id: stringValue(record.run_id),
          ee_id: stringValue(record.ee_id),
          worker: stringValue(record.worker),
          checkpoint_id: stringValue(checkpoint.checkpoint_id),
          epoch: numberValue(checkpoint.epoch, 0),
          status: stringValue(checkpoint.status) || "projected",
          projection_kind: "run.checkpoint.interface_binding",
        };
        const inbound = stringValue(checkpoint.interface);
        if (inbound) {
          rows.push({
            ...common,
            interface_id: inbound,
            direction: "inbound",
          });
        }
        for (const outbound of arrayValue(checkpoint.out_interfaces).map(stringValue).filter(Boolean)) {
          rows.push({
            ...common,
            interface_id: outbound,
            direction: "outbound",
          });
        }
      }
    }
    return rows;
  });
}

function markRows(projection) {
  return projectionMemo(projection, "ledger", "markRows", () => {
    const runs = new Map(arrayValue(projection.runRecords).map((record) => [record.run_id, record]));
    const originals = new Map(descriptorRows(projection, "interface").map((descriptor) => [stringValue(descriptor.id), stringValue(descriptor.copy_of)]));
    return projection.ledgerRows.flatMap((row) => {
    const match = /^MarkOutcomeAdmitted\((.+)\)$/.exec(stringValue(row.kind));
    if (!match) return [];
    const metadata = objectOrEmpty(row.invocation_metadata);
    const runId = movementRunId(row);
    const run = runs.get(runId);
    return [{
      movement_id: `${path.basename(stringValue(row.source_file) || "ledger", ".jsonl")}:${numberValue(row.source_line, 0)}`,
      mark_name: match[1],
      status: "admitted",
      run_id: runId,
      ee_id: stringValue(run?.ee_id),
      worker: stringValue(run?.worker),
      worker_original: originals.get(stringValue(run?.worker)) || stringValue(run?.worker),
      checkpoint_id: "",
      interface_id: "",
      artifact_id: ledgerArtifactId(row),
      created_at: timestampOf(row),
      seq: numberValue(row.seq, null),
      source: "ledger",
      raw: row,
    }];
    });
  });
}

function envelopeRows(projection) {
  return projectionMemo(projection, "ledger", "envelopeRows", () => projection.ledgerRows.map((row) => ({
    ...row,
    ts: timestampOf(row),
  })));
}

function toolRows(projection) {
  return descriptorRows(projection, "tool").map((row) => ({
    ...row,
    name: stringValue(row.id ?? row.name),
    effect_categories: arrayValue(row.effect_categories).length
      ? row.effect_categories
      : (row.is_world_effect ? ["world_effect"] : []),
    is_destructive: Boolean(row.is_destructive),
  }));
}

function verifierRows(projection) {
  return descriptorRows(projection, "verifier").map((row) => ({
    ...row,
    name: stringValue(row.id ?? row.name),
    source: stringValue(row.source) || "registry",
  }));
}

function policyRows(projection) {
  return eeTemplateRows(projection).map((row) => ({
    name: row.name,
    allowed_interfaces_count: arrayValue(row.allowed_interfaces ?? row.allowed_interface_templates).length,
    allowed_tools_count: arrayValue(row.allowed_tools).length,
    allowed_worker_environment_kinds_count: arrayValue(row.allowed_worker_environment_kinds).length,
    source: "registry.ee_template",
    raw_policy: row,
  }));
}

function descriptorInventoryRows(projection) {
  return DESCRIPTOR_REGISTRIES.map((registry) => ({
    registry: registry.kind,
    count: descriptorRows(projection, registry.kind).length,
    runtime_count: 0,
    warning_count: 0,
    descriptor_filename: registry.file,
  }));
}

function descriptorKindRows() {
  return DESCRIPTOR_REGISTRIES.map((registry) => ({
    kind: registry.kind,
    identity_field: registry.identity.join(" | "),
    descriptor_filename: registry.file,
    source: "projection.registry_index",
  }));
}

function markKindRows(projection) {
  return descriptorRows(projection, "mark")
    .map((mark) => {
      const name = stringValue(mark.name ?? mark.id ?? mark.kind);
      if (!name) return null;
      return {
      ...mark,
      name,
      lifecycle: stringValue(mark.lifecycle) || "registered",
      source: "registry.mark",
      };
    })
    .filter(Boolean)
    .sort((a, b) => a.name.localeCompare(b.name));
}

function runRows(projection, includeLedger = true) {
  if (!includeLedger) {
    return projectionMemo(projection, "structure", "runRows.structure", () => (
      arrayValue(projection.runRecords).map((record) => ({
      run_id: record.run_id,
      status: record.status,
      record_status: record.record_status,
      active_checkpoint_count: record.active_checkpoint_count,
      worker: record.worker,
      ee_id: record.ee_id,
      created_at: "",
      meta: { record_path: record.record_path },
      source: "run_record",
      }))
    ));
  }
  return projectionMemo(projection, "ledger", "runRows", () => {
  const byRun = new Map(runRows(projection, false).map((row) => [row.run_id, row]));
  for (const row of envelopeRows(projection)) {
    const runtime = objectOrEmpty(objectOrEmpty(row.invocation_metadata).runtime);
    const runId = stringValue(runtime.run);
    const eeId = stringValue(runtime.ee);
    const worker = stringValue(runtime.worker);
    if (!runId || !eeId || !worker) continue;
    if (byRun.get(runId)?.source === "run_record") continue;
    const current = byRun.get(runId) || {
      run_id: runId,
      status: "recorded",
      worker,
      ee_id: eeId,
      created_at: timestampOf(row),
      meta: {},
      source: "ledger",
    };
    current.worker = worker;
    current.ee_id = eeId;
    byRun.set(runId, current);
  }
  return Array.from(byRun.values());
  });
}

function filterInterfaces(rows, params) {
  return filterByString(rows, "interface_type", params.interface_type);
}

function withoutRawProjectionEvidence(row) {
  const { raw: _raw, ...projected } = row;
  return projected;
}

function descriptorRows(projection, kind) {
  return arrayValue(projection.descriptors[kind]);
}
