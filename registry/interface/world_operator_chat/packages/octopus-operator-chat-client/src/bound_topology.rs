use super::*;

pub(crate) fn assert_existing_run_contract(
    existing: &serde_json::Value,
    identity: &PlannedIdentity,
    selection: &RunSelection,
) -> Result<(), String> {
    if existing.get("interface_state").and_then(|state| state.get(&identity.model_interface_id))
        .is_some_and(|state| state.get("selected_consent").is_some()) {
        return Err("selected Home consent state cannot be reinterpreted as an ordinary task continuation".into());
    }
    let field = |name: &str| -> Result<&str, String> {
        existing
            .get(name)
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| format!("existing chat run.{name} is required"))
    };
    if field("id")? != identity.run_id {
        return Err(format!(
            "existing chat run identity mismatch at {}",
            identity.run_id
        ));
    }
    if field("ee")? != selection.ee_id {
        return Err(format!(
            "existing chat run execution environment does not match {}",
            selection.ee_id
        ));
    }
    if field("worker")? != identity.worker_interface_id {
        return Err(format!(
            "existing chat run worker pin does not match {}",
            identity.worker_interface_id
        ));
    }
    Ok(())
}

pub(crate) fn existing_bound_run_authoring_topology(
    existing: Option<&serde_json::Value>,
    identity: &PlannedIdentity,
) -> Result<Option<BoundRunAuthoringTopology>, String> {
    let Some(existing) = existing else {
        return Ok(None);
    };
    let value = existing
        .get("interface_state")
        .and_then(|state| state.get(&identity.model_interface_id))
        .and_then(|state| state.get("run_authoring_binding"));
    let Some(value) = value else {
        return Ok(None);
    };
    let binding: BoundRunAuthoringTopology = serde_json::from_value(value.clone())
        .map_err(|error| format!("existing run authoring binding is malformed: {error}"))?;
    if binding.schema_version != 2
        || binding.run.as_str() != identity.run_id
        || binding.worker.as_str() != identity.worker_interface_id
        || binding.provider_interface.as_str() != identity.model_interface_id
        || binding.authoring_source_interface.as_str() != identity.authoring_source_id
    {
        return Err("existing run authoring binding changed its exact task identity".to_string());
    }
    Ok(Some(binding))
}

pub(crate) fn bound_context_checkpoint_ids(
    checkpoints: &[serde_json::Value],
    active: &BTreeSet<String>,
    binding: &BoundRunAuthoringTopology,
    identity: &PlannedIdentity,
) -> Result<BTreeSet<String>, String> {
    let templates = binding
        .checkpoints
        .iter()
        .filter(|template| {
            matches!(
                template.role,
                BoundCheckpointRole::Stable | BoundCheckpointRole::IndependentInput
            )
        })
        .collect::<Vec<_>>();
    let mut ids = BTreeSet::new();
    for template in templates {
        let matches = checkpoints
            .iter()
            .filter_map(|checkpoint| {
                let id = checkpoint.get("id")?.as_str()?;
                active.contains(id).then_some((id, checkpoint))
            })
            .filter_map(|(id, checkpoint)| {
                match checkpoint_matches_bound_template(checkpoint, template, &identity.run_id) {
                    Ok(true) => Some(Ok(id.to_string())),
                    Ok(false) => None,
                    Err(error) => Some(Err(error)),
                }
            })
            .collect::<Result<Vec<_>, String>>()?;
        if matches.len() != 1 {
            return Err(format!(
                "existing run authoring template '{}' matches {} exact active checkpoints",
                template.name,
                matches.len(),
            ));
        }
        if !ids.insert(matches[0].clone()) {
            return Err(format!(
                "existing run authoring templates resolve the same checkpoint '{}'",
                matches[0],
            ));
        }
    }
    if ids.is_empty() {
        return Err("existing run authoring binding has no context inputs".to_string());
    }
    Ok(ids)
}

pub(crate) fn checkpoint_matches_bound_template(
    checkpoint: &serde_json::Value,
    template: &BoundCheckpointTemplate,
    run_id: &str,
) -> Result<bool, String> {
    let object = checkpoint
        .as_object()
        .ok_or_else(|| "existing checkpoint is not an object".to_string())?;
    let inbound = object
        .get("interface_binding")
        .cloned()
        .map(serde_json::from_value::<AuthoredInterfaceBinding>)
        .transpose()
        .map_err(|error| format!("existing checkpoint inbound binding is malformed: {error}"))?;
    let outbound = serde_json::from_value::<Vec<AuthoredInterfaceBinding>>(
        object
            .get("outbound_interface_bindings")
            .cloned()
            .ok_or_else(|| "existing checkpoint outbound bindings are absent".to_string())?,
    )
    .map_err(|error| format!("existing checkpoint outbound bindings are malformed: {error}"))?;
    let required = serde_json::from_value::<Vec<RequiredVerifiedArtifact>>(
        object
            .get("required_verified_artifacts")
            .cloned()
            .ok_or_else(|| "existing checkpoint requirements are absent".to_string())?,
    )
    .map_err(|error| format!("existing checkpoint requirements are malformed: {error}"))?;
    Ok(
        object.get("run").and_then(serde_json::Value::as_str) == Some(run_id)
            && inbound.as_ref() == Some(&template.interface_binding)
            && outbound == template.outbound_interface_bindings
            && required == template.required_verified_artifacts,
    )
}

pub(crate) fn derive_bound_run_authoring_topology(
    identity: &PlannedIdentity,
    cycle_checkpoints: &[serde_json::Value],
    external_checkpoints: &[serde_json::Value],
    frontier: &[serde_json::Value],
    message_artifact_label: &str,
    tool_call_verifier: &str,
) -> Result<BoundRunAuthoringTopology, String> {
    let frontier = frontier
        .iter()
        .map(|value| {
            value
                .as_str()
                .filter(|id| !id.is_empty())
                .map(str::to_string)
                .ok_or_else(|| "planned provider frontier contains a malformed id".to_string())
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    let mut templates = Vec::new();
    for checkpoint in cycle_checkpoints.iter().chain(external_checkpoints) {
        let id = checkpoint
            .get("id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "planned checkpoint has no id".to_string())?;
        let inbound = serde_json::from_value::<AuthoredInterfaceBinding>(
            checkpoint
                .get("interface_binding")
                .cloned()
                .ok_or_else(|| format!("planned checkpoint '{id}' has no inbound binding"))?,
        )
        .map_err(|error| {
            format!("planned checkpoint '{id}' inbound binding is malformed: {error}")
        })?;
        let outbound = serde_json::from_value::<Vec<AuthoredInterfaceBinding>>(
            checkpoint
                .get("outbound_interface_bindings")
                .cloned()
                .ok_or_else(|| format!("planned checkpoint '{id}' has no outbound bindings"))?,
        )
        .map_err(|error| {
            format!("planned checkpoint '{id}' outbound bindings are malformed: {error}")
        })?;
        let required = serde_json::from_value::<Vec<RequiredVerifiedArtifact>>(
            checkpoint
                .get("required_verified_artifacts")
                .cloned()
                .ok_or_else(|| format!("planned checkpoint '{id}' has no requirements"))?,
        )
        .map_err(|error| {
            format!("planned checkpoint '{id}' requirements are malformed: {error}")
        })?;
        let role = if inbound.interface.as_str() == identity.authoring_source_id
            && outbound.len() == 1
            && outbound[0].interface.as_str() == identity.authoring_environment_id
        {
            BoundCheckpointRole::AuthoringCall
        } else if inbound.interface.as_str() == identity.authoring_environment_id
            && outbound.len() == 1
            && outbound[0].interface.as_str() == identity.model_interface_id
        {
            BoundCheckpointRole::AuthoringResult
        } else if frontier.contains(id) && inbound.interface.as_str() == identity.model_interface_id
        {
            match required.as_slice() {
                [requirement] if requirement.artifact_label == PROVIDER_HANDOFF_COMPLETE_LABEL => {
                    BoundCheckpointRole::ProviderHandoffCompletion
                }
                [requirement]
                    if requirement.artifact_label == PROVIDER_TRANSPORT_DISPATCH_LABEL =>
                {
                    BoundCheckpointRole::ProviderTransportDispatch
                }
                [requirement] if requirement.verifier.as_str() == tool_call_verifier => {
                    BoundCheckpointRole::ProviderToolOutput
                }
                [] | [_] => BoundCheckpointRole::ProviderArtifactOutput,
                _ => {
                    return Err(format!(
                        "planned provider output '{id}' has multiple requirements"
                    ));
                }
            }
        } else if identity.owns_client()
            && inbound.interface.as_str() == identity.chat_interface_id
            && outbound.len() == 1
            && outbound[0].interface.as_str() == identity.model_interface_id
        {
            if binding_admits_named(&inbound, message_artifact_label) {
                BoundCheckpointRole::Stable
            } else {
                BoundCheckpointRole::IndependentInput
            }
        } else if outbound.len() == 1
            && outbound[0].interface.as_str() == identity.model_interface_id
        {
            BoundCheckpointRole::Stable
        } else {
            continue;
        };
        templates.push(bound_checkpoint_template(
            role, inbound, outbound, required,
        )?);
    }
    let authoring_source_interface = templates
        .iter()
        .find(|template| template.role == BoundCheckpointRole::AuthoringCall)
        .map(|template| template.interface_binding.interface.clone())
        .ok_or_else(|| "planned run authoring topology has no authoring call".to_string())?;
    if !templates
        .iter()
        .any(|template| template.role == BoundCheckpointRole::AuthoringResult)
        || !templates
            .iter()
            .any(|template| template.role == BoundCheckpointRole::Stable)
        || !templates.iter().any(|template| {
            matches!(
                template.role,
                BoundCheckpointRole::ProviderArtifactOutput
                    | BoundCheckpointRole::ProviderToolOutput
                    | BoundCheckpointRole::ProviderTransportDispatch
                    | BoundCheckpointRole::ProviderHandoffCompletion
            )
        })
    {
        return Err("planned run authoring topology is incomplete".to_string());
    }
    Ok(BoundRunAuthoringTopology {
        schema_version: 2,
        run: octopus_descriptors::RunId::new(&identity.run_id),
        worker: InterfaceId::new(&identity.worker_interface_id),
        provider_interface: InterfaceId::new(&identity.model_interface_id),
        authoring_source_interface,
        payload_artifact_bindings: Vec::new(),
        writer_run: None,
        checkpoints: templates,
    })
}

pub(crate) fn binding_admits_named(binding: &AuthoredInterfaceBinding, artifact_label: &str) -> bool {
    matches!(
        &binding.config.artifact_policy,
        BindingArtifactPolicy::Allow { names, namespaces }
            if namespaces.is_empty() && names.iter().any(|name| name == artifact_label)
    )
}

pub(crate) fn tool_result_requirements(output: &BoundCheckpointTemplate, result: &ToolResultReceiving) -> Result<BTreeMap<String, RequiredVerifiedArtifact>, String> {
    let [required] = output.required_verified_artifacts.as_slice() else { return Err("tool output requires one call contract".into()); };
    let tools = required.params.get("tools").and_then(serde_json::Value::as_array).ok_or("tool output has no explicit tools")?;
    let mut receiving = BTreeMap::new();
    for tool in tools {
        let tool = tool.as_str().filter(|tool| !tool.is_empty() && tool.trim() == *tool).ok_or("tool output has an invalid tool id")?;
        if receiving.insert(tool.to_string(), result.requirement(tool)?).is_some() { return Err("tool output repeats a tool".into()); }
    }
    if receiving.is_empty() { return Err("tool output has no result obligations".into()); }
    Ok(receiving)
}

pub(crate) fn result_receiving_authorized(
    registry: &Path, chain: &[crate::capability_formation::LiveEePolicyLayer],
    policy: &WorkerInterfacePolicy, result: &ToolResultReceiving,
) -> Result<bool, String> {
    result.requirement("declared")?;
    safe_policy_axis_value(result.mark.as_str(), "result mark")?;
    if matches!(result.mark.as_str(), "." | "..") { return Err("result mark must name an exact registry folder".into()); }
    safe_segment(result.verifier.as_str(), "result verifier")?;
    let mark: octopus_marks::MarkDescriptor = parse_strict(&std::fs::read(registry.join("mark").join(result.mark.as_str()).join("mark.json")).map_err(|e|e.to_string())?).map_err(|e|e.to_string())?;
    let verifier: octopus_verifiers::VerifierDescriptor = parse_strict(&std::fs::read(registry.join("verifier").join(result.verifier.as_str()).join("verifier.json")).map_err(|e|e.to_string())?).map_err(|e|e.to_string())?;
    if mark.id != result.mark || verifier.id != result.verifier { return Err("result correspondence descriptor identity differs from its exact folder".into()); }
    let mut layers = Vec::new();
    for entry in chain {
        let template: EeTemplateDescriptor = parse_strict(&std::fs::read(registry.join("ee_template").join(entry.template.as_str()).join("ee_template.json")).map_err(|e|e.to_string())?).map_err(|e|e.to_string())?;
        layers.extend([template.as_policy_layer(), entry.policy_narrowing.as_policy_layer()]);
    }
    Ok(octopus_policy::form_policy(policy.ceiling_grant(), &layers, &[], Default::default()).context.mark_outcomes.contains(&result.mark))
}

pub(crate) fn bound_checkpoint_template(
    role: BoundCheckpointRole,
    interface_binding: AuthoredInterfaceBinding,
    outbound_interface_bindings: Vec<AuthoredInterfaceBinding>,
    required_verified_artifacts: Vec<RequiredVerifiedArtifact>,
) -> Result<BoundCheckpointTemplate, String> {
    let include_context = role == BoundCheckpointRole::Stable;
    let bytes = serde_json::to_vec(&serde_json::json!({
        "role": role,
        "interface_binding": interface_binding,
        "outbound_interface_bindings": outbound_interface_bindings,
        "include_context": include_context,
        "required_verified_artifacts": required_verified_artifacts,
    }))
    .map_err(|error| format!("bound checkpoint template did not encode: {error}"))?;
    let digest = format!("{:x}", Sha256::digest(bytes));
    Ok(BoundCheckpointTemplate {
        name: format!("derived_{}", &digest[..20]),
        role,
        result_receiving: BTreeMap::new(),
        static_artifact_bindings: Vec::new(),
        owner_authoring: None,
        interface_binding,
        outbound_interface_bindings,
        include_context,
        required_verified_artifacts,
    })
}
