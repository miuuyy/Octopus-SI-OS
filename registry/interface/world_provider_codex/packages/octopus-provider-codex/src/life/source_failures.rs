use super::*;

pub(crate) async fn record_invalid_provider_context(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    handoff_session: &ProviderHandoffSession,
    run: &RunId,
    turn: &QueuedTurn,
    error: String,
) -> Result<(), String> {
    let message = format!(
        "codex turn stopped because its admitted provider context is invalid (movement seq {}): {error}",
        turn.seq
    );
    if author_failed_provider_source(ctx, descriptor, handoff_session, run, turn, &message, false).await? {
        return Ok(());
    }
    if let Err(error) = hand_failure_into_standing_route(ctx, run, turn.seq, &message).await {
        tracing::warn!(
            interface = ctx.interface.as_str(),
            run = run.as_str(),
            seq = turn.seq,
            error = %error,
            "codex failed provider source has no proven standing route into its run"
        );
        return Err(message);
    }
    Ok(())
}

pub(crate) async fn hand_failure_into_standing_route(
    ctx: &InterfaceLiftContext,
    run: &RunId,
    source_movement_seq: u64,
    message: &str,
) -> Result<(), String> {
    let path = ctx.root_slot("run_records_root")?.join(format!("{}.json", run.as_str()));
    let bytes = octopus_interfaces::read_artifact_carrier_file_bounded(&path, ctx.required_host_io_limits()?.max_payload_bytes)
        .map_err(|error| format!("provider run '{}' current record cannot be read: {error}", run.as_str()))?;
    let record = octopus_run_record::RunRecord::from_bytes(&bytes).map_err(|error| error.to_string())?;
    if record.id != *run {
        return Err(format!("provider run record '{}' names run '{}'", run.as_str(), record.id.as_str()));
    }
    octopus_run_authoring::standing_route_admits_only(&record, &ctx.interface, crate::handler::TURN_ERROR_LABEL)?;
    match ctx.hand_arrival(materialize_failed_source_error(ctx, source_movement_seq, message)?, Vec::new()).await? {
        octopus_ledger::DeliveryVerdict::Admitted { .. } => Ok(()),
        octopus_ledger::DeliveryVerdict::Refused { reason, detail } => Err(format!(
            "its turn.error was refused as '{}': {detail}",
            reason.as_str()
        )),
    }
}

pub(crate) fn materialize_failed_source_error(
    ctx: &InterfaceLiftContext,
    source_movement_seq: u64,
    message: &str,
) -> Result<octopus_interfaces::InterfaceArtifactFile, String> {
    crate::handler::materialize_terminal_turn_error_payload(
        &ctx.interface_var_dir().join("emitted"), message, None,
        serde_json::json!({"source_movement_seq": source_movement_seq}),
    ).map_err(
        |failure| {
            format!(
                "failed provider source {source_movement_seq} could not be materialized as turn.error: {failure}"
            )
        },
    )
}

pub(crate) async fn author_failed_provider_source(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    handoff_session: &ProviderHandoffSession,
    run: &RunId,
    turn: &QueuedTurn,
    message: &str,
    restore_consumed: bool,
) -> Result<bool, String> {
    let unavailable = match handoff_session.failed_source_handoff(
        ctx,
        descriptor,
        run,
        &turn.envelope,
        turn.seq,
    ) {
        Ok(handoff) => match handoff
            .hand_failed_source(ProviderEmission {
                artifact: materialize_consumed_source_outcome(ctx, descriptor, turn, message,
                    restore_consumed && handoff.consumes_context_free_independent_input().await)?,
                route: ProviderEmissionRoute::ClientProjection,
            })
            .await
        {
            Ok(_) => return Ok(true),
            Err(FailedSourceHandoffError::Interrupted(error)) => return Err(error),
            Err(FailedSourceHandoffError::Unavailable(error)) => error,
        },
        Err(error) => error,
    };
    tracing::warn!(
        interface = ctx.interface.as_str(),
        run = run.as_str(),
        seq = turn.seq,
        error = %unavailable,
        "codex failed provider source could not be authored into the run"
    );
    Ok(false)
}

pub(crate) fn materialize_consumed_source_outcome(
    ctx: &InterfaceLiftContext, descriptor: &InterfaceDescriptor, turn: &QueuedTurn, message: &str, context_free_independent: bool,
) -> Result<octopus_interfaces::InterfaceArtifactFile, String> {
    if context_free_independent {
        let manifest = turn.envelope.read_artifact_manifest().map_err(|error| error.to_string())?;
        if !is_inflight_turn_control_candidate_label(&manifest.label, descriptor) {
            return crate::handler::materialize_independent_input_receipt(
                &ctx.interface_var_dir().join("emitted"), turn.seq, &manifest.label, &turn.envelope.artifact,
            ).map_err(|error| error.to_string());
        }
    }
    materialize_failed_source_error(ctx, turn.seq, message)
}

pub(crate) async fn restore_consumed_provider_inputs(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    handoff_session: &ProviderHandoffSession,
    deferred_turns: &BTreeMap<u64, DeferredProviderTurn>,
    interrupted_effects: &InterruptedEffectArbiter,
    committed_before_seq: u64,
) -> Result<(), String> {
    let Ok(consumed) = handoff_session.consumed_context_input_sources(ctx, descriptor) else {
        return Ok(());
    };
    for (run, source) in consumed {
        if source >= committed_before_seq
            || deferred_turns.values().any(|deferred| deferred.run == run)
            || !matches!(interrupted_effects.durable_continuity(&run).await, Ok(None))
        {
            continue;
        }
        let Ok(Some(turn)) = replay_provider_source(ctx, &run, source) else {
            continue;
        };
        let message = format!(
            "codex turn ended without a projected outcome (movement seq {source}); its input checkpoint was restored and the message can be sent again"
        );
        author_failed_provider_source(ctx, descriptor, handoff_session, &run, &turn, &message, true)
            .await?;
    }
    Ok(())
}

pub(crate) async fn recover_pending_intent_control(
    ctx: &InterfaceLiftContext, descriptor: &InterfaceDescriptor, session: &ProviderHandoffSession,
    run: &RunId, broker: &CodexTurnControlBroker, transport: Option<&crate::handler::stopped_result::PendingIntentTransport>, control: &CodexTurnControl,
) -> Result<Option<CodexTurnControlResult>, String> {
    let (Some(source), Some(transport)) = (broker.source_movement_seq(), transport) else { return Ok(None); };
    if !matches!(control, CodexTurnControl::Interrupt { .. }) { return Ok(None); }
    let original = replay_provider_source(ctx, run, source)?.ok_or("recorded Stop lost its admitted original source")?;
    let pinned = ctx.pinned_run_record_bytes(&original.envelope)?;
    let handoff = wait_for_provider_handoff(ctx, descriptor, session, run, &original.envelope, source, &pinned).await?;
    let Some(intent) = handoff.pending_source_intent().await? else { return Ok(None); };
    let proof = crate::handler::stopped_result::prove_intent(descriptor, &original.envelope, intent, &handoff.current_session_context_artifacts().await).map_err(|error| error.to_string())?;
    transport.interrupted_control(&proof, control).await.map(Some).map_err(|error| error.to_string())
}

pub(crate) async fn withdraw_pending_source_control(
    ctx: &InterfaceLiftContext, descriptor: &InterfaceDescriptor, handoff_session: &ProviderHandoffSession,
    run: &RunId, broker: &Arc<CodexTurnControlBroker>, pending_transport: Option<crate::handler::stopped_result::PendingIntentTransport>,
    envelope: &InterfaceInvocationEnvelope, source_movement_seq: u64, result: &mut CodexTurnControlResult,
) -> Result<(), String> {
    let control_id = result.control_id.clone();
    let action = result.action.clone();
    let saved_source = result.detail.get("pending_source_withdrawal").map(|saved|
        saved["source_movement_seq"].as_u64().ok_or("saved withdrawal has no exact original source")).transpose()?;
    if saved_source.is_some() {
        let ack = materialize_turn_control_result(&ctx.interface_var_dir().join("emitted"), source_movement_seq, result.clone()).map_err(|error| error.to_string())?;
        if await_artifact_admission_scan(ctx, run, source_movement_seq, &materialized_artifact_ref(&ack)?).await?.movement_seq.is_some() { return Ok(()); }
    }
    if let Some(source) = saved_source.or(broker.source_movement_seq()) {
        if broker.source_movement_seq().is_some_and(|active| active != source) { return Err("saved withdrawal changed its active original source".into()); }
        let original = replay_provider_source(ctx, run, source)?.ok_or("cold Stop lost its exact original result source")?;
        let pinned = ctx.pinned_run_record_bytes(&original.envelope)?;
        let original_handoff = wait_for_provider_handoff(ctx, descriptor, handoff_session, run, &original.envelope, source, &pinned).await?;
        let intent = original_handoff.pending_source_intent().await?;
        let saved = result.detail.get("pending_source_withdrawal").cloned();
        if intent.is_some() || saved.is_some() {
            let (closure, intent) = if let Some(saved) = saved {
                let file = octopus_interfaces::InterfaceArtifactFile::new(std::path::PathBuf::from(saved["manifest_path"].as_str().ok_or("saved withdrawal lost its private artifact")?)).map_err(|error| error.to_string())?;
                let path = file.manifest_path().canonicalize().map_err(|error| error.to_string())?;
                if !path.starts_with(ctx.interface_var_dir().canonicalize().map_err(|error| error.to_string())?)
                    || serde_json::to_value(materialized_artifact_ref(&file)?).map_err(|error| error.to_string())? != saved["artifact"]
                    || saved["source_movement_seq"] != source
                { return Err("saved withdrawal changed its exact private closure".into()); }
                (ProviderEmission { artifact: file, route: ProviderEmissionRoute::ClientProjection }, intent)
            } else {
                let intent = intent.ok_or("withdrawal lost its installed intent")?;
                let proof = crate::handler::stopped_result::prove_intent(descriptor, &original.envelope, intent, &original_handoff.current_session_context_artifacts().await)
                    .map_err(|error| error.to_string())?;
                let closure = pending_transport.ok_or("cold Stop has no exact retained native transport")?.closure(&proof, &original.envelope,
                    source_movement_seq, &envelope.artifact, &result, &ctx.interface_var_dir().join("emitted"),
                    &codex_context_output_labels(descriptor).map_err(|error| error.to_string())?).await.map_err(|error| error.to_string())?;
                result.detail["pending_source_withdrawal"] = serde_json::json!({
                    "source_movement_seq":source,"artifact":materialized_artifact_ref(&closure.artifact)?,
                    "manifest_path":closure.artifact.manifest_path(),
                });
                persist_control_journal(ctx, source_movement_seq, run, &control_id, &action, TurnControlJournalPhase::Result { result: result.clone() })?;
                (closure, Some(proof.intent))
            };
            let value: serde_json::Value = serde_json::from_slice(&closure.artifact.read_payload_bytes().map_err(|error| error.to_string())?).map_err(|error| error.to_string())?;
            crate::handler::stopped_result::validate_withdrawal_closure(&value, &original.envelope, source, &envelope.artifact, source_movement_seq, &result)?;
            if let Some(intent) = intent {
                let proof = crate::handler::stopped_result::prove_intent(descriptor, &original.envelope, intent.clone(), &original_handoff.current_session_context_artifacts().await)
                    .map_err(|error| error.to_string())?;
                let control = decode_admitted_turn_control(ctx, descriptor, envelope).map_err(|error| error.to_string())?.ok_or("withdrawal lost its admitted control")?;
                crate::handler::stopped_result::validate_saved_intent_closure(&proof, &original.envelope, &value, &envelope.artifact, source_movement_seq, &control,
                    &codex_context_output_labels(descriptor).map_err(|error| error.to_string())?)?;
                validate_saved_source_withdrawal(ctx, descriptor, run, &original, &original_handoff).await?;
                match original_handoff.hand_source_withdrawal(&intent, source_movement_seq, &envelope.artifact, closure).await? {
                    ProviderHandoffOutcome::Admitted { .. } => {},
                    ProviderHandoffOutcome::TerminalFailure { failure_class, .. } => return Err(format!("pending intent withdrawal failed: {failure_class}")),
                }
            } else {
                validate_saved_source_withdrawal(ctx, descriptor, run, &original, &original_handoff).await?;
                if original_handoff.pending_source_withdrawal().await?.is_some() {
                    let (recovery, outcomes) = original_handoff.recover_source_with_outcomes().await?;
                    if recovery != ProviderSourceRecovery::Completed || outcomes.len() != 1 {
                        return Err("saved withdrawal has no exact checked completed closure".into());
                    }
                }
                original_handoff.validate_source_withdrawal(source_movement_seq, &envelope.artifact).await?;
            }
            let mut continuity = crate::handler::stopped_result::closure_continuity(&value, &ctx.interface, run.as_str())?;
            continuity.advance_control_context(descriptor)?;
            result.continuity = Some(continuity);
            persist_control_journal(ctx, source_movement_seq, run, &control_id, &action, TurnControlJournalPhase::Result { result: result.clone() })?;
            broker.finish();
        }
    }
    Ok(())
}

pub(crate) async fn retain_completed_source_control(
    ctx: &InterfaceLiftContext, descriptor: &InterfaceDescriptor, auth: &CodexRuntimeAuth,
    driver: &mut Option<Arc<CodexAppServerInterfaceDriver>>, control: &CodexTurnControlBroker,
    interrupted: &InterruptedEffectArbiter, run: &RunId, turn: &QueuedTurn, handoff: &ProviderOutputHandoff,
) -> Result<(), String> {
    if let Some(continuity) = interrupted.durable_continuity(run).await? {
        return control.publish_recovered_interrupted_pending(&continuity);
    }
    let continuity = crate::handler::completed_source_pending_continuity(descriptor, &turn.envelope,
        &handoff.current_run_record_bytes().await, &handoff.current_session_context_artifacts().await)
        .map_err(|error| error.to_string())?;
    if let Some(continuity) = continuity {
        if driver.is_none() {
            *driver = Some(connect_declared_interface_driver(ctx, descriptor, auth, Some(&turn.envelope)).await.map_err(|error| error.message)?);
        }
        driver.as_ref().expect("connected recovery transport").restore_pending_control(control, continuity).await;
    } else { control.finish(); }
    Ok(())
}

pub(crate) async fn restore_completed_provider_sources(
    ctx: &InterfaceLiftContext, descriptor: &InterfaceDescriptor, session: &ProviderHandoffSession,
    auth: &CodexRuntimeAuth, driver: &mut Option<Arc<CodexAppServerInterfaceDriver>>,
    active_controls: &ActiveTurnControls, interrupted: &InterruptedEffectArbiter, committed_before: u64,
) -> Result<(), String> {
    for (run, source) in session.completed_source_candidates(ctx)? {
        let turn = replay_provider_source(ctx, &run, source)?.ok_or("completed result source lost its original admitted input")?;
        let pinned = ctx.pinned_run_record_bytes(&turn.envelope)?;
        let handoff = match wait_for_provider_handoff(ctx, descriptor, session, &run, &turn.envelope, source, &pinned).await {
            Ok(handoff) => handoff,
            Err(error) if ctx.cancel.is_cancelled() => return Err(error),
            Err(error) => {
                let message = format!(
                    "codex run contract cannot be read for its completed turn (movement seq {source}): {error}"
                );
                if let Err(unreported) = hand_failure_into_standing_route(ctx, &run, source, &message).await {
                    tracing::warn!(
                        interface = ctx.interface.as_str(),
                        run = run.as_str(),
                        seq = source,
                        error = %message,
                        unreported = %unreported,
                        "completed provider source has an unreadable run contract"
                    );
                }
                continue;
            }
        };
        if let Some(intent) = handoff.pending_source_intent().await? {
            let continuity = crate::handler::stopped_result::intent_control_continuity(descriptor, &turn.envelope, intent, &handoff.current_session_context_artifacts().await)
                .map_err(|error| error.to_string())?;
            if driver.is_none() {
                *driver = Some(connect_declared_interface_driver(ctx, descriptor, auth, Some(&turn.envelope)).await.map_err(|error| error.message)?);
            }
            let broker = CodexTurnControlBroker::new(Some(source));
            driver.as_ref().expect("connected pending recovery transport").restore_pending_control(&broker, continuity).await;
            active_controls.lock().await.insert(run.to_string(), broker);
            continue;
        }
        validate_saved_source_withdrawal(ctx, descriptor, &run, &turn, &handoff).await?;
        if handoff.pending_source_withdrawal().await?.is_some() {
            let broker = CodexTurnControlBroker::new(Some(source));
            broker.finish();
            active_controls.lock().await.insert(run.to_string(), broker);
            continue;
        }
        if source >= committed_before { continue; }
        let (recovery, outcomes) = match handoff.recover_completed_source_with_outcomes().await {
            Ok(recovered) => recovered,
            Err(error) if ctx.cancel.is_cancelled() => return Err(error),
            Err(error) => {
                let message = format!("completed provider output could not be recovered (movement seq {source}): {error}");
                if let Err(unreported) = record_invalid_provider_context(ctx, descriptor, session, &run, &turn, message.clone()).await {
                    if ctx.cancel.is_cancelled() { return Err(unreported); }
                    tracing::warn!(interface = ctx.interface.as_str(), run = run.as_str(), seq = source,
                        error = %message, unreported = %unreported, "completed provider source remains failed");
                }
                continue;
            }
        };
        if matches!(recovery, ProviderSourceRecovery::Bridged { .. }) { continue; }
        if try_settle_completed_interrupted_result(ctx, descriptor, &run, &turn, &handoff, interrupted).await? { continue; }
        if recovery != ProviderSourceRecovery::Completed { continue; }
        let record = handoff.current_run_record_bytes().await;
        let context = handoff.current_session_context_artifacts().await;
        let Some(proof) = crate::handler::stopped_result::prove(descriptor, &turn.envelope, source, &record, &context, &outcomes)
            .map_err(|error| error.to_string())? else {
                let broker = CodexTurnControlBroker::new(Some(source));
                retain_completed_source_control(ctx, descriptor, auth, driver, &broker, interrupted, &run, &turn, &handoff).await?;
                if !broker.is_finished() { active_controls.lock().await.insert(run.to_string(), broker); }
                continue;
            };
        validate_stopped_result_control(ctx, descriptor, &run, &proof)?;
        if driver.is_none() {
            *driver = Some(connect_declared_interface_driver(ctx, descriptor, auth, Some(&turn.envelope)).await.map_err(|error| error.message)?);
        }
        let closure = driver.as_ref().expect("connected recovery transport").stopped_result_settlement(
            &proof, &turn.envelope, &ctx.interface_var_dir().join("emitted"),
            &codex_context_output_labels(descriptor).map_err(|error| error.to_string())?,
        ).await.map_err(|error| error.to_string())?;
        let expected: serde_json::Value = serde_json::from_slice(&closure.artifact.read_payload_bytes().map_err(|error| error.to_string())?).map_err(|error| error.to_string())?;
        match handoff.hand_completed_source_settlement(&proof.predecessor, proof.context_generation(), closure).await? {
            ProviderHandoffOutcome::Admitted { seq, artifact } => {
                let (manifest, bytes) = read_pinned_provider_artifact(ctx, &artifact, "stopped result settlement")?;
                let mut actual: serde_json::Value = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
                actual.as_object_mut().ok_or("stopped result settlement is not an object")?.remove("session_context");
                if manifest.label != TURN_INTERRUPTED_RESULT_LABEL || actual != expected {
                    return Err("stopped result admitted a different semantic closure".into());
                }
                validate_artifact_admission(ctx, &run, source, &DurableArtifactAdmission { artifact, movement_seq: seq })?;
            }
            ProviderHandoffOutcome::TerminalFailure { failure_seq, failure_class, .. } => {
                return Err(format!("stopped result settlement failed: {failure_class} at {failure_seq}"));
            }
        }
    }
    Ok(())
}

pub(crate) async fn try_settle_completed_interrupted_result(
    ctx: &InterfaceLiftContext, descriptor: &InterfaceDescriptor, run: &RunId, turn: &QueuedTurn,
    handoff: &ProviderOutputHandoff, interrupted: &InterruptedEffectArbiter,
) -> Result<bool, String> {
    let record: serde_json::Value = serde_json::from_slice(&handoff.current_run_record_bytes().await).map_err(|error| error.to_string())?;
    let pending = &record["interface_state"][ctx.interface.as_str()]["pending_handoff"];
    if pending["source_movement_seq"] != turn.seq || pending["terminal"] != true
        || (pending["settlement_of"].is_null() && pending["items"][0]["artifact_label"] != "turn.error") { return Ok(false); }
    let Some(journal) = interrupted.journal(run).await? else { return Ok(false); };
    if !matches!(&journal.phase,
        InterruptedEffectPhase::Resolving { result_source_movement_seq, .. }
        | InterruptedEffectPhase::Closing { result_source_movement_seq, .. } if *result_source_movement_seq == turn.seq)
        || !crate::handler::stopped_result::has_native_interrupted_result(&journal.continuity)? { return Ok(false); }
    settle_completed_interrupted_result(ctx, descriptor, run, turn, handoff, interrupted, &journal).await?;
    Ok(true)
}

pub(crate) async fn settle_completed_interrupted_result(
    ctx: &InterfaceLiftContext, descriptor: &InterfaceDescriptor, run: &RunId, turn: &QueuedTurn,
    handoff: &ProviderOutputHandoff, interrupted: &InterruptedEffectArbiter, journal: &InterruptedEffectJournal,
) -> Result<(), String> {
    let saved = handoff.pending_source_settlement().await?
        .ok_or("completed interrupted result has no checked settlement predecessor")?;
    let [ProviderHandoffOutcome::Admitted { seq, artifact }] = saved.predecessor_outcomes.as_slice()
        else { return Err("completed interrupted result requires one admitted predecessor".into()); };
    if artifact != &saved.predecessor { return Err("completed interrupted predecessor ref changed".into()); }
    let context = handoff.current_session_context_artifacts().await.iter()
        .filter(|entry| entry.source_movement_seq.is_some_and(|value| value <= *seq)).cloned().collect::<Vec<_>>();
    let proof = prove_completed_interrupted_result(ctx, descriptor, run, turn, journal,
        &saved.predecessor_record, &context, &saved.predecessor_outcomes)?;
    if let Some(closure) = saved.closure {
        let InterruptedEffectPhase::Closing { closure_artifact, .. } = &journal.phase else {
            return Err("saved interrupted settlement is outside its exact Closing phase".into());
        };
        let value: serde_json::Value = serde_json::from_slice(&closure.read_payload_bytes().map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
        let labels = codex_context_output_labels(descriptor).map_err(|error| error.to_string())?;
        crate::handler::stopped_result::validate_saved_settlement(&proof, &turn.envelope, &value, &labels)?;
        let original = octopus_interfaces::InterfaceArtifactFile::new(ctx.interface_var_dir().join("emitted")
            .join(format!("turn_interrupted_result-{}.artifact.json", closure_artifact.content.to_hex())))
            .map_err(|error| error.to_string())?;
        let limits = ctx.required_host_io_limits()?;
        let original_bytes = original.read_bounded(limits.max_manifest_bytes, limits.max_payload_bytes).map_err(|error| error.to_string())?;
        if materialized_artifact_ref(&original)? != *closure_artifact { return Err("interrupted Closing manifest pin changed".into()); }
        let original: serde_json::Value = serde_json::from_slice(&original_bytes.payload_bytes).map_err(|error| error.to_string())?;
        let mut candidate = value.clone();
        candidate.as_object_mut().ok_or("saved interrupted settlement is not an object")?.remove("session_context");
        if candidate != original { return Err("saved interrupted settlement changed its exact Closing closure".into()); }
        let (recovery, outcomes) = handoff.recover_completed_source_settlement(&proof.predecessor, &materialized_artifact_ref(&closure)?).await?;
        if matches!(recovery, ProviderSourceRecovery::Bridged { .. }) { return Ok(()); }
        if recovery != ProviderSourceRecovery::Completed { return Err("saved interrupted settlement did not complete its checked source".into()); }
        let admission = interrupted_result_admission(&outcomes)?.ok_or("saved interrupted settlement has no admitted closure")?;
        let (_, bytes) = read_pinned_provider_artifact(ctx, &admission.artifact, "recovered interrupted settlement")?;
        let admitted: serde_json::Value = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
        if admitted != value { return Err("saved interrupted settlement admitted a different snapshot".into()); }
        interrupted.complete_closing(run, turn.seq, closure_artifact, &admission).await
    } else {
        hand_completed_interrupted_result(ctx, descriptor, run, turn, handoff, interrupted, &proof).await
    }
}

pub(crate) fn prove_completed_interrupted_result(
    ctx: &InterfaceLiftContext, descriptor: &InterfaceDescriptor, run: &RunId, turn: &QueuedTurn,
    journal: &InterruptedEffectJournal, record: &[u8],
    context: &[octopus_run_authoring::SessionContextArtifact], outcomes: &[ProviderHandoffOutcome],
) -> Result<crate::handler::stopped_result::StoppedResultSettlement, String> {
    match &journal.phase {
        InterruptedEffectPhase::Resolving { result_source_movement_seq, source_artifact }
            if *result_source_movement_seq == turn.seq && source_artifact == &turn.envelope.artifact => {}
        InterruptedEffectPhase::Closing { result_source_movement_seq, .. }
        | InterruptedEffectPhase::Reconciled { result_source_movement_seq, .. }
            if *result_source_movement_seq == turn.seq => {}
        _ => return Err("completed interrupted settlement changed its durable result coordinate".into()),
    }
    let proof = crate::handler::stopped_result::prove_interrupted(descriptor, &turn.envelope, turn.seq,
        record, context, outcomes, &crate::handler::stopped_result::InterruptedResultSource {
            continuity: &journal.continuity, control_source: journal.control_source_movement_seq,
            control_id: &journal.control_id, acknowledgement: &journal.acknowledgement.artifact,
            acknowledgement_seq: journal.acknowledgement.movement_seq,
        }).map_err(|error| error.to_string())?;
    validate_completed_interrupted_result_control(ctx, descriptor, run, &journal.continuity, &proof)?;
    Ok(proof)
}

pub(crate) fn validate_completed_interrupted_result_control(
    ctx: &InterfaceLiftContext, descriptor: &InterfaceDescriptor, run: &RunId,
    continuity: &crate::handler::CodexContinuityState, proof: &crate::handler::stopped_result::StoppedResultSettlement,
) -> Result<(), String> {
    validate_stopped_result_control(ctx, descriptor, run, &proof)?;
    let control = load_control_journal(ctx, proof.control_source, run, &proof.control_id, &proof.action)?
        .ok_or("completed interrupted settlement lost its durable actual Stop")?;
    let result = match control.phase {
        TurnControlJournalPhase::Result { result } | TurnControlJournalPhase::Delivered { result } => result,
        _ => return Err("completed interrupted settlement has no durable Stop result".into()),
    };
    let (_, bytes) = read_pinned_provider_artifact(ctx, &proof.acknowledgement, "interrupted settlement ACK")?;
    let ack: serde_json::Value = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    if !result.acknowledged || result.control_id != proof.control_id || result.action != proof.action
        || result.continuity.as_ref() != Some(continuity)
        || ack["thread_id"] != serde_json::json!(result.thread_id)
        || ack["turn_id"] != serde_json::json!(result.turn_id) || ack["detail"] != result.detail
    { return Err("completed interrupted settlement ACK conflicts with its durable Stop result".into()); }
    Ok(())
}

pub(crate) async fn hand_completed_interrupted_result(
    ctx: &InterfaceLiftContext, descriptor: &InterfaceDescriptor, run: &RunId, turn: &QueuedTurn,
    handoff: &ProviderOutputHandoff, interrupted: &InterruptedEffectArbiter,
    proof: &crate::handler::stopped_result::StoppedResultSettlement,
) -> Result<(), String> {
    let closure = crate::handler::stopped_result::materialize_interrupted(proof, &turn.envelope,
        &ctx.interface_var_dir().join("emitted"), &codex_context_output_labels(descriptor).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())?;
    let reference = materialized_artifact_ref(&closure.artifact)?;
    let expected: serde_json::Value = serde_json::from_slice(&closure.artifact.read_payload_bytes().map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())?;
    interrupted.begin_closing(run, turn.seq, reference.clone()).await?;
    match handoff.hand_completed_source_settlement(&proof.predecessor, proof.context_generation(), closure).await? {
        ProviderHandoffOutcome::Admitted { seq, artifact } => {
            let (manifest, bytes) = read_pinned_provider_artifact(ctx, &artifact, "completed interrupted settlement")?;
            let mut value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
            value.as_object_mut().ok_or("completed interrupted settlement is not an object")?.remove("session_context");
            if manifest.label != TURN_INTERRUPTED_RESULT_LABEL || value != expected {
                return Err("completed interrupted settlement admitted a different semantic closure".into());
            }
            interrupted.complete_closing(run, turn.seq, &reference, &DurableArtifactAdmission { artifact, movement_seq: seq }).await
        }
        ProviderHandoffOutcome::TerminalFailure { failure_seq, failure_class, .. } =>
            Err(format!("completed interrupted settlement failed: {failure_class} at {failure_seq}")),
    }
}

pub(crate) async fn validate_saved_source_withdrawal(
    ctx: &InterfaceLiftContext, descriptor: &InterfaceDescriptor, run: &RunId,
    original: &QueuedTurn, handoff: &ProviderOutputHandoff,
) -> Result<(), String> {
    let Some(saved) = handoff.pending_source_withdrawal().await? else { return Ok(()); };
    let control = replay_provider_source(ctx, run, saved.control_source)?.ok_or("saved withdrawal lost its admitted control input")?;
    if control.envelope.artifact != saved.control { return Err("saved withdrawal control ArtifactRef changed".into()); }
    let decoded = decode_admitted_turn_control(ctx, descriptor, &control.envelope).map_err(|error| error.to_string())?
        .ok_or("saved withdrawal input is not a declared control")?;
    let context = handoff.current_session_context_artifacts().await;
    let prior = context.iter().filter(|entry| entry.source_movement_seq.is_some_and(|seq| seq < original.seq)).cloned().collect::<Vec<_>>();
    let proof = crate::handler::stopped_result::prove_intent(descriptor, &original.envelope, saved.intent, &prior).map_err(|error| error.to_string())?;
    let value: serde_json::Value = serde_json::from_slice(&saved.closure.read_payload_bytes().map_err(|error| error.to_string())?).map_err(|error| error.to_string())?;
    crate::handler::stopped_result::validate_saved_intent_closure(&proof, &original.envelope, &value, &saved.control, saved.control_source, &decoded,
        &codex_context_output_labels(descriptor).map_err(|error| error.to_string())?)?;
    handoff.validate_source_withdrawal_output(&materialized_artifact_ref(&saved.closure)?).await
}

pub(crate) fn validate_stopped_result_control(
    ctx: &InterfaceLiftContext, descriptor: &InterfaceDescriptor, run: &RunId,
    proof: &crate::handler::stopped_result::StoppedResultSettlement,
) -> Result<(), String> {
    validate_artifact_admission(ctx, run, proof.control_source, &DurableArtifactAdmission {
        artifact: proof.acknowledgement.clone(), movement_seq: proof.acknowledgement_seq,
    })?;
    let control = replay_provider_source(ctx, run, proof.control_source)?.ok_or("stopped result lost its admitted Stop input")?;
    let decoded = decode_admitted_turn_control(ctx, descriptor, &control.envelope).map_err(|error| error.to_string())?
        .ok_or("stopped result source is not a declared control")?;
    if control_id(&decoded) != proof.control_id || control_action(&decoded) != proof.action {
        return Err("stopped result acknowledgement changed its exact admitted control".into());
    }
    Ok(())
}

pub(crate) fn replay_provider_source(
    ctx: &InterfaceLiftContext,
    run: &RunId,
    source_movement_seq: u64,
) -> Result<Option<QueuedTurn>, String> {
    let mut replay = LedgerReplayReader::open(ctx)?;
    while let Some(batch) = replay.read_batch(PROVIDER_CONTROL_READ_BATCH_LIMIT)? {
        for own in batch {
            if own.movement.seq.0 < source_movement_seq {
                continue;
            }
            if own.movement.seq.0 > source_movement_seq
                || own.metadata()?.runtime.as_ref().map(|runtime| &runtime.run) != Some(run)
            {
                return Ok(None);
            }
            return Ok(Some(QueuedTurn {
                envelope: replay.envelope(&own)?,
                seq: source_movement_seq,
            }));
        }
    }
    Ok(None)
}

pub(crate) async fn backoff_or_give_up(
    ctx: &InterfaceLiftContext,
    max_attempts: u32,
    attempt: &mut u32,
) -> Result<bool, String> {
    if *attempt >= max_attempts {
        return Ok(false);
    }
    let backoff = TURN_RETRY_BACKOFF * *attempt;
    *attempt += 1;
    tokio::select! {
        _ = ctx.cancel.cancelled() => Err(
            "codex life cancelled before the movement reached an admitted outcome".to_string()
        ),
        _ = tokio::time::sleep(backoff) => Ok(true),
    }
}
