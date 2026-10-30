use super::*;

pub(super) fn validate_initial_history(
    ctx: &InterfaceLiftContext,
    path: &Path,
    bytes: &[u8],
    config: &RunAuthoringConfig,
    before_seq: u64,
) -> Result<(), String> {
    let record = RunRecord::from_bytes(bytes).map_err(|error| error.to_string())?;
    let state = record
        .interface_state
        .get(&config.provider_interface)
        .ok_or("initial provider state absent")?;
    if optional_u64(state, "authoring_generation")? == 0
        || persisted_pending_handoff(&record, &config.provider_interface)?.is_some()
    {
        return Ok(());
    }
    if optional_u64(state, "context_generation")? != 0 {
        return Err("initial provider state contains context without a handoff".into());
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
    let limits = ctx.required_host_io_limits()?;
    let evidence = CheckedRunEvidence {
        movements_path: &ctx.movements_path,
        ledger_index_path: &ctx.ledger_index_path,
        blobs_dir: &ctx.blobs_dir,
        max_control_frame_bytes: limits.max_control_frame_bytes,
        max_manifest_bytes: limits.max_manifest_bytes,
        max_payload_bytes: limits.max_payload_bytes,
    };
    let mut reader = FileCheckedRunReader::open(&evidence, None)?;
    let mut anchor: Option<(u64, Vec<u8>)> = None;
    let correlation = format!("provider-run-authoring:{}:1", record.id);
    while reader.next_sequence() < before_seq {
        let Some(row) = reader.movement(reader.next_sequence())? else {
            break;
        };
        if row.kind != MovementKind::InterfaceOutbound
            || row.from != Endpoint::Interface(writer.source.clone())
            || row.to != Endpoint::Interface(writer.door.clone())
        {
            continue;
        }
        let (manifest, payload) = reader.artifact(
            row.artifact
                .as_ref()
                .ok_or("initial writer call has no artifact")?,
        )?;
        if manifest.label != writer.call_label {
            continue;
        }
        let call: octopus_tool_handlers::ToolCallArtifact =
            serde_json::from_slice(&payload).map_err(|error| error.to_string())?;
        if call.tool != writer.tool || call.correlation_id.as_deref() != Some(&correlation) {
            continue;
        }
        let runtime = row
            .runtime()
            .ok_or("initial writer call has no admitted coordinate")?;
        let hash = runtime
            .run_record_hash
            .as_deref()
            .and_then(ContentHash::from_hex)
            .ok_or("initial writer call has no snapshot")?;
        let snapshot = reader.snapshot(&hash)?;
        if ContentHash::of_bytes(&snapshot) != hash {
            return Err("initial writer snapshot failed integrity".into());
        }
        let before = if call.arguments.get("ordered").is_some() {
            let ordered: writer_run::OrderedArguments =
                serde_json::from_value(call.arguments).map_err(|error| error.to_string())?;
            if !ordered.targets(path) {
                continue;
            }
            ordered
                .checked_call(&writer, &snapshot, path, limits.max_payload_bytes)?
                .0
        } else {
            let arguments: FileWriteCallArguments =
                serde_json::from_value(call.arguments).map_err(|error| error.to_string())?;
            if runtime.run != record.id || arguments.path != path {
                continue;
            }
            snapshot
        };
        let original = RunRecord::from_bytes(&before).map_err(|error| error.to_string())?;
        let state = original
            .interface_state
            .get(&writer.owner)
            .ok_or("initial writer preimage lost its provider")?;
        if original.id != record.id
            || original.worker != record.worker
            || optional_u64(state, "authoring_generation")? != 0
            || optional_u64(state, "context_generation")? != 0
            || persisted_pending_handoff(&original, &writer.owner)?.is_some()
        {
            return Err("initial writer preimage is not the exact unspent task".into());
        }
        if anchor.replace((row.seq.0, before)).is_some() {
            return Err("initial provider write is ambiguous".into());
        }
    }
    let (first, original) =
        anchor.ok_or("initial provider generation has no admitted definition write")?;
    let writers = declared_writer_closure(
        &RunRecord::from_bytes(&original).map_err(|error| error.to_string())?,
        vec![writer.clone()],
    )?;
    let mut proof = FileCheckedRunReader::open(
        &evidence,
        Some(
            first
                .checked_sub(1)
                .ok_or("initial write has no preceding coordinate")?,
        ),
    )?;
    let scan_start = proof.next_sequence();
    let chain = checked_run_file_chain_with_reader(
        &mut proof,
        path,
        &original,
        CheckedRunTarget::Snapshot(bytes),
        first - 1,
        scan_start,
        Some(before_seq),
        &writers,
        &[],
        None,
        None,
    )
    .map_err(String::from)?;
    if chain.is_empty() {
        return Err("initial provider generation has no complete checked write".into());
    }
    for transition in &chain {
        validate_checked_owner_transition(transition)?;
        if transition.writer.owner == writer.owner
            && !checked_revision(transition)
            && validate_payload_revision(&transition.before, &transition.after, &writer).is_err()
        {
            return Err(
                "initial provider history contains an execution write before its first input"
                    .into(),
            );
        }
    }
    Ok(())
}

fn profile(writer: &CheckedRunWriter) -> RunAuthoringProfile {
    RunAuthoringProfile {
        file_write_tool: writer.tool.clone(),
        file_write_label: writer.call_label.clone(),
        file_write_result_label: writer.result_label.clone(),
        result_timeout_ms: 1,
    }
}

fn topology(
    record: &RunRecord,
    provider: &InterfaceId,
) -> Result<BoundRunAuthoringTopology, String> {
    serde_json::from_value(
        record
            .interface_state
            .get(provider)
            .and_then(|state| state.get(RUN_AUTHORING_BINDING))
            .cloned()
            .ok_or("provider input definition has no bound topology")?,
    )
    .map_err(|error| error.to_string())
}

pub fn declared_owner_input_bindings(
    record: &RunRecord,
    provider_writer: &CheckedRunWriter,
    owner_writer: &CheckedRunWriter,
    binding: &CheckpointBindingPolicySnapshot,
) -> Result<Vec<ArtifactBinding>, String> {
    let declared = validate_declared_provider_writer(record, provider_writer)?;
    let config = declared.config(&profile(provider_writer));
    config.validate(&provider_writer.owner, &record.id)?;
    validate_checkpoint_topology(record, &provider_writer.owner, &declared.checkpoints, None)?;
    let candidates = declared
        .checkpoints
        .iter()
        .filter(|template| {
            template.role == RunAuthoringCheckpointRole::OwnerInput
                && template.binding_policy_snapshot() == *binding
        })
        .collect::<Vec<_>>();
    let [template] = candidates.as_slice() else {
        return Err("owner output requires one exact declared provider input".into());
    };
    if template.owner_authoring.as_ref() != Some(owner_writer)
        || !template.required_verified_artifacts.is_empty()
    {
        return Err("owner output differs from its declared writer or requirements".into());
    }
    if template.include_context {
        return input_artifacts::read_bindings(
            record,
            &owner_writer.owner,
            provider_context_bindings(record, &provider_writer.owner)?,
        );
    }
    let mut bindings = current_mandatory_role_bindings(record, &config)?;
    for id in &template.static_artifact_bindings {
        if !bindings.iter().any(|binding| binding.id == *id) {
            bindings.push(
                serde_json::from_value(serde_json::json!({"id":id}))
                    .map_err(|error| error.to_string())?,
            );
        }
    }
    Ok(bindings)
}

#[derive(Debug, serde::Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ProviderInputRevision {
    Unchanged,
    Deferred { checkpoints: Vec<CheckpointId> },
    Prepared { edit: CheckedRunEdit },
}

fn same_edge(before: &RunAuthoringCheckpoint, after: &RunAuthoringCheckpoint) -> bool {
    before.role == RunAuthoringCheckpointRole::OwnerInput
        && after.role == before.role
        && before.owner_authoring == after.owner_authoring
        && before.interface_binding == after.interface_binding
        && before.outbound_interface_bindings == after.outbound_interface_bindings
        && before.required_verified_artifacts == after.required_verified_artifacts
}

pub(super) fn replacements(
    before: &BoundRunAuthoringTopology,
    after: &BoundRunAuthoringTopology,
) -> Result<Vec<RunAuthoringCheckpoint>, String> {
    let mut protected = after.clone();
    protected.checkpoints = before.checkpoints.clone();
    if protected != *before || before.checkpoints.len() != after.checkpoints.len() {
        return Err(
            "input definition revision changed provider identity or topology membership".into(),
        );
    }
    let mut changed = Vec::new();
    for (old, new) in before.checkpoints.iter().zip(&after.checkpoints) {
        if old == new {
            continue;
        }
        if !same_edge(old, new) {
            return Err(
                "input definition revision changed an edge, writer, policy or requirement".into(),
            );
        }
        changed.push(old.clone());
    }
    Ok(changed)
}

fn active_returns(record: &RunRecord, changed: &[RunAuthoringCheckpoint]) -> Vec<CheckpointId> {
    record
        .checkpoints
        .iter()
        .filter(|checkpoint| {
            record.active_checkpoints.contains(&checkpoint.id)
                && changed.iter().any(|template| {
                    checkpoint.interface_binding.as_ref() == Some(&template.interface_binding)
                        && checkpoint.outbound_interface_bindings
                            == template.outbound_interface_bindings
                })
        })
        .map(|checkpoint| checkpoint.id.clone())
        .collect()
}

fn static_revision_candidate(
    record: &RunRecord, writer: &CheckedRunWriter, next: &BoundRunAuthoringTopology,
) -> Result<bool, String> {
    let old = validate_declared_provider_writer(record, writer)?;
    let changed = replacements(&old, next)?;
    let [template] = changed.as_slice() else { return Ok(false); };
    let replacement = next.checkpoints.iter().find(|candidate| same_edge(template, candidate))
        .ok_or("active input correction lost its exact owner edge")?;
    let mut protected = replacement.clone();
    protected.static_artifact_bindings = template.static_artifact_bindings.clone();
    Ok(protected == *template && template.include_context
        && template.static_artifact_bindings.iter().all(|id| replacement.static_artifact_bindings.contains(id))
        && record.status.as_str() == Some("idle") && active_returns(record, &changed).len() == 1
        && persisted_pending_handoff(record, &writer.owner)?.is_some_and(|pending| {
            pending.terminal && pending.source_without_context && pending.dispatch_fenced
                && pending.context_publication.is_none() && pending.settlement_of.is_none()
                && pending.withdrawal.is_none() && pending.items.len() == 1
        }))
}

fn static_revision_checkpoint<'a>(
    record: &'a RunRecord,
    writer: &CheckedRunWriter,
    next: &BoundRunAuthoringTopology,
) -> Result<&'a octopus_run_record::CheckpointRecord, String> {
    if !static_revision_candidate(record, writer, next)? {
        return Err("active input correction is not a terminal failed-source static revision".into());
    }
    let old = validate_declared_provider_writer(record, writer)?;
    let changed = replacements(&old, next)?;
    let [template] = changed.as_slice() else {
        return Err("active input correction requires exactly one owner input definition".into());
    };
    let replacement = next.checkpoints.iter().find(|candidate| same_edge(template, candidate))
        .ok_or("active input correction lost its exact owner edge")?;
    let active = active_returns(record, &changed);
    let [id] = active.as_slice() else {
        return Err("active input correction requires one exact materialized owner input".into());
    };
    let checkpoint = record.checkpoint(id).ok_or("active input correction checkpoint absent")?;
    if !checkpoint_record_matches_template(checkpoint, template, &record.id) {
        return Err("active input correction differs from its materialized contract".into());
    }
    let declared = old.checkpoints.iter()
        .filter(|row| row.role == RunAuthoringCheckpointRole::Stable && row.include_context)
        .flat_map(|row| row.static_artifact_bindings.iter()).collect::<BTreeSet<_>>();
    let context = provider_context_bindings(record, &writer.owner)?;
    let mut dynamic = payload_artifact_bindings(record, &writer.owner)?.into_iter()
        .map(|reference| reference.id).collect::<BTreeSet<_>>();
    for owner in record.interface_state.keys() {
        dynamic.extend(input_artifacts::selections(record, owner)?.into_iter().map(|input| input.artifact.id));
    }
    for id in replacement.static_artifact_bindings.iter().filter(|id| !template.static_artifact_bindings.contains(id)) {
        let bindings = checkpoint.artifact_bindings.iter().filter(|binding| &binding.id == id).collect::<Vec<_>>();
        let [binding] = bindings.as_slice() else { return Err("static correction needs one exact existing binding".into()); };
        if !declared.contains(id) || dynamic.contains(id) || !binding.access.read
            || context.iter().filter(|bound| *bound == *binding).count() != 1
        {
            return Err("static correction is not existing declared readable provider context".into());
        }
    }
    Ok(checkpoint)
}

#[derive(PartialEq, Eq)]
pub(super) struct FailedSourceStaticContext {
    coordinate: RuntimeCoordinateKey,
    snapshot: ContentHash,
    checkpoint: octopus_run_record::CheckpointRecord,
    source: IndexedOutboundArtifact,
    added: Vec<ArtifactRef>,
}

impl FailedSourceStaticContext {
    pub(super) fn refs_for(
        &self, coordinate: &RuntimeCoordinateKey, source: &IndexedOutboundArtifact,
        snapshot: &ContentHash, checkpoint: &octopus_run_record::CheckpointRecord,
    ) -> Result<&[ArtifactRef], String> {
        if &self.coordinate != coordinate || &self.source != source
            || &self.snapshot != snapshot || &self.checkpoint != checkpoint
        {
            return Err("static correction proof differs from the exact failed source context".into());
        }
        Ok(&self.added)
    }

    fn extend(&mut self, next: Self) -> Result<(), String> {
        self.refs_for(&next.coordinate, &next.source, &next.snapshot, &next.checkpoint)?;
        for reference in next.added {
            if !self.added.contains(&reference) { self.added.push(reference); }
        }
        Ok(())
    }
}

pub(super) fn checked_failed_source_static_context(
    evidence: &CheckedRunEvidence<'_>, path: &Path, chain: &[CheckedRunTransition],
    provider: &InterfaceId, source: &IndexedOutboundArtifact,
) -> Result<Option<FailedSourceStaticContext>, String> {
    let mut selected: Option<FailedSourceStaticContext> = None;
    let mut reader = FileCheckedRunReader::open(evidence, None)?;
    for transition in chain {
        if &transition.writer.owner != provider { continue; }
        let before = RunRecord::from_bytes(&transition.before).map_err(|error| error.to_string())?;
        if persisted_pending_handoff(&before, provider)?.is_none_or(|pending| pending.source_movement_seq != source.seq) {
            continue;
        }
        let after = RunRecord::from_bytes(&transition.after).map_err(|error| error.to_string())?;
        let old = topology(&before, provider)?;
        let next = topology(&after, provider)?;
        if !is_revision(&old, &next) || active_returns(&before, &replacements(&old, &next)?).is_empty() {
            continue;
        }
        let proof = static_revision_evidence(&mut reader, path, &transition.before,
            &transition.writer, &next, transition.call.seq.0)?;
        if &proof.source != source {
            return Err("static correction belongs to another failed source".into());
        }
        match &mut selected {
            Some(selected) => selected.extend(proof)?,
            None => selected = Some(proof),
        }
    }
    Ok(selected)
}

fn validate_static_revision_evidence(
    reader: &mut impl CheckedRunEvidenceReader,
    index: &ProviderLedgerEvidenceIndex,
    path: &Path,
    current: &[u8],
    writer: &CheckedRunWriter,
    next: &BoundRunAuthoringTopology,
    before_seq: u64,
) -> Result<FailedSourceStaticContext, String> {
    let record = RunRecord::from_bytes(current).map_err(|error| error.to_string())?;
    let checkpoint = static_revision_checkpoint(&record, writer, next)?;
    let pending = persisted_pending_handoff(&record, &writer.owner)?.ok_or("failed source absent")?;
    let matches = index.coordinate_outbounds.iter().filter(|(coordinate, _)| {
        coordinate.run == record.id.as_str() && coordinate.worker == record.worker.as_str()
            && coordinate.interface == writer.owner.as_str()
    }).flat_map(|(coordinate, rows)| rows.iter().filter(|row| row.seq == pending.source_movement_seq)
        .map(move |row| (coordinate, row))).collect::<Vec<_>>();
    let [(coordinate, source)] = matches.as_slice() else { return Err("failed source has no unique admitted provider input".into()); };
    let source_manifest = reader.artifact(&source.artifact)?.0;
    if index.session_provider_inputs.get(&(artifact_ref_key(&source.artifact), record.id.to_string()))
        .and_then(|inputs| inputs.get(&source.seq)) != Some(&source_manifest.label)
    { return Err("failed source is not a completely admitted input artifact".into()); }
    let hash = source.run_record_hash.as_deref().and_then(ContentHash::from_hex)
        .ok_or("failed source has no pinned run snapshot")?;
    let bytes = reader.snapshot(&hash)?;
    if ContentHash::of_bytes(&bytes) != hash { return Err("failed source snapshot integrity mismatch".into()); }
    let original = RunRecord::from_bytes(&bytes).map_err(|error| error.to_string())?;
    if persisted_pending_handoff(&original, &writer.owner)?.as_ref() == Some(&pending) {
        return Err("failed source was admitted after its claimed terminal completion".into());
    }
    let topology = validate_declared_provider_writer(&original, writer)?;
    let templates = topology.checkpoints.iter().filter(|template| {
        template.role == RunAuthoringCheckpointRole::OwnerInput && template.include_context
            && template.interface() == &source.source
            && checkpoint_record_matches_template(checkpoint, template, &record.id)
            && indexed_evidence_matches_checkpoint(source, template)
    }).collect::<Vec<_>>();
    if original.id != record.id || original.worker != record.worker
        || original.checkpoint(&checkpoint.id) != Some(checkpoint)
        || !original.active_checkpoints.contains(&checkpoint.id)
        || coordinate.checkpoint != checkpoint.id.as_str() || coordinate.epoch != checkpoint.epoch.0
        || templates.len() != 1 || !artifact_refs_match_bindings(&source.artifact_binding_refs, &checkpoint.artifact_bindings)
    {
        return Err("failed source differs from the exact active input snapshot or binding rights".into());
    }
    let added = next.checkpoints.iter().find(|template| same_edge(templates[0], template))
        .ok_or("failed source correction changed its admitted input edge")?;
    let declared = topology.checkpoints.iter()
        .filter(|template| template.role == RunAuthoringCheckpointRole::Stable && template.include_context)
        .flat_map(|template| template.static_artifact_bindings.iter()).collect::<BTreeSet<_>>();
    let mut dynamic = payload_artifact_bindings(&original, &writer.owner)?.into_iter().map(|reference| reference.id).collect::<BTreeSet<_>>();
    for owner in original.interface_state.keys() {
        dynamic.extend(input_artifacts::selections(&original, owner)?.into_iter().map(|input| input.artifact.id));
    }
    for id in added.static_artifact_bindings.iter().filter(|id| !templates[0].static_artifact_bindings.contains(id)) {
        if !declared.contains(id) || dynamic.contains(id) || id == &source.artifact.id || !source.readable_artifacts.contains(id)
            || source.artifact_binding_refs.iter().filter(|reference| &reference.id == id).count() != 1
        { return Err("static correction was not readable in the exact failed input".into()); }
    }
    for (other, rows) in &index.coordinate_outbounds {
        if other.run != coordinate.run || other.worker != coordinate.worker || other.interface != coordinate.interface { continue; }
        for row in rows.iter().filter(|row| row.seq < before_seq && row.seq != source.seq) {
            if (other.checkpoint == coordinate.checkpoint && other.epoch == coordinate.epoch)
                || (row.seq > source.seq && topology.checkpoints.iter().any(|template| {
                    template.role.is_provider_input() && template.interface() == &row.source
                        && indexed_evidence_matches_checkpoint(row, template)
                }))
            { return Err("static correction would cross another admitted provider input".into()); }
        }
    }
    let writers = declared_writer_closure(&original, vec![writer.clone()])?;
    let start = reader.concurrent_writer_start(path, &original, &bytes, source.seq, &writers)?;
    let mut history = PayloadHistoryReader(reader as &mut dyn CheckedRunEvidenceReader);
    let chain = checked_run_file_chain_with_reader(&mut history, path, &bytes, CheckedRunTarget::Snapshot(current),
        start, start.checked_add(1).ok_or("failed source history overflow")?, Some(before_seq), &writers, &[], None, None)
        .map_err(String::from)?;
    let mut terminal = None;
    for transition in &chain {
        validate_checked_owner_transition(transition)?;
        let after = RunRecord::from_bytes(&transition.after).map_err(|error| error.to_string())?;
        if transition.writer == *writer && persisted_pending_handoff(&after, &writer.owner)?.as_ref() == Some(&pending) {
            terminal.get_or_insert(transition.result.seq.0);
        }
    }
    let terminal = terminal.ok_or("failed source has no exact checked terminal error write")?;
    validate_pending_items_are_active(&record, &writer.owner, &pending)?;
    let item = &pending.items[0];
    let reference = pending_ref(item)?;
    let (manifest, payload) = history.artifact(&reference)?;
    let error: serde_json::Value = serde_json::from_slice(&payload).map_err(|error| error.to_string())?;
    if manifest.label != item.artifact_label || manifest.label != "turn.error"
        || error.get("label").and_then(serde_json::Value::as_str) != Some(manifest.label.as_str())
        || error.get("will_retry").and_then(serde_json::Value::as_bool) != Some(false)
        || error.pointer("/additional_details/source_movement_seq").and_then(serde_json::Value::as_u64) != Some(source.seq)
        || error.get("message").and_then(serde_json::Value::as_str).is_none_or(|message| message.trim().is_empty())
    { return Err("failed source has no exact non-retry terminal error artifact".into()); }
    let admission = index.indexed_handoff_seq(PendingHandoffLedgerCoordinate {
        interface: &writer.owner, run: &record.id, worker: &record.worker, artifact: &reference,
        required_evidence: pending_required_evidence(item)?, checkpoint: &item.checkpoint.id, epoch: item.checkpoint.epoch,
    }, before_seq)?.ok_or("failed source terminal error was not admitted before correction")?;
    if admission <= terminal { return Err("failed source terminal error predates its checked installation".into()); }
    Ok(FailedSourceStaticContext {
        coordinate: (**coordinate).clone(), snapshot: hash, checkpoint: checkpoint.clone(),
        source: (**source).clone(),
        added: added.static_artifact_bindings.iter()
            .filter(|id| !templates[0].static_artifact_bindings.contains(id))
            .map(|id| source.artifact_binding_refs.iter().find(|reference| &reference.id == id)
                .cloned().ok_or_else(|| "static correction lost its proven source ref".to_string()))
            .collect::<Result<_, _>>()?,
    })
}

impl CheckedRunEvidence<'_> {
    pub fn prepare_provider_input_revision(
        &self, path: PathBuf, current: &[u8], writer: &CheckedRunWriter,
        desired_inputs: Vec<serde_json::Value>, before_sequence: u64,
    ) -> Result<ProviderInputRevision, String> {
        prepare_input_revision(path, current, writer, desired_inputs, Some((self, before_sequence)))
    }
}

pub fn provider_input_rearm_ready(
    evidence: &CheckedRunEvidence<'_>,
    record: &RunRecord,
    provider: &InterfaceId,
) -> Result<bool, String> {
    readiness(evidence, record, provider, u64::MAX)
}

pub fn provider_input_payloads_ready(
    evidence: &CheckedRunEvidence<'_>,
    path: &Path,
    current: &[u8],
    writer: &CheckedRunWriter,
    source: &InterfaceId,
) -> Result<bool, String> {
    payload_readiness(evidence, path, current, writer, source, u64::MAX)
}

fn payload_phase(
    record: &RunRecord,
    provider: &InterfaceId,
    source: &InterfaceId,
) -> Result<(), String> {
    if record.status.as_str() != Some("idle") {
        return Err("payload selection requires an idle input boundary".into());
    }
    let declared = topology(record, provider)?;
    if declared
        .checkpoints
        .iter()
        .filter(|template| {
            template.role == RunAuthoringCheckpointRole::Stable
                && template.include_context
                && template.interface() == source
        })
        .count()
        != 1
    {
        return Err("payload selection requires one declared stable input source".into());
    }
    match persisted_pending_handoff(record, provider)? {
        Some(pending) if pending.terminal && pending.context_publication.is_none() => {}
        None if optional_u64(
            record
                .interface_state
                .get(provider)
                .ok_or("provider state absent")?,
            "context_generation",
        )? == 0 => {}
        _ => return Err("payload selection interrupts unfinished provider work".into()),
    }
    Ok(())
}

fn payload_index_ready<R: CheckedRunEvidenceReader + ?Sized>(
    reader: &mut R,
    index: &ProviderLedgerEvidenceIndex,
    path: &Path,
    current: &[u8],
    writer: &CheckedRunWriter,
    source: &InterfaceId,
    before_seq: u64,
) -> Result<bool, String> {
    let record = RunRecord::from_bytes(current).map_err(|error|error.to_string())?;
    let provider = &writer.owner;
    payload_phase(&record, provider, source)?;
    let declared = validate_declared_provider_writer(&record, writer)?;
    let pending = persisted_pending_handoff(&record, provider)?;
    if let Some(pending) = &pending {
        validate_pending_items_are_active(&record, provider, pending)?;
        if !all_admitted(index, pending, &record, provider, before_seq)? {
            return Ok(false);
        }
        for item in &pending.items {
            if reader.artifact(&pending_ref(item)?)?.0.label != item.artifact_label {
                return Err("payload readiness pending artifact changed its admitted label".into());
            }
        }
    }
    let mut newer = BTreeMap::new();
    for (coordinate, rows) in &index.coordinate_outbounds {
        if coordinate.run != record.id.as_str() || coordinate.worker != record.worker.as_str() {
            continue;
        }
        for row in rows.iter().filter(|row| row.seq < before_seq) {
            if pending
                .as_ref()
                .is_some_and(|pending| row.seq <= pending.source_movement_seq)
            {
                continue;
            }
            let templates = declared.checkpoints.iter().filter(|template| {
                template.role.is_provider_input()
                    && template.interface() == &row.source
                    && indexed_evidence_matches_checkpoint(row, template)
            }).collect::<Vec<_>>();
            if templates.is_empty() { continue; }
            if pending.is_none() || templates.len() != 1 {
                return Ok(false);
            }
            if newer.insert(row.seq, (coordinate.clone(), row.clone())).is_some() {
                return Err("payload readiness repeats an admitted input coordinate".into());
            }
        }
    }
    completed_inputs(reader, index, path, current, writer, before_seq, &newer)
}

pub(super) fn payload_readiness(
    evidence: &CheckedRunEvidence<'_>,
    path: &Path,
    current: &[u8],
    writer: &CheckedRunWriter,
    source: &InterfaceId,
    before_seq: u64,
) -> Result<bool, String> {
    let mut index = ProviderLedgerEvidenceIndex::with_spotlight(
        writer.owner.clone(),
        evidence.movements_path.to_path_buf(),
        evidence.max_control_frame_bytes,
        evidence.ledger_index_path,
    );
    index.catch_up()?;
    let mut reader = FileCheckedRunReader::open(evidence, None)?;
    payload_index_ready(&mut reader, &index, path, current, writer, source, before_seq.min(index.next_seq))
}

pub(super) fn reader_payload_readiness<R: CheckedRunEvidenceReader + ?Sized>(
    reader: &mut R,
    path: &Path,
    current: &[u8],
    writer: &CheckedRunWriter,
    source: &InterfaceId,
    before_seq: u64,
) -> Result<bool, String> {
    let mut index = ProviderLedgerEvidenceIndex::new(writer.owner.clone(), PathBuf::new(), 1);
    for sequence in 0..before_seq {
        let row = reader
            .admitted_movement(sequence)?
            .ok_or("payload selection admission interval is incomplete")?;
        if row.seq.0 != sequence {
            return Err("payload selection admission sequence changed".into());
        }
        index.note(&row)?;
    }
    payload_index_ready(reader, &index, path, current, writer, source, before_seq)
}

struct PayloadHistoryReader<'a, R: ?Sized>(&'a mut R);

impl<R: CheckedRunEvidenceReader + ?Sized> CheckedRunEvidenceReader for PayloadHistoryReader<'_, R> {
    fn max_payload_bytes(&self) -> u64 { self.0.max_payload_bytes() }
    fn source_context_snapshot(&mut self, record: &RunRecord, config: &RunAuthoringConfig, source: u64) -> Result<(serde_json::Value, Vec<(ArtifactRef, u64)>), String> {
        self.0.source_context_snapshot(record, config, source)
    }
    fn pending_output_payload(&mut self, provider: &InterfaceId, reference: &ArtifactRef, relative: &Path) -> Result<Vec<u8>, String> {
        self.0.pending_output_payload(provider, reference, relative)
    }
    fn movement(&mut self, sequence: u64) -> Result<Option<Movement>, String> { self.0.admitted_movement(sequence) }
    fn visit_admitted_prefix(&mut self, before_seq: u64, visitor: &mut dyn FnMut(&Movement) -> Result<(), String>) -> Result<(), String> {
        self.0.visit_admitted_prefix(before_seq, visitor)
    }
    fn artifact(&mut self, reference: &ArtifactRef) -> Result<(InterfaceArtifactManifest, Vec<u8>), String> { self.0.artifact(reference) }
    fn snapshot(&mut self, hash: &ContentHash) -> Result<Vec<u8>, String> { self.0.snapshot(hash) }
    fn concurrent_writer_start(&mut self, path: &Path, record: &RunRecord, before: &[u8], after_seq: u64, writers: &[CheckedRunWriter]) -> Result<u64, String> {
        self.0.concurrent_writer_start(path, record, before, after_seq, writers)
    }
    fn input_payloads_ready(&mut self, path: &Path, current: &[u8], writer: &CheckedRunWriter, source: &InterfaceId, before_seq: u64) -> Result<bool, String> {
        self.0.input_payloads_ready(path, current, writer, source, before_seq)
    }
    fn input_rearm_ready(&mut self, record: &RunRecord, provider: &InterfaceId, before_seq: u64) -> Result<bool, String> {
        self.0.input_rearm_ready(record, provider, before_seq)
    }
}

fn completed_inputs<R: CheckedRunEvidenceReader + ?Sized>(
    reader: &mut R, index: &ProviderLedgerEvidenceIndex, path: &Path, current: &[u8],
    writer: &CheckedRunWriter, before_seq: u64,
    candidates: &BTreeMap<u64, (RuntimeCoordinateKey, IndexedOutboundArtifact)>,
) -> Result<bool, String> {
    if candidates.is_empty() { return Ok(true); }
    let record = RunRecord::from_bytes(current).map_err(|error|error.to_string())?;
    let mut anchor = None;
    for (sequence, (coordinate, input)) in candidates {
        let Some(label) = index.session_provider_inputs.get(&(artifact_ref_key(&input.artifact), record.id.to_string()))
            .and_then(|inputs|inputs.get(sequence)) else { return Ok(false); };
        if reader.artifact(&input.artifact)?.0.label != *label { return Err("payload readiness input changed its exact admitted artifact".into()); }
        let hash = input.run_record_hash.as_deref().and_then(ContentHash::from_hex).ok_or("payload readiness input has no pinned snapshot")?;
        let bytes = reader.snapshot(&hash)?;
        if ContentHash::of_bytes(&bytes) != hash { return Err("payload readiness input snapshot failed integrity".into()); }
        let pinned = RunRecord::from_bytes(&bytes).map_err(|error|error.to_string())?;
        let checkpoint_id = CheckpointId::new(&coordinate.checkpoint);
        let checkpoint = pinned.checkpoint(&checkpoint_id).ok_or("payload readiness input lost its admitted checkpoint")?;
        let declared = validate_declared_provider_writer(&pinned, writer)?;
        let matches = declared.checkpoints.iter().filter(|template| {
            template.role.is_provider_input() && template.interface() == &input.source
                && checkpoint_record_matches_template(checkpoint, template, &pinned.id)
                && indexed_evidence_matches_checkpoint(input, template)
        }).count();
        if matches != 1 || pinned.id != record.id || pinned.worker != record.worker
            || coordinate.interface != writer.owner.as_str() || !pinned.active_checkpoints.contains(&checkpoint_id)
            || checkpoint.epoch.0 != coordinate.epoch
            || !artifact_refs_match_bindings(&input.artifact_binding_refs, &checkpoint.artifact_bindings) {
            return Err("payload readiness input differs from its exact pinned input contract".into());
        }
        if anchor.is_none() { anchor = Some((*sequence, bytes, pinned)); }
    }
    let (source, before, original) = anchor.ok_or("payload readiness lost its input history anchor")?;
    let writers = declared_writer_closure(&original, vec![writer.clone()])?;
    let start = reader.concurrent_writer_start(path, &original, &before, source, &writers)?;
    let mut replay = PayloadHistoryReader(reader);
    let chain = match checked_run_file_chain_with_reader(&mut replay, path, &before, CheckedRunTarget::Snapshot(current),
        start, start.checked_add(1).ok_or("payload readiness history interval overflow")?, Some(before_seq), &writers, &[], None, None) {
        Ok(chain) => chain,
        Err(CheckedRunError::AwaitingResults { .. }) => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    let mut previous = persisted_pending_handoff(&original, &writer.owner)?;
    let mut completed: BTreeMap<u64, PendingHandoff> = BTreeMap::new();
    for transition in &chain {
        validate_checked_owner_transition(transition)?;
        let authored = RunRecord::from_bytes(&transition.after).map_err(|error|error.to_string())?;
        let next = persisted_pending_handoff(&authored, &writer.owner)?;
        if previous == next { continue; }
        let next = next.ok_or("payload readiness history lost its pending handoff")?;
        validate_pending_items_are_active(&authored, &writer.owner, &next)?;
        let publication = previous.as_ref().is_some_and(|prior|context_publication::is_publication_completion(prior, &next));
        if let Some(prior) = previous.as_ref().filter(|prior| !source_withdrawal::follows(prior, &next) && !rejected_final::follows(prior, &next)) {
            if prior.publication_unfinished() && !publication { return Ok(false); }
            if !all_admitted(index, prior, &record, &writer.owner, transition.call.seq.0)? { return Ok(false); }
            for item in &prior.items {
                if replay.artifact(&pending_ref(item)?)?.0.label != item.artifact_label {
                    return Err("payload readiness completed output changed its admitted label".into());
                }
            }
        }
        if candidates.contains_key(&next.source_movement_seq) {
            if transition.writer != *writer { return Err("payload readiness completion changed its provider writer".into()); }
            if completed.get(&next.source_movement_seq).is_some_and(|prior|prior.terminal && !publication
                && !source_settlement::follows_completed_source(prior, &next) && !source_withdrawal::follows(prior, &next) && !rejected_final::follows(prior, &next)) {
                return Err("payload readiness history repeats a completed input".into());
            }
            completed.insert(next.source_movement_seq, next.clone());
        }
        previous = Some(next);
    }
    if previous != persisted_pending_handoff(&record, &writer.owner)? {
        return Err("payload readiness history differs from the selected current handoff".into());
    }
    for source in candidates.keys() {
        let Some(pending) = completed.get(source) else { return Ok(false); };
        if !pending.terminal || pending.publication_unfinished()
            || (pending.deferred_publication && previous.as_ref() == Some(pending))
            || !all_admitted(index, pending, &record, &writer.owner, before_seq)? { return Ok(false); }
        for item in &pending.items {
            if replay.artifact(&pending_ref(item)?)?.0.label != item.artifact_label {
                return Err("payload readiness terminal output changed its admitted label".into());
            }
        }
    }
    Ok(true)
}

fn ready_pending(
    record: &RunRecord,
    provider: &InterfaceId,
) -> Result<Option<PendingHandoff>, String> {
    let Some(pending) = persisted_pending_handoff(record, provider)? else {
        return Ok(None);
    };
    if !pending.terminal || pending.context_publication.is_some() {
        return Ok(None);
    }
    validate_pending_items_are_active(record, provider, &pending)?;
    Ok(Some(pending))
}

fn pending_ref(item: &PendingHandoffItem) -> Result<ArtifactRef, String> {
    let hash = ContentHash::from_hex(&item.artifact_content_hash)
        .filter(|hash| hash.to_hex() == item.artifact_content_hash)
        .ok_or("input rearm pending artifact has no canonical hash")?;
    Ok(ArtifactRef {
        id: item.artifact_id.clone(),
        content: hash,
    })
}

fn all_admitted(
    index: &ProviderLedgerEvidenceIndex,
    pending: &PendingHandoff,
    record: &RunRecord,
    provider: &InterfaceId,
    before_seq: u64,
) -> Result<bool, String> {
    let mut previous = pending.source_movement_seq;
    for item in &pending.items {
        let reference = pending_ref(item)?;
        let Some(sequence) = index.indexed_handoff_seq(
            PendingHandoffLedgerCoordinate {
                interface: provider,
                run: &record.id,
                worker: &record.worker,
                artifact: &reference,
                required_evidence: pending_required_evidence(item)?,
                checkpoint: &item.checkpoint.id,
                epoch: item.checkpoint.epoch,
            },
            before_seq,
        )?
        else {
            return Ok(false);
        };
        if sequence <= previous {
            return Err("input rearm pending admissions are not in source order".into());
        }
        previous = sequence;
    }
    Ok(true)
}

pub(super) fn readiness(
    evidence: &CheckedRunEvidence<'_>,
    record: &RunRecord,
    provider: &InterfaceId,
    before_seq: u64,
) -> Result<bool, String> {
    let Some(pending) = ready_pending(record, provider)? else {
        return Ok(false);
    };
    let mut index = ProviderLedgerEvidenceIndex::with_spotlight(
        provider.clone(),
        evidence.movements_path.to_path_buf(),
        evidence.max_control_frame_bytes,
        evidence.ledger_index_path,
    );
    index.catch_up()?;
    if !all_admitted(&index, &pending, record, provider, before_seq)? {
        return Ok(false);
    }
    for item in &pending.items {
        let (manifest, _) = read_pinned_artifact_with_bounds(
            evidence.blobs_dir,
            evidence.max_manifest_bytes,
            evidence.max_payload_bytes,
            &pending_ref(item)?,
        )?;
        if manifest.label != item.artifact_label {
            return Err("input rearm pending artifact changed its label".into());
        }
    }
    Ok(true)
}

pub(super) fn reader_readiness<R: CheckedRunEvidenceReader + ?Sized>(
    reader: &mut R,
    record: &RunRecord,
    provider: &InterfaceId,
    before_seq: u64,
) -> Result<bool, String> {
    let Some(pending) = ready_pending(record, provider)? else {
        return Ok(false);
    };
    let mut index = ProviderLedgerEvidenceIndex::new(provider.clone(), PathBuf::new(), 1);
    for sequence in pending
        .source_movement_seq
        .checked_add(1)
        .ok_or("input rearm source sequence overflow")?..before_seq
    {
        let row = reader
            .admitted_movement(sequence)?
            .ok_or("input rearm admission interval is incomplete")?;
        if row.seq.0 != sequence {
            return Err("input rearm admission sequence changed".into());
        }
        index.note(&row)?;
    }
    if !all_admitted(&index, &pending, record, provider, before_seq)? {
        return Ok(false);
    }
    for item in &pending.items {
        if reader.artifact(&pending_ref(item)?)?.0.label != item.artifact_label {
            return Err("input rearm pending artifact changed its label".into());
        }
    }
    Ok(true)
}

pub(super) fn validate_rearm_evidence(
    reader: &mut impl CheckedRunEvidenceReader,
    transition: &CheckedRunTransition,
    path: &Path,
) -> Result<(), String> {
    let before = RunRecord::from_bytes(&transition.before).map_err(|error| error.to_string())?;
    if !before
        .interface_state
        .get(&transition.writer.owner)
        .is_some_and(|state| state.contains_key(RUN_AUTHORING_BINDING))
    {
        return Ok(());
    }
    let after = RunRecord::from_bytes(&transition.after).map_err(|error| error.to_string())?;
    let old = topology(&before, &transition.writer.owner)?;
    let next = topology(&after, &transition.writer.owner)?;
    if is_revision(&old, &next) && !active_returns(&before, &replacements(&old, &next)?).is_empty() {
        return static_revision_evidence(reader, path, &transition.before, &transition.writer, &next, transition.call.seq.0).map(|_| ());
    }
    let initial_revision = persisted_pending_handoff(&before, &transition.writer.owner)?.is_none()
        && is_revision(
            &topology(&before, &transition.writer.owner)?,
            &topology(&after, &transition.writer.owner)?,
        );
    if is_payload_revision(&before, &after, &transition.writer.owner)? || initial_revision {
        let declared = topology(&before, &transition.writer.owner)?;
        let source = declared
            .checkpoints
            .iter()
            .find(|template| {
                template.role == RunAuthoringCheckpointRole::Stable && template.include_context
            })
            .ok_or("payload selection has no stable input")?
            .interface();
        if !reader.input_payloads_ready(
            path,
            &transition.before,
            &transition.writer,
            source,
            transition.call.seq.0,
        )? {
            return Err(
                "payload selection has an unconsumed input or incomplete terminal output".into(),
            );
        }
        return Ok(());
    }
    if is_rearm(&before, &after, &transition.writer.owner)?
        && !reader.input_rearm_ready(&before, &transition.writer.owner, transition.call.seq.0)?
    {
        return Err(
            "input rearm has no complete admitted terminal output batch before its write".into(),
        );
    }
    Ok(())
}

pub fn prepare_provider_input_payloads(
    path: PathBuf,
    current: &[u8],
    writer: &CheckedRunWriter,
    source: InterfaceId,
    selected: Vec<ArtifactRef>,
) -> Result<CheckedRunEdit, String> {
    let record = RunRecord::from_bytes(current).map_err(|error| error.to_string())?;
    let value = payload_value(&record, current, writer, &source, &selected)?;
    let generation = value["interface_state"][writer.owner.as_str()]["authoring_generation"]
        .as_u64()
        .ok_or("payload generation absent")?;
    CheckedRunEdit::prepare(
        path,
        current,
        value,
        format!("provider-run-authoring:{}:{generation}", record.id),
    )
}

fn payload_value(
    record: &RunRecord,
    bytes: &[u8],
    writer: &CheckedRunWriter,
    source: &InterfaceId,
    selected: &[ArtifactRef],
) -> Result<serde_json::Value, String> {
    let declared = validate_declared_provider_writer(record, writer)?;
    let config = declared.config(&profile(writer));
    validate_bound_run_authoring_topology(record, &config, None)?;
    payload_phase(record, &writer.owner, source)?;
    validate_payload_artifact_selection(selected)?;
    let original_context = provider_context_bindings(record, &writer.owner)?;
    let old_ids = declared
        .payload_artifact_bindings
        .iter()
        .map(|reference| &reference.id)
        .collect::<BTreeSet<_>>();
    for reference in selected {
        if declared
            .payload_artifact_bindings
            .iter()
            .any(|old| old.id == reference.id && old != reference)
            || original_context
                .iter()
                .any(|binding| binding.id == reference.id && !old_ids.contains(&binding.id))
        {
            return Err(
                "payload selection changed immutable content or collides with semantic context"
                    .into(),
            );
        }
    }
    let mut context = original_context
        .into_iter()
        .filter(|binding| !old_ids.contains(&binding.id))
        .collect::<Vec<_>>();
    context.extend(selected.iter().map(|reference| ArtifactBinding {
        id: reference.id.clone(),
        access: Default::default(),
    }));
    let generation = optional_u64(
        record
            .interface_state
            .get(&writer.owner)
            .ok_or("provider state absent")?,
        "authoring_generation",
    )?
    .checked_add(1)
    .ok_or("payload selection generation overflow")?;
    let mut value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
    let mut next = declared.clone();
    for template in &mut next.checkpoints {
        if template.role == RunAuthoringCheckpointRole::Stable && template.include_context {
            template
                .static_artifact_bindings
                .retain(|id| !old_ids.contains(id));
        }
        if template.role == RunAuthoringCheckpointRole::ProviderToolOutput {
            template
                .static_artifact_bindings
                .retain(|id| !old_ids.contains(id));
            for reference in selected {
                append_artifact(&mut template.static_artifact_bindings, reference.id.clone());
            }
        }
    }
    let pending = persisted_pending_handoff(record, &writer.owner)?;
    for template in &declared.checkpoints {
        let stable =
            template.role == RunAuthoringCheckpointRole::Stable && template.include_context;
        let authoring = config.writer_run.is_none()
            && matches!(
                template.role,
                RunAuthoringCheckpointRole::AuthoringCall
                    | RunAuthoringCheckpointRole::AuthoringResult
            );
        let future_tool = template.role == RunAuthoringCheckpointRole::ProviderToolOutput
            && pending.as_ref().is_none_or(|handoff| {
                retains_future_output(template)
                    && !handoff.items.iter().any(|item| pending_output_matches_template(item, template))
            });
        if !stable && !authoring && !future_tool {
            continue;
        }
        let rows = record
            .checkpoints
            .iter()
            .filter(|cp| {
                record.active_checkpoints.contains(&cp.id)
                    && checkpoint_record_matches_template(cp, template, &record.id)
            })
            .collect::<Vec<_>>();
        let [row] = rows.as_slice() else {
            return Err("payload selection has no unique declared checkpoint".into());
        };
        let id = format!(
            "cp_input_payload_{}_{}_{}",
            record.id, generation, template.name
        );
        if record.checkpoint(&CheckpointId::new(&id)).is_some() {
            return Err("payload selection reused a checkpoint generation".into());
        }
        rename_checkpoint(&mut value, &writer.owner, &row.id, &id, true)?;
        if stable || future_tool {
            let bindings = if stable {
                context.clone()
            } else {
                let mut bindings = row
                    .artifact_bindings
                    .iter()
                    .filter(|binding| !old_ids.contains(&binding.id))
                    .cloned()
                    .collect::<Vec<_>>();
                bindings.extend(selected.iter().map(|reference| ArtifactBinding {
                    id: reference.id.clone(),
                    access: Default::default(),
                }));
                bindings
            };
            let updated = value["checkpoints"]
                .as_array_mut()
                .ok_or("payload checkpoints absent")?
                .iter_mut()
                .find(|row| row["id"] == id)
                .ok_or("payload checkpoint absent")?;
            updated["artifact_bindings"] =
                serde_json::to_value(bindings).map_err(|error| error.to_string())?;
        }
    }
    // This list classifies bound bytes; the individual checkpoint supplies the read
    // grant. Retain old classifications only where an unchanged row still binds them.
    // New stable inputs and future tool outputs carry exactly `selected`.
    let mut classified = selected.to_vec();
    let live = value["active_checkpoints"]
        .as_array()
        .ok_or("payload active checkpoints absent")?;
    for reference in &declared.payload_artifact_bindings {
        if !classified.contains(reference)
            && value["checkpoints"]
                .as_array()
                .ok_or("payload checkpoints absent")?
                .iter()
                .any(|row| {
                    live.contains(&row["id"])
                        && row["artifact_bindings"].as_array().is_some_and(|bindings| {
                            bindings
                                .iter()
                                .any(|binding| binding["id"] == reference.id.as_str())
                        })
                })
        {
            classified.push(reference.clone());
        }
    }
    next.payload_artifact_bindings = classified;
    value["interface_state"][writer.owner.as_str()][RUN_AUTHORING_BINDING] =
        serde_json::to_value(next).map_err(|error| error.to_string())?;
    value["interface_state"][writer.owner.as_str()]["authoring_generation"] =
        serde_json::json!(generation);
    let after =
        RunRecord::from_bytes(&serde_json::to_vec(&value).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
    let next = topology(&after, &writer.owner)?;
    validate_bound_run_authoring_topology(&after, &next.config(&profile(writer)), None)?;
    Ok(value)
}

pub(super) fn is_payload_revision(
    before: &RunRecord,
    after: &RunRecord,
    provider: &InterfaceId,
) -> Result<bool, String> {
    let old = topology(before, provider)?;
    let new = topology(after, provider)?;
    if old.payload_artifact_bindings != new.payload_artifact_bindings {
        return Ok(true);
    }
    if old.payload_artifact_bindings.is_empty() {
        return Ok(false);
    }
    let old_ids = provider_context_bindings(before, provider)?
        .into_iter()
        .map(|binding| binding.id)
        .collect::<Vec<_>>();
    let new_ids = provider_context_bindings(after, provider)?
        .into_iter()
        .map(|binding| binding.id)
        .collect::<Vec<_>>();
    Ok(old
        .payload_artifact_bindings
        .iter()
        .any(|reference| old_ids.contains(&reference.id) != new_ids.contains(&reference.id)))
}

pub(super) fn validate_payload_revision(
    before: &[u8],
    after: &[u8],
    writer: &CheckedRunWriter,
) -> Result<(), String> {
    let original = RunRecord::from_bytes(before).map_err(|error| error.to_string())?;
    let current = RunRecord::from_bytes(after).map_err(|error| error.to_string())?;
    let declared = topology(&current, &writer.owner)?;
    let context = provider_context_bindings(&current, &writer.owner)?;
    let selected = declared
        .payload_artifact_bindings
        .iter()
        .filter(|reference| context.iter().any(|binding| binding.id == reference.id))
        .cloned()
        .collect::<Vec<_>>();
    let source = declared
        .checkpoints
        .iter()
        .find(|template| {
            template.role == RunAuthoringCheckpointRole::Stable && template.include_context
        })
        .ok_or("payload selection lost its stable input")?
        .interface();
    let expected = payload_value(&original, before, writer, source, &selected)?;
    let actual: serde_json::Value =
        serde_json::from_slice(after).map_err(|error| error.to_string())?;
    if actual != expected {
        return Err("payload selection changed protected state, pending work, grants or checkpoint identity".into());
    }
    Ok(())
}

pub fn prepare_provider_input_revision(
    path: PathBuf,
    current: &[u8],
    writer: &CheckedRunWriter,
    desired_inputs: Vec<serde_json::Value>,
) -> Result<ProviderInputRevision, String> {
    prepare_input_revision(path, current, writer, desired_inputs, None)
}

fn static_revision_evidence(
    reader: &mut impl CheckedRunEvidenceReader, path: &Path, current: &[u8], writer: &CheckedRunWriter,
    next: &BoundRunAuthoringTopology, before_seq: u64,
) -> Result<FailedSourceStaticContext, String> {
    let record = RunRecord::from_bytes(current).map_err(|error| error.to_string())?;
    static_revision_checkpoint(&record, writer, next)?;
    let source = persisted_pending_handoff(&record, &writer.owner)?.ok_or("failed source absent")?.source_movement_seq;
    if before_seq <= source { return Err("static correction has no later terminal evidence boundary".into()); }
    let mut index = ProviderLedgerEvidenceIndex::new(writer.owner.clone(), PathBuf::new(), 1);
    reader.visit_admitted_prefix(before_seq, &mut |row| index.note(row))?;
    validate_static_revision_evidence(reader, &index, path, current, writer, next, before_seq)
}

fn prepare_input_revision(
    path: PathBuf, current: &[u8], writer: &CheckedRunWriter,
    desired_inputs: Vec<serde_json::Value>, evidence: Option<(&CheckedRunEvidence<'_>, u64)>,
) -> Result<ProviderInputRevision, String> {
    let record = RunRecord::from_bytes(current).map_err(|error| error.to_string())?;
    let before = validate_declared_provider_writer(&record, writer)?;
    let config = before.config(&profile(writer));
    validate_bound_run_authoring_topology(&record, &config, None)?;
    let desired = desired_inputs
        .into_iter()
        .map(serde_json::from_value)
        .collect::<Result<Vec<RunAuthoringCheckpoint>, _>>()
        .map_err(|error| error.to_string())?;
    let old = before
        .checkpoints
        .iter()
        .filter(|template| template.role == RunAuthoringCheckpointRole::OwnerInput)
        .collect::<Vec<_>>();
    if desired.len() != old.len()
        || desired.iter().any(|template| {
            old.iter()
                .filter(|prior| same_edge(prior, template))
                .count()
                != 1
        })
    {
        return Err("input revision must declare exactly the existing owner input edges".into());
    }
    let mut next = before.clone();
    for template in &mut next.checkpoints {
        if template.role == RunAuthoringCheckpointRole::OwnerInput {
            let matches = desired
                .iter()
                .filter(|candidate| same_edge(template, candidate))
                .collect::<Vec<_>>();
            let [replacement] = matches.as_slice() else {
                return Err("input revision repeats an owner input edge".into());
            };
            *template = (*replacement).clone();
        }
    }
    let changed = replacements(&before, &next)?;
    if changed.is_empty() {
        return Ok(ProviderInputRevision::Unchanged);
    }
    next.config(&profile(writer))
        .validate(&writer.owner, &record.id)?;
    let pending = persisted_pending_handoff(&record, &writer.owner)?;
    let blocked = active_returns(&record, &changed);
    if pending
            .as_ref()
            .is_some_and(|pending| pending.context_publication.is_some())
    {
        return Ok(ProviderInputRevision::Deferred {
            checkpoints: blocked,
        });
    }
    if !blocked.is_empty() {
        let Some((evidence, before_seq)) = evidence else {
            return Ok(ProviderInputRevision::Deferred { checkpoints: blocked });
        };
        if !static_revision_candidate(&record, writer, &next)? {
            return Ok(ProviderInputRevision::Deferred { checkpoints: blocked });
        }
        let mut reader = FileCheckedRunReader::open(evidence, None)?;
        static_revision_evidence(&mut reader, &path, current, writer, &next, before_seq)?;
    }
    if pending.is_none()
        && optional_u64(
            record
                .interface_state
                .get(&writer.owner)
                .ok_or("provider state absent")?,
            "context_generation",
        )? != 0
    {
        return Err("input revision has no initial or retained provider context".into());
    }
    let mut value: serde_json::Value =
        serde_json::from_slice(current).map_err(|error| error.to_string())?;
    value["interface_state"][writer.owner.as_str()][RUN_AUTHORING_BINDING] =
        serde_json::to_value(next).map_err(|error| error.to_string())?;
    let generation = optional_u64(
        record
            .interface_state
            .get(&writer.owner)
            .ok_or("provider state absent")?,
        "authoring_generation",
    )?
    .checked_add(1)
    .ok_or("input revision authoring generation overflow")?;
    value["interface_state"][writer.owner.as_str()]["authoring_generation"] =
        serde_json::json!(generation);
    if config.writer_run.is_none() {
        for role in [
            RunAuthoringCheckpointRole::AuthoringCall,
            RunAuthoringCheckpointRole::AuthoringResult,
        ] {
            let old = active_checkpoint_for_role(&record, &config, role)?;
            let template = config.checkpoint_for_role(role)?;
            let id = format!(
                "cp_input_revision_{}_{}_{}",
                record.id, generation, template.name
            );
            if record.checkpoint(&CheckpointId::new(&id)).is_some() {
                return Err("input revision reused an existing checkpoint".into());
            }
            rename_checkpoint(&mut value, &writer.owner, &old.id, &id, true)?;
        }
    }
    let edit = CheckedRunEdit::prepare(
        path,
        current,
        value,
        format!("provider-run-authoring:{}:{generation}", record.id),
    )?;
    validate(current, edit.content.as_bytes(), writer)?;
    Ok(ProviderInputRevision::Prepared { edit })
}

pub fn prepare_provider_input_rearm(
    path: PathBuf,
    current: &[u8],
    writer: &CheckedRunWriter,
    source: InterfaceId,
) -> Result<CheckedRunEdit, String> {
    let record = RunRecord::from_bytes(current).map_err(|error| error.to_string())?;
    let declared = validate_declared_provider_writer(&record, writer)?;
    let config = declared.config(&profile(writer));
    validate_bound_run_authoring_topology(&record, &config, None)?;
    let pending = persisted_pending_handoff(&record, &writer.owner)?
        .ok_or("input rearm has no settled handoff")?;
    if !pending.terminal || pending.context_publication.is_some() {
        return Err("input rearm must preserve an already settled context publication".into());
    }
    let candidates = declared
        .checkpoints
        .iter()
        .filter(|template| {
            template.role == RunAuthoringCheckpointRole::Stable
                && template.interface() == &source
                && template.include_context
        })
        .collect::<Vec<_>>();
    let [input] = candidates.as_slice() else {
        return Err("input rearm requires one exact stable input".into());
    };
    let generation = optional_u64(
        record
            .interface_state
            .get(&writer.owner)
            .ok_or("provider state absent")?,
        "authoring_generation",
    )?
    .checked_add(1)
    .ok_or("input rearm generation overflow")?;
    let mut value: serde_json::Value =
        serde_json::from_slice(current).map_err(|error| error.to_string())?;
    let mut selected = vec![*input];
    if config.writer_run.is_none() {
        for role in [
            RunAuthoringCheckpointRole::AuthoringCall,
            RunAuthoringCheckpointRole::AuthoringResult,
        ] {
            selected.push(config.checkpoint_for_role(role)?);
        }
    }
    for template in selected {
        let candidates = record
            .checkpoints
            .iter()
            .filter(|cp| {
                record.active_checkpoints.contains(&cp.id)
                    && checkpoint_record_matches_template(cp, template, &record.id)
            })
            .collect::<Vec<_>>();
        let [old] = candidates.as_slice() else {
            return Err("input rearm checkpoint is absent or ambiguous".into());
        };
        let id = format!(
            "cp_input_rearm_{}_{}_{}",
            record.id, generation, template.name
        );
        if record.checkpoint(&CheckpointId::new(&id)).is_some() {
            return Err("input rearm reused an existing checkpoint".into());
        }
        rename_checkpoint(&mut value, &writer.owner, &old.id, &id, true)?;
    }
    value["interface_state"][writer.owner.as_str()]["authoring_generation"] =
        serde_json::json!(generation);
    let edit = CheckedRunEdit::prepare(
        path,
        current,
        value,
        format!("provider-run-authoring:{}:{generation}", record.id),
    )?;
    validate(current, edit.content.as_bytes(), writer)?;
    Ok(edit)
}

pub(super) fn rename_checkpoint(
    value: &mut serde_json::Value,
    provider: &InterfaceId,
    old: &CheckpointId,
    new: &str,
    reset_epoch: bool,
) -> Result<(), String> {
    for row in value["checkpoints"]
        .as_array_mut()
        .ok_or("run checkpoints absent")?
    {
        if row["id"].as_str() == Some(old.as_str()) {
            row["id"] = serde_json::json!(new);
            if reset_epoch {
                row["epoch"] = serde_json::json!(0);
            }
        }
    }
    for entry in value["active_checkpoints"]
        .as_array_mut()
        .ok_or("run active checkpoints absent")?
    {
        if entry.as_str() == Some(old.as_str()) {
            *entry = serde_json::json!(new);
        }
    }
    for entry in value["interface_state"][provider.as_str()][FRONTIER_CHECKPOINT_IDS]
        .as_array_mut()
        .ok_or("provider frontier absent")?
    {
        if entry.as_str() == Some(old.as_str()) {
            *entry = serde_json::json!(new);
        }
    }
    Ok(())
}

pub(super) fn is_rearm(
    before: &RunRecord,
    after: &RunRecord,
    provider: &InterfaceId,
) -> Result<bool, String> {
    let prior = before
        .interface_state
        .get(provider)
        .ok_or("provider state absent")?;
    let next = after
        .interface_state
        .get(provider)
        .ok_or("provider state absent")?;
    Ok(
        prior.get(RUN_AUTHORING_BINDING) == next.get(RUN_AUTHORING_BINDING)
            && prior.get("pending_handoff").is_some()
            && prior.get("pending_handoff") == next.get("pending_handoff")
            && prior.get("last_authored_artifact") == next.get("last_authored_artifact")
            && prior.get("context_generation") == next.get("context_generation")
            && prior.get("authoring_generation") != next.get("authoring_generation"),
    )
}

pub(super) fn validate(
    before: &[u8],
    after: &[u8],
    writer: &CheckedRunWriter,
) -> Result<(), String> {
    let original = RunRecord::from_bytes(before).map_err(|error| error.to_string())?;
    let current = RunRecord::from_bytes(after).map_err(|error| error.to_string())?;
    let old = validate_declared_provider_writer(&original, writer)?;
    let next = validate_declared_provider_writer(&current, writer)?;
    let changed = replacements(&old, &next)?;
    if !active_returns(&original, &changed).is_empty()
        || !active_returns(&current, &changed).is_empty()
    {
        let prior = static_revision_checkpoint(&original, writer, &next)?;
        if current.checkpoint(&prior.id) != Some(prior)
            || active_returns(&original, &changed) != active_returns(&current, &changed)
        {
            return Err("static input correction changed its materialized checkpoint or rights".into());
        }
    }
    let old_config = old.config(&profile(writer));
    let new_config = next.config(&profile(writer));
    new_config.validate(&writer.owner, &current.id)?;
    validate_bound_run_authoring_topology(&original, &old_config, None)?;
    validate_bound_run_authoring_topology(&current, &new_config, None)?;
    if current_mandatory_role_bindings(&original, &old_config)?
        != current_mandatory_role_bindings(&current, &new_config)?
    {
        return Err("input revision changed the mandatory role closure".into());
    }
    if persisted_pending_handoff(&original, &writer.owner)?
        .is_some_and(|pending| pending.context_publication.is_some())
    {
        return Err("input revision interrupts a partial context publication".into());
    }
    if persisted_pending_handoff(&original, &writer.owner)?.is_none()
        && optional_u64(
            original
                .interface_state
                .get(&writer.owner)
                .ok_or("provider state absent")?,
            "context_generation",
        )? != 0
    {
        return Err("input revision has no initial or retained provider context".into());
    }
    let prior = original
        .interface_state
        .get(&writer.owner)
        .ok_or("provider state absent")?;
    let state = current
        .interface_state
        .get(&writer.owner)
        .ok_or("provider state absent")?;
    if optional_u64(state, "authoring_generation")?
        != optional_u64(prior, "authoring_generation")?
            .checked_add(1)
            .ok_or("input revision generation overflow")?
    {
        return Err("input revision skipped an authoring generation".into());
    }
    let mut old_value: serde_json::Value =
        serde_json::from_slice(before).map_err(|error| error.to_string())?;
    let mut new_value: serde_json::Value =
        serde_json::from_slice(after).map_err(|error| error.to_string())?;
    let mut rearmed = 0;
    for template in old
        .checkpoints
        .iter()
        .filter(|template| template.role == RunAuthoringCheckpointRole::Stable)
    {
        let select = |record: &RunRecord| -> Result<CheckpointId, String> {
            let matches = record
                .checkpoints
                .iter()
                .filter(|cp| {
                    record.active_checkpoints.contains(&cp.id)
                        && checkpoint_record_matches_template(cp, template, &record.id)
                })
                .collect::<Vec<_>>();
            let [checkpoint] = matches.as_slice() else {
                return Err("input rearm lost its unique stable checkpoint".into());
            };
            Ok(checkpoint.id.clone())
        };
        let original_id = select(&original)?;
        let current_id = select(&current)?;
        if original_id == current_id {
            continue;
        }
        if !changed.is_empty()
            || original.checkpoint(&current_id).is_some()
            || current.checkpoint(&original_id).is_some()
            || current
                .checkpoint(&current_id)
                .is_none_or(|cp| cp.epoch.0 != 0)
            || !persisted_pending_handoff(&original, &writer.owner)?
                .is_some_and(|pending| pending.terminal)
        {
            return Err("input rearm changed an unsettled or reused checkpoint generation".into());
        }
        let token = format!("stable_{}", template.name);
        rename_checkpoint(&mut old_value, &writer.owner, &original_id, &token, true)?;
        rename_checkpoint(&mut new_value, &writer.owner, &current_id, &token, true)?;
        rearmed += 1;
    }
    if changed.is_empty() && rearmed != 1 {
        return Err("input rearm must replace exactly one stable checkpoint".into());
    }
    for (value, record, config, is_current) in [
        (&mut old_value, &original, &old_config, false),
        (&mut new_value, &current, &new_config, true),
    ] {
        let state = value["interface_state"][writer.owner.as_str()]
            .as_object_mut()
            .ok_or("provider state malformed")?;
        state.remove(RUN_AUTHORING_BINDING);
        state.remove("authoring_generation");
        if config.writer_run.is_none() {
            for role in [
                RunAuthoringCheckpointRole::AuthoringCall,
                RunAuthoringCheckpointRole::AuthoringResult,
            ] {
                let checkpoint = active_checkpoint_for_role(record, config, role)?;
                if is_current {
                    let prior = active_checkpoint_for_role(&original, &old_config, role)?;
                    if checkpoint.id == prior.id
                        || original.checkpoint(&checkpoint.id).is_some()
                        || checkpoint.epoch.0 != 0
                    {
                        return Err(
                            "input revision did not author a fresh writer checkpoint".into()
                        );
                    }
                }
                let token = match role {
                    RunAuthoringCheckpointRole::AuthoringCall => "call",
                    _ => "result",
                };
                rename_checkpoint(value, &writer.owner, &checkpoint.id, token, true)?;
            }
        }
    }
    if old_value != new_value {
        return Err(
            "input revision changed pending work, context, status or neighboring state".into(),
        );
    }
    Ok(())
}

pub(super) fn is_revision(
    before: &BoundRunAuthoringTopology,
    after: &BoundRunAuthoringTopology,
) -> bool {
    replacements(before, after).is_ok_and(|changes| !changes.is_empty())
}

pub(super) fn checked_revision(transition: &CheckedRunTransition) -> bool {
    let Ok(before) = RunRecord::from_bytes(&transition.before) else {
        return false;
    };
    let Ok(after) = RunRecord::from_bytes(&transition.after) else {
        return false;
    };
    let (Ok(old), Ok(next)) = (
        topology(&before, &transition.writer.owner),
        topology(&after, &transition.writer.owner),
    ) else {
        return false;
    };
    is_revision(&old, &next)
        && validate(&transition.before, &transition.after, &transition.writer).is_ok()
}

pub(super) struct RevisionDelta {
    pub protected: Vec<u8>,
    before: BoundRunAuthoringTopology,
    after: BoundRunAuthoringTopology,
    generation_delta: u64,
}

pub(super) fn revision_delta(
    before: &[u8],
    current: &[u8],
    config: &RunAuthoringConfig,
) -> Result<Option<RevisionDelta>, String> {
    let original = RunRecord::from_bytes(before).map_err(|error| error.to_string())?;
    let actual = RunRecord::from_bytes(current).map_err(|error| error.to_string())?;
    let old = topology(&original, &config.provider_interface)?;
    let next = topology(&actual, &config.provider_interface)?;
    if !is_revision(&old, &next) {
        return Ok(None);
    }
    let old_state = original
        .interface_state
        .get(&config.provider_interface)
        .ok_or("provider state absent")?;
    let new_state = actual
        .interface_state
        .get(&config.provider_interface)
        .ok_or("provider state absent")?;
    let generation_delta = optional_u64(new_state, "authoring_generation")?
        .checked_sub(optional_u64(old_state, "authoring_generation")?)
        .filter(|delta| *delta > 0)
        .ok_or("input revision retry has no forward authoring generation")?;
    let mut protected: serde_json::Value =
        serde_json::from_slice(current).map_err(|error| error.to_string())?;
    if config.writer_run.is_none() {
        for role in [
            RunAuthoringCheckpointRole::AuthoringCall,
            RunAuthoringCheckpointRole::AuthoringResult,
        ] {
            let prior = active_checkpoint_for_role(&original, config, role)?;
            let observed = active_checkpoint_for_role(&actual, config, role)?;
            let mut normalized = observed.clone();
            normalized.id = prior.id.clone();
            normalized.epoch = prior.epoch;
            if observed.epoch.0 != 0
                || normalized != *prior
                || observed.id == prior.id
                || original.checkpoint(&observed.id).is_some()
            {
                return Err("input revision retry changed the writer checkpoint contract".into());
            }
            rename_checkpoint(
                &mut protected,
                &config.provider_interface,
                &observed.id,
                prior.id.as_str(),
                false,
            )?;
            let rows = protected["checkpoints"]
                .as_array_mut()
                .ok_or("retry checkpoints absent")?;
            let row = rows
                .iter_mut()
                .find(|row| row["id"] == prior.id.as_str())
                .ok_or("retry original writer absent")?;
            row["epoch"] = serde_json::json!(prior.epoch.0);
        }
    }
    protected["interface_state"][config.provider_interface.as_str()][RUN_AUTHORING_BINDING] =
        serde_json::to_value(&old).map_err(|error| error.to_string())?;
    protected["interface_state"][config.provider_interface.as_str()]["authoring_generation"] =
        serde_json::json!(optional_u64(old_state, "authoring_generation")?);
    let checked =
        RunRecord::from_bytes(&serde_json::to_vec(&protected).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
    if checked.interface_state.get(&config.provider_interface) != Some(old_state) {
        return Err("input revision retry changed pending work or provider context".into());
    }
    Ok(Some(RevisionDelta {
        protected: JsonFormat::Compact.encode(&protected)?,
        before: old,
        after: next,
        generation_delta,
    }))
}

impl RevisionDelta {
    pub fn restore(
        &self,
        value: &mut serde_json::Value,
        provider: &InterfaceId,
    ) -> Result<u64, String> {
        let state = value["interface_state"][provider.as_str()]
            .as_object_mut()
            .ok_or("provider retry state absent")?;
        let mut planned: BoundRunAuthoringTopology = serde_json::from_value(
            state
                .get(RUN_AUTHORING_BINDING)
                .cloned()
                .ok_or("provider retry topology absent")?,
        )
        .map_err(|error| error.to_string())?;
        for (old, next) in self.before.checkpoints.iter().zip(&self.after.checkpoints) {
            if old == next {
                continue;
            }
            let matches = planned
                .checkpoints
                .iter_mut()
                .filter(|candidate| **candidate == *old)
                .collect::<Vec<_>>();
            let [selected] = matches.try_into().map_err(
                |_: Vec<_>| "provider retry conflicts with the current input definition",
            )?;
            *selected = next.clone();
        }
        state.insert(
            RUN_AUTHORING_BINDING.into(),
            serde_json::to_value(planned).map_err(|error| error.to_string())?,
        );
        let generation = state
            .get("authoring_generation")
            .and_then(serde_json::Value::as_u64)
            .ok_or("provider retry generation absent")?
            .checked_add(self.generation_delta)
            .ok_or("provider retry generation overflow")?;
        state.insert("authoring_generation".into(), serde_json::json!(generation));
        Ok(generation)
    }
}
