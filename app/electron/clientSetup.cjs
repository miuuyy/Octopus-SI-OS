const crypto = require("node:crypto");
const fs = require("node:fs");
const fsPromises = require("node:fs/promises");
const path = require("node:path");
const Ajv2020 = require("ajv/dist/2020");

const operationSchema = require("../schemas/client_setup_operation.schema.json");
const receiptSchema = require("../schemas/client_setup_receipt.schema.json");
const currentSchema = require("../schemas/client_setup_current.schema.json");

const OPERATION_ID = /^[A-Za-z0-9_-]{8,96}$/;
const DIGEST = /^[a-f0-9]{64}$/;
const MAX_DOCUMENT_BYTES = 2 * 1024 * 1024;
const STEPS = ["packages", "extensions", "provider", "workspace"];
const PROOF_KIND = {
  packages: "package_install_evidence",
  extensions: "extension_lifecycle_receipt",
  provider: "provider_observation",
  workspace: "workspace_receipt",
};
const transactionTails = new Map();
const executionTails = new Map();
const activeResumes = new Map();

const validators = new Ajv2020({ allErrors: true, strict: true });
validators.addSchema(operationSchema);
const validateOperation = validators.getSchema(operationSchema.$id);
const validateReceipt = validators.compile(receiptSchema);
const validateCurrent = validators.compile(currentSchema);

function createClientSetupStore(options) {
  const root = path.resolve(requiredAbsolutePath(options?.root, "client setup state root"));
  const operationsRoot = path.join(root, "operations");
  const receiptsRoot = path.join(root, "receipts");
  const currentPath = path.join(root, "current.json");
  const owners = normalizeOwners(options?.owners);

  async function ensureRoot() {
    await fsPromises.mkdir(operationsRoot, { recursive: true, mode: 0o700 });
    await fsPromises.mkdir(receiptsRoot, { recursive: true, mode: 0o700 });
    for (const candidate of [root, operationsRoot, receiptsRoot]) {
      const stat = await fsPromises.lstat(candidate);
      if (!stat.isDirectory() || stat.isSymbolicLink()) {
        throw new Error(`client setup path must be a real directory: ${candidate}`);
      }
    }
    for (const directory of [root, operationsRoot, receiptsRoot]) await recoverInterruptedWrites(directory);
  }

  async function begin(request) {
    return withClientSetupLock(root, () => withClientSetupLock(root, async () => {
      await ensureRoot();
      const operationId = requiredOperationId(request?.operationId);
      const plan = normalizePlan(request?.plan, operationId);
      const planDigest = sha256(stableJson(plan));
      const current = await readCurrentUnlocked();
      if (current?.status === "in_progress" && current.operationId !== operationId) {
        throw new Error(`Client setup operation '${current.operationId}' is still in progress; resume it before beginning another plan.`);
      }
      const receipt = await readReceipt(operationId);
      if (receipt) {
        if (receipt.plan_digest !== planDigest) throw operationCollision(operationId);
        await selectCurrent(receipt);
        return projectReceipt(receipt);
      }
      const existing = await readOperation(operationId);
      if (existing) {
        if (existing.plan_digest !== planDigest) throw operationCollision(operationId);
        await selectCurrent(existing);
        return projectOperation(existing);
      }
      const now = timestamp();
      const operation = parseOperation({
        schema: "octopus.client_setup_operation",
        schema_version: 1,
        operation_id: operationId,
        plan_digest: planDigest,
        plan,
        steps: STEPS.map((step) => ({ step, state: "pending" })),
        updated_at: now,
      });
      await writeJsonAtomic(operationPath(operationId), operation);
      await selectCurrent(operation);
      return projectOperation(operation);
    }), executionTails);
  }

  async function read(operationIdValue) {
    return withClientSetupLock(root, async () => {
      await ensureRoot();
      const operationId = requiredOperationId(operationIdValue?.operationId ?? operationIdValue);
      const receipt = await readReceipt(operationId);
      if (receipt) return projectReceipt(receipt);
      const operation = await readOperation(operationId);
      return operation ? projectOperation(operation) : null;
    });
  }

  async function readCurrent() {
    return withClientSetupLock(root, async () => {
      await ensureRoot();
      return readCurrentUnlocked();
    });
  }

  async function cancel(operationIdValue) {
    const operationId = requiredOperationId(operationIdValue?.operationId ?? operationIdValue);
    return withClientSetupLock(root, () => withClientSetupLock(root, async () => {
      await ensureRoot();
      const receipt = await readReceipt(operationId);
      if (receipt) {
        throw new Error(`Client setup operation '${operationId}' is already complete and cannot be cancelled.`);
      }
      const operation = await readOperation(operationId);
      if (!operation) {
        const selected = await readCurrentSelection();
        if (selected?.operation_id === operationId) {
          await fsPromises.rm(currentPath, { force: true });
        }
        return { cancelled: false, operationId };
      }
      if (operation.steps.some((entry) => entry.step === "workspace" && entry.state === "complete")) {
        throw new Error(`Client setup operation '${operationId}' already created Home and projects and cannot be cancelled.`);
      }
      await fsPromises.rm(operationPath(operationId), { force: true });
      const selected = await readCurrentSelection();
      if (selected?.operation_id === operationId) {
        await fsPromises.rm(currentPath, { force: true });
      }
      return { cancelled: true, operationId };
    }), executionTails);
  }

  async function resume(operationIdValue, resumeOptions = {}) {
    const stopBefore = typeof operationIdValue === "object" ? operationIdValue?.stopBefore : undefined;
    if (stopBefore !== undefined && stopBefore !== "provider") throw new Error("Unsupported client setup stop point.");
    const operationId = requiredOperationId(operationIdValue?.operationId ?? operationIdValue);
    return joinClientSetupResume(root, operationId, stopBefore, resumeOptions.onProgress, (onProgress) =>
      withClientSetupLock(root, () => resumeOperation(operationId, stopBefore, onProgress), executionTails));
  }

  async function resumeOperation(operationId, stopBefore, onProgress) {
    let operation = await withClientSetupLock(root, async () => {
      await ensureRoot();
      const completed = await readReceipt(operationId);
      if (completed) return projectReceipt(completed);
      const current = await readOperation(operationId);
      if (!current) throw new Error(`Client setup operation '${operationId}' does not exist.`);
      return current;
    });
    if (operation.status === "complete") return operation;

    for (const [index, step] of STEPS.entries()) {
      if (step === stopBefore) {
        return projectOperation(operation);
      }
      reportProgress(onProgress, operationId, step, "verifying", Math.round(index * 90 / STEPS.length));
      let proof;
      const previousProof = operation.steps.find((entry) => entry.step === step)?.proof ?? null;
      try {
        proof = await probeStep(owners[step], step, operation.plan, operationId, { previousProof });
      } catch (error) {
        operation = await recordFailure(operation, step, error);
        throw error;
      }
      if (!proof) {
        reportProgress(onProgress, operationId, step, "applying", Math.round((index + 0.35) * 90 / STEPS.length));
        let candidateEvidence;
        try {
          candidateEvidence = await owners[step].apply(operation.plan, {
            operationId,
            step,
            onProgress: (detail) => reportProgress(onProgress, operationId, step, "applying", Math.round((index + 0.35) * 90 / STEPS.length), detail),
          });
        } catch (error) {
          operation = await recordFailure(operation, step, error);
          throw error;
        }
        try {
          reportProgress(onProgress, operationId, step, "verifying", Math.round((index + 0.7) * 90 / STEPS.length));
          proof = await probeStep(owners[step], step, operation.plan, operationId, { candidateEvidence });
        } catch (error) {
          operation = await recordFailure(operation, step, error);
          throw error;
        }
        if (!proof) {
          const error = new Error(`Client setup step '${step}' completed without verifiable owner evidence.`);
          operation = await recordFailure(operation, step, error);
          throw error;
        }
      }
      operation = await checkpoint(operation, step, proof);
      reportProgress(onProgress, operationId, step, "complete", Math.round((index + 1) * 90 / STEPS.length));
    }

    const finalProofs = [];
    for (const [index, step] of STEPS.entries()) {
      reportProgress(onProgress, operationId, step, "verifying", 90 + index * 2);
      let proof;
      const previousProof = operation.steps.find((entry) => entry.step === step)?.proof ?? null;
      try {
        proof = await probeStep(owners[step], step, operation.plan, operationId, { previousProof });
      } catch (error) {
        operation = await recordFailure(operation, step, error, true);
        throw error;
      }
      if (!proof) {
        const error = new Error(`Client setup cannot seal because '${step}' evidence is no longer valid.`);
        operation = await recordFailure(operation, step, error, true);
        throw error;
      }
      finalProofs.push(proof);
    }
    const receipt = parseReceipt({
      schema: "octopus.client_setup_receipt",
      schema_version: 1,
      operation_id: operation.operation_id,
      plan_digest: operation.plan_digest,
      plan: operation.plan,
      proof_semantics: "sequential_owner_observations",
      proofs: finalProofs,
      completed_at: timestamp(),
    });
    await withClientSetupLock(root, async () => {
      await writeReceipt(receipt);
      await selectCurrent(receipt);
    });
    reportProgress(onProgress, operationId, "workspace", "sealed", 100);
    return projectReceipt(receipt);
  }

  async function checkpoint(operation, step, proof) {
    const steps = operation.steps.map((candidate) => (
      candidate.step === step ? { step, state: "complete", proof } : candidate
    ));
    const next = parseOperation({
      ...operation,
      steps,
      updated_at: timestamp(),
      last_error: undefined,
    });
    delete next.last_error;
    await withClientSetupLock(root, () => writeJsonAtomic(operationPath(operation.operation_id), next));
    return next;
  }

  async function recordFailure(operation, step, error, invalidateStep = false) {
    const steps = invalidateStep
      ? operation.steps.map((candidate) => candidate.step === step ? { step, state: "pending" } : candidate)
      : operation.steps;
    const next = parseOperation({
      ...operation,
      steps,
      updated_at: timestamp(),
      last_error: {
        step,
        message: errorMessage(error).slice(0, 8000) || "Unknown client setup failure",
        observed_at: timestamp(),
      },
    });
    await withClientSetupLock(root, () => writeJsonAtomic(operationPath(operation.operation_id), next));
    return next;
  }

  async function probeStep(owner, step, plan, operationId, evidence = {}) {
    const references = await owner.probe(plan, {
      operationId,
      step,
      previousProof: evidence.previousProof ?? null,
      candidateEvidence: evidence.candidateEvidence ?? null,
    });
    if (references === null || references === undefined) return null;
    if (
      !Array.isArray(references)
      || references.length === 0 && (step !== "extensions" || plan.extensions.length > 0)
    ) {
      throw new Error(`Client setup owner '${step}' returned no proof references.`);
    }
    const expectedKind = PROOF_KIND[step];
    const normalized = references.map((reference, index) => normalizeProofReference(reference, expectedKind, step, index));
    normalized.sort((left, right) => left.kind.localeCompare(right.kind) || left.id.localeCompare(right.id) || left.digest.localeCompare(right.digest));
    if (new Set(normalized.map((entry) => `${entry.kind}\u0000${entry.id}`)).size !== normalized.length) {
      throw new Error(`Client setup owner '${step}' returned duplicate proof references.`);
    }
    return {
      step,
      observed_at: timestamp(),
      evidence_digest: sha256(stableJson(normalized)),
      references: normalized,
    };
  }

  async function readOperation(operationId) {
    const value = await readJsonIfPresent(operationPath(operationId), `client setup operation '${operationId}'`);
    if (value === null) return null;
    const operation = parseOperation(value);
    if (operation.operation_id !== operationId) throw new Error(`Client setup operation '${operationId}' has drifted identity.`);
    return operation;
  }

  async function readReceipt(operationId) {
    const value = await readJsonIfPresent(receiptPath(operationId), `client setup receipt '${operationId}'`);
    if (value === null) return null;
    const receipt = parseReceipt(value);
    if (receipt.operation_id !== operationId) throw new Error(`Client setup receipt '${operationId}' has drifted identity.`);
    return receipt;
  }

  async function writeReceipt(receiptValue) {
    const receipt = parseReceipt(receiptValue);
    const existing = await readReceipt(receipt.operation_id);
    if (existing) {
      if (stableJson(existing) !== stableJson(receipt)) {
        throw new Error(`Client setup receipt '${receipt.operation_id}' already differs.`);
      }
      return;
    }
    await writeJsonCreate(receiptPath(receipt.operation_id), receipt);
  }

  async function readCurrentUnlocked() {
    const selected = await readCurrentSelection();
    const unfinished = await readUnfinishedOperations();
    if (unfinished.length > 1) {
      throw new Error(`Client setup has ${unfinished.length} unfinished operations; current setup identity is ambiguous.`);
    }
    if (unfinished.length === 1) {
      const operation = unfinished[0];
      if (
        !selected
        || selected.operation_id !== operation.operation_id
        || selected.plan_digest !== operation.plan_digest
      ) await selectCurrent(operation);
      return projectOperation(operation);
    }
    if (selected) {
      const receipt = await readReceipt(selected.operation_id);
      if (receipt) {
        if (receipt.plan_digest !== selected.plan_digest) throw new Error("Current client setup selection has drifted from its receipt.");
        return projectReceipt(receipt);
      }
      const operation = await readOperation(selected.operation_id);
      if (operation) {
        throw new Error(`Current client setup operation '${selected.operation_id}' has no completion receipt.`);
      }
      throw new Error(`Current client setup selection '${selected.operation_id}' has no durable operation or receipt.`);
    }
    const receiptIds = await listDocumentIds(receiptsRoot, "client setup receipt");
    if (receiptIds.length === 0) return null;
    if (receiptIds.length > 1) {
      throw new Error("Client setup has multiple receipts but no persisted current selection.");
    }
    const receipt = await readReceipt(receiptIds[0]);
    await selectCurrent(receipt);
    return projectReceipt(receipt);
  }

  async function readUnfinishedOperations() {
    const operations = [];
    for (const operationId of await listDocumentIds(operationsRoot, "client setup operation")) {
      if (await readReceipt(operationId)) continue;
      operations.push(await readOperation(operationId));
    }
    return operations;
  }

  async function readCurrentSelection() {
    const value = await readJsonIfPresent(currentPath, "current client setup selection");
    if (value === null) return null;
    if (!validateCurrent(value)) throw schemaError("Current client setup selection", validateCurrent.errors);
    validateTimestamp(value.selected_at, "Current client setup selection time");
    return value;
  }

  async function selectCurrent(record) {
    const current = await readCurrentSelection();
    if (current?.operation_id === record.operation_id && current.plan_digest === record.plan_digest) return;
    await writeJsonAtomic(currentPath, {
      schema: "octopus.client_setup_current",
      schema_version: 1,
      operation_id: record.operation_id,
      plan_digest: record.plan_digest,
      selected_at: timestamp(),
    });
  }

  function operationPath(operationId) {
    return path.join(operationsRoot, `${requiredOperationId(operationId)}.json`);
  }

  function receiptPath(operationId) {
    return path.join(receiptsRoot, `${requiredOperationId(operationId)}.json`);
  }

  return { root, begin, cancel, read, readCurrent, resume, ensureRoot };
}

async function listDocumentIds(directory, label) {
  const entries = await fsPromises.readdir(directory, { withFileTypes: true });
  const ids = [];
  for (const entry of entries) {
    if (entry.name.startsWith(".")) continue;
    const match = /^([A-Za-z0-9_-]{8,96})\.json$/.exec(entry.name);
    const file = path.join(directory, entry.name);
    const stat = await fsPromises.lstat(file);
    if (!match || !entry.isFile() || !stat.isFile() || stat.isSymbolicLink()) {
      throw new Error(`${label} path '${file}' is not a canonical regular document.`);
    }
    ids.push(requiredOperationId(match[1]));
  }
  return ids.sort();
}

async function recoverInterruptedWrites(directory) {
  const entries = await fsPromises.readdir(directory, { withFileTypes: true });
  let changed = false;
  for (const entry of entries) {
    if (!/^\.[A-Za-z0-9._-]+\.json-[0-9]+-[0-9a-f-]{36}\.tmp$/.test(entry.name)) continue;
    const file = path.join(directory, entry.name);
    const stat = await fsPromises.lstat(file);
    if (!entry.isFile() || !stat.isFile() || stat.isSymbolicLink()) {
      throw new Error(`client setup interrupted write '${file}' is not a regular file`);
    }
    await fsPromises.unlink(file);
    changed = true;
  }
  if (changed) await syncDirectory(directory);
}

function normalizeOwners(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error("Client setup owners must be an object.");
  }
  const owners = {};
  for (const step of STEPS) {
    const owner = value[step];
    if (!owner || typeof owner.probe !== "function" || typeof owner.apply !== "function") {
      throw new Error(`Client setup owner '${step}' must provide probe and apply.`);
    }
    owners[step] = owner;
  }
  return owners;
}

function normalizePlan(value, operationId) {
  let plan;
  try {
    plan = JSON.parse(JSON.stringify(value));
  } catch (error) {
    throw new Error(`Client setup plan is not serializable: ${errorMessage(error)}`);
  }
  if (!plan || typeof plan !== "object" || Array.isArray(plan)) throw new Error("Client setup plan must be an object.");
  plan.pack.selected_recommended.sort();
  plan.packages.sort((left, right) => String(left.id).localeCompare(String(right.id)));
  plan.extensions.sort((left, right) => String(left.id).localeCompare(String(right.id)));
  for (const projectDefault of plan.workspace.project_defaults) projectDefault.project_keys.sort();
  plan.workspace.project_defaults.sort((left, right) => String(left.package_id).localeCompare(String(right.package_id)));
  const candidate = {
    schema: "octopus.client_setup_operation",
    schema_version: 1,
    operation_id: operationId,
    plan_digest: "0".repeat(64),
    plan,
    steps: STEPS.map((step) => ({ step, state: "pending" })),
    updated_at: new Date(0).toISOString(),
  };
  if (!validateOperation(candidate)) throw schemaError("Client setup plan", validateOperation.errors);
  if (new Set(plan.packages.map((entry) => entry.id)).size !== plan.packages.length) {
    throw new Error("Client setup plan repeats a local package.");
  }
  if (new Set(plan.extensions.map((entry) => entry.id)).size !== plan.extensions.length) {
    throw new Error("Client setup plan repeats a client extension.");
  }
  if (new Set(plan.workspace.project_defaults.map((entry) => entry.package_id)).size !== plan.workspace.project_defaults.length) {
    throw new Error("Client setup plan repeats a Project default package.");
  }
  if (plan.workspace.request.operationId !== operationId || plan.workspace.request.action !== "initialize_workspace") {
    throw new Error("Client setup workspace request does not belong to the setup operation.");
  }
  return plan;
}

function parseOperation(value) {
  const clean = value && typeof value === "object" && !Array.isArray(value)
    ? Object.fromEntries(Object.entries(value).filter(([, entry]) => entry !== undefined))
    : value;
  if (!validateOperation(clean)) throw schemaError("Client setup operation", validateOperation.errors);
  validateTimestamp(clean.updated_at, "Client setup operation update time");
  if (clean.last_error) validateTimestamp(clean.last_error.observed_at, "Client setup operation error time");
  if (clean.plan_digest !== sha256(stableJson(clean.plan))) throw new Error("Client setup operation plan digest is invalid.");
  validatePlanInvariants(clean.plan, clean.operation_id);
  validateStepSet(clean.steps);
  return clean;
}

function parseReceipt(value) {
  if (!validateReceipt(value)) throw schemaError("Client setup receipt", validateReceipt.errors);
  validateTimestamp(value.completed_at, "Client setup receipt completion time");
  if (value.plan_digest !== sha256(stableJson(value.plan))) throw new Error("Client setup receipt plan digest is invalid.");
  validatePlanInvariants(value.plan, value.operation_id);
  validateProofSet(value.proofs);
  return value;
}

function validatePlanInvariants(plan, operationId) {
  if (new Set(plan.packages.map((entry) => entry.id)).size !== plan.packages.length) {
    throw new Error("Client setup plan repeats a local package.");
  }
  if (new Set(plan.extensions.map((entry) => entry.id)).size !== plan.extensions.length) {
    throw new Error("Client setup plan repeats a client extension.");
  }
  if (new Set(plan.workspace.project_defaults.map((entry) => entry.package_id)).size !== plan.workspace.project_defaults.length) {
    throw new Error("Client setup plan repeats a Project default package.");
  }
  for (const projectDefault of plan.workspace.project_defaults) {
    if (new Set(projectDefault.project_keys).size !== projectDefault.project_keys.length) {
      throw new Error(`Client setup Project default '${projectDefault.package_id}' repeats a Project key.`);
    }
  }
  if (plan.workspace.request.operationId !== operationId || plan.workspace.request.action !== "initialize_workspace") {
    throw new Error("Client setup workspace request does not belong to the setup operation.");
  }
}

function validateStepSet(steps) {
  if (steps.map((entry) => entry.step).join("\u0000") !== STEPS.join("\u0000")) {
    throw new Error("Client setup operation steps are not in canonical order.");
  }
  for (const entry of steps) {
    if (entry.state === "complete" && !entry.proof) throw new Error(`Client setup step '${entry.step}' has no proof.`);
    if (entry.state === "pending" && entry.proof) throw new Error(`Client setup pending step '${entry.step}' already has proof.`);
    if (entry.proof) validateProof(entry.proof, entry.step);
  }
}

function validateProofSet(proofs) {
  if (proofs.map((entry) => entry.step).join("\u0000") !== STEPS.join("\u0000")) {
    throw new Error("Client setup receipt proofs are not in canonical order.");
  }
  for (const proof of proofs) validateProof(proof, proof.step);
}

function validateProof(proof, step) {
  validateTimestamp(proof.observed_at, `Client setup '${step}' proof observation time`);
  const expectedKind = PROOF_KIND[step];
  const references = proof.references.map((reference, index) => normalizeProofReference(reference, expectedKind, step, index));
  if (proof.evidence_digest !== sha256(stableJson(references))) {
    throw new Error(`Client setup '${step}' proof digest is invalid.`);
  }
}

function normalizeProofReference(value, expectedKind, step, index) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`Client setup '${step}' proof reference ${index + 1} must be an object.`);
  }
  if (value.kind !== expectedKind) throw new Error(`Client setup '${step}' proof has a foreign evidence kind.`);
  const id = typeof value.id === "string" ? value.id.trim() : "";
  if (!/^[A-Za-z0-9][A-Za-z0-9._-]{0,159}$/.test(id)) {
    throw new Error(`Client setup '${step}' proof reference ${index + 1} has an invalid id.`);
  }
  let evidence;
  try {
    evidence = JSON.parse(JSON.stringify(value.evidence));
  } catch (error) {
    throw new Error(`Client setup '${step}' proof reference ${index + 1} evidence is not serializable: ${errorMessage(error)}`);
  }
  if (!evidence || typeof evidence !== "object" || Array.isArray(evidence)) {
    throw new Error(`Client setup '${step}' proof reference ${index + 1} evidence must be an object.`);
  }
  const digest = requiredDigest(value.digest, `Client setup '${step}' proof reference ${index + 1} digest`);
  if (digest !== sha256(stableJson(evidence))) {
    throw new Error(`Client setup '${step}' proof reference ${index + 1} digest does not match its frozen evidence.`);
  }
  if (Object.keys(value).some((key) => !["kind", "id", "digest", "evidence"].includes(key))) {
    throw new Error(`Client setup '${step}' proof reference ${index + 1} contains unknown fields.`);
  }
  return { kind: expectedKind, id, digest, evidence };
}

function projectOperation(operation) {
  return {
    status: "in_progress",
    operationId: operation.operation_id,
    planDigest: operation.plan_digest,
    plan: operation.plan,
    steps: operation.steps.map((step) => ({
      step: step.step,
      state: step.state,
      evidenceDigest: step.proof?.evidence_digest ?? "",
    })),
    updatedAt: operation.updated_at,
    lastError: operation.last_error ? {
      step: operation.last_error.step,
      message: operation.last_error.message,
      observedAt: operation.last_error.observed_at,
    } : null,
    completedAt: "",
    receiptDigest: "",
  };
}

function projectReceipt(receipt) {
  return {
    status: "complete",
    operationId: receipt.operation_id,
    planDigest: receipt.plan_digest,
    plan: receipt.plan,
    steps: receipt.proofs.map((proof) => ({
      step: proof.step,
      state: "complete",
      evidenceDigest: proof.evidence_digest,
    })),
    updatedAt: receipt.completed_at,
    lastError: null,
    completedAt: receipt.completed_at,
    receiptDigest: sha256(stableJson(receipt)),
  };
}

async function readJsonIfPresent(file, label) {
  let bytes;
  try {
    bytes = await stableReadFile(file, MAX_DOCUMENT_BYTES, label);
  } catch (error) {
    if (error?.code === "ENOENT") return null;
    throw error;
  }
  try {
    return JSON.parse(bytes.toString("utf8"));
  } catch (error) {
    throw new Error(`${label} is malformed JSON: ${errorMessage(error)}`);
  }
}

async function stableReadFile(file, maximumBytes, label) {
  const noFollow = typeof fs.constants.O_NOFOLLOW === "number" ? fs.constants.O_NOFOLLOW : 0;
  const handle = await fsPromises.open(file, fs.constants.O_RDONLY | noFollow);
  try {
    const before = await handle.stat();
    if (!before.isFile() || before.size > maximumBytes) throw new Error(`${label} exceeds its size limit or is not a regular file`);
    const bytes = await handle.readFile();
    const after = await handle.stat();
    if (bytes.byteLength !== before.size || before.size !== after.size || before.mtimeMs !== after.mtimeMs) {
      throw new Error(`${label} changed while it was being read`);
    }
    return bytes;
  } finally {
    await handle.close();
  }
}

async function writeJsonAtomic(file, value) {
  const bytes = Buffer.from(`${JSON.stringify(value, null, 2)}\n`, "utf8");
  if (bytes.byteLength > MAX_DOCUMENT_BYTES) throw new Error("Client setup document exceeds its size limit.");
  const temporary = temporaryPath(file);
  await writeTemporary(temporary, bytes);
  try {
    await fsPromises.rename(temporary, file);
    await syncDirectory(path.dirname(file));
  } catch (error) {
    await fsPromises.unlink(temporary).catch(() => undefined);
    throw error;
  }
}

async function writeJsonCreate(file, value) {
  const bytes = Buffer.from(`${JSON.stringify(value, null, 2)}\n`, "utf8");
  if (bytes.byteLength > MAX_DOCUMENT_BYTES) throw new Error("Client setup receipt exceeds its size limit.");
  const temporary = temporaryPath(file);
  await writeTemporary(temporary, bytes);
  try {
    await fsPromises.link(temporary, file);
    await fsPromises.unlink(temporary);
    await syncDirectory(path.dirname(file));
  } catch (error) {
    await fsPromises.unlink(temporary).catch(() => undefined);
    throw error;
  }
}

async function writeTemporary(file, bytes) {
  const handle = await fsPromises.open(file, "wx", 0o600);
  try {
    await handle.writeFile(bytes);
    await handle.sync();
  } finally {
    await handle.close();
  }
}

function temporaryPath(file) {
  return path.join(path.dirname(file), `.${path.basename(file)}-${process.pid}-${crypto.randomUUID()}.tmp`);
}

async function syncDirectory(directory) {
  const handle = await fsPromises.open(directory, "r");
  try {
    await handle.sync();
  } finally {
    await handle.close();
  }
}

function withClientSetupLock(root, operation, tails = transactionTails) {
  const key = path.resolve(root);
  const previous = tails.get(key) ?? Promise.resolve();
  let release;
  const current = new Promise((resolve) => { release = resolve; });
  const tail = previous.then(() => current);
  tails.set(key, tail);
  return previous.then(operation).finally(() => {
    release();
    if (tails.get(key) === tail) tails.delete(key);
  });
}

function joinClientSetupResume(root, operationId, stopBefore, listener, execute) {
  const key = JSON.stringify([root, operationId, stopBefore ?? null]);
  let active = activeResumes.get(key);
  if (!active) {
    active = { listeners: new Set(), progress: null, promise: null };
    activeResumes.set(key, active);
    active.promise = Promise.resolve().then(() => execute((progress) => {
      active.progress = progress;
      for (const notify of active.listeners) {
        try { notify(progress); } catch {}
      }
    })).finally(() => activeResumes.delete(key));
  }
  if (typeof listener === "function") {
    active.listeners.add(listener);
    if (active.progress) { try { listener(active.progress); } catch {} }
  }
  return active.promise;
}

function reportProgress(listener, operationId, step, phase, percent, detail) {
  try {
    listener({
      operationId,
      step,
      phase,
      percent: Math.max(0, Math.min(100, percent)),
      ...(typeof detail === "string" && detail.trim() ? { detail: detail.trim() } : {}),
    });
  } catch {}
}

function requiredAbsolutePath(value, label) {
  const text = typeof value === "string" ? value.trim() : "";
  if (!text || !path.isAbsolute(text)) throw new Error(`${label} must be an absolute path.`);
  return text;
}

function requiredOperationId(value) {
  const operationId = typeof value === "string" ? value.trim() : "";
  if (!OPERATION_ID.test(operationId)) throw new Error("Client setup operation id is invalid.");
  return operationId;
}

function requiredDigest(value, label) {
  const digest = typeof value === "string" ? value.trim() : "";
  if (!DIGEST.test(digest)) throw new Error(`${label} is invalid.`);
  return digest;
}

function validateTimestamp(value, label) {
  if (typeof value !== "string" || !Number.isFinite(Date.parse(value))) throw new Error(`${label} is invalid.`);
}

function timestamp() {
  return new Date().toISOString();
}

function operationCollision(operationId) {
  return new Error(`Client setup operation '${operationId}' already belongs to another immutable plan.`);
}

function schemaError(label, errors) {
  const detail = (errors ?? []).slice(0, 8).map((error) => `${error.instancePath || "/"} ${error.message || error.keyword}`).join("; ");
  return new Error(`${label} violates its schema${detail ? `: ${detail}` : ""}`);
}

function sha256(value) {
  return crypto.createHash("sha256").update(value).digest("hex");
}

function stableJson(value) {
  if (Array.isArray(value)) return `[${value.map(stableJson).join(",")}]`;
  if (value && typeof value === "object") {
    return `{${Object.keys(value).sort().map((key) => `${JSON.stringify(key)}:${stableJson(value[key])}`).join(",")}}`;
  }
  return JSON.stringify(value);
}

function errorMessage(error) {
  return error instanceof Error ? error.message : String(error);
}

module.exports = { createClientSetupStore };
