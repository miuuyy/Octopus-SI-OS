use super::*;
use octopus_run_authoring::capability_amendment::{ProviderCapabilityAmendment, ProviderCapabilityBridge, prepare_plan, rebase_plan, validate_tool_continuation};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CurrentRunSelection {
    pub selected_tools: Vec<SelectedToolSeed>,
    pub selected_worker_environment_kinds: Vec<String>,
    pub door_host_slots: BTreeMap<String, BTreeMap<String, PathBuf>>,
    pub external_inputs: Vec<ExternalInputSeed>,
    pub external_relays: Vec<ExternalRelaySeed>,
    pub model_outputs: Vec<ModelOutputSeed>,
    pub context_artifact_bindings: Vec<ArtifactBindingSeed>,
    pub payload_artifact_bindings: Vec<ArtifactRef>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthoredCurrentRun {
    pub expected_before_hash: String,
    pub expected_before: Value,
    pub run_record: Value,
    pub writes: Vec<RegistryCandidateWrite>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CurrentRunRequest {
    pub amendment_key: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub publications: Vec<octopus_context_collections::CollectionSelection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selection: Option<CurrentRunSelection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub change: Option<CurrentRunChange>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authored: Option<AuthoredCurrentRun>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CurrentRunSelectionEntries {
    #[serde(default)]
    pub selected_tools: Vec<SelectedToolSeed>,
    #[serde(default)]
    pub selected_worker_environment_kinds: Vec<String>,
    #[serde(default)]
    pub door_host_slots: BTreeMap<String, BTreeMap<String, PathBuf>>,
    #[serde(default)]
    pub external_inputs: Vec<ExternalInputSeed>,
    #[serde(default)]
    pub external_relays: Vec<ExternalRelaySeed>,
    #[serde(default)]
    pub model_outputs: Vec<ModelOutputSeed>,
    #[serde(default)]
    pub context_artifact_bindings: Vec<ArtifactBindingSeed>,
    #[serde(default)]
    pub payload_artifact_bindings: Vec<ArtifactRef>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CurrentRunChange {
    #[serde(default)]
    pub add: CurrentRunSelectionEntries,
    #[serde(default)]
    pub remove: CurrentRunSelectionEntries,
}

fn change_list<T: PartialEq + std::fmt::Debug>(field: &str, current: &mut Vec<T>, add: Vec<T>, remove: Vec<T>) -> Result<bool, String> {
    let changed = !add.is_empty() || !remove.is_empty();
    for entry in remove {
        let index = current.iter().position(|item| item == &entry)
            .ok_or_else(|| format!("change removes {field} entry {entry:?} that the current selection does not hold"))?;
        current.remove(index);
    }
    for entry in add {
        if current.contains(&entry) {
            return Err(format!("change adds {field} entry {entry:?} that the current selection already holds"));
        }
        current.push(entry);
    }
    Ok(changed)
}

pub fn apply_current_run_change(mut selection: CurrentRunSelection, change: CurrentRunChange) -> Result<CurrentRunSelection, String> {
    let CurrentRunChange { add, remove } = change;
    let mut changed = false;
    changed |= change_list("selected_tools", &mut selection.selected_tools, add.selected_tools, remove.selected_tools)?;
    changed |= change_list("selected_worker_environment_kinds", &mut selection.selected_worker_environment_kinds,
        add.selected_worker_environment_kinds, remove.selected_worker_environment_kinds)?;
    changed |= change_list("external_inputs", &mut selection.external_inputs, add.external_inputs, remove.external_inputs)?;
    changed |= change_list("external_relays", &mut selection.external_relays, add.external_relays, remove.external_relays)?;
    changed |= change_list("model_outputs", &mut selection.model_outputs, add.model_outputs, remove.model_outputs)?;
    changed |= change_list("context_artifact_bindings", &mut selection.context_artifact_bindings,
        add.context_artifact_bindings, remove.context_artifact_bindings)?;
    changed |= change_list("payload_artifact_bindings", &mut selection.payload_artifact_bindings,
        add.payload_artifact_bindings, remove.payload_artifact_bindings)?;
    for (door, slots) in remove.door_host_slots {
        changed = true;
        let held = selection.door_host_slots.get_mut(&door)
            .ok_or_else(|| format!("change removes host slots of door '{door}' that the current selection does not hold"))?;
        for (slot, path) in slots {
            if held.get(&slot) != Some(&path) {
                return Err(format!("change removes host slot '{slot}' of door '{door}' that the current selection does not hold at that path"));
            }
            held.remove(&slot);
        }
        if held.is_empty() { selection.door_host_slots.remove(&door); }
    }
    for (door, slots) in add.door_host_slots {
        changed = true;
        let held = selection.door_host_slots.entry(door.clone()).or_default();
        for (slot, path) in slots {
            if held.contains_key(&slot) {
                return Err(format!("change adds host slot '{slot}' of door '{door}' that the current selection already holds"));
            }
            held.insert(slot, path);
        }
    }
    if !changed { return Err("change adds and removes nothing".into()); }
    Ok(selection)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CurrentRunPlan {
    pub amendment: ProviderCapabilityAmendment,
    pub dependency_writes: Vec<RegistryCandidateWrite>,
    pub activation_writes: Vec<RegistryCandidateWrite>,
    pub validation: RegistryCandidateValidation,
    pub awaiting_activation: Vec<RegistryValidationIssue>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub publication_evidence: Vec<crate::publication_selection::PublicationSelectionEvidence>,
}

fn exact_field(value: &Value, field: &str) -> Result<String, String> {
    value.get(field).and_then(Value::as_str).filter(|value| !value.is_empty() && value.trim() == *value)
        .map(str::to_string).ok_or_else(|| format!("current run requires exact '{field}'"))
}

fn parse<T: serde::de::DeserializeOwned>(value: &Value) -> Result<T, String> {
    serde_json::from_value(value.clone()).map_err(|e| e.to_string())
}

fn retain_selection<T: PartialEq>(prior: &[T], next: &[T], field: &str) -> Result<(), String> {
    if prior.iter().any(|value| !next.contains(value)) {
        return Err(format!("current-run amendment cannot remove or change retained {field} during its accepted effect"));
    }
    Ok(())
}

pub(crate) struct CurrentWorkerPlanning {
    pub topology: FormedCapabilityTopology,
    pub policy: WorkerInterfacePolicy,
    pub tools: BTreeMap<String, ToolDescriptor>,
    pub retained_doors: BTreeSet<String>,
    selected_tools: Vec<SelectedToolSeed>,
    formed_for: (PathBuf, PathBuf, RunRecord, String, String),
}

impl CurrentWorkerPlanning {
    pub(crate) fn for_selection(self, capabilities: &CapabilityRegistry, ee_root: &Path, current: &RunRecord, client: &str, chat_id: &str) -> Result<Self, String> {
        if self.formed_for != (capabilities.registry_root().to_path_buf(), ee_root.to_path_buf(), current.clone(), client.to_string(), chat_id.to_string()) {
            return Err("selected current worker was formed for another registry, run or client".into());
        }
        Ok(self)
    }
}

fn current_nested_tools(
    registry_root: &Path,
    ee_root: &Path,
    current: &RunRecord,
    provider: &str,
    checkpoints: &[Value],
    topology: &FormedCapabilityTopology,
) -> Result<BTreeMap<String, ToolDescriptor>, String> {
    let mut selected = BTreeMap::new();
    let mut owners = BTreeSet::new();
    for template in checkpoints.iter().filter(|cp| cp["role"] == "owner_input") {
        let Some(writer) = template.get("owner_authoring").filter(|value| !value.is_null()) else { continue; };
        let writer: BoundAuthoringWriter = parse(writer)?;
        let Some(binding) = topology.tool_bindings.get(&writer.owner) else { continue; };
        if !binding.sidecars.iter().any(|sidecar| sidecar.file_name == "code_mode.json") { continue; }
        if !owners.insert(writer.owner.clone()) || writer.source != writer.owner
            || template["interface_binding"]["interface"] != writer.owner.as_str()
            || template["outbound_interface_bindings"] != json!([AuthoredInterfaceBinding::all(InterfaceId::new(provider))]) {
            return Err("current nested owner differs from its exact provider return and writer".into());
        }
        let sidecar_path = registry_root.join("interface").join(writer.owner.as_str()).join("code_mode.json");
        let bytes = octopus_interfaces::read_artifact_carrier_file_bounded(&sidecar_path, 1024 * 1024).map_err(|e| e.to_string())?;
        let sidecar: CodeModeSidecarReferences = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        if sidecar.settings.get("schema_version") != Some(&json!(1)) { return Err("current nested owner has an unsupported contract".into()); }
        let handoff = sidecar.handoff.as_ref().ok_or("current nested owner has no pinned handoff")?;
        let source_limit = sidecar.settings.get("max_source_bytes").and_then(Value::as_u64).filter(|n| *n > 0)
            .ok_or("current nested owner has no declared source bound")?;
        let prelude: ArtifactRef = parse(sidecar.settings.get("context").filter(|context| context["mode"] == "persistent")
            .and_then(|context| context.get("prelude_artifact")).ok_or("current nested owner has no persistent prelude")?)?;
        let owner_state = current.interface_state.get(&writer.owner).ok_or("current nested owner has no accepted state")?;
        let owned: Vec<octopus_descriptors::CheckpointId> = parse(owner_state.get("owned_checkpoint_ids").ok_or("current nested owner has no checkpoint ownership")?)?;
        let owned_checkpoints = owned.iter().map(|id| {
            current.checkpoint(id).filter(|_| current.active_checkpoints.contains(id)).ok_or("current nested owner lost an active checkpoint")
        }).collect::<Result<Vec<_>, _>>()?;
        let references = [&handoff.topology_artifact, &prelude];
        let input = owned_checkpoints.iter().find(|cp| cp.outbound_interfaces().any(|id| id == &writer.owner)
            && references.iter().all(|reference| cp.artifact_bindings.iter().any(|artifact| artifact.id == reference.id && artifact.access.read)))
            .ok_or("current nested contract is not readable through its accepted owner bindings")?;
        let grant = octopus_app_server::spine_root::inspect_registry_run_grant(registry_root, ee_root, current, &input.id)?;
        for reference in references {
            if !grant.may_read_artifact(reference.id.as_str())
                || !checkpoints.iter().any(|cp| cp["role"] == "provider_tool_output"
                    && cp["outbound_interface_bindings"] == json!([AuthoredInterfaceBinding::all(writer.owner.clone())])
                    && cp["static_artifact_bindings"].as_array().is_some_and(|ids| ids.contains(&json!(reference.id)))) {
                return Err("current nested contract is not readable through its accepted owner bindings".into());
            }
        }
        crate::context_bindings::validate_artifact_id(handoff.topology_artifact.id.as_str())?;
        let artifact_path = registry_root.join("artifact").join(handoff.topology_artifact.id.as_str()).join("artifact.json");
        let bytes = octopus_interfaces::read_artifact_carrier_file_bounded(&artifact_path, source_limit).map_err(|e| e.to_string())?;
        let artifact: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        if octopus_ledger::ContentHash::of_canonical_json(&artifact) != handoff.topology_artifact.content
            || artifact["id"] != handoff.topology_artifact.id.as_str() || artifact["schema_version"] != 1
            || artifact["run"] != current.id.as_str() || artifact["worker"] != current.worker.as_str()
            || artifact["owner_interface"] != writer.owner.as_str() || artifact["prelude_artifact"] != json!(prelude)
            || artifact["file_write_tool"] != writer.tool
            || artifact["model_output"]["binding"]["inbound"] != json!(AuthoredInterfaceBinding::all(writer.owner.clone()))
            || artifact["model_output"]["binding"]["outbound"] != json!([AuthoredInterfaceBinding::all(InterfaceId::new(provider))])
            || artifact["authoring_result"]["binding"]["inbound"] != json!(AuthoredInterfaceBinding::all(writer.door.clone()))
            || artifact["authoring_result"]["binding"]["outbound"] != json!([AuthoredInterfaceBinding::all(writer.owner.clone())]) {
            return Err("current nested topology differs from its accepted pin, run or owner".into());
        }
        let routes = artifact["routes"].as_array().ok_or("current nested topology has no routes")?;
        let mut expected_routes = routes.clone();
        let declared_routes: Vec<Value> = sidecar.routes.iter().map(serde_json::to_value).collect::<Result<_, _>>().map_err(|e| e.to_string())?;
        let mut configured = declared_routes.clone();
        partition_nested_returns(&mut expected_routes, &mut configured)?;
        if expected_routes != *routes || configured != declared_routes
            || routes.len() != sidecar.routes.len() {
            return Err("current nested return contracts differ from their declared process owners".into());
        }
        let mut route_tools = BTreeSet::new();
        for route in routes {
            let tool = exact_field(route, "tool")?;
            let declared = sidecar.routes.iter().find(|declared| declared.tool == tool).ok_or("current nested route has no declared tool")?;
            let call: octopus_interfaces::CheckpointBindingPolicySnapshot = parse(&route["call"]["binding"])?;
            let result: octopus_interfaces::CheckpointBindingPolicySnapshot = parse(&route["result"]["binding"])?;
            if !route_tools.insert(tool.clone()) || route["call"]["name"] != declared.checkpoint
                || route["result"]["name"].as_str() != declared.result_checkpoint.as_deref()
                || call.inbound.interface != writer.owner || call.outbound.len() != 1
                || call.outbound[0].interface.as_str() != declared.interface || !call.inbound.admits(&writer.call_label)
                || !call.outbound[0].admits(&writer.call_label) || result.inbound.interface.as_str() != declared.interface
                || result.outbound.len() != 1 || result.outbound[0].interface != writer.owner
                || !owned_checkpoints.iter().any(|cp| cp.interface_binding.as_ref() == Some(&result.inbound)
                    && cp.outbound_interface_bindings == result.outbound && cp.required_verified_artifacts.is_empty()) {
                return Err("current nested route is not represented by its exact accepted call and return".into());
            }
            let mut descriptors = vec![(tool, parse::<ToolDescriptor>(&route["process_descriptor"])?)];
            if let Some(approval) = route.get("app_approval").filter(|value| !value.is_null()) {
                if approval["operator_interface"].as_str() != sidecar.continuation.as_ref().and_then(|continuation| continuation.notification_interface.as_deref()) {
                    return Err("current nested approval differs from its declared operator recipient".into());
                }
                descriptors.push((exact_field(approval, "resume_tool")?, parse(&approval["resume_descriptor"])?));
            }
            for (tool, descriptor) in descriptors {
                let [kind] = descriptor.supported_worker_environment_kinds.as_slice() else { return Err("current nested tool has no exact code-world".into()); };
                let installed = load_tool_descriptor_from_dir(&registry_root.join("tool"), &tool).map_err(|e| e.to_string())?
                    .ok_or("current nested tool descriptor is absent")?;
                if descriptor != installed || descriptor.id.as_str() != tool || descriptor.interface.as_str() != declared.interface
                    || !grant.tools.contains(&tool) || grant.hard_blocked_tools.contains(&tool)
                    || !grant.interfaces.contains(&declared.interface)
                    || !topology.tool_bindings.get(&descriptor.interface).is_some_and(|binding| binding.tools.iter()
                        .any(|entry| entry.tool.as_str() == tool && &entry.worker_environment_kind == kind)) {
                    return Err(format!("current nested tool '{tool}' differs from its granted exact descriptor"));
                }
                if selected.insert(tool, descriptor.clone()).is_some_and(|prior| prior != descriptor) {
                    return Err("current nested owners disagree on a selected tool descriptor".into());
                }
            }
        }
    }
    Ok(selected)
}

pub(crate) fn form_current_worker(capabilities: &CapabilityRegistry, ee_root: &Path, current: &RunRecord, client: &str, chat_id: &str) -> Result<CurrentWorkerPlanning, String> {
    let registry_root = capabilities.registry_root();
    let chain = resolve_live_ee_policy_chain(ee_root, &current.ee).map_err(|e|e.to_string())?;
    let mut topology = capabilities.form(&CapabilityFormationRequest::from_live_chain(current.worker.clone(), chain)).map_err(|e|e.to_string())?;
    let policy: WorkerInterfacePolicy = serde_json::from_slice(&std::fs::read(registry_root.join("interface").join(current.worker.as_str()).join("policy.json")).map_err(|e|e.to_string())?).map_err(|e|e.to_string())?;
    let state = json!(current.interface_state.get(&InterfaceId::new(client)).ok_or("current worker has no client owner")?);
    let provider = exact_field(&state,"provider_runtime_interface")?;
    let provider_choice = exact_field(&state,"provider_choice_interface")?;
    let provider_policy = topology.interface_policies.get(&InterfaceId::new(&provider)).cloned().ok_or("current worker does not grant its provider")?;
    topology.interface_policies.insert(InterfaceId::new(provider_choice),provider_policy);
    let value=json!(current);
    let checkpoints=value["interface_state"][&provider]["run_authoring_binding"]["checkpoints"].as_array().ok_or("current provider has no authored tool contracts")?;
    let authoring_writers=checkpoints.iter().filter_map(|cp|cp.get("owner_authoring").filter(|value|!value.is_null()))
        .map(parse::<BoundAuthoringWriter>).collect::<Result<Vec<_>,_>>()?;
    let nested_tools=current_nested_tools(registry_root,ee_root,current,&provider,checkpoints,&topology)?;
    let mut current_tools=checkpoints.iter().filter(|cp|cp["role"]=="provider_tool_output")
        .flat_map(|cp|cp["required_verified_artifacts"].as_array().into_iter().flatten())
        .flat_map(|required|required["params"]["tools"].as_array().into_iter().flatten())
        .filter_map(Value::as_str).collect::<BTreeSet<_>>();
    current_tools.extend(nested_tools.keys().map(String::as_str));
    let mut projected=BTreeMap::new();
    let mut tools=BTreeMap::<String,ToolDescriptor>::new();
    let mut origins=BTreeMap::<String,(bool,bool)>::new();
    let mut retained_doors=BTreeSet::new();
    for (exact, mut binding) in std::mem::take(&mut topology.tool_bindings) {
        let sidecar=match std::fs::read(registry_root.join("interface").join(exact.as_str()).join("octopus_os.json")) {
            Ok(bytes)=>Some(serde_json::from_slice::<OctopusOsInterfaceSidecar>(&bytes).map_err(|e|e.to_string())?),
            Err(e) if e.kind()==std::io::ErrorKind::NotFound=>None,
            Err(e)=>return Err(e.to_string()),
        };
        let owned=sidecar.as_ref().filter(|sidecar|sidecar.schema_version==1 && sidecar.workspace=="octopus_os"
            && sidecar.interface==exact.as_str() && sidecar.owner_interface==client);
        if owned.is_some_and(|sidecar|sidecar.component=="authoring_environment") { continue; }
        let private=owned.is_some_and(|sidecar|sidecar.component=="worker_environment_door");
        if private { retained_doors.insert(exact.to_string()); }
        if owned.is_some() {
            binding.tools.retain(|tool|!authoring_writers.iter().any(|writer|writer.door==exact && writer.tool==tool.tool.as_str())
                || current_tools.contains(tool.tool.as_str()));
            if binding.tools.is_empty() { continue; }
        }
        let source=if private {
            exact.as_str().strip_suffix(&format!("_{chat_id}")).ok_or("current private door differs from its authored identity")?.to_string()
        } else {exact.to_string()};
        let source_id=InterfaceId::new(&source);
        let mut projected_binding=binding.clone();
        projected_binding.interface=load_interface_descriptor(&registry_root.join("interface"),&source)?;
        projected_binding.tools.clear();
        let merged=projected.entry(source_id.clone()).or_insert(projected_binding);
        for kind in &binding.worker_environment_kinds {if !merged.worker_environment_kinds.contains(kind) {merged.worker_environment_kinds.push(kind.clone());}}
        for sidecar in &binding.sidecars {if !merged.sidecars.contains(sidecar) {merged.sidecars.push(sidecar.clone());}}
        if private {
            let formed=topology.interface_policies.get(&exact).cloned().ok_or("current private door has no formed policy")?;
            topology.interface_policies.insert(source_id.clone(),formed);
        }
        for tool in binding.tools {
            let base=if private {tool.tool.as_str().strip_suffix(&format!("__{chat_id}")).ok_or("current private tool differs from its authored identity")?} else {tool.tool.as_str()}.to_string();
            let bound=current_tools.contains(tool.tool.as_str());
            if let Some((was_private,was_bound))=origins.get(&base) {
                if bound && *was_bound {return Err(format!("current tool '{base}' has multiple authored identities; use its exact authored contract"));}
                if *was_bound || (*was_private && !bound) {continue;}
            }
            let mut descriptor=load_tool_descriptor_from_dir(&registry_root.join("tool"),tool.tool.as_str()).map_err(|e|e.to_string())?.ok_or("formed current tool disappeared")?;
            if nested_tools.get(tool.tool.as_str()).is_some_and(|pinned|pinned != &descriptor) {return Err("current nested tool changed after its owner projection".into());}
            descriptor.id=ToolDescriptorId::new(&base);descriptor.interface=source_id.clone();
            tools.insert(base.clone(),descriptor);origins.insert(base.clone(),(private,bound));
        }
    }
    let mut selected_tools=Vec::new();
    for (base,descriptor) in &tools {
        let binding=projected.get_mut(&descriptor.interface).ok_or("current tool lost its exact owner")?;
        let [kind]=descriptor.supported_worker_environment_kinds.as_slice() else {return Err("current tool has no unique formed code-world".into());};
        binding.tools.push(crate::capability_formation::FormedToolCapability {tool:octopus_descriptors::ToolId::new(base),worker_environment_kind:kind.clone()});
        if origins[base].1 {selected_tools.push(SelectedToolSeed {tool:base.clone(),interface:descriptor.interface.to_string(),worker_environment_kind:kind.to_string()});}
    }
    projected.retain(|_,binding|!binding.tools.is_empty());
    topology.granted_tools=tools.keys().map(octopus_descriptors::ToolId::new).collect();
    topology.tool_bindings=projected;
    Ok(CurrentWorkerPlanning {topology,policy,tools,retained_doors,selected_tools,
        formed_for:(registry_root.to_path_buf(),ee_root.to_path_buf(),current.clone(),client.to_string(),chat_id.to_string())})
}

pub fn current_selection(roots: &CompositionRoots, current: &RunRecord, provider: &InterfaceId) -> Result<Option<CurrentRunSelection>, String> {
    Ok(current_selection_with_worker(roots, current, provider)?.map(|(selection, _)| selection))
}

fn current_selection_with_worker(roots: &CompositionRoots, current: &RunRecord, provider: &InterfaceId)
    -> Result<Option<(CurrentRunSelection, (CapabilityRegistry, CurrentWorkerPlanning))>, String> {
    let clients = current.interface_state.iter().filter(|(_, state)| state.get("provider_runtime_interface").and_then(Value::as_str) == Some(provider.as_str())
        && state.contains_key("client_surface_role")).collect::<Vec<_>>();
    let [(client, _)] = clients.as_slice() else {
        if clients.is_empty() { return Ok(None); }
        return Err("current run has ambiguous client authoring owners".into());
    };
    let value: Value = serde_json::from_slice(&std::fs::read(roots.registry_root.join("interface").join(client.as_str()).join("octopus_os.json")).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let profile: OctopusOsAuthoringProfile = parse(value.get("authoring_profile").ok_or("current client has no exact authoring profile")?)?;
    let config = load_client_config(&roots.registry_root.join("interface"))?;
    let chat_id = client.as_str().strip_prefix(&config.run.interface_prefix).ok_or("current client differs from its declared identity")?;
    let capabilities = CapabilityRegistry::load(&roots.registry_root);
    let worker=form_current_worker(&capabilities,&roots.ee_instances_root,current,client.as_str(),chat_id)?;
    let tools=worker.selected_tools.clone();
    if profile.selected_tools.iter().any(|tool| !tools.contains(tool)) {
        return Err("current explicit tool selection is not completely represented by its installed bindings".into());
    }
    Ok(Some((CurrentRunSelection {
        selected_tools: tools, selected_worker_environment_kinds: profile.selected_worker_environment_kinds,
        door_host_slots: profile.door_host_slots, external_inputs: profile.external_inputs, external_relays: profile.external_relays,
        model_outputs: profile.model_outputs, context_artifact_bindings: profile.context_artifact_bindings,
        payload_artifact_bindings: profile.payload_artifact_bindings,
    }, (capabilities, worker))))
}

fn plan_selection(roots: &CompositionRoots, current: &RunRecord, provider: &InterfaceId, key: &str, selection: CurrentRunSelection,
    publications: Vec<octopus_context_collections::CollectionSelection>, native_metadata: Option<&InvocationMetadata>) -> Result<(AuthoredCurrentRun, Vec<crate::publication_selection::PublicationSelectionEvidence>), String> {
    let (previous, selected_worker) = current_selection_with_worker(roots, current, provider)?.ok_or("this run has no client selection owner; supply its exact authored future record")?;
    let clients = current.interface_state.iter().filter(|(_, state)| state.get("provider_runtime_interface").and_then(Value::as_str) == Some(provider.as_str())
        && state.contains_key("client_surface_role")).collect::<Vec<_>>();
    let [(client, state)] = clients.as_slice() else { return Err("current run has no unique client planning owner".into()); };
    let state = json!(state);
    let config = load_client_config(&roots.registry_root.join("interface"))?;
    let chat_id = client.as_str().strip_prefix(&config.run.interface_prefix).ok_or("current client identity differs from its declared prefix")?;
    let descriptor = load_interface_descriptor(&roots.registry_root.join("interface"), provider.as_str())?;
    let private = json!(descriptor.interface_driver_config.as_ref().ok_or("current provider has no driver config")?.private_config);
    let request = OperatorChatPlanRequest {
        registry_root: roots.registry_root.clone(), ee_instances_root: roots.ee_instances_root.clone(), ee_policy_root: None,
        run_records_root: Some(roots.run_records_root.clone()), interface_private_state_root: Some(roots.interface_private_state_root.clone()),
        write_hash_algorithm: WriteHashAlgorithm::Blake3, chat_id: chat_id.into(),
        provider_interface_id: exact_field(&state, "provider_choice_interface")?, model: Some(exact_field(&private, "model")?),
        reasoning_effort: Some(exact_field(&private, "reasoning_effort")?), service_tier: private.get("service_tier").map(parse).transpose()?,
        ee_id: Some(current.ee.to_string()), worker_interface_id: Some(exact_field(&state, "worker_choice_interface")?),
        client_surface_role: exact_field(&state, "client_surface_role")?, cycle: format!("amend_{}", octopus_ledger::ContentHash::of_bytes(key.as_bytes()).to_hex()),
        door_host_slots: selection.door_host_slots, external_inputs: selection.external_inputs, external_relays: selection.external_relays,
        model_outputs: selection.model_outputs, context_artifact_bindings: selection.context_artifact_bindings,
        payload_artifact_bindings: selection.payload_artifact_bindings, selected_worker_environment_kinds: selection.selected_worker_environment_kinds,
        selected_tools: selection.selected_tools, publications, retain_current_capabilities:false, existing_run_record: Some(json!(current)),
        conversation: parse(&json!({"ledger_offset":state["ledger_offset"], "artifact_bindings":state["conversation_artifact_bindings"],
            "pinned_artifact_bindings":state["pinned_artifact_bindings"], "pinned_context_items":state["pinned_context_items"], "continued_from":state["continued_from"]}))?,
    };
    let plan = plan_operator_chat_topology_files_for_source(&request, true, native_metadata, Some(selected_worker))?;
    let selected = &plan.authoring_profile;
    retain_selection(&previous.selected_tools, &selected.selected_tools, "tools")?;
    retain_selection(&previous.selected_worker_environment_kinds, &selected.selected_worker_environment_kinds, "worker environment kinds")?;
    retain_selection(&previous.external_inputs, &selected.external_inputs, "external inputs")?;
    retain_selection(&previous.external_relays, &selected.external_relays, "external relays")?;
    retain_selection(&previous.model_outputs, &selected.model_outputs, "model outputs")?;
    retain_selection(&previous.context_artifact_bindings, &selected.context_artifact_bindings, "context artifacts")?;
    retain_selection(&previous.payload_artifact_bindings, &selected.payload_artifact_bindings, "payload artifacts")?;
    for (door, slots) in &previous.door_host_slots {
        if selected.door_host_slots.get(door) != Some(slots) { return Err(format!("current-run amendment changes retained door roots '{door}'")); }
    }
    if plan.run_id != current.id.as_str() || plan.model_interface_id != provider.as_str() || plan.worker_interface_id != current.worker.as_str() || !plan.removals.is_empty() {
        return Err("current-run capability selection changed identity or removes installed capability owners".into());
    }
    let mut candidate: Value = serde_json::from_str(&plan.run_record_content).map_err(|e| e.to_string())?;
    let current_value = json!(current);
    let current_topology = &current_value["interface_state"][provider.as_str()]["run_authoring_binding"];
    for prior in current_topology["checkpoints"].as_array().ok_or("current topology has no templates")?.iter()
        .filter(|template|template["role"] == "provider_artifact_output") {
        let templates = candidate["interface_state"][provider.as_str()]["run_authoring_binding"]["checkpoints"].as_array_mut().ok_or("planned provider topology has no templates")?;
        if templates.contains(prior) { continue; }
        let matches = templates.iter().enumerate().filter(|(_,next)|next["role"] == prior["role"]
            && next["interface_binding"] == prior["interface_binding"] && next["required_verified_artifacts"] == prior["required_verified_artifacts"])
            .map(|(index,_)|index).collect::<Vec<_>>();
        let [index] = matches.as_slice() else { return Err("current selection cannot replace its retained result/return contract".into()); };
        let generated = templates[*index].clone();
        let retained = prior["outbound_interface_bindings"].as_array().ok_or("retained result has no return bindings")?;
        if generated["outbound_interface_bindings"].as_array().ok_or("planned result has no return bindings")?.iter().any(|binding|!retained.contains(binding)) {
            return Err("current selection changes an existing result contract; author an explicit additional output instead".into());
        }
        templates[*index] = prior.clone();
        for cp in candidate["checkpoints"].as_array_mut().ok_or("planned checkpoints absent")? {
            if cp["interface_binding"] == generated["interface_binding"] && cp["outbound_interface_bindings"] == generated["outbound_interface_bindings"]
                && cp["required_verified_artifacts"] == generated["required_verified_artifacts"] {
                cp["outbound_interface_bindings"] = prior["outbound_interface_bindings"].clone();
            }
        }
    }
    let candidate_topology = &mut candidate["interface_state"][provider.as_str()]["run_authoring_binding"];
    let templates = candidate_topology["checkpoints"].as_array_mut().ok_or("planned provider topology has no templates")?;
    for template in current_topology["checkpoints"].as_array().ok_or("current topology has no templates")? {
        if matches!(template["role"].as_str(), Some("verified_input" | "result_input")) && !templates.contains(template) { templates.push(template.clone()); }
    }
    let current_context = octopus_run_authoring::provider_context_bindings(current, provider)?;
    let future_templates = candidate["interface_state"][provider.as_str()]["run_authoring_binding"]["checkpoints"].as_array().ok_or("planned templates absent")?.clone();
    for cp in candidate["checkpoints"].as_array_mut().ok_or("planned checkpoints absent")? {
        if future_templates.iter().any(|template| template["role"] == "stable" && template["include_context"] == true
            && template["interface_binding"] == cp["interface_binding"] && template["outbound_interface_bindings"] == cp["outbound_interface_bindings"]
            && template["required_verified_artifacts"] == cp["required_verified_artifacts"]) {
            let selected = cp["artifact_bindings"].as_array().ok_or("planned context absent")?.clone();
            let mut retained = Vec::new();
            for binding in current_context.iter().map(|binding| json!(binding)).chain(selected) {
                if !retained.iter().any(|value: &Value| value["id"] == binding["id"]) { retained.push(binding); }
            }
            cp["artifact_bindings"] = json!(retained);
        }
    }
    for (owner, value) in &current.interface_state {
        if owner != provider { candidate["interface_state"][owner.as_str()] = json!(value); }
    }
    let old_frontier = current_value["interface_state"][provider.as_str()]["frontier_checkpoint_ids"].as_array().ok_or("current frontier absent")?;
    let old_templates = current_topology["checkpoints"].as_array().ok_or("current templates absent")?;
    for cp in &current.checkpoints {
        let value = json!(cp);
        let owned = old_frontier.contains(&json!(cp.id)) || old_templates.iter().any(|template| template.get("owner_authoring").is_none_or(Value::is_null)
            && template["interface_binding"] == value["interface_binding"] && template["outbound_interface_bindings"] == value["outbound_interface_bindings"]
            && template["required_verified_artifacts"] == value["required_verified_artifacts"]);
        let admitted_input = old_templates.iter().any(|template| matches!(template["role"].as_str(), Some("verified_input" | "result_input"))
            && template["interface_binding"] == value["interface_binding"] && template["outbound_interface_bindings"] == value["outbound_interface_bindings"]
            && template["required_verified_artifacts"] == value["required_verified_artifacts"]);
        if !owned || admitted_input {
            let rows = candidate["checkpoints"].as_array_mut().ok_or("candidate checkpoints absent")?;
            rows.retain(|row| row["id"] != value["id"]);
            rows.push(value);
            let active = candidate["active_checkpoints"].as_array_mut().ok_or("candidate active checkpoints absent")?;
            active.retain(|id| id != &json!(cp.id));
            if current.active_checkpoints.contains(&cp.id) { active.push(json!(cp.id)); }
        }
    }
    let provider_owner = roots.registry_root.join("interface").join(provider.as_str());
    let catalog_pin = private.pointer("/run_affordances/catalog_artifact").map(parse::<ArtifactRef>).transpose()?;
    let planned = plan.interface_writes.iter().chain(&plan.run_writes).collect::<Vec<_>>();
    require_ee_admission(roots,current.ee.as_str(),&plan.required_interface_ids,&plan.required_tool_ids,&planned)?;
    let writes = plan.interface_writes.into_iter().chain(plan.run_writes)
        .filter(|write| !write.path.starts_with(&provider_owner)).map(|write| RegistryCandidateWrite {
        path:write.path, content:write.content, expected_before_hash:write.expected_before_hash,
    }).collect::<Vec<_>>();
    if let Some(pin) = catalog_pin {
        let path = roots.registry_root.join("artifact").join(pin.id.as_str()).join("artifact.json");
        if let Some(write) = writes.iter().find(|write|write.path == path) {
            let value: Value = serde_json::from_str(&write.content).map_err(|e|e.to_string())?;
            if octopus_ledger::ContentHash::of_bytes(&octopus_descriptors::canonical_bytes(&value)) != pin.content {
                return Err("current-run selection cannot replace the in-flight provider's pinned native catalog".into());
            }
        }
    }
    let bytes = std::fs::read(roots.run_records_root.join(format!("{}.json",current.id))).map_err(|e|e.to_string())?;
    Ok((AuthoredCurrentRun { expected_before_hash:octopus_ledger::ContentHash::of_bytes(&bytes).to_hex(), expected_before:current_value, run_record: candidate, writes }, plan.publication_evidence))
}

pub fn plan_current_run_amendment(roots: &CompositionRoots, current: &RunRecord, provider: &InterfaceId, request: CurrentRunRequest,
    bridge: ProviderCapabilityBridge, retained_tools: &[(String, InterfaceId)]) -> Result<CurrentRunPlan, String> {
    plan_current_run_amendment_with_source(roots, current, provider, request, bridge, retained_tools, None)
}

pub fn plan_current_run_amendment_for_native(roots: &CompositionRoots, current: &RunRecord, provider: &InterfaceId, request: CurrentRunRequest,
    bridge: ProviderCapabilityBridge, retained_tools: &[(String, InterfaceId)], metadata: &InvocationMetadata) -> Result<CurrentRunPlan, String> {
    let runtime = metadata.runtime.as_ref().ok_or("native current-run selection requires admitted runtime coordinates")?;
    if runtime.run != current.id || runtime.worker != current.worker {
        return Err("native current-run selection differs from its admitted run/worker coordinates".into());
    }
    plan_current_run_amendment_with_source(roots, current, provider, request, bridge, retained_tools, Some(metadata))
}

fn prospective_bridge_inputs(record: &RunRecord, provider: &InterfaceId, tool: &str, door: &InterfaceId)
    -> Result<(Vec<octopus_descriptors::CheckpointId>, Option<RequiredVerifiedArtifact>), String> {
    validate_tool_continuation(record, provider, tool, door)?;
    octopus_run_authoring::provider_context_bindings(record, provider)?;
    let topology: BoundRunAuthoringTopology = parse(record.interface_state.get(provider)
        .and_then(|state|state.get("run_authoring_binding")).ok_or("prospective bridge has no declared topology")?)?;
    let output = topology.checkpoints.iter().find(|cp| cp.role == BoundCheckpointRole::ProviderToolOutput
        && cp.required_verified_artifacts.iter().any(|required|required.params.get("tools").and_then(Value::as_array)
            .is_some_and(|tools|tools.iter().any(|id|id.as_str()==Some(tool)))))
        .ok_or("prospective bridge has no exact output contract")?;
    let result = output.result_receiving.get(tool).cloned();
    let mut inputs = Vec::new();
    for template in topology.checkpoints.iter().filter(|cp| cp.outbound_interface_bindings.iter().any(|binding|&binding.interface == provider)
        && if result.is_some() {
            (cp.role == BoundCheckpointRole::Stable && cp.include_context)
                || (cp.role == BoundCheckpointRole::ResultInput && &cp.interface_binding.interface == door)
        } else {
            matches!(cp.role, BoundCheckpointRole::Stable | BoundCheckpointRole::IndependentInput | BoundCheckpointRole::OwnerInput | BoundCheckpointRole::VerifiedInput)
                && &cp.interface_binding.interface == door
        }) {
        let rows = record.checkpoints.iter().filter(|cp| record.active_checkpoints.contains(&cp.id)
            && cp.run == record.id && cp.interface_binding.as_ref() == Some(&template.interface_binding)
            && cp.outbound_interface_bindings == template.outbound_interface_bindings
            && cp.required_verified_artifacts == template.required_verified_artifacts).collect::<Vec<_>>();
        let [input] = rows.as_slice() else { return Err(format!("prospective bridge input '{}' has no unique active checkpoint", template.name)); };
        inputs.push(input.id.clone());
    }
    if inputs.is_empty() { return Err("prospective bridge has no active declared input context".into()); }
    Ok((inputs, result))
}

fn plan_current_run_amendment_with_source(roots: &CompositionRoots, current: &RunRecord, provider: &InterfaceId, request: CurrentRunRequest,
    bridge: ProviderCapabilityBridge, retained_tools: &[(String, InterfaceId)], native_metadata: Option<&InvocationMetadata>) -> Result<CurrentRunPlan, String> {
    safe_segment(&request.amendment_key, "amendment_key")?;
    let exact_authored = request.authored.is_some();
    let (authored, publication_evidence) = match (request.selection, request.change, request.authored) {
        (Some(selection), None, None) => plan_selection(roots, current, provider, &request.amendment_key, selection, request.publications, native_metadata)?,
        (None, Some(change), None) => {
            let selection = current_selection(roots, current, provider)?
                .ok_or("this run has no client selection owner to change; supply its exact authored future record")?;
            let selection = apply_current_run_change(selection, change)?;
            plan_selection(roots, current, provider, &request.amendment_key, selection, request.publications, native_metadata)?
        }
        (None, None, Some(authored)) if request.publications.is_empty() => (authored, Vec::new()),
        (None, None, Some(_)) => return Err("exact authored current-run records cannot also request publication selection".into()),
        _ => return Err("current-run amendment requires exactly one of a complete selection, a change or an authored record".into()),
    };
    let descriptor = load_interface_descriptor(&roots.registry_root.join("interface"), provider.as_str())?;
    let authoring_profile = parse(descriptor.interface_driver_config.as_ref()
        .and_then(|config|config.private_config.get("run_authoring_profile")).ok_or("current provider has no declared run authoring profile")?)?;
    if octopus_ledger::ContentHash::from_hex(&authored.expected_before_hash).is_none_or(|hash|hash.to_hex()!=authored.expected_before_hash) {
        return Err("authored current-run baseline requires an exact content hash".into());
    }
    let amendment = ProviderCapabilityAmendment { amendment_key: request.amendment_key, provider_interface: provider.clone(), before:authored.expected_before, after:authored.run_record, bridge, authoring_profile };
    let amendment = if exact_authored { rebase_plan(current,amendment)? } else { prepare_plan(amendment)? };
    let future = RunRecord::from_bytes(&serde_json::to_vec(&amendment.after).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let bridge_inputs = retained_tools.iter().map(|(tool, door)| Ok((
        prospective_bridge_inputs(current, provider, tool, door)?,
        prospective_bridge_inputs(&future, provider, tool, door)?,
    ))).collect::<Result<Vec<_>, String>>()?;
    let run_path = roots.run_records_root.join(format!("{}.json", current.id));
    let mut dependency_writes = Vec::new();
    let mut activation_writes = Vec::new();
    for write in authored.writes {
        if write.path == run_path { return Err("caller run publication belongs to its checked provider handoff".into()); }
        if write.path.parent() == Some(roots.run_records_root.as_path()) && write.path.exists() {
            let bytes = std::fs::read(&write.path).map_err(|e| e.to_string())?;
            crate::run_revision::validate_checkpoint_extension(&bytes, write.content.as_bytes())?;
        }
        let before = match std::fs::read(&write.path) { Ok(bytes) => Some(bytes), Err(e) if e.kind() == std::io::ErrorKind::NotFound => None, Err(e) => return Err(e.to_string()) };
        if before.as_ref().is_some_and(|bytes| bytes == write.content.as_bytes()) { continue; }
        protect_bridge_descriptor(roots, provider, retained_tools, &write)?;
        if let Some(bytes) = &before {
            if write.expected_before_hash.as_deref() != Some(octopus_ledger::ContentHash::of_bytes(bytes).to_hex().as_str()) {
                return Err(format!("current-run dependency preimage changed at '{}'", write.path.display()));
            }
        } else if write.expected_before_hash.is_some() { return Err(format!("current-run dependency '{}' is missing", write.path.display())); }
        if before.is_none() { dependency_writes.push(write); } else { activation_writes.push(write); }
    }
    let mut writes = dependency_writes.clone();
    let current_bytes = std::fs::read(&run_path).map_err(|e| e.to_string())?;
    if RunRecord::from_bytes(&current_bytes).map_err(|e| e.to_string())? != *current { return Err("current run changed during amendment planning".into()); }
    writes.push(RegistryCandidateWrite { path:run_path, content:serde_json::to_string(&amendment.after).map_err(|e| e.to_string())?,
        expected_before_hash:Some(octopus_ledger::ContentHash::of_bytes(&current_bytes).to_hex()) });
    writes.extend(activation_writes.iter().cloned());
    let publication_index = dependency_writes.len() + 1;
    let mut baseline_roots = None;
    let target_topology: BoundRunAuthoringTopology = parse(&amendment.after["interface_state"][provider.as_str()]["run_authoring_binding"])?;
    let prospective = octopus_run_authoring::capability_amendment::prospective_capability_record(&amendment)?;
    let target_tools = target_topology.checkpoints.iter().filter(|cp| cp.role == BoundCheckpointRole::ProviderToolOutput)
        .flat_map(|cp|cp.required_verified_artifacts.iter()).map(|requirement| {
            requirement.params.get("tools").and_then(Value::as_array).ok_or("future tool contract has no exact tools")?
                .iter().map(|id|id.as_str().map(str::to_string).ok_or("future tool id is malformed")).collect::<Result<Vec<_>,_>>()
        }).collect::<Result<Vec<_>,_>>()?.into_iter().flatten().collect::<BTreeSet<_>>();
    let mut validation = octopus_app_server::validate_registry_candidate_with_inspector(&validation_roots(roots), &writes, |index, view| {
        if index == writes.len() {
            for checkpoint in &prospective.active_checkpoints {
                view.run_grant(&prospective, checkpoint)?;
            }
        }
        let selected = if index < publication_index {current} else {&future};
        for ((tool, _), (current_inputs, future_inputs)) in retained_tools.iter().zip(&bridge_inputs) {
            let (inputs, result) = if index < publication_index {current_inputs} else {future_inputs};
            for input in inputs {
                let grant = view.run_grant(selected, input)?;
                if !grant.tools.iter().any(|id|id==tool) || grant.hard_blocked_tools.iter().any(|id|id==tool) {
                    return Err(format!("publication step {index} removes the actual formed bridge tool '{tool}'"));
                }
                if let Some(required) = result {
                    if !grant.mark_outcomes.iter().any(|id|id == required.mark.as_str()) {
                        return Err(format!("publication step {index} removes the bridge result mark '{}'", required.mark));
                    }
                }
                if index == 0 && baseline_roots.is_none() { baseline_roots = Some(grant.ee_root_slot_allowance.clone()); }
                if let Some(old) = &baseline_roots {
                    let retained = match (old,&grant.ee_root_slot_allowance) {
                        (_,None)=>true,(Some(old),Some(new))=>old.iter().all(|slot|new.contains(slot)),(None,Some(_))=>false,
                    };
                    if !retained { return Err(format!("publication step {index} narrows the accepted bridge's root-slot allowance")); }
                }
                if index == writes.len() {
                    let missing = target_tools.iter().filter(|id|!grant.tools.contains(id)).collect::<Vec<_>>();
                    if !missing.is_empty() { return Err(format!("complete capability candidate has ungranted exact tools: {missing:?}")); }
                }
            }
        }
        Ok(())
    })?;
    let current_interfaces = current.checkpoints.iter().filter(|cp|current.active_checkpoints.contains(&cp.id))
        .flat_map(|cp|cp.inbound_interface().into_iter().chain(cp.outbound_interfaces())).collect::<BTreeSet<_>>();
    let future_only = future.checkpoints.iter().filter(|cp| future.active_checkpoints.contains(&cp.id)
        && cp.inbound_interface().into_iter().chain(cp.outbound_interfaces()).any(|interface|!current_interfaces.contains(interface)))
        .map(|cp|cp.id.to_string()).collect::<BTreeSet<_>>();
    let subject = format!("run/{}",current.id);
    let final_issues = &validation.stages.last().ok_or("amendment validation has no complete stage")?.issues;
    let awaiting_activation = validation.regressions.iter().filter(|issue| issue.subject == subject
        && issue.checkpoint.as_ref().is_some_and(|id|future_only.contains(id)) && !final_issues.contains(issue)
        && validation.stages.iter().filter(|stage|stage.issues.contains(issue)).all(|stage|stage.after_write >= publication_index && stage.after_write < writes.len()))
        .cloned().collect::<Vec<_>>();
    validation.regressions.retain(|issue|!awaiting_activation.contains(issue));
    validation.safe_to_apply=validation.regressions.is_empty();
    Ok(CurrentRunPlan { amendment, dependency_writes, activation_writes, validation, awaiting_activation, publication_evidence })
}

fn protect_bridge_descriptor(roots: &CompositionRoots, provider: &InterfaceId, tools: &[(String, InterfaceId)], write: &RegistryCandidateWrite) -> Result<(), String> {
    let relative = write.path.strip_prefix(&roots.registry_root).ok();
    if tools.iter().any(|(tool, door)| relative.is_some_and(|path| path.starts_with(Path::new("tool").join(tool)) || path.starts_with(Path::new("interface").join(door.as_str())))) {
        return Err(format!("current-run amendment cannot change its accepted bridge descriptor '{}'", write.path.display()));
    }
    for (tool, _) in tools {
        let descriptor = load_tool_descriptor_from_dir(&roots.registry_root.join("tool"), tool).map_err(|e|e.to_string())?
            .ok_or_else(||format!("accepted bridge tool '{tool}' is missing"))?;
        if descriptor.supported_worker_environment_kinds.iter().any(|kind|
            relative.is_some_and(|path|path.starts_with(Path::new("worker_environment_kind").join(kind.as_str())))) {
            return Err(format!("current-run amendment cannot change its accepted bridge's code-world '{}'", write.path.display()));
        }
    }
    if relative.is_some_and(|path| path.starts_with(Path::new("interface").join(provider.as_str()))) {
        return Err(format!("current-run amendment cannot switch the in-flight provider process or configuration '{}'", write.path.display()));
    }
    Ok(())
}
