use super::*;

pub(crate) async fn control_reader_life(
    ctx: InterfaceLiftContext,
    descriptor: InterfaceDescriptor,
    handoff_session: ProviderHandoffSession,
    active_controls: ActiveTurnControls,
    control_gates: ControlSourceGates,
    interrupted_effects: InterruptedEffectArbiter,
    mut main_reader_progress: tokio::sync::watch::Receiver<MainReaderProgress>,
    mut terminal_fences: tokio::sync::mpsc::UnboundedReceiver<ControlReaderFence>,
) -> Result<(), String> {
    use futures::StreamExt;

    let mut reader = LedgerTailReader::open_named(&ctx, "turn-control")?;
    let mut scanned_through = reader.next_seq();
    let mut movements = BTreeMap::new();
    let mut pending = BTreeMap::<u64, PendingControlReaderSource>::new();
    let mut settled = BTreeSet::new();
    let mut running = BTreeMap::<String, u64>::new();
    let mut tasks = futures::stream::FuturesUnordered::new();
    let mut fences = Vec::<PendingControlReaderFence>::new();
    loop {
        if ctx.cancel.is_cancelled() {
            return Ok(());
        }
        while let Ok(fence) = terminal_fences.try_recv() {
            fences.push(PendingControlReaderFence {
                fence,
                watermark: None,
            });
        }
        let batch = reader.read_new_uncommitted_limit(PROVIDER_CONTROL_READ_BATCH_LIMIT)?;
        let caught_up = batch.is_empty();
        for own in batch {
            let seq = own.movement.seq.0;
            scanned_through = scanned_through.max(seq.saturating_add(1));
            match control_reader_source(&reader, &own, &descriptor) {
                Some(source) => {
                    pending.insert(seq, source);
                }
                None => {
                    settled.insert(seq);
                }
            }
            movements.insert(seq, own);
        }
        if caught_up {
            for fence in &mut fences {
                fence.watermark.get_or_insert(scanned_through);
            }
        }
        let mut blocked_runs = running.keys().cloned().collect::<BTreeSet<_>>();
        for seq in pending.keys().copied().collect::<Vec<_>>() {
            let source = pending.get(&seq).expect("pending source selected above");
            let run_key = source.run.as_str().to_string();
            if !blocked_runs.insert(run_key.clone()) {
                continue;
            }
            let broker = active_control(&active_controls, &source.run).await;
            match control_reader_ownership(
                broker.as_deref(), main_reader_progress.borrow().next_seq, seq, &source.label,
            ) {
                ControlReaderOwnership::Main => {
                    pending.remove(&seq);
                    settled.insert(seq);
                    blocked_runs.remove(&run_key);
                    continue;
                }
                ControlReaderOwnership::AwaitMain => continue,
                ControlReaderOwnership::Independent => {}
            }
            let broker = broker.expect("independent control ownership requires a live target");
            let source = pending.remove(&seq).expect("pending source selected above");
            running.insert(run_key.clone(), seq);
            let ctx = ctx.clone();
            let descriptor = descriptor.clone();
            let handoff_session = handoff_session.clone();
            let control_gates = control_gates.clone();
            let interrupted_effects = interrupted_effects.clone();
            tasks.push(async move {
                let result = process_control_reader_source(
                    &ctx,
                    &descriptor,
                    &handoff_session,
                    &interrupted_effects,
                    &control_gates,
                    &broker,
                    seq,
                    source,
                )
                .await;
                (run_key, seq, result)
            });
        }
        let mut committed = None;
        while let Some((&seq, _)) = movements.first_key_value() {
            if !settled.remove(&seq) {
                break;
            }
            committed = movements.pop_first().map(|(_, own)| own);
        }
        if let Some(own) = committed {
            reader.commit_through(&own)?;
        }
        if movements.is_empty() {
            reader.advance_settled_scan()?;
            scanned_through = scanned_through.max(reader.next_seq());
        }
        fences.retain_mut(|pending_fence| {
            let Some(watermark) = pending_fence.watermark else {
                return true;
            };
            let run = &pending_fence.fence.run;
            let waiting = pending
                .range(..watermark)
                .any(|(_, source)| source.run == *run);
            let executing = running
                .get(run.as_str())
                .is_some_and(|seq| *seq < watermark);
            if waiting || executing {
                return true;
            }
            if let Some(acknowledged) = pending_fence.fence.acknowledged.take() {
                let _ = acknowledged.send(watermark);
            }
            false
        });
        tokio::select! {
            completed = tasks.next(), if !tasks.is_empty() => {
                let (run, seq, result) = completed.expect("control task set is not empty");
                result?;
                if running.remove(&run) != Some(seq) {
                    return Err(format!("control reader lost run '{run}' movement {seq} ownership"));
                }
                settled.insert(seq);
            }
            fence = terminal_fences.recv() => {
                let fence = fence.ok_or_else(||
                    "codex main ledger reader dropped the terminal control fence channel".to_string()
                )?;
                fences.push(PendingControlReaderFence { fence, watermark: None });
            }
            changed = main_reader_progress.changed() => {
                changed.map_err(|_| "codex main ledger reader stopped before turn-control intake settled".to_string())?;
            }
            result = reader.wait_for_change(&ctx.cancel) => result?,
            _ = ctx.cancel.cancelled() => return Ok(()),
        }
    }
}

pub(crate) fn decode_admitted_turn_control(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    envelope: &InterfaceInvocationEnvelope,
) -> Result<Option<CodexTurnControl>, InterfaceBoundaryError> {
    let label = envelope.read_artifact_manifest()?.label;
    let pinned = if is_inflight_turn_control_candidate_label(&label, descriptor)
        && !is_explicit_turn_control_label(&label)
    {
        Some(ctx.pinned_run_record_bytes(envelope)
            .map_err(InterfaceBoundaryError::FailedClosed)?)
    } else {
        None
    };
    decode_inflight_turn_control(descriptor, envelope, pinned.as_deref())
}

pub(crate) fn control_reader_source(
    reader: &LedgerTailReader,
    own: &octopus_world_pyecho_life_runtime::OwnOutboundMovement,
    descriptor: &InterfaceDescriptor,
) -> Option<PendingControlReaderSource> {
    let envelope = reader.envelope(own).ok()?;
    let label = envelope.artifact_file.read_manifest().ok()?.label;
    if !is_inflight_turn_control_candidate_label(&label, descriptor) {
        return None;
    }
    let run = own.metadata().ok()?.runtime.as_ref()?.run.clone();
    Some(PendingControlReaderSource {
        envelope,
        label,
        run,
    })
}

pub(crate) async fn process_control_reader_source(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    handoff_session: &ProviderHandoffSession,
    interrupted_effects: &InterruptedEffectArbiter,
    control_gates: &ControlSourceGates,
    broker: &Arc<CodexTurnControlBroker>,
    source_movement_seq: u64,
    source: PendingControlReaderSource,
) -> Result<(), String> {
    match decode_admitted_turn_control(ctx, descriptor, &source.envelope) {
        Ok(Some(control)) => {
            handle_control_source(
                ctx,
                descriptor,
                handoff_session,
                interrupted_effects,
                &source.run,
                ControlSource {
                    envelope: &source.envelope,
                    source_movement_seq,
                    broker,
                    control_gates,
                    control,
                    terminal_control_fenced: true,
                    independent_reader: true,
                },
            )
            .await?;
        }
        Ok(None) => {}
        Err(error) => {
            handle_malformed_control_source(
                ctx,
                descriptor,
                handoff_session,
                interrupted_effects,
                &source.run,
                MalformedControlSource {
                    envelope: &source.envelope,
                    source_movement_seq,
                    broker: Some(broker),
                    control_gates,
                    error: error.to_string(),
                },
            )
            .await?;
        }
    }
    Ok(())
}

pub(crate) async fn active_control(
    active_controls: &ActiveTurnControls,
    run: &RunId,
) -> Option<Arc<CodexTurnControlBroker>> {
    active_controls.lock().await.get(run.as_str()).cloned()
}

pub(crate) enum ControlReaderOwnership {
    Main,
    AwaitMain,
    Independent,
}

pub(crate) fn control_reader_ownership(
    broker: Option<&CodexTurnControlBroker>,
    main_next_seq: u64,
    source_seq: u64,
    artifact_label: &str,
) -> ControlReaderOwnership {
    let Some(broker) = broker else {
        return if main_next_seq > source_seq {
            ControlReaderOwnership::Main
        } else {
            ControlReaderOwnership::AwaitMain
        };
    };
    if broker.source_movement_seq().is_some_and(|start| source_seq <= start)
        || (broker.is_finished() && !is_explicit_turn_control_label(artifact_label))
    {
        return ControlReaderOwnership::Main;
    }
    if broker.source_movement_seq().is_none() && !broker.is_finished() {
        return if main_next_seq > source_seq {
            ControlReaderOwnership::Main
        } else {
            ControlReaderOwnership::AwaitMain
        };
    }
    ControlReaderOwnership::Independent
}

pub(crate) async fn fence_control_reader(
    terminal_fences: &tokio::sync::mpsc::UnboundedSender<ControlReaderFence>,
    run: &RunId,
    source_movement_seq: Option<u64>,
) -> Result<u64, String> {
    let (acknowledged, acknowledgement) = tokio::sync::oneshot::channel();
    terminal_fences
        .send(ControlReaderFence {
            run: run.clone(),
            acknowledged: Some(acknowledged),
        })
        .map_err(|_| "codex turn-control reader stopped before terminal retirement".to_string())?;
    let watermark = acknowledgement.await.map_err(|_| {
        "codex turn-control reader stopped before acknowledging terminal retirement".to_string()
    })?;
    if let Some(source) = source_movement_seq
        && watermark <= source
    {
        return Err(format!(
            "codex turn-control reader acknowledged terminal watermark {watermark} before provider source {source}"
        ));
    }
    Ok(watermark)
}

pub(crate) async fn retire_controls_before_deferred_turn(
    active_controls: &ActiveTurnControls,
    main_reader_progress: &tokio::sync::watch::Sender<MainReaderProgress>,
    terminal_fences: &tokio::sync::mpsc::UnboundedSender<ControlReaderFence>,
    run: &RunId,
) -> Result<(), String> {
    let broker = match active_control(active_controls, run).await {
        Some(broker) if broker.is_finished() => broker,
        Some(_) => return Err("deferred provider turn is ready before its active control target settled".into()),
        None => {
            let broker = CodexTurnControlBroker::new(None);
            broker.finish();
            active_controls.lock().await.insert(run.to_string(), broker.clone());
            broker
        }
    };
    let next_seq = main_reader_progress.borrow().next_seq;
    publish_main_reader_progress(main_reader_progress, next_seq);
    fence_control_reader(terminal_fences, run, None).await?;
    remove_control_if_same(active_controls, run, &broker).await;
    Ok(())
}

pub(crate) async fn remove_control_if_same(
    active_controls: &ActiveTurnControls,
    run: &RunId,
    broker: &Arc<CodexTurnControlBroker>,
) {
    let mut active = active_controls.lock().await;
    let key = run.as_str();
    if active
        .get(key)
        .is_some_and(|candidate| Arc::ptr_eq(candidate, broker))
    {
        active.remove(key);
    }
}

pub(crate) async fn handle_control_source(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    handoff_session: &ProviderHandoffSession,
    interrupted_effects: &InterruptedEffectArbiter,
    run: &RunId,
    source: ControlSource<'_>,
) -> Result<ControlSourceDisposition, String> {
    let ControlSource {
        envelope,
        source_movement_seq,
        broker,
        control_gates,
        control,
        terminal_control_fenced,
        independent_reader,
    } = source;
    let gate = control_source_gate(control_gates, run).await;
    let _control_guard = gate.lock().await;
    let pending_transport = broker.pending_intent_transport();
    let control_id = control_id(&control).to_string();
    let action = control_action(&control).to_string();
    let journal = load_control_journal(ctx, source_movement_seq, run, &control_id, &action)?;
    if journal.is_none() && matches!(&control, CodexTurnControl::Steer { .. } | CodexTurnControl::Inject { .. }) {
        let durable_interruption = interrupted_effects.durable_continuity(run).await?;
        let manifest = envelope
            .read_artifact_manifest()
            .map_err(|error| error.to_string())?;
        if !is_explicit_turn_control_label(&manifest.label)
            && (broker.has_interrupted_pending() || durable_interruption.is_some())
        {
            broker.wait_for_durable_interrupt().await?;
            return Ok(ControlSourceDisposition::DeferredTurn);
        }
    }
    let (
        CodexTurnControlApplied {
            mut result,
            disposition: _,
            output,
            handoff_barrier,
            durable_interrupt_handoff,
            effect_guard,
        },
        recovery,
    ) = match journal.map(|journal| journal.phase) {
        Some(TurnControlJournalPhase::DeferredTurn { .. }) => {
            return Ok(ControlSourceDisposition::DeferredTurn);
        }
        Some(TurnControlJournalPhase::Delivered { result })
        | Some(TurnControlJournalPhase::Result { result }) => (
            CodexTurnControlApplied {
                result,
                disposition: CodexTurnControlDisposition::Applied,
                output: broker.output().await,
                handoff_barrier: None,
                durable_interrupt_handoff: None,
                effect_guard: None,
            },
            true,
        ),
        Some(TurnControlJournalPhase::Intent) => {
            let recovered = Box::pin(recover_pending_intent_control(ctx, descriptor, handoff_session, run, broker,
                pending_transport.as_ref(), &control)).await?;
            let result = recovered.unwrap_or_else(|| CodexTurnControlResult {
                control_id: control_id.clone(),
                action: action.clone(),
                acknowledged: false,
                thread_id: None,
                turn_id: None,
                detail: serde_json::json!({
                    "error": "provider process restarted after recording a turn-control intent; the external effect outcome is ambiguous and was not replayed"
                }),
                continuity: None,
            });
            persist_control_journal(
                ctx,
                source_movement_seq,
                run,
                &control_id,
                &action,
                TurnControlJournalPhase::Result {
                    result: result.clone(),
                },
            )?;
            (
                CodexTurnControlApplied {
                    result,
                    disposition: CodexTurnControlDisposition::Applied,
                    output: broker.output().await,
                    handoff_barrier: None,
                    durable_interrupt_handoff: None,
                    effect_guard: None,
                },
                true,
            )
        }
        Some(TurnControlJournalPhase::RetargetIntent {
            expected_turn_id,
            actual_turn_id,
        }) => {
            let result = CodexTurnControlResult {
                control_id: control_id.clone(),
                action: action.clone(),
                acknowledged: false,
                thread_id: None,
                turn_id: Some(actual_turn_id.clone()),
                detail: serde_json::json!({
                    "error": "provider process restarted after recording a one-shot steer retarget intent; the retry outcome is ambiguous and was not replayed",
                    "first_expected_turn_id": expected_turn_id,
                    "retry_expected_turn_id": actual_turn_id,
                }),
                continuity: None,
            };
            persist_control_journal(
                ctx,
                source_movement_seq,
                run,
                &control_id,
                &action,
                TurnControlJournalPhase::Result {
                    result: result.clone(),
                },
            )?;
            (
                CodexTurnControlApplied {
                    result,
                    disposition: CodexTurnControlDisposition::Applied,
                    output: broker.output().await,
                    handoff_barrier: None,
                    durable_interrupt_handoff: None,
                    effect_guard: None,
                },
                true,
            )
        }
        None => {
            let mut applied = if terminal_control_fenced
                && terminal_control_is_satisfied(&action)
                && broker.is_finished()
            {
                terminally_satisfied_control(&control_id, &action)
            } else {
                broker
                    .apply(control, |intent| {
                        let phase = match intent {
                            CodexTurnControlEffectIntent::Initial => {
                                TurnControlJournalPhase::Intent
                            }
                            CodexTurnControlEffectIntent::Retarget {
                                expected_turn_id,
                                actual_turn_id,
                            } => TurnControlJournalPhase::RetargetIntent {
                                expected_turn_id,
                                actual_turn_id,
                            },
                        };
                        persist_control_journal(
                            ctx,
                            source_movement_seq,
                            run,
                            &control_id,
                            &action,
                            phase,
                        )
                    })
                    .await?
            };
            if let CodexTurnControlDisposition::DeferredTurn(cause) = applied.disposition {
                persist_control_journal(
                    ctx,
                    source_movement_seq,
                    run,
                    &control_id,
                    &action,
                    TurnControlJournalPhase::DeferredTurn {
                        result: applied.result,
                        cause,
                    },
                )?;
                return Ok(ControlSourceDisposition::DeferredTurn);
            }
            if terminal_control_fenced
                && terminal_control_is_satisfied(&action)
                && broker.is_finished()
                && control_finished_without_dispatch(&applied.result)
            {
                applied = terminally_satisfied_control(&control_id, &action);
            }
            persist_control_journal(
                ctx,
                source_movement_seq,
                run,
                &control_id,
                &action,
                TurnControlJournalPhase::Result {
                    result: applied.result.clone(),
                },
            )?;
            (applied, false)
        }
    };
    if result.acknowledged && terminal_control_is_satisfied(&result.action)
        && (pending_transport.is_some() || result.detail.get("pending_source_withdrawal").is_some())
    {
        Box::pin(withdraw_pending_source_control(ctx, descriptor, handoff_session, run, broker,
            pending_transport, envelope, source_movement_seq, &mut result)).await?;
    }
    let handoff = hand_control_result(
        ctx,
        descriptor,
        handoff_session,
        interrupted_effects,
        run,
        ControlResultDelivery {
            envelope,
            source_movement_seq,
            output,
            handoff_barrier,
            effect_guard,
            result: &mut result,
            recovery,
            defer_message_receipt: independent_reader,
        },
    )
    .await;
    if let Some(barrier) = durable_interrupt_handoff {
        barrier.finish(handoff.clone().map(|_| ()));
    }
    if !handoff? {
        return Ok(ControlSourceDisposition::Handled);
    }
    broker
        .release_late_calls_after_control(&control_id, source_movement_seq, envelope, result.continuity.as_ref())
        .await;
    persist_control_journal(
        ctx,
        source_movement_seq,
        run,
        &control_id,
        &action,
        TurnControlJournalPhase::Delivered { result },
    )?;
    Ok(ControlSourceDisposition::Handled)
}

pub(crate) fn terminal_control_is_satisfied(action: &str) -> bool {
    matches!(action, "interrupt" | "stop")
}

pub(crate) fn control_finished_without_dispatch(result: &CodexTurnControlResult) -> bool {
    !result.acknowledged && result.thread_id.is_none() && result.turn_id.is_none()
}

pub(crate) fn terminally_satisfied_control(control_id: &str, action: &str) -> CodexTurnControlApplied {
    CodexTurnControlApplied {
        result: CodexTurnControlResult {
            control_id: control_id.to_string(),
            action: action.to_string(),
            acknowledged: true,
            thread_id: None,
            turn_id: None,
            detail: serde_json::json!({
                "already_terminal": true,
                "external_effect_dispatched": false,
                "linearization": "control_reader_terminal_fence"
            }),
            continuity: None,
        },
        disposition: CodexTurnControlDisposition::Applied,
        output: None,
        handoff_barrier: None,
        durable_interrupt_handoff: None,
        effect_guard: None,
    }
}

pub(crate) fn handle_malformed_control_source<'a>(
    ctx: &'a InterfaceLiftContext,
    descriptor: &'a InterfaceDescriptor,
    handoff_session: &'a ProviderHandoffSession,
    interrupted_effects: &'a InterruptedEffectArbiter,
    run: &'a RunId,
    source: MalformedControlSource<'a>,
) -> std::pin::Pin<Box<impl std::future::Future<Output = Result<(), String>> + 'a>> {
    Box::pin(handle_malformed_control_source_inner(
        ctx, descriptor, handoff_session, interrupted_effects, run, source,
    ))
}

pub(crate) async fn handle_malformed_control_source_inner(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    handoff_session: &ProviderHandoffSession,
    interrupted_effects: &InterruptedEffectArbiter,
    run: &RunId,
    source: MalformedControlSource<'_>,
) -> Result<(), String> {
    let MalformedControlSource {
        envelope,
        source_movement_seq,
        broker,
        control_gates,
        error,
    } = source;
    let gate = control_source_gate(control_gates, run).await;
    let _control_guard = gate.lock().await;
    let control_id = envelope.artifact.id.as_str().to_string();
    let action = "decode".to_string();
    let journal = load_control_journal(ctx, source_movement_seq, run, &control_id, &action)?;
    let (mut result, recovery) = match journal.map(|journal| journal.phase) {
        Some(TurnControlJournalPhase::Delivered { .. }) => return Ok(()),
        Some(TurnControlJournalPhase::DeferredTurn { .. }) => {
            return Err(
                "deferred turn-control journal source no longer decodes as its operator message"
                    .to_string(),
            );
        }
        Some(TurnControlJournalPhase::Result { result }) => (result, true),
        Some(TurnControlJournalPhase::Intent)
        | Some(TurnControlJournalPhase::RetargetIntent { .. }) => (
            CodexTurnControlResult {
                control_id: control_id.clone(),
                action: action.clone(),
                acknowledged: false,
                thread_id: None,
                turn_id: None,
                detail: serde_json::json!({
                    "error": "malformed turn-control source recovered from an unexpected intent state"
                }),
                continuity: None,
            },
            true,
        ),
        None => (
            CodexTurnControlResult {
                control_id: control_id.clone(),
                action: action.clone(),
                acknowledged: false,
                thread_id: None,
                turn_id: None,
                detail: serde_json::json!({ "error": error }),
                continuity: None,
            },
            false,
        ),
    };
    persist_control_journal(
        ctx,
        source_movement_seq,
        run,
        &control_id,
        &action,
        TurnControlJournalPhase::Result {
            result: result.clone(),
        },
    )?;
    let output = match broker {
        Some(broker) => broker.output().await,
        None => None,
    };
    let handoff_barrier = None;
    let effect_guard = None;
    hand_control_result(
        ctx,
        descriptor,
        handoff_session,
        interrupted_effects,
        run,
        ControlResultDelivery {
            envelope,
            source_movement_seq,
            output,
            handoff_barrier,
            effect_guard,
            result: &mut result,
            recovery,
            defer_message_receipt: false,
        },
    )
    .await?;
    persist_control_journal(
        ctx,
        source_movement_seq,
        run,
        &control_id,
        &action,
        TurnControlJournalPhase::Delivered { result },
    )
}

pub(crate) fn control_journal_path(
    ctx: &InterfaceLiftContext,
    source_movement_seq: u64,
) -> std::path::PathBuf {
    ctx.interface_var_dir()
        .join("turn-control-journal")
        .join(format!("{source_movement_seq}.json"))
}

pub(crate) fn control_journal_exists(
    ctx: &InterfaceLiftContext,
    source_movement_seq: u64,
) -> Result<bool, String> {
    let path = control_journal_path(ctx, source_movement_seq);
    match std::fs::metadata(&path) {
        Ok(metadata) if metadata.is_file() => Ok(true),
        Ok(_) => Err(format!(
            "turn-control journal path '{}' is not a file",
            path.display()
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!(
            "turn-control journal path '{}' cannot be inspected: {error}",
            path.display()
        )),
    }
}

pub(crate) fn load_control_journal(
    ctx: &InterfaceLiftContext,
    source_movement_seq: u64,
    run: &RunId,
    control_id: &str,
    action: &str,
) -> Result<Option<TurnControlJournal>, String> {
    let path = control_journal_path(ctx, source_movement_seq);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "turn-control journal '{}' cannot be read: {error}",
                path.display()
            ));
        }
    };
    let journal = serde_json::from_slice::<TurnControlJournal>(&bytes).map_err(|error| {
        format!(
            "turn-control journal '{}' is malformed: {error}",
            path.display()
        )
    })?;
    if journal.schema_version != 1
        || journal.source_movement_seq != source_movement_seq
        || journal.run_id != run.as_str()
        || journal.control_id != control_id
        || journal.action != action
    {
        return Err(format!(
            "turn-control journal '{}' identity does not match movement {} run '{}' control '{}' action '{}'",
            path.display(),
            source_movement_seq,
            run.as_str(),
            control_id,
            action
        ));
    }
    Ok(Some(journal))
}

pub(crate) fn persist_control_journal(
    ctx: &InterfaceLiftContext,
    source_movement_seq: u64,
    run: &RunId,
    control_id: &str,
    action: &str,
    phase: TurnControlJournalPhase,
) -> Result<(), String> {
    let path = control_journal_path(ctx, source_movement_seq);
    let parent = path.parent().ok_or_else(|| {
        format!(
            "turn-control journal path '{}' has no parent",
            path.display()
        )
    })?;
    std::fs::create_dir_all(parent).map_err(|error| {
        format!(
            "turn-control journal directory '{}' cannot be created: {error}",
            parent.display()
        )
    })?;
    let journal = TurnControlJournal {
        schema_version: 1,
        source_movement_seq,
        run_id: run.as_str().to_string(),
        control_id: control_id.to_string(),
        action: action.to_string(),
        phase,
    };
    let mut bytes = serde_json::to_vec_pretty(&journal)
        .map_err(|error| format!("turn-control journal could not be encoded: {error}"))?;
    bytes.push(b'\n');
    let temporary = path.with_extension("json.tmp");
    {
        let mut file = std::fs::File::create(&temporary).map_err(|error| {
            format!(
                "turn-control journal temporary file '{}' cannot be created: {error}",
                temporary.display()
            )
        })?;
        std::io::Write::write_all(&mut file, &bytes).map_err(|error| {
            format!(
                "turn-control journal temporary file '{}' cannot be written: {error}",
                temporary.display()
            )
        })?;
        file.sync_all().map_err(|error| {
            format!(
                "turn-control journal temporary file '{}' cannot be synced: {error}",
                temporary.display()
            )
        })?;
    }
    std::fs::rename(&temporary, &path).map_err(|error| {
        format!(
            "turn-control journal '{}' cannot be installed: {error}",
            path.display()
        )
    })?;
    std::fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| {
            format!(
                "turn-control journal directory '{}' cannot be synced: {error}",
                parent.display()
            )
        })
}
