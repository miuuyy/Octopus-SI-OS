use super::*;

pub(crate) fn tool_call_requirement(config: &ClientConfig, tools: Vec<String>) -> serde_json::Value {
    serde_json::json!({
        "mark": config.run.tool_call_mark,
        "artifact_label": config.artifact_labels.tool_call,
        "verifier": config.run.tool_call_verifier,
        "params": { "tools": tools },
    })
}

pub(crate) fn authoring_call_params(tool: &str) -> serde_json::Value {
    serde_json::json!({
        "tools": [tool],
        "metadata_matches": [{
            "payload": "/arguments/expected_before_hash",
            "metadata": "/runtime/run_record_hash",
        }],
    })
}

pub(crate) fn authoring_call_requirement(config: &ClientConfig, tool: &str) -> serde_json::Value {
    let mut requirement = tool_call_requirement(config, vec![tool.to_string()]);
    requirement["params"] = authoring_call_params(tool);
    requirement
}

pub(crate) fn operator_chat_input_binding(
    interface: &str,
    artifact_label: &str,
) -> Result<AuthoredInterfaceBinding, String> {
    operator_chat_input_binding_names(interface, &[artifact_label.to_string()])
}

pub(crate) fn operator_chat_input_binding_names(
    interface: &str,
    artifact_labels: &[String],
) -> Result<AuthoredInterfaceBinding, String> {
    if artifact_labels.is_empty() {
        return Err("operator chat input artifact labels are empty".to_string());
    }
    let mut names = BTreeSet::new();
    for label in artifact_labels {
        let label = label.trim();
        if label.is_empty() || label.contains(['\r', '\n', '\0']) {
            return Err("operator chat input artifact label is malformed".to_string());
        }
        if !names.insert(label.to_string()) {
            return Err(format!(
                "operator chat input artifact label repeats '{label}'"
            ));
        }
    }
    let binding = AuthoredInterfaceBinding {
        interface: InterfaceId::new(interface),
        config: InterfaceBindingConfig {
            artifact_policy: BindingArtifactPolicy::Allow {
                names: names.into_iter().collect(),
                namespaces: Vec::new(),
            },
        },
    };
    binding
        .validate()
        .map_err(|error| format!("operator chat input artifact binding is invalid: {error}"))?;
    Ok(binding)
}

pub(crate) fn external_input_binding(
    input: &ExternalInputSeed,
) -> Result<AuthoredInterfaceBinding, String> {
    if input.artifact_names.is_empty() {
        return Ok(AuthoredInterfaceBinding::all(InterfaceId::new(&input.interface_id)));
    }
    operator_chat_input_binding_names(&input.interface_id, &input.artifact_names).map_err(|error| {
        format!("external input checkpoint '{}' artifact names: {error}", input.checkpoint_id)
    })
}

pub(crate) fn previous_external_route_templates(
    profile: &OctopusOsAuthoringProfile,
    model_interface_id: &str,
    operations: &OperationRoutes,
) -> Result<Vec<(String, BoundCheckpointTemplate)>, String> {
    let mut routes = Vec::new();
    for input in profile.external_inputs.iter().filter(|input| !operations.declared.contains(&input.interface_id)) {
        routes.push((
            input.checkpoint_id.clone(),
            bound_checkpoint_template(
                BoundCheckpointRole::Stable,
                external_input_binding(input)?,
                vec![AuthoredInterfaceBinding::all(InterfaceId::new(
                    model_interface_id,
                ))],
                input.required_verified_artifacts.clone(),
            )?,
        ));
    }
    for relay in &profile.external_relays {
        routes.push((
            relay.checkpoint_id.clone(),
            bound_checkpoint_template(
                BoundCheckpointRole::IndependentInput,
                AuthoredInterfaceBinding::all(InterfaceId::new(&relay.interface_id)),
                relay
                    .outbound_interface_ids
                    .iter()
                    .map(|interface| AuthoredInterfaceBinding::all(InterfaceId::new(interface)))
                    .collect(),
                relay.required_verified_artifacts.clone(),
            )?,
        ));
    }
    for output in &profile.model_outputs {
        routes.push((
            output.checkpoint_id.clone(),
            bound_checkpoint_template(
                BoundCheckpointRole::ProviderArtifactOutput,
                AuthoredInterfaceBinding::all(InterfaceId::new(model_interface_id)),
                vec![AuthoredInterfaceBinding::all(InterfaceId::new(
                    &output.interface_id,
                ))],
                vec![output.required_verified_artifact.clone()],
            )?,
        ));
    }
    Ok(routes)
}

pub(crate) fn resolve_previous_external_route_checkpoints(
    profile: Option<&OctopusOsAuthoringProfile>,
    checkpoints: &[serde_json::Value],
    active: &BTreeSet<String>,
    run_id: &str,
    model_interface_id: &str,
    operations: &OperationRoutes,
) -> Result<BTreeSet<String>, String> {
    let Some(profile) = profile else {
        return Ok(BTreeSet::new());
    };
    let mut resolved = BTreeSet::new();
    let provider = vec![AuthoredInterfaceBinding::all(InterfaceId::new(model_interface_id))];
    for input in profile.external_inputs.iter().filter(|input| operations.declared.contains(&input.interface_id)) {
        let rows = checkpoints.iter().filter(|checkpoint| {
            checkpoint.get("id").and_then(serde_json::Value::as_str).is_some_and(|id| active.contains(id))
                && checkpoint.get("run").and_then(serde_json::Value::as_str) == Some(run_id)
                && checkpoint["interface_binding"]["interface"].as_str() == Some(input.interface_id.as_str())
                && checkpoint.get("outbound_interface_bindings") == serde_json::to_value(&provider).ok().as_ref()
                && checkpoint.get("required_verified_artifacts") == serde_json::to_value(&input.required_verified_artifacts).ok().as_ref()
        }).filter_map(|checkpoint| checkpoint["id"].as_str().map(str::to_string)).collect::<Vec<_>>();
        if rows.len() > 1 || rows.iter().any(|id| !resolved.insert(id.clone())) {
            return Err(format!("existing application input '{}' has no unique standing route", input.checkpoint_id));
        }
    }
    for (authored_id, template) in previous_external_route_templates(profile, model_interface_id, operations)? {
        let matches = checkpoints
            .iter()
            .filter_map(|checkpoint| {
                let id = checkpoint.get("id")?.as_str()?;
                active.contains(id).then_some((id, checkpoint))
            })
            .filter_map(|(id, checkpoint)| {
                match checkpoint_matches_bound_template(checkpoint, &template, run_id) {
                    Ok(true) => Some(Ok(id.to_string())),
                    Ok(false) => None,
                    Err(error) => Some(Err(error)),
                }
            })
            .collect::<Result<Vec<_>, String>>()?;
        if matches.len() != 1 {
            return Err(format!(
                "existing external route '{authored_id}' matches {} exact active checkpoints",
                matches.len()
            ));
        }
        if !resolved.insert(matches[0].clone()) {
            return Err(format!(
                "existing external routes resolve the same checkpoint '{}'",
                matches[0]
            ));
        }
    }
    Ok(resolved)
}
