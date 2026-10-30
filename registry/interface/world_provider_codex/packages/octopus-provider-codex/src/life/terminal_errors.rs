use super::*;

pub(crate) fn emit_terminal_turn_error<'a>(
    ctx: &'a InterfaceLiftContext,
    descriptor: &'a InterfaceDescriptor,
    handoff_session: &'a ProviderHandoffSession,
    run: &'a RunId,
    turn: &'a QueuedTurn,
    handoff: &'a ProviderOutputHandoff,
    attempts: u32,
    error: &'a str,
) -> std::pin::Pin<Box<impl std::future::Future<Output = Result<(), String>> + 'a>> {
    Box::pin(emit_terminal_turn_error_inner(
        ctx, descriptor, handoff_session, run, turn, handoff, attempts, error,
    ))
}

pub(crate) async fn emit_terminal_turn_error_inner(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    handoff_session: &ProviderHandoffSession,
    run: &RunId,
    turn: &QueuedTurn,
    handoff: &ProviderOutputHandoff,
    attempts: u32,
    error: &str,
) -> Result<(), String> {
    let attempt_summary = match attempts {
        0 => "before any transport attempt".to_string(),
        1 => "after 1 attempt".to_string(),
        count => format!("after {count} attempts"),
    };
    let message = format!(
        "codex turn failed terminally {attempt_summary} (movement seq {}): {error}",
        turn.seq
    );
    emit_terminal_error_message(ctx, descriptor, handoff_session, run, turn, handoff, &message).await
}

pub(crate) async fn emit_terminal_error_message(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    handoff_session: &ProviderHandoffSession,
    run: &RunId,
    turn: &QueuedTurn,
    handoff: &ProviderOutputHandoff,
    message: &str,
) -> Result<(), String> {
    let current_session_context = handoff.current_session_context_artifacts().await;
    let output_root = ctx.interface_var_dir().join("emitted");
    let artifact = match terminal_output_continuity(
        descriptor,
        &turn.envelope,
        &current_session_context,
        run,
        &handoff.current_run_record_bytes().await,
        handoff.deferred_context_generations().await,
    ) {
        Ok(continuity) => {
            crate::handler::materialize_terminal_turn_error_payload(
                &output_root, message, continuity.as_ref(),
                serde_json::json!({"source_movement_seq": turn.seq}),
            )
        }
        Err(error) => match codex_continuity_incompatibility(&error) {
            Some(detail) => materialize_incompatible_continuity_turn_error(
                &output_root,
                turn.seq,
                detail,
                message,
            ),
            None => {
                return record_invalid_provider_context(
                    ctx, descriptor, handoff_session, run, turn,
                    format!("{message}; terminal continuity cannot be settled: {error}"),
                ).await;
            }
        },
    }
    .map_err(|error| {
        format!(
            "codex terminal error for run '{}' movement {} failed to materialize: {error}",
            run.as_str(),
            turn.seq
        )
    })?;
    let terminal_label = artifact
        .read_manifest()
        .map_err(|error| format!("codex terminal error manifest is unreadable: {error}"))?
        .label;
    let terminal_artifact_payload_blake3 = artifact_payload_blake3(&artifact)?;
    let private_config = &descriptor
        .interface_driver_config
        .as_ref()
        .ok_or_else(|| {
            format!(
                "provider interface '{}' has no interface_driver_config",
                descriptor.id.as_str()
            )
        })?
        .private_config;
    let rules = LiveStreamRules::from_private_config_or_disabled(private_config)
        .map_err(|error| error.to_string())?;
    let streams = seal_source_turn_stream(
        &ctx.interface_var_dir().join("streams"),
        &rules,
        descriptor.id.as_str(),
        run.as_str(),
        turn.seq,
        &terminal_label,
        &terminal_artifact_payload_blake3,
    )?;
    let terminal = ProviderEmission {
        artifact,
        route: ProviderEmissionRoute::ClientProjection,
    };
    for stream in streams {
        match handoff.hand_observation(ProviderEmission {
            artifact: stream,
            route: ProviderEmissionRoute::ClientProjection,
        }, false).await? {
            ProviderHandoffOutcome::Admitted { .. } => {}
            ProviderHandoffOutcome::TerminalFailure { failure_seq, failure_class, .. } => {
                return Err(format!(
                    "terminal stream observation stopped by authoring failure '{failure_class}' at movement {failure_seq}"
                ));
            }
        }
    }
    handoff.hand_final(terminal).await.map(|_| ())
}

pub(crate) async fn recover_interrupted_result_without_context(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    handoff_session: &ProviderHandoffSession,
    interrupted_effects: &InterruptedEffectArbiter,
    control: &CodexTurnControlBroker,
    run: &RunId,
    turn: &QueuedTurn,
) -> Result<bool, String> {
    let Some(continuity) = interrupted_effects.durable_continuity(run).await? else {
        return Ok(false);
    };
    if !exact_interrupted_result_source(&turn.envelope, &continuity)? {
        return Ok(false);
    }
    let Some((emissions, closure)) = recover_sealed_interrupted_result(
        ctx, descriptor, turn, &continuity,
    )? else { return Ok(false); };
    if !interrupted_effects.begin_result(run, turn.seq, &turn.envelope).await? {
        return Err("sealed interrupted result does not match the durable exact effect".into());
    }
    let handoff = handoff_session.failed_source_handoff(
        ctx, descriptor, run, &turn.envelope, turn.seq,
    )?;
    admit_interrupted_result(
        ctx, &handoff, control, Some(interrupted_effects), run, turn.seq, &closure, emissions,
    ).await?;
    Ok(true)
}

pub(crate) fn recover_sealed_interrupted_result(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    turn: &QueuedTurn,
    continuity: &crate::handler::CodexContinuityState,
) -> Result<Option<(Vec<ProviderEmission>, ArtifactRef)>, String> {
    let private_config = &descriptor.interface_driver_config.as_ref()
        .ok_or("sealed interruption recovery has no declared provider config")?.private_config;
    let rules = LiveStreamRules::from_private_config_or_disabled(private_config)
        .map_err(|error| error.to_string())?;
    let identity = result_turn_identity(continuity)?;
    let Some((stream, payload_hash)) = recover_sealed_terminal_stream(
        &ctx.interface_var_dir().join("streams"), &rules, &identity, turn.seq,
        TURN_INTERRUPTED_RESULT_LABEL,
    )? else { return Ok(None); };
    let output_root = ctx.interface_var_dir().join("emitted").canonicalize()
        .map_err(|error| error.to_string())?;
    let slug = TURN_INTERRUPTED_RESULT_LABEL.replace('.', "_");
    let manifest = octopus_interfaces::InterfaceArtifactManifest {
        label: TURN_INTERRUPTED_RESULT_LABEL.to_string(),
        payload_path: output_root.join(format!("{slug}-{payload_hash}.payload")),
        payload_content_hash: Some(payload_hash),
    };
    let manifest_bytes = serde_json::to_vec_pretty(&manifest).map_err(|error| error.to_string())?;
    let manifest_hash = ContentHash::of_bytes(&manifest_bytes).to_hex();
    let path = output_root.join(format!("{slug}-{manifest_hash}.artifact.json"));
    if std::fs::read(&path).map_err(|error| error.to_string())? != manifest_bytes {
        return Err("sealed interruption recovery changed its terminal manifest".into());
    }
    let terminal = octopus_interfaces::InterfaceArtifactFile::new(path).map_err(|error| error.to_string())?;
    let closure = if crate::handler::codex_continuity_has_interrupted_pending(continuity) {
        interrupted_result_closure_ref(&terminal, continuity, &turn.envelope.artifact, &codex_context_output_labels(descriptor).map_err(|error| error.to_string())?)?
    } else {
        native_terminal_result_closure_ref(&terminal, continuity, &turn.envelope, &codex_context_output_labels(descriptor).map_err(|error| error.to_string())?)?
    }
        .ok_or("sealed interruption has no exact result closure")?;
    let mut emissions = vec![
        ProviderEmission { artifact: stream, route: ProviderEmissionRoute::ClientProjection },
    ];
    emissions.extend(recover_retained_native_outputs(&terminal, &output_root, &codex_context_output_labels(descriptor).map_err(|error| error.to_string())?)?);
    emissions.push(ProviderEmission { artifact: terminal, route: ProviderEmissionRoute::ClientProjection });
    Ok(Some((emissions, closure)))
}

pub(crate) fn native_result_admitted_prefix(
    ctx: &InterfaceLiftContext,
    run: &RunId,
    source_seq: u64,
    emissions: &[ProviderEmission],
    snapshots: &BTreeSet<String>,
    context_labels: &BTreeSet<String>,
) -> Result<Vec<ProviderHandoffOutcome>, String> {
    let expected = emissions.iter().map(|emission| {
        Ok((emission.artifact.read_manifest().map_err(|error| error.to_string())?.label,
            materialized_artifact_ref(&emission.artifact)?,
            emission.artifact.read_payload_bytes().map_err(|error| error.to_string())?))
    }).collect::<Result<Vec<_>, String>>()?;
    if expected.iter().map(|item| &item.0).collect::<BTreeSet<_>>().len() != expected.len() {
        return Err("native result group repeats a recovery label".into());
    }
    let settled = serde_json::from_slice::<serde_json::Value>(&expected.last().ok_or("native result group is empty")?.2)
        .map_err(|error| error.to_string())?["codex_continuity"]["context_generation"].as_u64()
        .ok_or("native result group has no settled generation")?;
    let (mut file, length, limit) = open_arbitration_snapshot(ctx)?;
    let decoder = DurableMovementDecoder::for_log(&ctx.movements_path);
    let (offset, next) = authoritative_source_scan_start(ctx, &mut file, length, limit, &decoder, source_seq);
    let mut source_verified = false;
    let mut prefix = Vec::new();
    let scan = scan_authoritative_snapshot(ctx, &mut file, length, limit, &decoder, offset, next, None, |movement| {
        if movement.seq.0 < source_seq { return Ok(None::<()>); }
        if movement.seq.0 == source_seq {
            if !movement_is_provider_source(ctx, run, &movement) {
                return Err("native result prefix changed its exact provider source".into());
            }
            source_verified = true;
            return Ok(None);
        }
        if !source_verified { return Err("native result prefix passed its unverified source".into()); }
        let Some(reference) = movement.artifact.as_ref() else { return Ok(None); };
        if !movement_is_artifact_admission(ctx, run, reference, &movement)
            || !expected.iter().any(|item| artifact_ref_has_label(reference, &item.0)) {
            return Ok(None);
        }
        let runtime = movement.runtime().ok_or("native result prefix has no runtime coordinates")?;
        let hash = runtime.run_record_hash.as_deref().and_then(ContentHash::from_hex)
            .ok_or("native result prefix has no pinned run snapshot")?;
        let path = ctx.blobs_dir.join(hash.to_hex());
        if std::fs::metadata(&path).map_err(|error| error.to_string())?.len() > ctx.required_host_io_limits()?.max_payload_bytes {
            return Err("native result prefix run snapshot exceeds its declared payload limit".into());
        }
        let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
        if ContentHash::of_bytes(&bytes) != hash { return Err("native result prefix run snapshot changed its hash".into()); }
        let record = octopus_run_record::RunRecord::from_bytes(&bytes).map_err(|error| error.to_string())?;
        let state = record.interface_state.get(&ctx.interface).ok_or("native result prefix lost its provider state")?;
        let pending = state.get("pending_handoff").ok_or("native result prefix has no pending handoff")?;
        let owner = pending.get("source_movement_seq").and_then(serde_json::Value::as_u64)
            .ok_or("native result prefix pending source is malformed")?;
        if owner != source_seq { return Ok(None); }
        let (manifest, actual) = read_pinned_provider_artifact(ctx, reference, "native result prefix")?;
        let index = expected.iter().position(|item| item.0 == manifest.label)
            .ok_or("native result prefix manifest conflicts with its artifact label")?;
        if index != prefix.len() { return Err("native result admissions are not an exact contiguous prefix".into()); }
        let remaining = expected[index + 1..].iter().filter(|item| context_labels.contains(&item.0)).count() as u64;
        if state.get("context_generation").and_then(serde_json::Value::as_u64) != settled.checked_sub(remaining) {
            return Err("native result prefix changed its pinned authored context generation".into());
        }
        let checkpoint = record.checkpoint(&runtime.checkpoint).ok_or("native result prefix checkpoint is absent")?;
        if record.id != *run || record.ee != runtime.ee || record.worker != runtime.worker
            || !record.active_checkpoints.contains(&checkpoint.id) || checkpoint.epoch.0 != runtime.epoch
            || checkpoint.inbound_interface() != Some(&ctx.interface)
            || !checkpoint.required_verified_artifacts.is_empty()
        { return Err("native result prefix crossed its pinned checkpoint scope".into()); }
        let pinned_checkpoint = serde_json::json!({
            "id":checkpoint.id,"epoch":checkpoint.epoch.0,"interface_binding":checkpoint.interface_binding,
            "outbound_interface_bindings":checkpoint.outbound_interface_bindings,
            "artifact_bindings":checkpoint.artifact_bindings.iter().map(|binding| &binding.id).collect::<Vec<_>>(),
            "required_verified_artifacts":checkpoint.required_verified_artifacts,
        });
        let items = pending.get("items").and_then(serde_json::Value::as_array)
            .ok_or("native result prefix pending items are malformed")?;
        let matched = items.iter().filter(|item| item.get("artifact_id") == Some(&serde_json::json!(reference.id))
            && item.get("artifact_content_hash").and_then(serde_json::Value::as_str) == Some(reference.content.to_hex().as_str())
            && item.get("artifact_label").and_then(serde_json::Value::as_str) == Some(manifest.label.as_str())
            && item.get("checkpoint") == Some(&pinned_checkpoint)).count();
        if matched != 1 || pending.get("terminal").and_then(serde_json::Value::as_bool) != Some(index + 1 == expected.len()) {
            return Err("native result prefix differs from its exact pinned pending item".into());
        }
        let raw = &expected[index];
        if snapshots.contains(&manifest.label) {
            let mut value: serde_json::Value = serde_json::from_slice(&actual).map_err(|error| error.to_string())?;
            value.as_object_mut().ok_or("native result snapshot is not an object")?.remove("session_context")
                .ok_or("native result snapshot lost its declared session context")?;
            if value != serde_json::from_slice::<serde_json::Value>(&raw.2).map_err(|error| error.to_string())? {
                return Err("native result prefix changed its immutable semantic payload".into());
            }
        } else if *reference != raw.1 || actual != raw.2 {
            return Err("native result prefix changed its immutable artifact".into());
        }
        prefix.push(ProviderHandoffOutcome::Admitted { seq: movement.seq.0, artifact: reference.clone() });
        Ok(None)
    })?;
    ensure_arbitration_snapshot_length(ctx, &file, length)?;
    if !scan.complete_boundary || !source_verified { return Err("native result prefix lacks a complete authoritative ledger boundary".into()); }
    Ok(prefix)
}

pub(crate) async fn admit_interrupted_result(
    ctx: &InterfaceLiftContext,
    handoff: &ProviderOutputHandoff,
    control: &CodexTurnControlBroker,
    interrupted_effects: Option<&InterruptedEffectArbiter>,
    run: &RunId,
    source_movement_seq: u64,
    closure: &ArtifactRef,
    emissions: Vec<ProviderEmission>,
) -> Result<InterruptedResultAdmission, String> {
    let terminal = emissions.last().ok_or("interrupted-result group has no terminal closure")?;
    if materialized_artifact_ref(&terminal.artifact)? != *closure {
        return Err("interrupted-result terminal differs from its exact closure".into());
    }
    let expected_payload: serde_json::Value = serde_json::from_slice(
        &terminal.artifact.read_payload_bytes().map_err(|error| error.to_string())?,
    ).map_err(|error| error.to_string())?;
    let native = expected_payload.get("native_terminal_status").is_some();
    let native_recovery = if native {
        let recovered = handoff.recover_source_with_outcomes().await?;
        if matches!(recovered.0, ProviderSourceRecovery::Bridged { .. }) {
            control.finish();
            return Ok(InterruptedResultAdmission::Bridged);
        }
        Some(recovered)
    } else { None };
    if let Some(interrupted_effects) = interrupted_effects {
        interrupted_effects.begin_closing(run, source_movement_seq, closure.clone()).await?;
    }
    let expected = emissions.len();
    let mut outcomes = Vec::new();
    if native {
        let descriptor = ctx.descriptor_snapshot.as_ref().ok_or("native result admission has no declared provider")?;
        let labels = codex_context_output_labels(descriptor).map_err(|error| error.to_string())?;
        let snapshots = codex_session_snapshot_labels(descriptor).map_err(|error| error.to_string())?;
        let (recovery, recovered) = native_recovery.ok_or("native result has no source recovery")?;
        outcomes = native_result_admitted_prefix(ctx, run, source_movement_seq, &emissions, &snapshots, &labels)?;
        for outcome in &recovered {
            if !outcomes.iter().any(|admitted| matches!((admitted, outcome),
                (ProviderHandoffOutcome::Admitted {seq:a,artifact:x},ProviderHandoffOutcome::Admitted {seq:b,artifact:y}) if a == b && x == y)) {
                return Err("native result prefix conflicts with checked source recovery".into());
            }
        }
        match recovery {
            ProviderSourceRecovery::Process if !outcomes.is_empty() => return Err("native result prefix has no checked pending frontier".into()),
            ProviderSourceRecovery::Completed if outcomes.len() != expected => return Err("native result source completed before its whole exact group".into()),
            ProviderSourceRecovery::Interrupted { last_artifact_label } if outcomes.is_empty()
                || emissions[outcomes.len()-1].artifact.read_manifest().map_err(|error| error.to_string())?.label != last_artifact_label
                => return Err("native result prefix changed its checked last pending artifact".into()),
            _ => {}
        }
        let input_context = handoff.input_changes_context().await;
        let sealed_input = expected_payload.get("native_terminal_input_context").and_then(serde_json::Value::as_bool)
            .ok_or("native result closure has no exact input consumption")?;
        if (outcomes.is_empty() && input_context != sealed_input) || (!outcomes.is_empty() && input_context) {
            return Err("native result input consumption conflicts with checked source recovery".into());
        }
        let generation = handoff.context_generation().await?;
        let delta = emissions[outcomes.len()..].iter().map(|emission| emission.artifact.read_manifest()
            .map(|manifest| u64::from(labels.contains(&manifest.label))).map_err(|error| error.to_string()))
            .collect::<Result<Vec<_>, _>>()?.into_iter().sum::<u64>();
        let settled = expected_payload.get("codex_continuity").and_then(|state| state.get("context_generation"))
            .and_then(serde_json::Value::as_u64).ok_or("native result closure has no exact settled generation")?;
        if generation.checked_add(u64::from(input_context)).and_then(|value| value.checked_add(delta)) != Some(settled) {
            return Err("native result closure conflicts with checked authored context generation".into());
        }
    }
    let suffix = emissions.into_iter().skip(outcomes.len()).collect::<Vec<_>>();
    if !suffix.is_empty() { outcomes.extend(handoff.hand_batch(suffix).await?); }
    if outcomes.len() != expected {
        return Err("exact result group lost its ordered gate outcomes".into());
    }
    if native {
        let current = octopus_run_record::RunRecord::from_bytes(&handoff.current_run_record_bytes().await)
            .map_err(|error| error.to_string())?;
        if current.interface_state.get(&ctx.interface).and_then(|state| state.get("context_generation"))
            != expected_payload.get("codex_continuity").and_then(|state| state.get("context_generation")) {
            return Err("native result admitted closure differs from its checked authored context".into());
        }
    }
    for outcome in &outcomes {
        if let ProviderHandoffOutcome::TerminalFailure { failure_seq, failure_class, .. } = outcome {
            return Err(format!("exact result group was replaced by terminal run-authoring failure '{failure_class}' at movement {failure_seq}"));
        }
    }
    match outcomes.last().ok_or("exact result group has no admitted closure")? {
        ProviderHandoffOutcome::Admitted { seq, artifact } => {
            let (manifest, bytes) = read_pinned_provider_artifact(ctx, artifact, "admitted native result closure")?;
            let mut payload: serde_json::Value = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
            payload.as_object_mut().ok_or("admitted native result closure is not an object")?.remove("session_context");
            if manifest.label != TURN_INTERRUPTED_RESULT_LABEL || payload != expected_payload {
                return Err("exact result group admitted a different final closure payload".into());
            }
            validate_artifact_admission(ctx, run, source_movement_seq, &DurableArtifactAdmission {
                artifact: artifact.clone(), movement_seq: *seq,
            })?;
            if let Some(interrupted_effects) = interrupted_effects {
                interrupted_effects.complete_closing(run, source_movement_seq, closure, &DurableArtifactAdmission {
                    artifact: artifact.clone(), movement_seq: *seq,
                }).await?;
            }
        }
        ProviderHandoffOutcome::TerminalFailure { failure_seq, failure_class, .. } => {
            return Err(format!(
                "interrupted-result closure was replaced by terminal run-authoring failure '{failure_class}' at movement {failure_seq}"
            ));
        }
    }
    control.finish();
    Ok(InterruptedResultAdmission::Admitted(outcomes.len()))
}
