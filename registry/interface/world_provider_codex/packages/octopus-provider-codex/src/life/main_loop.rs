use super::*;

pub(crate) async fn codex_life(
    ctx: InterfaceLiftContext,
    expectation: ProviderLifeExpectation,
) -> Result<(), String> {
    let descriptor = load_own_descriptor(&ctx)?;
    let connection_mode = validate_provider_descriptor_contract(&descriptor, expectation)?;
    let provider_home = ctx.interface_var_dir().join("codex-home");
    let auth_store_home = resolve_provider_auth_store(&ctx, &descriptor, &connection_mode)?;
    let manager = auth_manager_from_provider_home(auth_store_home.clone()).await;
    let activation_secrets_home = ctx.interface_var_dir();
    let runtime_auth = CodexRuntimeAuth {
        manager,
        provider_home,
        auth_store_home,
        activation_secrets_home,
        connection_mode,
    };
    let authoring_result = RunAuthoringResultIdentity::from_descriptor(&descriptor)?;
    let handoff_session = ProviderHandoffSession::new(&ctx)?;
    let mut reader = LedgerTailReader::open(&ctx)?;
    let (main_reader_progress, control_reader_progress) =
        tokio::sync::watch::channel(MainReaderProgress {
            next_seq: reader.next_seq(),
            revision: 0,
        });
    let (control_fence_tx, control_fence_rx) = tokio::sync::mpsc::unbounded_channel();
    let instance_config =
        CodexInstanceConfig::from_descriptor(&descriptor).map_err(|error| error.to_string())?;
    let idle_disconnect = instance_config.idle_disconnect();
    let keep_ready_connected = instance_config.connect_on_ready();
    if keep_ready_connected
        && matches!(
            runtime_auth.connection_mode,
            ProviderConnectionMode::OpenCodex(_)
        )
    {
        return Err(format!(
            "OpenCodex interface '{}' cannot connect_on_ready before a formed interface policy arrives",
            descriptor.id
        ));
    }
    let mut driver = if keep_ready_connected {
        Some(
            connect_declared_interface_driver(
                &ctx,
                &descriptor,
                &runtime_auth,
                None,
            )
            .await
            .map_err(|error| {
                format!(
                    "codex interface '{}' could not connect its explicitly declared ready transport: {error}",
                    ctx.interface.as_str()
                )
            })?,
        )
    } else {
        None
    };
    let mut driver_idle_deadline = None;
    let active_controls = Arc::new(tokio::sync::Mutex::new(BTreeMap::new()));
    let control_gates = Arc::new(tokio::sync::Mutex::new(BTreeMap::new()));
    let interrupted_effects = InterruptedEffectArbiter::new(ctx.clone());
    let mut deferred_turns = load_deferred_provider_turns(&ctx, &descriptor)?;
    ctx.signal_ready().await?;
    restore_consumed_provider_inputs(
        &ctx,
        &descriptor,
        &handoff_session,
        &deferred_turns,
        &interrupted_effects,
        reader.next_seq(),
    )
    .await?;
    Box::pin(restore_completed_provider_sources(&ctx, &descriptor, &handoff_session, &runtime_auth, &mut driver,
        &active_controls, &interrupted_effects, reader.next_seq())).await?;
    let mut control_task =
        tokio_util::task::AbortOnDropHandle::new(tokio::spawn(Box::pin(control_reader_life(
            ctx.clone(), descriptor.clone(), handoff_session.clone(), active_controls.clone(),
            control_gates.clone(), interrupted_effects.clone(), control_reader_progress, control_fence_rx,
        ))));
    let restored_controls = active_controls.lock().await.iter().map(|(run, broker)|
        (RunId::new(run), broker.source_movement_seq())).collect::<Vec<_>>();
    for (run, source) in restored_controls {
        if let Err(error) = fence_control_reader(&control_fence_tx, &run, source).await {
            return flatten_control_task(control_task.await).and(Err(error));
        }
    }

    loop {
        if ctx.cancel.is_cancelled() {
            finish_active_controls(&active_controls).await;
            control_task.abort();
            let _ = control_task.await;
            shutdown_driver(&ctx, &mut driver).await?;
            return Ok(());
        }
        if control_task.is_finished() {
            let result = flatten_control_task(control_task.await);
            finish_active_controls(&active_controls).await;
            shutdown_driver(&ctx, &mut driver).await?;
            return result;
        }
        let deferred_candidates = deferred_turns
            .iter()
            .filter(|(seq, _)| **seq < reader.next_seq())
            .map(|(seq, deferred)| (*seq, deferred.clone()))
            .collect::<Vec<_>>();
        let mut ready_deferred = None;
        for (seq, deferred) in deferred_candidates {
            if !interrupted_effects.reconciled(&deferred.run).await? {
                continue;
            }
            match prepare_deferred_provider_turn(&ctx, &descriptor, &deferred.run, &deferred, &active_controls).await? {
                DeferredTurnReadiness::AwaitingSessionSettlement => {}
                DeferredTurnReadiness::Ready(turn) => {
                    ready_deferred = Some((seq, turn));
                    break;
                }
            }
        }
        if let Some((seq, turn)) = ready_deferred {
            let DeferredProviderTurn { run, .. } = deferred_turns
                .remove(&seq)
                .ok_or_else(|| format!("deferred provider turn {seq} disappeared"))?;
            retire_controls_before_deferred_turn(
                &active_controls, &main_reader_progress, &control_fence_tx, &run,
            ).await?;
            let broker = CodexTurnControlBroker::new(Some(turn.seq));
            active_controls
                .lock()
                .await
                .insert(run.as_str().to_string(), broker.clone());
            publish_main_reader_progress(&main_reader_progress, reader.next_seq());
            let arrival = turn.clone();
            let driven = tokio::select! {
                result = &mut control_task => {
                    broker.finish();
                    remove_control_if_same(&active_controls, &run, &broker).await;
                    let result = flatten_control_task(result);
                    finish_active_controls(&active_controls).await;
                    shutdown_driver(&ctx, &mut driver).await?;
                    return result;
                }
                _ = ctx.cancel.cancelled() => {
                    broker.finish();
                    remove_control_if_same(&active_controls, &run, &broker).await;
                    finish_active_controls(&active_controls).await;
                    control_task.abort();
                    let _ = control_task.await;
                    shutdown_driver(&ctx, &mut driver).await?;
                    return Ok(());
                }
                result = drive_turn_with_retries(
                    CodexTurnScope {
                        ctx: &ctx,
                        descriptor: &descriptor,
                        handoff_session: &handoff_session,
                        runtime_auth: &runtime_auth,
                        run: &run,
                        control: &broker,
                        interrupted_effects: &interrupted_effects,
                    },
                    &mut driver,
                    turn,
                ) => result,
            };
            if let Err(error) = driven {
                broker.finish();
                remove_control_if_same(&active_controls, &run, &broker).await;
                finish_active_controls(&active_controls).await;
                control_task.abort();
                let _ = control_task.await;
                shutdown_driver(&ctx, &mut driver).await?;
                return Err(error);
            }
            remove_deferred_provider_turn(&ctx, seq)?;
            if broker.is_starting() {
                broker.finish();
            }
            record_latest_arrival(&broker, &arrival);
            if broker.is_finished() || broker.is_recovered_pending() {
                if let Err(error) =
                    fence_control_reader(&control_fence_tx, &run, broker.source_movement_seq())
                        .await
                {
                    finish_active_controls(&active_controls).await;
                    control_task.abort();
                    let _ = control_task.await;
                    shutdown_driver(&ctx, &mut driver).await?;
                    return Err(error);
                }
                if broker.is_finished() { remove_control_if_same(&active_controls, &run, &broker).await; }
            }
            if !deferred_turns.is_empty() {
                continue;
            }
            if keep_ready_connected {
                driver_idle_deadline = None;
            } else if let Some(idle) = idle_disconnect {
                driver_idle_deadline = driver.as_ref().map(|_| tokio::time::Instant::now() + idle);
            } else {
                release_driver_if_quiescent(&ctx, &active_controls, &mut driver).await?;
                driver_idle_deadline = None;
            }
            continue;
        }
        let batch = reader.read_new()?;
        publish_main_reader_progress(&main_reader_progress, reader.next_seq());
        if batch.is_empty() {
            let mut late_arrivals = late_call_arrivals(&driver, &active_controls).await;
            if let Some(ready) = ready_late_call(&driver, &active_controls).await {
                let woken = tokio::select! {
                    result = &mut control_task => {
                        let result = flatten_control_task(result);
                        finish_active_controls(&active_controls).await;
                        shutdown_driver(&ctx, &mut driver).await?;
                        return result;
                    }
                    _ = ctx.cancel.cancelled() => {
                        finish_active_controls(&active_controls).await;
                        control_task.abort();
                        let _ = control_task.await;
                        shutdown_driver(&ctx, &mut driver).await?;
                        return Ok(());
                    }
                    result = wake_late_calls(&ctx, &descriptor, &handoff_session, &control_gates, &driver, &ready) => result,
                };
                if let Err(error) = woken {
                    ready.broker.finish();
                    remove_control_if_same(&active_controls, &ready.run, &ready.broker).await;
                    finish_active_controls(&active_controls).await;
                    control_task.abort();
                    let _ = control_task.await;
                    shutdown_driver(&ctx, &mut driver).await?;
                    return Err(error);
                }
                continue;
            }
            if let Some(deadline) = driver_idle_deadline {
                tokio::select! {
                    result = &mut control_task => {
                        let result = flatten_control_task(result);
                        finish_active_controls(&active_controls).await;
                        shutdown_driver(&ctx, &mut driver).await?;
                        return result;
                    }
                    _ = ctx.cancel.cancelled() => {
                        finish_active_controls(&active_controls).await;
                        control_task.abort();
                        let _ = control_task.await;
                        shutdown_driver(&ctx, &mut driver).await?;
                        return Ok(());
                    }
                    _ = late_call_arrival(&mut late_arrivals) => {}
                    _ = tokio::time::sleep_until(deadline) => {
                        if release_driver_if_quiescent(&ctx, &active_controls, &mut driver).await? {
                            driver_idle_deadline = None;
                        } else {
                            driver_idle_deadline = idle_disconnect
                                .map(|idle| tokio::time::Instant::now() + idle);
                        }
                    }
                    result = reader.wait_for_change(&ctx.cancel) => result?,
                }
            } else {
                tokio::select! {
                    result = &mut control_task => {
                        let result = flatten_control_task(result);
                        finish_active_controls(&active_controls).await;
                        shutdown_driver(&ctx, &mut driver).await?;
                        return result;
                    }
                    _ = ctx.cancel.cancelled() => {
                        finish_active_controls(&active_controls).await;
                        control_task.abort();
                        let _ = control_task.await;
                        shutdown_driver(&ctx, &mut driver).await?;
                        return Ok(());
                    }
                    _ = late_call_arrival(&mut late_arrivals) => {}
                    result = reader.wait_for_change(&ctx.cancel) => result?,
                }
            }
            continue;
        }
        for own in batch {
            let envelope = match reader.envelope(&own) {
                Ok(envelope) => envelope,
                Err(error) => {
                    ctx.hand_admitted_failure(
                        own.movement.seq.0,
                        "poisoned_outbound",
                        serde_json::json!({ "error": error }),
                    )
                    .await?;
                    commit_main_reader(&mut reader, &own, &main_reader_progress)?;
                    continue;
                }
            };
            let manifest = envelope.artifact_file.read_manifest().map_err(|error| {
                format!(
                    "provider movement {} has malformed artifact manifest: {error}",
                    own.movement.seq.0
                )
            })?;
            if manifest.label == crate::handler::composer_prediction::REQUEST_LABEL
                || manifest.label == crate::handler::client_generation::REQUEST_LABEL {
                if !matches!(runtime_auth.connection_mode, ProviderConnectionMode::Native) {
                    return Err("client generation requires a native Codex binding".into());
                }
                if driver.is_none() {
                    driver = Some(connect_declared_interface_driver(&ctx, &descriptor, &runtime_auth, Some(&envelope))
                        .await.map_err(|error| error.message)?);
                }
                let result = if manifest.label == crate::handler::client_generation::REQUEST_LABEL {
                    crate::handler::client_generation::run(&ctx, driver.as_ref().unwrap(), &envelope, own.movement.seq.0).await
                } else {
                    crate::handler::composer_prediction::run(&ctx, driver.as_ref().unwrap(),
                        &runtime_auth.manager, &envelope, own.movement.seq.0).await
                };
                if let Err(error) = result {
                    if error.retires_driver() {
                        if error.result_admitted() {
                            commit_main_reader(&mut reader, &own, &main_reader_progress)?;
                        }
                        finish_active_controls(&active_controls).await;
                        control_task.abort();
                        let _ = control_task.await;
                        shutdown_driver(&ctx, &mut driver).await?;
                        return Err(error.to_string());
                    }
                    let failure = if manifest.label == crate::handler::client_generation::REQUEST_LABEL {
                        "client_generation_failed"
                    } else {
                        "composer_prediction_failed"
                    };
                    ctx.hand_admitted_failure(own.movement.seq.0, failure, serde_json::json!({"error":error.to_string()})).await?;
                }
                commit_main_reader(&mut reader, &own, &main_reader_progress)?;
                continue;
            }
            let metadata = match own.metadata() {
                Ok(metadata) => metadata,
                Err(error) => {
                    ctx.hand_admitted_failure(
                        own.movement.seq.0,
                        "malformed_runtime_envelope",
                        serde_json::json!({ "error": error }),
                    )
                    .await?;
                    commit_main_reader(&mut reader, &own, &main_reader_progress)?;
                    continue;
                }
            };
            let Some(run) = metadata.runtime.as_ref().map(|runtime| runtime.run.clone()) else {
                ctx.hand_admitted_failure(
                    own.movement.seq.0,
                    "missing_runtime_envelope",
                    serde_json::json!({}),
                )
                .await?;
                commit_main_reader(&mut reader, &own, &main_reader_progress)?;
                continue;
            };
            if authoring_result
                .consume_if_matches(&ctx, &handoff_session, &own.movement, &envelope, &manifest.label)
                .await?
            {
                commit_main_reader(&mut reader, &own, &main_reader_progress)?;
                continue;
            }
            if deferred_turns.contains_key(&own.movement.seq.0) {
                let replayed = QueuedTurn {
                    envelope,
                    seq: own.movement.seq.0,
                };
                persist_deferred_provider_turn(&ctx, &run, &replayed)?;
                commit_main_reader(&mut reader, &own, &main_reader_progress)?;
                break;
            }

            let durable_interruption = interrupted_effects.durable_continuity(&run).await?;
            let mut existing_control = active_control(&active_controls, &run).await;
            if existing_control.is_none()
                && let Some(continuity) = durable_interruption.as_ref()
            {
                let broker = CodexTurnControlBroker::new(None);
                broker.publish_recovered_interrupted_pending(continuity)?;
                active_controls
                    .lock()
                    .await
                    .insert(run.as_str().to_string(), broker.clone());
                publish_main_reader_progress(&main_reader_progress, reader.next_seq());
                existing_control = Some(broker);
            }
            let explicit_inactive_resume = existing_control.is_none()
                && is_explicit_turn_control_label(&manifest.label)
                && decode_admitted_turn_control(&ctx, &descriptor, &envelope)
                    .ok()
                    .flatten()
                    .is_some_and(|control| control_action(&control) == "resume");
            if is_inflight_turn_control_candidate_label(&manifest.label, &descriptor)
                && control_journal_exists(&ctx, own.movement.seq.0)?
            {
                match decode_admitted_turn_control(&ctx, &descriptor, &envelope) {
                    Ok(Some(control)) => {
                        let terminal_control_fenced = existing_control.is_some();
                        let broker = existing_control.clone().unwrap_or_else(|| {
                            let broker = CodexTurnControlBroker::new(None);
                            broker.finish();
                            broker
                        });
                        let disposition = handle_control_source(
                            &ctx,
                            &descriptor,
                            &handoff_session,
                            &interrupted_effects,
                            &run,
                            ControlSource {
                                envelope: &envelope,
                                source_movement_seq: own.movement.seq.0,
                                broker: &broker,
                                control_gates: &control_gates,
                                control,
                                terminal_control_fenced,
                                independent_reader: false,
                            },
                        )
                        .await?;
                        if disposition == ControlSourceDisposition::DeferredTurn {
                            let turn = QueuedTurn {
                                envelope,
                                seq: own.movement.seq.0,
                            };
                            persist_deferred_provider_turn(&ctx, &run, &turn)?;
                            deferred_turns.entry(turn.seq).or_insert_with(|| {
                                DeferredProviderTurn {
                                    run: run.clone(),
                                    turn,
                                    release: None,
                                }
                            });
                            commit_main_reader(&mut reader, &own, &main_reader_progress)?;
                            break;
                        }
                    }
                    Ok(None) => {
                        handle_malformed_control_source(
                            &ctx,
                            &descriptor,
                            &handoff_session,
                            &interrupted_effects,
                            &run,
                            MalformedControlSource {
                                envelope: &envelope,
                                source_movement_seq: own.movement.seq.0,
                                broker: existing_control.as_ref(),
                                control_gates: &control_gates,
                                error: "journaled turn-control source decoded to no control"
                                    .to_string(),
                            },
                        )
                        .await?;
                    }
                    Err(error) => {
                        handle_malformed_control_source(
                            &ctx,
                            &descriptor,
                            &handoff_session,
                            &interrupted_effects,
                            &run,
                            MalformedControlSource {
                                envelope: &envelope,
                                source_movement_seq: own.movement.seq.0,
                                broker: existing_control.as_ref(),
                                control_gates: &control_gates,
                                error: error.to_string(),
                            },
                        )
                        .await?;
                    }
                }
                commit_main_reader(&mut reader, &own, &main_reader_progress)?;
                continue;
            }
            if let Some(broker) = existing_control.as_ref() {
                match decode_admitted_turn_control(&ctx, &descriptor, &envelope) {
                    Ok(Some(control)) => {
                        let disposition = handle_control_source(
                            &ctx,
                            &descriptor,
                            &handoff_session,
                            &interrupted_effects,
                            &run,
                            ControlSource {
                                envelope: &envelope,
                                source_movement_seq: own.movement.seq.0,
                                broker,
                                control_gates: &control_gates,
                                control,
                                terminal_control_fenced: true,
                                independent_reader: false,
                            },
                        )
                        .await?;
                        match disposition {
                            ControlSourceDisposition::Handled => {
                                commit_main_reader(&mut reader, &own, &main_reader_progress)?;
                                continue;
                            }
                            ControlSourceDisposition::DeferredTurn => {
                                let turn = QueuedTurn {
                                    envelope,
                                    seq: own.movement.seq.0,
                                };
                                persist_deferred_provider_turn(&ctx, &run, &turn)?;
                                deferred_turns.entry(turn.seq).or_insert_with(|| {
                                    DeferredProviderTurn {
                                        run: run.clone(),
                                        turn,
                                        release: None,
                                    }
                                });
                                commit_main_reader(&mut reader, &own, &main_reader_progress)?;
                                break;
                            }
                        }
                    }
                    Ok(None) => {}
                    Err(error) if is_inflight_turn_control_candidate_label(&manifest.label, &descriptor) => {
                        handle_malformed_control_source(
                            &ctx,
                            &descriptor,
                            &handoff_session,
                            &interrupted_effects,
                            &run,
                            MalformedControlSource {
                                envelope: &envelope,
                                source_movement_seq: own.movement.seq.0,
                                broker: Some(broker),
                                control_gates: &control_gates,
                                error: error.to_string(),
                            },
                        )
                        .await?;
                        commit_main_reader(&mut reader, &own, &main_reader_progress)?;
                        continue;
                    }
                    Err(_) => {}
                }
            } else if (is_explicit_turn_control_label(&manifest.label) && !explicit_inactive_resume) || is_admitted_peer_interrupt(&envelope, &descriptor) {
                match decode_admitted_turn_control(&ctx, &descriptor, &envelope) {
                    Ok(Some(control)) => {
                        let broker = CodexTurnControlBroker::new(None);
                        broker.finish();
                        handle_control_source(
                            &ctx,
                            &descriptor,
                            &handoff_session,
                            &interrupted_effects,
                            &run,
                            ControlSource {
                                envelope: &envelope,
                                source_movement_seq: own.movement.seq.0,
                                broker: &broker,
                                control_gates: &control_gates,
                                control,
                                terminal_control_fenced: true,
                                independent_reader: false,
                            },
                        )
                        .await?;
                    }
                    Ok(None) => {
                        handle_malformed_control_source(
                            &ctx,
                            &descriptor,
                            &handoff_session,
                            &interrupted_effects,
                            &run,
                            MalformedControlSource {
                                envelope: &envelope,
                                source_movement_seq: own.movement.seq.0,
                                broker: None,
                                control_gates: &control_gates,
                                error: "explicit turn-control label decoded to no control"
                                    .to_string(),
                            },
                        )
                        .await?;
                    }
                    Err(error) => {
                        handle_malformed_control_source(
                            &ctx,
                            &descriptor,
                            &handoff_session,
                            &interrupted_effects,
                            &run,
                            MalformedControlSource {
                                envelope: &envelope,
                                source_movement_seq: own.movement.seq.0,
                                broker: None,
                                control_gates: &control_gates,
                                error: error.to_string(),
                            },
                        )
                        .await?;
                    }
                }
                commit_main_reader(&mut reader, &own, &main_reader_progress)?;
                continue;
            }

            let turn = QueuedTurn {
                envelope,
                seq: own.movement.seq.0,
            };
            if matches!(is_provider_turn_input(&descriptor, &turn.envelope), Ok(true)) {
                match operator_turn_requires_deferral_with_context(&ctx, &descriptor, &handoff_session, &run, &turn).await {
                    Ok(true) => {
                        persist_deferred_provider_turn(&ctx, &run, &turn)?;
                        deferred_turns
                            .entry(turn.seq)
                            .or_insert_with(|| DeferredProviderTurn {
                                run: run.clone(),
                                turn,
                                release: None,
                            });
                        commit_main_reader(&mut reader, &own, &main_reader_progress)?;
                        break;
                    }
                    Ok(false) => {}
                    Err(error) => {
                        tracing::warn!(
                            interface = ctx.interface.as_str(),
                            run = run.as_str(),
                            seq = turn.seq,
                            error = %error,
                            "operator turn deferral preflight delegated validation to the canonical provider path"
                        );
                    }
                }
            }
            let (broker, created_broker) = match existing_control {
                Some(broker) => (broker, false),
                None => {
                    let broker = CodexTurnControlBroker::new(Some(own.movement.seq.0));
                    active_controls
                        .lock()
                        .await
                        .insert(run.as_str().to_string(), broker.clone());
                    publish_main_reader_progress(&main_reader_progress, reader.next_seq());
                    (broker, true)
                }
            };
            let arrival = turn.clone();
            let driven = tokio::select! {
                result = &mut control_task => {
                    broker.finish();
                    remove_control_if_same(&active_controls, &run, &broker).await;
                    let result = flatten_control_task(result);
                    finish_active_controls(&active_controls).await;
                    shutdown_driver(&ctx, &mut driver).await?;
                    return result;
                }
                _ = ctx.cancel.cancelled() => {
                    broker.finish();
                    remove_control_if_same(&active_controls, &run, &broker).await;
                    finish_active_controls(&active_controls).await;
                    control_task.abort();
                    let _ = control_task.await;
                    shutdown_driver(&ctx, &mut driver).await?;
                    return Ok(());
                }
                result = drive_turn_with_retries(
                    CodexTurnScope {
                        ctx: &ctx,
                        descriptor: &descriptor,
                        handoff_session: &handoff_session,
                        runtime_auth: &runtime_auth,
                        run: &run,
                        control: &broker,
                        interrupted_effects: &interrupted_effects,
                    },
                    &mut driver,
                    turn,
                ) => result,
            };
            if let Err(error) = driven {
                broker.finish();
                remove_control_if_same(&active_controls, &run, &broker).await;
                finish_active_controls(&active_controls).await;
                control_task.abort();
                let _ = control_task.await;
                shutdown_driver(&ctx, &mut driver).await?;
                return Err(error);
            }
            commit_main_reader(&mut reader, &own, &main_reader_progress)?;
            if created_broker && broker.is_starting() {
                broker.finish();
            }
            record_latest_arrival(&broker, &arrival);
            if broker.is_finished() || broker.is_recovered_pending() {
                if let Err(error) =
                    fence_control_reader(&control_fence_tx, &run, broker.source_movement_seq())
                        .await
                {
                    finish_active_controls(&active_controls).await;
                    control_task.abort();
                    let _ = control_task.await;
                    shutdown_driver(&ctx, &mut driver).await?;
                    return Err(error);
                }
                if broker.is_finished() { remove_control_if_same(&active_controls, &run, &broker).await; }
            }
            if !deferred_turns.is_empty() {
                break;
            }
            if keep_ready_connected {
                driver_idle_deadline = None;
            } else if let Some(idle) = idle_disconnect {
                driver_idle_deadline = driver.as_ref().map(|_| tokio::time::Instant::now() + idle);
            } else {
                release_driver_if_quiescent(&ctx, &active_controls, &mut driver).await?;
                driver_idle_deadline = None;
            }
        }
    }
}

pub(crate) fn publish_main_reader_progress(
    progress: &tokio::sync::watch::Sender<MainReaderProgress>,
    next_seq: u64,
) {
    progress.send_modify(|current| {
        current.next_seq = next_seq;
        current.revision = current.revision.saturating_add(1);
    });
}

pub(crate) fn commit_main_reader(
    reader: &mut LedgerTailReader,
    own: &octopus_world_pyecho_life_runtime::OwnOutboundMovement,
    progress: &tokio::sync::watch::Sender<MainReaderProgress>,
) -> Result<(), String> {
    reader.commit_through(own)?;
    publish_main_reader_progress(progress, reader.next_seq());
    Ok(())
}

pub(crate) async fn finish_active_controls(active_controls: &ActiveTurnControls) {
    let mut active = active_controls.lock().await;
    for broker in active.values() {
        broker.finish();
    }
    active.clear();
}

pub(crate) async fn active_controls_are_quiescent(active_controls: &ActiveTurnControls) -> bool {
    let mut active = active_controls.lock().await;
    active.retain(|_, broker| !broker.is_finished());
    active.is_empty()
}

pub(crate) async fn release_driver_if_quiescent(
    ctx: &InterfaceLiftContext,
    active_controls: &ActiveTurnControls,
    driver: &mut Option<Arc<CodexAppServerInterfaceDriver>>,
) -> Result<bool, String> {
    let Some(live) = driver.as_ref() else {
        return Ok(true);
    };
    if !active_controls_are_quiescent(active_controls).await || !live.is_strictly_quiescent().await
    {
        return Ok(false);
    }
    shutdown_driver(ctx, driver).await?;
    Ok(true)
}

pub(crate) async fn shutdown_driver(
    ctx: &InterfaceLiftContext,
    driver: &mut Option<Arc<CodexAppServerInterfaceDriver>>,
) -> Result<(), String> {
    let Some(live) = driver.take() else {
        return Ok(());
    };
    let live = Arc::try_unwrap(live).map_err(|live| {
        *driver = Some(live);
        format!(
            "codex interface '{}' cannot release a driver that is still shared by live work",
            ctx.interface.as_str()
        )
    })?;
    live.shutdown().await.map_err(|error| {
        format!(
            "codex interface '{}' could not shut down its private app-server driver: {error}",
            ctx.interface.as_str()
        )
    })
}

pub(crate) fn current_auth_revision(manager: &AuthManager) -> u64 {
    let receiver = manager.auth_change_receiver();
    *receiver.borrow()
}

pub(crate) async fn adopt_current_machine_auth_generation(
    ctx: &InterfaceLiftContext,
    runtime_auth: &CodexRuntimeAuth,
    driver: &mut Option<Arc<CodexAppServerInterfaceDriver>>,
) -> Result<(), String> {
    runtime_auth.manager.reload().await;
    let current_revision = current_auth_revision(&runtime_auth.manager);
    let Some(live) = driver.as_ref() else {
        return Ok(());
    };
    let connected_revision = live.connected_auth_revision().ok_or_else(|| {
        format!(
            "codex interface '{}' connected a driver without explicit auth-generation evidence",
            ctx.interface.as_str()
        )
    })?;
    if connected_revision == current_revision {
        return Ok(());
    }
    if !live.is_strictly_quiescent().await {
        return Err(format!(
            "codex interface '{}' observed a new machine auth generation while its exact driver was not quiescent",
            ctx.interface.as_str()
        ));
    }
    shutdown_driver(ctx, driver).await
}

pub(crate) async fn connect_declared_interface_driver(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    runtime_auth: &CodexRuntimeAuth,
    envelope: Option<&InterfaceInvocationEnvelope>,
) -> Result<Arc<CodexAppServerInterfaceDriver>, ProviderConnectionFailure> {
    let driver = match &runtime_auth.connection_mode {
        ProviderConnectionMode::Native => {
            connect_interface_driver_from_descriptor_with_auth_and_secrets(
                descriptor,
                &runtime_auth.manager,
                runtime_auth.provider_home.clone(),
                runtime_auth.auth_store_home.clone(),
                runtime_auth.activation_secrets_home.clone(),
            )
            .await
            .map_err(|error| ProviderConnectionFailure {
                retryable: error.is_retryable_transport_failure(),
                message: error.to_string(),
            })
        }
        ProviderConnectionMode::OpenCodex(config) => {
            let envelope = envelope.ok_or_else(|| ProviderConnectionFailure {
                message:
                    "OpenCodex driver connection requires freshly formed interface policy metadata"
                        .to_string(),
                retryable: false,
            })?;
            crate::opencodex::connect_interface_driver(
                ctx,
                descriptor,
                envelope,
                config,
                &runtime_auth.manager,
                runtime_auth.provider_home.clone(),
                runtime_auth.auth_store_home.clone(),
            )
            .await
            .map_err(|message| ProviderConnectionFailure {
                message,
                retryable: true,
            })
        }
    }?;
    driver.attach_life_context(ctx.clone()).map_err(|message| ProviderConnectionFailure { message, retryable: false })?;
    Ok(driver)
}

pub(crate) fn flatten_control_task(
    result: Result<Result<(), String>, tokio::task::JoinError>,
) -> Result<(), String> {
    match result {
        Ok(result) => result,
        Err(error) => Err(format!("codex turn-control reader task failed: {error}")),
    }
}
