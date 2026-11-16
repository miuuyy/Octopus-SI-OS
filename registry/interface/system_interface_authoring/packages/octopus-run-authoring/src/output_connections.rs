use super::*;

pub(super) fn is_extension(before: &BoundRunAuthoringTopology, after: &BoundRunAuthoringTopology) -> bool {
    after.checkpoints.len() > before.checkpoints.len()
        && after.checkpoints.starts_with(&before.checkpoints)
        && preserves_provider_topology(before, after)
}

pub(super) fn is_withdrawal(before: &BoundRunAuthoringTopology, after: &BoundRunAuthoringTopology) -> bool {
    if after.checkpoints.len() >= before.checkpoints.len() { return false; }
    let mut prior = before.clone(); prior.checkpoints.clear();
    let mut next = after.clone(); next.checkpoints.clear();
    let retained = after.checkpoints.iter().map(|cp| &cp.name).collect::<BTreeSet<_>>();
    prior == next
        && before.checkpoints.iter().filter(|cp| retained.contains(&cp.name)).eq(after.checkpoints.iter())
        && before.checkpoints.iter().filter(|cp| !retained.contains(&cp.name)).all(|cp| {
            cp.role == RunAuthoringCheckpointRole::ProviderInterfaceOutput && !cp.include_context
                && cp.owner_authoring.is_none() && cp.static_artifact_bindings.is_empty()
                && cp.required_verified_artifacts.is_empty() && cp.result_receiving.is_empty()
        })
}

pub(super) struct Extension {
    pub protected: Vec<u8>,
    checkpoints: Vec<serde_json::Value>,
    templates: Vec<serde_json::Value>,
    ids: Vec<serde_json::Value>,
}

impl Extension {
    // Restore only the validated routing delta after the ordinary three-way
    // merge. Its original checkpoint ids and contracts remain unchanged.
    pub fn restore(&self, value: &mut serde_json::Value, provider: &InterfaceId) -> Result<(), String> {
        fn append(target: &mut serde_json::Value, additions: &[serde_json::Value]) -> Result<(), String> {
            let rows = target.as_array_mut().ok_or("rebased connection array is absent")?;
            if additions.iter().any(|row| rows.contains(row)) {
                return Err("rebased connection overlaps the planned record".into());
            }
            rows.extend_from_slice(additions);
            Ok(())
        }
        append(&mut value["checkpoints"], &self.checkpoints)?;
        append(&mut value["active_checkpoints"], &self.ids)?;
        let state = &mut value["interface_state"][provider.as_str()];
        append(&mut state[FRONTIER_CHECKPOINT_IDS], &self.ids)?;
        append(&mut state[RUN_AUTHORING_BINDING]["checkpoints"], &self.templates)
    }
}

pub(super) fn extension(
    before: &[u8],
    current: &[u8],
    config: &RunAuthoringConfig,
) -> Result<Option<Extension>, String> {
    let read = |record: &RunRecord| -> Result<BoundRunAuthoringTopology, String> {
        serde_json::from_value(record.interface_state.get(&config.provider_interface)
            .and_then(|state| state.get(RUN_AUTHORING_BINDING)).cloned()
            .ok_or("outgoing connection has no provider topology")?).map_err(|error| error.to_string())
    };
    let before_value: serde_json::Value = serde_json::from_slice(before).map_err(|error| error.to_string())?;
    let before_bytes = before;
    let before = &RunRecord::from_bytes(before).map_err(|error| error.to_string())?;
    let prior = read(before)?;
    let record = RunRecord::from_bytes(current).map_err(|error| error.to_string())?;
    let next = read(&record)?;
    if !is_extension(&prior, &next) {
        return Ok(None);
    }
    let additions = &next.checkpoints[prior.checkpoints.len()..];
    if additions.iter().any(|template| !template.static_artifact_bindings.is_empty()
        || template.include_context || template.owner_authoring.is_some()
        || !template.required_verified_artifacts.is_empty()) {
        return Err("an observed outgoing connection cannot change context, ownership or requirements".into());
    }
    prior.validate_identity(before, &config.provider_interface)?;
    next.validate_identity(&record, &config.provider_interface)?;
    let mut next_config = config.clone();
    next_config.checkpoints = next.checkpoints.clone();
    next_config.validate(&config.provider_interface, &record.id)?;
    validate_bound_run_authoring_topology(&record, &next_config, None)?;
    let pending = persisted_pending_handoff(&record, &config.provider_interface)?
        .ok_or("an observed outgoing connection has no provider handoff")?;
    validate_pending_items_are_active(&record, &config.provider_interface, &pending)?;
    let mut ids = BTreeSet::new();
    for template in additions {
        let matches = record.checkpoints.iter().filter(|checkpoint| record.active_checkpoints.contains(&checkpoint.id)
            && checkpoint_record_matches_template(checkpoint, template, &record.id)).collect::<Vec<_>>();
        let [checkpoint] = matches.as_slice() else {
            return Err("an observed outgoing connection has no unique active checkpoint".into());
        };
        if before.checkpoint(&checkpoint.id).is_some()
            || pending.items.iter().any(|item| item.checkpoint.id == checkpoint.id)
            || !ids.insert(checkpoint.id.as_str().to_string()) {
            return Err("an observed outgoing connection claims an existing or pending checkpoint".into());
        }
    }
    let mut value: serde_json::Value = serde_json::from_slice(current).map_err(|error| error.to_string())?;
    let checkpoints = value["checkpoints"].as_array().ok_or("connection checkpoints are absent")?
        .iter().filter(|checkpoint| checkpoint["id"].as_str().is_some_and(|id| ids.contains(id)))
        .cloned().collect();
    let templates = additions.iter().map(serde_json::to_value).collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    let added_ids = value["active_checkpoints"].as_array().ok_or("connection active checkpoints are absent")?
        .iter().filter(|id| id.as_str().is_some_and(|id| ids.contains(id))).cloned().collect();
    value["checkpoints"].as_array_mut().ok_or("connection checkpoints are absent")?
        .retain(|checkpoint| !checkpoint["id"].as_str().is_some_and(|id| ids.contains(id)));
    value["active_checkpoints"].as_array_mut().ok_or("connection active checkpoints are absent")?
        .retain(|id| !id.as_str().is_some_and(|id| ids.contains(id)));
    let state = &mut value["interface_state"][config.provider_interface.as_str()];
    state[FRONTIER_CHECKPOINT_IDS].as_array_mut().ok_or("connection frontier is absent")?
        .retain(|id| !id.as_str().is_some_and(|id| ids.contains(id)));
    state[RUN_AUTHORING_BINDING] = serde_json::to_value(prior).map_err(|error| error.to_string())?;
    let bytes = if value == before_value { before_bytes.to_vec() } else { JsonFormat::Compact.encode(&value)? };
    Ok(Some(Extension { protected: bytes, checkpoints, templates, ids: added_ids }))
}
