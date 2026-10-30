use super::*;

pub(super) struct RetainedConnection {
    pub template: BoundCheckpointTemplate,
    pub artifacts: Vec<ArtifactBinding>,
}

pub(super) fn selected(
    existing: Option<&serde_json::Value>,
    identity: &PlannedIdentity,
    profile: Option<&OctopusOsAuthoringProfile>,
    inputs: &[ExternalInputSeed],
    relays: &[ExternalRelaySeed],
    outputs: &[ModelOutputSeed],
) -> Result<Vec<RetainedConnection>, String> {
    let Some(topology) = existing_bound_run_authoring_topology(existing, identity)? else {
        return Ok(Vec::new());
    };
    let record =
        RunRecord::from_bytes(&serde_json::to_vec(existing.unwrap()).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    let mut changed_client_interfaces = BTreeSet::new();
    if let Some(profile) = profile {
        for input in profile
            .external_inputs
            .iter()
            .filter(|input| !inputs.contains(input))
        {
            changed_client_interfaces.insert(input.interface_id.as_str());
        }
        for relay in profile
            .external_relays
            .iter()
            .filter(|relay| !relays.contains(relay))
        {
            changed_client_interfaces.insert(relay.interface_id.as_str());
            changed_client_interfaces
                .extend(relay.outbound_interface_ids.iter().map(String::as_str));
        }
        for output in profile
            .model_outputs
            .iter()
            .filter(|output| !outputs.contains(output))
        {
            changed_client_interfaces.insert(output.interface_id.as_str());
        }
    }
    let removed_doors = topology
        .checkpoints
        .iter()
        .filter(|template| template.role == BoundCheckpointRole::ProviderToolOutput)
        .flat_map(|template| template.outbound_interface_bindings.iter())
        .filter(|binding| {
            !identity
                .capability_doors
                .values()
                .any(|id| id == binding.interface.as_str())
        })
        .map(|binding| binding.interface.clone())
        .collect::<BTreeSet<_>>();
    let mut retained = Vec::new();
    let mut names = BTreeSet::new();
    let mut rows = BTreeSet::new();
    for template in topology.checkpoints {
        if !names.insert(template.name.clone()) {
            return Err("existing run authoring binding repeats a template name".into());
        }
        if template.role != BoundCheckpointRole::ProviderInterfaceOutput
            || !template.result_receiving.is_empty()
        {
            continue;
        }
        let [target] = template.outbound_interface_bindings.as_slice() else {
            return Err("retained interface output requires one exact recipient".into());
        };
        if changed_client_interfaces.contains(target.interface.as_str())
            || removed_doors.contains(&target.interface)
        {
            continue;
        }
        template.interface_binding.validate()?;
        target.validate()?;
        if template.interface_binding.interface.as_str() != identity.model_interface_id
            || target.interface == template.interface_binding.interface
            || template.include_context
            || template.owner_authoring.is_some()
            || !template.required_verified_artifacts.is_empty()
            || template.interface_binding.config != target.config
            || !matches!(&target.config.artifact_policy,
                BindingArtifactPolicy::Allow { names, namespaces } if names.len() == 1 && namespaces.is_empty())
        {
            return Err(
                "retained interface output changed its exact label or boundary contract".into(),
            );
        }
        let matches = record
            .checkpoints
            .iter()
            .filter(|cp| {
                record.active_checkpoints.contains(&cp.id)
                    && cp.interface_binding.as_ref() == Some(&template.interface_binding)
                    && cp.outbound_interface_bindings == template.outbound_interface_bindings
                    && cp.required_verified_artifacts == template.required_verified_artifacts
            })
            .collect::<Vec<_>>();
        let [checkpoint] = matches.as_slice() else {
            return Err(format!(
                "retained interface output '{}' has {} exact active checkpoints",
                template.name,
                matches.len()
            ));
        };
        if !record
            .interface_state
            .get(&topology.provider_interface)
            .and_then(|state| state.get("frontier_checkpoint_ids"))
            .and_then(serde_json::Value::as_array)
            .is_some_and(|ids| {
                ids.iter()
                    .any(|id| id.as_str() == Some(checkpoint.id.as_str()))
            })
        {
            return Err("retained interface output is outside its provider frontier".into());
        }
        if !rows.insert(checkpoint.id.clone()) {
            return Err("retained interface outputs share an active checkpoint".into());
        }
        let mut artifacts = Vec::new();
        let mut seen = BTreeSet::new();
        for id in &template.static_artifact_bindings {
            if !seen.insert(id) {
                return Err("retained interface output repeats a static artifact".into());
            }
            let binding = checkpoint
                .artifact_bindings
                .iter()
                .find(|binding| &binding.id == id && binding.access.read)
                .ok_or_else(|| {
                    format!(
                        "retained interface output '{}' lacks readable static artifact '{id}'",
                        template.name
                    )
                })?;
            artifacts.push(binding.clone());
        }
        retained.push(RetainedConnection {
            template,
            artifacts,
        });
    }
    Ok(retained)
}

pub(super) fn policy(
    interface_root: &Path,
    identity: &PlannedIdentity,
    retained: &[RetainedConnection],
) -> Result<octopus_policy::InterfacePolicyOverrideLayer, String> {
    if retained.is_empty() {
        return Ok(BTreeMap::new());
    }
    let path = interface_root
        .join(&identity.worker_interface_id)
        .join("policy.json");
    let current: WorkerInterfacePolicy = serde_json::from_slice(
        &std::fs::read(&path)
            .map_err(|e| format!("retained connection ceiling '{}': {e}", path.display()))?,
    )
    .map_err(|e| e.to_string())?;
    if current.id.as_str() != identity.worker_interface_id {
        return Err("retained connection ceiling changed its exact worker identity".into());
    }
    let mut policies = BTreeMap::new();
    for connection in retained {
        let id = &connection.template.outbound_interface_bindings[0].interface;
        let descriptor = load_interface_descriptor(interface_root, id.as_str())?;
        if descriptor.interface_type != octopus_interfaces::InterfaceType::System
            || !current.ceiling.interfaces.contains(id)
        {
            return Err(format!(
                "retained connection '{id}' is not an existing exact System ceiling entry"
            ));
        }
        policies.insert(
            id.clone(),
            current
                .ceiling
                .interface_policy
                .get(id)
                .cloned()
                .unwrap_or_default(),
        );
    }
    Ok(policies)
}

pub(super) fn preserve_policy(
    identity: &PlannedIdentity,
    retained: &octopus_policy::InterfacePolicyOverrideLayer,
    writes: &mut [PlannedClientWrite],
) -> Result<(), String> {
    if retained.is_empty() {
        return Ok(());
    }
    let write = writes
        .iter_mut()
        .find(|write| write.tag == format!("policy:{}", identity.worker_interface_id))
        .ok_or("retained connections have no planned worker ceiling")?;
    let mut policy: serde_json::Value =
        serde_json::from_str(&write.content).map_err(|e| e.to_string())?;
    let ceiling = policy
        .get_mut("ceiling")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or("planned worker ceiling is not an object")?;
    let overrides = ceiling
        .entry("interface_policy")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or("planned interface policy is not an object")?;
    for (id, values) in retained {
        if values.is_empty() {
            overrides.remove(id.as_str());
        } else {
            overrides.insert(
                id.to_string(),
                serde_json::to_value(values).map_err(|e| e.to_string())?,
            );
        }
    }
    let _: WorkerInterfacePolicy =
        serde_json::from_value(policy.clone()).map_err(|e| e.to_string())?;
    write.content = pretty_json(&policy)?;
    Ok(())
}
