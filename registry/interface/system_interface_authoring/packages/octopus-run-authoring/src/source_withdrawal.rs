use super::*;

#[derive(Clone, Debug)]
pub struct PendingSourceIntent {
    pub source_movement_seq: u64,
    pub artifact: ArtifactRef,
    pub payload: Vec<u8>,
    pub context_generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Withdrawal {
    pub(super) predecessor: PendingHandoff,
    predecessor_record: String,
    source: ArtifactRef,
    control: ArtifactRef,
    control_source: u64,
    parent: Option<ArtifactRef>,
    replaced_context: Vec<(ArtifactRef, u64)>,
}

pub(super) fn follows(before: &PendingHandoff, after: &PendingHandoff) -> bool {
    after.withdrawal.as_ref().is_some_and(|proof| proof.predecessor == *before)
        && before.source_movement_seq == after.source_movement_seq
        && before.items != after.items
}

fn proof<'a>(record: &'a RunRecord, provider: &InterfaceId) -> Result<Option<PendingHandoff>, String> {
    Ok(persisted_pending_handoff(record, provider)?.filter(|pending| pending.withdrawal.is_some()))
}

pub(super) fn publication_bindings(record: &RunRecord, config: &RunAuthoringConfig, pending: &PendingHandoff) -> Result<Vec<serde_json::Value>, String> {
    let proof = pending.withdrawal.as_ref().ok_or("source withdrawal has no proof")?;
    let [item] = pending.items.as_slice() else { return Err("source withdrawal requires one closure snapshot".into()); };
    let mut bindings = provider_context_bindings(record, &config.provider_interface)?;
    let protected = current_mandatory_role_bindings(record, config)?.into_iter().map(|binding| binding.id)
        .chain(config.payload_artifact_bindings.iter().map(|reference| reference.id.clone()))
        .chain(config.checkpoints.iter().flat_map(|template| template.static_artifact_bindings.iter().cloned())).collect::<BTreeSet<_>>();
    let replaced = proof.replaced_context.iter().map(|(reference, _)| reference.id.clone()).collect::<BTreeSet<_>>();
    if replaced.len() != proof.replaced_context.len() || replaced.is_empty()
        || !replaced.is_disjoint(&protected)
        || proof.parent.as_ref().is_some_and(|parent| !proof.replaced_context.iter().any(|(reference, _)| reference == parent))
        || proof.replaced_context.iter().any(|(reference, _)| bindings.iter().filter(|binding|
            binding.id == reference.id && binding.access == octopus_policy::ArtifactAccess::read_only()).count() != 1)
        || bindings.iter().any(|binding| binding.id == item.artifact_id)
    { return Err("source withdrawal lost its exact dynamic context or changed static artifact authority".into()); }
    bindings.retain(|binding| !replaced.contains(&binding.id));
    bindings.push(ArtifactBinding { id: item.artifact_id.clone(), access: Default::default() });
    if pending.context_publication.as_ref() != Some(&bindings.iter().map(|binding| binding.id.clone()).collect()) {
        return Err("source withdrawal may replace only its selected dynamic carrier".into());
    }
    bindings.iter().map(serde_json::to_value).collect::<Result<_, _>>().map_err(|error| error.to_string())
}

pub(super) fn validate_checked_transition(before: &RunRecord, after: &RunRecord, writer: &CheckedRunWriter) -> Result<(), String> {
    if let (Some(prior), Some(next)) = (persisted_pending_handoff(before, &writer.owner)?, persisted_pending_handoff(after, &writer.owner)?) {
        if prior.items == next.items && prior.dispatch_fenced != next.dispatch_fenced {
            return Err("checked transition cannot retrofit dispatch fencing onto a retained output".into());
        }
    }
    if proof(after, &writer.owner)?.is_none() { return Ok(()); }
    let config = config_from_persisted_profile(before, &writer.owner, &RunAuthoringProfile {
        file_write_tool: writer.tool.clone(), file_write_label: writer.call_label.clone(),
        file_write_result_label: writer.result_label.clone(), result_timeout_ms: 1,
    })?;
    validate_transition(before, after, &config)
}

fn validate_transition(before: &RunRecord, after: &RunRecord, config: &RunAuthoringConfig) -> Result<(), String> {
    let provider = &config.provider_interface;
    let Some(next) = proof(after, provider)? else { return Ok(()); };
    let proof = next.withdrawal.as_ref().expect("selected proof");
    let prior = persisted_pending_handoff(before, provider)?.ok_or("withdrawal has no installed predecessor")?;
    let publication = context_publication::is_publication_completion(&prior, &next);
    let previous = &before.interface_state[provider];
    let current = &after.interface_state[provider];
    let mut expected = serde_json::to_value(before).map_err(|error| error.to_string())?;
    result_inputs::cancel_pending_inputs(&mut expected, config, &proof.predecessor)?;
    if expected["interface_state"][provider.as_str()].get(RUN_AUTHORING_BINDING) != current.get(RUN_AUTHORING_BINDING)
        || !next.terminal || next.source_without_context || next.settlement_of.is_some()
        || !next.dispatch_fenced || next.items.len() != 1
    { return Err("source withdrawal changed its source, topology or terminal closure contract".into()); }
    if !publication && (!follows(&prior, &next) || !prior.dispatch_fenced
        || !prior.terminal || prior.source_without_context || prior.withdrawal.is_some()
        || prior.items.len() != 1 || prior.context_publication.is_none()
        || proof.control_source <= prior.source_movement_seq)
    { return Err("source withdrawal requires one exact never-submitted terminal intent".into()); }
    let old_generation = optional_u64(previous, "context_generation")?;
    if old_generation.checked_add(u64::from(!publication)) != Some(optional_u64(current, "context_generation")?) {
        return Err("source withdrawal changed its exact context generation".into());
    }
    let bindings = publication_bindings(before, config, if publication { &prior } else { &next })?;
    let prior_bindings = provider_context_bindings(before, provider)?.iter().map(serde_json::to_value)
        .collect::<Result<Vec<_>, _>>().map_err(|error| error.to_string())?;
    let prefix = checkpoint_prefix(&before.id, &next.items[0].artifact_id, optional_u64(current, "authoring_generation")?);
    let root = expected.as_object_mut().ok_or("withdrawal is not an object")?;
    replace_context_checkpoints(root, config, &before.id, &prefix, if publication { &bindings } else { &prior_bindings })?;
    if publication && !result_inputs::enabled(config) { replace_verified_input_checkpoints(root, config, config, &before.id, &prefix, &bindings)?; }
    let expected = RunRecord::from_bytes(&serde_json::to_vec(&expected).map_err(|error| error.to_string())?).map_err(|error| error.to_string())?;
    for template in config.checkpoints.iter().filter(|template| template.role.is_provider_input()) {
        let rows = |record: &RunRecord| record.checkpoints.iter().filter(|checkpoint|
            record.active_checkpoints.contains(&checkpoint.id) && checkpoint_record_matches_template(checkpoint, template, &record.id)
        ).cloned().collect::<Vec<_>>();
        if rows(&expected) != rows(after) { return Err("source withdrawal changed canonical input rows or artifact rights".into()); }
    }
    let future_outputs = validate_pending_output_frontier(after, provider, &next)?;
    for item in &proof.predecessor.items {
        if after.active_checkpoints.contains(&item.checkpoint.id) || after.checkpoints.iter().any(|checkpoint|
            after.active_checkpoints.contains(&checkpoint.id) && !future_outputs.contains(&checkpoint.id)
                && checkpoint.interface_binding.as_ref().is_some_and(|binding|
                binding.interface == *provider && binding.admits(&item.artifact_label))) {
            return Err("withdrawal left an active admission path for its old output".into());
        }
    }
    let item = &next.items[0];
    if !config.checkpoints.iter().any(|template| {
        let expected = pending_output_checkpoint(template, item.checkpoint.id.as_str(), &item.artifact_label, &[], &[]);
        template.role == RunAuthoringCheckpointRole::ProviderArtifactOutput
            && template.interface_binding.admits(&item.artifact_label)
            && item.checkpoint.interface_binding == expected.interface_binding
            && item.checkpoint.outbound_interface_bindings == expected.outbound_interface_bindings
            && item.checkpoint.required_verified_artifacts == expected.required_verified_artifacts
    }) { return Err("withdrawal closure cannot dispatch an effect".into()); }
    Ok(())
}

pub(super) fn validate_source(reader: &mut impl CheckedRunEvidenceReader, record: &RunRecord, provider: &InterfaceId, seq: u64, reference: &ArtifactRef) -> Result<Movement, String> {
    let movement = reader.admitted_movement(seq)?.ok_or("withdrawal source has no admitted movement")?;
    let metadata = movement.invocation_metadata.as_ref().and_then(|metadata| metadata.invocation()).ok_or("withdrawal source has no invocation")?;
    let runtime = metadata.runtime.as_ref().ok_or("withdrawal source has no runtime coordinate")?;
    if movement.seq.0 != seq || movement.kind != MovementKind::InterfaceOutbound
        || movement.to != Endpoint::Interface(provider.clone()) || movement.artifact.as_ref() != Some(reference)
        || runtime.run != record.id || runtime.worker != record.worker
        || runtime.interface != *provider || metadata.verification_failure.is_some()
        || !metadata.checkpoint_admission.as_ref().is_some_and(|admission| admission.complete)
    { return Err("withdrawal changed its exact admitted source coordinate".into()); }
    let hash = runtime.run_record_hash.as_deref().and_then(ContentHash::from_hex).ok_or("withdrawal source has no pinned checkpoint")?;
    let bytes = reader.snapshot(&hash)?;
    if ContentHash::of_bytes(&bytes) != hash { return Err("withdrawal source snapshot hash mismatch".into()); }
    let pinned = RunRecord::from_bytes(&bytes).map_err(|error| error.to_string())?;
    let checkpoint = pinned.checkpoint(&runtime.checkpoint).ok_or("withdrawal source checkpoint is absent")?;
    if pinned.id != record.id || !pinned.active_checkpoints.contains(&runtime.checkpoint) || checkpoint.epoch.0 != runtime.epoch
        || checkpoint.interface_binding.as_ref().map(|binding| Endpoint::Interface(binding.interface.clone())).as_ref() != Some(&movement.from)
        || !checkpoint.outbound_interface_bindings.iter().any(|binding| binding.interface == *provider)
    { return Err("withdrawal source differs from its exact pinned admission".into()); }
    if metadata.checkpoint_binding_policy.as_ref() != Some(&CheckpointBindingPolicySnapshot {
        inbound: checkpoint.interface_binding.clone().ok_or("withdrawal source has no inbound binding")?,
        outbound: checkpoint.outbound_interface_bindings.clone(),
    }) || metadata.required_verified_artifacts != checkpoint.required_verified_artifacts
        || metadata.outbound_interface_bindings != checkpoint.outbound_interface_bindings.iter().map(|binding| binding.interface.clone()).collect::<Vec<_>>() {
        return Err("withdrawal source metadata changed its pinned contract".into());
    }
    Ok(movement)
}

fn validate_control(reader: &mut impl CheckedRunEvidenceReader, record: &RunRecord, provider: &InterfaceId, proof: &Withdrawal) -> Result<(), String> {
    let movement = validate_source(reader, record, provider, proof.control_source, &proof.control)?;
    let runtime = movement.runtime().ok_or("withdrawal control lost its runtime coordinate")?;
    let topology: BoundRunAuthoringTopology = serde_json::from_value(record.interface_state[provider][RUN_AUTHORING_BINDING].clone()).map_err(|error| error.to_string())?;
    let checkpoint = record.checkpoint(&runtime.checkpoint).ok_or("withdrawal control is no longer the selected independent input")?;
    if !record.active_checkpoints.contains(&checkpoint.id) || checkpoint.epoch.0 != runtime.epoch || !topology.checkpoints.iter().any(|template|
        template.role == RunAuthoringCheckpointRole::IndependentInput && !template.include_context
            && checkpoint_record_matches_template(checkpoint, template, &record.id)) {
        return Err("withdrawal control is not an exact unchanged independent input".into());
    }
    Ok(())
}

fn rejects_old_movement(movement: &Movement, record: &RunRecord, proof: &Withdrawal) -> bool {
    movement.run().is_none_or(|run| run == &record.id) && proof.predecessor.items.iter().any(|item|
        movement.artifact.as_ref().is_some_and(|reference| reference.id == item.artifact_id && reference.content.to_hex() == item.artifact_content_hash))
}

fn validate_replaced_context(reader: &mut impl CheckedRunEvidenceReader, record: &RunRecord, provider: &InterfaceId, proof: &Withdrawal) -> Result<(), String> {
    for (reference, seq) in &proof.replaced_context {
        let row = reader.admitted_movement(*seq)?.ok_or("withdrawal dynamic context has no movement")?;
        if *seq > proof.predecessor.source_movement_seq || row.seq.0 != *seq || row.artifact.as_ref() != Some(reference)
            || row.run() != Some(&record.id)
            || !((row.kind == MovementKind::InterfaceOutbound && row.to == Endpoint::Interface(provider.clone()))
                || (row.kind == MovementKind::InterfaceArrival && row.from == Endpoint::Interface(provider.clone())))
        { return Err("withdrawal dynamic context differs from its exact provider movement".into()); }
        reader.artifact(reference)?;
    }
    Ok(())
}

pub(super) fn source_snapshot_from_evidence(
    evidence: &CheckedRunEvidence<'_>, ctx: Option<&InterfaceLiftContext>, record: &RunRecord,
    config: &RunAuthoringConfig, source: u64,
) -> Result<(serde_json::Value, Vec<(ArtifactRef, u64)>), String> {
    let limits = InterfaceHostIoLimits { max_control_frame_bytes: evidence.max_control_frame_bytes,
        max_stderr_bytes: evidence.max_control_frame_bytes, max_manifest_bytes: evidence.max_manifest_bytes,
        max_payload_bytes: evidence.max_payload_bytes };
    let session = ProviderHandoffSession { interface: config.provider_interface.clone(), movements_path: evidence.movements_path.to_path_buf(),
        writer_history: Arc::new(Mutex::new(BTreeMap::new())), writer_call_generations: Arc::new(Mutex::new(writer_run::WriterCallIndex::default())),
        evidence: Arc::new(Mutex::new(ProviderLedgerEvidenceIndex::with_spotlight(config.provider_interface.clone(),
            evidence.movements_path.to_path_buf(), evidence.max_control_frame_bytes, evidence.ledger_index_path))) };
    session.index()?.through_seq = Some(source);
    let (_, admitted) = session.index()?.exact_provider_source_outbound(&record.id, &record.worker, &config.provider_interface, source)?;
    let (outputs, snapshots) = if let Some(ctx) = ctx.filter(|ctx| ctx.interface == config.provider_interface) {
        provider_context_label_sets(ctx.descriptor_snapshot.as_ref().ok_or("withdrawal has no declared context contract")?)?
    } else {
        let mut outputs = BTreeSet::new();
        let mut snapshots = BTreeSet::new();
        let hash = admitted.run_record_hash.as_deref().and_then(ContentHash::from_hex).ok_or("withdrawal source lost its pinned record")?;
        let (_, pinned) = pinned_run_record_from_cache(evidence.blobs_dir, limits, &hash, "withdrawal source")?;
        let source_config = config_from_persisted_profile(&pinned, &config.provider_interface, &RunAuthoringProfile {
            file_write_tool: config.file_write_tool.clone(), file_write_label: config.file_write_label.clone(),
            file_write_result_label: config.file_write_result_label.clone(), result_timeout_ms: config.result_timeout_ms,
        })?;
        let (coordinate, _) = session.index()?.exact_provider_source_outbound(&record.id, &record.worker, &config.provider_interface, source)?;
        let checkpoint = pinned.checkpoint(&CheckpointId::new(&coordinate.checkpoint)).ok_or("withdrawal source checkpoint is absent")?;
        let templates = source_config.checkpoints.iter().filter(|template| template.role.is_provider_input()
            && template.include_context && checkpoint_record_matches_template(checkpoint, template, &pinned.id)).collect::<Vec<_>>();
        let [template] = templates.as_slice() else { return Err("withdrawal source has no unique context contract".into()); };
        let (payloads, static_ids) = source_context_payloads_from_cache(evidence.blobs_dir, limits, &pinned, &source_config, checkpoint, template, &admitted)?;
        let mut pending = payloads.into_iter().filter(|bound| !static_ids.contains(&bound.artifact.id)).map(|bound| bound.artifact).collect::<Vec<_>>();
        let mut seen = BTreeSet::new();
        while let Some(reference) = pending.pop() {
            if !seen.insert(artifact_ref_key(&reference)) { continue; }
            let bytes = read_artifact_ref_payload_from_cache(evidence.blobs_dir, limits, &reference)?;
            if reference.id.as_str() == format!("{INSTRUCTION_TEXT_LABEL}:{}", reference.content.to_hex()) { continue; }
            let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
            if let Some(snapshot) = bound_session_snapshot(&value, &reference)? {
                validate_session_snapshot(&snapshot, reference.id.as_str(), &snapshot.provider_interface)?;
                if snapshot.provider_interface != config.provider_interface { continue; }
                let label = value.get("label").and_then(serde_json::Value::as_str)
                    .or_else(|| reference.id.as_str().rsplit_once(':').map(|(label, _)| label))
                    .ok_or("admitted snapshot has no self-identifying label")?;
                snapshots.insert(label.to_string()); outputs.insert(label.to_string());
                pending.extend(snapshot.parent);
                pending.extend(snapshot.artifacts.into_iter().map(|entry| { outputs.insert(entry.label); entry.source_artifact }));
            } else if let Some(label) = value.get("label").and_then(serde_json::Value::as_str) {
                outputs.insert(label.to_string());
            }
        }
        (outputs, snapshots)
    };
    let (context, head, mut protected) = recover_source_session_context_from_cache(evidence.blobs_dir, limits, &session,
        record, config, source, &outputs, &snapshots)?;
    protected.extend(current_mandatory_role_artifacts(record, config)?);
    protected.extend(config.payload_artifact_bindings.iter().map(|reference| reference.id.clone()));
    protected.extend(config.checkpoints.iter().flat_map(|template| template.static_artifact_bindings.iter().cloned()));
    let replaced = provider_context_bindings(record, &config.provider_interface)?.into_iter()
        .filter(|binding| !protected.contains(&binding.id)).map(|binding| {
            if binding.access != octopus_policy::ArtifactAccess::read_only() { return Err("withdrawal dynamic context changed its read-only rights".into()); }
            let entry = context.artifacts.iter().find(|entry| entry.source_artifact.id == binding.id)
                .ok_or("withdrawal predecessor binding is absent from its exact source context")?;
            Ok((entry.source_artifact.clone(), entry.source_movement_seq.ok_or("withdrawal predecessor context has no movement")?))
        }).collect::<Result<Vec<_>, String>>()?;
    let snapshot = session_snapshot_for_context(&context, head.as_ref())?;
    Ok((serde_json::to_value(snapshot).map_err(|error| error.to_string())?, replaced))
}

fn validate_snapshot_evidence(reader: &mut impl CheckedRunEvidenceReader, record: &RunRecord, config: &RunAuthoringConfig, pending: &PendingHandoff) -> Result<(), String> {
    let proof = pending.withdrawal.as_ref().ok_or("withdrawal has no snapshot evidence")?;
    let (expected, replaced) = reader.source_context_snapshot(record, config, proof.predecessor.source_movement_seq)?;
    if proof.parent != serde_json::from_value(expected.get("parent").cloned().unwrap_or(serde_json::Value::Null)).map_err(|error| error.to_string())?
        || proof.replaced_context != replaced {
        return Err("withdrawal changed the exact predecessor snapshot parent or complete dynamic replacement set".into());
    }
    let [item] = pending.items.as_slice() else { return Err("withdrawal has no exact closure".into()); };
    let payload = reader.pending_output_payload(&config.provider_interface, &closure_ref(pending)?, &item.manifest_relative_path)?;
    let value: serde_json::Value = serde_json::from_slice(&payload).map_err(|error| error.to_string())?;
    if value.get(SESSION_CONTEXT_FIELD) != Some(&expected) {
        return Err("withdrawal snapshot differs from its exact source context parent and delta".into());
    }
    Ok(())
}

pub(super) fn validate_evidence(reader: &mut impl CheckedRunEvidenceReader, transition: &CheckedRunTransition) -> Result<(), String> {
    if result_inputs::definition_transition(transition)? { return Ok(()); }
    let after = RunRecord::from_bytes(&transition.after).map_err(|error| error.to_string())?;
    let Some(next) = proof(&after, &transition.writer.owner)? else { return Ok(()); };
    let before = RunRecord::from_bytes(&transition.before).map_err(|error| error.to_string())?;
    validate_checked_transition(&before, &after, &transition.writer)?;
    let prior = persisted_pending_handoff(&before, &transition.writer.owner)?.ok_or("withdrawal predecessor missing")?;
    let config = config_from_persisted_profile(&before, &transition.writer.owner, &RunAuthoringProfile {
        file_write_tool: transition.writer.tool.clone(), file_write_label: transition.writer.call_label.clone(),
        file_write_result_label: transition.writer.result_label.clone(), result_timeout_ms: 1,
    })?;
    validate_snapshot_evidence(reader, &before, &config, &next)?;
    if context_publication::is_publication_completion(&prior, &next) { return Ok(()); }
    let proof = next.withdrawal.as_ref().expect("selected proof");
    if ContentHash::of_bytes(&transition.before).to_hex() != proof.predecessor_record || proof.control_source >= transition.call.seq.0 {
        return Err("withdrawal changed its exact checked predecessor or causal control".into());
    }
    validate_source(reader, &before, &transition.writer.owner, prior.source_movement_seq, &proof.source)?;
    validate_control(reader, &before, &transition.writer.owner, proof)?;
    validate_replaced_context(reader, &before, &transition.writer.owner, proof)?;
    for seq in prior.source_movement_seq..=transition.result.seq.0 {
        let movement = reader.admitted_movement(seq)?.ok_or("withdrawal has no complete ledger through its checked write")?;
        if movement.seq.0 != seq || rejects_old_movement(&movement, &before, proof) {
            return Err("withdrawn output already has movement evidence or an incomplete prefix".into());
        }
    }
    Ok(())
}

pub(super) fn validate_saved_attempt(ctx: &InterfaceLiftContext, before: &[u8], after: &[u8], config: &RunAuthoringConfig) -> Result<(), String> {
    let record = RunRecord::from_bytes(before).map_err(|error| error.to_string())?;
    let candidate = RunRecord::from_bytes(after).map_err(|error| error.to_string())?;
    let Some(next) = proof(&candidate, &config.provider_interface)? else { return Ok(()); };
    let config = attempt_config(&record, config)?;
    validate_transition(&record, &candidate, &config)?;
    let prior = persisted_pending_handoff(&record, &config.provider_interface)?.ok_or("withdrawal predecessor missing")?;
    let limits = ctx.required_host_io_limits()?;
    let evidence = CheckedRunEvidence { movements_path: &ctx.movements_path, ledger_index_path: &ctx.ledger_index_path, blobs_dir: &ctx.blobs_dir,
        max_control_frame_bytes: limits.max_control_frame_bytes, max_manifest_bytes: limits.max_manifest_bytes, max_payload_bytes: limits.max_payload_bytes };
    let mut reader = FileCheckedRunReader::open(&evidence, None)?.with_provider_context(ctx);
    validate_snapshot_evidence(&mut reader, &record, &config, &next)?;
    if context_publication::is_publication_completion(&prior, &next) { return Ok(()); }
    let proof = next.withdrawal.as_ref().expect("selected proof");
    if proof.predecessor_record != ContentHash::of_bytes(before).to_hex() { return Err("withdrawal private attempt changed its preimage".into()); }
    validate_absence(ctx, &record, proof)
}

fn validate_absence(ctx: &InterfaceLiftContext, record: &RunRecord, proof: &Withdrawal) -> Result<(), String> {
    if !proof.predecessor.dispatch_fenced { return Err("pending intent predates durable hand-in fencing; withdrawal is not proven".into()); }
    for item in &proof.predecessor.items {
        if dispatch_path(ctx, record, item).try_exists().map_err(|error| error.to_string())? {
            return Err("pending output was submitted or its hand-in outcome is uncertain; withdrawal refused".into());
        }
    }
    let limits = ctx.required_host_io_limits()?;
    let evidence = CheckedRunEvidence { movements_path: &ctx.movements_path, ledger_index_path: &ctx.ledger_index_path, blobs_dir: &ctx.blobs_dir,
        max_control_frame_bytes: limits.max_control_frame_bytes, max_manifest_bytes: limits.max_manifest_bytes, max_payload_bytes: limits.max_payload_bytes };
    let mut reader = FileCheckedRunReader::open(&evidence, None)?;
    validate_source(&mut reader, record, &ctx.interface, proof.predecessor.source_movement_seq, &proof.source)?;
    validate_control(&mut reader, record, &ctx.interface, proof)?;
    validate_replaced_context(&mut reader, record, &ctx.interface, proof)?;
    loop {
        let row = match read_bounded_ledger_row(&mut reader.reader, limits.max_control_frame_bytes)? {
            BoundedLedgerRow::Complete(row) => row,
            BoundedLedgerRow::Eof => break,
            BoundedLedgerRow::Partial => return Err("withdrawal requires a complete durable ledger boundary".into()),
        };
        let movement = reader.rows.decoder.decode(&row, reader.next_seq as usize).map_err(|error| error.to_string())?;
        if movement.seq.0 != reader.next_seq || rejects_old_movement(&movement, record, proof) {
            return Err("withdrawal lost its ledger prefix or the output already reached an edge".into());
        }
        reader.next_seq = reader.next_seq.checked_add(1).ok_or("withdrawal ledger sequence overflow")?;
    }
    if reader.next_seq <= proof.control_source { return Err("withdrawal control lies beyond the durable prefix".into()); }
    Ok(())
}

pub(super) fn dispatch_path(ctx: &InterfaceLiftContext, record: &RunRecord, item: &PendingHandoffItem) -> PathBuf {
    let key = serde_json::to_vec(&(record.id.clone(), item)).expect("serializable pending coordinate");
    ctx.interface_var_dir().join("handoff-dispatch").join(format!("{}.json", ContentHash::of_bytes(&key).to_hex()))
}

impl ProviderOutputHandoff {
    pub(super) async fn dispatch_ownership(&self) -> Result<std::fs::File, String> {
        let root = self.ctx.interface_var_dir().join("handoff-dispatch");
        std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
        let lock = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false)
            .open(root.join(format!("{}.lock", ContentHash::of_bytes(self.config.run.as_str().as_bytes()).to_hex())))
            .map_err(|error| error.to_string())?;
        loop {
            match lock.try_lock() {
                Ok(()) => return Ok(lock),
                Err(std::fs::TryLockError::WouldBlock) => tokio::select! {
                    _ = self.ctx.cancel.cancelled() => return Err("provider handoff ownership was cancelled".into()),
                    _ = tokio::time::sleep(Duration::from_millis(10)) => {},
                },
                Err(error) => return Err(error.to_string()),
            }
        }
    }

    pub(super) fn validate_dispatch_frontier(&self, pending: &PendingHandoff, installed: &[u8]) -> Result<(), String> {
        let current = std::fs::read(&self.run_path).map_err(|error| error.to_string())?;
        let current = RunRecord::from_bytes(&current).map_err(|error| error.to_string())?;
        let installed = RunRecord::from_bytes(installed).map_err(|error| error.to_string())?;
        let active = persisted_pending_handoff(&current, &self.config.provider_interface)?.ok_or("handoff has no selected frontier")?;
        if current.id != installed.id || active.items != pending.items || active.source_movement_seq != pending.source_movement_seq
            || active.withdrawal != pending.withdrawal
        { return Err("handoff output no longer owns the selected checked frontier".into()); }
        validate_pending_items_are_active(&current, &self.config.provider_interface, pending).map(|_| ())
    }

    pub(super) fn record_pending_dispatch(&self, pending: &PendingHandoff, item: &PendingHandoffItem) -> Result<(), String> {
        if !pending.dispatch_fenced { return Ok(()); }
        let record = RunRecord::from_bytes(&std::fs::read(&self.run_path).map_err(|error| error.to_string())?).map_err(|error| error.to_string())?;
        let path = dispatch_path(&self.ctx, &record, item);
        let bytes = serde_json::to_vec(item).map_err(|error| error.to_string())?;
        use std::io::Write;
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                file.write_all(&bytes).and_then(|_| file.sync_all()).map_err(|error| error.to_string())?;
                std::fs::File::open(path.parent().ok_or("dispatch marker has no parent")?).and_then(|file| file.sync_all()).map_err(|error| error.to_string())?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if std::fs::read(&path).map_err(|error| error.to_string())? != bytes { return Err("handoff dispatch evidence is corrupt".into()); }
            }
            Err(error) => return Err(error.to_string()),
        }
        Ok(())
    }

    pub async fn pending_source_intent(&self) -> Result<Option<PendingSourceIntent>, String> {
        let state = self.state.lock().await;
        let record = RunRecord::from_bytes(&state.run_record_bytes).map_err(|error| error.to_string())?;
        let Some(pending) = persisted_pending_handoff(&record, &self.config.provider_interface)? else { return Ok(None); };
        if pending.source_movement_seq != self.source_movement_seq || pending.withdrawal.is_some() { return Ok(None); }
        if self.pending_handoff_evidence(&pending)?.iter().all(Option::is_some) { return Ok(None); }
        if pending.terminal && !pending.source_without_context
            && self.pending_source_fully_refused(&record, &pending)?
        { return Ok(None); }
        let effects = pending.items.iter().filter(|item| self.config.checkpoints.iter().any(|template| {
            let expected = pending_output_checkpoint(template, item.checkpoint.id.as_str(), &item.artifact_label, &[], &[]);
            template.role == RunAuthoringCheckpointRole::ProviderToolOutput
                && expected.interface_binding == item.checkpoint.interface_binding
                && expected.outbound_interface_bindings == item.checkpoint.outbound_interface_bindings
                && expected.required_verified_artifacts == item.checkpoint.required_verified_artifacts
        })).collect::<Vec<_>>();
        let item = match effects.as_slice() {
            [] => return Ok(None),
            [item] => *item,
            _ => return Err("cold pending source has multiple unresolved effect targets".into()),
        };
        let (file, artifact) = load_pending_artifact(&self.var_dir, item)?;
        Ok(Some(PendingSourceIntent { source_movement_seq: pending.source_movement_seq, artifact,
            payload: file.read_payload_bytes().map_err(|error| error.to_string())?, context_generation: optional_u64(&record.interface_state[&self.config.provider_interface], "context_generation")? }))
    }

    pub async fn hand_source_withdrawal(&self, intent: &PendingSourceIntent, control_source: u64, control: &ArtifactRef, emission: ProviderEmission) -> Result<ProviderHandoffOutcome, String> {
        let withdrawal = {
            let state = self.state.lock().await;
            let record = RunRecord::from_bytes(&state.run_record_bytes).map_err(|error| error.to_string())?;
            let pending = persisted_pending_handoff(&record, &self.config.provider_interface)?.ok_or("withdrawal has no pending source")?;
            if pending.source_movement_seq != intent.source_movement_seq || pending.items.len() != 1
                || pending.items[0].artifact_id != intent.artifact.id || pending.items[0].artifact_content_hash != intent.artifact.content.to_hex()
            { return Err("withdrawal intent changed before its checked write".into()); }
            self.independent_input_at_source(&state.run_record_bytes, control_source)?.ok_or("withdrawal requires an exact context-free independent input")?;
            let (_, source) = self.session.index()?.exact_provider_source_outbound(&self.config.run, &self.config.worker, &self.config.provider_interface, self.source_movement_seq)?;
            let replaced_context = provider_context_bindings(&record, &self.config.provider_interface)?.iter()
                .filter(|binding| !state.static_context_artifacts.contains(&binding.id)).map(|binding| {
                let entry = state.session_context.artifacts.iter().find(|entry| entry.source_artifact.id == binding.id).ok_or("withdrawal has unproved dynamic context")?;
                Ok((entry.source_artifact.clone(), entry.source_movement_seq.ok_or("withdrawal dynamic context has no admitted coordinate")?))
            }).collect::<Result<Vec<_>, String>>()?;
            Withdrawal { predecessor: pending, predecessor_record: ContentHash::of_bytes(&state.run_record_bytes).to_hex(), source: source.artifact,
                control: control.clone(), control_source, parent: state.session_snapshot_head.as_ref().map(|head| head.artifact.clone()), replaced_context }
        };
        let label = emission.artifact.read_manifest().map_err(|error| error.to_string())?.label;
        if emission.route != ProviderEmissionRoute::ClientProjection || !self.session_snapshot_labels.contains(&label) || !self.context_output_labels.contains(&label) {
            return Err("withdrawal requires one declared client context snapshot".into());
        }
        self.hand_rearm_batch_with_bindings(vec![emission.into()], true, &[], false, None, None, None, Some(&withdrawal), false).await?
            .into_iter().next().ok_or("withdrawal returned no closure".into())
    }

    pub async fn validate_source_withdrawal(&self, control_source: u64, control: &ArtifactRef) -> Result<(), String> {
        let state = self.state.lock().await;
        let record = RunRecord::from_bytes(&state.run_record_bytes).map_err(|error| error.to_string())?;
        let mut pending = persisted_pending_handoff(&record, &self.config.provider_interface)?.ok_or("source withdrawal lost its checked closure")?;
        if pending.source_movement_seq != self.source_movement_seq {
            let (_, source) = self.session.index()?.exact_provider_source_outbound(&self.config.run, &self.config.worker, &self.config.provider_interface, self.source_movement_seq)?;
            let hash = source.run_record_hash.as_deref().and_then(ContentHash::from_hex).ok_or("withdrawal lost its pinned original source")?;
            let (before, _) = pinned_run_record(&self.ctx, &hash, "withdrawal original source")?;
            let chain = checked_selected_run_advance_state(&self.ctx, &self.run_path, &before, &state.run_record_bytes, self.source_movement_seq, &self.config).map_err(String::from)?;
            pending = chain.iter().map(|transition| {
                let after = RunRecord::from_bytes(&transition.after).map_err(|error| error.to_string())?;
                persisted_pending_handoff(&after, &self.config.provider_interface)
            }).collect::<Result<Vec<_>, String>>()?.into_iter().flatten()
                .filter(|pending| pending.source_movement_seq == self.source_movement_seq).last().ok_or("withdrawal has no checked historical closure")?;
        }
        if pending.context_publication.is_some() { return Err("withdrawal closure is not yet published".into()); }
        self.admitted_replay_handoff(&pending)?;
        let proof = pending.withdrawal.as_ref().ok_or("source closure is not a checked withdrawal")?;
        if pending.source_movement_seq != self.source_movement_seq || proof.control_source != control_source || proof.control != *control {
            return Err("source withdrawal changed its exact authorizing input".into());
        }
        Ok(())
    }

    pub(super) fn attach_source_withdrawal(&self, before: &[u8], authored: &mut AuthoredRun, proof: &Withdrawal) -> Result<(), String> {
        let record = RunRecord::from_bytes(before).map_err(|error| error.to_string())?;
        if ContentHash::of_bytes(before).to_hex() != proof.predecessor_record { return Err("withdrawal predecessor changed".into()); }
        authored.pending_handoff.withdrawal = Some(Box::new(proof.clone()));
        let mut value: serde_json::Value = serde_json::from_str(&authored.content).map_err(|error| error.to_string())?;
        value["interface_state"][self.config.provider_interface.as_str()]["pending_handoff"] = serde_json::to_value(&authored.pending_handoff).map_err(|error| error.to_string())?;
        result_inputs::cancel_pending_inputs(&mut value, &self.config, &proof.predecessor)?;
        let prefix = checkpoint_prefix(&record.id, &authored.pending_handoff.items[0].artifact_id, correlation_generation(&record.id, &authored.correlation_id)?);
        let bindings = provider_context_bindings(&record, &self.config.provider_interface)?.iter().map(serde_json::to_value).collect::<Result<Vec<_>, _>>().map_err(|error| error.to_string())?;
        replace_context_checkpoints(value.as_object_mut().ok_or("withdrawal is not an object")?, &self.config, &record.id, &prefix, &bindings)?;
        let bytes = authored.json_format.unwrap_or(JsonFormat::Compact).encode(&value)?;
        validate_saved_attempt(&self.ctx, before, &bytes, &self.config)?;
        authored.content = String::from_utf8(bytes).map_err(|error| error.to_string())?;
        authored.after_hash = ContentHash::of_bytes(authored.content.as_bytes()).to_hex();
        authored.patch = json_patch::diff(&serde_json::from_slice(before).map_err(|error| error.to_string())?, &value);
        Ok(())
    }

    pub(super) fn validate_installed_withdrawal(&self, current: &[u8]) -> Result<(), String> {
        let record = RunRecord::from_bytes(current).map_err(|error| error.to_string())?;
        let pending = persisted_pending_handoff(&record, &self.config.provider_interface)?.ok_or("installed withdrawal is absent")?;
        validate_absence(&self.ctx, &record, pending.withdrawal.as_ref().ok_or("installed withdrawal lost its proof")?)
    }
}


pub struct PendingSourceWithdrawal {
    pub intent: PendingSourceIntent,
    pub closure: InterfaceArtifactFile,
    pub control_source: u64,
    pub control: ArtifactRef,
}

pub(super) fn closure_ref(pending: &PendingHandoff) -> Result<ArtifactRef, String> {
    let [item] = pending.items.as_slice() else { return Err("withdrawal has no single closure".into()); };
    Ok(ArtifactRef { id: item.artifact_id.clone(), content: ContentHash::from_hex(&item.artifact_content_hash).ok_or("withdrawal closure hash is malformed")? })
}

pub(super) fn require_validated_output(state: &HandoffState, pending: &PendingHandoff) -> Result<(), String> {
    if pending.withdrawal.is_some() && state.validated_withdrawal_output.as_ref() != Some(&closure_ref(pending)?) {
        return Err("saved source withdrawal requires provider validation of its exact persisted closure before install or hand-in".into());
    }
    Ok(())
}

impl ProviderOutputHandoff {
    pub async fn pending_source_withdrawal(&self) -> Result<Option<PendingSourceWithdrawal>, String> {
        let state = self.state.lock().await;
        let record = RunRecord::from_bytes(&state.run_record_bytes).map_err(|error| error.to_string())?;
        let selected = if let Some(pending) = proof(&record, &self.config.provider_interface)? { pending }
        else {
            let AuthoringAttemptRecovery::InFlight(attempt) = self.recover_authoring_attempt(&record, &state.run_record_bytes)? else { return Ok(None); };
            if attempt.authored.pending_handoff.withdrawal.is_none() { return Ok(None); }
            attempt.authored.pending_handoff
        };
        if selected.source_movement_seq != self.source_movement_seq { return Ok(None); }
        let proof = selected.withdrawal.as_ref().ok_or("saved withdrawal lost its evidence")?;
        let item = proof.predecessor.items.first().ok_or("saved withdrawal lost its predecessor")?;
        let (intent, artifact) = load_pending_artifact(&self.var_dir, item)?;
        let (closure, _) = load_pending_artifact(&self.var_dir, &selected.items[0])?;
        let value: serde_json::Value = serde_json::from_slice(&closure.read_payload_bytes().map_err(|error| error.to_string())?).map_err(|error| error.to_string())?;
        let _ = value.get(SESSION_CONTEXT_FIELD).ok_or("saved withdrawal has no snapshot")?;
        let generation = optional_u64(&record.interface_state[&self.config.provider_interface], "context_generation")?
            .checked_sub(u64::from(persisted_pending_handoff(&record, &self.config.provider_interface)?.is_some_and(|pending| pending.withdrawal.is_some())))
            .ok_or("saved withdrawal has invalid context generation")?;
        Ok(Some(PendingSourceWithdrawal { intent: PendingSourceIntent { source_movement_seq: selected.source_movement_seq, artifact,
            payload: intent.read_payload_bytes().map_err(|error| error.to_string())?, context_generation: generation }, closure,
            control_source: proof.control_source, control: proof.control.clone() }))
    }

    pub async fn validate_source_withdrawal_output(&self, reference: &ArtifactRef) -> Result<(), String> {
        let pending = self.pending_source_withdrawal().await?.ok_or("no saved withdrawal awaits provider validation")?;
        if artifact_identity(&pending.closure, &self.var_dir)?.artifact != *reference { return Err("provider validated a different withdrawal closure".into()); }
        self.state.lock().await.validated_withdrawal_output = Some(reference.clone());
        Ok(())
    }
}
