module.exports = { operatorChatListProjection };

const path = require("node:path");
const { projectHomeVoiceContext } = require("../homeVoiceProjection.cjs");
const { operatorActivityRows } = require("../operatorActivity.cjs");
const { activationDriverRef, scheduledInterfaceDisplayName } = require("./cronActivation.cjs");
const { cachedOperatorChatArtifactText } = require("./operatorChatCache.cjs");
const { ARTIFACT_PAYLOAD_READ_CONCURRENCY } = require("./constants.cjs");
const { readConnectedApplicationIdsByRun } = require("./applicationPackages.cjs");
const { compareMovementOrder, movementBoundaryInterface, openValidatedLedgerIndex, optionalUnsignedInteger, readIndexedTaskSearchCandidates, taskSearchSnippet } = require("./movementRows.cjs");
const { operatorActivityAgent, operatorActivityMessage, operatorChatExpandedStreamRows, operatorChatResponseKey, operatorChatRunProvider, operatorChatSearchMessage, operatorChatTurnText } = require("./operatorChatEvents.cjs");
const { eeInstanceRows, octopusOsInterfaceSidecars } = require("./registryRows.cjs");
const { arrayValue, contentHashHex, ledgerArtifactId, ledgerArtifactLabel, mapConcurrentOrdered, numberValue, objectOrEmpty, requiredString, rowTimeMs, stringValue } = require("./values.cjs");

function operatorChatListProjection(store) {
  async function operatorChatsList(projection) {
    const cached = projection.operatorChatsListCache;
    if (cached?.generation === store.projectionGeneration) return cached.promise;
    const promise = buildOperatorChatsList(projection);
    projection.operatorChatsListCache = { generation: store.projectionGeneration, promise };
    promise.catch(() => {
      if (projection.operatorChatsListCache?.promise === promise) {
        projection.operatorChatsListCache = null;
      }
    });
    return promise;
  }

  async function operatorChatTitle(projection, entry, sidecars, protocol, includeInputTitle = true) {
    const authoredTitle = entry.surfaceRole === "task"
      ? stringValue(sidecars.get(entry.chatInterfaceId)?.presentation?.name)
      : "";
    if (authoredTitle || !includeInputTitle) return authoredTitle || entry.runId;
    const firstInbound = entry.inbound
      .filter((row) => !entry.writable || ledgerArtifactLabel(row) === protocol.inputLabel)
      .sort((a, b) => rowTimeMs(a) - rowTimeMs(b))[0];
    if (firstInbound) {
      const text = await cachedOperatorChatArtifactText({ home: projection.octopusHome, entry, row: firstInbound, protocol,
        direction: "in", limit: 48, warnings: projection.warnings,
        resolve: (row, onVerifiedFiles) => store.resolveLedgerArtifactPayload(row, { projection: protocol.artifactReadProjection, onVerifiedFiles }) });
      if (text) return text;
    }
    if (entry.surfaceRole === "task") {
      for (const movement of entry.activity) {
        if (movement.kind !== "InterfaceArrival" || !store.artifactLabelMatches(ledgerArtifactLabel(movement), "cron.current_time", true)) continue;
        const interfaceId = movementBoundaryInterface(movement, "from");
        const descriptor = projection.descriptorIndexes?.interface?.get(interfaceId);
        const activation = arrayValue(arrayValue(descriptor?.activation_sidecars)[0]?.activations)
          .find((row) => activationDriverRef(row) === "cron_life");
        if (activation) return scheduledInterfaceDisplayName(interfaceId);
      }
    }
    return entry.runId;
  }

  async function operatorChatControlTargets(projection, includeTitles) {
    const scope = store.operatorOsScope(projection);
    if (scope.status !== "ready") throw new Error("Control target lookup requires the current valid client scope");
    const scoped = new Map(scope.client_runs.map(row => [row.run_id, row]));
    const sidecars = octopusOsInterfaceSidecars(projection);
    const protocol = objectOrEmpty(projection.operatorChatProtocol);
    const rows = await Promise.all((await store.operatorChatRunIndex(projection)).filter(entry => scoped.has(entry.runId)).map(async entry => {
      const owner = scoped.get(entry.runId);
      if (owner.interface_id !== entry.chatInterfaceId) throw new Error("Control target differs from its scoped client interface");
      const title = await operatorChatTitle(projection, entry, sidecars, protocol, includeTitles);
      const movements = entry.activity.length ? entry.activity : [...entry.inbound, ...entry.outbound];
      return { run_id: entry.runId, chat_interface_id: entry.chatInterfaceId, title,
        updated_at_ms: movements.reduce((last, row) => Math.max(last, rowTimeMs(row)), 0),
        archived: owner.archived, writable: entry.writable && !owner.archived, client_surface_role: owner.surface_role };
    }));
    return rows.sort((left, right) => right.updated_at_ms - left.updated_at_ms);
  }

  async function buildOperatorChatsList(projection) {
    const voiceRunsById = new Map(arrayValue(projection.runRecords).map((run) => [run.run_id, run]));
    const clientSidecars = octopusOsInterfaceSidecars(projection);
    const protocol = objectOrEmpty(projection.operatorChatProtocol);
    const projectColors = new Map(arrayValue(store.operatorOsScope(projection).project_presentations)
      .map((presentation) => [stringValue(presentation.ee_id), stringValue(presentation.color)]));
    const connectedApplicationsByRun = await readConnectedApplicationIdsByRun(projection, store.operatorOsScope(projection));
    const eeById = new Map(eeInstanceRows(projection, false).map((row) => [stringValue(row.ee_id), row]));
    const chats = await mapConcurrentOrdered(await store.operatorChatRunIndex(projection), ARTIFACT_PAYLOAD_READ_CONCURRENCY, async (entry) => {
      const movements = entry.activity.length ? entry.activity : [...entry.inbound, ...entry.outbound];
      const times = movements.map(rowTimeMs).filter(Boolean);
      const lastMovementSeq = movements.reduce(
        (latest, row) => Math.max(latest, numberValue(row.seq, 0)),
        0,
      );
      const title = await operatorChatTitle(projection, entry, clientSidecars, protocol);
      const provider = operatorChatRunProvider(projection, entry);
      const record = entry.record;
      const eeId = stringValue(record?.ee_id);
      const ee = eeById.get(eeId) ?? {};
      const workerInterfaceId = stringValue(record?.worker);
      const chatInterfaceId = entry.chatInterfaceId;
      const chatState = objectOrEmpty(record?.interface_state?.[chatInterfaceId]);
      const archived = arrayValue(record?.archived_client_interface_ids).includes(chatInterfaceId);
      const turn = entry.writable
        ? await store.operatorChatTurnState(projection, entry, protocol)
        : { open: false, state: "idle" };
      const lastTerminalMessage = await operatorChatLastTerminalMessage(entry, protocol, projection.warnings);
      const activityAgent = operatorActivityAgent(projection, eeId, entry.workerChoiceInterfaceId || workerInterfaceId);
      const titleInput = entry.surfaceRole === "task" && entry.writable && !archived
        && !stringValue(clientSidecars.get(chatInterfaceId)?.presentation?.name)
        ? entry.inbound.filter((row) => ledgerArtifactLabel(row) === protocol.inputLabel)
          .reduce((first, row) => !first || row.seq < first.seq ? row : first, null) : null;
      return {
        run_id: entry.runId,
        title,
        title_generation_needed: Boolean(titleInput),
        title_source: titleInput ? { artifact_id: ledgerArtifactId(titleInput),
          content_hash: contentHashHex(titleInput.artifact?.content), movement_seq: titleInput.seq } : null,
        chat_interface_id: chatInterfaceId,
        input_interface_ids: [...new Set(arrayValue(record?.checkpoints)
          .filter((checkpoint) => checkpoint.status === "projected")
          .map((checkpoint) => stringValue(checkpoint.interface)).filter(Boolean))],
        client_surface_role: entry.surfaceRole,
        writable: entry.writable && !archived,
        archived,
        os_root_ee_id: entry.osRootEeId,
        voice_context: projectHomeVoiceContext(record, octopusOsInterfaceSidecars(projection).get(chatInterfaceId), chatState, voiceRunsById),
        turn_open: turn.open,
        turn_state: turn.state,
        ee_id: eeId,
        ee_goal: stringValue(ee?.goal),
        ee_template_name: stringValue(ee?.template_name),
        worker_interface_id: workerInterfaceId,
        worker_choice_interface_id: entry.workerChoiceInterfaceId || workerInterfaceId,
        door_host_slots: entry.doorHostSlots,
        provider_interface_id: provider.id,
        provider_bound_interface_id: provider.boundId,
        provider_model: provider.model,
        provider_models_refreshable: provider.modelCatalogRefreshable,
        reasoning_effort: provider.reasoningEffort,
        service_tier: provider.serviceTier,
        created_at_ms: times.length ? Math.min(...times) : 0,
        updated_at_ms: times.length ? Math.max(...times) : 0,
        last_movement_seq: lastMovementSeq,
        message_count: entry.writable
          ? entry.inbound.filter((row) => ledgerArtifactLabel(row) === protocol.inputLabel).length
            + entry.outbound.filter((row) => {
          const label = ledgerArtifactLabel(row);
          return label === protocol.terminalOutputLabel || label === protocol.errorOutputLabel;
            }).length
          : entry.inbound.length + entry.outbound.length,
        pinned_artifact_ids: arrayValue(chatState.pinned_artifact_bindings)
          .map((binding) => stringValue(binding?.id))
          .filter(Boolean),
        pinned_context_items: arrayValue(chatState.pinned_context_items),
        continued_from: objectOrEmpty(chatState.continued_from),
        record_digest: stringValue(record?.record_digest),
        connected_application_ids: (connectedApplicationsByRun.get(entry.runId) ?? []).sort(),
        last_terminal_message: lastTerminalMessage,
        agent_id: activityAgent?.id ?? null,
        agent_name: activityAgent?.name ?? null,
        agent_icon: activityAgent?.icon ?? null,
        agent_color: activityAgent ? projectColors.get(eeId) || null : null,
      };
    });
    projection.operatorChatStatusInitialized = true;
    return chats.sort((a, b) => b.updated_at_ms - a.updated_at_ms);
  }

  async function operatorChatLastTerminalMessage(entry, protocol, warnings) {
    const rows = entry.outbound
      .filter((candidate) => (
        ledgerArtifactLabel(candidate) === protocol.terminalOutputLabel
        && movementBoundaryInterface(candidate, "to") === entry.chatInterfaceId
      ))
      .sort(compareMovementOrder);
    const row = rows.at(-1);
    const artifactId = ledgerArtifactId(row);
    const movementSeq = numberValue(row?.seq, 0);
    if (!row || !artifactId || !movementSeq) return {};
    const text = await cachedOperatorChatArtifactText({ home: store.octopusHome(), entry, row, protocol,
      direction: "out", limit: 240, warnings,
      resolve: (row, onVerifiedFiles) => store.resolveLedgerArtifactPayload(row, { projection: protocol.artifactReadProjection, onVerifiedFiles }) });
    return {
      artifact_id: artifactId,
      created_at_ms: rowTimeMs(row),
      interface_id: entry.chatInterfaceId,
      movement_seq: movementSeq,
      text,
    };
  }

  async function operatorActivity(projection, chats) {
    const key = JSON.stringify(chats.map((chat) => chat.run_id));
    const cached = projection.operatorActivityCache;
    if (cached?.generation === store.projectionGeneration && cached.key === key) return cached.promise;
    const promise = buildOperatorActivity(projection, chats);
    projection.operatorActivityCache = { generation: store.projectionGeneration, key, promise };
    promise.catch(() => {
      if (projection.operatorActivityCache?.promise === promise) projection.operatorActivityCache = null;
    });
    return promise;
  }

  async function buildOperatorActivity(projection, chats) {
    if (chats.length > 1000) throw new Error("Activity exceeds the 1000-chat read boundary");
    const protocol = objectOrEmpty(projection.operatorChatProtocol);
    const entries = new Map((await store.operatorChatRunIndex(projection)).map((entry) => [entry.runId, entry]));
    const items = [];
    const outputLabels = new Set([protocol.terminalOutputLabel, "operator.approval.request", "operator.elicitation.request", "operator.user_input.request", "turn.operator_request.resolved"]);
    const responseLabels = new Set([protocol.approvalResponseLabel, protocol.elicitationResponseLabel, protocol.userInputResponseLabel]);
    for (const chat of chats) {
      const entry = entries.get(chat.run_id);
      if (!entry) throw new Error(`Activity chat has no projected run: ${chat.run_id}`);
      const outbound = entry.outbound.filter((row) => row.kind === "InterfaceOutbound" && movementBoundaryInterface(row, "to") === entry.chatInterfaceId);
      const direct = outbound.filter((row) => outputLabels.has(ledgerArtifactLabel(row)));
      const inbound = entry.inbound.filter((row) => row.kind === "InterfaceArrival" && movementBoundaryInterface(row, "from") === entry.chatInterfaceId && responseLabels.has(ledgerArtifactLabel(row)));
      if (direct.length + inbound.length > 50_000) throw new Error(`Activity exceeds the 50000-receipt read boundary: ${chat.run_id}`);
      const payloads = await store.resolveLedgerArtifactPayloads([...direct, ...inbound], true, { projection: protocol.artifactReadProjection });
      const deliveries = direct.filter((row) => ledgerArtifactLabel(row) === protocol.terminalOutputLabel).map((row) => ({
          artifact_id: ledgerArtifactId(row), seq: row.seq,
          movement_id: `${path.basename(stringValue(row.source_file) || "ledger", ".jsonl")}:${numberValue(row.source_line, 0)}`,
        }));
      const responses = [];
      for (const row of inbound) {
        const label = ledgerArtifactLabel(row);
        const key = operatorChatResponseKey(label, payloads.get(row), protocol);
        if (!key) throw new Error(`Activity response has no request identity: ${ledgerArtifactId(row)}`);
        responses.push({ key, seq: row.seq });
      }
      const messages = direct.map((row) => operatorActivityMessage(row, payloads.get(row), protocol));
      const input = { chat, messages, deliveries, responses, agent: operatorActivityAgent(projection, chat.ee_id, chat.worker_choice_interface_id) };
      let rows = operatorActivityRows(input);
      const pending = rows.filter((row) => row.kind !== "message");
      if (pending.length) {
        const after = Math.min(...pending.map((row) => row.seq));
        const streams = outbound.filter((row) => ledgerArtifactLabel(row) === "turn.stream" && row.seq > after);
        if (streams.length > 1000) throw new Error(`Activity exceeds the 1000-stream resolution read boundary: ${chat.run_id}`);
        const streamPayloads = await store.resolveLedgerArtifactPayloads(streams, true);
        for (const row of await operatorChatExpandedStreamRows(streams, streamPayloads, entry.runId)) {
          if (ledgerArtifactLabel(row) === "turn.operator_request.resolved") messages.push(operatorActivityMessage(row, streamPayloads.get(row), protocol));
        }
        rows = operatorActivityRows(input);
      }
      items.push(...rows);
    }
    if (items.length > 10_000) throw new Error("Activity exceeds the 10000-item read boundary");
    return items.sort((a, b) => (b.created_at_ms ?? 0) - (a.created_at_ms ?? 0) || b.seq - a.seq || a.id.localeCompare(b.id));
  }

  async function operatorChatSearch(projection, params) {
    const query = requiredString(params.query, "query").replace(/\s+/g, " ").trim();
    if (query.length < 2) throw new Error("task transcript search query must contain at least two characters");
    if (query.length > 512) throw new Error("task transcript search query must not exceed 512 characters");
    if (!Array.isArray(params.task_run_ids)) throw new Error("task_run_ids must be an array of exact projected task run ids");
    const requestedTaskRunIds = [...new Set(params.task_run_ids.map((value) => requiredString(value, "task_run_ids[]")))];
    if (requestedTaskRunIds.length === 0) throw new Error("task transcript search requires at least one projected task run id");
    if (requestedTaskRunIds.length > 5_000) throw new Error("task transcript search scope exceeds 5000 projected tasks");
    const parsedLimit = params.limit === undefined ? 12 : optionalUnsignedInteger(params.limit);
    const limit = Math.min(50, Math.max(1, parsedLimit ?? 12));
    const requestedBeforeSeq = optionalUnsignedInteger(params.before_seq);
    const searchScope = operatorChatSearchScope(projection, requestedTaskRunIds);
    const index = await openValidatedLedgerIndex(projection);
    if (!index) throw new Error("ledger index is required for task transcript search");
    try {
      if (index.durableEndOffset !== index.ledgerSize) {
        throw new Error("ledger index must be current before task transcript search can run");
      }
      if (requestedBeforeSeq !== null && requestedBeforeSeq > index.watermark) {
        throw new Error("task transcript search cursor is beyond the ledger index watermark");
      }
      const needle = query.toLocaleLowerCase();
      const matches = [];
      let cursor = requestedBeforeSeq ?? index.watermark;
      let exhausted = cursor === 0;
      while (matches.length < limit && !exhausted) {
        const candidates = readIndexedTaskSearchCandidates(
          index,
          searchScope.targetsBySourceRun,
          cursor,
          Math.max(16, limit - matches.length),
          projection.operatorChatProtocol,
        );
        if (candidates.length === 0) {
          exhausted = true;
          break;
        }
        const resolved = await store.resolveLedgerArtifactPayloadResults(candidates.map((candidate) => candidate.row), { projection: projection.operatorChatProtocol?.artifactReadProjection });
        let stoppedAtLimit = false;
        for (const candidate of candidates) {
          cursor = numberValue(candidate.row.seq, cursor);
          const payloadResult = resolved.get(candidate.row);
          if (!payloadResult || payloadResult.error || payloadResult.payload === null) {
            throw new Error(`task transcript search could not resolve movement ${cursor}: ${payloadResult?.error || "artifact payload is unavailable"}`);
          }
          const message = operatorChatSearchMessage(
            candidate.row,
            candidate.target,
            objectOrEmpty(projection.operatorChatProtocol),
            payloadResult?.payload ?? null,
          );
          if (!message) continue;
          const normalizedText = message.text.replace(/\s+/g, " ").trim().toLocaleLowerCase();
          const matchIndex = normalizedText.indexOf(needle);
          if (matchIndex < 0) continue;
          matches.push({
            task_run_id: candidate.target.taskRunId,
            source_run_id: candidate.target.sourceRunId,
            snippet: taskSearchSnippet(message.text, matchIndex, needle.length),
            message,
          });
          if (matches.length >= limit) {
            stoppedAtLimit = true;
            break;
          }
        }
        if (stoppedAtLimit) break;
        if (candidates.length < Math.max(16, limit - matches.length)) exhausted = true;
      }
      const hasOlder = !exhausted && cursor > 0 && readIndexedTaskSearchCandidates(
        index,
        searchScope.targetsBySourceRun,
        cursor,
        1,
        projection.operatorChatProtocol,
      ).length > 0;
      return {
        matches,
        has_older: hasOlder,
        next_before_seq: hasOlder ? cursor : -1,
        index_watermark: index.watermark,
        scope: {
          kind: "octopus_os_task_transcripts",
          task_count: searchScope.taskCount,
          run_count: searchScope.targetsBySourceRun.size,
          message_classes: ["operator", "terminal_agent", "terminal_error"],
        },
      };
    } finally {
      index.database.close();
    }
  }

  function operatorChatSearchScope(projection, requestedTaskRunIds) {
    const scope = store.operatorOsScope(projection);
    const recordsByRun = new Map(arrayValue(projection.runRecords).map((record) => [
      stringValue(record.run_id),
      record,
    ]).filter(([runId]) => runId));
    const entriesByRunId = new Map([
      ...arrayValue(scope.client_runs),
      ...arrayValue(scope.read_only_runs),
    ].flatMap((row) => {
      const runId = stringValue(row.run_id);
      const record = recordsByRun.get(runId);
      const interfaceId = stringValue(row.interface_id);
      return runId && record && interfaceId
        ? [[runId, { runId, record, interfaceId }]]
        : [];
    }));
    const targetsBySourceRun = new Map();
    for (const taskRunId of requestedTaskRunIds) {
      const entry = entriesByRunId.get(taskRunId);
      if (!entry) throw new Error(`task transcript search scope contains an unavailable run: ${taskRunId}`);
      targetsBySourceRun.set(entry.runId, {
        taskRunId,
        sourceRunId: entry.runId,
        chatInterfaceId: entry.interfaceId,
      });
    }
    return { taskCount: requestedTaskRunIds.length, targetsBySourceRun };
  }

  return { buildOperatorActivity, buildOperatorChatsList, operatorActivity, operatorChatControlTargets, operatorChatLastTerminalMessage, operatorChatSearch, operatorChatSearchScope, operatorChatTitle, operatorChatsList };
}
