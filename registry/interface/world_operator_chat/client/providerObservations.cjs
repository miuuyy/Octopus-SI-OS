module.exports = { delayProviderLoginPoll, providerAccountArtifactCoordinates, providerAccountEvidence, providerAccountEvidenceFromCoordinates, providerAccountHomeOutput, providerAccountOperationError, providerAccountSnapshotCoordinates, providerAccountTurnError, providerAccountVerificationError, providerExactPayload, providerLoginExternalUrl, providerLoginId, providerLoginRemainingMs, providerLoginStartedHomeOutput, providerLoginStatusHomeOutput, providerModelCatalogOutput, providerModelCatalogRemainingMs, resolveProviderAccountArtifact, validateProviderAccountSnapshotPayload, validateProviderLoginCompletedPayload, validateProviderLoginStartedPayload, validateProviderLoginStatusPayload, validateProviderModelCatalogPayload, waitForProviderAccountObservation, waitForProviderLoginMovement, waitForProviderLoginStarted, waitForProviderLoginStatus, waitForProviderModelCatalogObservation };

const { readCompleteLedgerTail } = require("./chatArtifacts.cjs");
const { PROVIDER_ACCOUNT_EVIDENCE_SCHEMA } = require("./constants.cjs");
const { normalizeLiveCursor, watchLedgerChange } = require("./liveCursors.cjs");
const { artifactLabel, contentHashHex, endpointInterfaceId, exactOutboundRuntime, isObject, nonNegativeInteger, requiredArray, requiredObject, requiredString, requiredTrimmedString, safeSegment, stringValue } = require("./values.cjs");

async function waitForProviderModelCatalogObservation(input) {
  let offset = input.startOffset;
  for (;;) {
    const remaining = providerModelCatalogRemainingMs(input.deadline);
    const change = watchLedgerChange(input.movementsPath, remaining);
    let tail;
    try {
      tail = await readCompleteLedgerTail(input.movementsPath, offset);
      for (let index = 0; index < tail.records.length; index += 1) {
        let movement;
        try {
          movement = JSON.parse(tail.records[index].text);
        } catch (error) {
          throw new Error(`movement ledger contains malformed complete JSON at model-catalog tail line ${index + 1}: ${error.message}`);
        }
        offset = tail.records[index].nextOffset;
        const observed = await providerModelCatalogOutput(
          movement,
          input,
        );
        if (!observed) continue;
        change.cancel();
        return observed;
      }
    } catch (error) {
      change.cancel();
      throw error;
    }
    if (!(await change.promise)) {
      change.cancel();
      throw new Error("provider model catalog did not produce an exact client observation in time");
    }
  }
}

async function providerModelCatalogOutput(movement, input) {
  if (
    movement?.kind !== "InterfaceOutbound"
    || endpointInterfaceId(movement.to) !== input.input.interfaceId
  ) {
    return null;
  }
  const seq = nonNegativeInteger(movement.seq, "provider model catalog movement seq");
  if (seq <= input.requestSeq) return null;
  const runtime = exactOutboundRuntime(movement, input.input.interfaceId);
  if (!runtime || runtime.run !== input.input.runId) return null;
  if (endpointInterfaceId(movement.from) !== input.context.runtimeProviderInterfaceId) return null;
  const artifactId = requiredString(movement?.artifact?.id, "provider model catalog artifact id");
  const label = artifactLabel(artifactId);
  if (![input.context.outputLabel, input.context.operationErrorLabel, input.context.turnErrorLabel].includes(label)) {
    return null;
  }
  const artifact = providerAccountArtifactCoordinates(
    movement,
    label,
    "provider model catalog output",
  );
  const resolved = await resolveProviderAccountArtifact(
    input.readArtifactPayload,
    artifact,
    label,
  );
  if (label === input.context.operationErrorLabel) {
    throw providerAccountOperationError(resolved.payload, input.context.requestLabel);
  }
  if (label === input.context.turnErrorLabel) {
    const error = providerAccountTurnError(resolved.payload);
    if (!error.repairable) {
      error.name = "ProviderModelCatalogError";
      error.code = "provider_model_catalog_turn_error";
      error.message = "The provider failed to refresh the model catalog.";
    }
    throw error;
  }
  return {
    ...validateProviderModelCatalogPayload(resolved.payload, input.context.outputLabel),
    observedAtUnixMs: nonNegativeInteger(
      movement.recorded_at_unix_ms,
      "provider model catalog observation timestamp",
    ),
  };
}

function validateProviderModelCatalogPayload(value, expectedLabel) {
  const payload = requiredObject(value, "provider model catalog payload");
  if (payload.label !== expectedLabel) {
    throw new Error("provider model catalog payload carries the wrong label");
  }
  const models = requiredArray(payload.models, "provider model catalog models");
  const normalized = [];
  const seen = new Set();
  for (let index = 0; index < models.length; index += 1) {
    const model = requiredObject(models[index], `provider model catalog model ${index}`);
    const modelId = requiredString(model.model, `provider model catalog model ${index}.model`);
    if (seen.has(modelId)) throw new Error(`provider model catalog repeats '${modelId}'`);
    seen.add(modelId);
    const efforts = requiredArray(
      model.supportedReasoningEfforts,
      `provider model catalog model ${index}.supportedReasoningEfforts`,
    ).map((value, effortIndex) => {
      const effort = requiredObject(
        value,
        `provider model catalog model ${index}.supportedReasoningEfforts ${effortIndex}`,
      );
      return requiredString(
        effort.reasoningEffort,
        `provider model catalog model ${index}.supportedReasoningEfforts ${effortIndex}.reasoningEffort`,
      );
    });
    if (new Set(efforts).size !== efforts.length) {
      throw new Error(`provider model '${modelId}' repeats a reasoning effort`);
    }
    const defaultReasoningEffort = requiredString(
      model.defaultReasoningEffort,
      `provider model catalog model ${index}.defaultReasoningEffort`,
    );
    if (!efforts.includes(defaultReasoningEffort)) {
      throw new Error(`provider model '${modelId}' default reasoning effort is outside its supported list`);
    }
    normalized.push({
      model: modelId,
      displayName: requiredString(
        model.displayName,
        `provider model catalog model ${index}.displayName`,
      ),
      description: stringValue(model.description),
      defaultReasoningEffort,
      supportedReasoningEfforts: efforts,
      showInPicker: model.hidden !== true,
      isDefault: model.isDefault === true,
    });
  }
  let nextCursor = null;
  if (payload.next_cursor !== null) {
    nextCursor = requiredString(payload.next_cursor, "provider model catalog next_cursor");
  }
  return { models: normalized, nextCursor };
}

function providerModelCatalogRemainingMs(deadline) {
  const remaining = deadline - Date.now();
  if (remaining <= 0) throw new Error("provider model catalog refresh timed out");
  return remaining;
}

async function providerAccountSnapshotCoordinates(
  movement,
  input,
  context,
  readArtifactPayload,
  requestSeq,
) {
  const seq = nonNegativeInteger(movement?.seq, "provider account snapshot movement seq");
  if (seq <= requestSeq) {
    throw new Error("provider account snapshot does not follow its admitted request");
  }
  const runtime = exactOutboundRuntime(movement, input.interfaceId);
  if (!runtime || runtime.run !== input.runId) {
    throw new Error("provider account snapshot has the wrong Home/run coordinates");
  }
  if (endpointInterfaceId(movement.from) !== context.runtimeProviderInterfaceId) {
    throw new Error("provider account snapshot came from the wrong exact provider interface");
  }
  const artifact = providerAccountArtifactCoordinates(
    movement,
    context.snapshotLabel,
    "provider account snapshot",
  );
  const resolved = await resolveProviderAccountArtifact(
    readArtifactPayload,
    artifact,
    context.snapshotLabel,
  );
  validateProviderAccountSnapshotPayload(resolved.payload);
  return {
    snapshotMovementSeq: seq,
    snapshotCheckpointId: requiredString(runtime.checkpoint, "provider account checkpoint"),
    snapshotArtifactId: artifact.artifactId,
    snapshotManifestHash: artifact.manifestHash,
    snapshotPayloadHash: resolved.payloadHash,
    observedAtUnixMs: nonNegativeInteger(
      movement.recorded_at_unix_ms,
      "provider account observation timestamp",
    ),
  };
}

function providerAccountArtifactCoordinates(movement, expectedLabel, field) {
  const artifact = requiredObject(movement?.artifact, `${field} artifact`);
  const artifactId = requiredString(artifact.id, `${field} artifact id`);
  const manifestHash = contentHashHex(artifact.content, `${field} artifact content`);
  if (artifactId !== `${expectedLabel}:${manifestHash}`) {
    throw new Error(`${field} identity does not match its content hash`);
  }
  return { artifactId, manifestHash };
}

async function resolveProviderAccountArtifact(readArtifactPayload, artifact, label) {
  const resolved = await readArtifactPayload({
    artifactId: artifact.artifactId,
    manifestHash: artifact.manifestHash,
    label,
  });
  if (
    !isObject(resolved)
    || resolved.label !== label
    || contentHashHex(resolved.manifestHash, "resolved provider artifact manifest hash")
      !== artifact.manifestHash
  ) {
    throw new Error("resolved provider artifact differs from its ledger identity");
  }
  return {
    payload: resolved.payload,
    payloadHash: contentHashHex(
      resolved.payloadHash,
      "resolved provider artifact payload hash",
    ),
  };
}

function validateProviderAccountSnapshotPayload(value) {
  const payload = requiredObject(value, "provider account snapshot payload");
  if (payload.account === null) {
    throw providerAccountVerificationError(
      "provider_account_auth_required",
      "The provider account requires a fresh sign-in.",
      true,
      true,
    );
  }
  if (typeof payload.requiresOpenaiAuth !== "boolean" || !isObject(payload.account)) {
    throw providerAccountVerificationError(
      "provider_account_snapshot_malformed",
      "The provider account snapshot did not satisfy its declared contract.",
      false,
      false,
    );
  }
}

function providerAccountVerificationError(code, message, retryable, repairable) {
  const error = new Error(message);
  error.name = "ProviderAccountVerificationError";
  error.code = code;
  error.retryable = retryable;
  error.repairable = repairable;
  return error;
}

async function waitForProviderAccountObservation(input) {
  const deadline = Date.now() + input.timeoutMs;
  let cursor = normalizeLiveCursor(input.cursor);
  for (;;) {
    const remaining = Math.max(0, deadline - Date.now());
    const change = watchLedgerChange(input.movementsPath, remaining);
    let tail;
    try {
      tail = await readCompleteLedgerTail(input.movementsPath, cursor.offset);
      let expectedSeq = cursor.seq;
      for (let index = 0; index < tail.records.length; index += 1) {
        const record = tail.records[index];
        let movement;
        try {
          movement = JSON.parse(record.text);
        } catch (error) {
          throw new Error(`movement ledger contains malformed complete JSON at tail line ${index + 1}: ${error.message}`);
        }
        const seq = nonNegativeInteger(movement?.seq, "movement seq");
        if (expectedSeq !== null && seq !== expectedSeq) {
          throw new Error(`movement ledger is non-contiguous at seq ${seq}; expected ${expectedSeq}`);
        }
        expectedSeq = seq + 1;
        const nextCursor = { offset: record.nextOffset, seq: expectedSeq };
        let snapshot;
        try {
          snapshot = await providerAccountHomeOutput(
            movement,
            input,
            seq,
            input.requestCoordinates.requestMovementSeq,
          );
        } catch (error) {
          if (error?.consumeProviderAccountOutput === true) {
            Object.defineProperty(error, "providerAccountCursor", { value: nextCursor });
          }
          throw error;
        }
        cursor = nextCursor;
        if (!snapshot) continue;
        change.cancel();
        return {
          cursor,
          evidence: providerAccountEvidenceFromCoordinates(
            input,
            input.requestCoordinates,
            snapshot,
          ),
        };
      }
    } catch (error) {
      change.cancel();
      throw error;
    }
    if (remaining === 0 || !(await change.promise)) {
      change.cancel();
      throw providerAccountVerificationError(
        "provider_account_read_timeout",
        "The provider account read did not produce an exact Home observation in time.",
        true,
        false,
      );
    }
  }
}

async function providerAccountHomeOutput(movement, input, seq, requestSeq) {
  if (movement?.kind !== "InterfaceOutbound" || endpointInterfaceId(movement.to) !== input.interfaceId) {
    return null;
  }
  const runtime = exactOutboundRuntime(movement, input.interfaceId);
  if (!runtime || runtime.run !== input.runId) {
    throw new Error("provider account wait observed an outbound for the wrong Home run");
  }
  const source = endpointInterfaceId(movement.from);
  if (!source) throw new Error("provider account wait observed a non-interface Home outbound");
  const artifactId = requiredString(movement?.artifact?.id, "Home outbound artifact id");
  const label = artifactLabel(artifactId);
  providerAccountArtifactCoordinates(movement, label, "Home outbound");
  if (seq <= requestSeq) {
    if (source === input.context.runtimeProviderInterfaceId && label === input.context.snapshotLabel) return null;
    if (source === input.context.runtimeProviderInterfaceId && label === input.context.operationErrorLabel) {
      const artifact = providerAccountArtifactCoordinates(movement, label, "provider operation error");
      const resolved = await resolveProviderAccountArtifact(
        input.readArtifactPayload,
        artifact,
        label,
      );
      const payload = requiredObject(resolved.payload, "provider operation error payload");
      if (payload.operation === input.context.requestLabel && payload.status === "error") return null;
    }
    if (source === input.context.runtimeProviderInterfaceId && label === input.context.turnErrorLabel) {
      const artifact = providerAccountArtifactCoordinates(movement, label, "provider turn error");
      const resolved = await resolveProviderAccountArtifact(
        input.readArtifactPayload,
        artifact,
        label,
      );
      if (providerAccountTurnError(resolved.payload).repairable === true) return null;
    }
    throw new Error("provider account wait refuses to consume an unrelated pending Home output");
  }
  if (source !== input.context.runtimeProviderInterfaceId) {
    throw new Error("provider account wait observed output from the wrong exact provider interface");
  }
  if (label === input.context.snapshotLabel) {
    try {
      return await providerAccountSnapshotCoordinates(
        movement,
        input,
        input.context,
        input.readArtifactPayload,
        requestSeq,
      );
    } catch (error) {
      if (error?.name === "ProviderAccountVerificationError") {
        Object.defineProperty(error, "consumeProviderAccountOutput", { value: true });
      }
      throw error;
    }
  }
  if (label === input.context.operationErrorLabel) {
    const artifact = providerAccountArtifactCoordinates(movement, label, "provider operation error");
    const resolved = await resolveProviderAccountArtifact(
      input.readArtifactPayload,
      artifact,
      label,
    );
    const error = providerAccountOperationError(resolved.payload, input.context.requestLabel);
    Object.defineProperty(error, "consumeProviderAccountOutput", { value: true });
    throw error;
  }
  if (label === input.context.turnErrorLabel) {
    const artifact = providerAccountArtifactCoordinates(movement, label, "provider turn error");
    const resolved = await resolveProviderAccountArtifact(
      input.readArtifactPayload,
      artifact,
      label,
    );
    const error = providerAccountTurnError(resolved.payload);
    if (error.repairable === true) {
      Object.defineProperty(error, "consumeProviderAccountOutput", { value: true });
    }
    throw error;
  }
  throw new Error("provider account wait refuses to consume an unrelated Home output");
}

function providerAccountOperationError(value, expectedOperation) {
  const payload = requiredObject(value, "provider operation error payload");
  const detail = requiredObject(payload.error, "provider operation error detail");
  if (payload.operation !== expectedOperation || payload.status !== "error") {
    return providerAccountVerificationError(
      "provider_account_operation_error_malformed",
      "The provider returned an invalid operation error artifact.",
      false,
      false,
    );
  }
  return providerAccountVerificationError(
    "provider_account_operation_error",
    "The provider operation failed inside the provider interface.",
    detail.retryable === true,
    false,
  );
}

function providerAccountTurnError(value) {
  const payload = requiredObject(value, "provider turn error payload");
  const authenticationFailure = payload.failure_code === "provider.authentication.invalid";
  return providerAccountVerificationError(
    authenticationFailure ? "provider_account_auth_invalid" : "provider_account_turn_error",
    authenticationFailure
      ? "The provider rejected the current account authentication."
      : "The provider emitted a turn error while reading the account.",
    authenticationFailure,
    authenticationFailure,
  );
}

async function waitForProviderLoginStarted(input) {
  return waitForProviderLoginMovement(input, async (movement, seq) => {
    const output = await providerLoginStartedHomeOutput(movement, input, seq);
    if (!output) return null;
    return { done: true, loginId: output.loginId, authUrl: output.authUrl };
  });
}

async function waitForProviderLoginStatus(input) {
  let completion = null;
  return waitForProviderLoginMovement(input, async (movement, seq) => {
    const output = await providerLoginStatusHomeOutput(movement, input, seq);
    if (!output || output.kind === "account_updated") return null;
    if (output.kind === "completion") {
      if (completion !== null && completion !== output.success) {
        throw providerAccountVerificationError(
          "provider_account_login_completion_conflict",
          "The provider emitted conflicting login completion observations.",
          false,
          false,
        );
      }
      completion = output.success;
      return null;
    }
    if (output.status === "pending") {
      if (completion !== null) {
        throw providerAccountVerificationError(
          "provider_account_login_status_conflict",
          "The provider login status conflicts with its completion observation.",
          false,
          false,
        );
      }
      return { done: true, completed: false };
    }
    if (output.status === "cancelled") {
      throw providerAccountVerificationError(
        "provider_account_login_cancelled",
        "The provider sign-in was cancelled.",
        true,
        false,
      );
    }
    if (completion !== null && completion !== output.success) {
      throw providerAccountVerificationError(
        "provider_account_login_status_conflict",
        "The provider login status conflicts with its completion observation.",
        false,
        false,
      );
    }
    if (output.success === true && completion !== true) {
      throw providerAccountVerificationError(
        "provider_account_login_completion_missing",
        "The provider reported a completed sign-in without its exact completion observation.",
        false,
        false,
      );
    }
    if (output.success !== true) {
      throw providerAccountVerificationError(
        "provider_account_login_failed",
        "The provider sign-in did not complete successfully.",
        true,
        false,
      );
    }
    return { done: true, completed: true };
  });
}

async function waitForProviderLoginMovement(input, observe) {
  let cursor = normalizeLiveCursor(input.cursor);
  for (;;) {
    const remaining = providerLoginRemainingMs(input.deadline);
    const change = watchLedgerChange(input.movementsPath, remaining);
    try {
      const tail = await readCompleteLedgerTail(input.movementsPath, cursor.offset);
      let expectedSeq = cursor.seq;
      for (let index = 0; index < tail.records.length; index += 1) {
        let movement;
        try {
          movement = JSON.parse(tail.records[index].text);
        } catch (error) {
          throw new Error(`movement ledger contains malformed complete JSON at tail line ${index + 1}: ${error.message}`);
        }
        const seq = nonNegativeInteger(movement?.seq, "movement seq");
        if (expectedSeq !== null && seq !== expectedSeq) {
          throw new Error(`movement ledger is non-contiguous at seq ${seq}; expected ${expectedSeq}`);
        }
        expectedSeq = seq + 1;
        const nextCursor = { offset: tail.records[index].nextOffset, seq: expectedSeq };
        let observed;
        try {
          observed = await observe(movement, seq);
        } catch (error) {
          Object.defineProperty(error, "providerAccountCursor", { value: nextCursor });
          throw error;
        }
        cursor = nextCursor;
        if (!observed?.done) continue;
        change.cancel();
        return { ...observed, cursor };
      }
    } catch (error) {
      change.cancel();
      throw error;
    }
    if (!(await change.promise)) {
      change.cancel();
      const error = providerAccountVerificationError(
        "provider_account_login_timeout",
        "The provider sign-in did not produce an exact Home observation in time.",
        true,
        false,
      );
      Object.defineProperty(error, "providerAccountCursor", { value: cursor });
      throw error;
    }
  }
}

async function providerLoginStartedHomeOutput(movement, input, seq) {
  if (movement?.kind !== "InterfaceOutbound" || endpointInterfaceId(movement.to) !== input.interfaceId) {
    return null;
  }
  const runtime = exactOutboundRuntime(movement, input.interfaceId);
  if (!runtime || runtime.run !== input.runId) {
    throw new Error("provider login start observed an outbound for the wrong Home run");
  }
  if (endpointInterfaceId(movement.from) !== input.context.runtimeProviderInterfaceId) {
    throw new Error("provider login start observed output from the wrong exact provider interface");
  }
  if (seq <= input.requestSeq) {
    throw new Error("provider login start observed a Home output that does not follow its request");
  }
  const artifactId = requiredString(movement?.artifact?.id, "provider login Home output artifact id");
  const label = artifactLabel(artifactId);
  const artifact = providerAccountArtifactCoordinates(movement, label, "provider login Home output");
  const resolved = await resolveProviderAccountArtifact(input.readArtifactPayload, artifact, label);
  if (label === input.context.login.startedLabel) {
    return validateProviderLoginStartedPayload(resolved.payload, input.context.login.startRequestLabel);
  }
  if (label === input.context.operationErrorLabel) {
    throw providerAccountOperationError(resolved.payload, input.context.login.startRequestLabel);
  }
  if (label === input.context.turnErrorLabel) {
    throw providerAccountTurnError(resolved.payload);
  }
  throw new Error("provider login start refuses to consume an unrelated Home output");
}

async function providerLoginStatusHomeOutput(movement, input, seq) {
  if (movement?.kind !== "InterfaceOutbound" || endpointInterfaceId(movement.to) !== input.interfaceId) {
    return null;
  }
  const runtime = exactOutboundRuntime(movement, input.interfaceId);
  if (!runtime || runtime.run !== input.runId) {
    throw new Error("provider login status observed an outbound for the wrong Home run");
  }
  if (endpointInterfaceId(movement.from) !== input.context.runtimeProviderInterfaceId) {
    throw new Error("provider login status observed output from the wrong exact provider interface");
  }
  const artifactId = requiredString(movement?.artifact?.id, "provider login status artifact id");
  const label = artifactLabel(artifactId);
  const artifact = providerAccountArtifactCoordinates(movement, label, "provider login status output");
  const resolved = await resolveProviderAccountArtifact(input.readArtifactPayload, artifact, label);
  if (label === input.context.login.completedLabel) {
    if (seq <= input.startSeq) {
      throw new Error("provider login completion does not follow its login-start request");
    }
    return { kind: "completion", ...validateProviderLoginCompletedPayload(resolved.payload, input.loginId) };
  }
  if (label === input.context.login.statusLabel) {
    if (seq <= input.requestSeq) {
      throw new Error("provider login status does not follow its status-read request");
    }
    return { kind: "status", ...validateProviderLoginStatusPayload(resolved.payload, input.loginId, input.context.login.statusRequestLabel) };
  }
  if (label === input.context.login.accountUpdatedLabel) {
    if (seq <= input.startSeq) {
      throw new Error("provider account update does not follow its login-start request");
    }
    requiredObject(resolved.payload, "provider account-updated payload");
    return { kind: "account_updated" };
  }
  if (label === input.context.operationErrorLabel) {
    if (seq <= input.requestSeq) {
      throw new Error("provider login-status error does not follow its request");
    }
    throw providerAccountOperationError(resolved.payload, input.context.login.statusRequestLabel);
  }
  if (label === input.context.turnErrorLabel) {
    if (seq <= input.requestSeq) {
      throw new Error("provider login turn error does not follow its status request");
    }
    throw providerAccountTurnError(resolved.payload);
  }
  throw new Error("provider login status refuses to consume an unrelated Home output");
}

function validateProviderLoginStartedPayload(value, expectedOperation) {
  const payload = providerExactPayload(value, "provider login-start payload", [
    "operation",
    "method",
    "status",
    "login_id",
    "auth_url",
  ]);
  if (
    payload.operation !== expectedOperation
    || payload.method !== "chatgpt_browser"
    || payload.status !== "pending"
  ) {
    throw providerAccountVerificationError(
      "provider_account_login_started_malformed",
      "The provider returned an invalid browser sign-in start observation.",
      false,
      false,
    );
  }
  return {
    loginId: providerLoginId(payload.login_id),
    authUrl: providerLoginExternalUrl(payload.auth_url),
  };
}

function validateProviderLoginCompletedPayload(value, expectedLoginId) {
  const payload = providerExactPayload(value, "provider login-completed payload", [
    "login_id",
    "status",
    "success",
    "failure",
  ]);
  if (providerLoginId(payload.login_id) !== expectedLoginId) {
    throw providerAccountVerificationError(
      "provider_account_login_id_mismatch",
      "The provider completed a different sign-in attempt.",
      false,
      false,
    );
  }
  if (
    typeof payload.success !== "boolean"
    || payload.status !== (payload.success ? "completed" : "failed")
    || (payload.success ? payload.failure !== null : !isObject(payload.failure))
  ) {
    throw providerAccountVerificationError(
      "provider_account_login_completed_malformed",
      "The provider returned an invalid sign-in completion observation.",
      false,
      false,
    );
  }
  return { success: payload.success };
}

function validateProviderLoginStatusPayload(value, expectedLoginId, expectedOperation) {
  const payload = providerExactPayload(value, "provider login-status payload", [
    "operation",
    "login_id",
    "state",
  ]);
  if (payload.operation !== expectedOperation || providerLoginId(payload.login_id) !== expectedLoginId) {
    throw providerAccountVerificationError(
      "provider_account_login_id_mismatch",
      "The provider returned status for a different sign-in attempt.",
      false,
      false,
    );
  }
  const state = requiredObject(payload.state, "provider login state");
  if (state.status === "pending") {
    providerExactPayload(state, "provider pending login state", ["status", "method"]);
    if (state.method !== "chatgpt_browser") {
      throw providerAccountVerificationError(
        "provider_account_login_status_malformed",
        "The provider returned an invalid pending browser sign-in state.",
        false,
        false,
      );
    }
    return { status: "pending", success: null };
  }
  if (state.status === "completed") {
    providerExactPayload(state, "provider completed login state", ["status", "success"]);
    if (typeof state.success !== "boolean") {
      throw providerAccountVerificationError(
        "provider_account_login_status_malformed",
        "The provider returned an invalid completed sign-in state.",
        false,
        false,
      );
    }
    return { status: "completed", success: state.success };
  }
  if (state.status === "cancelled") {
    providerExactPayload(state, "provider cancelled login state", ["status"]);
    return { status: "cancelled", success: false };
  }
  throw providerAccountVerificationError(
    "provider_account_login_status_malformed",
    "The provider returned an unknown sign-in state.",
    false,
    false,
  );
}

function providerExactPayload(value, field, fields) {
  const payload = requiredObject(value, field);
  const keys = Object.keys(payload);
  if (keys.length !== fields.length || keys.some((key) => !fields.includes(key))) {
    throw providerAccountVerificationError(
      "provider_account_login_payload_malformed",
      `The ${field} has an invalid field set.`,
      false,
      false,
    );
  }
  return payload;
}

function providerLoginId(value) {
  const loginId = requiredTrimmedString(value, "provider login_id");
  if (loginId.length > 512 || /[\u0000-\u001f\u007f]/.test(loginId)) {
    throw providerAccountVerificationError(
      "provider_account_login_id_invalid",
      "The provider returned an invalid sign-in identity.",
      false,
      false,
    );
  }
  return loginId;
}

function providerLoginExternalUrl(value) {
  const authUrl = requiredTrimmedString(value, "provider auth_url");
  let parsed;
  try {
    parsed = new URL(authUrl);
  } catch {
    parsed = null;
  }
  if (
    !parsed
    || parsed.protocol !== "https:"
    || !parsed.hostname
    || parsed.username
    || parsed.password
    || authUrl.length > 4_096
  ) {
    throw providerAccountVerificationError(
      "provider_account_login_url_invalid",
      "The provider returned an unsafe browser sign-in URL.",
      false,
      false,
    );
  }
  return authUrl;
}

function providerLoginRemainingMs(deadline) {
  const remaining = deadline - Date.now();
  if (!Number.isSafeInteger(remaining) || remaining <= 0) {
    throw providerAccountVerificationError(
      "provider_account_login_timeout",
      "The provider sign-in did not complete before its bounded deadline.",
      true,
      false,
    );
  }
  return remaining;
}

async function delayProviderLoginPoll(delayMs, deadline) {
  const remaining = providerLoginRemainingMs(deadline);
  await new Promise((resolve) => setTimeout(resolve, Math.min(delayMs, remaining)));
  providerLoginRemainingMs(deadline);
}

function providerAccountEvidenceFromCoordinates(input, request, snapshot) {
  return {
    schema: PROVIDER_ACCOUNT_EVIDENCE_SCHEMA,
    schema_version: 1,
    operation_id: input.operationId,
    run_id: input.runId,
    interface_id: input.interfaceId,
    provider_interface_id: input.providerInterfaceId,
    runtime_provider_interface_id: input.context.runtimeProviderInterfaceId,
    request_movement_seq: request.requestMovementSeq,
    request_artifact_id: request.requestArtifactId,
    request_manifest_hash: request.requestManifestHash,
    request_payload_hash: request.requestPayloadHash,
    snapshot_movement_seq: snapshot.snapshotMovementSeq,
    snapshot_checkpoint_id: snapshot.snapshotCheckpointId,
    snapshot_artifact_id: snapshot.snapshotArtifactId,
    snapshot_manifest_hash: snapshot.snapshotManifestHash,
    snapshot_payload_hash: snapshot.snapshotPayloadHash,
    observed_at_unix_ms: snapshot.observedAtUnixMs,
  };
}

function providerAccountEvidence(value) {
  const evidence = requiredObject(value, "provider account evidence");
  const fields = [
    "schema",
    "schema_version",
    "operation_id",
    "run_id",
    "interface_id",
    "provider_interface_id",
    "runtime_provider_interface_id",
    "request_movement_seq",
    "request_artifact_id",
    "request_manifest_hash",
    "request_payload_hash",
    "snapshot_movement_seq",
    "snapshot_checkpoint_id",
    "snapshot_artifact_id",
    "snapshot_manifest_hash",
    "snapshot_payload_hash",
    "observed_at_unix_ms",
  ];
  const unknown = Object.keys(evidence).filter((field) => !fields.includes(field));
  if (unknown.length > 0 || fields.some((field) => !Object.hasOwn(evidence, field))) {
    throw new Error("provider account evidence has an invalid field set");
  }
  if (evidence.schema !== PROVIDER_ACCOUNT_EVIDENCE_SCHEMA || evidence.schema_version !== 1) {
    throw new Error("provider account evidence has an unsupported schema");
  }
  const normalized = {
    schema: PROVIDER_ACCOUNT_EVIDENCE_SCHEMA,
    schema_version: 1,
    operation_id: safeSegment(evidence.operation_id, "evidence.operation_id"),
    run_id: safeSegment(evidence.run_id, "evidence.run_id"),
    interface_id: safeSegment(evidence.interface_id, "evidence.interface_id"),
    provider_interface_id: safeSegment(
      evidence.provider_interface_id,
      "evidence.provider_interface_id",
    ),
    runtime_provider_interface_id: safeSegment(
      evidence.runtime_provider_interface_id,
      "evidence.runtime_provider_interface_id",
    ),
    request_movement_seq: nonNegativeInteger(
      evidence.request_movement_seq,
      "evidence.request_movement_seq",
    ),
    request_artifact_id: requiredString(
      evidence.request_artifact_id,
      "evidence.request_artifact_id",
    ),
    request_manifest_hash: contentHashHex(
      evidence.request_manifest_hash,
      "evidence.request_manifest_hash",
    ),
    request_payload_hash: contentHashHex(
      evidence.request_payload_hash,
      "evidence.request_payload_hash",
    ),
    snapshot_movement_seq: nonNegativeInteger(
      evidence.snapshot_movement_seq,
      "evidence.snapshot_movement_seq",
    ),
    snapshot_checkpoint_id: requiredString(
      evidence.snapshot_checkpoint_id,
      "evidence.snapshot_checkpoint_id",
    ),
    snapshot_artifact_id: requiredString(
      evidence.snapshot_artifact_id,
      "evidence.snapshot_artifact_id",
    ),
    snapshot_manifest_hash: contentHashHex(
      evidence.snapshot_manifest_hash,
      "evidence.snapshot_manifest_hash",
    ),
    snapshot_payload_hash: contentHashHex(
      evidence.snapshot_payload_hash,
      "evidence.snapshot_payload_hash",
    ),
    observed_at_unix_ms: nonNegativeInteger(
      evidence.observed_at_unix_ms,
      "evidence.observed_at_unix_ms",
    ),
  };
  if (normalized.snapshot_movement_seq <= normalized.request_movement_seq) {
    throw new Error("provider account evidence snapshot does not follow its request");
  }
  return normalized;
}
