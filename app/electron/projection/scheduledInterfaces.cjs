module.exports = { scheduledInterfacesProjection };

const crypto = require("node:crypto");
const fs = require("node:fs/promises");
const path = require("node:path");
const { parseScheduledDueAt } = require("../scheduledTime.cjs");
const { activationDriverRef, cronScheduleFingerprint, parseCronActivationDocument, scheduledInterfaceDisplayName } = require("./cronActivation.cjs");
const { readJsonFile, readOptionalRegularJsonDocument, stableJson } = require("./jsonFiles.cjs");
const { movementBoundaryInterface, movementRunId, readChangeJournal } = require("./movementRows.cjs");
const { baseInterfaceRows, octopusOsInterfaceSidecars } = require("./registryRows.cjs");
const { runInterfaceReferences } = require("./taskDeletion.cjs");
const { arrayValue, interfaceBindingId, isAdmittedInterfaceArtifactMovement, ledgerArtifactId, ledgerArtifactLabel, numberValue, objectOrEmpty, rowTimeMs, stringValue } = require("./values.cjs");

function scheduledInterfacesProjection(store) {
  function scheduledScopeOptions(projection) {
    const scope = store.operatorOsScope(projection);
    const names = new Map(arrayValue(scope.project_presentations).map((row) => [stringValue(row?.ee_id), stringValue(row?.name)]));
    return arrayValue(scope.projects).map((row) => ({
      ee_id: stringValue(row.ee_id),
      name: names.get(stringValue(row.ee_id)) || stringValue(row.goal) || stringValue(row.ee_id),
    })).filter((row) => row.ee_id);
  }

  async function scheduledInterfaceRows(projection) {
    const rows = [];
    const boundInterfaces = new Set(arrayValue(projection.runRecords).flatMap((record) => (
      [...runInterfaceReferences(record)]
    )).filter(Boolean));
    const interfaces = baseInterfaceRows(projection);
    const descriptors = interfaces.filter((descriptor) => (
      arrayValue(arrayValue(descriptor.activation_sidecars)[0]?.activations)
        .some((activation) => activationDriverRef(activation) === "cron_life")
    ));
    const indexedEvidence = descriptors.length > 0
      ? await store.indexedArtifactLabelRows(projection, "cron.current_time", { namespace: true })
      : [];
    if (indexedEvidence === null) await store.refreshLedgerProjection(projection);
    const evidenceRows = (indexedEvidence ?? projection.ledgerRows).filter((movement) => (
      store.artifactLabelMatches(ledgerArtifactLabel(movement), "cron.current_time", true)
      && isAdmittedInterfaceArtifactMovement(movement)
    ));
    for (const descriptor of descriptors) {
      const interfaceId = stringValue(descriptor.rule_id ?? descriptor.id);
      const sidecar = arrayValue(descriptor.activation_sidecars)[0];
      const activationPath = stringValue(sidecar?.descriptor_path);
      let activationDigest = "";
      try {
        activationDigest = crypto.createHash("sha256").update(await fs.readFile(activationPath)).digest("hex");
        const parsed = parseCronActivationDocument({
          schema_version: sidecar.schema_version,
          interface: sidecar.interface,
          activations: sidecar.activations,
        }, interfaceId);
        const schedule = parsed.config.schedule;
        const runTemplate = parsed.config.run_template;
        const trigger = runTemplate.checkpoints.find((checkpoint) => interfaceBindingId(checkpoint.interface_binding) === interfaceId);
        const cloneEligible = parsed.config.target.mode === "new_run"
          || arrayValue(trigger?.outbound_interface_bindings).some((value) => interfaceBindingId(value));
        const template = !boundInterfaces.has(interfaceId);
        if (template) {
          if (parsed.activation.enabled !== false) throw new Error(`scheduled interface '${interfaceId}' is not bound to a run`);
          if (!cloneEligible) continue;
        }
        const evidence = template
          ? { lastRunAtMs: null, lastMovementSeq: null, nextRunAtMs: null, nextRunSource: "", inactivityState: null, deliveries: [] }
          : await scheduledInterfaceEvidence(projection, interfaceId, parsed.config, evidenceRows, descriptor);
        rows.push({
          interface_id: interfaceId,
          display_name: scheduledInterfaceDisplayName(interfaceId, runTemplate.id),
          template,
          status: template ? "template" : parsed.activation.enabled === false ? "disabled" : "enabled",
          valid: true,
          activation_digest: activationDigest,
          activation_path: activationPath,
          descriptor_path: stringValue(descriptor.descriptor_path),
          schedule: schedule.inactivity != null
            ? { type: "inactivity", ...schedule.inactivity }
            : schedule.due_at != null
            ? { type: "once", due_at: schedule.due_at }
            : schedule.cron == null
              ? { type: "interval", interval_seconds: schedule.interval_seconds }
              : { type: "cron", cron: schedule.cron },
          timezone: schedule.timezone,
          fire_on_start: schedule.fire_on_start,
          instructions: schedule.reminder_default,
          reminder_env: schedule.reminder_env,
          target: parsed.config.target,
          run_template: {
            run_id: runTemplate.id,
            ee_id: runTemplate.ee,
            worker_interface_id: runTemplate.worker,
            checkpoint_count: runTemplate.checkpoints.length,
            outbound_interface_ids: arrayValue(trigger?.outbound_interface_bindings).map(interfaceBindingId).filter(Boolean),
            artifact_binding_count: arrayValue(trigger?.artifact_bindings).length,
          },
          clone_eligible: cloneEligible,
          last_run_at_ms: evidence.lastRunAtMs,
          last_movement_seq: evidence.lastMovementSeq,
          next_run_at_ms: parsed.activation.enabled === false ? null : evidence.nextRunAtMs,
          next_run_source: parsed.activation.enabled === false ? "" : evidence.nextRunSource,
          inactivity_state: evidence.inactivityState,
          deliveries: evidence.deliveries,
        });
      } catch (error) {
        rows.push({
          interface_id: interfaceId,
          display_name: scheduledInterfaceDisplayName(interfaceId, ""),
          status: "invalid",
          valid: false,
          activation_digest: activationDigest,
          activation_path: activationPath,
          descriptor_path: stringValue(descriptor.descriptor_path),
          error: String(error?.message ?? error),
        });
      }
    }
    const scheduledIds = new Set(rows.map((row) => row.interface_id));
    const taskInputLabels = (descriptor) => arrayValue(descriptor.interface_driver_config?.private_config?.task_input_labels)
      .filter((label) => typeof label === "string" && label);
    const eventSources = interfaces.filter((descriptor) => descriptor.interface_type === "world"
      && (descriptor.role === "sensor" || taskInputLabels(descriptor).length > 0)
      && ["inbound", "bidirectional"].includes(descriptor.direction)
      && !scheduledIds.has(stringValue(descriptor.rule_id ?? descriptor.id)));
    for (const descriptor of eventSources) {
      const interfaceId = stringValue(descriptor.rule_id ?? descriptor.id);
      const labels = new Set(taskInputLabels(descriptor));
      const targets = arrayValue(projection.runRecords).filter((record) => arrayValue(record.checkpoints)
        .some((checkpoint) => checkpoint.status === "projected" && checkpoint.interface === interfaceId));
      if (!targets.length) continue;
      const indexed = await store.readIndexedInterfaceArtifactRows(projection, "", interfaceId, labels, -1);
      if (indexed === null) await store.refreshLedgerProjection(projection);
      const evidence = await scheduledInterfaceEvidence(projection, interfaceId, { schedule: {} },
        (indexed ?? projection.ledgerRows).filter((movement) => isAdmittedInterfaceArtifactMovement(movement)
          && (labels.size === 0 || labels.has(ledgerArtifactLabel(movement)))));
      const entries = new Map((await store.operatorChatRunIndex(projection)).map((entry) => [entry.runId, entry]));
      const sidecars = octopusOsInterfaceSidecars(projection);
      const eventTargets = [];
      for (const target of targets) {
        const entry = entries.get(target.run_id);
        eventTargets.push({ run_id: target.run_id, chat_interface_id: entry?.chatInterfaceId ?? "",
          title: entry ? await store.operatorChatTitle(projection, entry, sidecars, objectOrEmpty(projection.operatorChatProtocol)) : target.run_id });
      }
      rows.push({
        interface_id: interfaceId, display_name: scheduledInterfaceDisplayName(interfaceId, ""),
        status: "enabled", valid: true, template: false, clone_eligible: false,
        descriptor_path: stringValue(descriptor.descriptor_path),
        event_source: { sources: arrayValue(descriptor.source_names), targets: eventTargets },
        last_run_at_ms: evidence.lastRunAtMs, last_movement_seq: evidence.lastMovementSeq,
        deliveries: evidence.deliveries,
      });
    }
    return rows.sort((left, right) => (
      numberValue(left.next_run_at_ms, Number.MAX_SAFE_INTEGER) - numberValue(right.next_run_at_ms, Number.MAX_SAFE_INTEGER)
      || stringValue(left.display_name).localeCompare(stringValue(right.display_name))
    ));
  }

  async function scheduledInterfaceEvidence(projection, interfaceId, config, evidenceRows, descriptor) {
    let lastRunAtMs = 0;
    let lastMovementSeq = 0;
    const firings = new Map();
    const deadline = config.schedule.due_at == null ? null : parseScheduledDueAt(config.schedule.due_at);
    for (const movement of evidenceRows) {
      if (
        movementBoundaryInterface(movement, "from") !== interfaceId
      ) continue;
      if (deadline !== null) {
        const payload = objectOrEmpty(await store.resolveLedgerArtifactPayload(movement));
        if (payload.schedule_kind !== "one_shot" || typeof payload.due_at !== "string" || parseScheduledDueAt(payload.due_at) !== deadline) continue;
      }
      const at = rowTimeMs(movement);
      const seq = numberValue(movement.seq, 0);
      if (at > lastRunAtMs || at === lastRunAtMs && seq > lastMovementSeq) {
        lastRunAtMs = at;
        lastMovementSeq = seq;
      }
      const artifactId = ledgerArtifactId(movement);
      const runId = movementRunId(movement);
      const targets = firings.get(artifactId) ?? new Map();
      if (!targets.has(runId) || seq < targets.get(runId).seq) {
        targets.set(runId, { seq, at_ms: at, run_id: runId });
      }
      firings.set(artifactId, targets);
    }
    const schedule = config.schedule;
    let nextRunAtMs = null;
    let nextRunSource = "";
    let inactivityState = null;
    if (schedule.inactivity != null) {
      const document = await readOptionalRegularJsonDocument(path.join(projection.octopusHome, "interfaces", interfaceId, "cron-inactivity-state.json"), "inactivity trigger state");
      const state = document?.document;
      if (state && (state.schema_version !== 1 || !["idle", "waiting", "prepared", "admitted"].includes(state.status))) throw new Error("inactivity trigger state has an invalid schema or status");
      if (state && ["waiting", "prepared"].includes(state.status) && (!Number.isSafeInteger(state.next_due_unix_ms) || state.next_due_unix_ms < 0)) throw new Error("inactivity trigger state has an invalid deadline");
      const current = state?.schema_version === 1 && stableJson(state.inactivity ?? null) === stableJson(schedule.inactivity);
      if (current && ["waiting", "prepared"].includes(state.status)
        && Number.isSafeInteger(state.next_due_unix_ms) && state.next_due_unix_ms >= 0) {
        nextRunAtMs = state.next_due_unix_ms;
        nextRunSource = "driver_state";
      }
      inactivityState = {
        status: stringValue(state?.status) || "unstarted",
        current: Boolean(current) || ["idle", "admitted"].includes(stringValue(state?.status)),
        last_activity_at_ms: Number.isSafeInteger(state?.activity?.recorded_at_unix_ms) ? state.activity.recorded_at_unix_ms : null,
        last_activity_run_id: stringValue(state?.activity?.run),
      };
    }
    if (deadline !== null && !lastRunAtMs) {
      nextRunAtMs = Number(deadline / 1_000_000n);
      nextRunSource = "schedule";
    }
    if (schedule.interval_seconds != null) {
      const statePath = path.join(projection.octopusHome, "interfaces", interfaceId, "cron-interval-state.json");
      const state = await readJsonFile(statePath);
      let fingerprint = null;
      if (state) {
        const privateRoot = config.target.mode === "new_run" ? path.dirname(await fs.realpath(path.dirname(statePath))) : null;
        if (privateRoot && config.target.interface_private_state_root !== undefined
          && await fs.realpath(config.target.interface_private_state_root) !== privateRoot) {
          throw new Error("cron target.interface_private_state_root must name this Home's interface state root");
        }
        fingerprint = cronScheduleFingerprint(interfaceId, config, descriptor, privateRoot);
      }
      if (
        state
        && state.schema_version === 1
        && state.schedule_fingerprint === fingerprint
        && ["waiting", "prepared"].includes(state.status)
        && typeof state.next_due_unix_ns === "number"
        && Number.isFinite(state.next_due_unix_ns)
        && Number.isInteger(state.next_due_unix_ns)
        && state.next_due_unix_ns >= 0
      ) {
        nextRunAtMs = Math.floor(state.next_due_unix_ns / 1_000_000);
        nextRunSource = "driver_state";
      }
    }
    if (!lastRunAtMs && config.schedule.cron != null) {
      const state = await readJsonFile(path.join(projection.octopusHome, "interfaces", interfaceId, "cron-state.json"));
      const parsed = typeof state?.last_cron_minute === "string" ? Date.parse(state.last_cron_minute) : NaN;
      if (Number.isFinite(parsed)) lastRunAtMs = parsed;
    }
    const clientRuns = new Map(arrayValue(store.operatorOsScope(projection).client_runs).map((row) => [stringValue(row.run_id), row]));
    const deliveries = [...firings.values()].flatMap((targets) => {
      const rows = [...targets.values()];
      const tasks = rows.filter((row) => clientRuns.has(row.run_id));
      return tasks.length ? tasks : rows.sort((left, right) => left.seq - right.seq).slice(0, 1);
    });
    const recent = deliveries.sort((left, right) => right.seq - left.seq).slice(0, 8);
    const entries = recent.some((row) => clientRuns.has(row.run_id))
      ? new Map((await store.operatorChatRunIndex(projection)).map((entry) => [entry.runId, entry])) : new Map();
    const sidecars = octopusOsInterfaceSidecars(projection);
    for (const delivery of recent) {
      delivery.chat_interface_id = stringValue(clientRuns.get(delivery.run_id)?.interface_id);
      delivery.journal = delivery.run_id ? await readChangeJournal(projection, delivery.run_id) : null;
      const entry = entries.get(delivery.run_id);
      delivery.title = entry ? await store.operatorChatTitle(projection, entry, sidecars, objectOrEmpty(projection.operatorChatProtocol)) : "";
    }
    return {
      lastRunAtMs: lastRunAtMs || null,
      lastMovementSeq: lastMovementSeq || null,
      nextRunAtMs,
      nextRunSource,
      inactivityState,
      deliveries: recent,
    };
  }

  return { scheduledInterfaceEvidence, scheduledInterfaceRows, scheduledScopeOptions };
}
