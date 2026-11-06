use super::*;

pub(crate) fn resume_control_continuity(
    ctx: &InterfaceLiftContext,
    run: &RunId,
    source_movement_seq: u64,
    context_generation: u64,
) -> Result<crate::handler::CodexContinuityState, String> {
    let directory = ctx.interface_var_dir().join("turn-control-journal");
    let mut candidates = Vec::new();
    for entry in std::fs::read_dir(&directory).map_err(|error| format!("Resume cannot read its stopping receipts: {error}"))? {
        let entry = entry.map_err(|error| error.to_string())?;
        if entry.path().extension().is_none_or(|extension| extension != "json") { continue; }
        let sequence = entry.path().file_stem().and_then(|stem| stem.to_str()).and_then(|stem| stem.parse::<u64>().ok())
            .ok_or("Resume found a stopping receipt with no exact source sequence")?;
        if sequence >= source_movement_seq { continue; }
        let journal: TurnControlJournal = serde_json::from_slice(&std::fs::read(entry.path()).map_err(|error| error.to_string())?)
            .map_err(|error| format!("Resume stopping receipt is malformed: {error}"))?;
        if journal.schema_version != 1 || journal.source_movement_seq != sequence {
            return Err("Resume stopping receipt differs from its source identity".into());
        }
        if journal.run_id != run.as_str() || !terminal_control_is_satisfied(&journal.action) { continue; }
        let TurnControlJournalPhase::Delivered { result } = journal.phase else { continue; };
        if result.control_id != journal.control_id || result.action != journal.action {
            return Err("Resume stopping receipt changed its acknowledged control".into());
        }
        if !result.acknowledged { continue; }
        candidates.push((sequence, result));
    }
    let (sequence, result) = candidates.into_iter().max_by_key(|(sequence, _)| *sequence)
        .ok_or("Resume has no delivered stopping receipt for this run")?;
    let continuity = result.continuity.clone().ok_or("Resume stopping receipt has no native thread continuity")?;
    if continuity.schema_version != 1 || continuity.provider_interface != ctx.interface || continuity.run != run.as_str()
        || continuity.context_generation != context_generation
        || result.thread_id.as_deref().is_some_and(|thread| thread != continuity.thread_id)
    {
        return Err("Resume stopping receipt differs from the current run continuity".into());
    }
    let artifact = materialize_turn_control_result(&ctx.interface_var_dir().join("emitted"), sequence, result)
        .map_err(|error| error.to_string())?;
    let reference = materialized_artifact_ref(&artifact)?;
    if find_artifact_admission(ctx, run, sequence, &reference)?.movement_seq.is_none_or(|admitted| admitted >= source_movement_seq) {
        return Err("Resume stopping receipt has no causally prior admitted outcome".into());
    }
    Ok(continuity)
}

pub(crate) async fn hand_control_result(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    handoff_session: &ProviderHandoffSession,
    interrupted_effects: &InterruptedEffectArbiter,
    run: &RunId,
    delivery: ControlResultDelivery<'_>,
) -> Result<bool, String> {
    let ControlResultDelivery {
        envelope,
        source_movement_seq,
        output,
        handoff_barrier,
        effect_guard: _effect_guard,
        result,
        recovery,
        defer_message_receipt,
    } = delivery;
    let interrupted_continuity = exact_interrupted_control_continuity(&result)?;
    let control_id = result.control_id.clone();
    let mut artifact = materialize_turn_control_result(
        &ctx.interface_var_dir().join("emitted"),
        source_movement_seq,
        result.clone(),
    )
    .map_err(|error| format!("codex turn-control result failed to materialize: {error}"))?;
    let mut artifact_ref = materialized_artifact_ref(&artifact)?;
    let mut admitted_seq = if recovery {
        await_artifact_admission_scan(ctx, run, source_movement_seq, &artifact_ref)
            .await?
            .movement_seq
    } else {
        None
    };
    if admitted_seq.is_none() {
        if let Some(output) = output {
            match output
                .hand_control(ProviderEmission {
                    artifact: artifact.clone(),
                    route: ProviderEmissionRoute::ClientProjection,
                }, source_movement_seq)
                .await
            {
                Ok(()) => {
                    admitted_seq =
                        await_artifact_admission_scan(ctx, run, source_movement_seq, &artifact_ref)
                            .await?
                            .movement_seq;
                }
                Err(ControlOutputError::Closed) => {}
                Err(ControlOutputError::Failed(error)) => return Err(error),
            }
        }
    }
    if admitted_seq.is_none() && defer_message_receipt
        && matches!(result.action.as_str(), "queue_message" | "steer")
    {
        return Ok(false);
    }
    if admitted_seq.is_none() {
        if let Some(barrier) = handoff_barrier {
            barrier.wait().await.map_err(|error| {
                format!(
                    "prior provider handoff failed before turn control could be authored: {error}"
                )
            })?;
        }
        let pinned_run_record = ctx.pinned_run_record_bytes(envelope)?;
        let handoff = wait_for_provider_handoff(
            ctx,
            descriptor,
            handoff_session,
            run,
            envelope,
            source_movement_seq,
            &pinned_run_record,
        ).await?;
        match handoff.recover_source().await? {
            ProviderSourceRecovery::Completed => {}
            ProviderSourceRecovery::Interrupted {
                last_artifact_label,
            } => {
                return Err(format!(
                    "turn-control result cannot cross while non-terminal provider artifact '{last_artifact_label}' owns the run frontier"
                ));
            }
            ProviderSourceRecovery::Process => {
                if result.continuity.is_none()
                    && result.acknowledged
                    && terminal_control_is_satisfied(&result.action)
                    && result.thread_id.is_none() && result.turn_id.is_none()
                    && result.detail == terminally_satisfied_control(&control_id, &result.action).result.detail
                {
                    let context = handoff.current_session_context_artifacts().await;
                    let continuity = terminal_output_continuity(
                        descriptor, envelope, &context, run, &handoff.current_run_record_bytes().await,
                        handoff.deferred_context_generations().await,
                    ).map_err(|error| error.to_string())?;
                    if let Some(pending) = continuity.as_ref().filter(|state| codex_continuity_waits_for_result(state)) {
                        let original = interrupted_effects.durable_continuity(run).await?
                            .ok_or("terminal turn-control receipt still has an unresolved native effect")?;
                        if !crate::handler::codex_continuity_retains_interrupted_effect(&original, pending) {
                            return Err("terminal turn-control receipt changed its unresolved interrupted effect".into());
                        }
                        let identity = crate::handler::interrupted_turn_identity(&original)?;
                        result.thread_id = Some(identity.thread_id);
                        result.turn_id = Some(identity.turn_id);
                        result.detail["exact_result_pending"] = serde_json::json!(true);
                    }
                    result.continuity = continuity;
                    persist_control_journal(ctx, source_movement_seq, run, &control_id, &result.action,
                        TurnControlJournalPhase::Result { result: result.clone() })?;
                    artifact = materialize_turn_control_result(
                        &ctx.interface_var_dir().join("emitted"), source_movement_seq, result.clone(),
                    ).map_err(|error| format!("codex turn-control result failed to materialize: {error}"))?;
                    artifact_ref = materialized_artifact_ref(&artifact)?;
                }
                let emission = ProviderEmission { artifact, route: ProviderEmissionRoute::ClientProjection };
                if result.continuity.is_none() && (!result.acknowledged || matches!(result.action.as_str(), "queue_message" | "steer")) {
                    handoff.hand_observation(emission, true).await?;
                } else {
                    handoff.hand_final(emission).await?;
                }
            }
        }
        admitted_seq = await_artifact_admission_scan(ctx, run, source_movement_seq, &artifact_ref)
            .await?
            .movement_seq;
    }
    if let Some(continuity) = interrupted_continuity {
        let movement_seq = admitted_seq.ok_or_else(|| {
            "acknowledged interrupt has no admitted control-result movement".to_string()
        })?;
        interrupted_effects
            .record_acknowledgement(
                run,
                source_movement_seq,
                &control_id,
                DurableArtifactAdmission {
                    artifact: artifact_ref,
                    movement_seq,
                },
                continuity,
            )
            .await?;
    }
    Ok(true)
}

pub(crate) fn materialized_artifact_ref(
    artifact: &octopus_interfaces::InterfaceArtifactFile,
) -> Result<ArtifactRef, String> {
    let manifest_bytes = std::fs::read(artifact.manifest_path()).map_err(|error| {
        format!(
            "turn-control result manifest '{}' cannot be read for recovery: {error}",
            artifact.manifest_path().display()
        )
    })?;
    let manifest = artifact.read_manifest().map_err(|error| {
        format!(
            "turn-control result manifest '{}' is invalid during recovery: {error}",
            artifact.manifest_path().display()
        )
    })?;
    let content = ContentHash::of_bytes(&manifest_bytes);
    Ok(ArtifactRef {
        id: ArtifactId::new(format!("{}:{}", manifest.label, content.to_hex())),
        content,
    })
}

pub(crate) fn control_id(control: &CodexTurnControl) -> &str {
    match control {
        CodexTurnControl::Inject { control_id, .. }
        | CodexTurnControl::Steer { control_id, .. }
        | CodexTurnControl::Interrupt { control_id, .. }
        | CodexTurnControl::Unsupported { control_id, .. } => control_id,
    }
}

pub(crate) fn control_action(control: &CodexTurnControl) -> &str {
    match control {
        CodexTurnControl::Inject { .. } => "queue_message",
        CodexTurnControl::Steer { .. } => "steer",
        CodexTurnControl::Interrupt { action, .. }
        | CodexTurnControl::Unsupported { action, .. } => action,
    }
}

pub(crate) async fn hand_stream_emission(
    handoff: &ProviderOutputHandoff,
    pending: PendingProviderEmission,
    terminal: bool,
) -> Result<ProviderHandoffOutcome, String> {
    let (emission, admitted, bindings) = pending;
    let result = handoff
        .hand_batch_with_outgoing_bindings(vec![emission], terminal, &bindings)
        .await
        .and_then(|mut outcomes| {
            if outcomes.len() != 1 {
                return Err(format!("provider emission produced {} outcomes instead of 1", outcomes.len()));
            }
            Ok(outcomes.remove(0))
        })
    .and_then(|outcome| {
        if let ProviderHandoffOutcome::TerminalFailure {
            failure_seq,
            failure_class,
            ..
        } = &outcome
            && !terminal
        {
            return Err(format!(
                "provider stream stopped by terminal run-authoring failure '{failure_class}' at movement {failure_seq}"
            ));
        }
        Ok(outcome)
    });
    match result {
        Ok(outcome) => {
            if let Some(admitted) = admitted {
                let _ = admitted.send(Ok(()));
            }
            Ok(outcome)
        }
        Err(error) => {
            if let Some(admitted) = admitted {
                let _ = admitted.send(Err(error.clone()));
            }
            Err(error)
        }
    }
}

pub(crate) async fn hand_stream_batch(
    handoff: &ProviderOutputHandoff,
    pending: Vec<PendingProviderEmission>,
) -> Result<(), String> {
    if pending.is_empty() {
        return Err("provider stream cannot hand an empty progress batch".to_string());
    }
    let mut pending = pending.into_iter();
    while let Some(emission) = pending.next() {
        if let Err(error) = hand_stream_emission(handoff, emission, false).await {
            for (_, admitted, _) in pending {
                if let Some(admitted) = admitted {
                    let _ = admitted.send(Err(error.clone()));
                }
            }
            return Err(error);
        }
    }
    Ok(())
}

pub(crate) fn flush_stream_batch<'a>(
    handoff: &'a ProviderOutputHandoff,
    pending: &'a mut Vec<PendingProviderEmission>,
    keep_last: bool,
) -> std::pin::Pin<Box<impl std::future::Future<Output = Result<usize, String>> + 'a>> {
    Box::pin(flush_stream_batch_inner(handoff, pending, keep_last))
}

pub(crate) async fn flush_stream_batch_inner(
    handoff: &ProviderOutputHandoff,
    pending: &mut Vec<PendingProviderEmission>,
    keep_last: bool,
) -> Result<usize, String> {
    let flush_len = pending.len().saturating_sub(usize::from(keep_last));
    if flush_len == 0 {
        return Ok(0);
    }
    let batch = pending.drain(..flush_len).collect::<Vec<_>>();
    hand_stream_batch(handoff, batch).await?;
    Ok(flush_len)
}

pub(crate) async fn hold_newest_stream_emission(
    handoff: &ProviderOutputHandoff,
    pending: &mut Vec<PendingProviderEmission>,
    emission: PendingProviderEmission,
) -> Result<usize, String> {
    pending.push(emission);
    flush_stream_batch(handoff, pending, true).await
}

pub(crate) async fn close_stream_emissions(
    handoff: &ProviderOutputHandoff,
    pending: &mut Vec<PendingProviderEmission>,
    terminal: bool,
) -> Result<(usize, Option<ProviderHandoffOutcome>), String> {
    let flushed = flush_stream_batch(handoff, pending, true).await?;
    let Some(last) = pending.pop() else {
        return Ok((flushed, None));
    };
    Ok((flushed, Some(hand_stream_emission(handoff, last, terminal).await?)))
}
