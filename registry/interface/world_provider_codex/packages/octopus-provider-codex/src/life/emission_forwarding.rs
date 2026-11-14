use super::*;
use crate::handler::CodexContinuityState;

pub(crate) struct EmissionForwarding<'a> {
    pub(crate) ctx: &'a InterfaceLiftContext,
    pub(crate) descriptor: &'a InterfaceDescriptor,
    pub(crate) control: &'a Arc<CodexTurnControlBroker>,
    pub(crate) handoff: ProviderOutputHandoff,
    pub(crate) interrupted_effects: InterruptedEffectArbiter,
    pub(crate) durable_interrupted_continuity: Option<CodexContinuityState>,
    pub(crate) native_terminal_source: Option<CodexContinuityState>,
    pub(crate) source_envelope: InterfaceInvocationEnvelope,
    pub(crate) run: RunId,
    pub(crate) source_artifact: ArtifactRef,
    pub(crate) result_source_movement_seq: u64,
    pub(crate) resolving_interrupted_result: bool,
}

pub(crate) async fn forward_provider_emissions(
    forwarding: EmissionForwarding<'_>,
    mut emit_rx: tokio::sync::mpsc::UnboundedReceiver<ProviderStreamEmission>,
    terminal_rx: tokio::sync::oneshot::Receiver<bool>,
) -> Result<ForwardedProviderEmissions, String> {
    let EmissionForwarding {
        ctx,
        descriptor,
        control,
        handoff,
        interrupted_effects,
        durable_interrupted_continuity,
        native_terminal_source,
        source_envelope,
        run,
        source_artifact,
        result_source_movement_seq,
        resolving_interrupted_result,
    } = forwarding;
    let mut pending = Vec::new();
    let mut forwarded = ForwardedProviderEmissions::default();
    let mut interrupted_closure = None;
    let mut terminal_group_handed = false;
    loop {
        let Some(stream_emission) = emit_rx.recv().await else {
            break;
        };
        if interrupted_closure.is_some() || terminal_group_handed {
            return Err(
                "provider emitted another artifact after its Codex-local terminal group"
                    .to_string(),
            );
        }
        match stream_emission {
            ProviderStreamEmission::Progress(emission) => {
                if resolving_interrupted_result {
                    return Err(
                        "interrupted result cannot emit progress outside its exact closure".into(),
                    );
                }
                forwarded.outputs += flush_stream_batch(&handoff, &mut pending, false).await?;
                hand_stream_emission(&handoff, (emission, None, Vec::new()), false).await?;
                forwarded.outputs += 1;
            }
            ProviderStreamEmission::CurrentRunRecord {
                expected_before_hash,
                reply,
            } => {
                forwarded.outputs += flush_stream_batch(&handoff, &mut pending, false).await?;
                let current = handoff.reconciled_run_record_bytes().await?;
                let current = match expected_before_hash {
                    Some(hash) => handoff
                        .checked_capability_baseline(&hash)
                        .await
                        .map(|(observed, current)| (current, Some(observed))),
                    None => Ok((current, None)),
                };
                reply
                    .send(current)
                    .map_err(|_| "checked run reader closed before its response".to_string())?;
            }
            ProviderStreamEmission::WithCapabilityAmendment {
                emission,
                amendment,
            } => {
                if resolving_interrupted_result {
                    return Err("interrupted result cannot amend its capability contract".into());
                }
                forwarded.outputs += flush_stream_batch(&handoff, &mut pending, false).await?;
                // The bridge ends this input's handoff. Its external result
                // remains pending in the emitted native continuity.
                let outcomes = handoff
                    .hand_batch_with_capability_amendment(vec![emission], true, &amendment)
                    .await?;
                if outcomes.len() != 1
                    || !outcomes
                        .iter()
                        .all(|outcome| matches!(outcome, ProviderHandoffOutcome::Admitted { .. }))
                {
                    return Err("capability amendment has no exact admitted bridge effect".into());
                }
                forwarded.outputs += 1;
                terminal_group_handed = true;
            }
            ProviderStreamEmission::WithOutgoingBindings { emission, bindings } => {
                if resolving_interrupted_result {
                    return Err("interrupted result cannot create outgoing bindings".into());
                }
                forwarded.outputs +=
                    hold_newest_stream_emission(&handoff, &mut pending, (emission, None, bindings))
                        .await?;
            }
            ProviderStreamEmission::Turn(emission) => {
                if resolving_interrupted_result {
                    return Err(
                        "interrupted exact-result recovery emitted a non-atomic provider artifact before its closure"
                            .to_string(),
                    );
                }
                forwarded.outputs += hold_newest_stream_emission(
                    &handoff,
                    &mut pending,
                    (emission, None, Vec::new()),
                )
                .await?;
            }
            ProviderStreamEmission::TerminalGroup { stream, terminal } => {
                if resolving_interrupted_result {
                    return Err(
                        "interrupted exact-result recovery emitted a normal terminal group"
                            .to_string(),
                    );
                }
                if !pending.is_empty() {
                    let flushed = flush_stream_batch(&handoff, &mut pending, false).await?;
                    forwarded.outputs += flushed;
                }
                let mut outcomes = handoff.hand_progress_batch(vec![stream]).await?;
                if outcomes
                    .iter()
                    .all(|outcome| matches!(outcome, ProviderHandoffOutcome::Admitted { .. }))
                {
                    outcomes.extend(handoff.hand_batch(vec![terminal]).await?);
                }
                if outcomes.len() != 2 {
                    return Err(format!(
                        "Codex terminal group produced {} outcomes instead of 2",
                        outcomes.len()
                    ));
                }
                if let Some((failure_seq, failure_class)) =
                    outcomes.iter().find_map(|outcome| match outcome {
                        ProviderHandoffOutcome::TerminalFailure {
                            failure_seq,
                            failure_class,
                            ..
                        } => Some((*failure_seq, failure_class.as_str())),
                        ProviderHandoffOutcome::Admitted { .. } => None,
                    })
                {
                    return Err(format!(
                        "Codex terminal group was replaced by run-authoring failure '{failure_class}' at movement {failure_seq}"
                    ));
                }
                forwarded.outputs += outcomes.len();
                terminal_group_handed = true;
            }
            ProviderStreamEmission::InterruptedResult {
                activity,
                stream,
                retained_outputs,
                closure,
            } => {
                if !pending.is_empty() {
                    return Err(
                        "provider emitted an interrupted-result group outside its exact durable source boundary"
                            .to_string(),
                    );
                }
                let closure_ref = if resolving_interrupted_result {
                    let continuity = durable_interrupted_continuity
                        .as_ref()
                        .ok_or("interrupted-result group has no durable interruption continuity")?;
                    interrupted_result_closure_ref(
                        &closure.artifact,
                        continuity,
                        &source_artifact,
                        &codex_context_output_labels(descriptor)
                            .map_err(|error| error.to_string())?,
                    )?
                } else {
                    let continuity = native_terminal_source.as_ref().ok_or(
                        "control-free terminal group has no original ordinary pending source",
                    )?;
                    native_terminal_result_closure_ref(
                        &closure.artifact,
                        continuity,
                        &source_envelope,
                        &codex_context_output_labels(descriptor)
                            .map_err(|error| error.to_string())?,
                    )?
                }
                .ok_or_else(|| {
                    "interrupted-result group has no semantic closure artifact".to_string()
                })?;
                let mut emissions = Vec::new();
                if let Some(activity) = activity {
                    emissions.push(activity);
                }
                if let Some(stream) = stream {
                    emissions.push(stream);
                }
                emissions.extend(retained_outputs);
                emissions.push(closure);
                match admit_interrupted_result(
                    &ctx,
                    &handoff,
                    control,
                    resolving_interrupted_result.then_some(&interrupted_effects),
                    &run,
                    result_source_movement_seq,
                    &closure_ref,
                    emissions,
                )
                .await?
                {
                    InterruptedResultAdmission::Admitted(count) => forwarded.outputs += count,
                }
                interrupted_closure = Some(closure_ref);
                terminal_group_handed = true;
            }
            ProviderStreamEmission::ContextBoundary {
                emissions,
                admitted,
            } => {
                let result = async {
                    if resolving_interrupted_result {
                        return Err(
                            "interrupted exact-result recovery cannot refresh provider context"
                                .to_string(),
                        );
                    }
                    if !pending.is_empty() {
                        forwarded.outputs +=
                            flush_stream_batch(&handoff, &mut pending, false).await?;
                    }
                    let count = emissions.len();
                    if count == 0 || count > 3 {
                        return Err(
                            "provider context boundary has an invalid emission count".to_string()
                        );
                    }
                    let outcomes = handoff
                        .hand_batch_with_outgoing_bindings(emissions, false, &[])
                        .await?;
                    if outcomes.len() != count
                        || outcomes.iter().any(|outcome| {
                            !matches!(outcome, ProviderHandoffOutcome::Admitted { .. })
                        })
                    {
                        return Err("provider context boundary was not fully admitted".to_string());
                    }
                    forwarded.outputs += count;
                    Ok(())
                }
                .await;
                let _ = admitted.send(result.clone());
                result?;
            }
            ProviderStreamEmission::Control {
                emission,
                source_movement_seq,
                admitted,
            } => {
                if resolving_interrupted_result {
                    let error = "interrupted exact-result recovery cannot emit a control artifact"
                        .to_string();
                    let _ = admitted.send(Err(error.clone()));
                    return Err(error);
                }
                let label = emission
                    .artifact
                    .read_manifest()
                    .map_err(|error| error.to_string())?
                    .label;
                if !pending.is_empty() {
                    forwarded.outputs += flush_stream_batch(&handoff, &mut pending, false).await?;
                }
                let consume_context = !matches!(
                    label.as_str(),
                    crate::handler::TURN_INPUT_ACKNOWLEDGED_LABEL
                        | crate::handler::TURN_CONTROL_FAILED_LABEL
                );
                let result = handoff.hand_from_input(emission, false, source_movement_seq, consume_context).await.and_then(|outcome| match outcome {
                    ProviderHandoffOutcome::Admitted { .. } => Ok(()),
                    ProviderHandoffOutcome::TerminalFailure { failure_seq, failure_class, .. } => Err(format!(
                        "provider control response stopped by authoring failure '{failure_class}' at {failure_seq}"
                    )),
                });
                let _ = admitted.send(result.clone());
                result?;
                forwarded.outputs += 1;
            }
            ProviderStreamEmission::TransportDispatch { artifact, admitted } => {
                if resolving_interrupted_result {
                    let error = "interrupted exact-result recovery cannot dispatch another provider request"
                        .to_string();
                    let _ = admitted.send(Err(error.clone()));
                    return Err(error);
                }
                if !pending.is_empty() {
                    let flushed = flush_stream_batch(&handoff, &mut pending, false).await?;
                    forwarded.outputs += flushed;
                }
                let result = handoff.hand_transport_dispatch(artifact).await.and_then(
                    |outcome| match outcome {
                        ProviderHandoffOutcome::Admitted { .. } => Ok(()),
                        ProviderHandoffOutcome::TerminalFailure {
                            failure_seq,
                            failure_class,
                            ..
                        } => Err(format!(
                            "provider transport dispatch stopped by terminal run-authoring failure '{failure_class}' at movement {failure_seq}"
                        )),
                    },
                );
                match result {
                    Ok(()) => {
                        let _ = admitted.send(Ok(()));
                        forwarded.dispatches += 1;
                    }
                    Err(error) => {
                        let _ = admitted.send(Err(error.clone()));
                        return Err(error);
                    }
                }
            }
        }
    }
    let terminal = terminal_rx.await.unwrap_or(false);
    if terminal_group_handed {
        if !pending.is_empty() {
            return Err("Codex-local terminal group left unhanded provider outputs".to_string());
        }
        return Ok::<ForwardedProviderEmissions, String>(forwarded);
    }
    let (flushed, last) = close_stream_emissions(
        &handoff,
        &mut pending,
        terminal || interrupted_closure.is_some(),
    )
    .await?;
    forwarded.outputs += flushed;
    if let Some(outcome) = last {
        if let Some(closure) = interrupted_closure {
            match outcome {
                ProviderHandoffOutcome::Admitted { seq, artifact } => {
                    let admitted = DurableArtifactAdmission {
                        artifact,
                        movement_seq: seq,
                    };
                    interrupted_effects
                        .complete_closing(&run, result_source_movement_seq, &closure, &admitted)
                        .await?;
                }
                ProviderHandoffOutcome::TerminalFailure {
                    failure_seq,
                    failure_class,
                    ..
                } => {
                    return Err(format!(
                        "interrupted-result closure was replaced by terminal run-authoring failure '{failure_class}' at movement {failure_seq}"
                    ));
                }
            }
        }
        forwarded.outputs += 1;
    }
    Ok(forwarded)
}
