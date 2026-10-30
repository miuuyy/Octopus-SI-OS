use super::*;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputArtifactBinding {
    pub artifact: ArtifactRef,
    pub source_movement_seq: u64,
}

impl InputArtifactBinding {
    pub fn read_bindings(&self, mut context: Vec<ArtifactBinding>) -> Result<Vec<ArtifactBinding>, String> {
        let suffix = format!(":{}", self.artifact.content.to_hex());
        if self.artifact.id.as_str().strip_suffix(&suffix).is_none_or(|name| name.is_empty() || name.trim() != name) {
            return Err("input artifact binding is not an exact manifest identity".into());
        }
        if let Some(bound) = context.iter().find(|bound| bound.id == self.artifact.id) {
            if bound.access != octopus_policy::ArtifactAccess::read_only() {
                return Err("input artifact binding cannot replace existing object rights".into());
            }
        } else {
            context.push(ArtifactBinding { id: self.artifact.id.clone(), access: octopus_policy::ArtifactAccess::read_only() });
        }
        Ok(context)
    }
}

pub fn selections(record: &RunRecord, owner: &InterfaceId) -> Result<Vec<InputArtifactBinding>, String> {
    record.interface_state.get(owner).and_then(|state| state.get("input_artifact_bindings"))
        .map(|value| serde_json::from_value(value.clone()).map_err(|error| error.to_string())).unwrap_or_else(|| Ok(Vec::new()))
}

pub fn read_bindings(record: &RunRecord, owner: &InterfaceId, context: Vec<ArtifactBinding>) -> Result<Vec<ArtifactBinding>, String> {
    selections(record, owner)?.iter().try_fold(context, |context, source| source.read_bindings(context))
}

pub fn validate_input_transition(reader: &mut impl CheckedRunEvidenceReader, transition: &CheckedRunTransition) -> Result<(), String> {
    let after = RunRecord::from_bytes(&transition.after).map_err(|error| error.to_string())?;
    let sources = selections(&after, &transition.writer.owner)?;
    let before = RunRecord::from_bytes(&transition.before).map_err(|error| error.to_string())?;
    let prior = selections(&before, &transition.writer.owner)?;
    if !sources.starts_with(&prior) || sources.len() > prior.len() + 1 {
        return Err("input artifact bindings must retain their exact admitted prefix".into());
    }
    for source in &sources[prior.len()..] {
        validate_source(reader, transition, &after, source)?;
    }
    Ok(())
}

pub(super) fn checked_source(reader: &mut impl CheckedRunEvidenceReader, after: &RunRecord,
    owner: &InterfaceId, source: &InputArtifactBinding) -> Result<(Movement, RunRecord), String> {
    let movement = reader.admitted_movement(source.source_movement_seq)?.ok_or("input artifact binding source is absent")?;
    let metadata = movement.invocation_metadata.as_ref().and_then(|value| value.invocation())
        .ok_or("input artifact binding source has no admitted metadata")?;
    let runtime = metadata.runtime.as_ref().ok_or("input artifact binding source has no runtime coordinate")?;
    let hash = runtime.run_record_hash.as_deref().and_then(ContentHash::from_hex)
        .ok_or("input artifact binding source has no pinned run snapshot")?;
    let bytes = reader.snapshot(&hash)?;
    if ContentHash::of_bytes(&bytes) != hash { return Err("input artifact binding snapshot failed integrity".into()); }
    let pinned = RunRecord::from_bytes(&bytes).map_err(|error| error.to_string())?;
    let checkpoint = pinned.checkpoint(&runtime.checkpoint).ok_or("input artifact binding source checkpoint is absent")?;
    let inbound = checkpoint.interface_binding.as_ref().ok_or("input artifact binding source has no inbound binding")?;
    let binding = CheckpointBindingPolicySnapshot { inbound: inbound.clone(), outbound: checkpoint.outbound_interface_bindings.clone() };
    let (manifest, _) = reader.artifact(&source.artifact)?;
    let emitted = movement.kind == MovementKind::InterfaceOutbound && metadata.checkpoint_admission.is_none();
    let edge = match movement.kind {
        MovementKind::InterfaceArrival => movement.from == Endpoint::Interface(owner.clone())
            && movement.to == Endpoint::Interface(after.worker.clone()) && inbound.interface == *owner
            && runtime.interface == *owner && pinned.id == after.id && pinned.worker == after.worker,
        MovementKind::InterfaceOutbound if emitted => {
            let active = pinned.active_checkpoints.iter().map(|id| pinned.checkpoint(id)
                .ok_or("input artifact binding source has an absent active checkpoint")).collect::<Result<Vec<_>, _>>()?;
            let targets = active.into_iter()
                .flat_map(|candidate| candidate.interface_binding.iter().chain(candidate.outbound_interface_bindings.iter()))
                .filter(|bound| bound.interface == *owner).cloned().collect::<Vec<_>>();
            let source_bound = movement.from == Endpoint::Interface(inbound.interface.clone())
                || checkpoint.outbound_interfaces().any(|interface| movement.from == Endpoint::Interface(interface.clone()));
            source_bound && movement.to == Endpoint::Interface(owner.clone()) && runtime.interface == *owner
                && !targets.is_empty() && targets.iter().all(|bound| bound.admits(&manifest.label))
                && metadata.allowed_interfaces().iter().any(|interface| interface == owner.as_str())
                && metadata.checkpoint_binding_policy.as_ref() == Some(&CheckpointBindingPolicySnapshot {
                    inbound: inbound.clone(), outbound: targets,
                })
                && metadata.required_verified_artifacts.is_empty()
                && metadata.outbound_interface_bindings.is_empty()
                && metadata.artifact_binding_refs.is_empty()
        }
        MovementKind::InterfaceOutbound => movement.from == Endpoint::Interface(inbound.interface.clone())
            && movement.to == Endpoint::Interface(owner.clone()) && runtime.interface == *owner
            && checkpoint.outbound_interfaces().any(|interface| interface == owner),
        _ => false,
    };
    let forwarded = emitted || (metadata.checkpoint_binding_policy.as_ref() == Some(&binding)
        && metadata.required_verified_artifacts == checkpoint.required_verified_artifacts
        && metadata.outbound_interface_bindings == checkpoint.outbound_interfaces().cloned().collect::<Vec<_>>()
        && metadata.checkpoint_admission.as_ref().is_some_and(|admission| admission.complete
            && admission.artifact_label == manifest.label
            && admission.required_artifact_labels == checkpoint.required_verified_artifacts.iter().map(|required| required.artifact_label.clone()).collect::<Vec<_>>())
        && inbound.admits(&manifest.label)
        && (movement.kind != MovementKind::InterfaceOutbound || checkpoint.outbound_interface_bindings.iter().any(|bound| bound.interface == *owner && bound.admits(&manifest.label))));
    if !edge || movement.seq.0 != source.source_movement_seq || movement.artifact.as_ref() != Some(&source.artifact) || movement.trace != checkpoint.trace
        || runtime.run != pinned.id || runtime.ee != pinned.ee || runtime.worker != pinned.worker || checkpoint.run != pinned.id
        || !pinned.active_checkpoints.contains(&checkpoint.id) || checkpoint.epoch.0 != runtime.epoch
        || metadata.verification_failure.is_some() || !forwarded {
        return Err("input artifact binding differs from its exact admitted source edge".into());
    }
    source.read_bindings(Vec::new())?;
    Ok((movement, pinned))
}

fn validate_source(reader: &mut impl CheckedRunEvidenceReader, transition: &CheckedRunTransition,
    after: &RunRecord, source: &InputArtifactBinding) -> Result<(), String> {
    if source.source_movement_seq >= transition.call.seq.0 {
        return Err("input artifact binding predates its admitted source".into());
    }
    let owner = &transition.writer.owner;
    checked_source(reader, after, owner, source)?;
    let before = RunRecord::from_bytes(&transition.before).map_err(|error| error.to_string())?;
    let templates = before.interface_state.iter().filter_map(|(provider, state)| state.get(RUN_AUTHORING_BINDING).map(|value| (provider, value)))
        .map(|(provider, value)| serde_json::from_value::<BoundRunAuthoringTopology>(value.clone()).map(|topology| (provider, topology)).map_err(|error| error.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    let mut count = 0;
    for (provider, topology) in templates {
        for template in topology.checkpoints.iter().filter(|template| template.role == RunAuthoringCheckpointRole::OwnerInput
            && template.include_context && template.owner_authoring.as_ref() == Some(&transition.writer)) {
            let expected = read_bindings(after, owner, provider_context_bindings(&before, provider)?)?;
            for checkpoint in after.checkpoints.iter().filter(|checkpoint| after.active_checkpoints.contains(&checkpoint.id)
                && checkpoint_record_matches_template(checkpoint, template, &after.id)) {
                if checkpoint.artifact_bindings != expected { return Err("input artifact binding was not published as exact read-only context".into()); }
                count += 1;
            }
        }
    }
    if count != 1 { return Err("input artifact binding has no unique declared owner input".into()); }
    Ok(())
}

pub(super) fn owner_sources(record: &RunRecord, provider: &InterfaceId) -> Result<Vec<ArtifactRef>, String> {
    let Some(value) = record.interface_state.get(provider).and_then(|state| state.get(RUN_AUTHORING_BINDING)) else { return Ok(Vec::new()); };
    let topology: BoundRunAuthoringTopology = serde_json::from_value(value.clone()).map_err(|error| error.to_string())?;
    let mut sources = Vec::new();
    for template in topology.checkpoints.iter().filter(|template| template.role == RunAuthoringCheckpointRole::OwnerInput && template.include_context) {
        if let Some(writer) = &template.owner_authoring {
            for source in selections(record, &writer.owner)? {
                if !sources.contains(&source.artifact) { sources.push(source.artifact); }
            }
        }
    }
    Ok(sources)
}

pub fn bound_sources(record: &RunRecord, provider: &InterfaceId, checkpoint: &CheckpointId, admitted: &[ArtifactRef]) -> Result<Vec<ArtifactRef>, String> {
    let mut sources = Vec::new();
    if let Some(source) = result_inputs::bound_source(record, provider, checkpoint)? {
        sources.push(source);
    }
    for source in owner_sources(record, provider)? {
        if !sources.contains(&source) { sources.push(source); }
    }
    if sources.is_empty() { return Ok(sources); }
    let checkpoint = record.checkpoint(checkpoint).ok_or("input source selection has no pinned invocation checkpoint")?;
    sources.retain(|source| checkpoint.artifact_bindings.iter().any(|binding| binding.id == source.id)
        || admitted.iter().any(|reference| reference.id == source.id));
    Ok(sources)
}

pub fn result_source(record: &RunRecord, provider: &InterfaceId, checkpoint: &CheckpointId) -> Result<Option<ArtifactRef>, String> {
    result_inputs::bound_source(record, provider, checkpoint)
}

pub fn result_call(record: &RunRecord, provider: &InterfaceId, checkpoint: &CheckpointId) -> Result<Option<ArtifactRef>, String> {
    result_inputs::bound_call_evidence(record, provider, checkpoint)
}
