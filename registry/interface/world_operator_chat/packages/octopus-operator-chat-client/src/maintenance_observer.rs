use super::*;
use crate::run_composition::{
    CompositionAuthoring, CompositionCommunication, CompositionOutputSelection, CompositionRoots,
};
use octopus_context_projection::{
    ContextProjectionConfig, OUTCOME_LABEL, OUTCOME_ERROR_LABEL, OUTPUT_LABEL, OutcomeProjectionConfig,
    ProjectionBounds, ProjectionRule,
};
use octopus_run_authoring::snapshot_projection::SnapshotInputRoute;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaintenanceObserverConfig {
    pub schema_version: u32,
    pub worker_template_interface_id: String,
    pub projection_template_interface_id: String,
    pub collection_source: String,
    pub reader_tools: Vec<SelectedToolSeed>,
    pub publication_writer: SelectedToolSeed,
    pub authoring: CompositionAuthoring,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_result_receiving: Option<ToolResultReceiving>,
    pub terminal_label: String,
    pub input_pointers: Vec<String>,
    #[serde(default)]
    pub additional_input_rules: Vec<ProjectionRule>,
    pub terminal_pointers: Vec<String>,
    pub evidence_rules: Vec<ProjectionRule>,
    #[serde(default)]
    pub tool_result_pointers: Vec<String>,
    pub bounds: ProjectionBounds,
    pub task: String,
    pub input_mark: String,
    pub input_verifier: String,
    pub outcome_mark: String,
    pub outcome_verifier: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaintenanceObserverRequest {
    pub roots: CompositionRoots,
    pub source_provider_choice_interface_id: String,
    pub source_provider_interface_id: String,
    pub source_client_interface_id: String,
    pub source_input_labels: Vec<String>,
    pub config: MaintenanceObserverConfig,
    pub write_hash_algorithm: WriteHashAlgorithm,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaintenanceObserverPlan {
    pub source_run_id: String,
    pub source_worker_interface_id: String,
    pub source_provider_interface_id: String,
    pub source_client_interface_id: String,
    pub terminal_label: String,
    pub provider_output_labels: Vec<String>,
    pub ee_id: String,
    pub run_id: String,
    pub projection_run_id: String,
    pub projection_worker_interface_id: String,
    pub experience_interface_id: String,
    pub worker_interface_id: String,
    pub provider_interface_id: String,
    pub relay_interface_id: String,
    pub projection_interface_id: String,
    pub outcome_interface_id: String,
    pub capture_interface_id: String,
    pub publication_door_interface_id: String,
    pub publication_tool_id: String,
    pub collection_entries_root: PathBuf,
    pub companion_declaration: Value,
    pub descriptor_writes: Vec<PlannedClientWrite>,
    pub run_writes: Vec<PlannedClientWrite>,
    pub required_interface_ids: Vec<String>,
    pub required_tool_ids: Vec<String>,
    pub source_required_interface_ids: Vec<String>,
    pub source_required_artifact_bindings: Vec<ArtifactBindingSeed>,
}

pub fn plan_maintenance_observer(
    request: &MaintenanceObserverRequest,
    source_run: &RunRecord,
    source_writes: &[PlannedClientWrite],
    capabilities: &CapabilityRegistry,
) -> Result<MaintenanceObserverPlan, String> {
    let cfg = &request.config;
    if cfg.schema_version != 1 {
        return Err("maintenance requires declared configuration schema 1".into());
    }
    source_run.validate_artifact_arrival_shape()?;
    for (name, id) in [
        ("source run", source_run.id.as_str()),
        ("source EE", source_run.ee.as_str()),
        ("source worker", source_run.worker.as_str()),
        ("source provider", &request.source_provider_interface_id),
        (
            "source provider choice",
            &request.source_provider_choice_interface_id,
        ),
        ("source client", &request.source_client_interface_id),
        (
            "observer worker template",
            &cfg.worker_template_interface_id,
        ),
        ("projection template", &cfg.projection_template_interface_id),
        ("collection", &cfg.collection_source),
    ] {
        safe_segment(id, name)?;
    }
    for (name, root) in [
        ("registry root", &request.roots.registry_root),
        ("EE instances root", &request.roots.ee_instances_root),
        ("run records root", &request.roots.run_records_root),
        (
            "interface private state root",
            &request.roots.interface_private_state_root,
        ),
    ] {
        require_absolute(name, root)?;
        if root
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
        {
            return Err(format!("{name} contains parent traversal"));
        }
    }
    if request.source_input_labels.is_empty()
        || cfg.reader_tools.is_empty()
        || cfg.task.trim().is_empty()
    {
        return Err(
            "maintenance requires explicit source inputs, reader tools and model task".into(),
        );
    }
    let registry = &request.roots.registry_root;
    let interface_root = registry.join("interface");
    let source_provider_path = interface_root
        .join(&request.source_provider_interface_id)
        .join("interface.json");
    let source_descriptor: InterfaceDescriptor =
        parse_strict(&planned_bytes(source_writes, &source_provider_path)?)
            .map_err(|e| e.to_string())?;
    if source_descriptor.id.as_str() != request.source_provider_interface_id {
        return Err("source provider descriptor differs from the selected exact binding".into());
    }
    let source_state = source_run
        .interface_state
        .get(&source_descriptor.id)
        .ok_or("source run has no selected provider state")?;
    let source_topology: BoundRunAuthoringTopology = serde_json::from_value(
        source_state
            .get("run_authoring_binding")
            .cloned()
            .ok_or("source provider has no ordinary handoff topology")?,
    )
    .map_err(|e| e.to_string())?;
    if source_topology.run != source_run.id
        || source_topology.worker != source_run.worker
        || source_topology.provider_interface != source_descriptor.id
        || !source_topology.checkpoints.iter().any(|cp| {
            cp.outbound_interface_bindings
                .iter()
                .any(|binding| binding.interface.as_str() == request.source_client_interface_id)
        })
    {
        return Err(
            "maintenance source does not match its exact run/worker/provider/client topology"
                .into(),
        );
    }
    let source_config = source_descriptor
        .interface_driver_config
        .as_ref()
        .ok_or("source provider configuration is absent")?;
    let (input_rules, input_routes, evidence_rules) =
        projection_contracts(request, &source_topology, source_writes)?;
    let model = source_config
        .private_config
        .get("model")
        .and_then(Value::as_str)
        .ok_or("selected source model is absent")?;
    let effort = match source_config.private_config.get("reasoning_effort") {
        Some(value) => value
            .as_str()
            .ok_or("selected reasoning effort is malformed")?,
        None => "",
    };
    let tier = source_config
        .private_config
        .get("service_tier")
        .map(|v| v.as_str().ok_or("selected service tier is malformed"))
        .transpose()?;
    let output_labels: Vec<String> = serde_json::from_value(
        source_config
            .private_config
            .get("client_projection_output_labels")
            .cloned()
            .ok_or("selected provider has no declared output labels")?,
    )
    .map_err(|e| e.to_string())?;
    let snapshot_labels: BTreeSet<String> = serde_json::from_value(
        source_config
            .private_config
            .get("session_snapshot_labels")
            .cloned()
            .ok_or("selected provider has no declared snapshot labels")?,
    )
    .map_err(|e| e.to_string())?;
    if !snapshot_labels.contains(&cfg.terminal_label)
        || !output_labels.contains(&cfg.terminal_label)
    {
        return Err(
            "maintenance terminal must be a declared source snapshot and provider output".into(),
        );
    }
    let record_labels = output_labels
        .iter()
        .filter(|label| *label != &cfg.terminal_label)
        .cloned()
        .collect::<Vec<_>>();
    if record_labels.is_empty() {
        return Err("maintenance requires declared non-terminal output ownership".into());
    }
    let collection_path = registry
        .join("artifact")
        .join(&cfg.collection_source)
        .join("artifact.json");
    let collection: octopus_context_collections::CollectionDescriptor =
        serde_json::from_slice(&planned_bytes(source_writes, &collection_path)?)
            .map_err(|e| e.to_string())?;
    if collection.id != cfg.collection_source
        || collection.label != "context.collection"
        || collection.schema_version != 1
        || Path::new(&collection.directory)
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
        || collection.directory.is_empty()
    {
        return Err("maintenance requires an exact declared collection entries directory".into());
    }
    let entries_root = collection_path
        .parent()
        .ok_or("collection has no owner")?
        .join(&collection.directory);
    if collection.directory != "entries" {
        return Err(
            "collection capture requires the collection owner's declared entries directory".into(),
        );
    }
    let reader_interface = &cfg.reader_tools[0].interface;
    if cfg
        .reader_tools
        .iter()
        .any(|tool| tool.interface != *reader_interface)
        || cfg.publication_writer.interface != *reader_interface
        || cfg.publication_writer.interface == cfg.authoring.room_interface_id
        || cfg.publication_writer.tool == cfg.authoring.tool_id
    {
        return Err("collection readers and writer must share one door apart from the hidden run-authoring writer".into());
    }
    validate_collection_writer(registry, &cfg.publication_writer, &cfg.collection_source)?;
    let identity_hash = blake3::hash(
        &serde_json::to_vec(&json!([
            "octopus.maintenance.observer.v1",
            source_run.id,
            source_descriptor.id,
            cfg,
        ]))
        .map_err(|e| e.to_string())?,
    )
    .to_hex()
    .to_string();
    let suffix = &identity_hash[..24];
    let worker_id = format!("system_worker_memory_observer_{suffix}");
    let relay_id = format!("system_worker_memory_source_{suffix}");
    let projection_id = format!("system_context_projection_{suffix}");
    let outcome_id = format!("system_worker_memory_outcome_{suffix}");
    let experience_id = format!("system_worker_memory_experience_{suffix}");
    let capture_id = format!("system_worker_memory_capture_{suffix}");
    let projection_run_id = format!("run_memory_projection_{suffix}");
    let projection_worker_id = format!("system_worker_memory_projection_{suffix}");
    let run_id = format!("run_memory_observer_{suffix}");
    let chain = resolve_live_ee_policy_chain(&request.roots.ee_instances_root, &source_run.ee)
        .map_err(|e| e.to_string())?;
    if let Some(result) = &cfg.tool_result_receiving {
        let policy: WorkerInterfacePolicy = serde_json::from_slice(&std::fs::read(
            interface_root.join(&cfg.worker_template_interface_id).join("policy.json"),
        ).map_err(|error| error.to_string())?).map_err(|error| error.to_string())?;
        if !result_receiving_authorized(registry, &chain, &policy, result)? {
            return Err("observer worker/EE policy does not admit the tool-result correspondence mark".into());
        }
    }
    if capabilities.registry_root() != registry.as_path() {
        return Err("maintenance capabilities were loaded from another registry root".into());
    }
    let topology = capabilities
        .form(&CapabilityFormationRequest::from_live_chain(
            InterfaceId::new(&cfg.worker_template_interface_id),
            chain.iter().cloned(),
        ))
        .map_err(|e| e.to_string())?;
    let mut provider = resolve_provider(
        &interface_root,
        &request.source_provider_choice_interface_id,
        model,
        effort,
        tier,
        &topology,
    )?;
    provider.interface = source_descriptor.clone();
    provider.interface.id = InterfaceId::new(&request.source_provider_choice_interface_id);
    provider.interface.copy_of = None;
    let private = &mut provider
        .interface
        .interface_driver_config
        .as_mut()
        .ok_or("provider configuration vanished")?
        .private_config;
    collection_context::configure_inline_selection(private, &[])?;
    if let Some(instructions) = private.get_mut("instruction_artifacts") {
        instructions.as_array_mut().ok_or("instruction_artifacts must be an array")?.clear();
    }
    for key in [
        "input_intake",
        "context_artifact_bindings",
        "run_affordances",
        "model_tool_names",
    ] {
        private.remove(key);
    }
    let config = ClientConfig {
        schema_version: 2,
        interface: outcome_id.clone(),
        home_voice: None,
        result_receiving_updates: Vec::new(),
        artifact_labels: ClientArtifactLabels {
            message: OUTPUT_LABEL.into(),
            turn_control: OUTPUT_LABEL.into(),
            tool_call: cfg.authoring.tool_call_label.clone(),
            _rest: Default::default(),
        },
        authoring: ClientAuthoringConfig {
            source_interface: cfg.authoring.source_interface_id.clone(),
            room_interface: cfg.authoring.room_interface_id.clone(),
            tool: cfg.authoring.tool_id.clone(),
            result_timeout_ms: cfg.authoring.result_timeout_ms,
            _rest: Default::default(),
        },
        run: ClientRunConfig {
            interface_prefix: String::new(),
            run_prefix: String::new(),
            ee: source_run.ee.to_string(),
            worker: cfg.worker_template_interface_id.clone(),
            tool_call_mark: cfg.authoring.tool_call_mark.clone(),
            tool_call_verifier: cfg.authoring.tool_call_verifier.clone(),
            tool_result_receiving: cfg.tool_result_receiving.clone(),
            listen_env: String::new(),
            context_artifact_bindings: Vec::new(),
            _rest: Default::default(),
        },
    };
    let tools = cfg
        .reader_tools
        .iter()
        .cloned()
        .chain(std::iter::once(cfg.publication_writer.clone()))
        .collect::<Vec<_>>();
    let mut topology = select_capability_topology(topology, &config, &[], &tools)?;
    topology.worker_relays.clear();
    topology.operation_bindings.clear();
    let selection = RunSelection {
        ee_id: source_run.ee.to_string(),
        worker_choice_interface: cfg.worker_template_interface_id.clone(),
        provider_choice_interface: request.source_provider_choice_interface_id.clone(),
        provider_runtime_interface: None,
        run_id: Some(run_id.clone()),
        chat_interface_id: Some(outcome_id.clone()),
        worker_runtime_interface: Some(worker_id.clone()),
        surface_role: String::new(),
    };
    let mut identity = plan_identity(&config, suffix, &provider, &topology, &selection)?;
    identity.native_communication = Some(CompositionCommunication {
        client_template_interface_id: None,
        client_title: None,
        receive_interface_id: worker_id.clone(),
        return_interface_ids: vec![outcome_id.clone()],
        client_interface_id: None,
        output_selection: Some(CompositionOutputSelection {
            return_labels: vec![cfg.terminal_label.clone()],
            record_labels,
        }),
    });
    let mut writes = Vec::new();
    let slots = BTreeMap::from([(
        reader_interface.clone(),
        BTreeMap::from([("registry_artifact_root".into(), registry.join("artifact"))]),
    )]);
    plan_capability_doors(
        registry,
        &interface_root,
        &config,
        &topology,
        &identity,
        &slots,
        &[],
        &mut writes,
    )?;
    plan_authoring_interfaces(&interface_root, &config, &topology, &identity, &mut writes)?;
    plan_private_tools(registry, &config, &topology, &identity, &mut writes)?;
    let external = vec![experience_id.clone(), outcome_id.clone()];
    let roles = plan_worker_pin(
        &interface_root,
        &config,
        &cfg.worker_template_interface_id,
        &topology,
        &identity,
        &external,
        None,
        &mut writes,
    )?;
    let selected_context = context_bindings::with_instruction_source_bindings(
        registry,
        roles.iter().cloned(),
        &[ArtifactBindingSeed {
            id: cfg.collection_source.clone(),
        }],
    )?;
    plan_provider_interface(
        &interface_root,
        &config,
        &identity,
        &provider,
        &topology,
        &roles,
        &[],
        Some(&request.roots.interface_private_state_root),
        &mut writes,
    )?;
    clone_source_life(
        &interface_root,
        &request.source_provider_interface_id,
        &identity.model_interface_id,
        source_writes,
        &mut writes,
    )?;
    let publication_tool = identity
        .capability_tools
        .get(&cfg.publication_writer.tool)
        .ok_or("publication writer was not formed")?
        .clone();
    let publication_door = identity
        .capability_doors
        .get(&cfg.publication_writer.interface)
        .ok_or("publication door was not formed")?
        .clone();
    let mut projection = ContextProjectionConfig {
        schema_version: 1,
        observer_run: run_id.clone(),
        projection_run: projection_run_id.clone(),
        source_run: source_run.id.to_string(),
        source_ee: None,
        source_worker_interface: source_run.worker.to_string(),
        source_provider_interface: request.source_provider_interface_id.clone(),
        source_client_interface: request.source_client_interface_id.clone(),
        source_relay_interface: relay_id.clone(),
        terminal_label: cfg.terminal_label.clone(),
        snapshot_labels,
        input_rules,
        input_routes,
        evidence_rules,
        terminal_pointers: cfg.terminal_pointers.clone(),
        collection_source: cfg.collection_source.clone(),
        publication_tool: publication_tool.clone(),
        bounds: cfg.bounds.clone(),
        task: cfg.task.clone(),
        outcome: None,
    };
    projection.validate()?;
    plan_projection(registry, cfg, &projection_id, &projection, &mut writes)?;
    plan_relay(
        registry,
        &cfg.worker_template_interface_id,
        &relay_id,
        &mut writes,
    )?;
    plan_relay(
        registry,
        &cfg.worker_template_interface_id,
        &outcome_id,
        &mut writes,
    )?;
    plan_relay(
        registry,
        &cfg.worker_template_interface_id,
        &experience_id,
        &mut writes,
    )?;
    plan_relay(
        registry,
        &cfg.worker_template_interface_id,
        &capture_id,
        &mut writes,
    )?;
    configure_reader_and_writer_roots(
        registry,
        &identity,
        reader_interface,
        &selected_context,
        &request.roots.run_records_root,
        source_run.id.as_str(),
        &mut writes,
    )?;
    let terminal_requirement = RequiredVerifiedArtifact {
        mark: octopus_descriptors::MarkId::new(&cfg.input_mark),
        artifact_label: cfg.terminal_label.clone(),
        verifier: octopus_descriptors::VerifierId::new(&cfg.input_verifier),
        params: json!({"stage":"terminal","source_provider_interface":request.source_provider_interface_id,
            "max_bytes":cfg.bounds.max_input_bytes}),
    };
    let projected_requirement = RequiredVerifiedArtifact {
        mark: octopus_descriptors::MarkId::new(&cfg.input_mark),
        artifact_label: OUTPUT_LABEL.into(),
        verifier: octopus_descriptors::VerifierId::new(&cfg.input_verifier),
        params: json!({"stage":"projection","source_run":source_run.id,
            "source_worker_interface":source_run.worker,"source_provider_interface":request.source_provider_interface_id,
            "source_client_interface":request.source_client_interface_id,"max_bytes":cfg.bounds.max_output_bytes}),
    };
    let content = build_chat_run_record(
        &config,
        &identity,
        "initial",
        &topology,
        &roles,
        &selection,
        None,
        &ConversationSeed {
            ledger_offset: 0,
            artifact_bindings: Vec::new(),
            pinned_artifact_bindings: Vec::new(),
            pinned_context_items: Vec::new(),
            continued_from: None,
        },
        &[ExternalInputSeed {
            checkpoint_id: "cp_memory_projection_initial".into(),
            interface_id: experience_id.clone(),
            artifact_names: Vec::new(),
            required_verified_artifacts: vec![projected_requirement.clone()],
        }],
        &[],
        &[],
        &selected_context,
        &[],
        &[],
        None,
        &OperationRoutes::default(),
    )?;
    let content = finish_observer_record(content, &identity, &projection, &entries_root, cfg)?;
    let content = context_bindings::configure_tool_context(registry, &identity, content, None)?;
    let (content, mut run_writes) = detach_writer(request, &run_id, content)?;
    narrow_detached_writer_roots(
        registry,
        &request.roots.run_records_root,
        &content,
        &identity.model_interface_id,
        &identity.authoring_environment_id,
        &identity.authoring_tool_id,
        &mut writes,
    )?;
    projection.outcome = Some(OutcomeProjectionConfig {
        worker_interface: worker_id.clone(),
        provider_interface: identity.model_interface_id.clone(),
        relay_interface: outcome_id.clone(),
        experience_interface: experience_id.clone(),
        input_routes: vec![SnapshotInputRoute {
            interface_binding: AuthoredInterfaceBinding::all(InterfaceId::new(&experience_id)),
        }],
        tool_call_label: cfg.authoring.tool_call_label.clone(),
        tool_result_labels: identity
            .capability_tools
            .values()
            .map(|tool| format!("{tool}.result"))
            .collect(),
    });
    let path = registry
        .join("interface")
        .join(&projection_id)
        .join("interface.json");
    let mut descriptor: Value =
        serde_json::from_slice(&planned_bytes(&writes, &path)?).map_err(|e| e.to_string())?;
    descriptor["interface_driver_config"]["private_config"]["context_projection"] =
        json!(projection);
    replace(&mut writes, &path, &descriptor)?;
    let (projection_content, mut projection_writes, projection_interfaces, projection_tools) =
        plan_projection_run(
            request,
            &identity,
            &projection,
            source_run.ee.as_str(),
            &projection_id,
            &projection_worker_id,
            &experience_id,
            &capture_id,
            terminal_requirement,
            projected_requirement,
            &mut writes,
        )?;
    let projection_initial: Value =
        serde_json::from_str(&projection_content).map_err(|e| e.to_string())?;
    run_writes.append(&mut projection_writes);
    let initial_record: Value = serde_json::from_str(&content).map_err(|e| e.to_string())?;
    let lifecycle_ownership = created_private_owners(
        registry,
        &writes,
        &[
            source_run.worker.as_str(),
            &request.source_provider_interface_id,
            &request.source_client_interface_id,
        ],
    )?;
    let declaration = json!({"schema_version":1,"run":run_id,"worker_interface":worker_id,
        "provider_interface":identity.model_interface_id,"source_run":source_run.id,"source_worker_interface":source_run.worker,
        "source_provider_interface":request.source_provider_interface_id,"source_client_interface":request.source_client_interface_id,
        "source_relay_interface":relay_id,"projection_interface":projection_id,"experience_interface":experience_id,
        "terminal_label":cfg.terminal_label,"provider_output_labels":output_labels,"input_label":OUTPUT_LABEL,"outcome_interface":outcome_id,"capture_interface":capture_id,"outcome_label":OUTCOME_LABEL,"outcome_error_label":OUTCOME_ERROR_LABEL,
        "projection_run":{"run":projection_run_id,"worker_interface":projection_worker_id,
            "provider_interface":projection_id,"initial_record":projection_initial,"initial_record_content":projection_content},
        "collection_source":cfg.collection_source,
        "collection_entries_root":entries_root,"publication_door_interface":publication_door,"publication_tool":publication_tool,
        "initial_record":initial_record,"initial_record_content":content,"lifecycle_ownership":lifecycle_ownership});
    push_json_write(
        request
            .roots
            .run_records_root
            .join(format!("{run_id}.json")),
        content,
        format!("run:{run_id}"),
        &mut run_writes,
    )?;
    normalize_write_hash_algorithm(&mut writes, request.write_hash_algorithm)?;
    normalize_write_hash_algorithm(&mut run_writes, request.write_hash_algorithm)?;
    let mut interfaces = exact_topology_interface_ids(&identity, &external);
    interfaces.extend(projection_interfaces);
    interfaces.sort();
    interfaces.dedup();
    let mut required_tools = identity
        .capability_tools
        .values()
        .cloned()
        .chain(std::iter::once(identity.authoring_tool_id.clone()))
        .collect::<Vec<_>>();
    required_tools.extend(projection_tools);
    required_tools.sort();
    required_tools.dedup();
    Ok(MaintenanceObserverPlan {
        source_run_id: source_run.id.to_string(),
        source_worker_interface_id: source_run.worker.to_string(),
        source_provider_interface_id: request.source_provider_interface_id.clone(),
        source_client_interface_id: request.source_client_interface_id.clone(),
        terminal_label: cfg.terminal_label.clone(),
        provider_output_labels: output_labels,
        ee_id: source_run.ee.to_string(),
        run_id,
        projection_run_id,
        projection_worker_interface_id: projection_worker_id,
        experience_interface_id: experience_id,
        worker_interface_id: worker_id,
        provider_interface_id: identity.model_interface_id,
        relay_interface_id: relay_id.clone(),
        projection_interface_id: projection_id,
        outcome_interface_id: outcome_id,
        capture_interface_id: capture_id,
        publication_door_interface_id: publication_door,
        publication_tool_id: publication_tool,
        collection_entries_root: entries_root,
        companion_declaration: declaration,
        descriptor_writes: writes,
        run_writes,
        required_interface_ids: interfaces,
        required_tool_ids: required_tools,
        source_required_interface_ids: vec![relay_id],
        source_required_artifact_bindings: Vec::new(),
    })
}

fn created_private_owners(
    registry: &Path,
    writes: &[PlannedClientWrite],
    source_interfaces: &[&str],
) -> Result<Value, String> {
    let mut interfaces = BTreeSet::new();
    let mut tools = BTreeSet::new();
    for (kind, filename, ids) in [
        ("interface", "interface.json", &mut interfaces),
        ("tool", "tool.json", &mut tools),
    ] {
        let root = registry.join(kind);
        for write in writes {
            if write.path.file_name() != Some(std::ffi::OsStr::new(filename))
                || write.path.parent().and_then(Path::parent) != Some(root.as_path())
            {
                continue;
            }
            if write.expected_before_hash.is_some() {
                return Err(format!(
                    "companion {kind} owner '{}' already exists",
                    write.path.display()
                ));
            }
            let descriptor: Value =
                serde_json::from_str(&write.content).map_err(|e| e.to_string())?;
            let id = descriptor["id"]
                .as_str()
                .ok_or("companion creation descriptor has no exact id")?;
            safe_policy_axis_value(id, "companion creation owner")?;
            if write.path.parent().and_then(Path::file_name) != Some(std::ffi::OsStr::new(id))
                || (kind == "interface" && source_interfaces.contains(&id))
                || !ids.insert(id.to_owned())
            {
                return Err("companion creation ownership is not exact and unique".into());
            }
        }
    }
    Ok(json!({"schema_version":1,"private_interface_ids":interfaces,"private_tool_ids":tools}))
}

fn admitted_label_family(inputs: &[&BoundCheckpointTemplate], label: &str) -> Vec<String> {
    let mut family = BTreeSet::new();
    let descendant = format!("{label}.");
    for input in inputs {
        if input.interface_binding.admits(label) {
            family.insert(label.to_string());
        }
        if let BindingArtifactPolicy::Allow { names, .. } = &input.interface_binding.config.artifact_policy {
            family.extend(names.iter().filter(|name| name.starts_with(&descendant)).cloned());
        }
    }
    family.into_iter().collect()
}

fn projection_contracts(
    request: &MaintenanceObserverRequest,
    topology: &BoundRunAuthoringTopology,
    source_writes: &[PlannedClientWrite],
) -> Result<
    (
        Vec<ProjectionRule>,
        Vec<SnapshotInputRoute>,
        Vec<ProjectionRule>,
    ),
    String,
> {
    let cfg = &request.config;
    let mut rules = cfg.additional_input_rules.clone();
    let additional = rules
        .iter()
        .flat_map(|rule| rule.labels.iter().cloned())
        .collect::<BTreeSet<_>>();
    let labels = request
        .source_input_labels
        .iter()
        .filter(|label| !additional.contains(*label))
        .cloned()
        .collect::<Vec<_>>();
    if !labels.is_empty() {
        rules.push(ProjectionRule {
            labels,
            pointers: cfg.input_pointers.clone(),
            origin: None,
        });
    }
    let inputs = topology
        .checkpoints
        .iter()
        .filter(|template| {
            matches!(
                template.role,
                BoundCheckpointRole::Stable
                    | BoundCheckpointRole::IndependentInput
                    | BoundCheckpointRole::OwnerInput
            ) && template
                .outbound_interface_bindings
                .iter()
                .any(|binding| binding.interface.as_str() == request.source_provider_interface_id)
        })
        .collect::<Vec<_>>();
    for rule in &mut rules {
        rule.labels = rule
            .labels
            .iter()
            .flat_map(|label| admitted_label_family(&inputs, label))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
    }
    rules.retain(|rule| !rule.labels.is_empty());
    if rules.is_empty() {
        return Err(
            "selected source has no declared input route matching capture configuration".into(),
        );
    }
    let input_routes = inputs
        .iter()
        .filter(|input| {
            rules.iter().any(|rule| {
                rule.labels
                    .iter()
                    .any(|label| input.interface_binding.admits(label))
            })
        })
        .map(|template| SnapshotInputRoute {
            interface_binding: template.interface_binding.clone(),
        })
        .collect::<Vec<_>>();
    let mut evidence = cfg.evidence_rules.clone();
    if !cfg.tool_result_pointers.is_empty() {
        let mut result_labels = BTreeSet::new();
        for template in topology
            .checkpoints
            .iter()
            .filter(|template| template.role == BoundCheckpointRole::ProviderToolOutput)
        {
            for requirement in &template.required_verified_artifacts {
                let tools: Vec<String> = serde_json::from_value(
                    requirement
                        .params
                        .get("tools")
                        .cloned()
                        .ok_or("source tool output has no exact tools declaration")?,
                )
                .map_err(|e| e.to_string())?;
                for tool in tools {
                    if safe_policy_axis_value(&tool, "source result tool")? != tool
                        || !tool
                            .bytes()
                            .next()
                            .is_some_and(|byte| byte.is_ascii_alphanumeric())
                    {
                        return Err("source result tool must be an exact registry component".into());
                    }
                    let descriptor: ToolDescriptor = serde_json::from_slice(&planned_bytes(
                        source_writes,
                        &request
                            .roots
                            .registry_root
                            .join("tool")
                            .join(&tool)
                            .join("tool.json"),
                    )?)
                    .map_err(|e| e.to_string())?;
                    let label = format!("{tool}.result");
                    if descriptor.id.as_str() != tool
                        || !template
                            .outbound_interface_bindings
                            .iter()
                            .any(|binding| binding.interface == descriptor.interface)
                    {
                        return Err(
                            "source tool descriptor differs from its exact output door".into()
                        );
                    }
                    if template.result_receiving.get(&tool).is_some_and(|receiving| receiving.artifact_label == label)
                        || inputs.iter().any(|input| {
                        input.interface_binding.interface == descriptor.interface
                            && input.interface_binding.admits(&label)
                    }) {
                        result_labels.insert(label);
                    }
                }
            }
        }
        if !result_labels.is_empty() {
            evidence.push(ProjectionRule {
                labels: result_labels.into_iter().collect(),
                pointers: cfg.tool_result_pointers.clone(),
                origin: None,
            });
        }
    }
    Ok((rules, input_routes, evidence))
}

fn planned_bytes(writes: &[PlannedClientWrite], path: &Path) -> Result<Vec<u8>, String> {
    match writes.iter().find(|write| write.path == path) {
        Some(write) => Ok(write.content.as_bytes().to_vec()),
        None => std::fs::read(path)
            .map_err(|e| format!("maintenance source {} cannot be read: {e}", path.display())),
    }
}

fn validate_collection_writer(
    registry: &Path,
    seed: &SelectedToolSeed,
    collection: &str,
) -> Result<(), String> {
    let tool = load_tool_descriptor_from_dir(&registry.join("tool"), &seed.tool)
        .map_err(|e| e.to_string())?
        .ok_or("declared publication writer is absent")?;
    let a = &tool.process.authority;
    if tool.interface.as_str() != seed.interface
        || tool
            .supported_worker_environment_kinds
            .iter()
            .all(|kind| kind.as_str() != seed.worker_environment_kind)
        || a.write_root_slots != BTreeSet::from(["registry_artifact_root".into()])
        || !a.write_argument_paths.is_empty()
        || !a.optional_write_root_slots.is_empty()
        || !a.write_paths.is_empty()
        || !a.write_env_paths.is_empty()
        || !a.optional_write_env_paths.is_empty()
        || !a.write_env_subpaths.is_empty()
    {
        return Err(
            "publication tool must write only through its door's collection backend under registry_artifact_root"
                .into(),
        );
    }
    let path = registry
        .join("interface")
        .join(safe_segment(&seed.interface, "publication door")?)
        .join("interface.json");
    let door: InterfaceDescriptor = parse_strict(
        &std::fs::read(&path).map_err(|e| format!("publication door cannot be read: {e}"))?,
    )
    .map_err(|e| e.to_string())?;
    let named = door
        .interface_driver_config
        .as_ref()
        .and_then(|config| config.private_config.get("memory_collection"))
        .and_then(Value::as_str);
    if door.id.as_str() != seed.interface || named != Some(collection) {
        return Err("publication door must name the declared collection as its memory_collection".into());
    }
    Ok(())
}

fn replace(writes: &mut [PlannedClientWrite], path: &Path, value: &Value) -> Result<(), String> {
    let write = writes
        .iter_mut()
        .find(|write| write.path == path)
        .ok_or("maintenance planned descriptor is absent")?;
    write.content = pretty_json(value)?;
    Ok(())
}

fn clone_source_life(
    root: &Path,
    source: &str,
    target: &str,
    staged: &[PlannedClientWrite],
    writes: &mut [PlannedClientWrite],
) -> Result<(), String> {
    let life = planned_bytes(staged, &root.join(source).join("life.json"))?;
    serde_json::from_slice::<SpawnLifeConfig>(&life).map_err(|e| e.to_string())?;
    let target_write = writes
        .iter_mut()
        .find(|write| write.path == root.join(target).join("life.json"))
        .ok_or("observer provider life was not planned")?;
    target_write.content = String::from_utf8(life).map_err(|e| e.to_string())?;
    Ok(())
}

fn plan_projection(
    registry: &Path,
    cfg: &MaintenanceObserverConfig,
    id: &str,
    projection: &ContextProjectionConfig,
    writes: &mut Vec<PlannedClientWrite>,
) -> Result<(), String> {
    let mut descriptor: Value = serde_json::from_slice(
        &std::fs::read(
            registry
                .join("interface")
                .join(&cfg.projection_template_interface_id)
                .join("interface.json"),
        )
        .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    if descriptor["id"] != cfg.projection_template_interface_id
        || descriptor["interface_type"] != "system"
        || descriptor["interface_driver_ref"] != octopus_context_projection::DRIVER
    {
        return Err(
            "projection template must declare the System bound-context-projection driver".into(),
        );
    }
    crate::copy_json_descriptor(&mut descriptor, id)?;
    descriptor["interface_driver_config"]["private_config"]["context_projection"] =
        json!(projection);
    push_json_write(
        registry.join("interface").join(id).join("interface.json"),
        pretty_json(&descriptor)?,
        format!("interface:{id}"),
        writes,
    )?;
    plan_interface_life(
        &registry.join("interface"),
        &cfg.projection_template_interface_id,
        id,
        writes,
    )
}

fn plan_relay(
    registry: &Path,
    template: &str,
    id: &str,
    writes: &mut Vec<PlannedClientWrite>,
) -> Result<(), String> {
    let mut descriptor: Value = serde_json::from_slice(
        &std::fs::read(
            registry
                .join("interface")
                .join(template)
                .join("interface.json"),
        )
        .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    if descriptor["interface_type"] != "system"
        || descriptor["role"] != "worker"
        || descriptor.get("interface_driver_ref").is_some()
    {
        return Err("maintenance relay template must be an inert System worker pin".into());
    }
    crate::copy_json_descriptor(&mut descriptor, id)?;
    let policy = json!({"id":id,"ceiling":{"interfaces":[],"tools":[],"wek":[],"mark_outcomes":[],"hard_blocked_tools":[],"artifact_verbs":["read"]},"role_artifacts":[]});
    push_json_write(
        registry.join("interface").join(id).join("interface.json"),
        pretty_json(&descriptor)?,
        format!("interface:{id}"),
        writes,
    )?;
    push_json_write(
        registry.join("interface").join(id).join("policy.json"),
        pretty_json(&policy)?,
        format!("policy:{id}"),
        writes,
    )
}

fn configure_reader_and_writer_roots(
    registry: &Path,
    identity: &PlannedIdentity,
    reader: &str,
    context: &[ArtifactBindingSeed],
    runs: &Path,
    observed_run: &str,
    writes: &mut Vec<PlannedClientWrite>,
) -> Result<(), String> {
    let reader = identity
        .capability_doors
        .get(reader)
        .ok_or("context reader was not formed")?;
    let path = registry
        .join("interface")
        .join(reader)
        .join("interface.json");
    let mut descriptor: Value =
        serde_json::from_slice(&planned_bytes(writes, &path)?).map_err(|e| e.to_string())?;
    descriptor["interface_driver_config"]["private_config"]["context_artifact_bindings"] =
        json!(context);
    descriptor["interface_driver_config"]["private_config"]["observed_run"] = json!(observed_run);
    replace(writes, &path, &descriptor)?;
    let path = registry
        .join("interface")
        .join(&identity.authoring_environment_id)
        .join("interface.json");
    let mut descriptor: Value =
        serde_json::from_slice(&planned_bytes(writes, &path)?).map_err(|e| e.to_string())?;
    descriptor["interface_driver_config"]["host_slots"]["run_records_root"] =
        json!({"path":runs.join(format!("{}.json",identity.run_id))});
    replace(writes, &path, &descriptor)
}

fn finish_observer_record(
    content: String,
    identity: &PlannedIdentity,
    _projection: &ContextProjectionConfig,
    _entries: &Path,
    _cfg: &MaintenanceObserverConfig,
) -> Result<String, String> {
    let mut record: Value = serde_json::from_str(&content).map_err(|e| e.to_string())?;
    let unwanted = format!("cp_worker_input_initial");
    record["checkpoints"]
        .as_array_mut()
        .ok_or("observer has no checkpoints")?
        .retain(|cp| cp["id"] != unwanted);
    record["active_checkpoints"]
        .as_array_mut()
        .ok_or("observer has no active checkpoints")?
        .retain(|id| id != &json!(unwanted));
    let templates = record["interface_state"][&identity.model_interface_id]
        ["run_authoring_binding"]["checkpoints"]
        .as_array_mut()
        .ok_or("observer has no handoff templates")?;
    templates.retain(|cp| cp["interface_binding"]["interface"] != identity.worker_interface_id);
    let content = pretty_json(&record)?;
    RunRecord::from_bytes(content.as_bytes()).map_err(|e| e.to_string())?;
    Ok(content)
}

fn detach_writer(
    request: &MaintenanceObserverRequest,
    run: &str,
    content: String,
) -> Result<(String, Vec<PlannedClientWrite>), String> {
    let writer_request:OperatorChatPlanRequest=serde_json::from_value(json!({
        "registry_root":request.roots.registry_root,"ee_instances_root":request.roots.ee_instances_root,
        "run_records_root":request.roots.run_records_root,"write_hash_algorithm":request.write_hash_algorithm,
        "chat_id":run,"provider_interface_id":request.source_provider_choice_interface_id,
        "client_surface_role":"","cycle":"initial",
        "conversation":{"ledger_offset":0,"artifact_bindings":[],"pinned_artifact_bindings":[],"pinned_context_items":[],"continued_from":null}
    })).map_err(|e|e.to_string())?;
    writer_runs::wire_record(&writer_request, run, None, content)
}

fn narrow_detached_writer_roots(
    registry: &Path,
    runs: &Path,
    content: &str,
    provider: &str,
    door: &str,
    tool: &str,
    writes: &mut [PlannedClientWrite],
) -> Result<(), String> {
    let record: Value = serde_json::from_str(content).map_err(|e| e.to_string())?;
    let run = record["id"]
        .as_str()
        .ok_or("detached owner run identity absent")?;
    let writer = record["interface_state"][provider]["run_authoring_binding"]["writer_run"]["run"]
        .as_str()
        .ok_or("detached owner writer identity absent")?;
    safe_segment(run, "detached owner run")?;
    safe_segment(writer, "detached writer run")?;
    let path = registry.join("interface").join(door).join("interface.json");
    let mut descriptor: Value =
        serde_json::from_slice(&planned_bytes(writes, &path)?).map_err(|e| e.to_string())?;
    descriptor["interface_driver_config"]["host_slots"]["run_records_root"] =
        json!({"path":runs.join(format!("{run}.json"))});
    descriptor["interface_driver_config"]["host_slots"]["registry_artifact_root"] =
        json!({"path":runs.join(format!("{writer}.json"))});
    replace(writes, &path, &descriptor)?;
    let path = registry.join("tool").join(tool).join("tool.json");
    let mut descriptor: Value =
        serde_json::from_slice(&planned_bytes(writes, &path)?).map_err(|e| e.to_string())?;
    let paths = descriptor["process"]["authority"]["write_argument_paths"]
        .as_array_mut()
        .ok_or("detached writer has no argument path authority")?;
    for path in paths {
        path["within_root_slots"] = json!(["run_records_root", "registry_artifact_root"]);
        path["include_parent"] = json!(false);
    }
    replace(writes, &path, &descriptor)
}

fn plan_projection_run(
    request: &MaintenanceObserverRequest,
    observer: &PlannedIdentity,
    projection: &ContextProjectionConfig,
    placement: &str,
    id: &str,
    worker: &str,
    experience: &str,
    capture: &str,
    terminal: RequiredVerifiedArtifact,
    projected: RequiredVerifiedArtifact,
    writes: &mut Vec<PlannedClientWrite>,
) -> Result<(String, Vec<PlannedClientWrite>, Vec<String>, Vec<String>), String> {
    let registry = &request.roots.registry_root;
    let suffix = blake3::hash(projection.projection_run.as_bytes())
        .to_hex()
        .to_string();
    let source = format!(
        "system_interface_authoring_memory_projection_{}",
        &suffix[..24]
    );
    let door = format!("system_room_memory_projection_{}", &suffix[..24]);
    let tool = format!("file.write__memory_projection_{}", &suffix[..24]);
    let profile = json!({"file_write_tool":tool,"file_write_label":request.config.authoring.tool_call_label,
        "file_write_result_label":format!("{tool}.result"),"result_timeout_ms":request.config.authoring.result_timeout_ms});
    for (from, to) in [
        (&observer.authoring_source_id, &source),
        (&observer.authoring_environment_id, &door),
    ] {
        let from_root = registry.join("interface").join(from);
        let staged = writes
            .iter()
            .filter(|write| write.path.starts_with(&from_root))
            .cloned()
            .collect::<Vec<_>>();
        for mut write in staged {
            let name = write
                .path
                .strip_prefix(&from_root)
                .map_err(|e| e.to_string())?
                .to_path_buf();
            if name == Path::new("octopus_os.json") {
                continue;
            }
            if matches!(name.to_str(), Some("interface.json" | "activation.json")) {
                let mut value: Value =
                    serde_json::from_str(&write.content).map_err(|e| e.to_string())?;
                if name == Path::new("interface.json") {
                    crate::copy_json_descriptor(&mut value, to)?;
                    if to == &door {
                        value["interface_driver_config"]["host_slots"]["run_records_root"] = json!({"path":request.roots.run_records_root.join(format!("{}.json",projection.projection_run))});
                    }
                } else {
                    value["interface"] = json!(to);
                }
                write.content = pretty_json(&value)?;
            }
            push_json_write(
                registry.join("interface").join(to).join(name),
                write.content,
                format!("projection_authoring:{to}"),
                writes,
            )?;
        }
    }
    let mut descriptor: Value = serde_json::from_slice(&planned_bytes(
        writes,
        &registry
            .join("tool")
            .join(&observer.authoring_tool_id)
            .join("tool.json"),
    )?)
    .map_err(|e| e.to_string())?;
    crate::copy_json_descriptor(&mut descriptor, &tool)?;
    descriptor["interface"] = json!(door);
    push_json_write(
        registry.join("tool").join(&tool).join("tool.json"),
        pretty_json(&descriptor)?,
        format!("tool:{tool}"),
        writes,
    )?;
    let projection_path = registry.join("interface").join(id).join("interface.json");
    let mut descriptor: Value = serde_json::from_slice(&planned_bytes(writes, &projection_path)?)
        .map_err(|e| e.to_string())?;
    descriptor["interface_driver_config"]["private_config"]["run_authoring_profile"] = profile;
    descriptor["interface_driver_config"]["private_config"]["model_hidden_tools"] = json!([tool]);
    descriptor["interface_driver_config"]["private_config"]["context_output_labels"] =
        json!([OUTCOME_LABEL, OUTCOME_ERROR_LABEL]);
    descriptor["interface_driver_config"]["private_config"]["session_snapshot_labels"] =
        json!([OUTCOME_LABEL, OUTCOME_ERROR_LABEL]);
    descriptor["interface_driver_config"]["private_config"]["client_projection_output_labels"] =
        json!([OUTPUT_LABEL, OUTCOME_LABEL, OUTCOME_ERROR_LABEL]);
    descriptor["interface_driver_config"]["host_slots"]["run_records_root"] =
        json!({"path":request.roots.run_records_root});
    replace(writes, &projection_path, &descriptor)?;
    plan_relay(
        registry,
        &request.config.worker_template_interface_id,
        worker,
        writes,
    )?;
    let policy_path = registry.join("interface").join(worker).join("policy.json");
    let interfaces = vec![
        worker.into(),
        id.into(),
        projection.source_relay_interface.clone(),
        experience.into(),
        source.clone(),
        door.clone(),
        capture.into(),
        projection
            .outcome
            .as_ref()
            .ok_or("projection outcome contract absent")?
            .relay_interface
            .clone(),
    ];
    let policy = json!({"id":worker,"ceiling":{"interfaces":interfaces,"tools":[tool],"wek":["system_room"],
        "mark_outcomes":[request.config.input_mark,request.config.outcome_mark,request.config.authoring.tool_call_mark],"hard_blocked_tools":[],"artifact_verbs":["read","create"]},"role_artifacts":[]});
    replace(writes, &policy_path, &policy)?;
    let requirement = serde_json::from_value(authoring_call_requirement(
        &ClientConfig {
            schema_version: 2,
            interface: experience.into(),
            home_voice: None,
            result_receiving_updates: Vec::new(),
            artifact_labels: ClientArtifactLabels {
                message: request.config.terminal_label.clone(),
                turn_control: request.config.terminal_label.clone(),
                tool_call: request.config.authoring.tool_call_label.clone(),
                _rest: Default::default(),
            },
            authoring: ClientAuthoringConfig {
                source_interface: source.clone(),
                room_interface: door.clone(),
                tool: tool.clone(),
                result_timeout_ms: request.config.authoring.result_timeout_ms,
                _rest: Default::default(),
            },
            run: ClientRunConfig {
                interface_prefix: String::new(),
                run_prefix: String::new(),
                ee: String::new(),
                worker: worker.into(),
                tool_call_mark: request.config.authoring.tool_call_mark.clone(),
                tool_call_verifier: request.config.authoring.tool_call_verifier.clone(),
                tool_result_receiving: None,
                listen_env: String::new(),
                context_artifact_bindings: Vec::new(),
                _rest: Default::default(),
            },
        },
        &tool,
    ))
    .map_err(|e| e.to_string())?;
    let input = bound_checkpoint_template(
        BoundCheckpointRole::Stable,
        names_binding(
            &projection.source_relay_interface,
            &[request.config.terminal_label.clone()],
        )?,
        vec![AuthoredInterfaceBinding::all(InterfaceId::new(id))],
        vec![terminal],
    )?;
    let outcome = projection
        .outcome
        .as_ref()
        .ok_or("projection outcome contract absent")?;
    let outcome_input = bound_checkpoint_template(
        BoundCheckpointRole::IndependentInput,
        names_binding(
            &outcome.relay_interface,
            &[request.config.terminal_label.clone()],
        )?,
        vec![AuthoredInterfaceBinding::all(InterfaceId::new(id))],
        vec![RequiredVerifiedArtifact {
            mark: octopus_descriptors::MarkId::new(&request.config.input_mark),
            artifact_label: request.config.terminal_label.clone(),
            verifier: octopus_descriptors::VerifierId::new(&request.config.input_verifier),
            params: json!({"stage":"terminal","source_provider_interface":outcome.provider_interface,"max_bytes":request.config.bounds.max_input_bytes}),
        }],
    )?;
    let outcome_verified = RequiredVerifiedArtifact {
        mark: octopus_descriptors::MarkId::new(&request.config.outcome_mark),
        artifact_label: OUTCOME_LABEL.into(),
        verifier: octopus_descriptors::VerifierId::new(&request.config.outcome_verifier),
        params: json!({"source_run":projection.source_run,"provider_interface":observer.model_interface_id,
            "publication_tool":projection.publication_tool,"entries_root":request.roots.registry_root.join("artifact").join(&request.config.collection_source).join("entries"),
            "max_bytes":request.config.bounds.max_output_bytes}),
    };
    let templates = vec![
        input,
        outcome_input,
        bound_checkpoint_template(
            BoundCheckpointRole::ProviderArtifactOutput,
            names_binding(id, &[OUTCOME_LABEL.into()])?,
            vec![names_binding(capture, &[OUTCOME_LABEL.into()])?],
            vec![outcome_verified],
        )?,
        bound_checkpoint_template(
            BoundCheckpointRole::ProviderArtifactOutput,
            names_binding(id, &[OUTCOME_ERROR_LABEL.into()])?,
            vec![names_binding(capture, &[OUTCOME_ERROR_LABEL.into()])?],
            Vec::new(),
        )?,
        bound_checkpoint_template(
            BoundCheckpointRole::ProviderArtifactOutput,
            names_binding(id, &[OUTPUT_LABEL.into()])?,
            vec![names_binding(experience, &[OUTPUT_LABEL.into()])?],
            vec![projected],
        )?,
        bound_checkpoint_template(
            BoundCheckpointRole::AuthoringCall,
            AuthoredInterfaceBinding::all(InterfaceId::new(&source)),
            vec![AuthoredInterfaceBinding::all(InterfaceId::new(&door))],
            vec![requirement],
        )?,
        bound_checkpoint_template(
            BoundCheckpointRole::AuthoringResult,
            AuthoredInterfaceBinding::all(InterfaceId::new(&door)),
            vec![AuthoredInterfaceBinding::all(InterfaceId::new(id))],
            Vec::new(),
        )?,
    ];
    let ids = (0..templates.len())
        .map(|n| format!("cp_memory_projection_{n}_initial"))
        .collect::<Vec<_>>();
    let checkpoints=templates.iter().zip(&ids).map(|(template,cp)|json!({"id":cp,"run":projection.projection_run,"epoch":0,
        "trace":format!("trace:{}:{cp}",projection.projection_run),"interface_binding":template.interface_binding,
        "outbound_interface_bindings":template.outbound_interface_bindings,"artifact_bindings":[],"required_verified_artifacts":template.required_verified_artifacts})).collect::<Vec<_>>();
    let record = json!({"id":projection.projection_run,"ee":placement,"worker":worker,"status":"idle",
        "trace":format!("trace:{}",projection.projection_run),"checkpoints":checkpoints,"active_checkpoints":ids,
        "interface_state":{id:{"context_generation":0,"authoring_generation":0,"frontier_checkpoint_ids":ids[2..],
            "run_authoring_binding":{"schema_version":2,"run":projection.projection_run,"worker":worker,"provider_interface":id,
                "authoring_source_interface":source,"payload_artifact_bindings":[],"checkpoints":templates}}}});
    let (content, mut run_writes) =
        detach_writer(request, &projection.projection_run, pretty_json(&record)?)?;
    narrow_detached_writer_roots(
        registry,
        &request.roots.run_records_root,
        &content,
        id,
        &door,
        &tool,
        writes,
    )?;
    push_json_write(
        request
            .roots
            .run_records_root
            .join(format!("{}.json", projection.projection_run)),
        content.clone(),
        format!("run:{}", projection.projection_run),
        &mut run_writes,
    )?;
    Ok((content, run_writes, interfaces, vec![tool]))
}

fn names_binding(interface: &str, labels: &[String]) -> Result<AuthoredInterfaceBinding, String> {
    operator_chat_input_binding_names(interface, labels)
}

pub fn wire_source_terminal_output(
    source_content: &str,
    observer: &MaintenanceObserverPlan,
) -> Result<String, String> {
    wire_source_terminal_declaration(source_content, &observer.companion_declaration)
}

pub fn wire_source_terminal_declaration(
    source_content: &str,
    declaration: &Value,
) -> Result<String, String> {
    let field = |name: &str| {
        declaration
            .get(name)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty() && value.trim() == *value)
            .ok_or_else(|| format!("maintenance declaration has no exact {name}"))
    };
    if declaration.get("schema_version") != Some(&json!(1)) {
        return Err("maintenance declaration requires schema 1".into());
    }
    let source_run = field("source_run")?;
    let source_worker = field("source_worker_interface")?;
    let source_provider = field("source_provider_interface")?;
    let source_client = field("source_client_interface")?;
    let source_relay = field("source_relay_interface")?;
    let observer_run = field("run")?;
    let terminal_label = field("terminal_label")?;
    for (name, id) in [
        ("source run", source_run),
        ("source worker", source_worker),
        ("source provider", source_provider),
        ("source client", source_client),
        ("source relay", source_relay),
        ("observer run", observer_run),
    ] {
        safe_segment(id, name)?;
    }
    if source_run == observer_run
        || source_relay == source_provider
        || source_relay == source_client
    {
        return Err(
            "maintenance declaration does not isolate source and companion identities".into(),
        );
    }
    let output_labels: Vec<String> = serde_json::from_value(
        declaration
            .get("provider_output_labels")
            .cloned()
            .ok_or("maintenance declaration has no immutable provider output labels")?,
    )
    .map_err(|e| e.to_string())?;
    let mut unique = BTreeSet::new();
    if output_labels.is_empty()
        || output_labels
            .iter()
            .any(|label| label.is_empty() || label.trim() != label || !unique.insert(label))
        || !output_labels.iter().any(|label| label == terminal_label)
    {
        return Err(
            "maintenance declaration requires unique exact output labels including terminal".into(),
        );
    }
    let parsed = RunRecord::from_bytes(source_content.as_bytes()).map_err(|e| e.to_string())?;
    if parsed.id.as_str() != source_run || parsed.worker.as_str() != source_worker {
        return Err(
            "maintenance declaration cannot attach to a different source run/worker".into(),
        );
    }
    let topology: BoundRunAuthoringTopology = serde_json::from_value(
        parsed
            .interface_state
            .get(&InterfaceId::new(source_provider))
            .and_then(|state| state.get("run_authoring_binding"))
            .cloned()
            .ok_or("maintenance source has no exact provider authoring topology")?,
    )
    .map_err(|e| e.to_string())?;
    if topology.run != parsed.id
        || topology.worker != parsed.worker
        || topology.provider_interface.as_str() != source_provider
    {
        return Err(
            "maintenance declaration differs from the selected source provider topology".into(),
        );
    }
    let mut record: Value = serde_json::from_str(source_content).map_err(|e| e.to_string())?;
    let binding = json!({"interface":source_relay,"config":{"artifact_policy":{"mode":"allow","names":[terminal_label],"namespaces":[]}}});
    let templates =
        record["interface_state"][source_provider]["run_authoring_binding"]["checkpoints"]
            .as_array_mut()
            .ok_or("selected source provider has no ordinary rearm templates")?;
    let candidates = templates
        .iter()
        .enumerate()
        .filter(|(_, cp)| {
            cp["role"] == "provider_artifact_output"
                && cp["interface_binding"]["interface"] == source_provider
                && serde_json::from_value::<AuthoredInterfaceBinding>(
                    cp["interface_binding"].clone(),
                )
                .is_ok_and(|b| b.config.artifact_policy.admits(terminal_label))
                && cp["outbound_interface_bindings"]
                    .as_array()
                    .is_some_and(|bindings| {
                        bindings.iter().any(|b| b["interface"] == source_client)
                    })
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let [index] = candidates.as_slice() else {
        return Err("source terminal has no unique existing output owner".into());
    };
    let original = templates[*index].clone();
    let inbound: AuthoredInterfaceBinding =
        serde_json::from_value(original["interface_binding"].clone()).map_err(|e| e.to_string())?;
    let returns: Vec<AuthoredInterfaceBinding> =
        serde_json::from_value(original["outbound_interface_bindings"].clone())
            .map_err(|e| e.to_string())?;
    if returns.iter().any(|b| b.interface.as_str() == source_relay) {
        if inbound.config.artifact_policy
            == (BindingArtifactPolicy::Allow {
                names: vec![terminal_label.to_owned()],
                namespaces: Vec::new(),
            })
            && record["interface_state"][source_provider]["maintenance_observer"] == *declaration
        {
            return Ok(source_content.to_owned());
        }
        return Err(
            "existing source observer relay is not confined to its accepted terminal owner".into(),
        );
    }
    let mut labels = BTreeSet::new();
    if output_labels
        .iter()
        .any(|label| label.is_empty() || !labels.insert(label.clone()))
    {
        return Err("source provider output labels are malformed or repeated".into());
    }
    let nonterminal = output_labels
        .iter()
        .filter(|label| {
            *label != terminal_label
                && inbound.config.artifact_policy.admits(label)
                && returns
                    .iter()
                    .all(|b| b.config.artifact_policy.admits(label))
        })
        .cloned()
        .collect::<Vec<_>>();
    let mut terminal = original.clone();
    terminal["name"] = json!(format!(
        "{}__memory_terminal",
        original["name"]
            .as_str()
            .ok_or("source output template has no name")?
    ));
    terminal["interface_binding"] = json!(names_binding(
        source_provider,
        &[terminal_label.to_owned()]
    )?);
    terminal["outbound_interface_bindings"]
        .as_array_mut()
        .ok_or("source returns absent")?
        .push(binding);
    let mut replacements = vec![terminal];
    if !nonterminal.is_empty() {
        let mut recorded = original.clone();
        recorded["name"] = json!(format!(
            "{}__memory_recorded",
            original["name"]
                .as_str()
                .ok_or("source output template has no name")?
        ));
        recorded["interface_binding"] = json!(names_binding(source_provider, &nonterminal)?);
        replacements.push(recorded);
    }
    templates.remove(*index);
    templates.extend(replacements.clone());
    let checkpoints = record["checkpoints"]
        .as_array_mut()
        .ok_or("source checkpoints absent")?;
    let matches = checkpoints
        .iter()
        .enumerate()
        .filter(|(_, cp)| {
            cp["interface_binding"] == original["interface_binding"]
                && cp["outbound_interface_bindings"] == original["outbound_interface_bindings"]
                && parsed
                    .active_checkpoints
                    .iter()
                    .any(|id| cp["id"] == id.as_str())
        })
        .map(|(i, _)| i)
        .collect::<Vec<_>>();
    let [index] = matches.as_slice() else {
        return Err(
            "source terminal must have one exact active checkpoint matching its output template"
                .into(),
        );
    };
    let old = checkpoints.remove(*index);
    let old_id = old["id"]
        .as_str()
        .ok_or("source output checkpoint has no identity")?
        .to_owned();
    let new_ids = replacements
        .iter()
        .map(|template| {
            let seed = serde_json::to_vec(&json!([old_id, observer_run, template["name"]]))
                .map_err(|e| e.to_string())?;
            Ok(format!("cp_memory_{}", &blake3::hash(&seed).to_hex()[..24]))
        })
        .collect::<Result<Vec<_>, String>>()?;
    for (template, id) in replacements.iter().zip(&new_ids) {
        let mut cp = old.clone();
        cp["id"] = json!(id);
        cp["epoch"] = json!(0);
        cp["interface_binding"] = template["interface_binding"].clone();
        cp["outbound_interface_bindings"] = template["outbound_interface_bindings"].clone();
        checkpoints.push(cp);
    }
    replace_checkpoint_ids(&mut record["active_checkpoints"], &old_id, &new_ids)?;
    for state in record["interface_state"]
        .as_object_mut()
        .ok_or("source interface states absent")?
        .values_mut()
    {
        for name in [
            "frontier_checkpoint_ids",
            "owned_checkpoint_ids",
            "cycle_checkpoint_ids",
        ] {
            if let Some(ids) = state.get_mut(name) {
                replace_checkpoint_ids(ids, &old_id, &new_ids)?;
            }
        }
    }
    let state = record["interface_state"][source_provider]
        .as_object_mut()
        .ok_or("source provider state must be an object")?;
    match state.get("maintenance_observer") {
        Some(existing) if existing != declaration => {
            return Err("source already accepts a different immutable maintenance companion".into());
        }
        Some(_) => {}
        None => {
            state.insert("maintenance_observer".into(), declaration.clone());
        }
    }
    let content = pretty_json(&record)?;
    RunRecord::from_bytes(content.as_bytes()).map_err(|e| e.to_string())?;
    Ok(content)
}

fn replace_checkpoint_ids(ids: &mut Value, old: &str, new: &[String]) -> Result<(), String> {
    let ids = ids
        .as_array_mut()
        .ok_or("source checkpoint ownership must be an array")?;
    if let Some(index) = ids.iter().position(|id| id == old) {
        ids.remove(index);
        for (offset, id) in new.iter().enumerate() {
            ids.insert(index + offset, json!(id));
        }
    }
    Ok(())
}
