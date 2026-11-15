module.exports = { operatorChatTurnsProjection };
const { isHomeVoiceInput } = require("./operatorChatVoiceInput.cjs");
const { cachedOperatorChatTurnState } = require("./operatorChatCache.cjs");

const { movementBoundaryInterface, movementRunId } = require("./movementRows.cjs");
const { nativeTurnRecordedInputs, operatorChatEventIdentity, operatorChatExecutionScope, operatorChatExpandedStreamRows, operatorChatProviderFailureActivity, operatorChatRuntimeProviderId, operatorTurnControlEndsTurn, operatorTurnControlOutcome } = require("./operatorChatEvents.cjs");
const { isOperatorChatProviderFailureRow, isOperatorChatProviderMovement } = require("./registryRows.cjs");
const { arrayValue, ledgerArtifactLabel, numberValue, objectOrEmpty, requiredString, rowTimeMs, stringValue } = require("./values.cjs");

function operatorChatTurnsProjection(store) {
  async function operatorChatTurnOpening(entry, row, protocol, readPayload = (row) => store.resolveLedgerArtifactPayload(row, { projection: protocol.artifactReadProjection })) {
    if (movementRunId(row) !== entry.runId) return null;
    const label = ledgerArtifactLabel(row);
    if (isHomeVoiceInput(entry, row)) return "waiting";
    if (row.kind === "InterfaceArrival"
      && label === protocol.inputLabel
      && movementBoundaryInterface(row, "from") === entry.chatInterfaceId) {
      return "waiting";
    }
    const provider = operatorChatRuntimeProviderId(entry);
    if (!provider) return null;
    if (row.kind === "InterfaceOutbound" && movementBoundaryInterface(row, "to") === provider
      && arrayValue(entry.externalInputs).some((input) => input.interface_id === movementBoundaryInterface(row, "from")
        && input.checkpoint_id === stringValue(row.runtime?.checkpoint ?? row.invocation_metadata?.runtime?.checkpoint))) {
      return "waiting";
    }
    if (row.kind === "InterfaceOutbound"
      && movementBoundaryInterface(row, "to") === provider
      && label === "provider.peer.input") {
      const payload = await readPayload(row);
      return payload?.message?.action === "followup" ? "waiting" : null;
    }
    if (!isOperatorChatProviderMovement(entry, row)) return null;
    if (label === "turn.started") return "working";
    if (label === "provider.transport.dispatch") {
      const payload = await readPayload(row);
      return payload?.provider_interface === provider && payload?.method === "turn/start"
        ? "working" : null;
    }
    return null;
  }

  async function reduceOperatorChatTurnState(initial, rows, entry, protocol, readPayload = (row) => store.resolveLedgerArtifactPayload(row, { projection: protocol.artifactReadProjection })) {
    const ordered = [...rows].sort((left, right) => numberValue(left.seq, -1) - numberValue(right.seq, -1));
    const executionScope = operatorChatExecutionScope(entry);
    let executionObserved = initial.executionObserved ?? !executionScope;
    let open = initial.open;
    let state = initial.state;
    let lastMovementSeq = initial.lastMovementSeq;
    let updatedAtMs = initial.updatedAtMs;
    let openedMovementSeq = initial.openedMovementSeq ?? null;
    let activeNativeTurn = initial.activeNativeTurn ?? null;
    let pendingInputs = [...(initial.pendingInputs ?? [])];
    const nativeTurns = new Map(initial.nativeTurns ?? []);
    const recordedInputs = new Map([...(initial.recordedInputs ?? [])].map(([key, inputs]) => [key, new Set(inputs)]));
    const nativeKey = (payload) => {
      const thread = stringValue(payload?.thread_id ?? payload?.threadId);
      const turn = operatorChatEventIdentity(payload, "").turn_id;
      return thread && turn ? JSON.stringify([thread, turn]) : null;
    };
    const observeNative = (payload, source) => {
      const key = nativeKey(payload);
      if (!key || !Number.isSafeInteger(source) || source < 0) return null;
      nativeTurns.set(key, Math.min(nativeTurns.get(key) ?? source, source));
      return key;
    };
    const settle = (payload, row, outcome) => {
      const key = nativeKey(payload), nativeSource = nativeTurns.get(key);
      const source = payload?.source_movement_seq ?? payload?.additional_details?.source_movement_seq;
      pendingInputs = pendingInputs.filter((input) => {
        if (row.seq <= input.seq) return true;
        if (input.clientId && recordedInputs.get(key)?.has(input.clientId)) return false;
        return nativeSource < input.seq || Number.isSafeInteger(source) && source < input.seq;
      });
      if (!key || activeNativeTurn === true || activeNativeTurn === key) activeNativeTurn = null;
      open = pendingInputs.length > 0 || activeNativeTurn !== null;
      state = open ? "working" : outcome;
      recordedInputs.delete(key);
      if (!open) recordedInputs.clear();
    };
    for (const row of ordered) {
      lastMovementSeq = Math.max(lastMovementSeq, numberValue(row.seq, -1));
      updatedAtMs = Math.max(updatedAtMs, rowTimeMs(row));
      if (!executionObserved) {
        executionObserved = executionScope.checkpoints.has(stringValue(
          row.runtime?.checkpoint ?? row.invocation_metadata?.runtime?.checkpoint,
        )) || row.seq === executionScope.sourceMovementSeq;
        if (!executionObserved) continue;
      }
      const label = ledgerArtifactLabel(row);
      const providerMovement = isOperatorChatProviderMovement(entry, row);
      let providerPayload;
      if (providerMovement && label === "turn.stream") {
        const payloads = new Map([[row, await readPayload(row)]]);
        const expanded = await operatorChatExpandedStreamRows([row], payloads, entry.runId,
          (header) => observeNative(header, header.opened_source_movement_seq));
        for (const event of expanded) {
          if (event.__stream.event_kind !== "input_recorded") continue;
          const receipt = payloads.get(event);
          const key = observeNative(receipt, receipt.source_movement_seq);
          if (!recordedInputs.has(key)) recordedInputs.set(key, new Set());
          recordedInputs.get(key).add(receipt.client_id);
        }
      }
      if (providerMovement && label !== "turn.stream") {
        providerPayload = await readPayload(row);
        observeNative(providerPayload, providerPayload?.source_movement_seq ?? providerPayload?.additional_details?.source_movement_seq ?? row.seq);
        if (label === "turn.completed") {
          for (const input of nativeTurnRecordedInputs(providerPayload)) {
            const key = nativeKey(input);
            if (!recordedInputs.has(key)) recordedInputs.set(key, new Set());
            recordedInputs.get(key).add(input.client_id);
          }
        }
      }
      const opening = await operatorChatTurnOpening(entry, row, protocol, readPayload);
      if (opening) {
        if (opening === "working") activeNativeTurn = nativeKey(providerPayload) ?? true;
        if (isHomeVoiceInput(entry, row) || (row.kind === "InterfaceArrival" && label === protocol.inputLabel
          && movementBoundaryInterface(row, "from") === entry.chatInterfaceId)) {
          const payload = await readPayload(row);
          pendingInputs.push({ seq: row.seq, clientId: stringValue(payload?.message_id) });
        }
        if (!open) {
          open = true;
          openedMovementSeq = numberValue(row.seq, -1);
          state = opening;
        } else if (opening === "working") {
          state = "working";
        }
        continue;
      }
      if (["turn.control.acknowledged", "turn.control.failed"].includes(label)) {
        if (!providerMovement) continue;
        const outcome = operatorTurnControlOutcome(
          await readPayload(row),
          row,
        );
        if (outcome?.status === "acknowledged" && outcome.action === "resume") {
          open = true;
          activeNativeTurn = nativeKey(outcome.payload) ?? true;
          openedMovementSeq = numberValue(row.seq, -1);
          state = "working";
          continue;
        }
        if (!open) continue;
        if (label === "turn.control.failed" && outcome?.status === "failed" && outcome.action === "steer") {
          const source = outcome.payload.source_movement_seq;
          const previous = pendingInputs.length;
          pendingInputs = pendingInputs.filter((input) => !(input.clientId === outcome.controlId
            && Number.isSafeInteger(source) && source >= input.seq && source < row.seq));
          if (pendingInputs.length !== previous) {
            open = pendingInputs.length > 0 || activeNativeTurn !== null;
            state = open ? "working" : "failed";
            continue;
          }
        }
        if (operatorTurnControlEndsTurn(outcome)) {
          settle(outcome.payload, row, "idle");
          continue;
        }
        state = "working";
        continue;
      }
      if (!open) continue;
      if (providerMovement && label === "turn.completed") {
        const payload = await readPayload(row);
        settle(payload, row, stringValue(payload?.status ?? payload?.turn?.status) === "failed" ? "failed" : "idle");
        continue;
      }
      if (providerMovement && label === protocol.terminalOutputLabel) {
        settle(await readPayload(row), row, "idle");
        continue;
      }
      if (providerMovement && label === "turn.interrupted.result") {
        settle(await readPayload(row), row, "idle");
        continue;
      }
      if (providerMovement && label === protocol.errorOutputLabel) {
        const payload = await readPayload(row);
        if (payload?.will_retry === true) {
          state = "working";
          continue;
        }
        settle(payload, row, "failed");
        continue;
      }
      if (isOperatorChatProviderFailureRow(entry, row)) {
        const payload = await readPayload(row);
        operatorChatProviderFailureActivity(entry, row, payload);
        settle(payload, row, "failed");
        continue;
      }
      state = "working";
    }
    return { open, state, lastMovementSeq, updatedAtMs, openedMovementSeq, executionObserved, activeNativeTurn, pendingInputs, nativeTurns, recordedInputs };
  }

  async function operatorChatTurnState(projection, entry, protocol) {
    const state = await cachedOperatorChatTurnState({ projection, entry, protocol,
      reduce: (initial, rows, readPayload) => reduceOperatorChatTurnState(initial, rows, entry, protocol, readPayload),
      resolve: (row, onVerifiedFiles) => store.resolveLedgerArtifactPayload(row, { projection: protocol.artifactReadProjection, onVerifiedFiles }),
    });
    projection.operatorChatTurnStateCache.set(entry.runId, state);
    return state;
  }

  async function operatorChatStatuses(projection, params) {
    if (!Array.isArray(params.run_ids)) {
      throw new Error("run_ids must be an array of exact projected task run ids");
    }
    const runIds = [...new Set(params.run_ids.map((value) => requiredString(value, "run_ids[]")))];
    if (runIds.length > 5_000) throw new Error("task status scope exceeds 5000 projected tasks");
    const refresh = projection.operatorChatStatusRefreshTail.then(() => (
      refreshOperatorChatTurnStates(projection)
    ));
    projection.operatorChatStatusRefreshTail = refresh.then(() => undefined, () => undefined);
    await refresh;
    const scopeRuns = new Map(arrayValue(store.operatorOsScope(projection).client_runs).map((row) => [stringValue(row.run_id), row]));
    return {
      chats: runIds.flatMap((runId) => {
        if (!scopeRuns.has(runId)) return [];
        const state = projection.operatorChatTurnStateCache.get(runId);
        return state ? [{
          run_id: runId,
          archived: scopeRuns.get(runId).archived === true,
          // Archival suspends the frontier, not the historical ledger state.
          turn_open: scopeRuns.get(runId).archived === true ? false : state.open,
          turn_state: scopeRuns.get(runId).archived === true ? "idle" : state.state,
          turn_opened_movement_seq: state.openedMovementSeq,
          updated_at_ms: state.updatedAtMs,
          last_movement_seq: Math.max(0, state.lastMovementSeq),
        }] : [];
      }),
      ledger_cursor_seq: projection.operatorChatStatusCursor,
    };
  }

  async function refreshOperatorChatTurnStates(projection) {
    if (!projection.operatorChatStatusInitialized) await store.operatorChatsList(projection);
    const scope = store.operatorOsScope(projection);
    const recordsByRun = new Map(arrayValue(projection.runRecords).map((record) => [
      stringValue(record.run_id), record,
    ]));
    const entries = new Map(arrayValue(scope.client_runs).flatMap((row) => {
      const runId = stringValue(row.run_id);
      const chatInterfaceId = stringValue(row.interface_id);
      return runId && chatInterfaceId
        ? [[runId, { runId, chatInterfaceId, record: recordsByRun.get(runId) ?? null,
          externalInputs: arrayValue(row.authoring_profile?.external_inputs) }]] : [];
    }));
    if (entries.size === 0) return;
    const delta = await store.operatorChatLedgerDelta(
      projection,
      new Set(entries.keys()),
      projection.operatorChatStatusCursor,
    );
    const protocol = objectOrEmpty(projection.operatorChatProtocol);
    for (const [runId, rows] of delta.rowsByRun) {
      if (rows.length === 0) continue;
      const entry = entries.get(runId);
      if (!entry) continue;
      const current = projection.operatorChatTurnStateCache.get(runId)
        ?? { open: false, state: "idle", lastMovementSeq: -1, updatedAtMs: 0 };
      const next = await reduceOperatorChatTurnState(
        current,
        rows.filter((row) => numberValue(row.seq, -1) > current.lastMovementSeq),
        entry,
        protocol,
      );
      projection.operatorChatTurnStateCache.set(runId, next);
    }
    projection.operatorChatStatusCursor = Math.max(
      projection.operatorChatStatusCursor,
      delta.nextMovementSeq,
    );
  }

  return { operatorChatStatuses, operatorChatTurnOpening, operatorChatTurnState, reduceOperatorChatTurnState, refreshOperatorChatTurnStates };
}
