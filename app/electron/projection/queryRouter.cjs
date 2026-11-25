module.exports = { queryRouterProjection };

const path = require("node:path");
const { parseProjectionReadRequest } = require("../projectionReadContract.cjs");
const { DESCRIPTOR_REGISTRIES } = require("./constants.cjs");
const { filterByEnvelopeKind, filterInterfaceArtifactTransferTargets, movementPayloadPreview, pendingArtifactRows, projectionRequiresFullLedger } = require("./movementRows.cjs");
const { readProviderInterfaceDescriptors } = require("./providerDescriptors.cjs");
const { bindingRows, descriptorInventoryRows, descriptorKindRows, descriptorRows, eeInstanceRows, eeTemplateDescriptorRows, eeTemplateRows, envelopeRows, filterInterfaces, goalRows, markKindRows, markRows, policyRows, projectionSnapshot, readRunRecords, recordCheckpointRowsFor, requiredVerifiedArtifactOutcomeIndex, runCheckpointRows, runRows, toolRows, verifierRows, workerEnvironmentRows, workerRows } = require("./registryRows.cjs");
const { arrayValue, limitRows, objectOrEmpty, requiredString, stringValue } = require("./values.cjs");

const PACKAGE_INVENTORY_METHODS = new Set([
  "projection.runtime.snapshot",
  "projection.operator.os.local_packages",
  "projection.operator.os.applications",
  "projection.operator.os.drivers",
  "projection.operator.chat.contexts",
]);

function scopedPackageInventoryFilter(method, params) {
  if (method !== "projection.operator.os.applications" && method !== "projection.operator.os.drivers") {
    return null;
  }
  const packageIds = arrayValue(params?.package_ids).map(stringValue).filter(Boolean);
  if (!packageIds.length) return null;
  if (method === "projection.operator.os.drivers") {
    return { packageIds, includeApplications: false, applicationIds: null };
  }
  const applicationIds = arrayValue(params?.application_ids).map(stringValue).filter(Boolean);
  return {
    packageIds,
    includeApplications: true,
    applicationIds: applicationIds.length ? applicationIds : null,
  };
}

function queryRouterProjection(store) {
  async function readProjection(request) {
    await store.beforeRead();
    const { method, params } = parseProjectionReadRequest(request);
    if (method === "projection.operator.providers.list") {
      const home = store.octopusHome();
      store.ensureProjectionWatchers();
      const providers = await readProviderInterfaceDescriptors(path.join(home, "registry"));
      if (home !== store.octopusHome()) throw new Error("Provider catalogue Home changed during read");
      return { providers };
    }
    if (method === "projection.interface_artifacts.list" || method === "projection.client_actions.list"
      || method === "projection.movements.list") {
      const home = store.octopusHome();
      if (method === "projection.client_actions.list" && params.home !== home) {
        throw new Error("Client action read does not match the selected Home.");
      }
      store.ensureProjectionWatchers();
      const ledgerView = {
        durableRoot: store.resolveDurableRoot(),
        warnings: [],
        ledgerRows: [],
        ledgerRevision: "",
        ledgerMemo: new Map(),
      };
      switch (method) {
        case "projection.interface_artifacts.list":
          return { artifacts: await store.interfaceArtifactPayloadRows(ledgerView, params) };
        case "projection.client_actions.list":
          return { home, run_id: params.run_id, interface_id: params.interface_id,
            actions: await store.clientActionRows(ledgerView, params) };
        case "projection.movements.list":
          return store.readMovementPage(ledgerView, params);
      }
    }
    const exactInterface = params.interface_id ?? params.interface;
    if (method === "projection.interfaces.artifact_transfer.targets"
      && typeof exactInterface === "string" && exactInterface) {
      const interfaces = await store.readDescriptorRows(
        path.join(store.octopusHome(), "registry"),
        DESCRIPTOR_REGISTRIES.find((registry) => registry.kind === "interface"),
        [], null, new Set([exactInterface]),
      );
      const runProjection = params.run_id === undefined ? undefined : {
        runRecords: (await readRunRecords(path.join(store.octopusHome(), "state", "runs"))).records,
      };
      return { artifact_transfer_targets: limitRows(
        filterInterfaceArtifactTransferTargets(store.interfaceArtifactTransferTargetRowsFor(interfaces), params, runProjection), params,
      ) };
    }
    const repoRoot = store.resolveRepoRoot();
    const includeLedger = projectionRequiresFullLedger(method, params);
    let projection = await store.cachedFileProjection(
      repoRoot,
      includeLedger,
    );
    if (PACKAGE_INVENTORY_METHODS.has(method)) {
      const scopedFilter = scopedPackageInventoryFilter(method, params);
      projection = await store.withFileProjectionPackages(projection, includeLedger, scopedFilter);
    }
    switch (method) {
      case "projection.runtime.snapshot":
        return projectionSnapshot(projection);
      case "projection.runtime.topology":
        return store.cachedTopologySnapshot(projection, params);
      case "projection.ee_instances.list":
        return { ee_instances: limitRows(eeInstanceRows(projection, params.include_ledger !== false), params) };
      case "projection.runs.list":
        return {
          runs: limitRows(runRows(projection, params.include_ledger !== false), params),
          invalid_run_records: projection.invalidRunRecords,
        };
      case "projection.runs.checkpoints.list": {
        const includeLedger = params.include_ledger !== false;
        const outcomeIndex = includeLedger ? requiredVerifiedArtifactOutcomeIndex(projection) : new Map();
        const recordByRun = new Map(arrayValue(projection.runRecords).map((record) => [record.run_id, record]));
        const runs = limitRows(runRows(projection, includeLedger), params);
        return {
          run_checkpoints: runs.map((run) => {
            const runId = stringValue(run.run_id);
            return {
              run_id: runId,
              checkpoints: recordCheckpointRowsFor(projection, runId, outcomeIndex, recordByRun)
                ?? runCheckpointRows(projection, runId),
            };
          }),
        };
      }
      case "projection.runs.get": {
        const runId = requiredString(params.run_id, "run_id");
        const includeLedger = params.include_ledger !== false;
        const run = runRows(projection, includeLedger).find((row) => stringValue(row.run_id) === runId);
        if (!run) throw new Error(`run is not present in file projection: ${runId}`);
        const workerEnvironments = workerEnvironmentRows(projection)
          .filter((row) => stringValue(row.run_id) === runId);
        return {
          run,
          bindings: bindingRows(projection).filter((row) => stringValue(row.run_id) === runId),
          artifacts: includeLedger
            ? store.artifactRows(projection).filter((row) => stringValue(row.run_id ?? row.meta?.run_id) === runId)
            : [],
          marks: includeLedger
            ? markRows(projection).filter((row) => stringValue(row.run_id ?? row.meta?.run_id) === runId)
            : [],
          checkpoints: recordCheckpointRowsFor(
            projection,
            runId,
            includeLedger ? requiredVerifiedArtifactOutcomeIndex(projection) : new Map(),
          ) ?? (includeLedger ? runCheckpointRows(projection, runId) : []),
          worker_environments: workerEnvironments,
          ...(workerEnvironments.length === 1 ? { worker_environment: workerEnvironments[0] } : {}),
        };
      }
      case "projection.trace.recent": {
        const runId = requiredString(params.run_id, "run_id");
        const rows = envelopeRows(projection).filter((row) => stringValue(row.run_id ?? row.meta?.run_id ?? row.payload?.run_id) === runId);
        return { events: limitRows(rows, params) };
      }
      case "projection.interfaces.list":
        return { interfaces: limitRows(filterInterfaces(store.interfaceRows(projection), params), params) };
      case "projection.interfaces.artifact_transfer.targets":
        return {
          artifact_transfer_targets: limitRows(
            filterInterfaceArtifactTransferTargets(store.interfaceArtifactTransferTargetRows(projection), params, projection),
            params,
          ),
        };
      case "projection.bindings.list":
        return { bindings: limitRows(bindingRows(projection), params) };
      case "projection.artifacts.list":
        return store.artifactListProjection(projection, params);
      case "projection.marks.list":
        return { marks: limitRows(markRows(projection), params) };
      case "projection.worker_environments.list":
        return { worker_environments: limitRows(workerEnvironmentRows(projection), params) };
      case "projection.worker_environment_pools.list":
        return { worker_environment_pools: [] };
      case "projection.envelopes.list":
        return { envelopes: limitRows(filterByEnvelopeKind(envelopeRows(projection), params.kind), params) };
      case "projection.interfaces.pending.list":
        return { pending_artifacts: limitRows(pendingArtifactRows(projection), params) };
      case "projection.goals.snapshot":
        return { goals: limitRows(goalRows(projection), params) };
      case "projection.ee_templates.list":
        return { ee_templates: limitRows(eeTemplateRows(projection), params) };
      case "projection.workers.list":
        return { workers: limitRows(workerRows(projection), params) };
      case "projection.tools.list":
        return { tools: limitRows(toolRows(projection), params) };
      case "projection.verifiers.list":
        return { verifiers: limitRows(verifierRows(projection), params) };
      case "projection.policy.ee.list":
        return { policy: limitRows(policyRows(projection), params) };
      case "projection.descriptors.inventory.list":
        return { descriptors: descriptorInventoryRows(projection) };
      case "projection.descriptors.kinds.list":
        return { descriptors: descriptorKindRows() };
      case "projection.descriptors.artifact_labels.list":
        return { descriptors: limitRows(store.artifactLabelRows(projection), params) };
      case "projection.descriptors.mark_kinds.list":
        return { descriptors: limitRows(markKindRows(projection), params) };
      case "projection.descriptors.tool.list":
        return { descriptors: limitRows(toolRows(projection), params) };
      case "projection.descriptors.worker_environment_kind.list":
        return { descriptors: limitRows(descriptorRows(projection, "worker_environment_kind"), params) };
      case "projection.descriptors.ee.list":
        return { descriptors: limitRows(eeTemplateDescriptorRows(projection), params) };
      case "projection.operator.chats.list": {
        if (params.control_targets_only === true) return { chats: await store.operatorChatControlTargets(projection, params.include_titles === true) };
        const chats = limitRows(await store.operatorChatsList(projection), params);
        return { chats, ...(params.include_activity === true ? { activity: await store.operatorActivity(projection, chats) } : {}) };
      }
      case "projection.operator.chats.status":
        return await store.operatorChatStatuses(projection, params);
      case "projection.operator.os.scope":
        return store.operatorOsScope(projection);
      case "projection.operator.os.local_packages":
        return projection.localPackages;
      case "projection.operator.os.workspace_setup":
        return projection.workspaceSetup;
      case "projection.operator.os.applications":
        return store.operatorOsApplications(projection);
      case "projection.operator.os.drivers":
        return store.operatorOsDrivers(projection);
      case "projection.operator.run.activity":
        return await store.operatorRunActivity(projection, requiredString(params.run_id, "run_id"), params);
      case "projection.operator.chat.contexts": {
        const applications = arrayValue(store.operatorOsApplications(projection).applications);
        return {
          contexts: store.operatorChatContextRows(projection),
          application_catalog: applications.map((application) => ({
            id: stringValue(application.id),
            name: stringValue(application.name),
            description: stringValue(application.description),
            category: stringValue(application.category),
            icon: stringValue(application.icon),
            logo_data_url: stringValue(application.logo_data_url),
            logo_dark_data_url: stringValue(application.logo_dark_data_url),
            interfaces: arrayValue(application.interfaces),
            artifact_bindings: arrayValue(application.artifact_bindings),
            task_capabilities: objectOrEmpty(application.task_capabilities),
          })),
        };
      }
      case "projection.operator.chat.messages":
        return { messages: await store.operatorChatMessages(projection, requiredString(params.run_id, "run_id")) };
      case "projection.operator.chat.search":
        return await store.operatorChatSearch(projection, params);
      case "projection.operator.providers.status":
        return { providers: await store.operatorProviderStatusRows(projection) };
      case "projection.scheduled_interfaces.list":
        return { scheduled_interfaces: await store.scheduledInterfaceRows(projection), scope_options: store.scheduledScopeOptions(projection) };
      case "projection.movements.payload":
        return await movementPayloadPreview(params);
      case "projection.client_surfaces.list":
        return await store.clientSurfaceDocumentSnapshot(projection);
      case "projection.artifact.bytes.read":
        return await store.admittedArtifactBytes(projection, params);
      case "projection.interface_media.read":
        return await store.interfaceArtifactMedia(projection, params);
      default:
        throw new Error(`projection method is not implemented: ${method}`);
    }
  }

  return { readProjection };
}
