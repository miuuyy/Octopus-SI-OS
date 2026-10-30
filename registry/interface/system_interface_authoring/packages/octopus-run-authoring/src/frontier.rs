use super::*;

#[cfg(test)]
pub(crate) fn checkpoint_declares_tool_route(required: &RequiredVerifiedArtifact) -> bool {
    required.verifier.as_str() == TOOL_ROUTE_VERIFIER
}

pub(crate) fn checkpoint_tool_ids(checkpoint: &RunAuthoringCheckpoint) -> Result<BTreeSet<String>, String> {
    if checkpoint.required_verified_artifacts.len() != 1 {
        return Err(format!(
            "run_authoring checkpoint '{}' tool route requires exactly one verified-artifact requirement",
            checkpoint.name
        ));
    }
    let params = checkpoint.required_verified_artifacts[0]
        .params
        .as_object()
        .ok_or_else(|| {
            format!(
                "run_authoring checkpoint '{}' tool route params are not an object",
                checkpoint.name
            )
        })?;
    if params.len() != 1 || !params.contains_key("tools") {
        return Err(format!(
            "run_authoring checkpoint '{}' tool route params must contain only tools",
            checkpoint.name
        ));
    }
    let tools = params["tools"].as_array().ok_or_else(|| {
        format!(
            "run_authoring checkpoint '{}' tool route tools are not an array",
            checkpoint.name
        )
    })?;
    if tools.is_empty() {
        return Err(format!(
            "run_authoring checkpoint '{}' tool route tools are empty",
            checkpoint.name
        ));
    }
    let mut ids = BTreeSet::new();
    for value in tools {
        let tool = value.as_str().ok_or_else(|| {
            format!(
                "run_authoring checkpoint '{}' tool route contains a non-string tool id",
                checkpoint.name
            )
        })?;
        let tool = non_empty_string(tool, "checkpoint tool")?;
        if !ids.insert(tool.clone()) {
            return Err(format!(
                "run_authoring checkpoint '{}' repeats tool '{}'",
                checkpoint.name, tool
            ));
        }
    }
    Ok(ids)
}

pub(crate) fn pending_output_checkpoint(
    checkpoint: &RunAuthoringCheckpoint,
    id: &str,
    artifact_label: &str,
    context_artifacts: &[ArtifactId],
    mandatory_role_artifacts: &[ArtifactId],
) -> PendingHandoffCheckpoint {
    let mut artifact_bindings = if checkpoint.include_context {
        context_artifacts.to_vec()
    } else {
        mandatory_role_artifacts.to_vec()
    };
    for artifact in &checkpoint.static_artifact_bindings { append_artifact(&mut artifact_bindings, artifact.clone()); }
    let mut interface_binding = checkpoint.interface_binding.clone();
    interface_binding.config.artifact_policy = BindingArtifactPolicy::Allow {
        names: vec![artifact_label.to_string()],
        namespaces: Vec::new(),
    };
    PendingHandoffCheckpoint {
        id: CheckpointId::new(id),
        epoch: 0,
        interface_binding,
        outbound_interface_bindings: checkpoint.outbound_interface_bindings.clone(),
        artifact_bindings,
        required_verified_artifacts: checkpoint.required_verified_artifacts.clone(),
    }
}

pub(crate) fn retains_future_output(template: &RunAuthoringCheckpoint) -> bool {
    template.role == RunAuthoringCheckpointRole::ProviderInterfaceOutput
        || (template.role == RunAuthoringCheckpointRole::ProviderToolOutput && !template.result_receiving.is_empty())
}

pub(crate) fn pending_output_matches_template(item: &PendingHandoffItem, template: &RunAuthoringCheckpoint) -> bool {
    let mut binding = template.interface_binding.clone();
    binding.config.artifact_policy = BindingArtifactPolicy::Allow {
        names: vec![item.artifact_label.clone()], namespaces: Vec::new(),
    };
    template.interface_binding.admits(&item.artifact_label)
        && item.checkpoint.interface_binding == binding
        && item.checkpoint.outbound_interface_bindings == template.outbound_interface_bindings
        && item.checkpoint.required_verified_artifacts == template.required_verified_artifacts
}

pub(crate) fn materialize_pending_checkpoint(
    checkpoint: &PendingHandoffCheckpoint,
    run: &RunId,
) -> serde_json::Value {
    serde_json::json!({
        "id": checkpoint.id,
        "run": run,
        "trace": format!("trace:{}:{}", run.as_str(), checkpoint.id.as_str()),
        "epoch": checkpoint.epoch,
        "interface_binding": checkpoint.interface_binding,
        "outbound_interface_bindings": checkpoint.outbound_interface_bindings,
        "artifact_bindings": checkpoint
            .artifact_bindings
            .iter()
            .map(|artifact| serde_json::json!({ "id": artifact }))
            .collect::<Vec<_>>(),
        "required_verified_artifacts": checkpoint.required_verified_artifacts
    })
}

pub(crate) fn pending_required_evidence(item: &PendingHandoffItem) -> Result<Option<(&str, &str)>, String> {
    match item.checkpoint.required_verified_artifacts.as_slice() {
        [] => Ok(None),
        [required] if required.artifact_label == item.artifact_label => {
            Ok(Some((required.mark.as_str(), required.verifier.as_str())))
        }
        [required] => Err(format!(
            "provider pending handoff artifact '{}' has label '{}' but its checkpoint requires '{}'",
            item.artifact_id.as_str(),
            item.artifact_label,
            required.artifact_label
        )),
        requirements => Err(format!(
            "provider pending handoff artifact '{}' checkpoint has {} verified-artifact requirements",
            item.artifact_id.as_str(),
            requirements.len()
        )),
    }
}

pub(crate) fn checkpoint_matches_pending(
    checkpoint: &octopus_run_record::CheckpointRecord,
    pending: &PendingHandoffCheckpoint,
) -> bool {
    checkpoint.id == pending.id
        && checkpoint.epoch.0 == pending.epoch
        && checkpoint.interface_binding.as_ref() == Some(&pending.interface_binding)
        && checkpoint.outbound_interface_bindings == pending.outbound_interface_bindings
        && checkpoint
            .artifact_bindings
            .iter()
            .map(|binding| &binding.id)
            .eq(pending.artifact_bindings.iter())
        && checkpoint
            .artifact_bindings
            .iter()
            .all(|binding| binding.access == Default::default())
        && checkpoint.required_verified_artifacts == pending.required_verified_artifacts
}

pub(crate) fn validate_pending_items_are_active(
    record: &RunRecord,
    provider: &InterfaceId,
    pending: &PendingHandoff,
) -> Result<usize, String> {
    Ok(validate_pending_output_frontier(record, provider, pending)?.len())
}

pub(crate) fn validate_pending_output_frontier(
    record: &RunRecord,
    provider: &InterfaceId,
    pending: &PendingHandoff,
) -> Result<BTreeSet<CheckpointId>, String> {
    let provider_state = record.interface_state.get(provider).ok_or_else(|| {
        format!(
            "provider run '{}' has no interface_state for '{}'",
            record.id.as_str(),
            provider.as_str()
        )
    })?;
    let frontier = frontier_checkpoint_ids(provider_state)?;
    if frontier.len() < 3 && provider_state.get(RUN_AUTHORING_BINDING).and_then(|value| value.get("writer_run")).is_none() {
        return Err(format!(
            "provider run '{}' pending handoff frontier must contain its outputs and two authoring checkpoints",
            record.id.as_str()
        ));
    }
    let active = record.active_checkpoints.iter().collect::<BTreeSet<_>>();
    for id in &frontier {
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
        })?;
    }
    let provider_bound = record
        .active_checkpoints
        .iter()
        .filter_map(|id| record.checkpoint(id))
        .filter(|checkpoint| checkpoint.inbound_interface() == Some(provider))
        .collect::<Vec<_>>();
    for item in &pending.items {
        let matches = provider_bound
            .iter()
            .filter(|checkpoint| checkpoint_matches_pending(checkpoint, &item.checkpoint))
            .count();
        if matches != 1 {
            return Err(format!(
                "provider run '{}' pending item '{}' matches {} active provider checkpoints",
                record.id.as_str(),
                item.artifact_id.as_str(),
                matches
            ));
        }
    }
    let topology: Option<BoundRunAuthoringTopology> = provider_state.get(RUN_AUTHORING_BINDING).cloned()
        .map(serde_json::from_value).transpose().map_err(|e|e.to_string())?;
    if let Some(topology) = &topology { topology.validate_identity(record,provider)?; }
    let retained = topology.iter().flat_map(|topology| &topology.checkpoints).filter(|template| retains_future_output(template)
        && !pending.items.iter().any(|item| pending_output_matches_template(item, template))).collect::<Vec<_>>();
    let mut retained_ids = BTreeSet::new();
    let item_ids = pending.items.iter().map(|item| item.checkpoint.id.clone()).collect::<BTreeSet<_>>();
    let mut standing_ids = BTreeSet::new();
    if let Some(topology) = &topology {
        let authoring = |role| -> Result<&RunAuthoringCheckpoint,String> {
            let matches = topology.checkpoints.iter().filter(|template| template.role == role).collect::<Vec<_>>();
            match matches.as_slice() { [one] => Ok(*one), _ => Err("retained output has no unique authoring boundary".into()) }
        };
        let frontier_records = frontier.iter().map(|id| record.checkpoint(&CheckpointId::new(id)).ok_or("retained frontier checkpoint is absent".to_string())).collect::<Result<Vec<_>,_>>()?;
        let roles = match &topology.writer_run {
            Some(binding) => binding.roles(record, provider, &topology.checkpoints)?,
            None => canonical_role_bindings(&frontier_records,authoring(RunAuthoringCheckpointRole::AuthoringCall)?,authoring(RunAuthoringCheckpointRole::AuthoringResult)?,&record.id)?,
        };
        for template in &retained {
            let matches = provider_bound.iter().filter(|checkpoint| checkpoint_record_matches_template(checkpoint,template,&record.id)
                && checkpoint_role_bindings_match(checkpoint,&roles,template)).collect::<Vec<_>>();
            let [checkpoint] = matches.as_slice() else { return Err("pending provider lost or duplicated its exact retained output/read bindings".into()); };
            if !retained_ids.insert(checkpoint.id.clone()) { return Err("retained outputs overlap one checkpoint".into()); }
        }
        let claimed = item_ids.union(&retained_ids).cloned().collect::<BTreeSet<_>>();
        standing_ids = standing_routes::classify_standing_frontier(&provider_bound, &claimed, &topology.checkpoints, provider, &roles)?;
    }
    if provider_bound.len() != pending.items.len() + retained.len() + standing_ids.len() {
        return Err(format!(
            "provider run '{}' pending handoff has {} active provider checkpoints for {} items",
            record.id.as_str(),
            provider_bound.len(),
            pending.items.len()
        ));
    }
    for checkpoint in &provider_bound {
        if !frontier.contains(checkpoint.id.as_str()) {
            return Err(format!(
                "provider run '{}' active provider checkpoint '{}' is outside the provider-owned frontier",
                record.id.as_str(),
                checkpoint.id.as_str()
            ));
        }
    }
    retained_ids.extend(standing_ids);
    Ok(retained_ids)
}
pub(crate) fn frontier_checkpoint_ids(
    values: &std::collections::BTreeMap<String, serde_json::Value>,
) -> Result<BTreeSet<String>, String> {
    let ids = raw_frontier_checkpoint_ids(values)?;
    if ids.len() < 2 && values.get(RUN_AUTHORING_BINDING).and_then(|value| value.get("writer_run")).is_none() {
        return Err(format!(
            "provider interface_state.{FRONTIER_CHECKPOINT_IDS} must contain the two authoring checkpoints"
        ));
    }
    Ok(ids)
}

pub(crate) fn raw_frontier_checkpoint_ids(
    values: &std::collections::BTreeMap<String, serde_json::Value>,
) -> Result<BTreeSet<String>, String> {
    let raw = values
        .get(FRONTIER_CHECKPOINT_IDS)
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            format!("provider interface_state.{FRONTIER_CHECKPOINT_IDS} must be an array")
        })?;
    if raw.is_empty() {
        return Err(format!(
            "provider interface_state.{FRONTIER_CHECKPOINT_IDS} must not be empty"
        ));
    }
    let mut ids = BTreeSet::new();
    for value in raw {
        let id = value.as_str().ok_or_else(|| {
            format!("provider interface_state.{FRONTIER_CHECKPOINT_IDS} contains a non-string id")
        })?;
        let id = non_empty_string(id, FRONTIER_CHECKPOINT_IDS)?;
        if !ids.insert(id.clone()) {
            return Err(format!(
                "provider interface_state.{FRONTIER_CHECKPOINT_IDS} repeats checkpoint '{id}'"
            ));
        }
    }
    Ok(ids)
}

pub(crate) fn materialize_checkpoint(
    checkpoint: &RunAuthoringCheckpoint,
    id: &str,
    run: &RunId,
    artifact_bindings: &[serde_json::Value],
) -> serde_json::Value {
    let mut artifact_bindings = artifact_bindings.to_vec();
    for artifact in &checkpoint.static_artifact_bindings {
        let binding = serde_json::json!({"id":artifact});
        if !artifact_bindings.iter().any(|existing| existing.get("id") == binding.get("id")) {
            artifact_bindings.push(binding);
        }
    }
    serde_json::json!({
        "id": id,
        "run": run,
        "trace": format!("trace:{}:{id}", run.as_str()),
        "epoch": 0,
        "interface_binding": checkpoint.interface_binding,
        "outbound_interface_bindings": checkpoint.outbound_interface_bindings,
        "artifact_bindings": artifact_bindings,
        "required_verified_artifacts": checkpoint.required_verified_artifacts
    })
}

pub(crate) fn replace_owned_frontier(
    root: &mut serde_json::Map<String, serde_json::Value>,
    previous_frontier: &BTreeSet<String>,
    next_frontier: Vec<serde_json::Value>,
    next_frontier_ids: &[String],
) -> Result<(), String> {
    let checkpoints = root
        .get_mut("checkpoints")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or_else(|| "provider run checkpoints are not an array".to_string())?;
    let mut retained = Vec::with_capacity(checkpoints.len() + next_frontier.len());
    let mut removed_checkpoints = BTreeSet::new();
    for checkpoint in std::mem::take(checkpoints) {
        let id = checkpoint_value_id(&checkpoint)?;
        if previous_frontier.contains(id) {
            removed_checkpoints.insert(id.to_string());
        } else {
            retained.push(checkpoint);
        }
    }
    if &removed_checkpoints != previous_frontier {
        return Err(format!(
            "provider-owned frontier checkpoint set drifted: state={:?}, run={:?}",
            previous_frontier, removed_checkpoints
        ));
    }
    retained.extend(next_frontier);
    *checkpoints = retained;

    let active = root
        .get_mut("active_checkpoints")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or_else(|| "provider run active_checkpoints are not an array".to_string())?;
    let mut retained_active = Vec::with_capacity(active.len() + next_frontier_ids.len());
    let mut removed_active = BTreeSet::new();
    for checkpoint in std::mem::take(active) {
        let id = checkpoint.as_str().ok_or_else(|| {
            "provider run active_checkpoints contains a non-string id".to_string()
        })?;
        if previous_frontier.contains(id) {
            removed_active.insert(id.to_string());
        } else {
            retained_active.push(checkpoint);
        }
    }
    if &removed_active != previous_frontier {
        return Err(format!(
            "provider-owned active frontier set drifted: state={:?}, run={:?}",
            previous_frontier, removed_active
        ));
    }
    retained_active.extend(
        next_frontier_ids
            .iter()
            .map(|id| serde_json::Value::String(id.clone())),
    );
    *active = retained_active;
    Ok(())
}

pub(crate) fn replace_context_checkpoints(
    root: &mut serde_json::Map<String, serde_json::Value>,
    config: &RunAuthoringConfig,
    run: &RunId,
    checkpoint_prefix: &str,
    artifact_bindings: &[serde_json::Value],
) -> Result<(), String> {
    let active_values = root
        .get("active_checkpoints")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "provider run active_checkpoints are not an array".to_string())?;
    let mut active_ids = BTreeSet::new();
    for value in active_values {
        let id = value.as_str().ok_or_else(|| {
            "provider run active_checkpoints contains a non-string id".to_string()
        })?;
        if !active_ids.insert(id.to_string()) {
            return Err(format!(
                "provider run active_checkpoints repeats checkpoint '{}'",
                id
            ));
        }
    }
    let checkpoint_values = root
        .get("checkpoints")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "provider run checkpoints are not an array".to_string())?;
    let mut existing_ids = BTreeSet::new();
    for checkpoint in checkpoint_values {
        let id = checkpoint_value_id(checkpoint)?;
        if !existing_ids.insert(id.to_string()) {
            return Err(format!(
                "provider run checkpoints repeats checkpoint '{}'",
                id
            ));
        }
    }

    let mut replaced_ids = BTreeSet::new();
    let mut replacements = Vec::new();
    for template in config.checkpoints.iter().filter(|template| {
        template.role == RunAuthoringCheckpointRole::Stable && template.include_context
    }) {
        let matches = checkpoint_values
            .iter()
            .filter(|checkpoint| {
                checkpoint_value_id(checkpoint)
                    .ok()
                    .is_some_and(|id| active_ids.contains(id))
            })
            .filter_map(
                |checkpoint| match checkpoint_matches_template(checkpoint, template, run) {
                    Ok(true) => Some(Ok(checkpoint)),
                    Ok(false) => None,
                    Err(error) => Some(Err(error)),
                },
            )
            .collect::<Result<Vec<_>, _>>()?;
        if matches.len() != 1 {
            return Err(format!(
                "provider context template '{}' matches {} active checkpoints",
                template.name,
                matches.len()
            ));
        }
        let previous_id = checkpoint_value_id(matches[0])?.to_string();
        if !replaced_ids.insert(previous_id.clone()) {
            return Err(format!(
                "provider context templates resolve the same checkpoint '{}'",
                previous_id
            ));
        }
        let replacement_id = format!("{checkpoint_prefix}_context_{}", template.name);
        if existing_ids.contains(&replacement_id) && !replaced_ids.contains(&replacement_id) {
            return Err(format!(
                "provider context replacement checkpoint '{}' already exists",
                replacement_id
            ));
        }
        replacements.push((
            replacement_id.clone(),
            materialize_checkpoint(template, &replacement_id, run, artifact_bindings),
        ));
    }
    if replacements.is_empty() {
        return Err("run_authoring has no context-bearing stable checkpoints".to_string());
    }

    let checkpoints = root
        .get_mut("checkpoints")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or_else(|| "provider run checkpoints are not an array".to_string())?;
    let mut retained = Vec::with_capacity(checkpoints.len() + replacements.len());
    let mut removed = BTreeSet::new();
    for checkpoint in std::mem::take(checkpoints) {
        let id = checkpoint_value_id(&checkpoint)?;
        if replaced_ids.contains(id) {
            removed.insert(id.to_string());
        } else {
            retained.push(checkpoint);
        }
    }
    if removed != replaced_ids {
        return Err(format!(
            "provider context checkpoint set drifted: expected={:?}, removed={:?}",
            replaced_ids, removed
        ));
    }
    retained.extend(
        replacements
            .iter()
            .map(|(_, checkpoint)| checkpoint.clone()),
    );
    *checkpoints = retained;

    let active = root
        .get_mut("active_checkpoints")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or_else(|| "provider run active_checkpoints are not an array".to_string())?;
    let mut retained_active = Vec::with_capacity(active.len() + replacements.len());
    let mut removed_active = BTreeSet::new();
    for checkpoint in std::mem::take(active) {
        let id = checkpoint.as_str().ok_or_else(|| {
            "provider run active_checkpoints contains a non-string id".to_string()
        })?;
        if replaced_ids.contains(id) {
            removed_active.insert(id.to_string());
        } else {
            retained_active.push(checkpoint);
        }
    }
    if removed_active != replaced_ids {
        return Err(format!(
            "provider active context checkpoint set drifted: expected={:?}, removed={:?}",
            replaced_ids, removed_active
        ));
    }
    retained_active.extend(
        replacements
            .iter()
            .map(|(id, _)| serde_json::Value::String(id.clone())),
    );
    *active = retained_active;
    Ok(())
}

pub(crate) fn checkpoint_artifact_binding_set(
    checkpoint: &serde_json::Value,
) -> Result<Vec<ArtifactBinding>, String> {
    let mut bindings = checkpoint
        .get("artifact_bindings")
        .cloned()
        .map(serde_json::from_value::<Vec<ArtifactBinding>>)
        .transpose()
        .map_err(|error| format!("provider checkpoint artifact bindings are malformed: {error}"))?
        .unwrap_or_default();
    bindings.sort_by(|left, right| left.id.as_str().cmp(right.id.as_str()));
    Ok(bindings)
}

pub(crate) fn replace_consumed_independent_input_checkpoint(
    root: &mut serde_json::Map<String, serde_json::Value>,
    template: &RunAuthoringCheckpoint,
    run: &RunId,
    checkpoint_prefix: &str,
    artifact_bindings: &[serde_json::Value],
) -> Result<(), String> {
    if template.role != RunAuthoringCheckpointRole::IndependentInput || template.include_context {
        return Err(
            "provider can re-arm only a consumed context-free independent input".to_string(),
        );
    }
    let active_values = root
        .get("active_checkpoints")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "provider run active_checkpoints are not an array".to_string())?;
    let active_ids = active_values
        .iter()
        .map(|value| {
            value.as_str().map(str::to_string).ok_or_else(|| {
                "provider run active_checkpoints contains a non-string id".to_string()
            })
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    if active_ids.len() != active_values.len() {
        return Err("provider run active_checkpoints repeats an id".to_string());
    }
    let checkpoint_values = root
        .get("checkpoints")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "provider run checkpoints are not an array".to_string())?;
    let matches = checkpoint_values
        .iter()
        .filter(|checkpoint| {
            checkpoint_value_id(checkpoint)
                .ok()
                .is_some_and(|id| active_ids.contains(id))
        })
        .filter_map(
            |checkpoint| match checkpoint_matches_template(checkpoint, template, run) {
                Ok(true) => Some(Ok(checkpoint)),
                Ok(false) => None,
                Err(error) => Some(Err(error)),
            },
        )
        .collect::<Result<Vec<_>, _>>()?;
    if matches.len() != 1 {
        return Err(format!(
            "provider consumed independent-input template '{}' matches {} active checkpoints",
            template.name,
            matches.len()
        ));
    }
    let replacement_id = format!("{checkpoint_prefix}_independent_input_{}", template.name);
    let replacement = materialize_checkpoint(template, &replacement_id, run, artifact_bindings);
    if checkpoint_artifact_binding_set(matches[0])? != checkpoint_artifact_binding_set(&replacement)? {
        return Err(format!(
            "provider consumed context-free independent input '{}' differs from its mandatory role bindings",
            template.name
        ));
    }
    let previous_id = checkpoint_value_id(matches[0])?.to_string();
    if checkpoint_values.iter().any(|checkpoint| {
        checkpoint_value_id(checkpoint).ok() == Some(replacement_id.as_str())
            && checkpoint_value_id(checkpoint).ok() != Some(previous_id.as_str())
    }) {
        return Err(format!(
            "provider independent-input replacement checkpoint '{}' already exists",
            replacement_id
        ));
    }
    let checkpoints = root
        .get_mut("checkpoints")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or_else(|| "provider run checkpoints are not an array".to_string())?;
    let mut retained = Vec::with_capacity(checkpoints.len());
    let mut removed = 0usize;
    for checkpoint in std::mem::take(checkpoints) {
        if checkpoint_value_id(&checkpoint)? == previous_id {
            removed += 1;
        } else {
            retained.push(checkpoint);
        }
    }
    if removed != 1 {
        return Err(format!(
            "provider independent-input checkpoint '{}' was removed {} times",
            previous_id, removed
        ));
    }
    retained.push(replacement);
    *checkpoints = retained;

    let active = root
        .get_mut("active_checkpoints")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or_else(|| "provider run active_checkpoints are not an array".to_string())?;
    let mut retained_active = Vec::with_capacity(active.len());
    let mut removed_active = 0usize;
    for checkpoint in std::mem::take(active) {
        if checkpoint.as_str() == Some(previous_id.as_str()) {
            removed_active += 1;
        } else {
            retained_active.push(checkpoint);
        }
    }
    if removed_active != 1 {
        return Err(format!(
            "provider active independent-input checkpoint '{}' was removed {} times",
            previous_id, removed_active
        ));
    }
    retained_active.push(serde_json::Value::String(replacement_id));
    *active = retained_active;
    Ok(())
}

pub(crate) fn replace_verified_input_checkpoints(
    root: &mut serde_json::Map<String, serde_json::Value>,
    current_config: &RunAuthoringConfig,
    next_config: &RunAuthoringConfig,
    run: &RunId,
    checkpoint_prefix: &str,
    artifact_bindings: &[serde_json::Value],
) -> Result<(), String> {
    let active_values = root
        .get("active_checkpoints")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "provider run active_checkpoints are not an array".to_string())?;
    let active_ids = active_values
        .iter()
        .map(|value| {
            value.as_str().map(str::to_string).ok_or_else(|| {
                "provider run active_checkpoints contains a non-string id".to_string()
            })
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    if active_ids.len() != active_values.len() {
        return Err("provider run active_checkpoints repeats an id".to_string());
    }
    let checkpoint_values = root
        .get("checkpoints")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "provider run checkpoints are not an array".to_string())?;
    let mut existing_ids = BTreeSet::new();
    for checkpoint in checkpoint_values {
        let id = checkpoint_value_id(checkpoint)?;
        if !existing_ids.insert(id.to_string()) {
            return Err(format!(
                "provider run checkpoints repeats checkpoint '{}'",
                id
            ));
        }
    }

    let mut removed_ids = BTreeSet::new();
    for template in current_config
        .checkpoints
        .iter()
        .filter(|template| template.role == RunAuthoringCheckpointRole::VerifiedInput)
    {
        let matches = checkpoint_values
            .iter()
            .filter(|checkpoint| {
                checkpoint_value_id(checkpoint)
                    .ok()
                    .is_some_and(|id| active_ids.contains(id))
            })
            .filter_map(
                |checkpoint| match checkpoint_matches_template(checkpoint, template, run) {
                    Ok(true) => Some(Ok(checkpoint)),
                    Ok(false) => None,
                    Err(error) => Some(Err(error)),
                },
            )
            .collect::<Result<Vec<_>, _>>()?;
        if matches.len() != 1 {
            return Err(format!(
                "provider verified-input template '{}' matches {} active checkpoints",
                template.name,
                matches.len()
            ));
        }
        let id = checkpoint_value_id(matches[0])?.to_string();
        if !removed_ids.insert(id.clone()) {
            return Err(format!(
                "provider verified-input templates resolve the same checkpoint '{}'",
                id
            ));
        }
    }

    let mut replacements = Vec::new();
    for template in next_config
        .checkpoints
        .iter()
        .filter(|template| template.role == RunAuthoringCheckpointRole::VerifiedInput)
    {
        let id = format!("{checkpoint_prefix}_verified_input_{}", template.name);
        if existing_ids.contains(&id) && !removed_ids.contains(&id) {
            return Err(format!(
                "provider verified-input checkpoint '{}' already exists",
                id
            ));
        }
        replacements.push((
            id.clone(),
            materialize_checkpoint(template, &id, run, artifact_bindings),
        ));
    }

    let checkpoints = root
        .get_mut("checkpoints")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or_else(|| "provider run checkpoints are not an array".to_string())?;
    let mut retained = Vec::with_capacity(checkpoints.len() + replacements.len());
    let mut removed = BTreeSet::new();
    for checkpoint in std::mem::take(checkpoints) {
        let id = checkpoint_value_id(&checkpoint)?;
        if removed_ids.contains(id) {
            removed.insert(id.to_string());
        } else {
            retained.push(checkpoint);
        }
    }
    if removed != removed_ids {
        return Err(format!(
            "provider verified-input checkpoint set drifted: expected={:?}, removed={:?}",
            removed_ids, removed
        ));
    }
    retained.extend(
        replacements
            .iter()
            .map(|(_, checkpoint)| checkpoint.clone()),
    );
    *checkpoints = retained;

    let active = root
        .get_mut("active_checkpoints")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or_else(|| "provider run active_checkpoints are not an array".to_string())?;
    let mut retained_active = Vec::with_capacity(active.len() + replacements.len());
    let mut removed_active = BTreeSet::new();
    for checkpoint in std::mem::take(active) {
        let id = checkpoint.as_str().ok_or_else(|| {
            "provider run active_checkpoints contains a non-string id".to_string()
        })?;
        if removed_ids.contains(id) {
            removed_active.insert(id.to_string());
        } else {
            retained_active.push(checkpoint);
        }
    }
    if removed_active != removed_ids {
        return Err(format!(
            "provider active verified-input set drifted: expected={:?}, removed={:?}",
            removed_ids, removed_active
        ));
    }
    retained_active.extend(
        replacements
            .iter()
            .map(|(id, _)| serde_json::Value::String(id.clone())),
    );
    *active = retained_active;
    Ok(())
}

pub(crate) fn checkpoint_matches_template(
    checkpoint: &serde_json::Value,
    template: &RunAuthoringCheckpoint,
    run: &RunId,
) -> Result<bool, String> {
    let object = checkpoint
        .as_object()
        .ok_or_else(|| "provider run checkpoint is not an object".to_string())?;
    let checkpoint_run = object
        .get("run")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "provider run checkpoint has no string run".to_string())?;
    let interface = serde_json::from_value::<AuthoredInterfaceBinding>(
        object
            .get("interface_binding")
            .cloned()
            .ok_or_else(|| "provider run checkpoint has no interface_binding".to_string())?,
    )
    .map_err(|error| format!("provider checkpoint inbound binding is malformed: {error}"))?;
    let outbound = serde_json::from_value::<Vec<AuthoredInterfaceBinding>>(
        object
            .get("outbound_interface_bindings")
            .cloned()
            .ok_or_else(|| {
                "provider run checkpoint has no outbound_interface_bindings".to_string()
            })?,
    )
    .map_err(|error| format!("provider checkpoint outbound bindings are malformed: {error}"))?;
    let required = serde_json::from_value::<Vec<RequiredVerifiedArtifact>>(
        object
            .get("required_verified_artifacts")
            .cloned()
            .ok_or_else(|| {
                "provider run checkpoint has no required_verified_artifacts".to_string()
            })?,
    )
    .map_err(|error| format!("provider checkpoint requirements are malformed: {error}"))?;
    Ok(checkpoint_run == run.as_str()
        && interface == template.interface_binding
        && outbound == template.outbound_interface_bindings
        && required == template.required_verified_artifacts)
}

pub(crate) fn current_context_artifacts(
    record: &RunRecord,
    config: &RunAuthoringConfig,
) -> Result<Vec<ArtifactId>, String> {
    Ok(current_context_bindings(record, &config.checkpoints)?
        .into_iter().map(|binding| binding.id).collect())
}

pub(crate) fn effective_context_bindings(
    record: &RunRecord,
    templates: &[RunAuthoringCheckpoint],
    provider: &InterfaceId,
) -> Result<Vec<ArtifactBinding>, String> {
    let current = current_context_bindings(record, templates)?;
    let Some(target) = persisted_pending_handoff(record, provider)?
        .filter(|pending| pending.deferred_publication)
        .and_then(|pending| pending.context_publication)
    else {
        return Ok(current);
    };
    Ok(target.into_iter().map(|id| current.iter().find(|binding| binding.id == id).cloned()
        .unwrap_or(ArtifactBinding { id, access: Default::default() })).collect())
}

pub(crate) fn effective_context_artifacts(
    record: &RunRecord,
    config: &RunAuthoringConfig,
) -> Result<Vec<ArtifactId>, String> {
    Ok(effective_context_bindings(record, &config.checkpoints, &config.provider_interface)?
        .into_iter().map(|binding| binding.id).collect())
}

pub(crate) fn current_mandatory_role_artifacts(
    record: &RunRecord,
    config: &RunAuthoringConfig,
) -> Result<Vec<ArtifactId>, String> {
    Ok(current_mandatory_role_bindings(record, config)?
        .into_iter()
        .map(|binding| binding.id)
        .collect())
}

pub(crate) fn current_mandatory_role_bindings(
    record: &RunRecord,
    config: &RunAuthoringConfig,
) -> Result<Vec<ArtifactBinding>, String> {
    if let Some(binding) = &config.writer_run {
        return binding.roles(record, &config.provider_interface, &config.checkpoints);
    }
    let call =
        active_checkpoint_for_role(record, config, RunAuthoringCheckpointRole::AuthoringCall)?;
    let result =
        active_checkpoint_for_role(record, config, RunAuthoringCheckpointRole::AuthoringResult)?;
    if call.artifact_bindings != result.artifact_bindings {
        return Err(format!(
            "provider run '{}' authoring checkpoints carry different mandatory role bindings",
            record.id.as_str()
        ));
    }
    if call
        .artifact_bindings
        .iter()
        .any(|binding| !binding.access.read)
    {
        return Err(format!(
            "provider run '{}' authoring checkpoints contain a non-readable mandatory role binding",
            record.id.as_str()
        ));
    }
    Ok(call.artifact_bindings.clone())
}
