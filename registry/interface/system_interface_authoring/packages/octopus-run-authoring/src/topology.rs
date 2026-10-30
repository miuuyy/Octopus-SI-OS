use super::*;

pub fn bound_client_interface(
    value: &serde_json::Value,
    provider: &InterfaceId,
) -> Result<InterfaceId, String> {
    let record =
        RunRecord::from_bytes(&serde_json::to_vec(value).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
    let value = record
        .interface_state
        .get(provider)
        .and_then(|state| state.get(RUN_AUTHORING_BINDING))
        .ok_or_else(|| "provider client projection has no bound topology".to_string())?;
    let topology: BoundRunAuthoringTopology =
        serde_json::from_value(value.clone()).map_err(|error| error.to_string())?;
    topology.validate_identity(&record, provider)?;
    let clients = topology.checkpoints.iter()
        .filter(|checkpoint| checkpoint.role == RunAuthoringCheckpointRole::ProviderArtifactOutput)
        .flat_map(|checkpoint| checkpoint.outbound_interfaces())
        .filter(|interface| *interface != &record.worker && topology.checkpoints.iter().any(|input| {
            input.role.is_provider_input() && input.interface() == *interface
        }))
        .cloned().collect::<BTreeSet<_>>();
    let clients = clients.into_iter().collect::<Vec<_>>();
    let [client] = clients.as_slice() else {
        return Err("provider has no unique bidirectional client projection".to_string());
    };
    Ok(client.clone())
}

pub fn bound_tool_door(
    value: &serde_json::Value,
    provider: &InterfaceId,
    tool: &str,
) -> Result<InterfaceId, String> {
    let record =
        RunRecord::from_bytes(&serde_json::to_vec(value).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
    let value = record
        .interface_state
        .get(provider)
        .and_then(|state| state.get(RUN_AUTHORING_BINDING))
        .ok_or_else(|| "provider tool route has no bound authoring topology".to_string())?;
    let topology: BoundRunAuthoringTopology = serde_json::from_value(value.clone())
        .map_err(|error| format!("provider tool topology is malformed: {error}"))?;
    topology.validate_identity(&record, provider)?;
    let mut owners = Vec::new();
    for checkpoint in &topology.checkpoints {
        if checkpoint.role == RunAuthoringCheckpointRole::ProviderToolOutput
            && checkpoint.interface() == provider
            && checkpoint_tool_ids(checkpoint)?.contains(tool)
        {
            let [door] = checkpoint.outbound_interface_bindings.as_slice() else {
                return Err("provider tool route has no exact door".to_string());
            };
            owners.push(door.interface.clone());
        }
    }
    match owners.as_slice() {
        [door] => Ok(door.clone()),
        _ => Err(format!(
            "provider tool '{tool}' has {} exact owners",
            owners.len()
        )),
    }
}

pub fn declared_provider_run_writer(
    record: &RunRecord, provider: &InterfaceId, profile: &RunAuthoringProfile,
) -> Result<CheckedRunWriter, String> {
    let config = config_from_persisted_profile(record, provider, profile)?;
    let writer = CheckedRunWriter { owner: provider.clone(), source: config.authoring_source_interface.clone(),
        door: config.checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringResult)?.interface().clone(),
        tool: profile.file_write_tool.clone(), call_label: profile.file_write_label.clone(), result_label: profile.file_write_result_label.clone() };
    validate_declared_provider_writer(record, &writer)?;
    Ok(writer)
}

pub(crate) fn config_from_persisted_profile(
    record: &RunRecord,
    provider: &InterfaceId,
    profile: &RunAuthoringProfile,
) -> Result<RunAuthoringConfig, String> {
    profile.validate(provider)?;
    let provider_state = record.interface_state.get(provider).ok_or_else(|| {
        format!(
            "provider run '{}' has no interface_state for '{}'",
            record.id.as_str(),
            provider.as_str()
        )
    })?;
    let value = provider_state.get(RUN_AUTHORING_BINDING).ok_or_else(|| {
        format!(
            "provider run '{}' interface '{}' authoring result has no durable run_authoring_binding",
            record.id.as_str(), provider.as_str()
        )
    })?;
    let topology: BoundRunAuthoringTopology =
        serde_json::from_value(value.clone()).map_err(|error| {
            format!(
                "provider run '{}' run_authoring_binding is malformed: {error}",
                record.id.as_str()
            )
        })?;
    topology.validate_identity(record, provider)?;
    let config = topology.config(profile);
    config.validate(provider, &record.id)?;
    validate_bound_run_authoring_topology(record, &config, None)?;
    Ok(config)
}

#[cfg(test)]
pub(crate) fn derive_bound_run_authoring_topology(
    record: &RunRecord,
    provider: &InterfaceId,
    source_checkpoint: &CheckpointId,
) -> Result<BoundRunAuthoringTopology, String> {
    let provider_state = record.interface_state.get(provider).ok_or_else(|| {
        format!(
            "provider run '{}' has no interface_state for '{}'",
            record.id.as_str(),
            provider.as_str()
        )
    })?;
    let frontier = raw_frontier_checkpoint_ids(provider_state)?;
    if frontier.len() < 3 {
        return Err(format!(
            "provider run '{}' bind-time frontier must contain output, authoring-call, and authoring-result checkpoints",
            record.id.as_str()
        ));
    }
    let active = record.active_checkpoints.iter().collect::<BTreeSet<_>>();
    let mut frontier_records = Vec::with_capacity(frontier.len());
    for id in &frontier {
        let checkpoint_id = CheckpointId::new(id);
        if !active.contains(&checkpoint_id) {
            return Err(format!(
                "provider run '{}' bind-time frontier checkpoint '{}' is not active",
                record.id.as_str(),
                id
            ));
        }
        frontier_records.push(record.checkpoint(&checkpoint_id).ok_or_else(|| {
            format!(
                "provider run '{}' bind-time frontier checkpoint '{}' is absent",
                record.id.as_str(),
                id
            )
        })?);
    }
    for checkpoint in record
        .active_checkpoints
        .iter()
        .filter(|id| !frontier.contains(id.as_str()))
        .filter_map(|id| record.checkpoint(id))
    {
        if checkpoint.inbound_interface() == Some(provider) {
            return Err(format!(
                "provider run '{}' active provider-bound checkpoint '{}' is outside the bind-time frontier",
                record.id.as_str(),
                checkpoint.id.as_str()
            ));
        }
    }
    let output_records = frontier_records
        .iter()
        .copied()
        .filter(|checkpoint| checkpoint.inbound_interface() == Some(provider))
        .collect::<Vec<_>>();
    if output_records.is_empty() {
        return Err(format!(
            "provider run '{}' bind-time frontier has no provider-bound output routes",
            record.id.as_str()
        ));
    }
    let mut output_templates = Vec::with_capacity(output_records.len());
    for checkpoint in output_records.iter().copied() {
        let role = match checkpoint.required_verified_artifacts.as_slice() {
            [] => RunAuthoringCheckpointRole::ProviderArtifactOutput,
            [required] if required.artifact_label == PROVIDER_HANDOFF_COMPLETE_LABEL => {
                RunAuthoringCheckpointRole::ProviderHandoffCompletion
            }
            [required] if required.artifact_label == PROVIDER_TRANSPORT_DISPATCH_LABEL => {
                RunAuthoringCheckpointRole::ProviderTransportDispatch
            }
            [required] if checkpoint_declares_tool_route(required) => {
                RunAuthoringCheckpointRole::ProviderToolOutput
            }
            [_] => RunAuthoringCheckpointRole::ProviderArtifactOutput,
            requirements => {
                return Err(format!(
                    "provider run '{}' bind-time output checkpoint '{}' has {} verified-artifact requirements",
                    record.id.as_str(),
                    checkpoint.id.as_str(),
                    requirements.len()
                ));
            }
        };
        let template = derived_checkpoint_template(role, checkpoint)?;
        match role {
            RunAuthoringCheckpointRole::ProviderToolOutput => {
                checkpoint_tool_ids(&template)?;
            }
            RunAuthoringCheckpointRole::ProviderTransportDispatch => {
                transport_dispatch_attempt(&template)?;
            }
            RunAuthoringCheckpointRole::ProviderHandoffCompletion => {
                validate_handoff_completion_checkpoint(&template)?;
            }
            _ => {}
        }
        output_templates.push(template);
    }
    let result_records = frontier_records
        .iter()
        .copied()
        .filter(|checkpoint| checkpoint.inbound_interface() != Some(provider))
        .filter(|checkpoint| {
            checkpoint.outbound_interface_bindings.len() == 1
                && checkpoint.outbound_interfaces().any(|id| id == provider)
        })
        .filter(|checkpoint| checkpoint.required_verified_artifacts.is_empty())
        .filter(|checkpoint| checkpoint.artifact_bindings.is_empty())
        .collect::<Vec<_>>();
    let [result_record] = result_records.as_slice() else {
        return Err(format!(
            "provider run '{}' bind-time frontier has {} structural authoring-result routes",
            record.id.as_str(),
            result_records.len()
        ));
    };
    let result_interface = result_record.inbound_interface().ok_or_else(|| {
        "provider bind-time authoring-result checkpoint has no interface binding".to_string()
    })?;
    let call_records = frontier_records
        .iter()
        .copied()
        .filter(|checkpoint| checkpoint.id != result_record.id)
        .filter(|checkpoint| checkpoint.inbound_interface() != Some(provider))
        .filter(|checkpoint| {
            checkpoint.outbound_interface_bindings.len() == 1
                && checkpoint.outbound_interfaces().next() == Some(result_interface)
        })
        .filter(|checkpoint| checkpoint.required_verified_artifacts.is_empty())
        .filter(|checkpoint| checkpoint.artifact_bindings.is_empty())
        .collect::<Vec<_>>();
    let [call_record] = call_records.as_slice() else {
        return Err(format!(
            "provider run '{}' bind-time frontier has {} structural authoring-call routes",
            record.id.as_str(),
            call_records.len()
        ));
    };
    if output_records.len() + 2 != frontier_records.len() {
        return Err(format!(
            "provider run '{}' bind-time frontier contains an unclassified checkpoint",
            record.id.as_str()
        ));
    }
    let mut input_templates = Vec::new();
    let mut current_source = false;
    for checkpoint in record
        .active_checkpoints
        .iter()
        .filter(|id| !frontier.contains(id.as_str()))
        .filter_map(|id| record.checkpoint(id))
        .filter(|checkpoint| {
            checkpoint.outbound_interface_bindings.len() == 1
                && checkpoint.outbound_interfaces().any(|id| id == provider)
        })
    {
        current_source |= checkpoint.id == *source_checkpoint;
        let role = if checkpoint.artifact_bindings.is_empty() {
            RunAuthoringCheckpointRole::IndependentInput
        } else {
            RunAuthoringCheckpointRole::Stable
        };
        input_templates.push(derived_checkpoint_template(role, checkpoint)?);
    }
    if !current_source {
        return Err(format!(
            "provider invocation checkpoint '{}' is not an active non-frontier input to '{}'",
            source_checkpoint.as_str(),
            provider.as_str()
        ));
    }
    if input_templates.is_empty() {
        return Err(format!(
            "provider run '{}' bind-time topology has no provider inputs",
            record.id.as_str()
        ));
    }
    let call_template =
        derived_checkpoint_template(RunAuthoringCheckpointRole::AuthoringCall, call_record)?;
    let result_template =
        derived_checkpoint_template(RunAuthoringCheckpointRole::AuthoringResult, result_record)?;
    let authoring_source_interface = call_template.interface().clone();
    let mut checkpoints = input_templates;
    checkpoints.extend(output_templates);
    checkpoints.push(call_template);
    checkpoints.push(result_template);
    Ok(BoundRunAuthoringTopology {
        schema_version: 2,
        run: record.id.clone(),
        worker: record.worker.clone(),
        provider_interface: provider.clone(),
        authoring_source_interface,
        payload_artifact_bindings: Vec::new(),
        writer_run: None,
        checkpoints,
    })
}

#[cfg(test)]
pub(crate) fn derived_checkpoint_template(
    role: RunAuthoringCheckpointRole,
    checkpoint: &octopus_run_record::CheckpointRecord,
) -> Result<RunAuthoringCheckpoint, String> {
    let interface_binding = checkpoint.interface_binding.clone().ok_or_else(|| {
        format!(
            "provider bind-time checkpoint '{}' has no interface binding",
            checkpoint.id.as_str()
        )
    })?;
    let include_context = role == RunAuthoringCheckpointRole::Stable;
    let name = derived_checkpoint_name(
        role,
        &interface_binding,
        &checkpoint.outbound_interface_bindings,
        include_context,
        &checkpoint.required_verified_artifacts,
    )?;
    Ok(RunAuthoringCheckpoint {
        name,
        role,
        result_receiving: BTreeMap::new(),
        interface_binding,
        outbound_interface_bindings: checkpoint.outbound_interface_bindings.clone(),
        include_context,
        static_artifact_bindings: Vec::new(),
        owner_authoring: None,
        required_verified_artifacts: checkpoint.required_verified_artifacts.clone(),
    })
}

pub(crate) fn derived_checkpoint_name(
    role: RunAuthoringCheckpointRole,
    interface_binding: &AuthoredInterfaceBinding,
    outbound_interface_bindings: &[AuthoredInterfaceBinding],
    include_context: bool,
    required_verified_artifacts: &[RequiredVerifiedArtifact],
) -> Result<String, String> {
    let bytes = serde_json::to_vec(&serde_json::json!({
        "role": role,
        "interface_binding": interface_binding,
        "outbound_interface_bindings": outbound_interface_bindings,
        "include_context": include_context,
        "required_verified_artifacts": required_verified_artifacts,
    }))
    .map_err(|error| format!("provider derived checkpoint template did not encode: {error}"))?;
    let digest = format!("{:x}", Sha256::digest(bytes));
    Ok(format!("derived_{}", &digest[..20]))
}

pub(crate) fn validate_bound_run_authoring_topology(
    record: &RunRecord,
    config: &RunAuthoringConfig,
    current_source: Option<&CheckpointId>,
) -> Result<(), String> {
    validate_payload_artifact_selection(&config.payload_artifact_bindings)?;
    let roles = current_mandatory_role_bindings(record, config)?;
    for reference in &config.payload_artifact_bindings {
        if roles.iter().any(|binding| binding.id == reference.id) {
            return Err("payload selection collides with an authoring role binding".into());
        }
        for checkpoint in record.checkpoints.iter().filter(|checkpoint| record.active_checkpoints.contains(&checkpoint.id)) {
            let bound = checkpoint.artifact_bindings.iter().filter(|binding| binding.id == reference.id).collect::<Vec<_>>();
            if !bound.is_empty() && (bound.len() != 1 || !bound[0].access.read) {
                return Err("payload classification has an ambiguous or unreadable checkpoint binding".into());
            }
        }
    }
    validate_checkpoint_topology(record,&config.provider_interface,&config.checkpoints,current_source)
}

pub(crate) fn validate_checkpoint_topology(
    record: &RunRecord,
    provider: &InterfaceId,
    templates: &[RunAuthoringCheckpoint],
    current_source: Option<&CheckpointId>,
) -> Result<(), String> {
    let checkpoint_for_role = |role| {
        let candidates = templates.iter().filter(|template| template.role == role).collect::<Vec<_>>();
        if candidates.len() != 1 { return Err("provider topology has an ambiguous authoring role".to_string()); }
        Ok(candidates[0])
    };
    let provider_state = record
        .interface_state
        .get(provider)
        .ok_or_else(|| {
            format!(
                "provider run '{}' has no interface_state for '{}'",
                record.id.as_str(),
                provider.as_str()
            )
        })?;
    let frontier = raw_frontier_checkpoint_ids(provider_state)?;
    let active = record.active_checkpoints.iter().collect::<BTreeSet<_>>();
    let frontier_records = frontier
        .iter()
        .map(|id| {
            let checkpoint_id = CheckpointId::new(id);
            if !active.contains(&checkpoint_id) {
                return Err(format!(
                    "provider run '{}' frontier checkpoint '{}' is not active",
                    record.id.as_str(),
                    id
                ));
            }
            record.checkpoint(&checkpoint_id).ok_or_else(|| {
                format!(
                    "provider run '{}' frontier checkpoint '{}' is absent",
                    record.id.as_str(),
                    id
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    for checkpoint_id in record
        .active_checkpoints
        .iter()
        .filter(|checkpoint_id| !frontier.contains(checkpoint_id.as_str()))
    {
        let checkpoint = record.checkpoint(checkpoint_id).ok_or_else(|| {
            format!(
                "provider run '{}' lacks active checkpoint '{}'",
                record.id.as_str(),
                checkpoint_id.as_str()
            )
        })?;
        if checkpoint.inbound_interface() == Some(provider) {
            return Err(format!(
                "provider run '{}' active provider-bound checkpoint '{}' is outside the provider-owned frontier",
                record.id.as_str(),
                checkpoint.id.as_str()
            ));
        }
    }
    let input_templates = templates
        .iter()
        .filter(|template| template.role.is_provider_input())
        .collect::<Vec<_>>();
    let output_templates = templates
        .iter()
        .filter(|template| {
            matches!(
                template.role,
                RunAuthoringCheckpointRole::ProviderArtifactOutput
                    | RunAuthoringCheckpointRole::ProviderInterfaceOutput
                    | RunAuthoringCheckpointRole::ProviderToolOutput
                    | RunAuthoringCheckpointRole::ProviderTransportDispatch
                    | RunAuthoringCheckpointRole::ProviderHandoffCompletion
            )
        })
        .collect::<Vec<_>>();
    let authoring_call = checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringCall)?;
    let authoring_result =
        checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringResult)?;
    let writer_run = writer_run::binding(record, provider)?;
    let authoring_count = if writer_run.is_some() { 0 } else { 2 };
    let mandatory_role_bindings = match &writer_run {
        Some(binding) => binding.roles(record, provider, templates)?,
        None => canonical_role_bindings(&frontier_records, authoring_call, authoring_result, &record.id)?,
    };
    for checkpoint_id in record
        .active_checkpoints
        .iter()
        .filter(|checkpoint_id| !frontier.contains(checkpoint_id.as_str()))
    {
        let checkpoint = record.checkpoint(checkpoint_id).ok_or_else(|| {
            format!(
                "provider run '{}' lacks active checkpoint '{}'",
                record.id.as_str(),
                checkpoint_id.as_str()
            )
        })?;
        if (checkpoint.inbound_interface() == Some(authoring_call.interface())
            || checkpoint.inbound_interface() == Some(authoring_result.interface())
        ) && result_inputs::bound_source(record, provider, &checkpoint.id)?.is_none()
        {
            return Err(format!(
                "provider run '{}' active checkpoint '{}' shares a provider-owned authoring interface outside the frontier",
                record.id.as_str(),
                checkpoint.id.as_str()
            ));
        }
    }
    for template in [authoring_call, authoring_result].into_iter().filter(|_| writer_run.is_none()) {
        let matches = frontier_records
            .iter()
            .filter(|checkpoint| {
                checkpoint_record_matches_template(checkpoint, template, &record.id)
                    && checkpoint_role_bindings_match(
                        checkpoint,
                        &mandatory_role_bindings,
                        template,
                    )
            })
            .count();
        if matches != 1 {
            return Err(format!(
                "provider run '{}' frontier matches {} {:?} templates",
                record.id.as_str(),
                matches,
                template.role
            ));
        }
    }
    let active_output_matches = frontier_records
        .iter()
        .filter(|checkpoint| {
            output_templates.iter().any(|template| {
                checkpoint_record_matches_template(checkpoint, template, &record.id)
                    && checkpoint_role_bindings_match(
                        checkpoint,
                        &mandatory_role_bindings,
                        template,
                    )
            })
        })
        .count();
    let validate_rearmed_frontier = || -> Result<(), String> {
        for template in &output_templates {
            let matches = frontier_records
                .iter()
                .filter(|checkpoint| {
                    checkpoint_record_matches_template(checkpoint, template, &record.id)
                        && checkpoint_role_bindings_match(
                            checkpoint,
                            &mandatory_role_bindings,
                            template,
                        )
                })
                .count();
            if matches != 1 {
                return Err(format!(
                    "provider run '{}' rearmed frontier matches {} output template '{}'",
                    record.id.as_str(),
                    matches,
                    template.name
                ));
            }
        }
        if active_output_matches != output_templates.len()
            || frontier.len() != output_templates.len() + authoring_count
        {
            return Err(format!(
                "provider run '{}' rearmed frontier contains unclassified checkpoint topology",
                record.id.as_str()
            ));
        }
        Ok(())
    };
    let pending = provider_state.contains_key("pending_handoff");
    if pending {
        let pending =
            persisted_pending_handoff(record, provider)?.ok_or_else(|| {
                format!(
                    "provider run '{}' frontier declares pending state without a pending handoff",
                    record.id.as_str()
                )
            })?;
        for item in &pending.items {
            if item.artifact_label == PROVIDER_TRANSPORT_DISPATCH_LABEL
                && item.checkpoint.outbound_interface_bindings.is_empty()
                && !item
                    .checkpoint
                    .artifact_bindings
                    .iter()
                    .eq(mandatory_role_bindings.iter().map(|binding| &binding.id))
            {
                return Err(format!(
                    "provider run '{}' transport dispatch checkpoint '{}' must carry exactly its mandatory role bindings",
                    record.id.as_str(),
                    item.checkpoint.id.as_str()
                ));
            }
        }
        match validate_pending_items_are_active(record, provider, &pending) {
            Ok(retained) => {
                if frontier.len() != pending.items.len() + retained + authoring_count {
                    return Err(format!(
                        "provider run '{}' live profile frontier must contain every pending output and two authoring checkpoints",
                        record.id.as_str(),
                    ));
                }
            }
            Err(error) if pending.terminal => {
                validate_rearmed_frontier().map_err(|rearmed_error| {
                    format!("{error}; completed terminal frontier is not rearmed: {rearmed_error}")
                })?;
            }
            Err(error) => return Err(error),
        }
    } else {
        validate_rearmed_frontier()?;
    }
    let mut active_inputs = Vec::new();
    for checkpoint in record
        .active_checkpoints
        .iter()
        .filter(|id| !frontier.contains(id.as_str()))
        .filter_map(|id| record.checkpoint(id))
        .filter(|checkpoint| {
            checkpoint.outbound_interfaces().any(|id| id == provider)
        })
    {
        let matches = input_templates
            .iter()
            .filter(|template| {
                checkpoint_record_matches_template(checkpoint, template, &record.id)
                    && checkpoint_role_bindings_match(
                        checkpoint,
                        &mandatory_role_bindings,
                        template,
                    )
            })
            .count();
        if matches != 1 {
            return Err(format!(
                "provider run '{}' provider input checkpoint '{}' matches {} bound input templates",
                record.id.as_str(),
                checkpoint.id.as_str(),
                matches
            ));
        }
        active_inputs.push(checkpoint.id.clone());
    }
    for template in &input_templates {
        let count = active_inputs.iter().filter_map(|id| record.checkpoint(id)).filter(|checkpoint| checkpoint_record_matches_template(checkpoint, template, &record.id)).count();
        if count > 1 || (count == 0 && template.role != RunAuthoringCheckpointRole::OwnerInput) {
            return Err(format!("provider run '{}' input '{}' has {} active rows", record.id.as_str(), template.name, count));
        }
    }
    if let Some(source) = current_source {
        if !active_inputs.contains(source) {
            return Err(format!(
                "provider invocation checkpoint '{}' is not one active bound provider input",
                source.as_str()
            ));
        }
    }
    Ok(())
}

pub(crate) fn checkpoint_record_matches_template(
    checkpoint: &octopus_run_record::CheckpointRecord,
    template: &RunAuthoringCheckpoint,
    run: &RunId,
) -> bool {
    checkpoint.run == *run
        && checkpoint.interface_binding.as_ref() == Some(&template.interface_binding)
        && checkpoint.outbound_interface_bindings == template.outbound_interface_bindings
        && checkpoint.required_verified_artifacts == template.required_verified_artifacts
}

pub(crate) fn canonical_role_bindings(
    frontier: &[&octopus_run_record::CheckpointRecord],
    authoring_call: &RunAuthoringCheckpoint,
    authoring_result: &RunAuthoringCheckpoint,
    run: &RunId,
) -> Result<Vec<ArtifactBinding>, String> {
    let checkpoint = |template: &RunAuthoringCheckpoint| {
        let matches = frontier
            .iter()
            .copied()
            .filter(|checkpoint| checkpoint_record_matches_template(checkpoint, template, run))
            .collect::<Vec<_>>();
        let [checkpoint] = matches.as_slice() else {
            return Err(format!(
                "provider run '{}' frontier matches {} {:?} checkpoint structures",
                run.as_str(),
                matches.len(),
                template.role
            ));
        };
        Ok(*checkpoint)
    };
    let call = checkpoint(authoring_call)?;
    let result = checkpoint(authoring_result)?;
    if call.artifact_bindings != result.artifact_bindings {
        return Err(format!(
            "provider run '{}' authoring checkpoints carry different mandatory role bindings",
            run.as_str()
        ));
    }
    if call
        .artifact_bindings
        .iter()
        .any(|binding| !binding.access.read)
    {
        return Err(format!(
            "provider run '{}' authoring checkpoints contain a non-readable mandatory role binding",
            run.as_str()
        ));
    }
    Ok(call.artifact_bindings.clone())
}

pub(crate) fn checkpoint_role_bindings_match(
    checkpoint: &octopus_run_record::CheckpointRecord,
    mandatory_role_bindings: &[ArtifactBinding],
    template: &RunAuthoringCheckpoint,
) -> bool {
    if template.include_context {
        return checkpoint.artifact_bindings.starts_with(mandatory_role_bindings)
            && mandatory_role_bindings.iter().all(|binding| binding.access.read)
            && template.static_artifact_bindings.iter().all(|id| {
                let mut matches = checkpoint.artifact_bindings.iter().filter(|binding| &binding.id == id);
                matches.next().is_some_and(|binding| {
                    match mandatory_role_bindings.iter().find(|role| &role.id == id) {
                        Some(role) => binding == role,
                        None => binding.access == Default::default(),
                    }
                })
                    && matches.next().is_none()
            });
    }
    let mut required = mandatory_role_bindings.to_vec();
    for id in &template.static_artifact_bindings {
        if required.iter().any(|binding| &binding.id == id) { continue; }
        required.push(ArtifactBinding { id: id.clone(), access: Default::default() });
    }
    let mandatory_role_bindings = required.as_slice();
    if checkpoint.artifact_bindings.len() != mandatory_role_bindings.len() {
        return false;
    }
    checkpoint
        .artifact_bindings
        .iter()
        .zip(mandatory_role_bindings)
        .all(|(binding, required)| binding == required && binding.access.read)
}

pub(crate) fn artifact_refs_match_bindings(refs: &[ArtifactRef], bindings: &[ArtifactBinding]) -> bool {
    refs.iter()
        .map(|artifact| &artifact.id)
        .eq(bindings.iter().map(|binding| &binding.id))
}

pub(crate) fn correlation_generation(run: &RunId, correlation_id: &str) -> Result<u64, String> {
    let prefix = format!("provider-run-authoring:{}:", run.as_str());
    let raw_generation = correlation_id.strip_prefix(&prefix).ok_or_else(|| {
        "provider authoring result correlation_id is outside the private authoring namespace"
            .to_string()
    })?;
    let generation = raw_generation
        .parse::<u64>()
        .map_err(|_| "provider authoring result correlation generation is malformed".to_string())?;
    if generation == 0 || generation.to_string() != raw_generation {
        return Err(
            "provider authoring result correlation generation is not canonical".to_string(),
        );
    }
    Ok(generation)
}

pub(crate) fn derive_handoff_delivery_mode(
    input: &InterfaceInvocationEnvelope,
    config: &RunAuthoringConfig,
    record: &RunRecord,
    source_checkpoint: &CheckpointId,
    granted_tools: &BTreeSet<String>,
) -> Result<HandoffDeliveryMode, String> {
    let metadata = input.metadata().map_err(|error| error.to_string())?;
    let hard_blocked = input
        .hard_blocked_tool_names()
        .map_err(|error| error.to_string())?
        .into_iter()
        .collect::<BTreeSet<_>>();
    if let Some(writer) = &config.writer_run {
        writer.roles(record, &config.provider_interface, &config.checkpoints)?;
        if hard_blocked.contains(&config.file_write_tool) {
            return Err("provider's declared writer tool is hard-blocked".into());
        }
        if metadata.grant().ee_root_slot_allowance_set()
            .is_some_and(|allowance| !allowance.contains("run_records_root")) {
            return Err("provider's declared writer Run is outside the EE root-slot allowance".into());
        }
        return Ok(HandoffDeliveryMode::Rearm);
    }
    if granted_tools.contains(&config.file_write_tool) {
        if hard_blocked.contains(&config.file_write_tool) {
            return Err(format!(
                "provider run '{}' grants and hard-blocks authoring tool '{}'",
                config.run.as_str(),
                config.file_write_tool
            ));
        }
        let authoring_call =
            config.checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringCall)?;
        let authoring_result =
            config.checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringResult)?;
        for interface in [
            &config.provider_interface,
            &config.authoring_source_interface,
            authoring_result.interface(),
        ] {
            if !metadata
                .allowed_interfaces()
                .iter()
                .any(|allowed| allowed == interface.as_str())
            {
                return Err(format!(
                    "provider run '{}' authoring path interface '{}' is absent from the formed grant",
                    config.run.as_str(),
                    interface.as_str()
                ));
            }
        }
        if authoring_call.outbound_interfaces().next() != Some(authoring_result.interface()) {
            return Err(format!(
                "provider run '{}' authoring path is incomplete",
                config.run.as_str()
            ));
        }
        if metadata.grant().ee_root_slot_allowance_set()
            .is_some_and(|allowance| !allowance.contains("run_records_root")) {
            return Err(format!(
                "provider run '{}' formed grant does not admit run_records_root",
                config.run.as_str()
            ));
        }
        return Ok(HandoffDeliveryMode::Rearm);
    }
    if !granted_tools.is_empty() {
        return Err(format!(
            "provider run '{}' exposes model-visible tool continuation without run re-arm authority",
            config.run.as_str()
        ));
    }
    if !record.active_checkpoints.contains(source_checkpoint) {
        return Err(format!(
            "provider one-shot source checkpoint '{}' is no longer active",
            source_checkpoint.as_str()
        ));
    }
    let source = record.checkpoint(source_checkpoint).ok_or_else(|| {
        format!(
            "provider one-shot source checkpoint '{}' is absent",
            source_checkpoint.as_str()
        )
    })?;
    let source_interface = source.inbound_interface().ok_or_else(|| {
        format!(
            "provider one-shot source checkpoint '{}' has no interface binding",
            source_checkpoint.as_str()
        )
    })?;
    let active_source_routes = record
        .active_checkpoints
        .iter()
        .filter_map(|checkpoint| record.checkpoint(checkpoint))
        .filter(|checkpoint| checkpoint.inbound_interface() == Some(source_interface))
        .count();
    if active_source_routes != 1 {
        return Err(format!(
            "provider one-shot source interface '{}' is bound to {} active checkpoints in run '{}'",
            source_interface.as_str(),
            active_source_routes,
            record.id.as_str()
        ));
    }
    let output_templates = config
        .checkpoints
        .iter()
        .filter(|checkpoint| {
            matches!(
                checkpoint.role,
                RunAuthoringCheckpointRole::ProviderArtifactOutput
                    | RunAuthoringCheckpointRole::ProviderInterfaceOutput
                    | RunAuthoringCheckpointRole::ProviderToolOutput
                    | RunAuthoringCheckpointRole::ProviderTransportDispatch
                    | RunAuthoringCheckpointRole::ProviderHandoffCompletion
            )
        })
        .collect::<Vec<_>>();
    let mut terminal = BTreeMap::new();
    let mut dispatch = BTreeMap::new();
    let mut completion = None;
    for checkpoint_id in &record.active_checkpoints {
        let checkpoint = record.checkpoint(checkpoint_id).ok_or_else(|| {
            format!(
                "provider one-shot active checkpoint '{}' is absent",
                checkpoint_id.as_str()
            )
        })?;
        if checkpoint.inbound_interface() != Some(&config.provider_interface) {
            continue;
        }
        let owners = output_templates
            .iter()
            .copied()
            .filter(|template| checkpoint_record_matches_template(checkpoint, template, &record.id))
            .collect::<Vec<_>>();
        let [owner] = owners.as_slice() else {
            return Err(format!(
                "provider one-shot checkpoint '{}' has {} descriptor-derived output owners",
                checkpoint.id.as_str(),
                owners.len()
            ));
        };
        let [required] = checkpoint.required_verified_artifacts.as_slice() else {
            return Err(format!(
                "provider one-shot checkpoint '{}' is not verifier-isolated",
                checkpoint.id.as_str()
            ));
        };
        let slot = OneShotHandoffSlot {
            checkpoint: checkpoint.id.clone(),
            epoch: checkpoint.epoch.0,
            required_mark: required.mark.as_str().to_string(),
            required_verifier: required.verifier.as_str().to_string(),
        };
        match owner.role {
            RunAuthoringCheckpointRole::ProviderArtifactOutput => {
                if terminal
                    .insert(required.artifact_label.clone(), slot)
                    .is_some()
                {
                    return Err(format!(
                        "provider one-shot topology repeats terminal artifact label '{}'",
                        required.artifact_label
                    ));
                }
            }
            RunAuthoringCheckpointRole::ProviderTransportDispatch => {
                let attempt = transport_dispatch_attempt(owner)?;
                if dispatch.insert(attempt, slot).is_some() {
                    return Err(format!(
                        "provider one-shot topology repeats transport attempt {attempt}"
                    ));
                }
            }
            RunAuthoringCheckpointRole::ProviderToolOutput | RunAuthoringCheckpointRole::ProviderInterfaceOutput => {
                return Err(format!(
                    "provider one-shot checkpoint '{}' carries tool continuation without re-arm authority",
                    checkpoint.id.as_str()
                ));
            }
            RunAuthoringCheckpointRole::ProviderHandoffCompletion => {
                validate_handoff_completion_checkpoint(owner)?;
                if completion.replace(slot).is_some() {
                    return Err(
                        "provider one-shot topology repeats the handoff completion slot"
                            .to_string(),
                    );
                }
            }
            _ => unreachable!(),
        }
    }
    let completion = completion.ok_or_else(|| {
        format!(
            "provider run '{}' one-shot topology has no preauthored handoff completion slot",
            config.run.as_str()
        )
    })?;
    if terminal.is_empty() || dispatch.is_empty() {
        return Err(format!(
            "provider run '{}' one-shot topology requires preauthored verifier-isolated terminal, completion, and transport-dispatch slots",
            config.run.as_str()
        ));
    }
    Ok(HandoffDeliveryMode::OneShot(OneShotHandoffPlan {
        terminal,
        dispatch,
        completion,
    }))
}

pub(crate) fn transport_dispatch_attempt(checkpoint: &RunAuthoringCheckpoint) -> Result<u32, String> {
    let [required] = checkpoint.required_verified_artifacts.as_slice() else {
        return Err(format!(
            "run_authoring checkpoint '{}' transport route requires exactly one verified-artifact requirement",
            checkpoint.name
        ));
    };
    if required.artifact_label != PROVIDER_TRANSPORT_DISPATCH_LABEL {
        return Err(format!(
            "run_authoring checkpoint '{}' transport route requires artifact label '{}'",
            checkpoint.name, PROVIDER_TRANSPORT_DISPATCH_LABEL
        ));
    }
    let params = required.params.as_object().ok_or_else(|| {
        format!(
            "run_authoring checkpoint '{}' transport route params are not an object",
            checkpoint.name
        )
    })?;
    let attempt = params
        .get("attempt")
        .and_then(serde_json::Value::as_u64)
        .and_then(|attempt| u32::try_from(attempt).ok())
        .filter(|attempt| *attempt > 0)
        .ok_or_else(|| {
            format!(
                "run_authoring checkpoint '{}' transport route has no positive u32 attempt",
                checkpoint.name
            )
        })?;
    Ok(attempt)
}

pub(crate) fn validate_handoff_completion_checkpoint(
    checkpoint: &RunAuthoringCheckpoint,
) -> Result<(), String> {
    let [required] = checkpoint.required_verified_artifacts.as_slice() else {
        return Err(format!(
            "run_authoring checkpoint '{}' completion route requires exactly one verified-artifact requirement",
            checkpoint.name
        ));
    };
    if required.artifact_label != PROVIDER_HANDOFF_COMPLETE_LABEL {
        return Err(format!(
            "run_authoring checkpoint '{}' completion route requires artifact label '{}'",
            checkpoint.name, PROVIDER_HANDOFF_COMPLETE_LABEL
        ));
    }
    Ok(())
}
