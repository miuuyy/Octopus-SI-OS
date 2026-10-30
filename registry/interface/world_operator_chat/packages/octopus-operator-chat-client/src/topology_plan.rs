use super::*;

pub fn plan_operator_chat_topology(
    request: &OperatorChatPlanRequest,
) -> Result<OperatorChatPlan, String> {
    let plan = plan_operator_chat_topology_files(request, false)?;
    let planned = plan.interface_writes.iter().collect::<Vec<_>>();
    let lineage = run_composition::planned_copy_lineage(&request.registry_root, &plan.required_interface_ids, &plan.required_tool_ids, &planned)?;
    validate_ee_capability_allowances(&request.registry_root, &plan.ee_template_chain.iter().map(|id|octopus_descriptors::EeTemplateId::new(id)).collect::<Vec<_>>(),
        &plan.required_interface_ids, &plan.required_tool_ids, &lineage)?;
    Ok(plan)
}

pub(crate) fn plan_operator_chat_topology_files(
    request: &OperatorChatPlanRequest,
    preserve_existing_writers: bool,
) -> Result<OperatorChatPlan, String> {
    plan_operator_chat_topology_files_for_source(request, preserve_existing_writers, None, None)
}

pub(crate) fn plan_operator_chat_topology_files_for_source(
    request: &OperatorChatPlanRequest,
    preserve_existing_writers: bool,
    native_metadata: Option<&octopus_ledger::InvocationMetadata>,
    selected_worker: Option<(CapabilityRegistry, run_composition::current_run::CurrentWorkerPlanning)>,
) -> Result<OperatorChatPlan, String> {
    let chat_id = safe_segment(&request.chat_id, "chat_id")?;
    let cycle = safe_segment(&request.cycle, "cycle")?;
    let registry_root = require_absolute("registry_root", &request.registry_root)?;
    let ee_instances_root = require_absolute("ee_instances_root", &request.ee_instances_root)?;
    if let Some(root) = request.interface_private_state_root.as_deref() {
        require_absolute("interface_private_state_root", root)?;
    }
    let mut external_interfaces = validate_external_routes(
        &request.external_inputs,
        &request.external_relays,
        &request.model_outputs,
    )?;
    let interface_root = registry_root.join("interface");
    let mut config = load_client_config(&interface_root)?;
    let selection = resolve_run_selection(request, &config)?;
    if request.retain_current_capabilities { config.run.context_artifact_bindings.clear(); }

    let ee_policy_root = match request.ee_policy_root.as_deref() {
        Some(root) => require_absolute("ee_policy_root", root)?,
        None => ee_instances_root,
    };
    let chain = resolve_live_ee_policy_chain(ee_policy_root, &EeId::new(&selection.ee_id))
        .map_err(|error| error.to_string())?;
    if selection.surface_role == "home" {
        if chain.len() != 1 {
            return Err("Home must live directly in the root Existence EE; select its actual root EE".into());
        }
        let path = registry_root.join("ee_template").join(chain[0].template.as_str()).join("ee_template.json");
        let template: EeTemplateDescriptor = parse_strict(&std::fs::read(&path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        if !template.bootstrap_root {
            return Err("Home requires the declared bootstrap-root EE".into());
        }
    }
    let retain_current_capabilities = preserve_existing_writers || request.retain_current_capabilities;
    if retain_current_capabilities && request.existing_run_record.is_none() {
        return Err("retained capability planning requires its existing record".into());
    }
    let (capabilities, selected_worker) = match selected_worker {
        Some((capabilities, worker)) if retain_current_capabilities && capabilities.registry_root() == registry_root => (capabilities, Some(worker)),
        Some(_) => return Err("a selected current worker requires retained planning over its own registry".into()),
        None => (CapabilityRegistry::load(registry_root), None),
    };
    let current_worker = if retain_current_capabilities {
        let existing = request.existing_run_record.as_ref().ok_or("retained capability planning requires its existing record")?;
        let current = RunRecord::from_bytes(&serde_json::to_vec(existing).map_err(|e|e.to_string())?).map_err(|e|e.to_string())?;
        let client = selection.chat_interface_id.as_deref().ok_or("existing-run planning has no client owner")?;
        Some(match selected_worker {
            Some(worker) => worker.for_selection(&capabilities, ee_policy_root, &current, client, &chat_id)?,
            None => run_composition::current_run::form_current_worker(&capabilities, ee_policy_root, &current, client, &chat_id)?,
        })
    } else { None };
    let topology = if let Some(current) = &current_worker { current.topology.clone() } else { capabilities.form(
        &CapabilityFormationRequest::from_live_chain(
            InterfaceId::new(&selection.worker_choice_interface),
            chain.iter().cloned(),
        ),
    )
    .map_err(|error| error.to_string())? };
    if let Some(result) = &config.run.tool_result_receiving {
        let worker = current_worker.as_ref().map(|current| current.topology.worker.as_str()).unwrap_or(&selection.worker_choice_interface);
        let policy: WorkerInterfacePolicy = serde_json::from_slice(&std::fs::read(interface_root.join(worker).join("policy.json")).map_err(|e|e.to_string())?).map_err(|e|e.to_string())?;
        if !result_receiving_authorized(registry_root, &chain, &policy, result)? {
            return Err("selected worker/EE policy does not admit the tool-result correspondence mark".into());
        }
    }
    ensure_external_interfaces_admitted_by_ee(&topology, &external_interfaces)?;
    let initial_topology = select_capability_topology(
        topology.clone(),
        &config,
        &request.selected_worker_environment_kinds,
        &request.selected_tools,
    )?;
    let initial_interfaces = initial_topology.tool_bindings.keys().cloned()
        .chain(external_interfaces.iter().map(|id| InterfaceId::new(id.as_str())))
        .chain(std::iter::once(InterfaceId::new(&selection.provider_choice_interface)))
        .collect::<Vec<_>>();
    let initial_provider = resolve_provider(&interface_root, &selection.provider_choice_interface, request.model.as_deref().unwrap_or(""),
        request.reasoning_effort.as_deref().unwrap_or(""), request.service_tier.as_deref(), &initial_topology)?;
    let initial_identity = plan_identity(&config, &chat_id, &initial_provider, &initial_topology, &selection)?;
    let preserved_client_configuration = if initial_identity.owns_client() {
        preserve_octopus_os_client_configuration(&interface_root, &initial_identity.chat_interface_id, &initial_identity)?
    } else { None };
    let retained_evidence = if request.existing_run_record.is_some() {
        preserved_client_configuration.as_ref().and_then(|preserved| preserved.declaration.authoring_profile.as_ref())
            .map(|profile| profile.publication_evidence.clone()).unwrap_or_default()
    } else { Vec::new() };
    publication_selection::validate_retained_snapshots(registry_root, &retained_evidence)?;
    let original_selection = publication_selection::PublicationSelectionSeeds::from_operator(request);
    let explicit_publications = request.publications.clone();
    let publication_defaults = if request.retain_current_capabilities { Vec::new() } else {
        publication_selection::declared_publication_defaults(registry_root, initial_interfaces.iter().cloned())?
    };
    let mut publications = explicit_publications.clone();
    let mut reused_defaults = Vec::new();
    for pin in publication_defaults.iter().flat_map(|default|&default.publications) {
        if !explicit_publications.contains(pin) && retained_evidence.iter().any(|evidence| &evidence.pin == pin) {
            if !reused_defaults.contains(pin) { reused_defaults.push(pin.clone()); }
        } else { publications.push(pin.clone()); }
    }
    let mut expanded_request;
    let mut publication_writes = Vec::new();
    let mut staged_publication_artifacts = BTreeMap::new();
    let mut publication_evidence = Vec::new();
    let request = if publications.is_empty() { request } else {
        publication_selection::validate_target_read_formation(registry_root, ee_policy_root, &selection.ee_id,
            current_worker.as_ref().map(|current| current.policy.id.as_str()).unwrap_or(&selection.worker_choice_interface))?;
        let mut source_context = if request.retain_current_capabilities {
            context_bindings::retained_context_bindings(request.existing_run_record.as_ref().ok_or("retained publication selection requires its existing record")?,
                &initial_identity.model_interface_id, &request.context_artifact_bindings)?
        } else { context_bindings::declared_context_bindings(registry_root, initial_interfaces, &request.context_artifact_bindings)? };
        for binding in &config.run.context_artifact_bindings {
            if !source_context.contains(binding) { source_context.push(binding.clone()); }
        }
        let authority = match native_metadata {
            Some(metadata) => publication_selection::PublicationSourceAuthority::Native { metadata },
            None => publication_selection::PublicationSourceAuthority::Operator { context_bindings:&source_context, payload_bindings:&request.payload_artifact_bindings },
        };
        let resolved = publication_selection::resolve_publication_selection(registry_root,
            &octopus_context_collections::ScopeContext { ee_id:selection.ee_id.clone(),
                worker_interface:selection.worker_runtime_interface.clone().unwrap_or_else(|| format!("{}_{}",selection.worker_choice_interface,chat_id)) },
            &publications, publication_selection::PublicationSelectionSeeds::from_operator(request), authority, request.write_hash_algorithm)?;
        expanded_request = request.clone();
        resolved.selection.apply_operator(&mut expanded_request);
        publication_writes = resolved.artifact_writes;
        staged_publication_artifacts = resolved.staged_artifacts;
        publication_evidence = resolved.evidence;
        &expanded_request
    };
    external_interfaces = validate_external_routes(&request.external_inputs, &request.external_relays, &request.model_outputs)?;
    ensure_external_interfaces_admitted_by_ee(&topology, &external_interfaces)?;
    let topology = if publications.is_empty() { initial_topology } else {
        select_capability_topology(topology, &config, &request.selected_worker_environment_kinds, &request.selected_tools)?
    };

    let room_binding = topology
        .tool_bindings
        .get(&InterfaceId::new(&config.authoring.room_interface))
        .ok_or_else(|| {
            format!(
                "authoring room interface '{}' is absent from the formed capability topology",
                config.authoring.room_interface
            )
        })?;
    if !room_binding
        .tools
        .iter()
        .any(|tool| tool.tool.as_str() == config.authoring.tool)
    {
        return Err(format!(
            "authoring tool '{}' is absent from formed room interface '{}'",
            config.authoring.tool, config.authoring.room_interface
        ));
    }

    let provider = resolve_provider(
        &interface_root,
        &selection.provider_choice_interface,
        request.model.as_deref().unwrap_or(""),
        request.reasoning_effort.as_deref().unwrap_or(""),
        request.service_tier.as_deref(),
        &topology,
    )?;
    let mut identity = plan_identity(&config, &chat_id, &provider, &topology, &selection)?;
    if let Some(current) = &current_worker { identity.capability_tool_sources = current.tools.clone(); }
    preserve_selected_file_writer(&config, &chat_id, &topology, &request.selected_tools,
        &request.selected_worker_environment_kinds, &mut identity)?;
    identity.voice = home_voice::select(&interface_root, &config, &identity, &topology,
        request.existing_run_record.as_ref().filter(|_| current_worker.is_some()))?;
    ensure_external_interfaces_admitted_by_ee(&topology, &external_interfaces)?;
    let selected_context = if request.retain_current_capabilities {
        context_bindings::retained_context_bindings(request.existing_run_record.as_ref().ok_or("retained context selection requires its existing record")?,
            &identity.model_interface_id, &request.context_artifact_bindings)?
    } else { context_bindings::declared_context_bindings(
        registry_root,
        topology.tool_bindings.keys().cloned()
            .chain(external_interfaces.iter().map(|id| InterfaceId::new(id.as_str())))
            .chain(std::iter::once(provider.interface.id.clone())),
        &request.context_artifact_bindings,
    )? };
    plan_code_mode_owners(
        registry_root,
        &config,
        &topology,
        &mut identity,
        &selected_context,
        request.existing_run_record.as_ref(),
    )?;
    let selected_publication_evidence = publication_evidence.clone();
    publication_evidence = publication_selection::retain_publication_evidence(&retained_evidence, &publication_evidence)?;
    let requested_authoring_profile = OctopusOsAuthoringProfile {
        ee_id: selection.ee_id.clone(),
        worker_choice_interface: selection.worker_choice_interface.clone(),
        door_host_slots: request.door_host_slots.clone(),
        external_inputs: request.external_inputs.clone(),
        external_relays: request.external_relays.clone(),
        model_outputs: request.model_outputs.clone(),
        context_artifact_bindings: request.context_artifact_bindings.clone(),
        payload_artifact_bindings: request.payload_artifact_bindings.clone(),
        selected_worker_environment_kinds: request.selected_worker_environment_kinds.clone(),
        selected_tools: request.selected_tools.clone(),
        publication_evidence: publication_evidence.clone(),
    };
    let publication_expansion = if publications.is_empty() && publication_defaults.is_empty() { None } else {
        let mut requested = requested_authoring_profile.clone();
        original_selection.apply_profile(&mut requested);
        requested.publication_evidence = retained_evidence.to_vec();
        let receipt = publication_selection::PublicationExpansionReceipt { requested, publications:explicit_publications,
            defaults:publication_defaults, reused_defaults, selected:selected_publication_evidence, expanded:requested_authoring_profile.clone() };
        publication_selection::validate_publication_expansion_receipt(registry_root, &receipt)?;
        Some(receipt)
    };
    let previous_authoring_profile = if request.existing_run_record.is_some() {
        preserved_client_configuration.as_ref()
        .and_then(|preserved| preserved.declaration.authoring_profile.clone())
        .ok_or_else(|| "existing operator chat task has no durable authoring profile".to_string())?
    } else {
        requested_authoring_profile.clone()
    };
    let mut authored_authoring_profile = serde_json::to_value(&requested_authoring_profile)
        .map_err(|error| error.to_string())?;
    if let Some(preserved) = preserved_client_configuration.as_ref() {
        if let Some(profile) = preserved.declaration.authoring_profile.as_ref() {
            let previous = serde_json::to_value(profile).map_err(|error| error.to_string())?;
            preserve_unchanged_client_json(&preserved.authored["authoring_profile"], &previous, &mut authored_authoring_profile);
        }
    }
    let retained_connections = retained_connections::selected(
        request.existing_run_record.as_ref(), &identity, Some(&previous_authoring_profile),
        &request.external_inputs, &request.external_relays, &request.model_outputs,
    )?;
    let retained_policy = retained_connections::policy(
        &interface_root, &identity, &retained_connections,
    )?;
    external_interfaces.extend(retained_policy.keys().map(ToString::to_string));
    external_interfaces.sort();
    external_interfaces.dedup();
    ensure_external_interfaces_admitted_by_ee(&topology, &external_interfaces)?;
    let worker_relays: Vec<String> = topology
        .worker_relays
        .keys()
        .map(|pin| pin.as_str().to_string())
        .collect();

    let mut writes = publication_writes;
    write_code_mode_owner_artifacts(registry_root, &identity, &mut writes)?;
    let previous_owned = request
        .existing_run_record
        .as_ref()
        .map(|record| existing_private_capability_ownership(record, &identity, registry_root))
        .transpose()?;
    plan_chat_interface(
        &interface_root,
        &config,
        &identity,
        Some(&requested_authoring_profile),
        request.existing_run_record.is_some(),
        &mut writes,
    )?;
    plan_capability_doors(
        registry_root,
        &interface_root,
        &config,
        &topology,
        &identity,
        &request.door_host_slots,
        &capability_plan::project_source_folders(ee_policy_root, &identity.ee_id)?,
        &mut writes,
    )?;
    plan_authoring_interfaces(&interface_root, &config, &topology, &identity, &mut writes)?;
    plan_independent_code_mode_authoring(
        registry_root,
        &config,
        &topology,
        &identity,
        &mut writes,
    )?;
    plan_private_tools(registry_root, &config, &topology, &identity, &mut writes)?;
    let private_worker_role_artifacts = plan_worker_pin(
        &interface_root,
        &config,
        &selection.worker_choice_interface,
        &topology,
        &identity,
        &external_interfaces,
        current_worker.as_ref().map(|current| &current.policy),
        &mut writes,
    )?;
    retained_connections::preserve_policy(&identity, &retained_policy, &mut writes)?;
    let role_dependency_workers = worker_role_dependency_interfaces(
        &interface_root,
        topology.worker_relays.keys(),
        &external_interfaces,
    )?;
    let role_artifacts = formed_worker_role_artifacts(
        &interface_root,
        &private_worker_role_artifacts,
        role_dependency_workers.iter(),
    )?;
    let retained_context_ids = if request.retain_current_capabilities {
        let existing = RunRecord::from_bytes(&serde_json::to_vec(request.existing_run_record.as_ref().ok_or("retained instruction selection requires its existing record")?).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        octopus_run_authoring::provider_context_bindings(&existing, &InterfaceId::new(&identity.model_interface_id))?
            .into_iter().map(|binding| binding.id.to_string()).collect::<BTreeSet<_>>()
    } else { BTreeSet::new() };
    let mut selected_context = context_bindings::with_instruction_source_bindings_from_planned(
        registry_root,
        role_artifacts.iter().cloned()
            .chain(config.run.context_artifact_bindings.iter().map(|binding| binding.id.clone()))
            .chain(selected_context.iter().map(|binding| binding.id.clone()))
            .filter(|id| !retained_context_ids.contains(id)),
        &selected_context,
        &staged_publication_artifacts,
    )?;
    let operations = OperationRoutes::load(registry_root, &request.external_inputs,
        request.existing_run_record.as_ref().map(|_| previous_authoring_profile.external_inputs.iter()).into_iter().flatten())?;
    for binding in operations.catalog_bindings() {
        if !selected_context.contains(&binding) { selected_context.push(binding); }
    }
    let catalogue = collection_context::plan(collection_context::PlanContext {
        registry_root, ee_instances_root: ee_policy_root, ee_id: &selection.ee_id, identity: &identity,
        bindings: config.run.context_artifact_bindings.iter()
            .chain(request.conversation.artifact_bindings.iter())
            .chain(request.conversation.pinned_artifact_bindings.iter())
            .chain(selected_context.iter()).cloned().collect(),
        native_metadata, existing: request.existing_run_record.as_ref(),
    }, &mut selected_context, &mut writes)?;
    plan_provider_interface(
        &interface_root,
        &config,
        &identity,
        &provider,
        &topology,
        &private_worker_role_artifacts,
        &catalogue.inline_ids(),
        request.interface_private_state_root.as_deref(),
        &request.door_host_slots,
        &mut writes,
    )?;
    home_voice::plan_descriptors(registry_root, &config, &topology, &identity, &mut writes)?;
    selected_context.extend(home_outputs::plan(request, &identity, &private_worker_role_artifacts, &mut writes)?);
    if let Some(binding) = native_run_authoring::plan(native_run_authoring::PlanContext {
        registry_root,
        ee_instances_root: &request.ee_instances_root,
        run_records_root: request.run_records_root.as_deref(),
        interface_private_state_root: request.interface_private_state_root.as_deref(),
        config: &config,
        identity: &identity,
        provider: &provider,
        topology: &topology,
        door_host_slots: &request.door_host_slots,
    }, &mut writes)? { selected_context.push(binding); }
    if let Some(current) = &current_worker {
        for write in &mut writes {
            let owner=write.path.parent().and_then(Path::file_name).and_then(|name|name.to_str());
            let file=write.path.file_name().and_then(|name|name.to_str());
            if owner.is_some_and(|owner|current.retained_doors.contains(owner)) && matches!(file,Some("interface.json" | "life.json")) {
                write.content=std::fs::read_to_string(&write.path).map_err(|e|e.to_string())?;
            } else if file==Some("interface.json") {
                if let Some((source,_))=identity.capability_doors.iter().find(|(_,exact)|Some(exact.as_str())==owner) {
                    let policy=topology.interface_policies.get(&InterfaceId::new(source)).ok_or("planned current capability has no formed interface policy")?;
                    let mut descriptor:InterfaceDescriptor=parse_strict(write.content.as_bytes()).map_err(|e|e.to_string())?;
                    descriptor.interface_policy.values=policy.context.clone();
                    write.content=pretty_json(&serde_json::to_value(descriptor).map_err(|e|e.to_string())?)?;
                }
            }
            if request.retain_current_capabilities && file == Some("interface.json") && write.path.exists() {
                let accepted: InterfaceDescriptor = parse_strict(&std::fs::read(&write.path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
                let mut planned: InterfaceDescriptor = parse_strict(write.content.as_bytes()).map_err(|e| e.to_string())?;
                if let Some(config) = planned.interface_driver_config.as_mut() {
                    for field in ["context_artifact_bindings", "publications"] {
                        match accepted.interface_driver_config.as_ref().and_then(|config| config.private_config.get(field)) {
                            Some(value) => { config.private_config.insert(field.into(), value.clone()); }
                            None => { config.private_config.remove(field); }
                        }
                    }
                }
                write.content = pretty_json(&serde_json::to_value(planned).map_err(|e| e.to_string())?)?;
            }
        }
    }
    let exact_interfaces = exact_topology_interface_ids(&identity, &external_interfaces);
    let mut required_interface_ids = exact_interfaces.clone();
    required_interface_ids.extend(worker_relays.iter().cloned());
    required_interface_ids.sort();
    required_interface_ids.dedup();
    let mut required_tool_ids = identity
        .capability_tools
        .values()
        .cloned()
        .chain(std::iter::once(identity.authoring_tool_id.clone()))
        .collect::<Vec<_>>();
    required_tool_ids.extend(
        identity
            .code_mode_owners
            .values()
            .map(|owner| owner.authoring_tool.clone()),
    );
    if let Some(voice) = &identity.voice { required_tool_ids.push(voice.writer.clone()); }
    required_tool_ids.sort();
    required_tool_ids.dedup();
    let run_record_content = build_chat_run_record(
        &config,
        &identity,
        &cycle,
        &topology,
        &role_artifacts,
        &selection,
        request.existing_run_record.as_ref(),
        &request.conversation,
        &request.external_inputs,
        &request.external_relays,
        &request.model_outputs,
        &selected_context,
        &request.payload_artifact_bindings,
        &provider.client_operation_request_labels,
        request
            .existing_run_record
            .as_ref()
            .map(|_| &previous_authoring_profile),
        &operations,
    )?;
    let run_record_content = catalogue.wire(&identity.model_interface_id, run_record_content)?;
    let run_record_content = home_outputs::wire(&identity, &topology, &role_artifacts, &selected_context, run_record_content)?;
    let (run_record_content, intake) = input_intake::wire(registry_root, &identity, &mut writes, run_record_content)
        .map_err(|error| format!("input intake composition: {error}"))?;
    let (run_record_content, mut voice_run_writes) = home_voice::wire(request, &config, &identity, &role_artifacts, &selected_context, run_record_content)?;
    let run_record_content = context_bindings::configure_tool_context(registry_root, &identity, run_record_content, request.existing_run_record.as_ref())
        .map_err(|error| format!("tool context composition: {error}"))?;
    let run_record_content = if request.retain_current_capabilities {
        context_bindings::retain_context_access(run_record_content,
            request.existing_run_record.as_ref().ok_or("retained context access requires its existing record")?, &identity.model_interface_id)?
    } else { run_record_content };
    let (run_record_content, mut maintenance_runs) = maintenance_attachment::wire(
        maintenance_attachment::AttachmentRequest {
            capabilities: &capabilities,
            ee_instances_root: ee_policy_root,
            run_records_root: request.run_records_root.as_deref(),
            interface_private_state_root: request.interface_private_state_root.as_deref(),
            config: &config,
            identity: &identity,
            source_input_labels: vec![config.artifact_labels.message.clone()],
            existing: request.existing_run_record.as_ref(),
            write_hash_algorithm: request.write_hash_algorithm,
        },
        run_record_content,
        &mut writes,
        &mut required_interface_ids,
        &mut required_tool_ids,
    )?;
    if let Some(intake) = intake {
        for (id, component) in [
            (&intake.interface, "authoring_source"),
            (&intake.door, "authoring_environment"),
        ] {
            plan_octopus_os_sidecar(
                &registry_root.join("interface"), id, &identity, component,
                false, None, false, &mut writes,
            )?;
        }
        required_interface_ids.extend([intake.interface, intake.door]);
        required_tool_ids.push(intake.tool);
        required_interface_ids.sort();
        required_interface_ids.dedup();
        required_tool_ids.sort();
        required_tool_ids.dedup();
    }
    let (run_record_content, mut run_writes) = if preserve_existing_writers {
        writer_runs::wire_preserving_existing(request, &identity, run_record_content)?
    } else { writer_runs::wire(request, &identity, run_record_content).map_err(|e| format!("writer composition: {e}"))? };
    run_writes.append(&mut voice_run_writes);
    run_writes.append(&mut maintenance_runs);
    let retained_paths = planned_retained_registry_paths(registry_root, &identity, &writes);
    let removals = previous_owned
        .as_ref()
        .map(|owned| plan_private_capability_removals(registry_root, owned, &retained_paths))
        .transpose()?
        .unwrap_or_default();
    remove_unchanged_writes(&mut writes)?;
    normalize_write_hash_algorithm(&mut writes, request.write_hash_algorithm)?;

    Ok(OperatorChatPlan {
        run_id: identity.run_id.clone(),
        chat_interface_id: identity.chat_interface_id.clone(),
        model_interface_id: identity.model_interface_id.clone(),
        worker_interface_id: identity.worker_interface_id.clone(),
        client_surface_role: identity.surface_role.clone(),
        authoring_source_interface_id: identity.authoring_source_id.clone(),
        authoring_environment_interface_id: identity.authoring_environment_id.clone(),
        model: provider.model.clone(),
        reasoning_effort: provider.reasoning_effort.clone(),
        service_tier: provider.service_tier.clone(),
        expected_before_hash_algorithm: request.write_hash_algorithm,
        ee_template_chain: chain
            .iter()
            .map(|layer| layer.template.as_str().to_string())
            .collect(),
        capability_doors: identity.capability_doors.clone(),
        capability_tools: identity.capability_tools.clone(),
        authoring_tool_id: identity.authoring_tool_id.clone(),
        worker_relays,
        publication_evidence,
        publication_expansion,
        authoring_profile: requested_authoring_profile,
        authored_authoring_profile,
        required_interface_ids,
        required_tool_ids,
        interface_writes: writes,
        run_writes,
        removals,
        run_record_content,
    })
}
