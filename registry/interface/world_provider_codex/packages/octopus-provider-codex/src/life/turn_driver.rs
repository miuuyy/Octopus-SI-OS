use super::*;

pub(crate) fn drive_turn_with_retries<'a>(
    scope: CodexTurnScope<'a>,
    driver: &'a mut Option<Arc<CodexAppServerInterfaceDriver>>,
    turn: QueuedTurn,
) -> std::pin::Pin<Box<impl std::future::Future<Output = Result<(), String>> + 'a>> {
    Box::pin(drive_turn_with_retries_inner(scope, driver, turn))
}

pub(crate) async fn drive_turn_with_retries_inner(
    scope: CodexTurnScope<'_>,
    driver: &mut Option<Arc<CodexAppServerInterfaceDriver>>,
    turn: QueuedTurn,
) -> Result<(), String> {
    let CodexTurnScope {
        ctx,
        descriptor,
        handoff_session,
        runtime_auth,
        run,
        control,
        interrupted_effects,
    } = scope;
    let current_descriptor = load_current_own_descriptor(ctx)?;
    if current_descriptor != *descriptor {
        return Err(format!(
            "codex interface '{}' descriptor changed after this life was lifted; refusing movement {} until a fresh life adopts the exact descriptor",
            ctx.interface.as_str(),
            turn.seq
        ));
    }
    adopt_current_machine_auth_generation(ctx, runtime_auth, driver).await?;
    let pinned_run_record = match ctx.pinned_run_record_bytes(&turn.envelope) {
        Ok(record) => record,
        Err(error) => {
            fail_source_closed(ctx, descriptor, handoff_session, run, &turn, driver, error)
                .await?;
            control.finish();
            return Ok(());
        }
    };
    if let Err(error) = handoff_session.validate_run_placement(ctx, run, &pinned_run_record, turn.seq) {
        let message = format!("codex turn stopped before it began (movement seq {}): {error}", turn.seq);
        hand_failure_into_standing_route(ctx, run, turn.seq, &message).await
            .map_err(|unreported| format!("{message}; {unreported}"))?;
        control.finish();
        return Ok(());
    }
    let handoff = match wait_for_provider_handoff(
        ctx,
        descriptor,
        handoff_session,
        run,
        &turn.envelope,
        turn.seq,
        &pinned_run_record,
    ).await {
        Ok(handoff) => handoff,
        Err(error) if ctx.cancel.is_cancelled() => return Err(error),
        Err(error) => {
            if recover_interrupted_result_without_context(
                ctx, descriptor, handoff_session, interrupted_effects, control, run, &turn,
            ).await? {
                control.finish();
                return Ok(());
            }
            fail_source_closed(ctx, descriptor, handoff_session, run, &turn, driver, error)
                .await?;
            control.finish();
            return Ok(());
        }
    };
    let saved = async {
        validate_saved_source_withdrawal(ctx, descriptor, run, &turn, &handoff).await?;
        handoff.recover_source_with_outcomes().await
    }.await;
    let (source_recovery, recovered_outcomes) = match saved {
        Ok(recovered) => recovered,
        Err(error) if ctx.cancel.is_cancelled() => return Err(error),
        Err(error) => {
            fail_source_closed(ctx, descriptor, handoff_session, run, &turn, driver, error).await?;
            control.finish();
            return Ok(());
        }
    };
    if try_settle_completed_interrupted_result(ctx, descriptor, run, &turn, &handoff, interrupted_effects).await? {
        control.finish();
        return Ok(());
    }
    // Classify exact, completed duplicate results before interruption arbitration:
    // an older result must neither resolve nor cancel its successor's pending effect.
    if crate::handler::correlated_tool_result(&turn.envelope).map_err(|error| error.to_string())?.is_some()
        && let Some(completed_source) = handoff.completed_input_artifact_source(&turn.envelope.artifact).await?
    {
        if source_recovery == ProviderSourceRecovery::Completed { return Ok(()); }
        if source_recovery != ProviderSourceRecovery::Process {
            return Err("duplicate result retains an unfinished source handoff".into());
        }
        handoff.validate_client_output_routes(&codex_projection_output_labels(descriptor).map_err(|error| error.to_string())?)?;
        let receipt = crate::handler::duplicate_tool_result_receipt(
            descriptor, &turn.envelope, turn.seq, completed_source,
            &handoff.current_run_record_bytes().await,
            &handoff.current_session_context_artifacts().await,
            &ctx.interface_var_dir().join("emitted"),
        ).map_err(|error| error.to_string())?;
        match handoff.hand_observation(receipt, true).await? {
            ProviderHandoffOutcome::Admitted { .. } => {
                if let Some(wake) = control.late_call_wake() {
                    wake.record_arrival(turn.seq, &turn.envelope);
                }
                return Ok(());
            }
            ProviderHandoffOutcome::TerminalFailure { failure_seq, failure_class, .. } => {
                return Err(format!("duplicate tool result receipt failed: {failure_class} at {failure_seq}"));
            }
        }
    }
    control.prepare_recovered_result().await?;
    let durable_interrupted_continuity = interrupted_effects.durable_continuity(run).await?;
    let resolving_interrupted_result = interrupted_effects
        .begin_result(run, turn.seq, &turn.envelope)
        .await?;
    let durable_interrupted_continuity = if resolving_interrupted_result {
        let original = durable_interrupted_continuity.as_ref().ok_or("interrupted source lost its durable continuity")?;
        Some(crate::handler::checked_interrupted_context_revision(original, &handoff.current_session_context_artifacts().await,
            &codex_context_output_labels(descriptor).map_err(|error| error.to_string())?, None)?)
    } else { durable_interrupted_continuity };
    let (mut interrupted_after_artifact, mut recovered_handoff_artifacts) = match source_recovery {
        ProviderSourceRecovery::Process => {
            if !recovered_outcomes.is_empty() {
                return Err(format!(
                    "provider source movement {} returned recovery outcomes without an interrupted frontier",
                    turn.seq
                ));
            }
            (None, Arc::new(Vec::new()))
        }
        ProviderSourceRecovery::Completed => {
            interrupted_effects
                .recover_completed_source(run, turn.seq, &recovered_outcomes)
                .await?;
            retain_completed_source_control(ctx, descriptor, runtime_auth, driver, control, interrupted_effects, run, &turn, &handoff).await?;
            return Ok(());
        }
        ProviderSourceRecovery::Interrupted {
            last_artifact_label,
        } => (
            Some(last_artifact_label),
            recovered_codex_handoff_artifacts(ctx, run, turn.seq, &recovered_outcomes).await?,
        ),
    };
    let output_labels = codex_projection_output_labels(descriptor).map_err(|error| error.to_string())?;
    if let Err(error) = handoff.validate_client_output_routes(&output_labels) {
        fail_source_closed(
            ctx, descriptor, handoff_session, run, &turn, driver,
            format!("provider output topology is invalid before dispatch: {error}"),
        ).await?;
        control.finish();
        return Ok(());
    }
    if interrupted_after_artifact.is_some() && !resolving_interrupted_result {
        let record = handoff.current_run_record_bytes().await;
        let session = handoff.current_session_context_artifacts().await;
        if let Some(receipt) = crate::handler::recovered_tool_handoff_receipt(
            descriptor, &turn.envelope, turn.seq, &record, &session, &recovered_outcomes,
            &ctx.interface_var_dir().join("emitted"),
        ).map_err(|error| error.to_string())? {
            match handoff.hand_final(receipt).await? {
                ProviderHandoffOutcome::Admitted { .. } => {
                    control.finish();
                    return Ok(());
                }
                ProviderHandoffOutcome::TerminalFailure { failure_seq, failure_class, .. } => {
                    return Err(format!("recovered tool handoff failed to close its source: {failure_class} at {failure_seq}"));
                }
            }
        }
    }
    if interrupted_after_artifact.is_none()
        && !resolving_interrupted_result
        && handoff.consumes_context_free_independent_input().await
    {
        let manifest = turn
            .envelope
            .read_artifact_manifest()
            .map_err(|error| error.to_string())?;
        if !is_inflight_turn_control_candidate_label(&manifest.label, descriptor) {
            let status = serde_json::from_slice::<serde_json::Value>(
                &handoff.current_run_record_bytes().await,
            )
            .map_err(|error| error.to_string())?;
            let artifact = crate::handler::materialize_independent_input_receipt(
                &ctx.interface_var_dir().join("emitted"),
                turn.seq,
                &manifest.label,
                &turn.envelope.artifact,
            )
            .map_err(|error| {
                format!(
                    "independent input {} receipt failed to materialize: {error}",
                    turn.seq
                )
            })?;
            handoff
                .hand_observation(
                    ProviderEmission {
                        artifact,
                        route: ProviderEmissionRoute::ClientProjection,
                    },
                    status.get("status").and_then(serde_json::Value::as_str) == Some("idle"),
                )
                .await?;
            control.finish();
            return Ok(());
        }
    }
    if resolving_interrupted_result {
        let continuity = durable_interrupted_continuity.as_ref()
            .ok_or("interrupted source lost its durable continuity")?;
        if let Some((emissions, closure)) = recover_sealed_interrupted_result(
            ctx, descriptor, &turn, continuity,
        )? {
            admit_interrupted_result(ctx, &handoff, control, Some(interrupted_effects), run, turn.seq, &closure, emissions).await?;
            control.finish();
            return Ok(());
        }
        if let Some(emissions) = crate::handler::stopped_result::materialize_pending_interrupted(
            descriptor, continuity, &turn.envelope, turn.seq, &ctx.interface_var_dir().join("emitted"), &ctx.interface_var_dir().join("streams"),
            handoff.input_changes_context().await,
        ).map_err(|error| error.to_string())? {
            let reference = materialized_artifact_ref(&emissions.last().ok_or("interrupted result group has no closure")?.artifact)?;
            admit_interrupted_result(ctx, &handoff, control, Some(interrupted_effects), run, turn.seq, &reference, emissions).await?;
            return Ok(());
        }
    }
    let native_terminal_source = if resolving_interrupted_result { None } else {
        let snapshot = handoff.invocation_session_snapshot();
        let current_session = handoff.current_session_context_artifacts().await;
        recover_provider_result_continuity(descriptor, &turn.envelope, snapshot.as_ref(), &current_session, run, &pinned_run_record)
            .map_err(|error| error.to_string())?
    };
    if let Some(source) = native_terminal_source.as_ref() {
        if let Some((emissions, closure)) = recover_sealed_interrupted_result(ctx, descriptor, &turn, source)? {
            admit_interrupted_result(ctx, &handoff, control, None, run, turn.seq, &closure, emissions).await?;
            control.finish();
            return Ok(());
        }
    }
    if close_sealed_terminal_error(ctx, descriptor, handoff_session, run, &turn, &handoff).await? {
        control.finish();
        return Ok(());
    }
    let DispatchAdmission { callable_tool_names, max_attempts } = match admit_dispatch(descriptor, &turn.envelope, &handoff)? {
        Ok(admission) => admission,
        Err(DispatchRefusal { malformed_policy, message }) => {
            if let Some((field, error)) = malformed_policy {
                ctx.hand_admitted_failure(
                    turn.seq,
                    "malformed_provider_policy",
                    serde_json::json!({ "field": field, "error": error }),
                )
                .await?;
            }
            control.finish();
            return emit_terminal_turn_error(ctx, descriptor, handoff_session, run, &turn, &handoff, 0, &message).await;
        }
    };
    let mut attempt: u32 = 1;
    loop {
        if ctx.cancel.is_cancelled() {
            return Err(
                "codex life cancelled before the movement reached an admitted outcome".to_string(),
            );
        }
        if driver.is_none() {
            match connect_declared_interface_driver(
                ctx,
                descriptor,
                runtime_auth,
                Some(&turn.envelope),
            )
            .await
            {
                Ok(connected) => *driver = Some(connected),
                Err(error) => {
                    tracing::warn!(
                        interface = ctx.interface.as_str(),
                        run = run.as_str(),
                        seq = turn.seq,
                        attempt,
                        error = %error,
                        "codex transport connect failed"
                    );
                    if !error.retryable {
                        emit_terminal_turn_error(
                            ctx,
                            descriptor,
                            handoff_session,
                            run,
                            &turn,
                            &handoff,
                            0,
                            &error.message,
                        )
                        .await?;
                        control.finish();
                        return Ok(());
                    }
                    if !backoff_or_give_up(ctx, max_attempts, &mut attempt).await? {
                        emit_terminal_turn_error(
                            ctx,
                            descriptor,
                            handoff_session,
                            run,
                            &turn,
                            &handoff,
                            max_attempts,
                            &error.message,
                        )
                        .await?;
                        control.finish();
                        return Ok(());
                    }
                    continue;
                }
            }
        }
        let live = driver.as_ref().expect("driver connected above").clone();
        control.publish_starting();

        let (output_stream, emit_rx) = ProviderOutputStream::channel();
        let handoff_barrier = ProviderHandoffBarrier::new();
        let (terminal_tx, terminal_rx) = tokio::sync::oneshot::channel::<bool>();
        let forwarder = Box::pin(forward_provider_emissions(
            EmissionForwarding {
                ctx,
                descriptor,
                control,
                handoff: handoff.clone(),
                interrupted_effects: interrupted_effects.clone(),
                durable_interrupted_continuity: durable_interrupted_continuity.clone(),
                native_terminal_source: native_terminal_source.clone(),
                source_envelope: turn.envelope.clone(),
                run: run.clone(),
                source_artifact: turn.envelope.artifact.clone(),
                result_source_movement_seq: turn.seq,
                resolving_interrupted_result,
            },
            emit_rx,
            terminal_rx,
        ));

        let output_root = ctx.interface_var_dir().join("emitted");
        let live_stream_root = ctx.interface_var_dir().join("streams");
        let current_run_record = handoff.current_run_record_bytes().await;
        let current_session_context = handoff.current_session_context_artifacts().await;
        let reconciled_result_continuity = match is_provider_turn_input(descriptor, &turn.envelope) {
            Ok(true) => {
            let snapshot = bound_operator_session_snapshot(ctx, descriptor, &turn)?;
            let bound = recover_provider_turn_continuity(descriptor, &turn.envelope,
                snapshot.as_ref().map(|(_, snapshot)| snapshot), run, &pinned_run_record)
                .map_err(|error| error.to_string())?;
            let deferred = load_deferred_provider_turns(ctx, descriptor)?;
            let release = deferred.get(&turn.seq).and_then(|turn| turn.release.as_ref())
                .filter(|release| matches!(release, DeferredProviderTurnRelease::InterruptedResult { .. }));
            checked_deferred_interrupted_result(ctx, descriptor, run, &turn, bound.as_ref(), release,
                Some(&current_session_context)).await?.map(|(_, continuity)| continuity)
            }
            Ok(false) | Err(_) => None,
        };
        let input_changes_context = handoff.input_changes_context().await;
        let deferred_context_generations = handoff.deferred_context_generations().await;
        let receive = Box::pin(async {
            let result = live
                .receive_artifact_and_stream_outputs(CodexProviderInvocation {
                    callable_tool_names: callable_tool_names.clone(),
                    input: turn.envelope.clone(),
                    emit: output_stream.clone(),
                    resolved_session_snapshot: handoff.invocation_session_snapshot(),
                    current_session_context,
                    output_root: &output_root,
                    live_stream_root: &live_stream_root,
                    source_movement_seq: turn.seq,
                    attempt,
                    admitted_run_record: &pinned_run_record,
                    current_run_record: &current_run_record,
                    deferred_context_generations,
                    input_changes_context,
                    interrupted_after_artifact: interrupted_after_artifact.clone(),
                    recovered_handoff_artifacts: recovered_handoff_artifacts.clone(),
                    durable_interrupted_continuity: durable_interrupted_continuity.clone(),
                    reconciled_result_continuity,
                    control,
                    handoff_barrier: handoff_barrier.clone(),
                })
                .await;
            drop(output_stream);
            let _ = terminal_tx.send(result.is_ok());
            result
        });
        let (result, forwarded) = {
            let mut receive = receive;
            let mut forwarder = forwarder;
            tokio::select! {
                result = &mut receive => (result, forwarder.await),
                forwarded = &mut forwarder => {
                    match forwarded {
                        Ok(forwarded) => (receive.await, Ok(forwarded)),
                        Err(error) => {
                            handoff_barrier.finish(Err(error.clone()));
                            return Err(error);
                        }
                    }
                }
            }
        };
        handoff_barrier.finish(forwarded.as_ref().map(|_| ()).map_err(Clone::clone));
        let forwarded = forwarded?;
        drop(live);

        if resolving_interrupted_result {
            if interrupted_effects.reconciled_source(run, turn.seq).await? {
                control.finish();
                return Ok(());
            }
            return Err(format!(
                "interrupted exact-result source {} ended without one ledger-admitted semantic closure{}",
                turn.seq, result.err().map(|error| format!(": {error}")).unwrap_or_default()
            ));
        }

        match result {
            Ok(()) if forwarded.outputs == 0 => {
                control.finish();
                let error = match shutdown_driver(ctx, driver).await {
                    Ok(()) => "codex transport completed a turn without emitting an artifact".to_string(),
                    Err(shutdown_error) => format!("codex transport completed a turn without emitting an artifact; {shutdown_error}"),
                };
                return emit_terminal_turn_error(
                    ctx, descriptor, handoff_session, run, &turn, &handoff, attempt, &error,
                )
                .await;
            }
            Ok(()) => return Ok(()),
            Err(InterfaceBoundaryError::Protocol(error))
            | Err(InterfaceBoundaryError::FailedClosed(error)) => {
                control.finish();
                let error = match shutdown_driver(ctx, driver).await {
                    Ok(()) => error,
                    Err(shutdown_error) => format!("{error}; {shutdown_error}"),
                };
                return emit_terminal_turn_error(
                    ctx, descriptor, handoff_session, run, &turn, &handoff, attempt, &error,
                )
                .await;
            }
            Err(InterfaceBoundaryError::Transport(error)) => {
                tracing::warn!(
                    interface = ctx.interface.as_str(),
                    run = run.as_str(),
                    seq = turn.seq,
                    attempt,
                    error = %error,
                    "codex turn transport failed"
                );
                if let Err(shutdown_error) = shutdown_driver(ctx, driver).await {
                    control.finish();
                    return emit_terminal_turn_error(
                        ctx, descriptor, handoff_session, run, &turn, &handoff, attempt,
                        &format!("{error}; {shutdown_error}"),
                    ).await;
                }
                let (source_recovery, recovered_outcomes) =
                    handoff.recover_source_with_outcomes().await?;
                (interrupted_after_artifact, recovered_handoff_artifacts) = match source_recovery {
                    ProviderSourceRecovery::Process => {
                        if !recovered_outcomes.is_empty() {
                            return Err(format!(
                                "provider source movement {} returned recovery outcomes without an interrupted frontier",
                                turn.seq
                            ));
                        }
                        (None, Arc::new(Vec::new()))
                    }
                    ProviderSourceRecovery::Completed => {
                        interrupted_effects
                            .recover_completed_source(run, turn.seq, &recovered_outcomes)
                            .await?;
                        retain_completed_source_control(ctx, descriptor, runtime_auth, driver, control, interrupted_effects, run, &turn, &handoff).await?;
                        return Ok(());
                    }
                    ProviderSourceRecovery::Interrupted {
                        last_artifact_label,
                    } => (
                        Some(last_artifact_label),
                        recovered_codex_handoff_artifacts(ctx, run, turn.seq, &recovered_outcomes)
                            .await?,
                    ),
                };
                if !backoff_or_give_up(ctx, max_attempts, &mut attempt).await? {
                    emit_terminal_turn_error(
                        ctx,
                        descriptor,
                        handoff_session,
                        run,
                        &turn,
                        &handoff,
                        max_attempts,
                        &error,
                    )
                    .await?;
                    control.finish();
                    return Ok(());
                }
            }
        }
    }
}
