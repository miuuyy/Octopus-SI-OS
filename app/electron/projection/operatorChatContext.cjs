module.exports = { applicationWithinWorkerCeiling, operatorChatContextProjection };

const { payloadArtifactBindings } = require("../contentAddressedArtifact.cjs");
const { descriptorRows, eeInstanceRows, octopusOsInterfaceSidecars } = require("./registryRows.cjs");
const { arrayValue, numberValue, objectOrEmpty, stringValue } = require("./values.cjs");

function applicationWithinWorkerCeiling(application, ceiling) {
  const interfaces = new Set(arrayValue(ceiling?.interfaces).map(stringValue).filter(Boolean));
  const tools = new Set(arrayValue(ceiling?.tools).map(stringValue).filter(Boolean));
  const kinds = new Set(arrayValue(ceiling?.wek).map(stringValue).filter(Boolean));
  return arrayValue(application?.interfaces).map(stringValue).filter(Boolean).every((interfaceId) => interfaces.has(interfaceId))
    && arrayValue(application?.task_capabilities?.tools).every((capability) => (
      tools.has(stringValue(capability?.tool))
      && interfaces.has(stringValue(capability?.interface))
      && kinds.has(stringValue(capability?.worker_environment_kind))
    ));
}

function operatorChatContextProjection(store) {
  function operatorChatContextRows(projection) {
    const protocol = objectOrEmpty(projection.operatorChatProtocol);
    const osScope = store.operatorOsScope(projection);
    const projectNames = new Map(arrayValue(osScope.project_presentations).map((presentation) => [
      stringValue(presentation.ee_id),
      stringValue(presentation.name),
    ]).filter(([eeId, name]) => eeId && name));
    const projectProfileIndexes = new Map();
    const taskProfiles = osScope.status === "invalid" ? [] : arrayValue(osScope.task_authoring_profiles);
    const agentProjectSources = new Map(arrayValue(osScope.agent_project_sources).map((source) => [
      stringValue(source?.project_ee_id),
      stringValue(source?.source_worker_interface),
    ]).filter(([projectEeId, workerInterfaceId]) => projectEeId && workerInterfaceId));
    const agentPresentations = new Map(arrayValue(osScope.agent_presentations).map((agent) => [
      `${stringValue(agent?.project_ee_id)}\0${stringValue(agent?.worker_interface)}`,
      agent,
    ]));
    const indexedTaskProfiles = taskProfiles.map((profile) => {
      const eeId = stringValue(profile.ee_id);
      const index = projectProfileIndexes.get(eeId) ?? 0;
      projectProfileIndexes.set(eeId, index + 1);
      return { profile, index };
    });
    const homeProfile = osScope.status === "invalid"
      ? null
      : arrayValue(osScope.client_runs)
        .find((run) => stringValue(run?.surface_role) === "home")?.authoring_profile ?? null;
    const authoringProfiles = new Map([
      ...indexedTaskProfiles,
      ...(homeProfile ? [{ profile: homeProfile, index: 0 }] : []),
    ].map(({ profile, index }) => [
      `${stringValue(profile.ee_id)}\0${stringValue(profile.worker_choice_interface)}`,
      { profile, index },
    ]));
    const scopedEnvironmentIds = new Set(arrayValue(osScope.environments).map((row) => stringValue(row.ee_id)).filter(Boolean));
    const allEnvironments = eeInstanceRows(projection);
    const environments = scopedEnvironmentIds.size
      ? allEnvironments.filter((environment) => scopedEnvironmentIds.has(stringValue(environment.ee_id)))
      : osScope.status === "invalid"
        ? []
        : allEnvironments;
    const policiesByEe = new Map(arrayValue(projection.eePolicyProjection?.environments).map((row) => [
      stringValue(row?.ee_id),
      row,
    ]).filter(([eeId]) => eeId));
    const privateInterfaceIds = new Set(octopusOsInterfaceSidecars(projection).keys());
    const providerIds = descriptorRows(projection, "interface")
      .filter((row) => (
        !privateInterfaceIds.has(stringValue(row.id ?? row.rule_id))
        && stringValue(row.interface_type).toLowerCase() === "world"
        && stringValue(row.role).toLowerCase() === "provider"
        && arrayValue(row.interface_driver_config?.private_config?.client_projection_output_labels).includes(protocol.terminalOutputLabel)
        && arrayValue(row.interface_driver_config?.private_config?.client_projection_output_labels).includes(protocol.errorOutputLabel)
      ))
      .map((row) => stringValue(row.id ?? row.rule_id))
      .filter(Boolean);
    const workers = descriptorRows(projection, "interface")
      .filter((row) => (
        !privateInterfaceIds.has(stringValue(row.id ?? row.rule_id))
        && stringValue(row.interface_type).toLowerCase() === "system"
        && stringValue(row.role).toLowerCase() === "worker"
      ));
    const workersById = new Map(workers.map((row) => [stringValue(row.id ?? row.rule_id), row]));
    const installedApplications = arrayValue(store.operatorOsApplications(projection).applications);
    const installedDrivers = arrayValue(store.operatorOsDrivers(projection).drivers);
    return environments.flatMap((environment) => {
      const projectedPolicy = policiesByEe.get(stringValue(environment.ee_id));
      if (!projectedPolicy) return [];
      const effectivePolicy = projectedPolicy.formed_envelope;
      const formedWorkerPolicies = new Map(arrayValue(projectedPolicy.workers).map((row) => [
        stringValue(row?.worker_interface_id),
        objectOrEmpty(row?.interface_policy),
      ]).filter(([workerInterfaceId]) => workerInterfaceId));
      const allowedInterfaces = Array.isArray(effectivePolicy.allowed_interfaces)
        ? new Set(effectivePolicy.allowed_interfaces)
        : null;
      const allowedTools = Array.isArray(effectivePolicy.allowed_tools)
        ? new Set(effectivePolicy.allowed_tools)
        : null;
      const allowedKinds = Array.isArray(effectivePolicy.allowed_worker_environment_kinds)
        ? new Set(effectivePolicy.allowed_worker_environment_kinds)
        : null;
      const interfaceAllowed = (interfaceId) => allowedInterfaces === null || allowedInterfaces.has(interfaceId);
      const availableWorkers = workers
        .filter((worker) => interfaceAllowed(stringValue(worker.id)))
        .map((worker) => {
          const clientTemplate = objectOrEmpty(worker.client_worker_template);
          const clientTemplateMatchesProject = stringValue(clientTemplate.project_ee_id) === stringValue(environment.ee_id)
            && stringValue(clientTemplate.worker_interface) === stringValue(worker.id);
          const sourceWorker = clientTemplateMatchesProject ? workersById.get(stringValue(worker.copy_of)) : worker;
          const outsideCeilingReason = sourceWorker
            ? "Not in this Agent's capability ceiling"
            : `Source worker template '${stringValue(worker.copy_of)}' is not installed`;
          const ceiling = objectOrEmpty(objectOrEmpty((sourceWorker ?? worker).worker_policy).ceiling);
          const workerInterfaces = new Set(arrayValue(ceiling.interfaces).map(stringValue).filter(Boolean));
          const workerTools = new Set(arrayValue(ceiling.tools).map(stringValue).filter(Boolean));
          const workerKinds = new Set(arrayValue(ceiling.wek).map(stringValue).filter(Boolean));
          const authoredTemplate = authoringProfiles.get(
            `${stringValue(environment.ee_id)}\0${stringValue(worker.id)}`,
          );
          const authoringProfile = authoredTemplate?.profile;
          const agentPresentation = agentPresentations.get(
            `${stringValue(environment.ee_id)}\0${stringValue(worker.id)}`,
          );
          const profileInputs = new Set(arrayValue(authoringProfile?.external_inputs).map((route) => stringValue(route?.interface_id)).filter(Boolean));
          const profileOutputs = new Set(arrayValue(authoringProfile?.model_outputs).map((route) => stringValue(route?.interface_id)).filter(Boolean));
          const profileArtifacts = new Set(arrayValue(authoringProfile?.context_artifact_bindings).map((binding) => stringValue(binding?.id)).filter(Boolean));
          const profileKinds = new Set(arrayValue(authoringProfile?.selected_worker_environment_kinds).map(stringValue).filter(Boolean));
          const projectApplications = installedApplications.flatMap((application) => {
            const projectState = arrayValue(application?.project_states).find((state) => (
              stringValue(state?.ee_id) === stringValue(environment.ee_id)
            ));
            const interfaces = arrayValue(application?.interfaces).map(stringValue).filter(Boolean);
            const taskTools = arrayValue(application?.task_capabilities?.tools).map((capability) => ({
              tool: stringValue(capability?.tool),
              owner: stringValue(capability?.interface),
              kind: stringValue(capability?.worker_environment_kind),
            }));
            const projectAvailable = application?.valid === true
              && projectState?.policy_allowed === true
              && interfaces.every(interfaceAllowed)
              && taskTools.every(({ tool, owner, kind }) => tool && owner && kind
                && (allowedTools === null || allowedTools.has(tool))
                && interfaceAllowed(owner)
                && (allowedKinds === null || allowedKinds.has(kind)));
            if (!projectAvailable) return [];
            return [{ application, projectState, interfaces, withinCeiling: applicationWithinWorkerCeiling(application, ceiling) }];
          });
          const applications = projectApplications
            .filter(({ withinCeiling }) => withinCeiling)
            .map(({ application, projectState, interfaces }) => {
              const explicitlySelected = interfaces.some((interfaceId) => (
                profileInputs.has(interfaceId) || profileOutputs.has(interfaceId)
              )) || arrayValue(application?.artifact_bindings).some((binding) => profileArtifacts.has(stringValue(binding?.artifact_id)));
              return {
                id: stringValue(application.id),
                selected_by_default: projectState?.selected_by_default === true || explicitlySelected,
              };
            });
          const unavailableApplications = projectApplications
            .filter(({ withinCeiling }) => !withinCeiling)
            .map(({ application }) => ({
              id: stringValue(application.id),
              reason: outsideCeilingReason,
            }));
          const drivers = installedDrivers.flatMap((driver) => {
            const name = stringValue(driver?.name);
            const projectState = arrayValue(driver?.project_states).find((state) => (
              stringValue(state?.ee_id) === stringValue(environment.ee_id)
            ));
            if (
              !name
              || projectState?.policy_allowed !== true
              || !workerKinds.has(name)
              || allowedKinds !== null && !allowedKinds.has(name)
            ) return [];
            const toolBindings = arrayValue(driver?.tool_bindings).filter((binding) => {
              const toolId = stringValue(binding?.tool_id);
              const interfaceId = stringValue(binding?.interface_id);
              return toolId
                && interfaceId
                && workerTools.has(toolId)
                && workerInterfaces.has(interfaceId)
                && (allowedTools === null || allowedTools.has(toolId))
                && interfaceAllowed(interfaceId);
            });
            if (!toolBindings.length) return [];
            return [{
              name,
              capabilities: arrayValue(driver.capabilities).map(stringValue).filter(Boolean),
              root_slots: [...new Set([
                ...arrayValue(driver?.boundary_defaults?.read_root_refs),
                ...arrayValue(driver?.boundary_defaults?.write_root_refs),
              ].map((root) => stringValue(root?.path)).filter(Boolean))].sort(),
              tool_ids: [...new Set(toolBindings.map((binding) => stringValue(binding.tool_id)))].sort(),
              door_interfaces: [...new Set(toolBindings.map((binding) => stringValue(binding.interface_id)))].sort(),
              tools: toolBindings.map((binding) => ({
                tool: stringValue(binding.tool_id),
                interface: stringValue(binding.interface_id),
                worker_environment_kind: name,
              })),
              host_slots: objectOrEmpty(projectState.host_slots),
              selected_by_default: projectState.selected_by_default === true || profileKinds.has(name),
            }];
          });
          const workerProviderIds = providerIds
            .filter((providerId) => workerInterfaces.has(providerId) && interfaceAllowed(providerId))
            .sort();
          const interfacePolicy = formedWorkerPolicies.get(stringValue(worker.id));
          if (!interfacePolicy) return null;
          return {
            id: stringValue(worker.id),
            label: arrayValue(worker.source_names).map(stringValue).find(Boolean) || stringValue(worker.id),
            authoring_profile: Boolean(authoringProfile),
            authoring_profile_index: authoredTemplate?.index ?? -1,
            agent_source_template: agentProjectSources.get(stringValue(environment.ee_id)) === stringValue(worker.id),
            client_agent_id: stringValue(agentPresentation?.id),
            client_agent_name: stringValue(agentPresentation?.name),
            client_agent_icon: stringValue(agentPresentation?.icon),
            client_agent_created_at_ms: numberValue(agentPresentation?.created_at_ms, 0),
            client_agent_updated_at_ms: numberValue(agentPresentation?.updated_at_ms, 0),
            client_agent_deleted_at_ms: numberValue(agentPresentation?.deleted_at_ms, 0),
            client_template: clientTemplateMatchesProject,
            client_template_provider_interface_id: clientTemplateMatchesProject
              ? stringValue(clientTemplate.provider_interface_id)
              : "",
            client_template_provider_model: clientTemplateMatchesProject
              ? stringValue(clientTemplate.provider_model)
              : "",
            client_template_reasoning_effort: clientTemplateMatchesProject
              ? stringValue(clientTemplate.reasoning_effort)
              : "",
            client_template_service_tier: clientTemplateMatchesProject
              ? stringValue(clientTemplate.service_tier) : "",
            client_template_digest: clientTemplateMatchesProject
              ? stringValue(worker.client_worker_template_digest)
              : "",
            client_template_application_ids: clientTemplateMatchesProject
              ? arrayValue(clientTemplate.application_ids)
              : [],
            client_template_worker_environment_kinds: clientTemplateMatchesProject
              ? arrayValue(clientTemplate.worker_environment_kinds)
              : [],
            provider_interface_ids: workerProviderIds,
            interface_policy: structuredClone(interfacePolicy),
            role_artifact_ids: arrayValue(worker.worker_policy?.role_artifacts).map(stringValue).filter(Boolean),
            authoring_room_interface_id: stringValue(protocol.authoringRoomInterfaceId),
            door_host_slots: objectOrEmpty(authoringProfile?.door_host_slots),
            external_inputs: arrayValue(authoringProfile?.external_inputs),
            external_relays: arrayValue(authoringProfile?.external_relays),
            model_outputs: arrayValue(authoringProfile?.model_outputs),
            context_artifact_bindings: arrayValue(authoringProfile?.context_artifact_bindings),
            payload_artifact_bindings: payloadArtifactBindings(authoringProfile?.payload_artifact_bindings),
            selected_worker_environment_kinds: arrayValue(authoringProfile?.selected_worker_environment_kinds),
            selected_tools: arrayValue(authoringProfile?.selected_tools),
            applications,
            unavailable_applications: unavailableApplications,
            drivers,
          };
        })
        .filter((worker) => worker?.authoring_profile)
        .sort((left, right) => left.authoring_profile_index - right.authoring_profile_index || left.id.localeCompare(right.id));
      const defaultTaskAccessMode = stringValue(
        objectOrEmpty(objectOrEmpty(environment.config).octopus_os).task_access_default,
      ) || "full_access";
      if (!["project_access", "full_access"].includes(defaultTaskAccessMode)) {
        throw new Error(`EE '${stringValue(environment.ee_id)}' has an invalid Octopus OS task access default`);
      }
      return [{
        ee_id: stringValue(environment.ee_id),
        is_os_root: stringValue(environment.ee_id) === stringValue(osScope.root_ee_id),
        display_name: projectNames.get(stringValue(environment.ee_id)) ?? "",
        goal: stringValue(environment.goal),
        template_name: stringValue(environment.template_name),
        parent_ee_id: stringValue(environment.parent_ee_id),
        agent_catalog_digest: stringValue(osScope.agent_catalog_digest),
        default_task_access_mode: defaultTaskAccessMode,
        host_root_refs: objectOrEmpty(projection.hostRootRefs),
        full_access_root: stringValue(projection.fullAccessRoot),
        source_folders: arrayValue(environment.config?.octopus_os?.source_folders),
        formed_envelope: effectivePolicy,
        workers: availableWorkers,
      }];
    }).filter((row) => row.ee_id && row.workers.length > 0);
  }

  return { operatorChatContextRows };
}
