use super::*;
use crate::handler::{LateCallDrain, LateCallWake};

pub(crate) fn record_latest_arrival(broker: &CodexTurnControlBroker, turn: &QueuedTurn) {
    if let Some(wake) = broker.late_call_wake().filter(|wake| wake.source_movement_seq == turn.seq) {
        wake.record_arrival(turn.seq, &turn.envelope);
    }
}

pub(crate) async fn late_call_arrivals(
    driver: &Option<Arc<CodexAppServerInterfaceDriver>>,
    active_controls: &ActiveTurnControls,
) -> Option<tokio::sync::watch::Receiver<u64>> {
    let driver = driver.as_ref()?;
    let waiting = active_controls.lock().await.values().any(|broker| broker.late_call_wake().is_some());
    if waiting { Some(driver.event_arrivals().await) } else { None }
}

pub(crate) async fn late_call_arrival(arrivals: &mut Option<tokio::sync::watch::Receiver<u64>>) {
    let Some(arrivals) = arrivals else {
        return std::future::pending().await;
    };
    if arrivals.changed().await.is_err() {
        std::future::pending::<()>().await;
    }
}

pub(crate) struct ReadyLateCall {
    pub(crate) run: RunId,
    pub(crate) broker: Arc<CodexTurnControlBroker>,
    pub(crate) wake: Arc<LateCallWake>,
}

pub(crate) async fn ready_late_call(
    driver: &Option<Arc<CodexAppServerInterfaceDriver>>,
    active_controls: &ActiveTurnControls,
) -> Option<ReadyLateCall> {
    let driver = driver.as_ref()?;
    let brokers = active_controls
        .lock()
        .await
        .iter()
        .map(|(run, broker)| (run.clone(), broker.clone()))
        .collect::<Vec<_>>();
    for (run, broker) in brokers {
        let Some(wake) = broker.late_call_wake().filter(|wake| wake.has_arrival()) else { continue };
        if driver.late_call_ready(&wake).await {
            return Some(ReadyLateCall { run: RunId::new(run.as_str()), broker, wake });
        }
    }
    None
}

async fn frontier_handoff(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    handoff_session: &ProviderHandoffSession,
    run: &RunId,
    seq: u64,
    source: &InterfaceInvocationEnvelope,
) -> Result<(ProviderOutputHandoff, Option<u64>), String> {
    let pinned = ctx.pinned_run_record_bytes(source)?;
    let handoff =
        wait_for_provider_handoff(ctx, descriptor, handoff_session, run, source, seq, &pinned).await?;
    let record: serde_json::Value = serde_json::from_slice(&handoff.current_run_record_bytes().await)
        .map_err(|error| error.to_string())?;
    let frontier = &record["interface_state"][ctx.interface.as_str()]["pending_handoff"];
    let completed = frontier["terminal"] == true;
    Ok((handoff, frontier["source_movement_seq"].as_u64().filter(|_| completed)))
}

pub(crate) async fn wake_late_calls(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    handoff_session: &ProviderHandoffSession,
    control_gates: &ControlSourceGates,
    driver: &Option<Arc<CodexAppServerInterfaceDriver>>,
    ready: &ReadyLateCall,
) -> Result<(), String> {
    let ReadyLateCall { run, broker, wake } = ready;
    let Some(driver) = driver.as_ref() else {
        return Ok(());
    };
    let gate = control_source_gate(control_gates, run).await;
    let _control_guard = gate.lock().await;
    if !broker.late_call_wake().is_some_and(|current| Arc::ptr_eq(&current, wake))
        || !driver.late_call_ready(wake).await
    {
        return Ok(());
    }
    let Some(latest) = wake.latest_arrival() else {
        return Ok(());
    };
    let refused = |seq: u64, reason: String| {
        format!(
            "late native call of run '{}' cannot be emitted as the output of source movement {seq}: {reason}",
            run.as_str(),
        )
    };
    let (mut handoff, mut frontier) = frontier_handoff(ctx, descriptor, handoff_session, run, latest.0, &latest.1)
        .await
        .map_err(|error| refused(latest.0, error))?;
    let mut source = latest;
    if frontier != Some(source.0)
        && let Some(owner) = frontier.and_then(|seq| wake.arrival(seq).map(|envelope| (seq, envelope)))
    {
        (handoff, frontier) = frontier_handoff(ctx, descriptor, handoff_session, run, owner.0, &owner.1)
            .await
            .map_err(|error| refused(owner.0, error))?;
        source = owner;
    }
    if frontier != Some(source.0) {
        wake.withdraw();
        return Ok(());
    }
    let (seq, envelope) = source;
    let refused = |reason: String| refused(seq, reason);
    match handoff.recover_source().await.map_err(&refused)? {
        ProviderSourceRecovery::Completed => {}
        recovery => return Err(refused(format!("its completed handoff recovered as {recovery:?}"))),
    }
    handoff
        .validate_client_output_routes(
            &codex_projection_output_labels(descriptor).map_err(|error| refused(error.to_string()))?,
        )
        .map_err(&refused)?;
    let (output_stream, emit_rx) = ProviderOutputStream::channel();
    let handoff_barrier = ProviderHandoffBarrier::new();
    let output_root = ctx.interface_var_dir().join("emitted");
    let live_stream_root = ctx.interface_var_dir().join("streams");
    let drain = async {
        let drained = driver
            .drain_late_calls(LateCallDrain {
                control: broker,
                wake,
                source: &envelope,
                source_movement_seq: seq,
                emit: &output_stream,
                handoff_barrier: &handoff_barrier,
                output_root: &output_root,
                live_stream_root: &live_stream_root,
            })
            .await;
        drop(output_stream);
        drained
    };
    let (drained, collected) = tokio::join!(drain, collect_late_call_emissions(emit_rx));
    let handed = async {
        drained.map_err(|error| refused(error.to_string()))?;
        let emissions = collected.map_err(&refused)?;
        if emissions.is_empty() {
            return Ok(());
        }
        for emission in emissions {
            let outcomes = handoff
                .hand_completed_source_continuation(vec![emission])
                .await
                .map_err(&refused)?;
            if let Some((failure_seq, failure_class)) = outcomes.iter().find_map(|outcome| match outcome {
                ProviderHandoffOutcome::TerminalFailure { failure_seq, failure_class, .. } => {
                    Some((*failure_seq, failure_class.clone()))
                }
                ProviderHandoffOutcome::Admitted { .. } => None,
            }) {
                return Err(refused(format!(
                    "run authoring replaced it with failure '{failure_class}' at movement {failure_seq}"
                )));
            }
        }
        Ok(())
    }
    .await;
    handoff_barrier.finish(handed.clone());
    handed
}

async fn collect_late_call_emissions(
    mut emit_rx: tokio::sync::mpsc::UnboundedReceiver<ProviderStreamEmission>,
) -> Result<Vec<ProviderEmission>, String> {
    let refused = "a source continuation carries only progress and call artifacts".to_string();
    let mut emissions = Vec::new();
    while let Some(emission) = emit_rx.recv().await {
        match emission {
            ProviderStreamEmission::Progress(emission) | ProviderStreamEmission::Turn(emission) => {
                emissions.push(emission);
            }
            ProviderStreamEmission::Control { admitted, .. }
            | ProviderStreamEmission::TransportDispatch { admitted, .. }
            | ProviderStreamEmission::ContextBoundary { admitted, .. } => {
                let _ = admitted.send(Err(refused.clone()));
                return Err(refused);
            }
            ProviderStreamEmission::CurrentRunRecord { reply, .. } => {
                let _ = reply.send(Err(refused.clone()));
                return Err(refused);
            }
            ProviderStreamEmission::WithOutgoingBindings { .. }
            | ProviderStreamEmission::WithCapabilityAmendment { .. }
            | ProviderStreamEmission::TerminalGroup { .. }
            | ProviderStreamEmission::InterruptedResult { .. } => return Err(refused),
        }
    }
    Ok(emissions)
}
