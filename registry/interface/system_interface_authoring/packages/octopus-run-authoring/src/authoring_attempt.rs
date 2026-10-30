use super::*;

pub(crate) fn attempt_config(record: &RunRecord, config: &RunAuthoringConfig) -> Result<RunAuthoringConfig, String> {
    config_from_persisted_profile(record, &config.provider_interface, &RunAuthoringProfile {
        file_write_tool: config.file_write_tool.clone(), file_write_label: config.file_write_label.clone(),
        file_write_result_label: config.file_write_result_label.clone(), result_timeout_ms: config.result_timeout_ms,
    })
}

// A definition write can occupy the generation of an already saved response.
// Discover only predecessors of the exact current pending source, through its
// admitted write chain; private files do not authorize an older generation.
pub(crate) fn revision_attempt_generations(
    ctx: &InterfaceLiftContext, session: &ProviderHandoffSession, record: &RunRecord, current: &[u8], config: &RunAuthoringConfig,
) -> Result<BTreeMap<u64, String>, String> {
    let mut generations = BTreeMap::new();
    if !config.checkpoints.iter().any(|checkpoint| checkpoint.role == RunAuthoringCheckpointRole::OwnerInput) {
        return Ok(generations);
    }
    let Some(pending) = persisted_pending_handoff(record, &config.provider_interface)? else { return Ok(generations); };
    let (_, source) = session.index()?.exact_provider_source_outbound(
        &record.id, &record.worker, &config.provider_interface, pending.source_movement_seq,
    )?;
    let hash = source.run_record_hash.as_deref().and_then(ContentHash::from_hex)
        .ok_or("input revision recovery source has no exact snapshot")?;
    let (before, original) = pinned_run_record(ctx, &hash, "input revision recovery source")?;
    let original_config = attempt_config(&original, config)?;
    let path = ctx.root_slot("run_records_root")?.canonicalize().map_err(|error| error.to_string())?
        .join(format!("{}.json", record.id));
    let chain = checked_selected_run_advance(ctx, &path, &before, current, pending.source_movement_seq, &original_config)?;
    for transition in chain.iter().rev().filter(|transition| !transition.placement && transition.writer.owner == config.provider_interface) {
        if !input_revision::checked_revision(transition) { break; }
        let prior = RunRecord::from_bytes(&transition.before).map_err(|error| error.to_string())?;
        generations.insert(optional_u64(prior.interface_state.get(&config.provider_interface)
            .ok_or("revision predecessor has no provider state")?, "authoring_generation")?
            .checked_add(1).ok_or("revision predecessor generation overflow")?,
            ContentHash::of_bytes(&transition.after).to_hex());
    }
    Ok(generations)
}

pub(crate) fn validate_revision_attempt_predecessor(
    ctx: &InterfaceLiftContext, before: &[u8], current: &[u8], source: u64, config: &RunAuthoringConfig,
) -> Result<(), String> {
    let original = RunRecord::from_bytes(before).map_err(|error| error.to_string())?;
    let record = RunRecord::from_bytes(current).map_err(|error| error.to_string())?;
    if original.id != record.id || original.worker != record.worker || original.trace != record.trace {
        return Err("revision predecessor changed its original run identity".into());
    }
    let path = ctx.root_slot("run_records_root")?.canonicalize().map_err(|error| error.to_string())?
        .join(format!("{}.json", record.id));
    let chain = checked_selected_run_advance(ctx, &path, before, current, source, config)?;
    let provider = chain.iter().filter(|transition| !transition.placement && transition.writer.owner == config.provider_interface).collect::<Vec<_>>();
    if provider.is_empty() || provider.iter().any(|transition| !input_revision::checked_revision(transition)) {
        return Err("saved response has no exact input-revision predecessor proof".into());
    }
    input_revision::revision_delta(before, current, config)?
        .ok_or("saved response revision changed its pending work or context")?;
    Ok(())
}

pub(crate) fn persisted_authoring_attempt(
    ctx: &InterfaceLiftContext,
    hand_in_root: &Path,
    record: &RunRecord,
    record_bytes: &[u8],
    config: &RunAuthoringConfig,
    recover_predecessors: bool,
) -> Result<Option<ValidatedAuthoringAttempt>, String> {
    let current_generation = optional_u64(
        record
            .interface_state
            .get(&config.provider_interface)
            .ok_or("provider authoring recovery run has no provider state")?,
        "authoring_generation",
    )?;
    let expected_generation = current_generation
        .checked_add(1)
        .ok_or("provider authoring generation overflowed")?;
    let current_hash = ContentHash::of_bytes(record_bytes).to_hex();
    let calls = private_authoring_calls(ctx, hand_in_root, config)?;
    if calls.is_empty() {
        return Ok(None);
    }
    let predecessors = if recover_predecessors {
        revision_attempt_generations(ctx, &ProviderHandoffSession::new(ctx)?, record, record_bytes, config)?
    } else { BTreeMap::new() };
    let mut attempts = Vec::new();
    for (path, call) in calls {
        let prefix = format!("provider-run-authoring:{}:", record.id);
        if !call.correlation_id.starts_with(&prefix) { continue; }
        let generation = correlation_generation(&record.id, &call.correlation_id)?;
        if generation != expected_generation && !predecessors.contains_key(&generation) { continue; }
        // The admitted definition write itself is not an unfinished response.
        if predecessors.get(&generation) == Some(&call.arguments.expected_after_hash) { continue; }
        let (before_bytes, before_record) = if call.arguments.expected_before_hash == current_hash {
            (record_bytes.to_vec(), record.clone())
        } else {
            let hash = ContentHash::from_hex(&call.arguments.expected_before_hash)
                .ok_or("provider authoring attempt has no canonical preimage")?;
            let before = pinned_run_record(ctx, &hash, "provider authoring attempt")?;
            if before.1.id != record.id
                || before.1.worker != record.worker
                || before.1.trace != record.trace
                || (generation == expected_generation && before.1.interface_state.get(&config.provider_interface)
                    != record.interface_state.get(&config.provider_interface))
            {
                return Err(
                    "provider authoring attempt changed its run or provider-owned state".into(),
                );
            }
            before
        };
        let original_config = attempt_config(&before_record, config)?;
        let attempt = validate_authoring_attempt_source(AuthoringAttemptSourceValidation {
            ctx,
            record: &before_record,
            record_bytes: &before_bytes,
            config: &original_config,
            call,
            manifest_path: path,
        })?;
        if generation != expected_generation {
            validate_revision_attempt_predecessor(ctx, &before_bytes, record_bytes,
                attempt.provider_source_movement_seq, &original_config)?;
        }
        attempts.push((attempt, before_bytes));
    }
    if attempts.len() <= 1 {
        return Ok(attempts.pop().map(|(attempt, _)| attempt));
    }
    let mut ordered = attempts
        .into_iter()
        .map(|(attempt, before)| {
            let rejected = authoring_retry::rejected_write(ctx, config, &attempt.authored)?;
            Ok((attempt, before, rejected))
        })
        .collect::<Result<Vec<_>, String>>()?;
    if ordered
        .iter()
        .filter(|(_, _, rejection)| rejection.is_none())
        .count()
        > 1
    {
        return Err("provider authoring has competing unaccounted generation attempts".into());
    }
    ordered.sort_by_key(|(_, _, rejected)| {
        rejected
            .as_ref()
            .map_or(u64::MAX, |rejected| rejected.result_seq)
    });
    for pair in ordered.windows(2) {
        let (previous, before, rejection) = &pair[0];
        let (next, current, _) = &pair[1];
        if rejection.is_none() {
            return Err("provider authoring retry precedes its actual rejection".into());
        }
        let path = ctx
            .root_slot("run_records_root")?
            .canonicalize()
            .map_err(|error| error.to_string())?
            .join(format!("{}.json", record.id));
        let chain = checked_selected_run_advance(
            ctx,
            &path,
            before,
            current,
            previous.provider_source_movement_seq,
            config,
        )?;
        if chain.is_empty()
            || chain
                .iter()
                .any(|transition| !transition.placement && transition.writer.owner == config.provider_interface && !input_revision::checked_revision(transition))
        {
            return Err("provider authoring retry changed its original intent or independent write evidence".into());
        }
        authoring_retry::validate_rebased_authored_run(before, current, &previous.authored, &next.authored, config)?;
    }
    Ok(ordered.pop().map(|(attempt, _, _)| attempt))
}

pub(crate) fn private_authoring_calls(
    ctx: &InterfaceLiftContext,
    hand_in_root: &Path,
    config: &RunAuthoringConfig,
) -> Result<Vec<(PathBuf, FileWriteCallEnvelope)>, String> {
    let entries = match std::fs::read_dir(hand_in_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(format!(
                "provider authoring private outbox '{}' cannot be read: {error}",
                hand_in_root.display()
            ));
        }
    };
    let mut paths = entries
        .map(|entry| {
            entry.map(|entry| entry.path()).map_err(|error| {
                format!(
                    "provider authoring private outbox '{}' contains an unreadable entry: {error}",
                    hand_in_root.display()
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    paths.sort();
    let limits = ctx.required_host_io_limits()?;
    let mut calls = Vec::new();
    for path in paths {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            return Err(format!(
                "provider authoring private outbox '{}' contains a non-UTF8 entry",
                hand_in_root.display()
            ));
        };
        if !name.ends_with(".artifact.json") {
            continue;
        }
        let artifact = InterfaceArtifactFile::new(path.clone()).map_err(|error| {
            format!(
                "provider authoring private artifact '{}' is invalid: {error}",
                path.display()
            )
        })?;
        let snapshot = artifact
            .read_bounded(limits.max_manifest_bytes, limits.max_payload_bytes)
            .map_err(|error| {
                format!(
                    "provider authoring private artifact '{}' cannot be read: {error}",
                    path.display()
                )
            })?;
        if snapshot.manifest.label != config.file_write_label {
            continue;
        }
        let expected_prefix = format!("provider-run-authoring:{}:", config.run.as_str());
        let Some(call) = decode_owned_run_authoring_call(&snapshot.payload_bytes, &expected_prefix)
            .map_err(|error| {
                format!(
                    "provider authoring private call '{}' is malformed: {error}",
                    path.display()
                )
            })?
        else {
            continue;
        };
        calls.push((path, call));
    }
    Ok(calls)
}

pub(crate) fn decode_owned_run_authoring_call(
    payload: &[u8],
    expected_prefix: &str,
) -> Result<Option<FileWriteCallEnvelope>, serde_json::Error> {
    let value: serde_json::Value = match serde_json::from_slice(payload) {
        Ok(value) => value,
        Err(_) => return Ok(None),
    };
    if !value
        .get("correlation_id")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|correlation_id| correlation_id.starts_with(expected_prefix))
    {
        return Ok(None);
    }
    serde_json::from_value(value).map(Some)
}

pub(crate) fn authoring_success_evidence(
    ctx: &InterfaceLiftContext,
    session: &ProviderHandoffSession,
    config: &RunAuthoringConfig,
    authored: &AuthoredRun,
    after_seq: Option<u64>,
) -> Result<Option<u64>, String> {
    if config.writer_run.is_some() { return writer_run::success_evidence(ctx, session, config, authored, after_seq); }
    let candidate = RunRecord::from_bytes(authored.content.as_bytes()).map_err(|error| {
        format!(
            "provider recovered authoring candidate for run '{}' is malformed: {error}",
            config.run.as_str()
        )
    })?;
    let checkpoint = active_checkpoint_for_role(
        &candidate,
        config,
        RunAuthoringCheckpointRole::AuthoringResult,
    )?;
    if checkpoint.id != authored.authoring_result_checkpoint {
        return Err(format!(
            "provider recovered authoring candidate for run '{}' changed its result checkpoint",
            config.run.as_str()
        ));
    }
    let coordinate = RuntimeCoordinateKey::new(
        &candidate.id,
        &candidate.worker,
        &config.provider_interface,
        &checkpoint.id,
        checkpoint.epoch.0,
    );
    let Some(evidence) = session.index()?.unique_outbound_artifact(
        &coordinate,
        &authored.authoring_result_interface,
        after_seq,
    )?
    else {
        return Ok(None);
    };
    let expected = config.checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringResult)?;
    if !indexed_evidence_matches_checkpoint(&evidence, expected)
        || !artifact_refs_match_bindings(
            &evidence.artifact_binding_refs,
            &checkpoint.artifact_bindings,
        )
    {
        return Err(format!(
            "provider recovered authoring result for run '{}' differs from the authored checkpoint",
            config.run.as_str()
        ));
    }
    if evidence.run_record_hash.as_deref() != Some(&authored.after_hash) {
        if !config.checkpoints.iter().any(|cp| cp.owner_authoring.is_some()) {
            return Err("provider authoring result changed its prospective snapshot".into());
        }
        let hash = evidence.run_record_hash.as_deref().and_then(ContentHash::from_hex)
            .ok_or("provider authoring result has no pinned snapshot")?;
        let (pinned, _) = pinned_run_record(ctx, &hash, "provider authoring result")?;
        validate_unchanged_admitted_checkpoint(authored.content.as_bytes(), &pinned, &checkpoint.id)?;
    }
    let (manifest, payload) = read_pinned_artifact(ctx, &evidence.artifact)?;
    if manifest.label != config.file_write_result_label {
        return Err(format!(
            "provider recovered authoring result checkpoint for run '{}' delivered unexpected artifact '{}'",
            config.run.as_str(),
            manifest.label
        ));
    }
    let result: FileWriteResultEnvelope = serde_json::from_slice(&payload).map_err(|error| {
        format!(
            "provider recovered authoring result for run '{}' is malformed: {error}",
            config.run.as_str()
        )
    })?;
    if result.correlation_id.as_deref() != Some(&authored.correlation_id) {
        return Err(format!(
            "provider recovered authoring result for run '{}' names the wrong correlation",
            config.run.as_str()
        ));
    }
    let run_path = ctx
        .root_slot("run_records_root")?
        .canonicalize()
        .map_err(|error| format!("provider run-record root cannot be canonicalized: {error}"))?
        .join(format!("{}.json", config.run.as_str()));
    validate_write_result_evidence(config, &run_path, result, authored)?;
    Ok(Some(evidence.seq))
}

pub(crate) fn completed_authoring_failure_duplicate(
    ctx: &InterfaceLiftContext,
    session: &ProviderHandoffSession,
    record: &RunRecord,
    record_bytes: &[u8],
    config: &RunAuthoringConfig,
    failure: &InterfaceFailureEnvelope,
    failure_seq: u64,
) -> Result<bool, String> {
    let authoring_result =
        config.checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringResult)?;
    let hand_in_root = ctx
        .root_slot("interface_hand_in_root")?
        .join(config.authoring_source_interface.as_str());
    let expected_path = ctx
        .root_slot("run_records_root")?
        .canonicalize()
        .map_err(|error| format!("provider run-record root cannot be canonicalized: {error}"))?
        .join(format!("{}.json", record.id.as_str()));
    let current_generation = optional_u64(
        record
            .interface_state
            .get(&config.provider_interface)
            .ok_or_else(|| "provider completed authoring run has no provider state".to_string())?,
        "authoring_generation",
    )?;
    let current_hash = ContentHash::of_bytes(record_bytes).to_hex();
    let mut matched = None;
    for (_, call) in private_authoring_calls(ctx, &hand_in_root, config)? {
        let prefix = format!("provider-run-authoring:{}:", record.id.as_str());
        if !call.correlation_id.starts_with(&prefix)
            || correlation_generation(&record.id, &call.correlation_id)? != current_generation
            || call.tool != config.file_write_tool
            || call.arguments.path != expected_path
            || call.arguments.expected_after_hash != current_hash
        {
            continue;
        }
        let canonical_before =
            ContentHash::from_hex(&call.arguments.expected_before_hash).map(|hash| hash.to_hex());
        if call.arguments.patch.is_empty()
            || canonical_before.as_deref() != Some(call.arguments.expected_before_hash.as_str())
        {
            return Err(
                "provider completed authoring attempt carries malformed patch evidence".to_string(),
            );
        }
        let pending = pending_handoff(record, &config.provider_interface)?
            .ok_or_else(|| "provider completed authoring run has no pending handoff".to_string())?;
        if pending.items.is_empty()
            || pending.source_movement_seq >= failure.source_movement_seq
            || failure.source_movement_seq >= failure_seq
        {
            continue;
        }
        validate_pending_items_are_active(record, &config.provider_interface, &pending)?;
        validate_bound_run_authoring_topology(record, config, None)?;
        let authoring_result_checkpoint = active_checkpoint_for_role(
            record,
            config,
            RunAuthoringCheckpointRole::AuthoringResult,
        )?
        .id
        .clone();
        let authored = AuthoredRun {
            json_format: call.arguments.json_format,
            after_hash: current_hash.clone(),
            before_hash: call.arguments.expected_before_hash,
            content: String::from_utf8(record_bytes.to_vec()).map_err(|error| {
                format!("provider completed authored run is not UTF-8: {error}")
            })?,
            patch: call.arguments.patch,
            correlation_id: call.correlation_id,
            authoring_result_checkpoint,
            authoring_result_interface: authoring_result.interface().clone(),
            pending_handoff: pending,
        };
        if matched.replace(authored).is_some() {
            return Err(format!(
                "provider completed authoring run '{}' has repeated private attempts",
                record.id.as_str()
            ));
        }
    }
    let Some(authored) = matched else {
        return Ok(false);
    };
    let Some(result_seq) = authoring_success_evidence(ctx, session, config, &authored, None)?
    else {
        return Ok(false);
    };
    if result_seq >= failure_seq {
        return Err(format!(
            "provider late authoring failure movement {failure_seq} precedes its completed result {result_seq}"
        ));
    }
    Ok(true)
}

pub(crate) fn recover_source_session_context(
    ctx: &InterfaceLiftContext,
    session: &ProviderHandoffSession,
    record: &RunRecord,
    config: &RunAuthoringConfig,
    source_movement_seq: u64,
    context_output_labels: &BTreeSet<String>,
    session_snapshot_labels: &BTreeSet<String>,
) -> Result<(ResolvedSessionContext, Option<SessionSnapshotHead>), String> {
    let (context, head, _) = recover_source_session_context_from_cache(
        &ctx.blobs_dir, ctx.required_host_io_limits()?, session, record, config, source_movement_seq, context_output_labels, session_snapshot_labels,
    )?;
    Ok((context, head))
}

pub(crate) fn recover_source_session_context_from_cache(
    blobs_dir: &Path,
    limits: InterfaceHostIoLimits,
    session: &ProviderHandoffSession,
    record: &RunRecord,
    config: &RunAuthoringConfig,
    source_movement_seq: u64,
    context_output_labels: &BTreeSet<String>,
    session_snapshot_labels: &BTreeSet<String>,
) -> Result<(ResolvedSessionContext, Option<SessionSnapshotHead>, Vec<ArtifactId>), String> {
    recover_source_session_context_with_static_correction(
        blobs_dir, limits, session, record, config, source_movement_seq,
        context_output_labels, session_snapshot_labels, None,
    )
}

pub(crate) fn recover_source_session_context_with_static_correction(
    blobs_dir: &Path,
    limits: InterfaceHostIoLimits,
    session: &ProviderHandoffSession,
    record: &RunRecord,
    config: &RunAuthoringConfig,
    source_movement_seq: u64,
    context_output_labels: &BTreeSet<String>,
    session_snapshot_labels: &BTreeSet<String>,
    correction: Option<&input_revision::FailedSourceStaticContext>,
) -> Result<(ResolvedSessionContext, Option<SessionSnapshotHead>, Vec<ArtifactId>), String> {
    let (coordinate, source) = session.index()?.exact_provider_source_outbound(
        &record.id,
        &record.worker,
        &config.provider_interface,
        source_movement_seq,
    )?;
    let hash = source.run_record_hash.as_deref().and_then(ContentHash::from_hex).ok_or("source context has no pinned run record")?;
    let (_, pinned) = pinned_run_record_from_cache(blobs_dir, limits, &hash, "provider source context")?;
    if pinned.id != record.id || pinned.worker != record.worker {
        return Err(format!(
            "provider source movement {source_movement_seq} pinned a different run identity"
        ));
    }
    let checkpoint_id = CheckpointId::new(&coordinate.checkpoint);
    let checkpoint = pinned.checkpoint(&checkpoint_id).ok_or_else(|| {
        format!(
            "provider source movement {source_movement_seq} names checkpoint '{}' absent from its pinned run record",
            coordinate.checkpoint
        )
    })?;
    if !pinned.active_checkpoints.contains(&checkpoint_id) || checkpoint.epoch.0 != coordinate.epoch
    {
        return Err(format!(
            "provider source movement {source_movement_seq} does not name an active pinned checkpoint epoch"
        ));
    }
    let source_config = config_from_persisted_profile(&pinned, &config.provider_interface, &RunAuthoringProfile {
        file_write_tool: config.file_write_tool.clone(), file_write_label: config.file_write_label.clone(),
        file_write_result_label: config.file_write_result_label.clone(), result_timeout_ms: config.result_timeout_ms,
    })?;
    let matched = source_config
        .checkpoints
        .iter()
        .filter(|template| {
            template.role.is_provider_input()
                && template.include_context
                && source.source == *template.interface()
                && checkpoint_record_matches_template(checkpoint, template, &pinned.id)
        })
        .collect::<Vec<_>>();
    if matched.len() != 1 {
        return Err(format!(
            "provider source movement {source_movement_seq} matches {} context-bearing input checkpoints", matched.len()
        ));
    }
    if !checkpoint
        .outbound_interfaces()
        .eq(source.outbound_interface_bindings.iter())
        || source.checkpoint_binding_policy.as_ref()
            != Some(&CheckpointBindingPolicySnapshot {
                inbound: checkpoint.interface_binding.clone().ok_or_else(|| {
                    format!(
                        "provider source movement {source_movement_seq} pinned checkpoint has no inbound binding"
                    )
                })?,
                outbound: checkpoint.outbound_interface_bindings.clone(),
            })
        || source.required_verified_artifacts != checkpoint.required_verified_artifacts
        || checkpoint
            .artifact_bindings
            .iter()
            .map(|binding| &binding.id)
            .ne(source
                .artifact_binding_refs
                .iter()
                .map(|artifact| &artifact.id))
    {
        return Err(format!(
            "provider source movement {source_movement_seq} differs from its pinned run checkpoint"
        ));
    }
    let mut static_context_artifacts = Vec::new();
    let mut session_context = ResolvedSessionContext {
        provider_interface: config.provider_interface.clone(),
        artifacts: Vec::new(),
    };
    let mut session_snapshot_head = None;
    let bound_provenance = session.index()?.bound_provenance(
        &record.id,
        &config.provider_interface,
        source.artifact_binding_refs.iter(),
    )?;
    let (payloads, mut classified_static) = source_context_payloads_from_cache(blobs_dir, limits, &pinned,
        &source_config, checkpoint, matched[0], &source)?;
    if let Some(correction) = correction {
        classified_static.extend(correction.refs_for(&coordinate, &source, &hash, checkpoint)?
            .iter().map(|reference| reference.id.clone()));
    }
    for bound in &payloads {
        let artifact = &bound.artifact;
        let payload = bound.read_bytes().map_err(|error| error.to_string())?;
        if classified_static.contains(&artifact.id) {
            append_artifact(&mut static_context_artifacts, artifact.id.clone());
            continue;
        }
        collect_bound_context_from_cache(
            blobs_dir, limits,
            &mut static_context_artifacts,
            &mut session_context,
            &mut session_snapshot_head,
            artifact,
            &payload,
            bound_provenance
                .get(&artifact_ref_key(artifact))
                .cloned()
                .unwrap_or_default(),
            session_snapshot_labels,
            &config.provider_interface,
        )?;
    }
    validate_session_context_evidence(
        &mut session_context,
        SessionContextEvidenceScope {
            blobs_dir,
            limits,
            run: &record.id,
            provider: &config.provider_interface,
            context_output_labels,
            session_snapshot_labels,
        },
        session,
    )?;
    let (manifest, payload) = read_pinned_artifact_from_cache(blobs_dir, limits, &source.artifact)?;
    append_session_payload(
        &mut session_context,
        &manifest.label,
        &payload,
        &source.artifact,
        Some(source_movement_seq),
        "provider source movement",
    )?;
    ground_session_context_movements(&mut session_context, session, &record.id, config)?;
    validate_resolved_session_context(
        &session_context,
        "provider authoring failure source",
        &config.provider_interface,
    )?;
    reconcile_resolved_session_snapshot_head(&session_context, &mut session_snapshot_head)?;
    Ok((session_context, session_snapshot_head, static_context_artifacts))
}

pub(crate) fn source_context_payloads_from_cache(
    blobs_dir: &Path, limits: InterfaceHostIoLimits, pinned: &RunRecord, config: &RunAuthoringConfig,
    checkpoint: &octopus_run_record::CheckpointRecord, template: &RunAuthoringCheckpoint, source: &IndexedOutboundArtifact,
) -> Result<(Vec<octopus_interfaces::ArtifactBindingPayload>, BTreeSet<ArtifactId>), String> {
    let mut selected = payload_artifact_bindings(pinned, &config.provider_interface)?;
    let call_evidence = result_inputs::bound_call_evidence(pinned, &config.provider_interface, &checkpoint.id)?;
    if let Some(reference) = &call_evidence {
        if !selected.contains(reference) { selected.push(reference.clone()); }
    }
    for reference in input_artifacts::bound_sources(pinned, &config.provider_interface, &checkpoint.id, &source.artifact_binding_refs)? {
        if !selected.contains(&reference) { selected.push(reference); }
    }
    validate_payload_read_bindings(&selected, checkpoint, &source.artifact_binding_refs, &source.readable_artifacts, &source.role_artifacts)?;
    let mut payloads = source.artifact_binding_refs.iter().map(|artifact| {
        Ok(octopus_interfaces::ArtifactBindingPayload::new(artifact.clone(), read_artifact_ref_payload_from_cache(blobs_dir, limits, artifact)?))
    }).collect::<Result<Vec<_>, String>>()?;
    payloads.retain(|bound|call_evidence.as_ref() != Some(&bound.artifact));
    let mut static_ids = bound_instruction_sources(&payloads, &selected)?;
    static_ids.extend(source.role_artifacts.iter().map(ArtifactId::new));
    static_ids.extend(selected.iter().map(|reference| reference.id.clone()));
    static_ids.extend(template.static_artifact_bindings.iter().cloned());
    Ok((payloads, static_ids))
}

pub(crate) fn ground_session_context_movements(
    session_context: &mut ResolvedSessionContext,
    session: &ProviderHandoffSession,
    run: &RunId,
    config: &RunAuthoringConfig,
) -> Result<(), String> {
    if session_context.artifacts.is_empty() {
        return Ok(());
    }
    let refs = session_context
        .artifacts
        .iter()
        .map(|artifact| artifact.source_artifact.clone())
        .collect::<Vec<_>>();
    let provenance =
        session
            .index()?
            .bound_provenance(run, &config.provider_interface, refs.iter())?;
    for artifact in session_context.artifacts.iter_mut() {
        if artifact.source_movement_seq.is_some() {
            continue;
        }
        let grounded = provenance
            .get(&artifact_ref_key(&artifact.source_artifact))
            .and_then(|found| found.provider_arrival_seq.or(found.movement_seq));
        artifact.source_movement_seq =
            Some(grounded.ok_or_else(|| {
                format!(
                    "session context artifact '{}' has no movement in run '{}'",
                    artifact.source_artifact.id.as_str(),
                    run.as_str()
                )
            })?);
    }
    Ok(())
}

pub(crate) fn materialize_authoring_turn_error(
    ctx: &InterfaceLiftContext,
    authoring_interface: &InterfaceId,
    failure: &InterfaceFailureEnvelope,
) -> Result<InterfaceArtifactFile, String> {
    let detail = failure
        .details
        .get("error")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(failure.failure_class.as_str());
    let payload = serde_json::to_vec(&serde_json::json!({
        "label": "turn.error",
        "class": "run_authoring_failed",
        "code": failure.failure_class,
        "message": format!("run authoring failed: {detail}"),
        "will_retry": false,
        "evidence": {
            "authoring_interface": authoring_interface,
            "source_movement_seq": failure.source_movement_seq,
            "failure_details": failure.details
        }
    }))
    .map_err(|error| format!("provider authoring turn.error did not encode: {error}"))?;
    InterfaceArtifactFile::write(
        ctx.interface_var_dir().join("emitted"),
        "turn.error",
        &payload,
    )
    .map_err(|error| format!("provider authoring turn.error did not materialize: {error}"))
}

pub(crate) async fn hand_authoring_terminal_failure(
    ctx: &InterfaceLiftContext,
    session: &ProviderHandoffSession,
    record: &RunRecord,
    config: &RunAuthoringConfig,
    failure_seq: u64,
    failure: &InterfaceFailureEnvelope,
    session_context: &ResolvedSessionContext,
    session_snapshot_head: Option<&SessionSnapshotHead>,
    session_snapshot_labels: &BTreeSet<String>,
) -> Result<ProviderHandoffOutcome, String> {
    let authoring_result =
        config.checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringResult)?;
    let output = config.output_checkpoint(&ProviderHandoffRoute::ClientProjection,
        &BTreeSet::new(), &BTreeSet::new(), "turn.error")?;
    let client = active_checkpoint_for_template(record, config, &output)?;
    let mut artifact =
        materialize_authoring_turn_error(ctx, authoring_result.interface(), failure)?;
    let manifest = artifact.read_manifest().map_err(|error| {
        format!("provider authoring terminal error manifest cannot be read: {error}")
    })?;
    if session_snapshot_labels.contains(&manifest.label) {
        artifact = materialize_session_snapshot(&artifact, session_context, session_snapshot_head)?;
    }
    let expected = artifact_identity(&artifact, &ctx.interface_var_dir())?.artifact;
    let coordinate = PendingHandoffLedgerCoordinate {
        interface: &ctx.interface,
        run: &record.id,
        worker: &record.worker,
        artifact: &expected,
        required_evidence: None,
        checkpoint: &client.id,
        epoch: client.epoch.0,
    };
    if let Some(client_seq) = session.index()?.admitted_handoff_seq(coordinate)? {
        if client_seq <= failure_seq {
            return Err(
                "provider authoring terminal error does not follow its failure movement"
                    .to_string(),
            );
        }
        return Ok(ProviderHandoffOutcome::TerminalFailure {
            failure_seq,
            client_seq,
            failure_class: failure.failure_class.clone(),
        });
    }
    let verdict = ctx.hand_arrival(artifact, Vec::new()).await?;
    let client_seq = session.index()?.admitted_handoff_seq(coordinate)?;
    match (verdict, client_seq) {
        (DeliveryVerdict::Admitted { .. }, Some(client_seq))
        | (DeliveryVerdict::Refused { .. }, Some(client_seq))
            if client_seq > failure_seq =>
        {
            Ok(ProviderHandoffOutcome::TerminalFailure {
                failure_seq,
                client_seq,
                failure_class: failure.failure_class.clone(),
            })
        }
        (DeliveryVerdict::Admitted { .. }, Some(_))
        | (DeliveryVerdict::Refused { .. }, Some(_)) => Err(
            "provider authoring terminal error does not follow its failure movement".to_string(),
        ),
        (DeliveryVerdict::Admitted { .. }, None) => Err(
            "provider authoring terminal error was admitted without exact client checkpoint evidence"
                .to_string(),
        ),
        (DeliveryVerdict::Refused { reason, detail }, None) => Err(format!(
            "provider authoring terminal error was refused as '{}': {}",
            reason.as_str(),
            detail
        )),
    }
}

pub(crate) enum BoundedLedgerRow {
    Complete(Vec<u8>),
    Partial,
    Eof,
}

pub(crate) async fn await_checked_evidence<T>(
    ctx: &InterfaceLiftContext,
    timeout_ms: u64,
    mut validate: impl FnMut() -> Result<T, CheckedRunError>,
) -> Result<T, CheckedRunError> {
    let deadline = Instant::now()
        .checked_add(Duration::from_millis(timeout_ms))
        .ok_or("checked input evidence deadline overflow")?;
    let mut watcher = None;
    loop {
        if ctx.cancel.is_cancelled() {
            return Err("checked input preparation was cancelled".into());
        }
        match validate() {
            Ok(value) => return Ok(value),
            Err(error @ CheckedRunError::Invalid(_)) => return Err(error),
            Err(changed @ CheckedRunError::ChangedDuringRead { .. }) => {
                if Instant::now() >= deadline {
                    return Err(changed);
                }
                tokio::task::yield_now().await;
            }
            Err(waiting @ CheckedRunError::AwaitingResults { .. }) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(waiting);
                }
                if watcher.is_none() {
                    watcher = Some(LedgerTailReader::open_named(ctx, "checked-input-evidence")?);
                }
                tokio::time::timeout(
                    remaining,
                    watcher.as_mut().unwrap().wait_for_change(&ctx.cancel),
                )
                .await
                .map_err(|_| waiting)??;
            }
        }
    }
}
