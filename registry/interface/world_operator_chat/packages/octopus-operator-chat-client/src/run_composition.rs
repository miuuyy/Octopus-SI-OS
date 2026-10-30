use super::*;
pub use octopus_app_server::{
    RegistryCandidateValidation, RegistryCandidateWrite, RegistryValidationIssue,
    RegistryValidationRoots, validate_registry_candidate,
};
use octopus_ee::EeInstance;
pub use octopus_context_collections::CollectionSelection;
use octopus_ledger::{ArtifactRef, InvocationMetadata};
use octopus_run_authoring::ProviderOutgoingBinding;
use crate::publication_selection::{
    PublicationSelectionEvidence, PublicationSelectionSeeds, PublicationSourceAuthority,
    resolve_publication_selection, validate_target_read_formation,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub mod current_run;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompositionRoots {
    pub registry_root: PathBuf,
    pub ee_instances_root: PathBuf,
    pub run_records_root: PathBuf,
    pub interface_private_state_root: PathBuf,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompositionAuthoring {
    pub source_interface_id: String,
    pub room_interface_id: String,
    pub tool_id: String,
    pub tool_call_label: String,
    pub tool_call_mark: String,
    pub tool_call_verifier: String,
    pub result_timeout_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompositionCommunication {
    #[serde(default)]
    pub client_template_interface_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_title: Option<String>,
    pub receive_interface_id: String,
    pub return_interface_ids: Vec<String>,
    pub client_interface_id: Option<String>,
    #[serde(default)]
    pub output_selection: Option<CompositionOutputSelection>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompositionOutputSelection {
    pub return_labels: Vec<String>,
    pub record_labels: Vec<String>,
}

impl CompositionOutputSelection {
    fn validate_provider_outputs(&self, provider: &InterfaceDescriptor, model_outputs: &[ModelOutputSeed]) -> Result<(), String> {
        let declared = provider.interface_driver_config.as_ref()
            .and_then(|config| config.private_config.get("client_projection_output_labels"))
            .ok_or_else(|| format!(
                "provider '{}' must declare client_projection_output_labels for output selection", provider.id,
            ))?;
        let labels: Vec<String> = serde_json::from_value(declared.clone())
            .map_err(|error| format!("provider '{}' output labels are malformed: {error}", provider.id))?;
        let mut unique = BTreeSet::new();
        if labels.is_empty() || labels.iter().any(|label| {
            label.is_empty() || label.trim() != label || !unique.insert(label)
        }) {
            return Err(format!("provider '{}' output labels must be non-empty, exact and unique", provider.id));
        }
        let mut selected = self.return_labels.iter().chain(&self.record_labels).collect::<BTreeSet<_>>();
        for output in model_outputs {
            if !selected.insert(&output.required_verified_artifact.artifact_label) {
                return Err(format!(
                    "model output '{}' carries '{}', which another provider output already owns",
                    output.checkpoint_id, output.required_verified_artifact.artifact_label,
                ));
            }
        }
        let missing = unique.difference(&selected).map(|label| label.as_str()).collect::<Vec<_>>();
        if !missing.is_empty() {
            return Err(format!(
                "provider '{}' output selection has no owner for declared outputs: {}", provider.id, missing.join(", "),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompositionSeed {
    pub label: String,
    pub payload: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunCompositionRequest {
    pub roots: CompositionRoots,
    pub metadata: InvocationMetadata,
    pub admitted_parent_record: RunRecord,
    pub identity_seed: String,
    pub ee_id: String,
    pub worker_template_interface_id: String,
    pub provider_interface_id: String,
    pub model: String,
    pub reasoning_effort: String,
    #[serde(default)]
    pub service_tier: Option<String>,
    pub authoring: CompositionAuthoring,
    pub communication: CompositionCommunication,
    pub selected_worker_environment_kinds: Vec<String>,
    pub selected_tools: Vec<SelectedToolSeed>,
    pub door_host_slots: BTreeMap<String, BTreeMap<String, PathBuf>>,
    #[serde(default)]
    pub external_inputs: Vec<ExternalInputSeed>,
    #[serde(default)]
    pub external_relays: Vec<ExternalRelaySeed>,
    #[serde(default)]
    pub model_outputs: Vec<ModelOutputSeed>,
    #[serde(default)]
    pub publications: Vec<CollectionSelection>,
    pub context_artifact_bindings: Vec<ArtifactBindingSeed>,
    pub payload_artifact_bindings: Vec<ArtifactRef>,
    pub seed: CompositionSeed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequiredPolicyUpdate {
    pub write: PlannedClientWrite,
    pub added_interface_ids: Vec<String>,
    pub added_tool_ids: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequiredBindingUpdate {
    pub run_id: String,
    pub checkpoint_id: String,
    pub expected_record_hash: Option<String>,
    pub outbound_binding: AuthoredInterfaceBinding,
    pub required_artifact_bindings: Vec<ArtifactBindingSeed>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompositionAuthoringResult {
    pub source_interface_id: String,
    pub room_interface_id: String,
    pub tool_id: String,
    pub capability_doors: BTreeMap<String, String>,
    pub capability_tools: BTreeMap<String, String>,
    pub required_write_paths: Vec<PathBuf>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlannedCompositionSeed {
    pub interface_id: String,
    pub label: String,
    pub payload: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunCompositionPlan {
    pub run_id: String,
    pub worker_interface_id: String,
    pub provider_interface_id: String,
    pub receive_interface_id: String,
    pub return_interface_ids: Vec<String>,
    pub model: String,
    pub reasoning_effort: String,
    #[serde(default)]
    pub service_tier: Option<String>,
    pub descriptor_writes: Vec<PlannedClientWrite>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub run_writes: Vec<PlannedClientWrite>,
    pub run_write: PlannedClientWrite,
    pub required_interface_ids: Vec<String>,
    pub required_tool_ids: Vec<String>,
    pub required_policy_updates: Vec<RequiredPolicyUpdate>,
    pub required_binding_updates: Vec<RequiredBindingUpdate>,
    pub authoring: CompositionAuthoringResult,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub publication_evidence: Vec<PublicationSelectionEvidence>,
    pub seed: PlannedCompositionSeed,
}

/// Exact authored files are the contract, including every checkpoint, wait,
/// result route, artifact right and verifier parameter. This mode makes no
/// template choices and adds no run fields or descriptor defaults.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthoredRunComposition {
    pub run_id: String,
    pub writes: Vec<RegistryCandidateWrite>,
    pub outgoing_bindings: Vec<ProviderOutgoingBinding>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub publication_read_refs: Vec<ArtifactRef>,
    #[serde(default)]
    pub seed: Option<PlannedCompositionSeed>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthoredRunCompositionPlan {
    pub authored: AuthoredRunComposition,
    pub run: RunRecord,
    pub validation: RegistryCandidateValidation,
}

pub fn validation_roots(roots: &CompositionRoots) -> RegistryValidationRoots {
    RegistryValidationRoots {
        registry_root: roots.registry_root.clone(),
        ee_instances_root: roots.ee_instances_root.clone(),
        run_records_root: roots.run_records_root.clone(),
    }
}

pub fn composition_candidate_writes(plan: &RunCompositionPlan) -> Vec<RegistryCandidateWrite> {
    plan.descriptor_writes
        .iter()
        .chain(
            plan.required_policy_updates
                .iter()
                .map(|update| &update.write),
        )
        .chain(&plan.run_writes)
        .chain(std::iter::once(&plan.run_write))
        .map(|write| RegistryCandidateWrite {
            path: write.path.clone(),
            content: write.content.clone(),
            expected_before_hash: write.expected_before_hash.clone(),
        })
        .collect()
}

/// Preview and application share the same exact plan. Caller connection changes
/// are returned to the provider's existing fresh-generation outgoing owner;
/// they are never a direct rewrite of an in-flight caller record.
pub fn plan_authored_run_composition(
    roots: &CompositionRoots,
    source: &RunRecord,
    authored: AuthoredRunComposition,
    revise: bool,
) -> Result<AuthoredRunCompositionPlan, String> {
    safe_segment(&authored.run_id, "authored run id")?;
    if authored.writes.is_empty() {
        return Err("authored run requires exact file writes".into());
    }
    let path = roots
        .run_records_root
        .join(format!("{}.json", authored.run_id));
    let run_write = authored
        .writes
        .iter()
        .find(|write| write.path == path)
        .ok_or("authored run requires its complete exact run-record file")?;
    let run =
        RunRecord::from_bytes(run_write.content.as_bytes()).map_err(|error| error.to_string())?;
    if run.id.as_str() != authored.run_id {
        return Err("authored run id differs from its record".into());
    }
    if run.id == source.id {
        return Err("in-flight caller edits must use its checked provider generation owner".into());
    }
    let existing = match std::fs::read(&path) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.to_string()),
    };
    if revise != existing.is_some() {
        return Err(if revise {
            "revise_run requires an existing target run"
        } else {
            "create_run requires a new run identity"
        }
        .into());
    }
    for write in &authored.writes {
        if write.path == roots.run_records_root.join(format!("{}.json", source.id)) {
            return Err("authored graph cannot rewrite its in-flight caller".into());
        }
        if write.path.parent() == Some(roots.run_records_root.as_path()) {
            let after = RunRecord::from_bytes(write.content.as_bytes())
                .map_err(|error| error.to_string())?;
            if write.path.file_stem().and_then(|id| id.to_str()) != Some(after.id.as_str()) {
                return Err("authored graph run identity differs from its filename".into());
            }
            match std::fs::read(&write.path) {
                Ok(before) => crate::run_revision::validate_checkpoint_extension(
                    &before,
                    write.content.as_bytes(),
                )?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.to_string()),
            }
        }
    }
    let mut connections = BTreeSet::new();
    for binding in &authored.outgoing_bindings {
        safe_segment(binding.interface.as_str(), "authored connection interface")?;
        if binding.artifact_label.is_empty()
            || binding.artifact_label.trim() != binding.artifact_label
            || !connections.insert((binding.interface.clone(), binding.artifact_label.clone()))
        {
            return Err(
                "authored outgoing bindings must have exact unique interface/label pairs".into(),
            );
        }
        let descriptor_path = roots
            .registry_root
            .join("interface")
            .join(binding.interface.as_str())
            .join("interface.json");
        let bytes = authored
            .writes
            .iter()
            .find(|write| write.path == descriptor_path)
            .map(|write| Ok(write.content.as_bytes().to_vec()))
            .unwrap_or_else(|| {
                std::fs::read(&descriptor_path).map_err(|error| error.to_string())
            })?;
        let descriptor =
            parse_strict::<InterfaceDescriptor>(&bytes).map_err(|error| error.to_string())?;
        if descriptor.id != binding.interface
            || descriptor.interface_type != octopus_interfaces::InterfaceType::System
        {
            return Err("authored connection must address an exact System interface".into());
        }
    }
    if authored.seed.is_none() && !authored.outgoing_bindings.is_empty() {
        return Err("caller connection publication requires an explicit seed; a parked run without emission declares no caller connections".into());
    }
    if let Some(seed) = &authored.seed {
        if seed.label.is_empty()
            || seed.label.trim() != seed.label
            || !authored.outgoing_bindings.iter().any(|binding| {
                binding.interface.as_str() == seed.interface_id
                    && binding.artifact_label == seed.label
            })
        {
            return Err(
                "authored seed requires an explicitly declared exact outgoing interface/label"
                    .into(),
            );
        }
        if !run
            .active_checkpoints
            .iter()
            .filter_map(|id| run.checkpoint(id))
            .any(|checkpoint| {
                checkpoint
                    .inbound_interface()
                    .is_some_and(|interface| interface.as_str() == seed.interface_id)
            })
        {
            return Err("authored seed has no active checkpoint on the target run".into());
        }
    }
    let records = authored.writes.iter()
        .filter(|write| write.path.parent() == Some(roots.run_records_root.as_path()))
        .map(|write| RunRecord::from_bytes(write.content.as_bytes()).map_err(|error| error.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    let validation = octopus_app_server::validate_registry_candidate_with_inspector(
        &validation_roots(roots), &authored.writes, |index, view| {
            if index != authored.writes.len() { return Ok(()); }
            for record in &records {
                let interfaces = record.checkpoints.iter()
                    .filter(|checkpoint| record.active_checkpoints.contains(&checkpoint.id))
                    .filter_map(|checkpoint| checkpoint.inbound_interface())
                    .collect::<BTreeSet<_>>();
                for interface in interfaces {
                    let descriptor = load_interface_descriptor(&view.roots.registry_root.join("interface"), interface.as_str())?;
                    if descriptor.role.0 != "provider" { continue; }
                    let Some(profile) = descriptor.interface_driver_config.as_ref()
                        .and_then(|config| config.private_config.get("run_authoring_profile")) else { continue; };
                    let profile = serde_json::from_value(profile.clone()).map_err(|error| format!("provider '{interface}' authoring profile is invalid: {error}"))?;
                    octopus_run_authoring::declared_provider_run_writer(record, interface, &profile)?;
                }
            }
            Ok(())
        },
    )?;
    Ok(AuthoredRunCompositionPlan {
        authored,
        run,
        validation,
    })
}

/// Interface-owned authoring compiles ordinary descriptors and run bindings.
/// Execution remains an admitted file-tool effect followed by artifact.send.
pub fn plan_run_composition(request: &RunCompositionRequest) -> Result<RunCompositionPlan, String> {
    validate_request(request)?;
    if !request.publications.is_empty() {
        validate_target_read_formation(
            &request.roots.registry_root,
            &request.roots.ee_instances_root,
            &request.ee_id,
            &request.worker_template_interface_id,
        )?;
    }
    let resolved = resolve_publication_selection(
        &request.roots.registry_root,
        &octopus_context_collections::ScopeContext {
            ee_id: request.ee_id.clone(),
            worker_interface: request.communication.receive_interface_id.clone(),
        },
        &request.publications,
        PublicationSelectionSeeds {
            selected_tools: request.selected_tools.clone(),
            selected_worker_environment_kinds: request.selected_worker_environment_kinds.clone(),
            door_host_slots: request.door_host_slots.clone(),
            external_inputs: request.external_inputs.clone(),
            external_relays: request.external_relays.clone(),
            model_outputs: request.model_outputs.clone(),
            context_artifact_bindings: request.context_artifact_bindings.clone(),
            payload_artifact_bindings: request.payload_artifact_bindings.clone(),
        },
        PublicationSourceAuthority::Native { metadata: &request.metadata },
        WriteHashAlgorithm::Blake3,
    )?;
    let mut expanded_request = request.clone();
    expanded_request.selected_tools = resolved.selection.selected_tools;
    expanded_request.selected_worker_environment_kinds = resolved.selection.selected_worker_environment_kinds;
    expanded_request.door_host_slots = resolved.selection.door_host_slots;
    expanded_request.external_inputs = resolved.selection.external_inputs;
    expanded_request.external_relays = resolved.selection.external_relays;
    expanded_request.model_outputs = resolved.selection.model_outputs;
    expanded_request.context_artifact_bindings = resolved.selection.context_artifact_bindings;
    expanded_request.payload_artifact_bindings = resolved.selection.payload_artifact_bindings;
    let request = &expanded_request;
    validate_request(request)?;
    let roots = &request.roots;
    let interface_root = roots.registry_root.join("interface");
    let parent = &request.admitted_parent_record;
    let runtime = request
        .metadata
        .runtime
        .as_ref()
        .ok_or("composition requires runtime metadata")?;
    let suffix = blake3::hash(
        &serde_json::to_vec(&json!([
            "octopus.run.composition.v1",
            parent.id,
            request.identity_seed
        ]))
        .map_err(|e| e.to_string())?,
    )
    .to_hex()
    .to_string();
    let suffix = &suffix[..24];
    let chain = resolve_live_ee_policy_chain(&roots.ee_instances_root, &EeId::new(&request.ee_id))
        .map_err(|e| e.to_string())?;
    let capabilities = CapabilityRegistry::load(&roots.registry_root);
    let mut topology = capabilities
        .form(&CapabilityFormationRequest::from_live_chain(
            InterfaceId::new(&request.worker_template_interface_id),
            chain.iter().cloned(),
        ))
        .map_err(|e| e.to_string())?;
    let provider = resolve_provider(
        &interface_root,
        &request.provider_interface_id,
        &request.model,
        &request.reasoning_effort,
        request.service_tier.as_deref(),
        &topology,
    )?;
    if let Some(selection) = &request.communication.output_selection {
        selection.validate_provider_outputs(&provider.interface, &request.model_outputs)?;
    }
    let mut config = composition_config(request);
    if let Some(template) = &request.communication.client_template_interface_id {
        let client = load_named_client_config(&interface_root, template)?;
        if !topology.interface_policies.contains_key(&InterfaceId::new(template)) {
            return Err("client template is outside selected worker/EE policy".into());
        }
        config.interface = client.interface;
        config.artifact_labels = client.artifact_labels;
        config.run.interface_prefix = client.run.interface_prefix;
        config.run.run_prefix = client.run.run_prefix;
        config.run.listen_env = client.run.listen_env;
        config.run.context_artifact_bindings = client.run.context_artifact_bindings;
        config.run.tool_result_receiving = client.run.tool_result_receiving;
        if let Some(observer) = client.run._rest.get("maintenance_observer") {
            config.run._rest.insert("maintenance_observer".into(), observer.clone());
        }
    }
    let owns_client = request.communication.client_template_interface_id.is_some();
    topology = select_capability_topology(
        topology,
        &config,
        &request.selected_worker_environment_kinds,
        &request.selected_tools,
    )?;
    let selected_kinds = if request.selected_tools.is_empty() {
        request.selected_worker_environment_kinds.as_slice()
    } else {
        for tool in &request.selected_tools {
            if !request.selected_worker_environment_kinds.is_empty()
                && !request.selected_worker_environment_kinds.contains(&tool.worker_environment_kind)
            {
                return Err(format!("selected tool '{}' is outside the selected worker environment kinds", tool.tool));
            }
        }
        topology = select_capability_topology(topology, &config, &[], &request.selected_tools)?;
        &[]
    };
    let mut external = validate_external_routes(
        &request.external_inputs,
        &request.external_relays,
        &request.model_outputs,
    )?;
    ensure_external_interfaces_admitted_by_ee(&topology, &external)?;
    external.extend(request.communication.return_interface_ids.iter().cloned());
    external.extend(request.communication.client_interface_id.iter().cloned());
    external.sort();
    external.dedup();
    let connected = external.iter().map(String::as_str).collect::<BTreeSet<_>>();
    topology.worker_relays.retain(|id, _| connected.contains(id.as_str()));
    let selection = RunSelection {
        ee_id: request.ee_id.clone(),
        worker_choice_interface: request.worker_template_interface_id.clone(),
        provider_choice_interface: request.provider_interface_id.clone(),
        provider_runtime_interface: None,
        run_id: Some(if owns_client { format!("{}{suffix}", config.run.run_prefix) } else { format!("run_{suffix}") }),
        chat_interface_id: Some(if owns_client {
            format!("{}{suffix}", config.run.interface_prefix)
        } else {
            request
                .communication
                .client_interface_id
                .clone()
                .unwrap_or_else(|| request.communication.receive_interface_id.clone())
        }),
        worker_runtime_interface: Some(request.communication.receive_interface_id.clone()),
        surface_role: if owns_client { "task".into() } else { String::new() },
    };
    let mut identity = plan_identity(&config, suffix, &provider, &topology, &selection)?;
    let mut communication = request.communication.clone();
    if owns_client { communication.client_interface_id = Some(identity.chat_interface_id.clone()); }
    identity.native_communication = Some(communication);
    preserve_selected_file_writer(&config, suffix, &topology, &request.selected_tools,
        selected_kinds, &mut identity)?;
    let source =
        load_interface_descriptor(&interface_root, &request.authoring.source_interface_id)?;
    if !topology.interface_policies.contains_key(&source.id) {
        return Err("authoring source interface is outside selected worker/EE policy".into());
    }
    for id in &external {
        let descriptor = load_interface_descriptor(&interface_root, id)?;
        if request.communication.return_interface_ids.contains(id)
            && descriptor.interface_type != octopus_interfaces::InterfaceType::System
        {
            return Err(format!(
                "return interface '{id}' must be an exact System interface"
            ));
        }
    }
    let mut selected_context = context_bindings::declared_context_bindings(
        &roots.registry_root,
        topology
            .tool_bindings
            .keys()
            .cloned()
            .chain(external.iter().map(InterfaceId::new))
            .chain(std::iter::once(provider.interface.id.clone())),
        &request.context_artifact_bindings,
    )?;
    for binding in context_bindings::declared_context_bindings(
        &roots.registry_root,
        std::iter::empty(),
        &config.run.context_artifact_bindings,
    )? {
        if !selected_context.iter().any(|existing| existing.id == binding.id) {
            selected_context.push(binding);
        }
    }
    plan_code_mode_owners(
        &roots.registry_root,
        &config,
        &topology,
        &mut identity,
        &selected_context,
        None,
    )?;
    validate_private_identity(request, &identity)?;
    let mut writes = resolved.artifact_writes;
    if owns_client {
        let profile = OctopusOsAuthoringProfile {
            ee_id: request.ee_id.clone(), worker_choice_interface: request.worker_template_interface_id.clone(),
            door_host_slots: request.door_host_slots.clone(), external_inputs: request.external_inputs.clone(),
            external_relays: request.external_relays.clone(), model_outputs: request.model_outputs.clone(),
            context_artifact_bindings: request.context_artifact_bindings.clone(), payload_artifact_bindings: request.payload_artifact_bindings.clone(),
            selected_worker_environment_kinds: request.selected_worker_environment_kinds.clone(), selected_tools: request.selected_tools.clone(),
            publication_evidence: resolved.evidence.clone(),
        };
        plan_chat_interface(&interface_root, &config, &identity, Some(&profile), false, &mut writes)?;
    }
    write_code_mode_owner_artifacts(&roots.registry_root, &identity, &mut writes)?;
    plan_capability_doors(
        &roots.registry_root,
        &interface_root,
        &config,
        &topology,
        &identity,
        &request.door_host_slots,
        &capability_plan::project_source_folders(&roots.ee_instances_root, &identity.ee_id)?,
        &mut writes,
    )?;
    plan_authoring_interfaces(&interface_root, &config, &topology, &identity, &mut writes)?;
    plan_independent_code_mode_authoring(
        &roots.registry_root,
        &config,
        &topology,
        &identity,
        &mut writes,
    )?;
    plan_private_tools(
        &roots.registry_root,
        &config,
        &topology,
        &identity,
        &mut writes,
    )?;
    let private_roles = plan_worker_pin(
        &interface_root,
        &config,
        &request.worker_template_interface_id,
        &topology,
        &identity,
        &external,
        None,
        &mut writes,
    )?;
    let dependency_workers =
        worker_role_dependency_interfaces(&interface_root, topology.worker_relays.keys(), &external)?;
    let roles =
        formed_worker_role_artifacts(&interface_root, &private_roles, dependency_workers.iter())?;
    let mut selected_context = context_bindings::with_instruction_source_bindings_from_planned(
        &roots.registry_root,
        roles
            .iter()
            .cloned()
            .chain(config.run.context_artifact_bindings.iter().map(|binding| binding.id.clone()))
            .chain(selected_context.iter().map(|binding| binding.id.clone())),
        &selected_context,
        &resolved.staged_artifacts,
    )?;
    let operations = OperationRoutes::load(&roots.registry_root, &request.external_inputs, std::iter::empty())?;
    for binding in operations.catalog_bindings() {
        if !selected_context.contains(&binding) { selected_context.push(binding); }
    }
    let catalogue = collection_context::plan(collection_context::PlanContext {
        registry_root: &roots.registry_root, ee_instances_root: &roots.ee_instances_root,
        ee_id: &request.ee_id, identity: &identity, bindings: selected_context.clone(),
        native_metadata: Some(&request.metadata), existing: None,
    }, &mut selected_context, &mut writes)?;
    plan_provider_interface(
        &interface_root,
        &config,
        &identity,
        &provider,
        &topology,
        &private_roles,
        &catalogue.inline_ids(),
        Some(&roots.interface_private_state_root),
        &mut writes,
    )?;
    if let Some(binding) = native_run_authoring::plan(
        native_run_authoring::PlanContext {
            registry_root: &roots.registry_root,
            ee_instances_root: &roots.ee_instances_root,
            run_records_root: Some(&roots.run_records_root),
            interface_private_state_root: Some(&roots.interface_private_state_root),
            config: &config,
            identity: &identity,
            provider: &provider,
            topology: &topology,
            door_host_slots: &request.door_host_slots,
        },
        &mut writes,
    )? {
        selected_context.push(binding);
    }
    selected_context.push(home_outputs::plan_task_coordinates(
        &roots.registry_root,
        &roots.ee_instances_root,
        Some(&roots.run_records_root),
        &identity,
        &mut writes,
    )?);
    let mut required_interface_ids = exact_topology_interface_ids(&identity, &external);
    let mut required_tool_ids = identity
        .capability_tools
        .values()
        .cloned()
        .chain(std::iter::once(identity.authoring_tool_id.clone()))
        .chain(
            identity
                .code_mode_owners
                .values()
                .map(|owner| owner.authoring_tool.clone()),
        )
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let interface_mapping = private_interface_mapping(&config, &identity);
    preserve_formed_private_policies(request, &topology, &interface_mapping, &mut writes)?;
    validate_context_and_marks(request, &chain, &roles, &selected_context, config.run.tool_result_receiving.as_ref())?;
    validate_new_descriptor_targets(&writes)?;
    let run_content = build_chat_run_record(
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
        &request.external_inputs,
        &request.external_relays,
        &request.model_outputs,
        &selected_context,
        &request.payload_artifact_bindings,
        &provider.client_operation_request_labels,
        None,
        &operations,
    )?;
    let run_content = catalogue.wire(&identity.model_interface_id, run_content)?;
    let (run_content, intake) = input_intake::wire(&roots.registry_root, &identity, &mut writes, run_content)?;
    let run_content = context_bindings::configure_tool_context(&roots.registry_root, &identity, run_content, None)?;
    let receive_interface_id = if let Some(intake) = intake {
        required_interface_ids.extend([intake.interface.clone(), intake.door]);
        required_tool_ids.push(intake.tool);
        required_interface_ids.sort();
        required_interface_ids.dedup();
        required_tool_ids.sort();
        required_tool_ids.dedup();
        intake.interface
    } else { identity.worker_interface_id.clone() };
    let (run_content, mut run_writes) = maintenance_attachment::wire(
        maintenance_attachment::AttachmentRequest {
            capabilities: &capabilities,
            ee_instances_root: &roots.ee_instances_root,
            run_records_root: Some(&roots.run_records_root),
            interface_private_state_root: Some(&roots.interface_private_state_root),
            config: &config,
            identity: &identity,
            source_input_labels: vec![request.seed.label.clone(), config.artifact_labels.message.clone()],
            existing: None,
            write_hash_algorithm: WriteHashAlgorithm::Blake3,
        },
        run_content,
        &mut writes,
        &mut required_interface_ids,
        &mut required_tool_ids,
    )?;
    let run_content = if owns_client {
        let (content, mut writers) = writer_runs::wire_new_record(
            &roots.run_records_root, &identity.run_id, run_content,
        )?;
        run_writes.append(&mut writers);
        content
    } else {
        run_content
    };
    validate_new_descriptor_targets(&writes)?;
    normalize_write_hash_algorithm(&mut writes, WriteHashAlgorithm::Blake3)?;
    let run_write = checked_new_write(
        roots
            .run_records_root
            .join(format!("{}.json", identity.run_id)),
        run_content,
        format!("run:{}", identity.run_id),
    )?;
    let mut updates = BTreeMap::new();
    let planned_descriptors = writes.iter().chain(&run_writes).collect::<Vec<_>>();
    require_ee_admission(
        roots,
        &request.ee_id,
        &required_interface_ids,
        &required_tool_ids,
        &planned_descriptors,
    )?;
    require_ee_admission(
        roots,
        parent.ee.as_str(),
        &[receive_interface_id.clone()],
        &[],
        &planned_descriptors,
    )?;
    plan_policy_requirement(
        interface_root
            .join(parent.worker.as_str())
            .join("policy.json"),
        "/ceiling/interfaces",
        "/ceiling/tools",
        &[receive_interface_id.clone()],
        &[],
        &mut updates,
    )?;
    let required_policy_updates = finish_policy_updates(updates)?;
    let checkpoint = parent
        .checkpoint(&runtime.checkpoint)
        .ok_or("metadata checkpoint is absent from parent record")?;
    let existing_binding = checkpoint
        .outbound_interface_bindings
        .iter()
        .find(|binding| binding.interface.as_str() == receive_interface_id);
    if existing_binding
        .is_some_and(|binding| !binding.config.artifact_policy.admits(&request.seed.label))
    {
        return Err("existing caller binding does not admit the requested seed label".into());
    }
    let caller_roles = private_roles
        .iter()
        .map(|id| ArtifactBindingSeed { id: id.clone() })
        .collect::<Vec<_>>();
    let required_artifact_bindings = context_bindings::with_instruction_source_bindings(
        &roots.registry_root,
        private_roles.into_iter(),
        &caller_roles,
    )?
    .into_iter()
    .filter(|required| {
        !checkpoint
            .artifact_bindings
            .iter()
            .any(|bound| bound.id.as_str() == required.id && bound.access.read)
    })
    .collect::<Vec<_>>();
    let required_binding_updates =
        if existing_binding.is_some() && required_artifact_bindings.is_empty() {
            Vec::new()
        } else {
            vec![RequiredBindingUpdate {
                run_id: parent.id.as_str().to_string(),
                checkpoint_id: runtime.checkpoint.as_str().to_string(),
                expected_record_hash: runtime.run_record_hash.clone(),
                outbound_binding: existing_binding.cloned().unwrap_or_else(|| {
                    AuthoredInterfaceBinding::all(InterfaceId::new(&receive_interface_id))
                }),
                required_artifact_bindings,
            }]
        };
    let required_write_paths = writes
        .iter()
        .chain(&run_writes)
        .map(|write| write.path.clone())
        .chain(
            required_policy_updates
                .iter()
                .map(|update| update.write.path.clone()),
        )
        .chain(std::iter::once(run_write.path.clone()))
        .chain(
            (!required_binding_updates.is_empty())
                .then(|| roots.run_records_root.join(format!("{}.json", parent.id))),
        )
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    Ok(RunCompositionPlan {
        run_id: identity.run_id,
        worker_interface_id: identity.worker_interface_id.clone(),
        provider_interface_id: identity.model_interface_id,
        receive_interface_id: receive_interface_id.clone(),
        return_interface_ids: request.communication.return_interface_ids.clone(),
        model: provider.model,
        reasoning_effort: provider.reasoning_effort,
        service_tier: provider.service_tier,
        descriptor_writes: writes,
        run_writes,
        run_write,
        required_interface_ids,
        required_tool_ids,
        required_policy_updates,
        required_binding_updates,
        publication_evidence: resolved.evidence,
        authoring: CompositionAuthoringResult {
            source_interface_id: identity.authoring_source_id,
            room_interface_id: identity.authoring_environment_id,
            tool_id: identity.authoring_tool_id,
            capability_doors: identity.capability_doors,
            capability_tools: identity.capability_tools,
            required_write_paths,
        },
        seed: PlannedCompositionSeed {
            interface_id: receive_interface_id,
            label: request.seed.label.clone(),
            payload: request.seed.payload.clone(),
        },
    })
}

fn composition_config(request: &RunCompositionRequest) -> ClientConfig {
    ClientConfig {
        home_voice: None,
        result_receiving_updates: Vec::new(),
        schema_version: 2,
        interface: request
            .communication
            .client_interface_id
            .clone()
            .unwrap_or_else(|| request.communication.receive_interface_id.clone()),
        artifact_labels: ClientArtifactLabels {
            message: request.seed.label.clone(),
            turn_control: request.seed.label.clone(),
            tool_call: request.authoring.tool_call_label.clone(),
            _rest: Default::default(),
        },
        authoring: ClientAuthoringConfig {
            source_interface: request.authoring.source_interface_id.clone(),
            room_interface: request.authoring.room_interface_id.clone(),
            tool: request.authoring.tool_id.clone(),
            result_timeout_ms: request.authoring.result_timeout_ms,
            _rest: Default::default(),
        },
        run: ClientRunConfig {
            interface_prefix: String::new(),
            run_prefix: String::new(),
            ee: request.ee_id.clone(),
            worker: request.worker_template_interface_id.clone(),
            tool_call_mark: request.authoring.tool_call_mark.clone(),
            tool_call_verifier: request.authoring.tool_call_verifier.clone(),
            tool_result_receiving: None,
            listen_env: String::new(),
            context_artifact_bindings: Vec::new(),
            _rest: Default::default(),
        },
    }
}

fn validate_request(request: &RunCompositionRequest) -> Result<(), String> {
    if let Some(title) = &request.communication.client_title {
        if request.communication.client_template_interface_id.is_none()
            || title.is_empty() || title.trim() != title
            || title.chars().count() > 80 || title.chars().any(char::is_control)
        {
            return Err("client_title requires a new client and an exact, nonempty display name of at most 80 characters without control characters".into());
        }
    }
    if let Some(template) = &request.communication.client_template_interface_id {
        safe_segment(template, "client template")?;
        if request.communication.client_interface_id.is_some() {
            return Err("select a new client template or an existing client interface, not both".into());
        }
    }
    if !request.model_outputs.is_empty() && request.communication.output_selection.is_none() {
        return Err("model outputs require an output selection, so the task's return output does not also carry their labels".into());
    }
    if let Some(outputs) = &request.communication.output_selection {
        let mut names = BTreeSet::new();
        if outputs.return_labels.is_empty() || outputs.record_labels.is_empty() {
            return Err("output selection requires explicit return and record labels".into());
        }
        for label in outputs.return_labels.iter().chain(&outputs.record_labels) {
            if label.is_empty() || label.trim() != label || !names.insert(label) {
                return Err("output selection labels must be non-empty, exact and disjoint".into());
            }
        }
    }
    for (name, path) in [
        ("registry_root", &request.roots.registry_root),
        ("ee_instances_root", &request.roots.ee_instances_root),
        ("run_records_root", &request.roots.run_records_root),
        (
            "interface_private_state_root",
            &request.roots.interface_private_state_root,
        ),
    ] {
        require_absolute(name, path)?;
        if path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
        {
            return Err(format!("{name} must not contain parent traversal"));
        }
    }
    for (name, value) in [
        ("ee_id", &request.ee_id),
        (
            "worker_template_interface_id",
            &request.worker_template_interface_id,
        ),
        ("provider_interface_id", &request.provider_interface_id),
        (
            "receive_interface_id",
            &request.communication.receive_interface_id,
        ),
        (
            "authoring.source_interface_id",
            &request.authoring.source_interface_id,
        ),
        (
            "authoring.room_interface_id",
            &request.authoring.room_interface_id,
        ),
    ] {
        if safe_segment(value, name)? != *value {
            return Err(format!("{name} must be exact"));
        }
    }
    for (name, value) in [
        ("tool_id", &request.authoring.tool_id),
        ("tool_call_label", &request.authoring.tool_call_label),
        ("tool_call_mark", &request.authoring.tool_call_mark),
        ("tool_call_verifier", &request.authoring.tool_call_verifier),
    ] {
        safe_policy_axis_value(value, name)?;
    }
    for (name, value) in [
        ("identity_seed", &request.identity_seed),
        ("model", &request.model),
        ("seed.label", &request.seed.label),
    ] {
        if value.is_empty() || value.trim() != value || value.contains(['\r', '\n', '\0']) {
            return Err(format!("{name} must be a non-empty exact string"));
        }
    }
    if request.reasoning_effort.trim() != request.reasoning_effort
        || request.reasoning_effort.contains(['\r', '\n', '\0'])
        || request.authoring.result_timeout_ms == 0
    {
        return Err("malformed reasoning effort or authoring timeout".into());
    }
    let parent = &request.admitted_parent_record;
    validate_run_authoring_source(&request.metadata, parent)?;
    let mut endpoints = BTreeSet::from([request.communication.receive_interface_id.clone()]);
    for id in request
        .communication
        .return_interface_ids
        .iter()
        .chain(request.communication.client_interface_id.iter())
    {
        if safe_segment(id, "communication interface")? != *id {
            return Err("communication interface must be exact".into());
        }
        if !endpoints.insert(id.clone()) {
            return Err("communication endpoints must be distinct exact interfaces".into());
        }
    }
    if request.communication.receive_interface_id == parent.worker.as_str()
        || request.communication.receive_interface_id == request.worker_template_interface_id
    {
        return Err("receive interface must be a new private worker pin".into());
    }
    for (name, value) in request.external_inputs.iter().flat_map(|input| [
        ("external_inputs.checkpoint_id", &input.checkpoint_id),
        ("external_inputs.interface_id", &input.interface_id),
    ]).chain(request.external_relays.iter().flat_map(|relay| [
        ("external_relays.checkpoint_id", &relay.checkpoint_id),
        ("external_relays.interface_id", &relay.interface_id),
    ])).chain(request.external_relays.iter().flat_map(|relay| relay.outbound_interface_ids.iter()
        .map(|id| ("external_relays.outbound_interface_ids", id))))
        .chain(request.model_outputs.iter().flat_map(|output| [
            ("model_outputs.checkpoint_id", &output.checkpoint_id),
            ("model_outputs.interface_id", &output.interface_id),
        ]))
    {
        if safe_segment(value, name)? != *value {
            return Err(format!("{name} must be exact"));
        }
    }
    Ok(())
}

pub fn validate_run_authoring_source(
    metadata: &InvocationMetadata,
    parent: &RunRecord,
) -> Result<(), String> {
    for (name, value) in [
        ("parent run", parent.id.as_str()),
        ("parent worker", parent.worker.as_str()),
    ] {
        if safe_segment(value, name)? != value {
            return Err(format!("{name} must be exact"));
        }
    }
    parent.validate_artifact_arrival_shape()?;
    let runtime = metadata
        .runtime
        .as_ref()
        .ok_or("composition requires explicit runtime metadata")?;
    if runtime.run != parent.id
        || runtime.ee != parent.ee
        || runtime.worker != parent.worker
        || !parent.active_checkpoints.contains(&runtime.checkpoint)
    {
        return Err("composition metadata and admitted parent record disagree".into());
    }
    let checkpoint = parent
        .checkpoint(&runtime.checkpoint)
        .ok_or("parent checkpoint is absent")?;
    if checkpoint.epoch.0 != runtime.epoch {
        return Err("composition metadata checkpoint epoch is stale".into());
    }
    Ok(())
}

pub(super) fn private_interface_mapping(
    config: &ClientConfig,
    identity: &PlannedIdentity,
) -> BTreeMap<String, Vec<String>> {
    let mut mapping = BTreeMap::<String, Vec<String>>::new();
    if identity.owns_client() {
        mapping.insert(config.interface.clone(), vec![identity.chat_interface_id.clone()]);
    }
    for (source, exact) in [
        (&identity.provider_choice_interface, &identity.model_interface_id),
        (
            &identity.worker_choice_interface,
            &identity.worker_interface_id,
        ),
        (
            &config.authoring.source_interface,
            &identity.authoring_source_id,
        ),
        (
            &config.authoring.room_interface,
            &identity.authoring_environment_id,
        ),
    ] {
        mapping
            .entry(source.clone())
            .or_default()
            .push(exact.clone());
    }
    for (source, exact) in &identity.capability_doors {
        mapping
            .entry(source.clone())
            .or_default()
            .push(exact.clone());
    }
    for owner in identity.code_mode_owners.values() {
        mapping
            .entry(config.authoring.room_interface.clone())
            .or_default()
            .push(owner.authoring_environment.clone());
    }
    mapping
}

fn preserve_formed_private_policies(
    request: &RunCompositionRequest,
    topology: &FormedCapabilityTopology,
    mapping: &BTreeMap<String, Vec<String>>,
    writes: &mut [PlannedClientWrite],
) -> Result<(), String> {
    for write in writes {
        if write.path.file_name().and_then(|v| v.to_str()) == Some("interface.json") {
            let mut descriptor = parse_strict::<InterfaceDescriptor>(write.content.as_bytes())
                .map_err(|e| e.to_string())?;
            if let Some((source, _)) = mapping
                .iter()
                .find(|(_, exact)| exact.iter().any(|id| id == descriptor.id.as_str()))
            {
                if let Some(policy) = topology.interface_policies.get(&InterfaceId::new(source)) {
                    descriptor.interface_policy.values = policy.context.clone();
                    write.content =
                        pretty_json(&serde_json::to_value(descriptor).map_err(|e| e.to_string())?)?;
                }
            }
        } else if write.path
            == request
                .roots
                .registry_root
                .join("interface")
                .join(&request.communication.receive_interface_id)
                .join("policy.json")
        {
            let mut policy: WorkerInterfacePolicy =
                serde_json::from_str(&write.content).map_err(|e| e.to_string())?;
            remap_private_worker_policy(&mut policy, mapping);
            write.content = pretty_json(&serde_json::to_value(policy).map_err(|e| e.to_string())?)?;
        }
    }
    Ok(())
}

pub(super) fn remap_private_worker_policy(policy: &mut WorkerInterfacePolicy, mapping: &BTreeMap<String, Vec<String>>) {
    let original = std::mem::take(&mut policy.ceiling.interface_policy);
    for (source, overrides) in original {
        if let Some(exact) = mapping.get(source.as_str()) {
            for id in exact {
                policy.ceiling.interface_policy.insert(InterfaceId::new(id), overrides.clone());
            }
        } else if policy.ceiling.interfaces.contains(&source) {
            policy.ceiling.interface_policy.insert(source, overrides);
        }
    }
}

fn validate_new_descriptor_targets(writes: &[PlannedClientWrite]) -> Result<(), String> {
    let mut paths = BTreeSet::new();
    for write in writes {
        if !paths.insert(&write.path) {
            return Err(format!(
                "composition repeats write target '{}'",
                write.path.display()
            ));
        }
        if write.expected_before_hash.is_some() {
            let bytes = std::fs::read(&write.path).map_err(|e| e.to_string())?;
            if bytes != write.content.as_bytes() {
                return Err(format!(
                    "new composition collides with existing descriptor '{}'",
                    write.path.display()
                ));
            }
        }
    }
    Ok(())
}

fn checked_new_write(
    path: PathBuf,
    content: String,
    tag: String,
) -> Result<PlannedClientWrite, String> {
    let expected_before_hash = match std::fs::read(&path) {
        Ok(bytes) if bytes == content.as_bytes() => Some(blake3::hash(&bytes).to_hex().to_string()),
        Ok(_) => {
            return Err(format!(
                "new composition collides with existing run '{}'",
                path.display()
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(format!(
                "composition target '{}' cannot be read: {error}",
                path.display()
            ));
        }
    };
    Ok(PlannedClientWrite {
        path,
        content,
        tag,
        expected_before_hash,
    })
}

struct PolicyDraft {
    before: Vec<u8>,
    value: Value,
    interfaces: BTreeSet<String>,
    tools: BTreeSet<String>,
}

fn plan_policy_requirement(
    path: PathBuf,
    interface_pointer: &str,
    tool_pointer: &str,
    interfaces: &[String],
    tools: &[String],
    drafts: &mut BTreeMap<PathBuf, PolicyDraft>,
) -> Result<(), String> {
    if !drafts.contains_key(&path) {
        let before = std::fs::read(&path)
            .map_err(|e| format!("policy '{}' cannot be read: {e}", path.display()))?;
        let value = serde_json::from_slice(&before)
            .map_err(|e| format!("policy '{}' is malformed: {e}", path.display()))?;
        drafts.insert(
            path.clone(),
            PolicyDraft {
                before,
                value,
                interfaces: BTreeSet::new(),
                tools: BTreeSet::new(),
            },
        );
    }
    let draft = drafts.get_mut(&path).ok_or("policy draft disappeared")?;
    if interface_pointer.starts_with("/ceiling/") {
        let ceiling = draft
            .value
            .get_mut("ceiling")
            .and_then(Value::as_object_mut)
            .ok_or("worker ceiling is absent")?;
        for key in ["interfaces", "tools"] {
            ceiling.entry(key).or_insert_with(|| json!([]));
        }
    }
    for (pointer, required, added) in [
        (interface_pointer, interfaces, &mut draft.interfaces),
        (tool_pointer, tools, &mut draft.tools),
    ] {
        let Some(value) = draft.value.pointer_mut(pointer) else {
            continue;
        };
        if value.is_null() {
            continue;
        }
        let array = value
            .as_array_mut()
            .ok_or_else(|| format!("policy '{}' {pointer} must be an array", path.display()))?;
        let mut allowed = array
            .iter()
            .map(|entry| {
                entry.as_str().map(str::to_string).ok_or_else(|| {
                    format!(
                        "policy '{}' {pointer} contains a non-string",
                        path.display()
                    )
                })
            })
            .collect::<Result<BTreeSet<_>, _>>()?;
        for id in required {
            if allowed.insert(id.clone()) {
                array.push(json!(id));
                added.insert(id.clone());
            }
        }
    }
    Ok(())
}

fn planned_descriptor(
    registry_root: &Path,
    kind: &str,
    file: &str,
    id: &str,
    planned: &[&PlannedClientWrite],
) -> Result<Option<Value>, String> {
    let path = registry_root.join(kind).join(id).join(file);
    match planned.iter().rev().find(|write| write.path == path) {
        Some(write) => serde_json::from_str(&write.content).map(Some).map_err(|e| e.to_string()),
        None => match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|e| e.to_string()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(format!("{kind} '{id}' cannot be read: {error}")),
        },
    }
}

fn declared_original(kind: &str, id: &str, descriptor: &Value) -> Result<Option<String>, String> {
    match descriptor.get("copy_of") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(original)) => Ok(Some(original.clone())),
        Some(_) => Err(format!("{kind} '{id}' has a malformed copy_of")),
    }
}

pub(crate) fn planned_copy_lineage(
    registry_root: &Path,
    interfaces: &[String],
    tools: &[String],
    planned: &[&PlannedClientWrite],
) -> Result<octopus_policy::CopyLineage, String> {
    let mut lineage = octopus_policy::CopyLineage::default();
    for id in interfaces {
        let Some(descriptor) = planned_descriptor(registry_root, "interface", "interface.json", id, planned)? else { continue };
        if let Some(original) = declared_original("interface", id, &descriptor)? {
            if planned_descriptor(registry_root, "interface", "interface.json", &original, planned)?.is_some() {
                lineage.insert_interface(InterfaceId::new(id), InterfaceId::new(original));
            }
        }
    }
    for id in tools {
        let Some(descriptor) = planned_descriptor(registry_root, "tool", "tool.json", id, planned)? else { continue };
        let Some(original) = declared_original("tool", id, &descriptor)? else { continue };
        let copy: ToolDescriptor = serde_json::from_value(descriptor).map_err(|e| format!("tool '{id}': {e}"))?;
        let held = planned_descriptor(registry_root, "tool", "tool.json", &original, planned)?
            .map(|value| serde_json::from_value::<ToolDescriptor>(value).map_err(|e| format!("tool '{original}': {e}")))
            .transpose()?;
        if held.is_some_and(|held| copy.never_wider_than(&held)) {
            lineage.insert_tool(octopus_descriptors::ToolId::new(id), octopus_descriptors::ToolId::new(original));
        } else {
            lineage.insert_declared_tool(octopus_descriptors::ToolId::new(id), octopus_descriptors::ToolId::new(original));
        }
    }
    Ok(lineage)
}

fn require_ee_admission(
    roots: &CompositionRoots,
    ee_id: &str,
    interfaces: &[String],
    tools: &[String],
    planned: &[&PlannedClientWrite],
) -> Result<(), String> {
    let lineage = planned_copy_lineage(&roots.registry_root, interfaces, tools, planned)?;
    let mut visited = BTreeSet::new();
    let mut cursor = ee_id.to_string();
    loop {
        safe_segment(&cursor, "EE instance")?;
        if !visited.insert(cursor.clone()) {
            return Err("EE admission chain cycles".into());
        }
        let bytes = std::fs::read(roots.ee_instances_root.join(format!("{cursor}.json")))
            .map_err(|e| e.to_string())?;
        let ee: EeInstance = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        if ee.ee_id.as_str() != cursor {
            return Err("EE admission identity mismatch".into());
        }
        safe_segment(ee.template.as_str(), "EE template")?;
        let template = parse_strict::<EeTemplateDescriptor>(
            &std::fs::read(
                roots
                    .registry_root
                    .join("ee_template")
                    .join(ee.template.as_str())
                    .join("ee_template.json"),
            )
            .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        for (label, layer) in [
            (format!("EE template '{}'", template.id), template.as_policy_layer()),
            (format!("EE '{cursor}'"), ee.policy_narrowing.as_policy_layer()),
        ] {
            if let Some(allowed) = &layer.interfaces {
                for id in interfaces {
                    if !lineage.admits_interface(allowed, &InterfaceId::new(id)) {
                        return Err(format!("{label} does not admit interface '{id}' or its original"));
                    }
                }
            }
            for id in tools {
                let tool = octopus_descriptors::ToolId::new(id);
                if layer.tools.as_ref().is_some_and(|allowed| !lineage.admits_tool(allowed, &tool))
                    || lineage.denies_tool(&layer.hard_blocks_tools, &tool)
                {
                    return Err(format!("{label} does not admit tool '{id}' or its original"));
                }
            }
        }
        match ee.parent_ee {
            Some(parent) => cursor = parent.as_str().to_string(),
            None => break,
        }
    }
    Ok(())
}

fn finish_policy_updates(
    drafts: BTreeMap<PathBuf, PolicyDraft>,
) -> Result<Vec<RequiredPolicyUpdate>, String> {
    drafts
        .into_iter()
        .filter(|(_, draft)| !draft.interfaces.is_empty() || !draft.tools.is_empty())
        .map(|(path, draft)| {
            let content = pretty_json(&draft.value)?;
            match path.file_name().and_then(|name| name.to_str()) {
                Some("policy.json") => {
                    serde_json::from_str::<WorkerInterfacePolicy>(&content)
                        .map_err(|e| e.to_string())?;
                }
                Some("ee_template.json") => {
                    parse_strict::<EeTemplateDescriptor>(content.as_bytes())
                        .map_err(|e| e.to_string())?;
                }
                _ => {
                    serde_json::from_str::<EeInstance>(&content).map_err(|e| e.to_string())?;
                }
            }
            Ok(RequiredPolicyUpdate {
                write: PlannedClientWrite {
                    path,
                    content,
                    tag: "required-policy-update".into(),
                    expected_before_hash: Some(blake3::hash(&draft.before).to_hex().to_string()),
                },
                added_interface_ids: draft.interfaces.into_iter().collect(),
                added_tool_ids: draft.tools.into_iter().collect(),
            })
        })
        .collect()
}

fn validate_context_and_marks(
    request: &RunCompositionRequest,
    chain: &[octopus_world_terminal_client::LiveEePolicyLayer],
    roles: &[String],
    context: &[ArtifactBindingSeed],
    result_receiving: Option<&ToolResultReceiving>,
) -> Result<(), String> {
    let policy: WorkerInterfacePolicy = serde_json::from_slice(
        &std::fs::read(
            request
                .roots
                .registry_root
                .join("interface")
                .join(&request.worker_template_interface_id)
                .join("policy.json"),
        )
        .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    if let Some(result) = result_receiving {
        if !result_receiving_authorized(&request.roots.registry_root, chain, &policy, result)? {
            return Err("selected worker/EE policy does not admit the tool-result correspondence mark".into());
        }
    }
    let mut layers = Vec::new();
    for entry in chain {
        let template = parse_strict::<EeTemplateDescriptor>(
            &std::fs::read(
                request
                    .roots
                    .registry_root
                    .join("ee_template")
                    .join(entry.template.as_str())
                    .join("ee_template.json"),
            )
            .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        layers.extend([
            template.as_policy_layer(),
            entry.policy_narrowing.as_policy_layer(),
        ]);
    }
    let formed =
        octopus_policy::form_policy(policy.ceiling_grant(), &layers, &[], Default::default());
    if (!roles.is_empty() || !context.is_empty() || !request.payload_artifact_bindings.is_empty())
        && !formed
            .context
            .artifact_verbs
            .contains(&octopus_policy::ArtifactVerb::Read)
    {
        return Err("selected worker/EE policy does not allow reading the declared context".into());
    }
    if !formed
        .context
        .mark_outcomes
        .contains(&octopus_descriptors::MarkId::new(
            &request.authoring.tool_call_mark,
        ))
    {
        return Err("selected worker/EE policy does not admit the tool-call mark".into());
    }
    Ok(())
}

fn validate_private_identity(
    request: &RunCompositionRequest,
    identity: &PlannedIdentity,
) -> Result<(), String> {
    let mut unique = BTreeSet::new();
    for id in [
        &identity.worker_interface_id,
        &identity.model_interface_id,
        &identity.authoring_source_id,
        &identity.authoring_environment_id,
    ]
    .into_iter()
    .chain(request.communication.client_template_interface_id.as_ref().map(|_| &identity.chat_interface_id))
    .chain(identity.capability_doors.values())
    .chain(
        identity
            .code_mode_owners
            .values()
            .map(|owner| &owner.authoring_environment),
    ) {
        if !unique.insert(id) {
            return Err(format!("private composition interfaces collide at '{id}'"));
        }
        let state = request.roots.interface_private_state_root.join(id);
        match std::fs::symlink_metadata(&state) {
            Ok(_) => {
                return Err(format!(
                    "new composition would reuse existing private state '{}'",
                    state.display()
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "private state '{}' cannot be checked: {error}",
                    state.display()
                ));
            }
        }
    }
    Ok(())
}
