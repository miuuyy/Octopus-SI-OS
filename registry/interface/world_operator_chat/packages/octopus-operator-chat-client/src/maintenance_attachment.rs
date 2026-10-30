use super::*;
use crate::maintenance_observer::{MaintenanceObserverConfig, MaintenanceObserverRequest,
    plan_maintenance_observer, wire_source_terminal_declaration, wire_source_terminal_output};
use crate::run_composition::CompositionRoots;
use serde_json::Value;

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Selection {
    template_interface_id: String,
    config_file: String,
}

pub(super) struct AttachmentRequest<'a> {
    pub capabilities: &'a CapabilityRegistry,
    pub ee_instances_root: &'a Path,
    pub run_records_root: Option<&'a Path>,
    pub interface_private_state_root: Option<&'a Path>,
    pub config: &'a ClientConfig,
    pub identity: &'a PlannedIdentity,
    pub source_input_labels: Vec<String>,
    pub existing: Option<&'a Value>,
    pub write_hash_algorithm: WriteHashAlgorithm,
}

pub(super) fn wire(
    request: AttachmentRequest<'_>,
    content: String,
    writes: &mut Vec<PlannedClientWrite>,
    required_interfaces: &mut Vec<String>,
    required_tools: &mut Vec<String>,
) -> Result<(String, Vec<PlannedClientWrite>), String> {
    let identity = request.identity;
    if let Some(existing) = request.existing {
        let Some(declaration) = existing.get("interface_state")
            .and_then(|states| states.get(&identity.model_interface_id))
            .and_then(|state| state.get("maintenance_observer")) else {
            return Ok((content, Vec::new()));
        };
        let relay = validate_retained(existing, declaration, identity)?;
        retain_source_ceiling(request.capabilities.registry_root(), identity, &relay, writes)?;
        let content = wire_source_terminal_declaration(&content, declaration)?;
        required_interfaces.push(relay);
        required_interfaces.sort();
        required_interfaces.dedup();
        return Ok((content, Vec::new()));
    }
    let Some(config) = request.config.run._rest.get("maintenance_observer") else {
        return Ok((content, Vec::new()));
    };
    if config.is_null() {
        return Ok((content, Vec::new()));
    }
    let selection: Selection = serde_json::from_value(config.clone())
        .map_err(|e| format!("client maintenance_observer selection: {e}"))?;
    selection_component(&selection.template_interface_id, "maintenance template")?;
    selection_component(&selection.config_file, "maintenance configuration file")?;
    let owner = request.capabilities.registry_root().join("interface").join(&selection.template_interface_id);
    for path in [&owner, &owner.join(&selection.config_file)] {
        if std::fs::symlink_metadata(path).map_err(|e| e.to_string())?.file_type().is_symlink() {
            return Err("maintenance selection cannot follow a symlink".into());
        }
    }
    let bytes = octopus_interfaces::read_artifact_carrier_file_bounded(&owner.join(selection.config_file), 1_048_576)
        .map_err(|e| e.to_string())?;
    let config: MaintenanceObserverConfig = serde_json::from_slice(&bytes)
        .map_err(|e| format!("client maintenance_observer configuration: {e}"))?;
    if config.worker_template_interface_id != selection.template_interface_id {
        return Err("maintenance configuration belongs to a different selected worker template".into());
    }
    let roots = CompositionRoots {
        registry_root: request.capabilities.registry_root().to_path_buf(),
        ee_instances_root: request.ee_instances_root.to_path_buf(),
        run_records_root: request.run_records_root
            .ok_or("maintenance attachment requires explicit run_records_root")?.to_path_buf(),
        interface_private_state_root: request.interface_private_state_root
            .ok_or("maintenance attachment requires explicit interface_private_state_root")?.to_path_buf(),
    };
    let mut source_input_labels = request.source_input_labels;
    source_input_labels.sort();
    source_input_labels.dedup();
    let record = RunRecord::from_bytes(content.as_bytes()).map_err(|e| e.to_string())?;
    let observer = plan_maintenance_observer(&MaintenanceObserverRequest {
        roots,
        source_provider_choice_interface_id: identity.provider_choice_interface.clone(),
        source_provider_interface_id: identity.model_interface_id.clone(),
        source_client_interface_id: identity.chat_interface_id.clone(),
        source_input_labels,
        config,
        write_hash_algorithm: request.write_hash_algorithm,
    }, &record, writes, request.capabilities)?;
    if !observer.source_required_artifact_bindings.is_empty() {
        return Err("maintenance source requires artifact bindings absent from its authored context".into());
    }
    if observer.run_writes.iter().any(|write| write.expected_before_hash.is_some()) {
        return Err("new maintenance attachment cannot replace an existing companion or writer run".into());
    }
    extend_source_ceiling(identity, &observer.source_required_interface_ids, None, writes)?;
    let content = wire_source_terminal_output(&content, &observer)?;
    required_interfaces.extend(observer.required_interface_ids);
    required_interfaces.extend(observer.source_required_interface_ids);
    required_interfaces.sort();
    required_interfaces.dedup();
    required_tools.extend(observer.required_tool_ids);
    required_tools.sort();
    required_tools.dedup();
    writes.extend(observer.descriptor_writes);
    Ok((content, observer.run_writes))
}

fn selection_component(value: &str, field: &str) -> Result<(), String> {
    if value.len() > 192
        || !value.as_bytes().first().is_some_and(u8::is_ascii_alphanumeric)
        || !value.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.'))
    {
        return Err(format!("{field} must be an exact single path component starting with a letter or digit"));
    }
    Ok(())
}

fn validate_retained(existing: &Value, declaration: &Value, identity: &PlannedIdentity) -> Result<String, String> {
    if declaration.get("schema_version") != Some(&serde_json::json!(1)) {
        return Err("accepted maintenance declaration has an unsupported schema".into());
    }
    for (field, expected) in [
        ("source_run", &identity.run_id),
        ("source_worker_interface", &identity.worker_interface_id),
        ("source_provider_interface", &identity.model_interface_id),
        ("source_client_interface", &identity.chat_interface_id),
    ] {
        if declaration.get(field).and_then(Value::as_str) != Some(expected.as_str()) {
            return Err(format!("accepted maintenance declaration changed {field}"));
        }
    }
    let relay = declaration.get("source_relay_interface").and_then(Value::as_str)
        .ok_or("accepted maintenance declaration has no source relay")?;
    safe_segment(relay, "accepted maintenance relay")?;
    let terminal = declaration.get("terminal_label").and_then(Value::as_str)
        .filter(|s| !s.is_empty() && s.trim() == *s)
        .ok_or("accepted maintenance declaration has no exact terminal label")?;
    let record = RunRecord::from_bytes(&serde_json::to_vec(existing).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let topology = existing_bound_run_authoring_topology(Some(existing), identity)?
        .ok_or("accepted maintenance source has no authoring topology")?;
    let templates = topology.checkpoints.iter().filter(|template| {
        template.outbound_interface_bindings.iter().any(|b| b.interface.as_str() == relay)
    }).collect::<Vec<_>>();
    let [template] = templates.as_slice() else {
        return Err("accepted maintenance source has no unique terminal relay owner".into());
    };
    let names = BindingArtifactPolicy::Allow { names: vec![terminal.into()], namespaces: Vec::new() };
    if template.role != BoundCheckpointRole::ProviderArtifactOutput
        || template.interface_binding.interface.as_str() != identity.model_interface_id
        || template.interface_binding.config.artifact_policy != names
        || !template.outbound_interface_bindings.iter().any(|b|
            b.interface.as_str() == identity.chat_interface_id && b.config.artifact_policy.admits(terminal))
        || template.outbound_interface_bindings.iter().find(|b| b.interface.as_str() == relay)
            .is_none_or(|b| b.config.artifact_policy != names)
    {
        return Err("accepted maintenance source changed its terminal-only route".into());
    }
    let checkpoints = record.checkpoints.iter().filter(|checkpoint| {
        record.active_checkpoints.contains(&checkpoint.id)
            && checkpoint.interface_binding.as_ref() == Some(&template.interface_binding)
            && checkpoint.outbound_interface_bindings == template.outbound_interface_bindings
            && checkpoint.required_verified_artifacts == template.required_verified_artifacts
    }).count();
    if checkpoints != 1 {
        let pending = existing.get("interface_state").and_then(|states| states.get(&identity.model_interface_id))
            .and_then(|state| state.get("pending_handoff"));
        let ended_without_terminal = record.status.as_str() == Some("idle") && pending.is_some_and(|pending|
            pending.get("terminal") == Some(&serde_json::json!(true))
                && pending.get("items").and_then(Value::as_array).is_some_and(|items| !items.is_empty()
                    && items.iter().all(|item| item.get("artifact_label").and_then(Value::as_str).is_some_and(|label| label != terminal))));
        if checkpoints != 0 || !ended_without_terminal {
            return Err("accepted maintenance source does not retain one exact active terminal checkpoint".into());
        }
        octopus_run_authoring::provider_context_bindings(&record, &InterfaceId::new(&identity.model_interface_id))?;
    }
    Ok(relay.into())
}

fn retain_source_ceiling(registry_root: &Path, identity: &PlannedIdentity, relay: &str, writes: &mut [PlannedClientWrite]) -> Result<(), String> {
    let root = registry_root.join("interface");
    let descriptor = load_interface_descriptor(&root, relay)?;
    if descriptor.id.as_str() != relay || descriptor.interface_type != octopus_interfaces::InterfaceType::System {
        return Err("accepted maintenance relay is not its exact System interface".into());
    }
    let bytes = std::fs::read(root.join(&identity.worker_interface_id).join("policy.json"))
        .map_err(|e| e.to_string())?;
    let policy: WorkerInterfacePolicy = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let id = InterfaceId::new(relay);
    if policy.id.as_str() != identity.worker_interface_id || !policy.ceiling.interfaces.contains(&id) {
        return Err("accepted maintenance relay is outside its existing worker ceiling".into());
    }
    let tag = format!("policy:{}", identity.worker_interface_id);
    if !writes.iter().any(|write| write.tag == tag) {
        return Ok(());
    }
    extend_source_ceiling(identity, &[relay.into()], Some(&policy), writes)
}

fn extend_source_ceiling(identity: &PlannedIdentity, interfaces: &[String], previous: Option<&WorkerInterfacePolicy>, writes: &mut [PlannedClientWrite]) -> Result<(), String> {
    let write = writes.iter_mut().find(|write| write.tag == format!("policy:{}", identity.worker_interface_id))
        .ok_or("maintenance source has no planned worker ceiling")?;
    let mut policy: Value = serde_json::from_str(&write.content).map_err(|e| e.to_string())?;
    let ceiling = policy.get_mut("ceiling").and_then(Value::as_object_mut)
        .ok_or("maintenance source ceiling is malformed")?;
    let mut ids: Vec<String> = serde_json::from_value(ceiling.get("interfaces").cloned()
        .ok_or("maintenance source has no interface ceiling")?).map_err(|e| e.to_string())?;
    ids.extend_from_slice(interfaces);
    set_policy_axis(ceiling, "interfaces", ids)?;
    if let Some(previous) = previous {
        for interface in interfaces {
            if let Some(values) = previous.ceiling.interface_policy.get(&InterfaceId::new(interface)) {
                ceiling.entry("interface_policy").or_insert_with(|| serde_json::json!({}))
                    .as_object_mut().ok_or("maintenance source interface policy is malformed")?
                    .insert(interface.clone(), serde_json::to_value(values).map_err(|e| e.to_string())?);
            }
        }
    }
    write.content = pretty_json(&policy)?;
    Ok(())
}

#[cfg(test)]
#[path = "../tests/support/retained_source_ceiling.rs"]
mod retained_source_ceiling_tests;
