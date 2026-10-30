module.exports = { operatorOsProjection };

const { clientAgentCatalogPath } = require("../clientAgentCatalog.cjs");
const { applicationTaskCapabilityReferences } = require("./applicationPackages.cjs");
const { stableJson } = require("./jsonFiles.cjs");
const { applicationPrivateConfigurationProjection, descendantEnvironmentIds, descriptorRows, eeInstanceRows, environmentLineage, lowestCommonEnvironment, octopusOsInterfaceSidecars, operatorOsProjectRows, projectedRunInterfaceIds, projectionMemo, runRows, workerEnvironmentRows } = require("./registryRows.cjs");
const { arrayValue, isPlainRecord, numberValue, objectOrEmpty, stringValue } = require("./values.cjs");

function operatorOsProjection(store) {
  function operatorOsScope(projection) {
    return projectionMemo(projection, "structure", "operatorOsScope", () => {
    const sidecars = octopusOsInterfaceSidecars(projection);
    const clientInterfaces = new Map([...sidecars].filter(([, sidecar]) => sidecar.component === "client"));
    const errors = [];
    if (projection.eePolicyProjectionError) {
      errors.push(`canonical EE policy projection failed: ${projection.eePolicyProjectionError}`);
    }
    const policiesByEe = new Map(arrayValue(projection.eePolicyProjection?.environments).map((row) => [
      stringValue(row?.ee_id),
      row,
    ]).filter(([eeId]) => eeId));
    const clientRuns = [];
    for (const record of arrayValue(projection.runRecords)) {
      const matches = [...new Set([
        ...projectedRunInterfaceIds(record),
        ...arrayValue(record.archived_client_interface_ids),
      ])].filter((interfaceId) => clientInterfaces.has(interfaceId)
        && record.interface_state?.[interfaceId]?.client_surface_role !== undefined);
      if (matches.length > 1) {
        errors.push(`run '${record.run_id}' projects more than one exact Octopus OS client interface`);
        continue;
      }
      const interfaceId = matches[0];
      if (!interfaceId) continue;
      const sidecar = clientInterfaces.get(interfaceId);
      const clientState = record.interface_state?.[interfaceId];
      if (clientState?.client_surface_role === undefined) continue;
      if (clientState.client_surface_role !== sidecar.surface_role) {
        errors.push(`run '${record.run_id}' conflicts with its exact client surface role`);
        continue;
      }
      const authoringProfile = sidecar.authoring_profile ?? null;
      if (authoringProfile && stringValue(authoringProfile.worker_choice_interface)
        !== stringValue(sidecar.worker_choice_interface)) {
        errors.push(`Octopus OS authoring profile '${interfaceId}' conflicts with run '${record.run_id}'`);
      }
      clientRuns.push({
        run_id: stringValue(record.run_id),
        ee_id: stringValue(record.ee_id),
        record_path: stringValue(record.record_path),
        record_digest: stringValue(record.record_digest),
        interface_id: interfaceId,
        surface_role: stringValue(sidecar.surface_role),
        archived: arrayValue(record.archived_client_interface_ids).includes(interfaceId),
        provider_choice_interface: stringValue(sidecar.provider_choice_interface),
        worker_choice_interface: stringValue(sidecar.worker_choice_interface),
        authoring_profile: authoringProfile,
      });
    }
    if (!clientRuns.length) {
      return {
        status: errors.length ? "invalid" : "unbound",
        root_ee_id: "",
        root_environment: null,
        home_run_id: "",
        home_interface_id: "",
        space_name: "",
        home_sidecar_digest: "",
        client_interface_ids: [],
        client_runs: [],
        task_authoring_profiles: [],
        agent_catalog_path: clientAgentCatalogPath(store.octopusHome()),
        agent_catalog_digest: "",
        agent_project_sources: [],
        agent_presentations: [],
        read_only_runs: [],
        environments: [],
        projects: [],
        project_presentations: [],
        ee_templates: [],
        errors,
      };
    }
    const environments = eeInstanceRows(projection, false);
    const eeById = new Map(environments.map((row) => [stringValue(row.ee_id), row]).filter(([id]) => id));
    const rootEeId = lowestCommonEnvironment(
      [...new Set(clientRuns.map((row) => row.ee_id).filter(Boolean))],
      eeById,
      errors,
    );
    const scopeEeIds = rootEeId ? descendantEnvironmentIds(rootEeId, eeById) : new Set();
    const scopedClientRuns = clientRuns.filter((row) => scopeEeIds.has(row.ee_id));
    const homeCandidates = scopedClientRuns.filter((row) => row.surface_role === "home");
    let homeRun = null;
    if (homeCandidates.length > 1) {
      errors.push(`Octopus OS scope '${rootEeId}' projects more than one Home run`);
    } else if (homeCandidates.length === 1) {
      if (homeCandidates[0].ee_id !== rootEeId) {
        errors.push(`Home run '${homeCandidates[0].run_id}' is not directly inside OS root EE '${rootEeId}'`);
      } else {
        homeRun = homeCandidates[0];
      }
    }
    const homeSidecar = clientInterfaces.get(homeRun?.interface_id);
    const homeDescriptor = projection.descriptorIndexes?.interface?.get(homeRun?.interface_id);
    if (homeDescriptor?.interface_driver_config?.private_config?.application_consent_authority !== undefined) {
      errors.push("Home application_consent_authority selection is retired and requires explicit configuration repair");
    }
    const taskAuthoringProfiles = arrayValue(homeSidecar?.task_authoring_profiles);
    for (const profile of taskAuthoringProfiles) {
      const profileEeId = stringValue(profile.ee_id);
      const profileWorkerId = stringValue(profile.worker_choice_interface);
      const worker = projection.descriptorIndexes?.interface?.get(profileWorkerId);
      if (!scopeEeIds.has(profileEeId)) {
        errors.push(`Octopus OS task authoring profile references EE '${profileEeId}' outside workspace scope '${rootEeId}'`);
      }
      if (stringValue(worker?.interface_type).toLowerCase() !== "system"
        || stringValue(worker?.role).toLowerCase() !== "worker") {
        errors.push(`Octopus OS task authoring profile references missing worker '${profileWorkerId}'`);
      }
    }
    const clientRunIds = new Set(scopedClientRuns.map((row) => row.run_id));
    const runRecordsById = new Map(arrayValue(projection.runRecords).map((record) => [
      stringValue(record?.run_id),
      record,
    ]).filter(([runId]) => runId));
    const readOnlyRuns = runRows(projection, false)
      .filter((row) => scopeEeIds.has(stringValue(row.ee_id)) && !clientRunIds.has(stringValue(row.run_id)))
      .flatMap((row) => {
        const runId = stringValue(row.run_id);
        const record = runRecordsById.get(runId);
        const recordPath = stringValue(record?.record_path);
        const recordDigest = stringValue(record?.record_digest);
        if (!recordPath || !recordDigest) {
          errors.push(`read-only run '${runId}' has no authorable record evidence`);
          return [];
        }
        const recordedClientInterfaces = Object.keys(objectOrEmpty(record?.interface_state))
          .filter((interfaceId) => clientInterfaces.has(interfaceId)
            && record.interface_state[interfaceId]?.client_surface_role === clientInterfaces.get(interfaceId).surface_role)
          .sort();
        if (recordedClientInterfaces.length > 1) {
          errors.push(`read-only run '${runId}' records more than one exact Octopus OS client interface`);
          return [];
        }
        const interfaceId = recordedClientInterfaces[0] ?? "";
        const sidecar = clientInterfaces.get(interfaceId) ?? {};
        return [{
          run_id: runId,
          ee_id: stringValue(row.ee_id),
          record_path: recordPath,
          record_digest: recordDigest,
          interface_id: interfaceId,
          surface_role: stringValue(sidecar.surface_role) || "task",
          provider_choice_interface: stringValue(sidecar.provider_choice_interface),
          worker_choice_interface: stringValue(sidecar.worker_choice_interface),
        }];
      });
    const templates = new Map(descriptorRows(projection, "ee_template").map((row) => [
      stringValue(row.id ?? row.name),
      row,
    ]).filter(([id]) => id));
    const taskRunIdsByEe = new Map();
    for (const row of scopedClientRuns.filter((candidate) => candidate.surface_role === "task")) {
      const ids = taskRunIdsByEe.get(row.ee_id) ?? [];
      ids.push(row.run_id);
      taskRunIdsByEe.set(row.ee_id, ids);
    }
    const readOnlyRunIdsByEe = new Map();
    for (const row of readOnlyRuns) {
      const ids = readOnlyRunIdsByEe.get(row.ee_id) ?? [];
      ids.push(row.run_id);
      readOnlyRunIdsByEe.set(row.ee_id, ids);
    }
    const scopedEnvironments = [...scopeEeIds].flatMap((eeId) => {
      const environment = eeById.get(eeId);
      if (!environment) return [];
      const policy = policiesByEe.get(eeId);
      if (!policy) {
        errors.push(`canonical EE policy projection is missing EE '${eeId}'`);
        return [];
      }
      const templateName = stringValue(environment.template_name);
      const template = templates.get(templateName) ?? {};
      return [{
        ee_id: eeId,
        name: stringValue(environment.name),
        parent_ee_id: stringValue(environment.parent_ee_id),
        template_name: templateName,
        goal: stringValue(environment.goal),
        status: "projected",
        record_path: stringValue(environment.record_path),
        record_digest: stringValue(environment.record_digest),
        config: objectOrEmpty(environment.config),
        policy_narrowing: objectOrEmpty(environment.policy_narrowing),
        formed_envelope: policy.formed_envelope,
        boundary_envelope: policy.boundary_envelope,
        allowed_child_ee_templates: arrayValue(template.allowed_child_ee_templates).map(stringValue).filter(Boolean),
        client_run_ids: (taskRunIdsByEe.get(eeId) ?? []).sort(),
        read_only_run_ids: (readOnlyRunIdsByEe.get(eeId) ?? []).sort(),
        depth: Math.max(0, environmentLineage(eeId, eeById, []).length - 1),
      }];
    }).sort((left, right) => left.depth - right.depth || left.goal.localeCompare(right.goal) || left.ee_id.localeCompare(right.ee_id));
    const rootEnvironment = scopedEnvironments.find((row) => row.ee_id === rootEeId) ?? null;
    const projects = scopedEnvironments.filter((row) => row.ee_id !== rootEeId);
    const projectEeIds = new Set(projects.map((row) => row.ee_id));
    const workspacePresentation = clientInterfaces.get(homeRun?.interface_id)?.presentation;
    const projectPresentations = arrayValue(workspacePresentation?.projects);
    for (const presentation of projectPresentations) {
      if (!projectEeIds.has(stringValue(presentation.ee_id))) {
        errors.push(
          `Octopus OS Home presentation references project EE '${stringValue(presentation.ee_id)}' outside scope '${rootEeId}'`,
        );
      }
    }
    const profileIds = taskAuthoringProfiles.map((profile) => stringValue(profile?.ee_id)).filter(Boolean);
    const profileScopeIds = [...new Set(profileIds)].sort();
    const presentationIds = projectPresentations.map((presentation) => stringValue(presentation?.ee_id)).filter(Boolean);
    if (new Set(presentationIds).size !== presentationIds.length) {
      errors.push("Octopus OS Home repeats a Project presentation");
    }
    const agentCatalog = projection.clientAgentCatalog?.document ?? null;
    const agentProjectSources = arrayValue(agentCatalog?.project_sources);
    const agentPresentations = arrayValue(agentCatalog?.agents);
    if (!agentCatalog || projection.clientAgentCatalogError) {
      errors.push(projection.clientAgentCatalogError || "Client Agent catalog is unavailable");
    } else {
      const sourceProjectIds = agentProjectSources.map((source) => stringValue(source?.project_ee_id)).sort();
      if (stableJson(sourceProjectIds) !== stableJson(profileScopeIds)) {
        errors.push("Client Agent source workers and task-authoring EEs are not a bijection");
      }
      for (const source of agentProjectSources) {
        const sourceMatches = taskAuthoringProfiles.filter((profile) => (
          stringValue(profile?.ee_id) === stringValue(source?.project_ee_id)
          && stringValue(profile?.worker_choice_interface) === stringValue(source?.source_worker_interface)
        ));
        if (sourceMatches.length !== 1) {
          errors.push(
            `Client Agent source '${stringValue(source?.source_worker_interface)}' has ${sourceMatches.length} exact authoring profiles in '${stringValue(source?.project_ee_id)}'`,
          );
        }
      }
      for (const agent of agentPresentations) {
        const agentMatches = taskAuthoringProfiles.filter((profile) => (
          stringValue(profile?.ee_id) === stringValue(agent?.project_ee_id)
          && stringValue(profile?.worker_choice_interface) === stringValue(agent?.worker_interface)
        ));
        if (!scopeEeIds.has(stringValue(agent?.project_ee_id)) || agentMatches.length !== 1) {
          errors.push(
            `Client Agent '${stringValue(agent?.id)}' has no exact Project worker-template profile`,
          );
        }
        if (numberValue(agent?.deleted_at_ms, 0) === 0) {
          const worker = projection.descriptorIndexes?.interface?.get(stringValue(agent?.worker_interface));
          if (!worker?.client_worker_template) {
            errors.push(`Client Agent '${stringValue(agent?.id)}' has no active client worker template`);
          }
        }
      }
    }
    const templateRows = [...templates].map(([id, template]) => ({
      id,
      goal_class: stringValue(template.goal_class ?? template.goal ?? id),
    })).sort((left, right) => left.goal_class.localeCompare(right.goal_class) || left.id.localeCompare(right.id));
    return {
      status: errors.length ? "invalid" : homeRun ? "ready" : "awaiting_home",
      root_ee_id: rootEeId,
      root_environment: rootEnvironment,
      home_run_id: homeRun?.run_id ?? "",
      home_interface_id: homeRun?.interface_id ?? "",
      space_name: stringValue(workspacePresentation?.name),
      home_sidecar_digest: stringValue(homeSidecar?.source_digest),
      client_interface_ids: scopedClientRuns.map((row) => row.interface_id).sort(),
      client_runs: scopedClientRuns,
      task_authoring_profiles: taskAuthoringProfiles,
      agent_catalog_path: projection.clientAgentCatalog?.file ?? clientAgentCatalogPath(store.octopusHome()),
      agent_catalog_digest: projection.clientAgentCatalog?.digest ?? "",
      agent_project_sources: agentProjectSources,
      agent_presentations: agentPresentations,
      read_only_runs: readOnlyRuns,
      environments: scopedEnvironments,
      projects,
      project_presentations: projectPresentations,
      ee_templates: templateRows,
      errors,
    };
    });
  }

  function operatorOsApplications(projection) {
    return projectionMemo(projection, "structure", "operatorOsApplications", () => {
      const scope = operatorOsScope(projection);
      const environments = arrayValue(scope.environments);
      const scopeEeIds = new Set(environments.map((environment) => stringValue(environment.ee_id)).filter(Boolean));
      const runRecords = arrayValue(projection.runRecords).filter((record) => scopeEeIds.has(stringValue(record.ee_id)));
      const taskProfilesByRun = new Map(arrayValue(scope.client_runs).map((run) => [
        stringValue(run?.run_id),
        objectOrEmpty(run?.authoring_profile),
      ]).filter(([runId]) => runId));
      const descriptors = descriptorRows(projection, "interface");
      const descriptorsByInterface = new Map(descriptors.map((candidate) => [
        stringValue(candidate.id ?? candidate.rule_id),
        candidate,
      ]).filter(([interfaceId]) => interfaceId));
      const toolDescriptors = new Map(descriptorRows(projection, "tool").map((descriptor) => [
        stringValue(descriptor.id),
        descriptor,
      ]).filter(([id]) => id));
      const descriptorIds = new Set(descriptors.map((descriptor) => stringValue(descriptor.id ?? descriptor.rule_id)).filter(Boolean));
      const installedDescriptors = new Map(descriptors.flatMap((descriptor) => {
        const manifest = descriptor.application_manifest;
        return isPlainRecord(manifest) ? [[stringValue(manifest.id), descriptor]] : [];
      }));
      const packageCatalog = objectOrEmpty(projection.applicationPackages);
      const activePackages = new Map(arrayValue(packageCatalog.active).map((candidate) => [
        stringValue(candidate?.manifest?.id),
        candidate,
      ]).filter(([id]) => id));
      const sourcePackages = new Map(arrayValue(packageCatalog.sources).map((candidate) => [
        stringValue(candidate?.manifest?.id),
        candidate,
      ]).filter(([id]) => id));
      const sourceConflicts = objectOrEmpty(packageCatalog.source_conflicts);
      const localApplicationPackages = new Map(arrayValue(projection.localPackages?.packages)
        .filter((candidate) => stringValue(candidate?.product_kind) === "application")
        .map((candidate) => [stringValue(candidate?.product_id), candidate])
        .filter(([id]) => id));
      const applicationIds = [...new Set([
        ...installedDescriptors.keys(),
        ...sourcePackages.keys(),
        ...localApplicationPackages.keys(),
      ])].sort();
      const applications = applicationIds.flatMap((applicationId) => {
        const descriptor = installedDescriptors.get(applicationId);
        const sourcePackage = sourcePackages.get(applicationId);
        const localPackage = localApplicationPackages.get(applicationId);
        const installedPackage = activePackages.get(applicationId);
        const ownerFoldersPresent = Boolean(descriptor);
        const receiptState = stringValue(localPackage?.receipt_state);
        const materialized = localPackage?.materialized === true || ownerFoldersPresent;
        const partiallyMaterialized = localPackage?.partially_materialized === true;
        const installed = localPackage?.installed === true
          && receiptState === "valid"
          && ownerFoldersPresent;
        const materializationNeedsAttention = !installed && (
          materialized || partiallyMaterialized || receiptState === "stale"
        );
        const manifest = descriptor?.application_manifest ?? sourcePackage?.manifest;
        if (!isPlainRecord(manifest)) return [];
        const interfaceIds = arrayValue(manifest.interfaces).map(stringValue).filter(Boolean);
        const missingInterfaceIds = ownerFoldersPresent
          ? interfaceIds.filter((interfaceId) => !descriptorIds.has(interfaceId))
          : [];
        const collidingInterfaceIds = installed
          ? []
          : interfaceIds.filter((interfaceId) => descriptorIds.has(interfaceId));
        const artifactIds = arrayValue(manifest.artifact_bindings)
          .map((binding) => stringValue(binding?.artifact_id))
          .filter(Boolean);
        const taskTools = arrayValue(manifest?.task_capabilities?.tools);
        const capabilityContractValid = taskTools.every((capability) => {
          const toolId = stringValue(capability?.tool);
          const interfaceId = stringValue(capability?.interface);
          const kind = stringValue(capability?.worker_environment_kind);
          const descriptor = toolDescriptors.get(toolId);
          return Boolean(toolId && interfaceId && kind && descriptor)
            && stringValue(descriptor.interface) === interfaceId
            && arrayValue(descriptor.supported_worker_environment_kinds).map(stringValue).includes(kind);
        });
        const runApplicationReferences = runRecords.map((record) => {
          const projectedInterfaces = projectedRunInterfaceIds(record);
          const projectedArtifactIds = new Set(arrayValue(record.checkpoints)
            .filter((checkpoint) => checkpoint.status === "projected")
            .flatMap((checkpoint) => arrayValue(checkpoint.artifact_binding_ids).map(stringValue).filter(Boolean)));
          const runId = stringValue(record.run_id);
          const profile = taskProfilesByRun.get(runId) ?? {};
          const selectedTaskCapabilities = applicationTaskCapabilityReferences(
            profile,
            manifest,
          );
          return {
            runId,
            connected: interfaceIds.every((interfaceId) => projectedInterfaces.has(interfaceId))
              && artifactIds.every((artifactId) => projectedArtifactIds.has(artifactId))
              && selectedTaskCapabilities.complete,
            referenced: interfaceIds.some((interfaceId) => projectedInterfaces.has(interfaceId))
              || artifactIds.some((artifactId) => projectedArtifactIds.has(artifactId)),
          };
        });
        const connectedRunIds = runApplicationReferences
          .filter((reference) => reference.runId && reference.connected)
          .map((reference) => reference.runId)
          .sort();
        const referencedRunIds = runApplicationReferences
          .filter((reference) => reference.runId && reference.referenced)
          .map((reference) => reference.runId)
          .sort();
        const connectedRunIdSet = new Set(connectedRunIds);
        const partiallyConnectedRunIds = referencedRunIds.filter((runId) => !connectedRunIdSet.has(runId));
        const projectStates = environments.map((environment) => {
          const effective = objectOrEmpty(environment.formed_envelope);
          const allowedInterfaces = Array.isArray(effective.allowed_interfaces)
            ? new Set(effective.allowed_interfaces.map(stringValue).filter(Boolean))
            : null;
          const ceiling = objectOrEmpty(environment.boundary_envelope);
          const ceilingInterfaces = Array.isArray(ceiling.allowed_interfaces)
            ? new Set(ceiling.allowed_interfaces.map(stringValue).filter(Boolean))
            : null;
          const allowedTools = Array.isArray(effective.allowed_tools)
            ? new Set(effective.allowed_tools.map(stringValue).filter(Boolean))
            : null;
          const allowedKinds = Array.isArray(effective.allowed_worker_environment_kinds)
            ? new Set(effective.allowed_worker_environment_kinds.map(stringValue).filter(Boolean))
            : null;
          const ceilingTools = Array.isArray(ceiling.allowed_tools)
            ? new Set(ceiling.allowed_tools.map(stringValue).filter(Boolean))
            : null;
          const ceilingKinds = Array.isArray(ceiling.allowed_worker_environment_kinds)
            ? new Set(ceiling.allowed_worker_environment_kinds.map(stringValue).filter(Boolean))
            : null;
          const capabilityInterfaces = taskTools.map((capability) => stringValue(capability?.interface)).filter(Boolean);
          const capabilityTools = taskTools.map((capability) => stringValue(capability?.tool)).filter(Boolean);
          const capabilityKinds = taskTools.map((capability) => stringValue(capability?.worker_environment_kind)).filter(Boolean);
          const requiredInterfaces = [...new Set([...interfaceIds, ...capabilityInterfaces])];
          const policyAllowed = capabilityContractValid && interfaceIds.length > 0
            && (allowedInterfaces === null || requiredInterfaces.every((interfaceId) => allowedInterfaces.has(interfaceId)))
            && (allowedTools === null || capabilityTools.every((toolId) => allowedTools.has(toolId)))
            && (allowedKinds === null || capabilityKinds.every((kind) => allowedKinds.has(kind)));
          const defaults = new Set(arrayValue(
            objectOrEmpty(objectOrEmpty(environment.config).octopus_os).application_defaults,
          ).map(stringValue).filter(Boolean));
          return {
            ee_id: stringValue(environment.ee_id),
            is_root: stringValue(environment.ee_id) === stringValue(scope.root_ee_id),
            policy_allowed: policyAllowed,
            ceiling_allowed: capabilityContractValid
              && (ceilingInterfaces === null || requiredInterfaces.every((interfaceId) => ceilingInterfaces.has(interfaceId)))
              && (ceilingTools === null || capabilityTools.every((toolId) => ceilingTools.has(toolId)))
              && (ceilingKinds === null || capabilityKinds.every((kind) => ceilingKinds.has(kind))),
            selected_by_default: defaults.has(stringValue(manifest.id)),
            record_digest: stringValue(environment.record_digest),
          };
        });
        const sourceConflict = stringValue(sourceConflicts[applicationId]);
        const packageError = stringValue(localPackage?.error);
        const installBlockReason = installed
          ? "Application is already installed."
          : sourceConflict
            || packageError
            || (materializationNeedsAttention
              ? "Application files exist without one valid local install receipt. Repair the package before continuing."
              : "")
            || (!sourcePackage || localPackage?.available !== true
              ? "No verified package source is available."
              : collidingInterfaceIds.length
                ? `Cannot install while interface folders already exist: ${collidingInterfaceIds.join(", ")}`
                : "");
        const sourceKind = localPackage?.available === true ? "local" : "none";
        const managed = stringValue(localPackage?.release_status) === "system";
        const configuration = applicationPrivateConfigurationProjection(
          manifest,
          interfaceIds,
          descriptorsByInterface,
          installed,
        );
        const availableProjects = projectStates.filter((state) => state.policy_allowed);
        const defaultProjects = projectStates.filter((state) => state.selected_by_default);
        const removalBlockReason = !installed
          ? "Application is not installed."
          : managed
            ? "This application is supplied by the Octopus installation and cannot be removed independently."
            : stringValue(localPackage?.receipt_state) !== "valid"
              ? "This application has no valid local install receipt and must be repaired before removal."
            : referencedRunIds.length
              ? partiallyConnectedRunIds.length
                ? `Repair this application's incomplete connection in ${partiallyConnectedRunIds.length} task${partiallyConnectedRunIds.length === 1 ? "" : "s"} before removing it.`
                : `Disconnect this application from ${connectedRunIds.length} task${connectedRunIds.length === 1 ? "" : "s"} before removing it.`
              : availableProjects.length || defaultProjects.length
                ? "Remove this application from every project before removing its package."
                : missingInterfaceIds.length
                  ? "Repair the incomplete package before removing it."
                  : "";
        const presentation = descriptor?.application_manifest ?? sourcePackage;
        return [{
          ...manifest,
          logo_data_url: stringValue(presentation?.logo_data_url),
          logo_dark_data_url: stringValue(presentation?.logo_dark_data_url),
          installed,
          materialized,
          partially_materialized: partiallyMaterialized,
          valid: installed
            ? missingInterfaceIds.length === 0 && capabilityContractValid
            : materializationNeedsAttention
              ? false
            : Boolean(sourcePackage)
              && localPackage?.available === true
              && !packageError
              && !sourceConflict
              && collidingInterfaceIds.length === 0,
          missing_interface_ids: missingInterfaceIds,
          descriptor_path: stringValue(descriptor?.descriptor_path ?? sourcePackage?.descriptor_path),
          manifest_path: stringValue(descriptor?.application_manifest_path ?? sourcePackage?.manifest_path),
          manifest_digest: stringValue(descriptor?.application_manifest_digest ?? sourcePackage?.manifest_digest),
          package_id: stringValue(localPackage?.id) || applicationId,
          installed_digest: stringValue(localPackage?.installed_digest ?? installedPackage?.package_digest),
          source_available: Boolean(sourcePackage) && localPackage?.available === true,
          source_kind: sourceKind,
          install_packages: arrayValue(localPackage?.install_packages),
          source_digest: stringValue(localPackage?.source_digest),
          source_file_count: numberValue(localPackage?.source_file_count, 0),
          receipt_state: receiptState,
          receipt_digest: stringValue(localPackage?.receipt_digest),
          managed,
          install_block_reason: installBlockReason,
          removal_allowed: !removalBlockReason,
          removal_block_reason: removalBlockReason,
          connected_run_ids: connectedRunIds,
          partially_connected_run_ids: partiallyConnectedRunIds,
          project_states: projectStates,
          configuration,
          connection_interfaces: installed
            ? interfaceIds.filter((interfaceId) => Object.prototype.hasOwnProperty.call(
                objectOrEmpty(objectOrEmpty(descriptorsByInterface.get(interfaceId)?.interface_driver_config).private_config),
                "client_connection",
              ))
            : [],
        }];
      }).sort((left, right) => (
        stringValue(left.category).localeCompare(stringValue(right.category))
        || stringValue(left.name).localeCompare(stringValue(right.name))
      ));
      return {
        status: stringValue(scope.status),
        root_ee_id: stringValue(scope.root_ee_id),
        applications,
        projects: operatorOsProjectRows(scope, environments),
        errors: [
          ...arrayValue(scope.errors),
          ...arrayValue(projection.warnings).filter((warning) => {
            const normalized = stringValue(warning).toLowerCase();
            return normalized.includes("application manifest")
              || normalized.includes("application logo")
              || normalized.includes("application package")
              || normalized.includes("application source");
          }),
        ],
      };
    });
  }

  function operatorOsDrivers(projection) {
    return projectionMemo(projection, "structure", "operatorOsDrivers", () => {
      const scope = operatorOsScope(projection);
      const environments = arrayValue(scope.environments);
      const scopeEeIds = new Set(environments.map((environment) => stringValue(environment.ee_id)).filter(Boolean));
      const tools = descriptorRows(projection, "tool");
      const instances = workerEnvironmentRows(projection).filter((instance) => (
        scopeEeIds.has(stringValue(instance.ee_id))
      ));
      const installedDriverDescriptors = new Map(descriptorRows(projection, "worker_environment_kind").map((descriptor) => [
        stringValue(
          descriptor.worker_environment_kind_id
          ?? descriptor.worker_environment_kind
          ?? descriptor.kind
          ?? descriptor.id,
        ),
        descriptor,
      ]).filter(([name]) => name));
      const localDriverPackages = new Map(arrayValue(projection.localPackages?.packages)
        .filter((candidate) => stringValue(candidate?.product_kind) === "driver")
        .map((candidate) => [stringValue(candidate?.product_id), candidate])
        .filter(([name]) => name));
      const driverNames = [...new Set([
        ...installedDriverDescriptors.keys(),
        ...localDriverPackages.keys(),
      ])].sort();
      const drivers = driverNames.map((name) => {
        const installedDescriptor = installedDriverDescriptors.get(name);
        const localPackage = localDriverPackages.get(name);
        const descriptor = installedDescriptor ?? objectOrEmpty(localPackage?.product_metadata?.descriptor);
        const receiptState = stringValue(localPackage?.receipt_state);
        const materialized = localPackage?.materialized === true || Boolean(installedDescriptor);
        const partiallyMaterialized = localPackage?.partially_materialized === true;
        const installed = localPackage?.installed === true
          && receiptState === "valid"
          && Boolean(installedDescriptor);
        const materializationNeedsAttention = !installed && (
          materialized || partiallyMaterialized || receiptState === "stale"
        );
        const supportedTools = tools.filter((tool) => (
          arrayValue(tool.supported_worker_environment_kinds).map(stringValue).includes(name)
        ));
        const toolIds = installedDescriptor
          ? supportedTools.map((tool) => stringValue(tool.id)).filter(Boolean).sort()
          : arrayValue(localPackage?.product_metadata?.tool_ids).map(stringValue).filter(Boolean).sort();
        const toolBindings = supportedTools.map((tool) => ({
          tool_id: stringValue(tool.id),
          interface_id: stringValue(tool.interface),
        })).filter((binding) => binding.tool_id && binding.interface_id)
          .sort((left, right) => left.tool_id.localeCompare(right.tool_id));
        const doorInterfaces = installedDescriptor
          ? [...new Set(supportedTools.map((tool) => stringValue(tool.interface)).filter(Boolean))].sort()
          : arrayValue(localPackage?.product_metadata?.door_interfaces).map(stringValue).filter(Boolean).sort();
        const activeInstances = instances.filter((instance) => (
          stringValue(instance.worker_environment_kind) === name
          && instance.live_available === true
        ));
        const projectStates = environments.map((environment) => {
          const effective = objectOrEmpty(environment.formed_envelope);
          const allowedKinds = Array.isArray(effective.allowed_worker_environment_kinds)
            ? new Set(effective.allowed_worker_environment_kinds.map(stringValue).filter(Boolean))
            : null;
          const ceiling = objectOrEmpty(environment.boundary_envelope);
          const ceilingKinds = Array.isArray(ceiling.allowed_worker_environment_kinds)
            ? new Set(ceiling.allowed_worker_environment_kinds.map(stringValue).filter(Boolean))
            : null;
          const octopusOsConfig = objectOrEmpty(objectOrEmpty(environment.config).octopus_os);
          const defaults = new Set(arrayValue(
            octopusOsConfig.driver_defaults,
          ).map(stringValue).filter(Boolean));
          return {
            ee_id: stringValue(environment.ee_id),
            is_root: stringValue(environment.ee_id) === stringValue(scope.root_ee_id),
            policy_allowed: allowedKinds === null || allowedKinds.has(name),
            ceiling_allowed: ceilingKinds === null || ceilingKinds.has(name),
            selected_by_default: defaults.has(name),
            record_digest: stringValue(environment.record_digest),
            host_slots: objectOrEmpty(objectOrEmpty(octopusOsConfig.driver_host_slots)[name]),
          };
        });
        const projectConfigured = projectStates.some((state) => (
          state.policy_allowed || state.selected_by_default
        ));
        return {
          name,
          display_name: stringValue(localPackage?.display?.name),
          description: stringValue(localPackage?.display?.description),
          icon: stringValue(localPackage?.display?.icon),
          package_id: stringValue(localPackage?.id) || `driver.${name}`,
          installed,
          materialized,
          partially_materialized: partiallyMaterialized,
          source_available: localPackage?.available === true,
          source_digest: stringValue(localPackage?.source_digest),
          installed_digest: stringValue(localPackage?.installed_digest),
          source_file_count: numberValue(localPackage?.source_file_count, 0),
          install_packages: arrayValue(localPackage?.install_packages),
          agent_presets: arrayValue(projection.localPackages?.packs).flatMap((pack) => arrayValue(pack.workspace_setup?.agent_presets)
            .filter((preset) => preset.component_package_id === localPackage?.id)
            .map((preset) => {
              const worker = descriptorRows(projection, "interface").find((entry) => entry.id === preset.worker_interface_id);
              const providerIds = arrayValue(worker?.worker_policy?.ceiling?.interfaces);
              return { ...preset, pack_id: pack.id, pack_manifest_digest: pack.manifest_digest,
                provider_interface_ids: providerIds,
                project_ee_ids: environments.filter((environment) => arrayValue(scope.task_authoring_profiles).some((profile) => profile.ee_id === environment.ee_id)
                  && (!Array.isArray(environment.formed_envelope?.allowed_interfaces) || environment.formed_envelope.allowed_interfaces.includes(preset.worker_interface_id))
                  && arrayValue(projection.eePolicyProjection?.environments).some((entry) => entry.ee_id === environment.ee_id && arrayValue(entry.workers).some((candidate) => candidate.worker_interface_id === preset.worker_interface_id)))
                  .map((environment) => environment.ee_id),
              };
            })),
          receipt_state: receiptState,
          install_block_reason: installed
            ? "Driver is already installed."
            : stringValue(localPackage?.error)
              || (materializationNeedsAttention
                ? "Driver files exist without one valid local install receipt. Repair the package before continuing."
                : "")
              || (localPackage?.available === true ? "" : "No verified package source is available."),
          removal_allowed: installed
            && receiptState === "valid"
            && activeInstances.length === 0
            && !projectConfigured,
          removal_block_reason: !installed
            ? "Driver is not installed."
            : receiptState !== "valid"
              ? "This Driver has no valid local install receipt and must be repaired before removal."
              : activeInstances.length
                ? `Release ${activeInstances.length} active worker environment${activeInstances.length === 1 ? "" : "s"} before removing this Driver.`
                : projectConfigured
                  ? "Remove this Driver from every project before removing its package."
                  : "",
          substrate_family: stringValue(descriptor.substrate_family),
          worker_consent: stringValue(descriptor.worker_consent),
          capabilities: arrayValue(descriptor.capabilities).map(stringValue).filter(Boolean),
          supported_executor_kinds: arrayValue(descriptor.supported_executor_kinds).map(stringValue).filter(Boolean),
          instance_config_files: arrayValue(descriptor.instance_config_files).map(stringValue).filter(Boolean),
          door_interfaces: doorInterfaces,
          door_driver_refs: arrayValue(descriptor.door_driver_refs).map(stringValue).filter(Boolean),
          tool_ids: toolIds,
          tool_bindings: toolBindings,
          boundary_defaults: objectOrEmpty(descriptor.boundary_defaults),
          lifecycle: objectOrEmpty(descriptor.lifecycle),
          descriptor_path: stringValue(descriptor.descriptor_path),
          active_instances: activeInstances,
          project_states: projectStates,
        };
      }).filter((driver) => driver.name).sort((left, right) => left.name.localeCompare(right.name));
      return {
        status: stringValue(scope.status),
        root_ee_id: stringValue(scope.root_ee_id),
        host_root_refs: objectOrEmpty(projection.hostRootRefs),
        host_manifest_ready: !projection.hostManifestError,
        octopus_home: projection.octopusHome,
        drivers,
        projects: operatorOsProjectRows(scope, environments),
        errors: [
          ...arrayValue(scope.errors),
          ...arrayValue(projection.localPackages?.errors),
          ...(projection.hostManifestError ? [projection.hostManifestError] : []),
        ],
      };
    });
  }

  return { operatorOsApplications, operatorOsDrivers, operatorOsScope };
}
