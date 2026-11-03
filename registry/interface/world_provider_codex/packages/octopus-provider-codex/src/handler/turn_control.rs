use super::*;

#[derive(Clone)]
pub(crate) enum CodexTurnControl {
    Inject {
        control_id: String,
        items: Vec<serde_json::Value>,
    },
    Steer {
        control_id: String,
        input: Vec<UserInput>,
        source_artifact: Option<ArtifactRef>,
    },
    Interrupt {
        control_id: String,
        action: String,
    },
    Unsupported {
        control_id: String,
        action: String,
    },
}

#[derive(Clone)]
pub(crate) enum CodexTurnControlTarget {
    Starting,
    RecoveredPending {
        handle: AppServerRequestHandle,
        request_ids: Arc<std::sync::atomic::AtomicI64>,
        continuity: CodexContinuityState,
        effect_gate: Arc<tokio::sync::Mutex<()>>,
    },
    Draining {
        handle: AppServerRequestHandle,
        request_ids: Arc<std::sync::atomic::AtomicI64>,
        pending_requests: PendingServerRequests,
        scope: TransportStateScope,
        output: std::sync::Weak<ProviderOutputStream>,
        continuity: Option<CodexContinuityState>,
        handoff_barrier: Arc<ProviderHandoffBarrier>,
        effect_gate: Arc<tokio::sync::Mutex<()>>,
        turn_interrupted: Arc<std::sync::atomic::AtomicBool>,
        thread_id: String,
        turn_id: String,
    },
    Suspended {
        handle: AppServerRequestHandle,
        request_ids: Arc<std::sync::atomic::AtomicI64>,
        pending_requests: PendingServerRequests,
        scope: TransportStateScope,
        output: std::sync::Weak<ProviderOutputStream>,
        continuity: Option<CodexContinuityState>,
        handoff_barrier: Arc<ProviderHandoffBarrier>,
        effect_gate: Arc<tokio::sync::Mutex<()>>,
        turn_interrupted: Arc<std::sync::atomic::AtomicBool>,
        thread_id: String,
        turn_id: String,
    },
    InterruptedPending {
        output: std::sync::Weak<ProviderOutputStream>,
        durable_handoff: Arc<ProviderHandoffBarrier>,
        thread_id: String,
        turn_id: String,
    },
    InterruptedDraining {
        output: std::sync::Weak<ProviderOutputStream>,
        durable_handoff: Arc<ProviderHandoffBarrier>,
        thread_id: String,
        turn_id: String,
    },
    Finished,
}

pub(crate) struct CodexTurnControlBroker {
    pub(crate) state: watch::Sender<CodexTurnControlTarget>,
    pub(crate) source_movement_seq: Option<u64>,
}

#[derive(Clone, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CodexTurnControlResult {
    pub control_id: String,
    pub action: String,
    pub acknowledged: bool,
    pub thread_id: Option<String>,
    pub turn_id: Option<String>,
    pub detail: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) continuity: Option<CodexContinuityState>,
}

pub(crate) struct CodexTurnControlApplied {
    pub result: CodexTurnControlResult,
    pub disposition: CodexTurnControlDisposition,
    pub output: Option<Arc<ProviderOutputStream>>,
    pub handoff_barrier: Option<Arc<ProviderHandoffBarrier>>,
    pub durable_interrupt_handoff: Option<Arc<ProviderHandoffBarrier>>,
    pub effect_guard: Option<tokio::sync::OwnedMutexGuard<()>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CodexTurnControlDisposition {
    Applied,
    DeferredTurn(CodexDeferredTurnCause),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CodexDeferredTurnCause {
    PendingSettlement,
    TerminalTurnMoved,
}

pub(crate) enum CodexTurnControlEffectIntent {
    Initial,
    Retarget {
        expected_turn_id: String,
        actual_turn_id: String,
    },
}

impl CodexTurnControl {
    pub(crate) fn requires_external_effect(&self) -> bool {
        matches!(self, Self::Inject { .. } | Self::Steer { .. } | Self::Interrupt { .. })
    }

    pub(crate) fn control_id(&self) -> &str {
        match self {
            Self::Inject { control_id, .. }
            | Self::Steer { control_id, .. }
            | Self::Interrupt { control_id, .. }
            | Self::Unsupported { control_id, .. } => control_id,
        }
    }

    pub(crate) fn action(&self) -> &str {
        match self {
            Self::Inject { .. } => "queue_message",
            Self::Steer { .. } => "steer",
            Self::Interrupt { action, .. } | Self::Unsupported { action, .. } => action,
        }
    }
}

impl CodexTurnControlBroker {
    pub(crate) fn new(source_movement_seq: Option<u64>) -> Arc<Self> {
        let (state, _) = watch::channel(CodexTurnControlTarget::Starting);
        Arc::new(Self {
            state,
            source_movement_seq,
        })
    }

    pub(crate) fn source_movement_seq(&self) -> Option<u64> {
        self.source_movement_seq
    }

    pub(crate) fn publish_starting(&self) {
        if !self.has_interrupted_pending() {
            self.state.send_replace(CodexTurnControlTarget::Starting);
        }
    }

    pub(crate) fn publish(&self, target: CodexTurnControlTarget) {
        self.state.send_replace(target);
    }

    pub(crate) fn finish(&self) {
        self.state.send_replace(CodexTurnControlTarget::Finished);
    }

    pub(crate) fn is_finished(&self) -> bool {
        matches!(*self.state.borrow(), CodexTurnControlTarget::Finished)
    }

    pub(crate) fn is_starting(&self) -> bool {
        matches!(*self.state.borrow(), CodexTurnControlTarget::Starting)
    }

    pub(crate) fn is_recovered_pending(&self) -> bool {
        matches!(*self.state.borrow(), CodexTurnControlTarget::RecoveredPending { .. })
    }

    pub(crate) async fn prepare_recovered_result(&self) -> Result<(), String> {
        let gate = match self.state.borrow().clone() {
            CodexTurnControlTarget::RecoveredPending { effect_gate, .. } => Some(effect_gate),
            _ => None,
        };
        if let Some(gate) = gate {
            let _guard = gate.lock().await;
            if self.is_recovered_pending() { self.publish_starting(); }
        }
        self.wait_for_durable_interrupt().await
    }

    pub(crate) fn has_interrupted_pending(&self) -> bool {
        matches!(
            *self.state.borrow(),
            CodexTurnControlTarget::InterruptedPending { .. }
                | CodexTurnControlTarget::InterruptedDraining { .. }
        )
    }

    #[cfg(test)]
    pub(crate) fn is_interrupted_draining(&self) -> bool {
        matches!(
            *self.state.borrow(),
            CodexTurnControlTarget::InterruptedDraining { .. }
        )
    }

    pub(crate) async fn wait_for_durable_interrupt(&self) -> Result<(), String> {
        let barrier = match &*self.state.borrow() {
            CodexTurnControlTarget::InterruptedPending {
                durable_handoff, ..
            }
            | CodexTurnControlTarget::InterruptedDraining {
                durable_handoff, ..
            } => Some(durable_handoff.clone()),
            _ => None,
        };
        match barrier {
            Some(barrier) => barrier.wait().await,
            None => Ok(()),
        }
    }

    pub(crate) fn publish_recovered_interrupted_pending(
        &self,
        continuity: &CodexContinuityState,
    ) -> Result<(), String> {
        let pending = continuity.interrupted_pending.as_ref().ok_or_else(|| {
            "recovered interrupted control target has no exact effect".to_string()
        })?;
        if !interrupted_pending_has_external_result(pending) {
            return Err(
                "recovered interrupted control target is not an external effect".to_string(),
            );
        }
        let durable_handoff = ProviderHandoffBarrier::new();
        durable_handoff.finish(Ok(()));
        self.publish(CodexTurnControlTarget::InterruptedPending {
            output: std::sync::Weak::new(),
            durable_handoff,
            thread_id: continuity.thread_id.clone(),
            turn_id: pending_turn_id(pending).to_string(),
        });
        Ok(())
    }

    pub(crate) fn publish_interrupted_draining(&self) -> Result<(), InterfaceDriverError> {
        let target = self.state.borrow().clone();
        match target {
            CodexTurnControlTarget::InterruptedPending {
                output,
                durable_handoff,
                thread_id,
                turn_id,
            } => {
                self.publish(CodexTurnControlTarget::InterruptedDraining {
                    output,
                    durable_handoff,
                    thread_id,
                    turn_id,
                });
                Ok(())
            }
            CodexTurnControlTarget::InterruptedDraining { .. } => Ok(()),
            _ => Err(InterfaceDriverError::FailedClosed(
                "interrupted exact-result drain lost its non-steerable broker target".to_string(),
            )),
        }
    }

    pub(crate) async fn output(&self) -> Option<Arc<ProviderOutputStream>> {
        let mut state = self.state.subscribe();
        loop {
            let target = state.borrow_and_update().clone();
            match target {
                CodexTurnControlTarget::Starting => {
                    if state.changed().await.is_err() {
                        return None;
                    }
                }
                CodexTurnControlTarget::Draining { output, .. }
                | CodexTurnControlTarget::Suspended { output, .. }
                | CodexTurnControlTarget::InterruptedPending { output, .. }
                | CodexTurnControlTarget::InterruptedDraining { output, .. } => {
                    return output.upgrade();
                }
                CodexTurnControlTarget::Finished => return None,
                CodexTurnControlTarget::RecoveredPending { .. } => return None,
            }
        }
    }

    pub(crate) async fn apply(
        &self,
        control: CodexTurnControl,
        mut before_external_effect: impl FnMut(CodexTurnControlEffectIntent) -> Result<(), String>,
    ) -> Result<CodexTurnControlApplied, String> {
        let mut state = self.state.subscribe();
        loop {
            let target = state.borrow_and_update().clone();
            match target {
                CodexTurnControlTarget::RecoveredPending { handle, request_ids, continuity, effect_gate } => {
                    let effect_guard = effect_gate.lock_owned().await;
                    if state.has_changed().unwrap_or(true) { drop(effect_guard); continue; }
                    let pending = continuity.pending.as_ref().ok_or("recovered control lost its exact pending effect")?;
                    let thread_id = continuity.thread_id.clone();
                    let turn_id = pending_turn_id(pending).to_string();
                    if !matches!(&control, CodexTurnControl::Interrupt { .. }) {
                        let disposition = if matches!(&control, CodexTurnControl::Inject { .. } | CodexTurnControl::Steer { .. }) {
                            CodexTurnControlDisposition::DeferredTurn(CodexDeferredTurnCause::PendingSettlement)
                        } else { CodexTurnControlDisposition::Applied };
                        return Ok(CodexTurnControlApplied {
                            result: failed_control_result(&control, Some(thread_id), Some(turn_id), "the recovered provider turn still awaits its exact external result"),
                            disposition, output: None, handoff_barrier: None, durable_interrupt_handoff: None, effect_guard: None,
                        });
                    }
                    before_external_effect(CodexTurnControlEffectIntent::Initial)?;
                    let application = apply_control_to_target(control, handle, request_ids, thread_id.clone(), turn_id.clone(), Some(continuity), &mut before_external_effect).await?;
                    let durable_interrupt_handoff = if exact_interrupted_control_continuity(&application.result)?.is_some() {
                        let barrier = ProviderHandoffBarrier::new();
                        self.publish(CodexTurnControlTarget::InterruptedPending {
                            output: std::sync::Weak::new(), durable_handoff: barrier.clone(), thread_id, turn_id,
                        });
                        Some(barrier)
                    } else { None };
                    return Ok(CodexTurnControlApplied { result: application.result, disposition: application.disposition,
                        output: None, handoff_barrier: None, durable_interrupt_handoff, effect_guard: Some(effect_guard) });
                }
                CodexTurnControlTarget::Starting => {
                    if state.changed().await.is_err() {
                        return Ok(CodexTurnControlApplied {
                            result: failed_control_result(
                                &control,
                                None,
                                None,
                                "active turn control target disappeared before app-server accepted the turn",
                            ),
                            disposition: CodexTurnControlDisposition::Applied,
                            output: None,
                            handoff_barrier: None,
                            durable_interrupt_handoff: None,
                            effect_guard: None,
                        });
                    }
                }
                CodexTurnControlTarget::Finished => {
                    let disposition = if matches!(&control, CodexTurnControl::Steer { .. } | CodexTurnControl::Inject { .. }) {
                        CodexTurnControlDisposition::DeferredTurn(
                            CodexDeferredTurnCause::TerminalTurnMoved,
                        )
                    } else {
                        CodexTurnControlDisposition::Applied
                    };
                    return Ok(CodexTurnControlApplied {
                        result: failed_control_result(
                            &control,
                            None,
                            None,
                            "no active provider turn accepts this control artifact",
                        ),
                        disposition,
                        output: None,
                        handoff_barrier: None,
                        durable_interrupt_handoff: None,
                        effect_guard: None,
                    });
                }
                CodexTurnControlTarget::InterruptedPending {
                    output,
                    durable_handoff,
                    thread_id,
                    turn_id,
                }
                | CodexTurnControlTarget::InterruptedDraining {
                    output,
                    durable_handoff,
                    thread_id,
                    turn_id,
                } => {
                    let disposition = if matches!(&control, CodexTurnControl::Inject { .. }) {
                        CodexTurnControlDisposition::DeferredTurn(CodexDeferredTurnCause::PendingSettlement)
                    } else { CodexTurnControlDisposition::Applied };
                    return Ok(CodexTurnControlApplied {
                        result: failed_control_result(
                            &control,
                            Some(thread_id),
                            Some(turn_id),
                            "the interrupted provider turn still has one unresolved exact external result",
                        ),
                        disposition,
                        output: output.upgrade(),
                        handoff_barrier: None,
                        durable_interrupt_handoff: Some(durable_handoff),
                        effect_guard: None,
                    });
                }
                CodexTurnControlTarget::Draining {
                    handle,
                    request_ids,
                    pending_requests,
                    scope,
                    output,
                    continuity,
                    handoff_barrier,
                    effect_gate,
                    turn_interrupted,
                    thread_id,
                    turn_id,
                } => {
                    let output = output.upgrade();
                    let effect_guard = effect_gate.lock_owned().await;
                    if state.has_changed().unwrap_or(true) {
                        drop(effect_guard);
                        continue;
                    }
                    let continuity = match output.as_ref() {
                        Some(output) => output.continuity_snapshot(),
                        None => continuity,
                    };
                    if let Err(error) = validate_pending_steer_target(
                        &control, continuity.as_ref(), &thread_id, &turn_id,
                        &pending_requests, &scope, &effect_guard, &turn_interrupted,
                    ).await {
                        return Ok(CodexTurnControlApplied {
                            result: failed_control_result(
                                &control,
                                Some(thread_id),
                                Some(turn_id),
                                &error,
                            ),
                            disposition: CodexTurnControlDisposition::Applied,
                            output,
                            handoff_barrier: Some(handoff_barrier),
                            durable_interrupt_handoff: None,
                            effect_guard: Some(effect_guard),
                        });
                    }
                    if control.requires_external_effect() {
                        before_external_effect(CodexTurnControlEffectIntent::Initial)?;
                    }
                    let ControlTargetApplication {
                        mut result,
                        disposition,
                    } = apply_control_to_target(
                        control,
                        handle,
                        request_ids,
                        thread_id,
                        turn_id,
                        continuity,
                        &mut before_external_effect,
                    )
                    .await?;
                    if result.acknowledged && result.action == "steer"
                        && let Some(output) = &output {
                        output.record_control_continuity(&result);
                    }
                    let mut durable_interrupt_handoff = None;
                    if result.acknowledged && matches!(result.action.as_str(), "interrupt" | "stop")
                    {
                        turn_interrupted.store(true, std::sync::atomic::Ordering::Release);
                        let retained_effect = retain_interrupted_effect_request(
                            &pending_requests,
                            &scope,
                            result.thread_id.as_deref(),
                            result.turn_id.as_deref(),
                        )
                        .await;
                        if !retained_effect {
                            self.finish();
                        }
                        reconcile_interrupted_control_result(&mut result, retained_effect);
                        if let Some(output) = &output {
                            output.record_control_continuity(&result);
                        }
                        if retained_effect {
                            let durable_handoff = ProviderHandoffBarrier::new();
                            self.publish(CodexTurnControlTarget::InterruptedPending {
                                output: output.as_ref().map(Arc::downgrade).unwrap_or_default(),
                                durable_handoff: durable_handoff.clone(),
                                thread_id: result
                                    .thread_id
                                    .clone()
                                    .expect("acknowledged interrupt has thread id"),
                                turn_id: result
                                    .turn_id
                                    .clone()
                                    .expect("acknowledged interrupt has turn id"),
                            });
                            durable_interrupt_handoff = Some(durable_handoff);
                        }
                    }
                    return Ok(CodexTurnControlApplied {
                        result,
                        disposition,
                        output,
                        handoff_barrier: Some(handoff_barrier),
                        durable_interrupt_handoff,
                        effect_guard: Some(effect_guard),
                    });
                }
                CodexTurnControlTarget::Suspended {
                    handle,
                    request_ids,
                    pending_requests,
                    scope,
                    output,
                    continuity,
                    handoff_barrier,
                    effect_gate,
                    turn_interrupted,
                    thread_id,
                    turn_id,
                } => {
                    let output = output.upgrade();
                    let effect_guard = effect_gate.lock_owned().await;
                    if state.has_changed().unwrap_or(true) {
                        drop(effect_guard);
                        continue;
                    }
                    let continuity = match output.as_ref() {
                        Some(output) => output.continuity_snapshot(),
                        None => continuity,
                    };
                    if let Err(error) = validate_pending_steer_target(
                        &control, continuity.as_ref(), &thread_id, &turn_id,
                        &pending_requests, &scope, &effect_guard, &turn_interrupted,
                    ).await {
                        return Ok(CodexTurnControlApplied {
                            result: failed_control_result(
                                &control,
                                Some(thread_id),
                                Some(turn_id),
                                &error,
                            ),
                            disposition: CodexTurnControlDisposition::Applied,
                            output,
                            handoff_barrier: Some(handoff_barrier),
                            durable_interrupt_handoff: None,
                            effect_guard: Some(effect_guard),
                        });
                    }
                    if control.requires_external_effect() {
                        before_external_effect(CodexTurnControlEffectIntent::Initial)?;
                    }
                    let ControlTargetApplication {
                        mut result,
                        disposition,
                    } = apply_control_to_target(
                        control,
                        handle,
                        request_ids,
                        thread_id,
                        turn_id,
                        continuity,
                        &mut before_external_effect,
                    )
                    .await?;
                    if result.acknowledged && result.action == "steer"
                        && let Some(output) = &output {
                        output.record_control_continuity(&result);
                    }
                    let mut durable_interrupt_handoff = None;
                    if result.acknowledged && matches!(result.action.as_str(), "interrupt" | "stop")
                    {
                        turn_interrupted.store(true, std::sync::atomic::Ordering::Release);
                        let retained_effect = retain_interrupted_effect_request(
                            &pending_requests,
                            &scope,
                            result.thread_id.as_deref(),
                            result.turn_id.as_deref(),
                        )
                        .await;
                        if !retained_effect {
                            self.finish();
                        }
                        reconcile_interrupted_control_result(&mut result, retained_effect);
                        if let Some(output) = &output {
                            output.record_control_continuity(&result);
                        }
                        if retained_effect {
                            let durable_handoff = ProviderHandoffBarrier::new();
                            self.publish(CodexTurnControlTarget::InterruptedPending {
                                output: output.as_ref().map(Arc::downgrade).unwrap_or_default(),
                                durable_handoff: durable_handoff.clone(),
                                thread_id: result
                                    .thread_id
                                    .clone()
                                    .expect("acknowledged interrupt has thread id"),
                                turn_id: result
                                    .turn_id
                                    .clone()
                                    .expect("acknowledged interrupt has turn id"),
                            });
                            durable_interrupt_handoff = Some(durable_handoff);
                        }
                    }
                    return Ok(CodexTurnControlApplied {
                        result,
                        disposition,
                        output,
                        handoff_barrier: Some(handoff_barrier),
                        durable_interrupt_handoff,
                        effect_guard: Some(effect_guard),
                    });
                }
            }
        }
    }
}

pub(crate) async fn validate_pending_steer_target(
    control: &CodexTurnControl,
    continuity: Option<&CodexContinuityState>,
    thread_id: &str,
    turn_id: &str,
    pending_requests: &PendingServerRequests,
    scope: &TransportStateScope,
    effect_guard: &tokio::sync::OwnedMutexGuard<()>,
    turn_interrupted: &Arc<std::sync::atomic::AtomicBool>,
) -> Result<(), String> {
    if !matches!(control, CodexTurnControl::Steer { .. }) { return Ok(()); }
    let Some(state) = continuity else { return Ok(()); };
    if state.interrupted_pending.is_some() || turn_interrupted.load(std::sync::atomic::Ordering::Acquire) {
        return Err("the interrupted provider turn cannot accept steering".into());
    }
    if state.pending.is_none() { return Ok(()); }
    let requests = pending_requests.lock().await;
    let live = requests.iter().filter(|((candidate, _), _)| candidate == scope).collect::<Vec<_>>();
    if live.len() != state.outstanding_len() {
        return Err("steering has no exact live pending app-server request".into());
    }
    for durable in state.outstanding() {
        let mut matching = live.iter().filter(|(_, pending)| pending_coordinates_match(Some(durable), pending));
        let Some(((_, key), pending)) = matching.next() else {
            return Err("steering has no exact live pending app-server request".into());
        };
        if matching.next().is_some() || key != &pending.key || state.thread_id != thread_id
            || pending.thread_id != thread_id || pending.turn_id != turn_id
            || !Arc::ptr_eq(&pending.effect_gate, tokio::sync::OwnedMutexGuard::mutex(effect_guard))
            || !Arc::ptr_eq(&pending.turn_interrupted, turn_interrupted)
        {
            return Err("steering target differs from the exact pending app-server request".into());
        }
    }
    Ok(())
}

pub(crate) struct ControlTargetApplication {
    pub(crate) result: CodexTurnControlResult,
    pub(crate) disposition: CodexTurnControlDisposition,
}

pub(crate) async fn apply_control_to_target(
    control: CodexTurnControl,
    handle: AppServerRequestHandle,
    request_ids: Arc<std::sync::atomic::AtomicI64>,
    thread_id: String,
    turn_id: String,
    continuity: Option<CodexContinuityState>,
    before_external_effect: &mut impl FnMut(CodexTurnControlEffectIntent) -> Result<(), String>,
) -> Result<ControlTargetApplication, String> {
    match control {
        CodexTurnControl::Inject { control_id, items } => {
            let request = ClientRequest::ThreadInjectItems {
                request_id: RequestId::Integer(request_ids.fetch_add(1, std::sync::atomic::Ordering::SeqCst)),
                params: ThreadInjectItemsParams { thread_id: thread_id.clone(), items },
            };
            let (acknowledged, detail) = match handle.request_typed::<ThreadInjectItemsResponse>(request).await {
                Ok(_) => (true, serde_json::json!({"queued": true, "turn_triggered": false})),
                Err(error) => (false, serde_json::json!({"error": error.to_string()})),
            };
            Ok(ControlTargetApplication {
                result: CodexTurnControlResult {
                    control_id, action: "queue_message".into(), acknowledged,
                    thread_id: Some(thread_id), turn_id: Some(turn_id), detail, continuity: None,
                },
                disposition: CodexTurnControlDisposition::Applied,
            })
        }
        CodexTurnControl::Steer { control_id, input, source_artifact } => {
            let next_continuity = source_artifact.map(|source| {
                let mut state = continuity.clone().ok_or("user steering has no native continuity")?;
                state.context_generation = state.context_generation.checked_add(1)
                    .ok_or("user steering exhausted its context generation")?;
                state.turn_input = Some(source);
                Ok::<_, String>(state)
            }).transpose()?;
            let first = steer_attempt_application_with_retarget(
                control_id.clone(),
                thread_id.clone(),
                turn_id.clone(),
                handle
                    .request_typed::<TurnSteerResponse>(turn_steer_request(
                        &request_ids,
                        &thread_id,
                        &control_id,
                        input.clone(),
                        &turn_id,
                    ))
                    .await,
                continuity.as_ref().is_none_or(|state| state.pending.is_none()),
            );
            let SteerAttemptApplication::Retarget { actual_turn_id } = first else {
                let SteerAttemptApplication::Complete(mut application) = first else {
                    unreachable!()
                };
                if continuity.as_ref().is_some_and(|state| state.pending.is_some())
                    && matches!(application.disposition, CodexTurnControlDisposition::DeferredTurn(_))
                {
                    application.disposition = CodexTurnControlDisposition::DeferredTurn(CodexDeferredTurnCause::PendingSettlement);
                }
                return Ok(retain_steer_source(application, next_continuity));
            };
            before_external_effect(CodexTurnControlEffectIntent::Retarget {
                expected_turn_id: turn_id,
                actual_turn_id: actual_turn_id.clone(),
            })?;
            let second = steer_attempt_application_with_retarget(
                control_id.clone(),
                thread_id.clone(),
                actual_turn_id.clone(),
                handle
                    .request_typed::<TurnSteerResponse>(turn_steer_request(
                        &request_ids,
                        &thread_id,
                        &control_id,
                        input,
                        &actual_turn_id,
                    ))
                    .await,
                false,
            );
            match second {
                SteerAttemptApplication::Complete(application) => Ok(retain_steer_source(application, next_continuity)),
                SteerAttemptApplication::Retarget { .. } => unreachable!(),
            }
        }
        CodexTurnControl::Interrupt { control_id, action } => {
            let request = ClientRequest::TurnInterrupt {
                request_id: RequestId::Integer(
                    request_ids.fetch_add(1, std::sync::atomic::Ordering::SeqCst),
                ),
                params: TurnInterruptParams {
                    thread_id: thread_id.clone(),
                    turn_id: turn_id.clone(),
                },
            };
            let result = match handle.request_typed::<TurnInterruptResponse>(request).await {
                Ok(_) => match interrupted_continuity(continuity) {
                    Ok((continuity, retained_effect)) => CodexTurnControlResult {
                        control_id,
                        action,
                        acknowledged: true,
                        thread_id: Some(thread_id),
                        turn_id: Some(turn_id),
                        detail: serde_json::json!({
                            "interrupted": true,
                            "exact_result_pending": retained_effect,
                        }),
                        continuity,
                    },
                    Err(error) => CodexTurnControlResult {
                        control_id,
                        action,
                        acknowledged: false,
                        thread_id: Some(thread_id),
                        turn_id: Some(turn_id),
                        detail: serde_json::json!({
                            "error": error,
                            "external_interrupt_acknowledged": true,
                        }),
                        continuity: None,
                    },
                },
                Err(error) => {
                    let terminal = read_exact_terminal_turn(&handle, &request_ids, &thread_id, &turn_id).await;
                    match terminal {
                        Ok(Some(terminal)) => {
                            let (mut continuity, retained_effect) = interrupted_continuity(continuity)?;
                            if let Some(continuity) = continuity.as_mut().filter(|state| state.parallel_pending.is_empty()) {
                                continuity.native_terminal = Some(terminal.clone());
                            }
                            CodexTurnControlResult {
                                control_id, action, acknowledged: true,
                                thread_id: Some(thread_id), turn_id: Some(turn_id),
                                detail: serde_json::json!({
                                    "already_terminal": true,
                                    "terminal_evidence_source": "thread/read",
                                    "native_terminal": terminal,
                                    "interrupt_error": error.to_string(),
                                    "exact_result_pending": retained_effect,
                                }),
                                continuity,
                            }
                        }
                        terminal => CodexTurnControlResult {
                            control_id, action, acknowledged: false,
                            thread_id: Some(thread_id), turn_id: Some(turn_id),
                            detail: serde_json::json!({
                                "error": error.to_string(),
                                "terminal_read_error": terminal.err(),
                            }),
                            continuity: None,
                        },
                    }
                },
            };
            Ok(ControlTargetApplication {
                result,
                disposition: CodexTurnControlDisposition::Applied,
            })
        }
        CodexTurnControl::Unsupported { control_id, action } => Ok(ControlTargetApplication {
            result: CodexTurnControlResult {
                control_id,
                action,
                acknowledged: false,
                thread_id: Some(thread_id),
                turn_id: Some(turn_id),
                detail: serde_json::json!({
                    "error": "Codex app-server exposes no in-flight pause/resume contract; no provider effect was claimed"
                }),
                continuity: None,
            },
            disposition: CodexTurnControlDisposition::Applied,
        }),
    }
}

pub(crate) fn turn_steer_request(
    request_ids: &std::sync::atomic::AtomicI64,
    thread_id: &str,
    client_user_message_id: &str,
    input: Vec<UserInput>,
    expected_turn_id: &str,
) -> ClientRequest {
    ClientRequest::TurnSteer {
        request_id: RequestId::Integer(
            request_ids.fetch_add(1, std::sync::atomic::Ordering::SeqCst),
        ),
        params: TurnSteerParams {
            thread_id: thread_id.to_string(),
            client_user_message_id: Some(client_user_message_id.to_string()),
            input,
            responsesapi_client_metadata: None,
            additional_context: None,
            expected_turn_id: expected_turn_id.to_string(),
        },
    }
}

pub(crate) enum SteerAttemptApplication {
    Complete(ControlTargetApplication),
    Retarget { actual_turn_id: String },
}

fn retain_steer_source(
    mut application: ControlTargetApplication,
    continuity: Option<CodexContinuityState>,
) -> ControlTargetApplication {
    if application.result.acknowledged {
        application.result.continuity = continuity;
    }
    application
}

pub(crate) fn steer_attempt_application_with_retarget(
    control_id: String,
    thread_id: String,
    turn_id: String,
    response: Result<TurnSteerResponse, TypedRequestError>,
    allow_retarget: bool,
) -> SteerAttemptApplication {
    let (result, disposition) = match response {
        Ok(response) if response.turn_id == turn_id => (
            CodexTurnControlResult {
                control_id,
                action: "steer".to_string(),
                acknowledged: true,
                thread_id: Some(thread_id),
                turn_id: Some(turn_id),
                detail: serde_json::json!({
                    "accepted_turn_id": response.turn_id,
                    "stage": "accepted_by_app_server",
                    "source_event": "turn/steer",
                    "consumption_proven": false,
                }),
                continuity: None,
            },
            CodexTurnControlDisposition::Applied,
        ),
        Ok(response) => (
            CodexTurnControlResult {
                control_id,
                action: "steer".to_string(),
                acknowledged: false,
                thread_id: Some(thread_id),
                turn_id: Some(turn_id.clone()),
                detail: serde_json::json!({
                    "error": "app-server acknowledged steering against a different turn",
                    "accepted_turn_id": response.turn_id,
                    "expected_turn_id": turn_id,
                }),
                continuity: None,
            },
            CodexTurnControlDisposition::Applied,
        ),
        Err(error) => {
            if let Some(TurnSteerRace::ExpectedTurnMismatch { actual_turn_id }) =
                turn_steer_race(&error, &turn_id)
                && allow_retarget
                && actual_turn_id != turn_id
            {
                return SteerAttemptApplication::Retarget { actual_turn_id };
            }
            let missing = matches!(
                turn_steer_race(&error, &turn_id),
                Some(TurnSteerRace::Missing)
            );
            let mut detail = if missing {
                serde_json::json!({
                    "error": error.to_string(),
                    "deferred_to_next_turn": true,
                    "reason": "no_active_turn",
                })
            } else {
                serde_json::json!({
                    "error": error.to_string(),
                    "retarget_retry_exhausted": !allow_retarget,
                })
            };
            if let TypedRequestError::Server { method, source } = &error {
                detail["upstream_error"] = serde_json::json!({
                    "method": method,
                    "code": source.code,
                    "message": source.message,
                    "data": source.data,
                });
            }
            (
                CodexTurnControlResult {
                    control_id,
                    action: "steer".to_string(),
                    acknowledged: false,
                    thread_id: Some(thread_id),
                    turn_id: Some(turn_id),
                    detail,
                    continuity: None,
                },
                if missing {
                    CodexTurnControlDisposition::DeferredTurn(
                        CodexDeferredTurnCause::TerminalTurnMoved,
                    )
                } else {
                    CodexTurnControlDisposition::Applied
                },
            )
        }
    };
    SteerAttemptApplication::Complete(ControlTargetApplication {
        result,
        disposition,
    })
}

pub(crate) enum TurnSteerRace {
    Missing,
    ExpectedTurnMismatch { actual_turn_id: String },
}

pub(crate) fn turn_steer_race(error: &TypedRequestError, expected_turn_id: &str) -> Option<TurnSteerRace> {
    let TypedRequestError::Server { method, source } = error else {
        return None;
    };
    if method != "turn/steer" || source.code != -32600 {
        return None;
    }
    if source.message == "no active turn to steer" {
        return Some(TurnSteerRace::Missing);
    }
    let prefix = "expected active turn id `";
    let separator = "` but found `";
    let (expected, actual) = source.message.strip_prefix(prefix)?.split_once(separator)?;
    let actual = actual.strip_suffix('`')?;
    (expected == expected_turn_id
        && !actual.is_empty()
        && actual.trim() == actual
        && !actual.contains(['`', '\r', '\n', '\0']))
    .then(|| {
        TurnSteerRace::ExpectedTurnMismatch {
            actual_turn_id: actual.to_string(),
        }
    })
}

pub(crate) fn interrupted_continuity(
    continuity: Option<CodexContinuityState>,
) -> Result<(Option<CodexContinuityState>, bool), String> {
    let Some(mut continuity) = continuity else {
        return Ok((None, false));
    };
    continuity.context_generation = continuity
        .context_generation
        .checked_add(1)
        .ok_or_else(|| "Codex interrupt continuity context generation overflowed".to_string())?;
    let interrupted = continuity.pending.take();
    let retained_effect = interrupted
        .as_ref()
        .is_some_and(interrupted_pending_has_external_result);
    continuity.interrupted_pending = if retained_effect { interrupted } else { None };
    Ok((Some(continuity), retained_effect))
}

pub(crate) fn interrupted_pending_has_external_result(pending: &CodexPendingContinuity) -> bool {
    matches!(
        pending,
        CodexPendingContinuity::DynamicTool { .. }
            | CodexPendingContinuity::InterfaceOperation { .. }
    )
}

pub(crate) fn reconcile_durable_interrupted_continuity(
    recovered: Option<CodexContinuityState>,
    durable: Option<CodexContinuityState>,
) -> Result<(Option<CodexContinuityState>, bool), InterfaceDriverError> {
    let Some(durable) = durable else {
        return Ok((recovered, false));
    };
    if durable.pending.is_some()
        || durable
            .interrupted_pending
            .as_ref()
            .is_none_or(|pending| !interrupted_pending_has_external_result(pending))
    {
        return Err(InterfaceDriverError::FailedClosed(
            "durable interrupt arbitration has no exact external result correlation".to_string(),
        ));
    }
    let Some(recovered) = recovered else {
        return Ok((Some(durable), true));
    };
    if codex_continuity_retains_interrupted_effect(&durable, &recovered) {
        return Ok((Some(recovered), false));
    }
    let next_generation = recovered.context_generation.checked_add(1).ok_or_else(|| {
        InterfaceDriverError::FailedClosed(
            "Codex durable interrupt arbitration context generation overflowed".to_string(),
        )
    })?;
    if recovered.provider_interface != durable.provider_interface
        || recovered.run != durable.run
        || recovered.thread_id != durable.thread_id
        || recovered.thread_shape_fingerprint != durable.thread_shape_fingerprint
        || recovered.static_context_fingerprint != durable.static_context_fingerprint
        || recovered.interrupted_pending.is_some()
        || recovered.pending.as_ref() != durable.interrupted_pending.as_ref()
        || recovered.parallel_pending != durable.parallel_pending
        || durable.context_generation != next_generation
    {
        return Err(InterfaceDriverError::FailedClosed(
            "durable interrupt arbitration conflicts with the invocation continuity chain"
                .to_string(),
        ));
    }
    Ok((Some(durable), true))
}

pub(crate) fn reconcile_interrupted_control_result(
    result: &mut CodexTurnControlResult,
    retained_effect: bool,
) {
    if !result.acknowledged || !matches!(result.action.as_str(), "interrupt" | "stop") {
        return;
    }
    if let Some(detail) = result.detail.as_object_mut() {
        detail.insert(
            "exact_result_pending".to_string(),
            serde_json::Value::Bool(retained_effect),
        );
    }
    if retained_effect {
        return;
    }
    if let Some(continuity) = result.continuity.as_mut() {
        continuity.interrupted_pending = None;
    }
}

pub(crate) async fn retain_interrupted_effect_request(
    pending_requests: &PendingServerRequests,
    scope: &TransportStateScope,
    thread_id: Option<&str>,
    turn_id: Option<&str>,
) -> bool {
    let mut requests = pending_requests.lock().await;
    let mut retained_effect = false;
    requests.retain(|(candidate_scope, _), pending| {
        let matches_target = candidate_scope == scope
            && thread_id.is_none_or(|expected| pending.thread_id == expected)
            && turn_id.is_none_or(|expected| pending.turn_id == expected);
        if matches_target
            && matches!(
                pending.kind,
                PendingServerRequestKind::DynamicTool { .. }
                    | PendingServerRequestKind::InterfaceOperation { .. }
            )
        {
            retained_effect = true;
            return true;
        }
        !matches_target
    });
    retained_effect
}

pub(crate) fn failed_control_result(
    control: &CodexTurnControl,
    thread_id: Option<String>,
    turn_id: Option<String>,
    error: &str,
) -> CodexTurnControlResult {
    CodexTurnControlResult {
        control_id: control.control_id().to_string(),
        action: control.action().to_string(),
        acknowledged: false,
        thread_id,
        turn_id,
        detail: serde_json::json!({ "error": error }),
        continuity: None,
    }
}
