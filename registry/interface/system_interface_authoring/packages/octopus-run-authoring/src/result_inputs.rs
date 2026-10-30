use super::*;

mod invocation;
pub(super) use invocation::{bound_source, definition_source, source_was_bridged};
pub(super) use invocation::rebased_value as rebased_invocation_value;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Correspondence {
    call_artifact: ArtifactRef,
    tool: String,
    correlation_id: String,
    params: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    invocation: Option<invocation::ResultInvocation>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct OperationCall {
    operation: String,
    #[serde(rename = "arguments")]
    _arguments: serde_json::Value,
    correlation_id: String,
}

fn call_identity(bytes: &[u8]) -> Result<(String, Option<String>), String> {
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
    if value.get("operation").is_some() {
        let call: OperationCall = serde_json::from_value(value).map_err(|error| error.to_string())?;
        return Ok((call.operation, Some(call.correlation_id)));
    }
    let call: octopus_tool_handlers::ToolCallArtifact =
        serde_json::from_value(value).map_err(|error| error.to_string())?;
    Ok((call.tool, call.correlation_id))
}

fn result_identity(bytes: &[u8]) -> Result<(String, Option<String>, String), String> {
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
    if value.get("operation").is_some() {
        let field = |name: &str| value.get(name).and_then(serde_json::Value::as_str).map(str::to_string);
        return Ok((
            field("operation").ok_or("operation result names no operation")?,
            field("correlation_id"),
            field("label").ok_or("operation result has no label")?,
        ));
    }
    let result: ToolResultArtifact = serde_json::from_value(value).map_err(|error| error.to_string())?;
    Ok((result.tool, result.correlation_id, result.label))
}

fn receives(output: &RunAuthoringCheckpoint, subject: &str, label: &str) -> bool {
    match output.role {
        RunAuthoringCheckpointRole::ProviderToolOutput => output.result_receiving.contains_key(subject),
        RunAuthoringCheckpointRole::ProviderInterfaceOutput => output.result_receiving.contains_key(label),
        _ => false,
    }
}

fn receiving_output<'a>(
    checkpoints: &'a [RunAuthoringCheckpoint],
    template: &RunAuthoringCheckpoint,
    call: &Correspondence,
) -> Result<&'a RunAuthoringCheckpoint, String> {
    let label = &template.required_verified_artifacts[0].artifact_label;
    let outputs = checkpoints
        .iter()
        .filter(|output| output.outbound_interfaces().eq([template.interface()]) && receives(output, &call.tool, label))
        .collect::<Vec<_>>();
    let [output] = outputs.as_slice() else {
        return Err("result input has no exact declared receiving output".into());
    };
    Ok(output)
}

pub(super) fn select_call_result(
    output: &mut RunAuthoringCheckpoint,
    route: &ProviderHandoffRoute,
) -> Result<(), String> {
    if output.role != RunAuthoringCheckpointRole::ProviderInterfaceOutput || output.result_receiving.is_empty() {
        return Ok(());
    }
    let ProviderHandoffRoute::Operation { result_label, .. } = route else {
        return Err("an interface output that waits for results requires its call to name its result label".into());
    };
    output.result_receiving.retain(|label, _| label == result_label);
    if output.result_receiving.is_empty() {
        return Err(format!(
            "operation call names result label '{result_label}' outside its interface output's declared results"
        ));
    }
    Ok(())
}

fn rearms(
    template: &RunAuthoringCheckpoint,
    old: &[RunAuthoringCheckpoint],
    next: &[RunAuthoringCheckpoint],
) -> bool {
    next.iter().any(|input| {
        input.role == RunAuthoringCheckpointRole::VerifiedInput
            && !old.contains(input)
            && input.outbound_interfaces().skip(1).any(|id| id == template.interface())
    })
}

fn active_rows(record: &RunRecord, template: &RunAuthoringCheckpoint) -> Vec<octopus_run_record::CheckpointRecord> {
    record
        .checkpoints
        .iter()
        .filter(|checkpoint| {
            record.active_checkpoints.contains(&checkpoint.id)
                && checkpoint_record_matches_template(checkpoint, template, &record.id)
        })
        .cloned()
        .collect()
}

pub(super) fn enabled(config: &RunAuthoringConfig) -> bool {
    config.checkpoints.iter().any(|template| {
        template.role == RunAuthoringCheckpointRole::ResultInput
            || !template.result_receiving.is_empty()
    })
}

fn pending_inputs<'a>(config: &'a RunAuthoringConfig, pending: &PendingHandoff) -> Result<Vec<&'a RunAuthoringCheckpoint>, String> {
    let mut inputs = Vec::new();
    for template in config.checkpoints.iter().filter(|template| template.role == RunAuthoringCheckpointRole::ResultInput) {
        let call = correspondence(template)?;
        let Some(item) = pending.items.iter().find(|item|item.artifact_id == call.call_artifact.id) else { continue; };
        if call.invocation.is_some() || item.artifact_content_hash != call.call_artifact.content.to_hex()
            || !item.checkpoint.outbound_interface_bindings.iter().map(|binding|&binding.interface).eq([template.interface()]) {
            return Err("withdrawal result input differs from its exact unsubmitted call".into());
        }
        inputs.push(template);
    }
    Ok(inputs)
}

pub(super) fn cancel_pending_inputs(value: &mut serde_json::Value, config: &RunAuthoringConfig, pending: &PendingHandoff) -> Result<(), String> {
    let inputs = pending_inputs(config, pending)?;
    if inputs.is_empty() { return Ok(()); }
    let record = RunRecord::from_bytes(&serde_json::to_vec(value).map_err(|error|error.to_string())?).map_err(|error|error.to_string())?;
    let mut topology: BoundRunAuthoringTopology = serde_json::from_value(record.interface_state.get(&config.provider_interface)
        .and_then(|state|state.get(RUN_AUTHORING_BINDING)).ok_or("withdrawal lost its result topology")?.clone()).map_err(|error|error.to_string())?;
    let mut removed = BTreeSet::new();
    for template in &inputs {
        let rows = record.checkpoints.iter().filter(|row|record.active_checkpoints.contains(&row.id)
            && checkpoint_record_matches_template(row, template, &record.id)).collect::<Vec<_>>();
        let [row] = rows.as_slice() else { return Err("withdrawal result input has no unique active row".into()); };
        if !topology.checkpoints.contains(template) { return Err("withdrawal changed its pending result template".into()); }
        removed.insert(row.id.to_string());
    }
    topology.checkpoints.retain(|template|!inputs.contains(&template));
    value["interface_state"][config.provider_interface.as_str()][RUN_AUTHORING_BINDING] = serde_json::to_value(topology).map_err(|error|error.to_string())?;
    replace_owned_frontier(value.as_object_mut().ok_or("withdrawal is not an object")?, &removed, Vec::new(), &[])
}

pub(super) fn saved_attempt_config(
    record: &RunRecord,
    declared: &RunAuthoringConfig,
) -> Result<RunAuthoringConfig, String> {
    let actual = attempt_config(record, declared)?;
    let mut expected = declared.clone();
    let mut observed = actual.clone();
    expected
        .checkpoints
        .retain(|template| template.role != RunAuthoringCheckpointRole::ResultInput);
    observed
        .checkpoints
        .retain(|template| template.role != RunAuthoringCheckpointRole::ResultInput);
    if expected != observed {
        return Err(
            "saved authoring attempt changed its declared contract outside result inputs".into(),
        );
    }
    Ok(actual)
}

pub(crate) fn definition_value(
    current: &[u8],
    writer: &CheckedRunWriter,
    checkpoints: &[CheckpointId],
    context: Option<&[ArtifactBinding]>,
) -> Result<serde_json::Value, String> {
    let record = RunRecord::from_bytes(current).map_err(|error| error.to_string())?;
    refuse_selected_consent_state(&record, &writer.owner)?;
    let mut topology = validate_declared_provider_writer(&record, writer)?;
    let config = topology.config(&RunAuthoringProfile {
        file_write_tool: writer.tool.clone(),
        file_write_label: writer.call_label.clone(),
        file_write_result_label: writer.result_label.clone(),
        result_timeout_ms: 1,
    });
    validate_bound_run_authoring_topology(&record, &config, None)?;
    if let Some(bindings) = context {
        if current_context_bindings(&record, &config.checkpoints)? != bindings {
            return Err(
                "result revision must explicitly bind the exact current context and rights".into(),
            );
        }
    }
    let selected = checkpoints.iter().collect::<BTreeSet<_>>();
    if selected.is_empty() || selected.len() != checkpoints.len() {
        return Err("result retirement requires distinct explicit checkpoint ids".into());
    }
    let mut names = BTreeSet::new();
    for id in &selected {
        let checkpoint = record
            .checkpoint(id)
            .filter(|_| record.active_checkpoints.contains(id))
            .ok_or("result retirement selected an absent receiving checkpoint")?;
        let templates = topology
            .checkpoints
            .iter()
            .filter(|template| {
                template.role == RunAuthoringCheckpointRole::ResultInput
                    && checkpoint_record_matches_template(checkpoint, template, &record.id)
            })
            .collect::<Vec<_>>();
        let [template] = templates.as_slice() else {
            return Err(
                "result retirement selected a checkpoint outside its exact result inputs".into(),
            );
        };
        names.insert(template.name.clone());
    }
    if context.is_none() {
        topology
            .checkpoints
            .retain(|template| !names.contains(&template.name));
    }
    let generation = optional_u64(
        &record.interface_state[&writer.owner],
        "authoring_generation",
    )?
    .checked_add(1)
    .ok_or("result retirement generation overflow")?;
    let mut value: serde_json::Value =
        serde_json::from_slice(current).map_err(|error| error.to_string())?;
    if let Some(context) = context {
        for id in selected {
            let checkpoint = record
                .checkpoint(id)
                .ok_or("result revision checkpoint absent")?;
            let template = config
                .checkpoints
                .iter()
                .find(|template| {
                    names.contains(&template.name)
                        && checkpoint_record_matches_template(checkpoint, template, &record.id)
                })
                .ok_or("result revision template absent")?;
            let call = correspondence(template)?;
            topology.checkpoints.iter_mut().find(|next| next.name == template.name)
                .ok_or("result revision declared template absent")?.static_artifact_bindings =
                result_static_bindings(&config, Some(context), &call.call_artifact);
            let mut bindings = context.to_vec();
            if let Some(binding) = bindings
                .iter()
                .find(|binding| binding.id == call.call_artifact.id)
            {
                if binding.access != Default::default() {
                    return Err(
                        "result revision changed the original call's read-only access".into(),
                    );
                }
            } else {
                bindings.push(ArtifactBinding {
                    id: call.call_artifact.id,
                    access: Default::default(),
                });
            }
            let next_id = format!(
                "cp_result_revision_{}_{}_{}",
                record.id, generation, template.name
            );
            if record.checkpoint(&CheckpointId::new(&next_id)).is_some() {
                return Err("result revision reused a checkpoint identity".into());
            }
            input_revision::rename_checkpoint(&mut value, &writer.owner, id, &next_id, true)?;
            let row = value["checkpoints"]
                .as_array_mut()
                .ok_or("result revision checkpoints absent")?
                .iter_mut()
                .find(|row| row["id"].as_str() == Some(&next_id))
                .ok_or("result revision replacement absent")?;
            row["artifact_bindings"] =
                serde_json::to_value(bindings).map_err(|error| error.to_string())?;
        }
    } else {
        let removed = checkpoints.iter().map(ToString::to_string).collect();
        replace_owned_frontier(
            value
                .as_object_mut()
                .ok_or("result retirement is not an object")?,
            &removed,
            Vec::new(),
            &[],
        )?;
    }
    value["interface_state"][writer.owner.as_str()][RUN_AUTHORING_BINDING] =
        serde_json::to_value(&topology).map_err(|error| error.to_string())?;
    value["interface_state"][writer.owner.as_str()]["authoring_generation"] = generation.into();
    if config.writer_run.is_none() {
        for role in [
            RunAuthoringCheckpointRole::AuthoringCall,
            RunAuthoringCheckpointRole::AuthoringResult,
        ] {
            let checkpoint = active_checkpoint_for_role(&record, &config, role)?;
            let template = config.checkpoint_for_role(role)?;
            let id = format!(
                "cp_result_retirement_{}_{}_{}",
                record.id, generation, template.name
            );
            if record.checkpoint(&CheckpointId::new(&id)).is_some() {
                return Err("result retirement reused a checkpoint identity".into());
            }
            input_revision::rename_checkpoint(
                &mut value,
                &writer.owner,
                &checkpoint.id,
                &id,
                true,
            )?;
        }
    }
    let after =
        RunRecord::from_bytes(&serde_json::to_vec(&value).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
    let next = topology.config(&RunAuthoringProfile {
        file_write_tool: writer.tool.clone(),
        file_write_label: writer.call_label.clone(),
        file_write_result_label: writer.result_label.clone(),
        result_timeout_ms: 1,
    });
    next.validate(&writer.owner, &after.id)?;
    validate_bound_run_authoring_topology(&after, &next, None)?;
    Ok(value)
}

pub(super) fn definition_transition(transition: &CheckedRunTransition) -> Result<bool, String> {
    definition_transition_between(&transition.before, &transition.after, &transition.writer)
}

fn definition_transition_between(
    before: &[u8],
    after: &[u8],
    writer: &CheckedRunWriter,
) -> Result<bool, String> {
    if invocation::definition(before, after, writer)?.is_some() {
        return Ok(true);
    }
    let original = before;
    let replacement = after;
    let before = RunRecord::from_bytes(before).map_err(|error| error.to_string())?;
    let after = RunRecord::from_bytes(after).map_err(|error| error.to_string())?;
    let Some(value) = before
        .interface_state
        .get(&writer.owner)
        .and_then(|state| state.get(RUN_AUTHORING_BINDING))
    else {
        return Ok(false);
    };
    let old: BoundRunAuthoringTopology =
        serde_json::from_value(value.clone()).map_err(|error| error.to_string())?;
    let Some(value) = after
        .interface_state
        .get(&writer.owner)
        .and_then(|state| state.get(RUN_AUTHORING_BINDING))
    else {
        return Ok(false);
    };
    let next: BoundRunAuthoringTopology =
        serde_json::from_value(value.clone()).map_err(|error| error.to_string())?;
    let retains_call = |template: &RunAuthoringCheckpoint| next.checkpoints.iter().any(|replacement| {
        let mut replacement = replacement.clone();
        replacement.static_artifact_bindings = template.static_artifact_bindings.clone();
        replacement == *template
    });
    let removed = old
        .checkpoints
        .iter()
        .filter(|template| {
            template.role == RunAuthoringCheckpointRole::ResultInput
                && !retains_call(template)
        })
        .collect::<Vec<_>>();
    if persisted_pending_handoff(&before, &writer.owner)?
        != persisted_pending_handoff(&after, &writer.owner)?
    {
        return Ok(false);
    }
    let revised = old
        .checkpoints
        .iter()
        .filter(|template| {
            template.role == RunAuthoringCheckpointRole::ResultInput
                && retains_call(template)
                && (!next.checkpoints.contains(template) || before
                    .checkpoints
                    .iter()
                    .filter(|checkpoint| {
                        before.active_checkpoints.contains(&checkpoint.id)
                            && checkpoint_record_matches_template(checkpoint, template, &before.id)
                    })
                    .ne(after.checkpoints.iter().filter(|checkpoint| {
                        after.active_checkpoints.contains(&checkpoint.id)
                            && checkpoint_record_matches_template(checkpoint, template, &after.id)
                    })))
        })
        .collect::<Vec<_>>();
    if removed.is_empty() && revised.is_empty() {
        return Ok(false);
    }
    if !removed.is_empty() && !revised.is_empty() {
        return Err("result definition edit cannot mix retirement and context revision".into());
    }
    let context = (!revised.is_empty())
        .then(|| provider_context_bindings(&before, &writer.owner))
        .transpose()?;
    let selected = if revised.is_empty() {
        &removed
    } else {
        &revised
    };
    let ids = before
        .checkpoints
        .iter()
        .filter(|checkpoint| {
            before.active_checkpoints.contains(&checkpoint.id)
                && selected.iter().any(|template| {
                    checkpoint_record_matches_template(checkpoint, template, &before.id)
                })
        })
        .map(|checkpoint| checkpoint.id.clone())
        .collect::<Vec<_>>();
    let expected = definition_value(original, writer, &ids, context.as_deref())?;
    if expected
        != serde_json::from_slice::<serde_json::Value>(replacement)
            .map_err(|error| error.to_string())?
    {
        return Err(
            "checked result definition edit changed context, pending effects or neighboring state"
                .into(),
        );
    }
    Ok(true)
}

pub(super) fn saved_definition_transition(
    before: &RunRecord,
    after: &RunRecord,
    config: &RunAuthoringConfig,
) -> Result<bool, String> {
    if !config
        .checkpoints
        .iter()
        .any(|template| template.role == RunAuthoringCheckpointRole::ResultInput)
    {
        return Ok(false);
    }
    let writer = CheckedRunWriter {
        owner: config.provider_interface.clone(),
        source: config.authoring_source_interface.clone(),
        door: config
            .checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringResult)?
            .interface()
            .clone(),
        tool: config.file_write_tool.clone(),
        call_label: config.file_write_label.clone(),
        result_label: config.file_write_result_label.clone(),
    };
    definition_transition_between(
        &serde_json::to_vec(before).map_err(|error| error.to_string())?,
        &serde_json::to_vec(after).map_err(|error| error.to_string())?,
        &writer,
    )
}

pub(super) fn retains_invocation(handoff: &ProviderOutputHandoff, record: &RunRecord) -> bool {
    handoff
        .invocation_metadata
        .runtime
        .as_ref()
        .is_some_and(|runtime| {
            record.active_checkpoints.contains(&runtime.checkpoint)
                && record
                    .checkpoint(&runtime.checkpoint)
                    .is_some_and(|checkpoint| {
                        handoff.config.checkpoints.iter().any(|template| {
                            template.role == RunAuthoringCheckpointRole::ResultInput
                                && checkpoint_record_matches_template(
                                    checkpoint, template, &record.id,
                                )
                        })
                    })
        })
}

pub(super) fn validate_source_advance(
    handoff: &ProviderOutputHandoff,
    before: &RunRecord,
    after: &RunRecord,
    after_bytes: &[u8],
) -> Result<(), String> {
    if !retains_invocation(handoff, before) {
        return Ok(());
    }
    let runtime = handoff
        .invocation_metadata
        .runtime
        .as_ref()
        .ok_or("result source coordinate absent")?;
    if before.checkpoint(&runtime.checkpoint) == after.checkpoint(&runtime.checkpoint)
        && after.active_checkpoints.contains(&runtime.checkpoint)
    {
        return Ok(());
    }
    let consumed = persisted_pending_handoff(after, &handoff.config.provider_interface)?
        .is_some_and(|pending| pending.source_movement_seq == handoff.source_movement_seq)
        && after.checkpoint(&runtime.checkpoint).is_none();
    if !consumed {
        if source_was_bridged(
            &handoff.completed_source_chain(after_bytes)?,
            handoff.source_movement_seq,
        )?
        .is_some()
        {
            return Ok(());
        }
        return Err("result source was explicitly retired or revised; its admitted generation cannot continue".into());
    }
    Ok(())
}

fn correspondence(template: &RunAuthoringCheckpoint) -> Result<Correspondence, String> {
    let [required] = template.required_verified_artifacts.as_slice() else {
        return Err("result input must have one exact correspondence requirement".into());
    };
    let value: Correspondence = serde_json::from_value(required.params.clone())
        .map_err(|error| format!("result input correspondence is malformed: {error}"))?;
    non_empty_string(
        value.call_artifact.id.as_str(),
        "result input call artifact",
    )?;
    non_empty_string(&value.tool, "result input tool")?;
    non_empty_string(&value.correlation_id, "result input correlation")?;
    if !value.params.is_object() {
        return Err("result input verifier params must be an object".into());
    }
    if let Some(invocation) = &value.invocation {
        invocation.validate()?;
    }
    Ok(value)
}

fn result_static_bindings(config: &RunAuthoringConfig, context: Option<&[ArtifactBinding]>, call: &ArtifactRef) -> Vec<ArtifactId> {
    let mut bindings = Vec::new();
    for template in config.checkpoints.iter().filter(|template| template.role == RunAuthoringCheckpointRole::Stable && template.include_context) {
        for id in &template.static_artifact_bindings { append_artifact(&mut bindings, id.clone()); }
    }
    if context.is_none_or(|context| !context.iter().any(|binding| binding.id == call.id)) {
        append_artifact(&mut bindings, call.id.clone());
    }
    bindings
}

pub(super) fn bound_call_evidence(record: &RunRecord, provider: &InterfaceId, checkpoint: &CheckpointId)
    -> Result<Option<ArtifactRef>, String> {
    let Some(topology) = record.interface_state.get(provider).and_then(|state|state.get(RUN_AUTHORING_BINDING))
        .map(|value|serde_json::from_value::<BoundRunAuthoringTopology>(value.clone()).map_err(|error|error.to_string())).transpose()?
        else { return Ok(None); };
    topology.validate_identity(record, provider)?;
    let checkpoint = record.checkpoint(checkpoint).ok_or("result context has no pinned checkpoint")?;
    let templates = topology.checkpoints.iter().filter(|template| template.role == RunAuthoringCheckpointRole::ResultInput
        && checkpoint_record_matches_template(checkpoint, template, &record.id)).collect::<Vec<_>>();
    let template = match templates.as_slice() {
        [] => return Ok(None),
        [template] => *template,
        _ => return Err("result context has ambiguous call evidence".into()),
    };
    let call = correspondence(template)?.call_artifact;
    if !template.static_artifact_bindings.contains(&call.id) { return Ok(None); }
    if checkpoint.artifact_bindings.iter().filter(|binding|binding.id == call.id)
            .ne(std::iter::once(&ArtifactBinding {id:call.id.clone(),access:Default::default()})) {
        return Err("result context changed its exact read-only call evidence".into());
    }
    Ok(Some(call))
}

pub(super) fn validate_template(
    template: &RunAuthoringCheckpoint,
    config: &RunAuthoringConfig,
) -> Result<(), String> {
    if !template.result_receiving.is_empty() {
        let tool_output = template.role == RunAuthoringCheckpointRole::ProviderToolOutput;
        if !tool_output && template.role != RunAuthoringCheckpointRole::ProviderInterfaceOutput {
            return Err("result receiving contracts belong to provider tool or interface outputs".into());
        }
        if tool_output
            && template.result_receiving.keys().cloned().collect::<BTreeSet<_>>()
                != checkpoint_tool_ids(template)?
        {
            return Err("result receiving contracts must cover the exact declared tool set".into());
        }
        for (key, required) in &template.result_receiving {
            required.validate("result receiving contract")?;
            let label = if tool_output { format!("{key}.result") } else { key.clone() };
            if required.artifact_label != label || !required.params.is_object() {
                return Err(
                    "result receiving requires the exact declared result label and object params"
                        .into(),
                );
            }
        }
        let door = &template.outbound_interface_bindings[0].interface;
        if config.checkpoints.iter().any(|input| {
            input.role.is_provider_input()
                && input.role != RunAuthoringCheckpointRole::ResultInput
                && input.interface() == door
                && template
                    .result_receiving
                    .values()
                    .any(|required| input.interface_binding.admits(&required.artifact_label))
        }) {
            return Err(
                "result receiving door retains another live provider input template".into(),
            );
        }
    }
    if template.role != RunAuthoringCheckpointRole::ResultInput {
        return Ok(());
    }
    if !template.include_context
        || template.interface() == &config.provider_interface
        || template.owner_authoring.is_some()
        || template.outbound_interface_bindings.len() != 1
        || template.outbound_interfaces().next() != Some(&config.provider_interface)
    {
        return Err(
            "result input must carry its captured context through one provider edge".into(),
        );
    }
    let call = correspondence(template)?;
    if (template.interface() == &config.authoring_source_interface) != call.invocation.is_some() {
        return Err(
            "result invocation bridge must use its exact authoring-source interface".into(),
        );
    }
    let required = &template.required_verified_artifacts[0];
    let declared = config.checkpoints.iter().any(|output| {
        output.role == RunAuthoringCheckpointRole::ProviderInterfaceOutput
            && output.outbound_interfaces().eq([template.interface()])
            && output.result_receiving.contains_key(&required.artifact_label)
    });
    if (!declared && required.artifact_label != format!("{}.result", call.tool))
        || !binding_admits_exact_name(&template.interface_binding, &required.artifact_label)
        || !binding_admits_exact_name(
            &template.outbound_interface_bindings[0],
            &required.artifact_label,
        )
    {
        return Err("result input must preserve its exact result name on both bindings".into());
    }
    Ok(())
}

fn template_for_call(
    output: &RunAuthoringCheckpoint,
    reference: ArtifactRef,
    payload: &[u8],
    config: &RunAuthoringConfig,
    call_in_context: bool,
    result_label: Option<&str>,
) -> Result<RunAuthoringCheckpoint, String> {
    let (subject, correlation_id, key) = if output.role == RunAuthoringCheckpointRole::ProviderInterfaceOutput {
        let call: OperationCall = serde_json::from_slice(payload)
            .map_err(|error| format!("result receiving operation call is malformed: {error}"))?;
        let label = match result_label {
            Some(label) => label.to_string(),
            None => match output.result_receiving.keys().collect::<Vec<_>>().as_slice() {
                [label] => (*label).clone(),
                _ => return Err("operation call has no single selected result label".into()),
            },
        };
        (call.operation, Some(call.correlation_id), label)
    } else {
        let call: octopus_tool_handlers::ToolCallArtifact = serde_json::from_slice(payload)
            .map_err(|error| format!("result receiving call is malformed: {error}"))?;
        (call.tool.clone(), call.correlation_id, call.tool)
    };
    non_empty_string(&subject, "result receiving call subject")?;
    let contract = output
        .result_receiving
        .get(&key)
        .ok_or("result receiving call has no declared result contract")?;
    let correlation_id = correlation_id.ok_or("result receiving call has no correlation")?;
    non_empty_string(&correlation_id, "result receiving call correlation")?;
    let mut required = contract.clone();
    let mut static_artifact_bindings = result_static_bindings(config, None, &reference);
    if call_in_context {
        static_artifact_bindings.retain(|id| id != &reference.id);
    }
    required.params = serde_json::to_value(Correspondence {
        call_artifact: reference,
        tool: subject,
        correlation_id,
        params: contract.params.clone(),
        invocation: None,
    })
    .map_err(|error| error.to_string())?;
    let binding = |interface| AuthoredInterfaceBinding {
        interface,
        config: InterfaceBindingConfig {
            artifact_policy: BindingArtifactPolicy::Allow {
                names: vec![required.artifact_label.clone()],
                namespaces: Vec::new(),
            },
        },
    };
    let interface_binding = binding(output.outbound_interface_bindings[0].interface.clone());
    let outbound_interface_bindings = vec![binding(config.provider_interface.clone())];
    let required_verified_artifacts = vec![required];
    Ok(RunAuthoringCheckpoint {
        name: derived_checkpoint_name(
            RunAuthoringCheckpointRole::ResultInput,
            &interface_binding,
            &outbound_interface_bindings,
            true,
            &required_verified_artifacts,
        )?,
        role: RunAuthoringCheckpointRole::ResultInput,
        result_receiving: BTreeMap::new(),
        static_artifact_bindings,
        owner_authoring: None,
        interface_binding,
        outbound_interface_bindings,
        include_context: true,
        required_verified_artifacts,
    })
}

pub(super) fn prepare(
    handoff: &ProviderOutputHandoff,
    record: &RunRecord,
    outputs: &[PreparedHandoff],
    terminal_context: Option<&[ArtifactId]>,
    topology: &mut BoundRunAuthoringTopology,
) -> Result<Option<CheckpointId>, String> {
    let consumed = handoff
        .invocation_metadata
        .runtime
        .as_ref()
        .and_then(|runtime| {
            record
                .checkpoint(&runtime.checkpoint)
                .filter(|checkpoint| record.active_checkpoints.contains(&checkpoint.id))
        })
        .filter(|checkpoint| {
            handoff.config.checkpoints.iter().any(|template| {
                template.role == RunAuthoringCheckpointRole::ResultInput
                    && checkpoint_record_matches_template(checkpoint, template, &record.id)
            })
        });
    if let Some(checkpoint) = consumed {
        if !metadata_matches_checkpoint(
            &handoff.invocation_metadata,
            handoff
                .config
                .checkpoints
                .iter()
                .find(|template| {
                    template.role == RunAuthoringCheckpointRole::ResultInput
                        && checkpoint_record_matches_template(checkpoint, template, &record.id)
                })
                .ok_or("result input template absent")?,
        ) || !artifact_refs_match_bindings(
            &handoff.invocation_metadata.artifact_binding_refs,
            &checkpoint.artifact_bindings,
        ) {
            return Err("result retirement changed its admitted binding evidence".into());
        }
        let retained = topology.checkpoints.clone();
        topology.checkpoints.retain(|template| {
            template.role != RunAuthoringCheckpointRole::ResultInput
                || !checkpoint_record_matches_template(checkpoint, template, &record.id)
                || rearms(template, &handoff.config.checkpoints, &retained)
        });
    }
    for output in outputs
        .iter()
        .filter(|output| !output.output_checkpoint.result_receiving.is_empty())
    {
        let path = handoff
            .var_dir
            .join(&output.identity.manifest_relative_path);
        let file = InterfaceArtifactFile::new(&path).map_err(|error| error.to_string())?;
        if artifact_identity(&file, &handoff.var_dir)? != output.identity {
            return Err("result receiving call changed its saved artifact".into());
        }
        let template = template_for_call(
            &output.output_checkpoint,
            output.identity.artifact.clone(),
            &file
                .read_payload_bytes()
                .map_err(|error| error.to_string())?,
            &handoff.config,
            terminal_context.is_some_and(|context| context.contains(&output.identity.artifact.id)),
            None,
        )?;
        if !handoff
            .granted_mark_outcomes
            .contains(template.required_verified_artifacts[0].mark.as_str())
        {
            return Err("result receiving mark is outside the admitted invocation grant".into());
        }
        let call = correspondence(&template)?;
        for previous in topology
            .checkpoints
            .iter()
            .filter(|template| template.role == RunAuthoringCheckpointRole::ResultInput)
        {
            let prior = correspondence(previous)?;
            if prior.call_artifact == call.call_artifact
                || (prior.tool == call.tool && prior.correlation_id == call.correlation_id)
            {
                return Err("result receiving call repeats an outstanding correspondence".into());
            }
        }
        topology.checkpoints.push(template);
    }
    Ok(consumed.map(|checkpoint| checkpoint.id.clone()))
}

pub(super) fn materialize(
    root: &mut serde_json::Map<String, serde_json::Value>,
    before: &RunRecord,
    config: &RunAuthoringConfig,
    next: &RunAuthoringConfig,
    prefix: &str,
    consumed: Option<&CheckpointId>,
    deferred_context: Option<&[ArtifactBinding]>,
) -> Result<(), String> {
    let rearmed = consumed
        .and_then(|id| before.checkpoint(id))
        .and_then(|row| {
            next.checkpoints.iter().find(|template| {
                template.role == RunAuthoringCheckpointRole::ResultInput
                    && checkpoint_record_matches_template(row, template, &before.id)
            })
        });
    if consumed.is_none()
        && !next.checkpoints.iter().any(|template| {
            template.role == RunAuthoringCheckpointRole::ResultInput
                && !config.checkpoints.contains(template)
        })
    {
        return Ok(());
    }
    let context = match deferred_context {
        Some(context) => context.to_vec(),
        None => current_context_bindings(
            &RunRecord::from_bytes(&serde_json::to_vec(root).map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())?,
            &config.checkpoints,
        )?,
    };
    let mut rows = Vec::new();
    let mut ids = Vec::new();
    for template in next
        .checkpoints
        .iter()
        .filter(|template| {
            template.role == RunAuthoringCheckpointRole::ResultInput
                && !config.checkpoints.contains(template)
        })
        .chain(rearmed)
    {
        let call = correspondence(template)?;
        let mut bindings = context.clone();
        if !bindings
            .iter()
            .any(|binding| binding.id == call.call_artifact.id)
        {
            bindings.push(ArtifactBinding {
                id: call.call_artifact.id,
                access: Default::default(),
            });
        }
        let bindings = bindings
            .iter()
            .map(serde_json::to_value)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        let id = format!("{prefix}_result_{}", template.name);
        if before.checkpoint(&CheckpointId::new(&id)).is_some() {
            return Err("result input reused a checkpoint identity".into());
        }
        rows.push(materialize_checkpoint(template, &id, &before.id, &bindings));
        ids.push(id);
    }
    let removed = consumed.into_iter().map(ToString::to_string).collect();
    replace_owned_frontier(root, &removed, rows, &ids)
}

pub(super) fn validate_transition(
    before: &RunRecord,
    after: &RunRecord,
    config: &RunAuthoringConfig,
) -> Result<(), String> {
    let old = attempt_config(before, config)?;
    let next = attempt_config(after, config)?;
    let pending = persisted_pending_handoff(after, &config.provider_interface)?;
    let rows = active_rows;
    for template in old
        .checkpoints
        .iter()
        .filter(|template| template.role == RunAuthoringCheckpointRole::ResultInput)
    {
        if next.checkpoints.contains(template) && rows(before, template) != rows(after, template) {
            if !rearms(template, &old.checkpoints, &next.checkpoints) {
                return Err("outstanding result input changed before consumption".into());
            }
            let call = correspondence(template)?;
            let mut expected = effective_context_bindings(after, &old.checkpoints, &config.provider_interface)?;
            if !expected.iter().any(|binding| binding.id == call.call_artifact.id) {
                expected.push(ArtifactBinding { id: call.call_artifact.id.clone(), access: Default::default() });
            }
            let (prior, rearmed) = (rows(before, template), rows(after, template));
            let ([prior], [row]) = (prior.as_slice(), rearmed.as_slice()) else {
                return Err("re-armed result input has no unique replaced generation".into());
            };
            if after.checkpoint(&prior.id).is_some()
                || before.checkpoint(&row.id).is_some()
                || row.epoch.0 != 0
                || row.artifact_bindings != expected
            {
                return Err("re-armed result input differs from a fresh generation of its exact wait".into());
            }
        }
    }
    for template in next.checkpoints.iter().filter(|template| {
        template.role == RunAuthoringCheckpointRole::ResultInput
            && !old.checkpoints.contains(template)
    }) {
        let call = correspondence(template)?;
        let pending = pending
            .as_ref()
            .ok_or("result input has no saved call handoff")?;
        let matches = pending
            .items
            .iter()
            .filter(|item| {
                item.artifact_id == call.call_artifact.id
                    && item.artifact_content_hash == call.call_artifact.content.to_hex()
                    && item.checkpoint.outbound_interface_bindings.len() == 1
                    && item.checkpoint.outbound_interface_bindings[0].interface
                        == *template.interface()
            })
            .count();
        let added = rows(after, template);
        let [checkpoint] = added.as_slice() else {
            return Err("result call has no unique receiving checkpoint".into());
        };
        provider_context_bindings(after, &config.provider_interface)?;
        let context = effective_context_bindings(after, &old.checkpoints, &config.provider_interface)?;
        let mut expected = context.clone();
        if !expected.iter().any(|binding| binding.id == call.call_artifact.id) {
            expected.push(ArtifactBinding {
                id: call.call_artifact.id.clone(),
                access: Default::default(),
            });
        }
        if matches != 1
            || checkpoint.artifact_bindings != expected
            || template.static_artifact_bindings != result_static_bindings(&old, Some(&context), &call.call_artifact)
            || checkpoint.epoch.0 != 0
            || before.checkpoint(&checkpoint.id).is_some()
        {
            return Err(
                "result input differs from its exact call and explicitly captured context".into(),
            );
        }
    }
    Ok(())
}

pub(super) fn validate_saved(
    ctx: &InterfaceLiftContext,
    before: &RunRecord,
    after: &RunRecord,
    config: &RunAuthoringConfig,
) -> Result<(), String> {
    if saved_definition_transition(before, after, config)? {
        invocation::validate_saved(ctx, before, after, config)?;
        return Ok(());
    }
    validate_transition(before, after, config)?;
    let old = attempt_config(before, config)?;
    let next = attempt_config(after, config)?;
    let pending = persisted_pending_handoff(after, &config.provider_interface)?;
    for template in next.checkpoints.iter().filter(|template| {
        template.role == RunAuthoringCheckpointRole::ResultInput
            && !old.checkpoints.contains(template)
    }) {
        let call = correspondence(template)?;
        let item = pending
            .as_ref()
            .and_then(|pending| {
                pending
                    .items
                    .iter()
                    .find(|item| item.artifact_id == call.call_artifact.id)
            })
            .ok_or("saved result input lost its call")?;
        let (file, reference) = load_pending_artifact(&ctx.interface_var_dir(), item)?;
        let output = receiving_output(&old.checkpoints, template, &call)
            .map_err(|_| "saved result input lost its declared output".to_string())?;
        let call_in_context = effective_context_bindings(after, &old.checkpoints, &config.provider_interface)?
            .iter().any(|binding| binding.id == reference.id);
        let expected = template_for_call(
            output,
            reference,
            &file
                .read_payload_bytes()
                .map_err(|error| error.to_string())?,
            &old,
            call_in_context,
            Some(&template.required_verified_artifacts[0].artifact_label),
        )?;
        if expected != *template {
            return Err("saved result input changed its exact original call".into());
        }
    }
    if enabled(config)
        || old.checkpoints.iter().any(|template| {
            template.role == RunAuthoringCheckpointRole::ResultInput
                && !next.checkpoints.contains(template)
        })
        || next.checkpoints.iter().any(|template| {
            template.role == RunAuthoringCheckpointRole::ResultInput
                && !old.checkpoints.contains(template)
        })
    {
        let limits = ctx.required_host_io_limits()?;
        let evidence = CheckedRunEvidence {
            movements_path: &ctx.movements_path,
            ledger_index_path: &ctx.ledger_index_path,
            blobs_dir: &ctx.blobs_dir,
            max_control_frame_bytes: limits.max_control_frame_bytes,
            max_manifest_bytes: limits.max_manifest_bytes,
            max_payload_bytes: limits.max_payload_bytes,
        };
        let mut reader = FileCheckedRunReader::open(&evidence, None)?.with_provider_context(ctx);
        validate_snapshot_publication(&mut reader, before, after, &old)?;
        validate_new_marks(&mut reader, before, after, &old, &next, None)?;
        validate_retirement(&mut reader, before, after, &old, &next, None)?;
    }
    Ok(())
}

pub(super) fn validate_invocation(
    input: &InterfaceInvocationEnvelope,
    template: &RunAuthoringCheckpoint,
) -> Result<(), String> {
    if template.role != RunAuthoringCheckpointRole::ResultInput {
        return Ok(());
    }
    let call = correspondence(template)?;
    if let Some(invocation) = &call.invocation {
        let metadata = input.metadata().map_err(|error| error.to_string())?;
        if input.artifact != invocation.artifact
            || metadata
                .artifact_binding_refs
                .iter()
                .filter(|reference| reference.id == invocation.source.artifact.id)
                .ne(std::iter::once(&invocation.source.artifact))
            || !metadata
                .grant
                .artifact_access
                .get(invocation.source.artifact.id.as_str())
                .is_some_and(|access| {
                    access.read && !access.change && !access.create && !access.delete
                })
        {
            return Err("result invocation differs from its exact checked bridge".into());
        }
    }
    let originals = input
        .artifact_binding_payloads
        .iter()
        .filter(|bound| bound.artifact == call.call_artifact)
        .collect::<Vec<_>>();
    let [original] = originals.as_slice() else {
        return Err("result invocation has no exact bound call artifact".into());
    };
    let original = call_identity(&original.read_bytes().map_err(|error| error.to_string())?)?;
    let result = result_identity(
        &input
            .read_artifact_payload()
            .map_err(|error| error.to_string())?,
    )?;
    if original.0 != call.tool
        || original.1.as_deref() != Some(&call.correlation_id)
        || result.0 != call.tool
        || result.1.as_deref() != Some(&call.correlation_id)
        || result.2 != template.required_verified_artifacts[0].artifact_label
    {
        return Err("result invocation differs from its bound original call".into());
    }
    Ok(())
}

pub(super) fn validate_checked(
    reader: &mut impl CheckedRunEvidenceReader,
    transition: &CheckedRunTransition,
) -> Result<(), String> {
    if definition_transition(transition)? {
        invocation::validate_checked(reader, transition)?;
        return Ok(());
    }
    let before = RunRecord::from_bytes(&transition.before).map_err(|error| error.to_string())?;
    let after = RunRecord::from_bytes(&transition.after).map_err(|error| error.to_string())?;
    let Some(value) = before
        .interface_state
        .get(&transition.writer.owner)
        .and_then(|state| state.get(RUN_AUTHORING_BINDING))
    else {
        return Ok(());
    };
    let topology: BoundRunAuthoringTopology =
        serde_json::from_value(value.clone()).map_err(|error| error.to_string())?;
    let config = topology.config(&RunAuthoringProfile {
        file_write_tool: transition.writer.tool.clone(),
        file_write_label: transition.writer.call_label.clone(),
        file_write_result_label: transition.writer.result_label.clone(),
        result_timeout_ms: 1,
    });
    validate_transition(&before, &after, &config)?;
    let next = attempt_config(&after, &config)?;
    let pending = persisted_pending_handoff(&after, &transition.writer.owner)?;
    for template in next.checkpoints.iter().filter(|template| {
        template.role == RunAuthoringCheckpointRole::ResultInput
            && !topology.checkpoints.contains(template)
    }) {
        let call = correspondence(template)?;
        let item = pending
            .as_ref()
            .and_then(|pending| {
                pending
                    .items
                    .iter()
                    .find(|item| item.artifact_id == call.call_artifact.id)
            })
            .ok_or("checked result input lost its pending call")?;
        let output = receiving_output(&config.checkpoints, template, &call)
            .map_err(|_| "checked result input has no exact declared output".to_string())?;
        let payload = reader.pending_output_payload(
            &config.provider_interface,
            &call.call_artifact,
            &item.manifest_relative_path,
        )?;
        let call_in_context = effective_context_bindings(&after, &config.checkpoints, &config.provider_interface)?
            .iter().any(|binding| binding.id == call.call_artifact.id);
        let expected = template_for_call(
            output,
            call.call_artifact.clone(),
            &payload,
            &config,
            call_in_context,
            Some(&template.required_verified_artifacts[0].artifact_label),
        )?;
        if expected != *template {
            return Err("checked result input changed its original call".into());
        }
    }
    validate_snapshot_publication(reader, &before, &after, &config)?;
    validate_new_marks(
        reader,
        &before,
        &after,
        &config,
        &next,
        Some(transition.call.seq.0),
    )?;
    validate_retirement(
        reader,
        &before,
        &after,
        &config,
        &next,
        Some(transition.call.seq.0),
    )
}

fn validate_snapshot_publication(
    reader: &mut impl CheckedRunEvidenceReader,
    before: &RunRecord,
    after: &RunRecord,
    config: &RunAuthoringConfig,
) -> Result<(), String> {
    if !enabled(config) {
        return Ok(());
    }
    let Some(pending) = persisted_pending_handoff(after, &config.provider_interface)? else {
        return Ok(());
    };
    let Some(target) = &pending.context_publication else {
        return Ok(());
    };
    let context = current_context_bindings(before, &config.checkpoints)?;
    let mut static_context = current_mandatory_role_artifacts(before, config)?;
    static_context.extend(
        config
            .payload_artifact_bindings
            .iter()
            .map(|reference| reference.id.clone()),
    );
    let displaced = context
        .iter()
        .filter(|binding| !static_context.contains(&binding.id) && !target.contains(&binding.id))
        .map(|binding| binding.id.clone())
        .collect::<Vec<_>>();
    if displaced.is_empty() {
        return Ok(());
    }
    let mut snapshot_found = false;
    for item in pending
        .items
        .iter()
        .filter(|item| target.contains(&item.artifact_id))
    {
        let source = ArtifactRef {
            id: item.artifact_id.clone(),
            content: ContentHash::from_hex(&item.artifact_content_hash)
                .ok_or("result continuation snapshot has no exact pending content")?,
        };
        let mut payload = reader.pending_output_payload(
            &config.provider_interface,
            &source,
            &item.manifest_relative_path,
        )?;
        let mut value: serde_json::Value =
            serde_json::from_slice(&payload).map_err(|error| error.to_string())?;
        if value.get(SESSION_CONTEXT_FIELD).is_none() {
            continue;
        }
        snapshot_found = true;
        let mut included = BTreeSet::new();
        let mut seen = BTreeSet::from([artifact_ref_key(&source)]);
        loop {
            let snapshot: SessionContextSnapshot = serde_json::from_value(
                value
                    .get(SESSION_CONTEXT_FIELD)
                    .ok_or("result continuation snapshot parent has no context")?
                    .clone(),
            )
            .map_err(|error| error.to_string())?;
            validate_session_snapshot(
                &snapshot,
                "result continuation",
                &config.provider_interface,
            )?;
            included.extend(
                snapshot
                    .artifacts
                    .iter()
                    .map(|entry| entry.source_artifact.id.clone()),
            );
            let Some(parent) = snapshot.parent else {
                break;
            };
            if !seen.insert(artifact_ref_key(&parent)) {
                return Err("result continuation snapshot has cyclic parents".into());
            }
            included.insert(parent.id.clone());
            payload = reader.artifact(&parent)?.1;
            value = serde_json::from_slice(&payload).map_err(|error| error.to_string())?;
        }
        if displaced.iter().any(|id| !included.contains(id)) {
            return Err(
                "result continuation snapshot discarded newer explicitly bound context".into(),
            );
        }
    }
    if !snapshot_found {
        return Err(
            "result continuation removed future context without a covering snapshot".into(),
        );
    }
    Ok(())
}

fn validate_new_marks(
    reader: &mut impl CheckedRunEvidenceReader,
    before: &RunRecord,
    after: &RunRecord,
    config: &RunAuthoringConfig,
    next: &RunAuthoringConfig,
    before_seq: Option<u64>,
) -> Result<(), String> {
    let added = next
        .checkpoints
        .iter()
        .filter(|template| {
            template.role == RunAuthoringCheckpointRole::ResultInput
                && !config.checkpoints.contains(template)
        })
        .collect::<Vec<_>>();
    if added.is_empty() {
        return Ok(());
    }
    let pending = persisted_pending_handoff(after, &config.provider_interface)?
        .ok_or("result receiving has no saved source")?;
    let source = reader
        .admitted_movement(pending.source_movement_seq)?
        .ok_or("result receiving has no admitted source")?;
    input_artifacts::checked_source(
        reader,
        before,
        &config.provider_interface,
        &input_artifacts::InputArtifactBinding {
            artifact: source
                .artifact
                .clone()
                .ok_or("result receiving source has no artifact")?,
            source_movement_seq: pending.source_movement_seq,
        },
    )?;
    let metadata = source
        .invocation_metadata
        .as_ref()
        .and_then(|value| value.invocation())
        .ok_or("result receiving has no source grant")?;
    let runtime = metadata
        .runtime
        .as_ref()
        .ok_or("result receiving has no source coordinate")?;
    if source.seq.0 != pending.source_movement_seq
        || source.kind != MovementKind::InterfaceOutbound
        || before_seq.is_some_and(|sequence| source.seq.0 >= sequence)
        || source.to != Endpoint::Interface(config.provider_interface.clone())
        || runtime.run != before.id
        || runtime.worker != before.worker
        || runtime.interface != config.provider_interface
        || metadata.verification_failure.is_some()
        || !metadata
            .checkpoint_admission
            .as_ref()
            .is_some_and(|admission| admission.complete)
        || added.iter().any(|template| {
            !metadata
                .grant
                .mark_outcomes
                .iter()
                .any(|mark| mark == template.required_verified_artifacts[0].mark.as_str())
        })
    {
        return Err(
            "result receiving mark is outside its exact admitted provider source grant".into(),
        );
    }
    Ok(())
}

fn validate_retirement(
    reader: &mut impl CheckedRunEvidenceReader,
    before: &RunRecord,
    after: &RunRecord,
    config: &RunAuthoringConfig,
    next: &RunAuthoringConfig,
    before_seq: Option<u64>,
) -> Result<(), String> {
    let mut removed = config
        .checkpoints
        .iter()
        .filter(|template| {
            template.role == RunAuthoringCheckpointRole::ResultInput
                && (!next.checkpoints.contains(template)
                    || active_rows(before, template) != active_rows(after, template))
        })
        .collect::<Vec<_>>();
    if removed.is_empty() {
        return Ok(());
    }
    let pending = persisted_pending_handoff(after, &config.provider_interface)?
        .ok_or("result retirement has no handoff")?;
    if let Some(proof) = &pending.withdrawal {
        let cancelled = pending_inputs(config, &proof.predecessor)?;
        removed.retain(|template|!cancelled.contains(template));
        if removed.is_empty() { return Ok(()); }
    }
    let [template] = removed.as_slice() else {
        return Err("one source cannot consume several result inputs".into());
    };
    let source = reader
        .admitted_movement(pending.source_movement_seq)?
        .ok_or("result retirement has no admitted source")?;
    validate_result_source(
        reader,
        before,
        config,
        template,
        &input_artifacts::InputArtifactBinding {
            artifact: source
                .artifact
                .clone()
                .ok_or("result retirement has no artifact")?,
            source_movement_seq: pending.source_movement_seq,
        },
        before_seq,
    )?;
    Ok(())
}

fn validate_result_source(
    reader: &mut impl CheckedRunEvidenceReader,
    before: &RunRecord,
    config: &RunAuthoringConfig,
    template: &RunAuthoringCheckpoint,
    binding: &input_artifacts::InputArtifactBinding,
    before_seq: Option<u64>,
) -> Result<(Movement, RunRecord), String> {
    let (source, pinned) =
        input_artifacts::checked_source(reader, before, &config.provider_interface, binding)?;
    let metadata = source
        .invocation_metadata
        .as_ref()
        .and_then(|value| value.invocation())
        .ok_or("result retirement has no admitted metadata")?;
    let runtime = metadata
        .runtime
        .as_ref()
        .ok_or("result retirement has no coordinate")?;
    let checkpoint = pinned
        .checkpoint(&runtime.checkpoint)
        .ok_or("result retirement lost its checkpoint")?;
    if source.kind != MovementKind::InterfaceOutbound
        || source.seq.0 != binding.source_movement_seq
        || before_seq.is_some_and(|sequence| source.seq.0 >= sequence)
        || source.from != Endpoint::Interface(template.interface().clone())
        || source.to != Endpoint::Interface(config.provider_interface.clone())
        || runtime.run != before.id
        || runtime.worker != before.worker
        || runtime.ee != pinned.ee
        || runtime.interface != config.provider_interface
        || pinned.id != before.id
        || pinned.worker != before.worker
        || !pinned.active_checkpoints.contains(&checkpoint.id)
        || !before.active_checkpoints.contains(&checkpoint.id)
        || before.checkpoint(&checkpoint.id) != Some(checkpoint)
        || runtime.epoch != checkpoint.epoch.0
        || !checkpoint_record_matches_template(checkpoint, template, &before.id)
        || metadata.verification_failure.is_some()
        || !metadata_matches_checkpoint(metadata, template)
        || !artifact_refs_match_bindings(
            &metadata.artifact_binding_refs,
            &checkpoint.artifact_bindings,
        )
        || !metadata
            .checkpoint_admission
            .as_ref()
            .is_some_and(|admission| {
                admission.complete
                    && admission.artifact_label
                        == template.required_verified_artifacts[0].artifact_label
                    && admission.required_artifact_labels == vec![admission.artifact_label.clone()]
            })
    {
        return Err("result retirement did not consume its exact admitted source".into());
    }
    let call = correspondence(template)?;
    if call
        .invocation
        .as_ref()
        .is_some_and(|invocation| invocation.artifact != binding.artifact)
    {
        return Err("result source differs from its exact invocation bridge".into());
    }
    if checkpoint
        .artifact_bindings
        .iter()
        .filter(|binding| binding.id == call.call_artifact.id)
        .ne(std::iter::once(&ArtifactBinding {
            id: call.call_artifact.id.clone(),
            access: Default::default(),
        }))
        || !metadata
            .grant
            .artifact_access
            .get(call.call_artifact.id.as_str())
            .is_some_and(|access| access.read && !access.change && !access.create && !access.delete)
    {
        return Err("result source lost its exact read-only original call binding".into());
    }
    let (_, original) = reader.artifact(&call.call_artifact)?;
    let original = call_identity(&original)?;
    let (manifest, result) = reader.artifact(
        source
            .artifact
            .as_ref()
            .ok_or("result retirement has no artifact")?,
    )?;
    let result = result_identity(&result)?;
    if manifest.label != result.2
        || result.2 != template.required_verified_artifacts[0].artifact_label
        || original.0 != call.tool
        || original.1.as_deref() != Some(&call.correlation_id)
        || result.0 != call.tool
        || result.1.as_deref() != Some(&call.correlation_id)
    {
        return Err("result retirement differs from its original call correspondence".into());
    }
    Ok((source, pinned))
}

pub(super) fn checked_topology_advance(
    original: &RunRecord,
    current: &RunRecord,
    before: &BoundRunAuthoringTopology,
    after: &BoundRunAuthoringTopology,
    consumed: &RunAuthoringCheckpoint,
    checkpoint: &CheckpointId,
    source: u64,
    chain: &[CheckedRunTransition],
) -> Result<bool, String> {
    let consumed_by_own_write = || {
        chain.iter().any(|transition| {
            transition.writer.owner == before.provider_interface
                && RunRecord::from_bytes(&transition.before)
                    .ok()
                    .is_some_and(|record| {
                        record.checkpoint(checkpoint) == original.checkpoint(checkpoint)
                            && record.active_checkpoints.contains(checkpoint)
                    })
                && RunRecord::from_bytes(&transition.after)
                    .ok()
                    .is_some_and(|record| {
                        record.checkpoint(checkpoint).is_none()
                            && persisted_pending_handoff(&record, &before.provider_interface)
                                .ok()
                                .flatten()
                                .is_some_and(|pending| pending.source_movement_seq == source)
                    })
        })
    };
    if consumed.role == RunAuthoringCheckpointRole::ResultInput
        && after.checkpoints.contains(consumed)
        && (original.checkpoint(checkpoint) != current.checkpoint(checkpoint)
            || !current.active_checkpoints.contains(checkpoint))
        && !consumed_by_own_write()
    {
        return Err("result invocation requires its original admitted generation after a checked context revision".into());
    }
    let mut old = before.clone();
    let mut next = after.clone();
    old.checkpoints
        .retain(|template| template.role != RunAuthoringCheckpointRole::ResultInput);
    next.checkpoints
        .retain(|template| template.role != RunAuthoringCheckpointRole::ResultInput);
    if consumed.role == RunAuthoringCheckpointRole::ResultInput {
        old.checkpoints
            .retain(|template| template.role != RunAuthoringCheckpointRole::VerifiedInput);
        next.checkpoints
            .retain(|template| template.role != RunAuthoringCheckpointRole::VerifiedInput);
    }
    let routing_changed = output_connections::is_extension(&old, &next)
        || output_connections::is_withdrawal(&old, &next);
    if (old != next && !routing_changed) || before == after {
        return Ok(false);
    }
    if chain.is_empty() {
        return Err("result input topology changed without a checked authoring chain".into());
    }
    if consumed.role != RunAuthoringCheckpointRole::ResultInput {
        return Ok(true);
    }
    if !after.checkpoints.contains(consumed) {
        if source_was_bridged(chain, source)?.is_some() {
            return Ok(true);
        }
        let pending = persisted_pending_handoff(current, &before.provider_interface)?
            .ok_or("retired result invocation has no checked handoff")?;
        if pending.source_movement_seq != source || !consumed_by_own_write() {
            return Err(
                "result invocation was removed without consuming its own admitted source".into(),
            );
        }
    }
    Ok(true)
}

pub(super) fn validate_snapshot_context(
    config: &RunAuthoringConfig,
    future: &[ArtifactId],
    static_context: &[ArtifactId],
    captured: &ResolvedSessionContext,
) -> Result<(), String> {
    if enabled(config)
        && future.iter().any(|id| {
            !static_context.contains(id)
                && !captured
                    .artifacts
                    .iter()
                    .any(|entry| &entry.source_artifact.id == id)
        })
    {
        return Err("provider snapshot would discard newer explicitly bound context; revise the receiving checkpoint before a new admission".into());
    }
    Ok(())
}
