use super::*;

#[derive(Clone, Copy)]
pub(crate) struct ResultArrival<'a, 's> {
    pub(crate) turn_context: TurnContext<'a, 's>,
    pub(crate) sink: &'a TurnSink<'s>,
    pub(crate) runtime: &'a RuntimeInvocationMetadata,
    pub(crate) input: &'a InterfaceInvocationEnvelope,
    pub(crate) scope: &'a TransportStateScope,
    pub(crate) lifecycle: &'a Option<LifecycleBinding>,
    pub(crate) lifecycle_payloads: &'a Vec<ArtifactBindingPayload>,
    pub(crate) lifecycle_resumed_provider_input: bool,
    pub(crate) recovered: &'a Option<CodexContinuityState>,
    pub(crate) context_generation: u64,
    pub(crate) current_session_context: &'a Arc<Vec<SessionContextArtifact>>,
    pub(crate) effective_manifest_label: &'a String,
    pub(crate) effective_request_bytes: &'a Vec<u8>,
    pub(crate) effective_source_artifact: &'a ArtifactRef,
    pub(crate) interrupted_after_artifact: &'a Option<String>,
    pub(crate) source_interface: &'a Option<InterfaceId>,
}

#[derive(Default)]
pub(crate) struct ArrivalContinuations {
    pub(crate) tool: Option<RecoveredToolContinuation>,
    pub(crate) cut_turn: Option<CutTurnContinuation>,
    pub(crate) operator: Option<(OperatorResponseArtifact, CodexPendingContinuity)>,
}

pub(crate) enum Arrival {
    Settled,
    Continue,
}

impl CodexAppServerInterfaceDriver {
    pub(crate) async fn arrive(
        &self,
        frame: ResultArrival<'_, '_>,
        provider_input: &ProviderInput,
        continuations: &mut ArrivalContinuations,
    ) -> Result<Arrival, InterfaceDriverError> {
        let ResultArrival {
            turn_context,
            sink,
            runtime,
            input,
            scope,
            recovered,
            context_generation,
            effective_manifest_label,
            effective_source_artifact,
            ..
        } = frame;
        match provider_input {
            ProviderInput::ToolResult(result) => {
                return self.arrive_tool_result(frame, result, continuations).await;
            }
            ProviderInput::InterfaceOperationResult(result) => {
                return self
                    .arrive_interface_operation_result(frame, result, continuations)
                    .await;
            }
            ProviderInput::InterfaceContext(context) => {
                if self.has_pending_request(&scope).await
                    || recovered
                        .as_ref()
                        .is_some_and(|state| state.pending.is_some())
                {
                    return Err(InterfaceDriverError::FailedClosed(
                        "interface context artifact cannot replace an unresolved app-server request"
                            .to_string(),
                    ));
                }
                return sink
                    .send(CodexTurnOutput::client(
                        INTERFACE_CONTEXT_UPDATED_LABEL,
                        serde_json::json!({
                            "interface": context.interface,
                            "source_label": context.label,
                            "source_artifact": effective_source_artifact.clone(),
                            "content": context.content,
                        }),
                    ))
                    .map(|()| Arrival::Settled);
            }
            ProviderInput::OperatorResponse(response) => {
                validate_operator_response_evidence(
                    &self.operator_continuity()?,
                    &effective_manifest_label,
                    &response,
                    input.metadata().map_err(|error| {
                        InterfaceDriverError::Protocol(format!(
                            "operator response has no invocation metadata: {error}"
                        ))
                    })?,
                )?;
                if self
                    .resume_operator_response(
                        turn_context,
                        response.clone(),
                        recovered.as_ref(),
                        context_generation,
                    )
                    .await?
                {
                    return Ok(Arrival::Settled);
                }
                if let Some(pending) = recovered.as_ref().and_then(|state| state.pending.as_ref()) {
                    validate_recovered_operator_response(response, pending)?;
                    if matches!(response, OperatorResponseArtifact::UserInput { .. }) {
                        let state = recovered.as_ref().expect("pending continuity");
                        if state.context_generation != context_generation || state.interrupted_pending.is_some()
                            || state.native_terminal.is_some() || state.tool_operator_request.is_some()
                        {
                            return Err(InterfaceDriverError::FailedClosed("question response changed its pending native context".into()));
                        }
                        let handle = self.client.lock().await.request_handle();
                        if let Some(terminal) = read_exact_terminal_turn(&handle, &self.next_request_id,
                            &state.thread_id, pending_turn_id(pending)).await.map_err(InterfaceDriverError::FailedClosed)?
                            && terminal.exact_turn(&state.thread_id, Some(pending_turn_id(pending)))?.status == TurnStatus::Interrupted
                        {
                            let request = exact_pending_question(&frame.current_session_context, state, response)?;
                            let events = take_native_terminal_events(&mut *self.client.lock().await, &terminal, &state.thread_id, pending_turn_id(pending))?;
                            continuations.cut_turn = Some(CutTurnContinuation {
                                context: cut_turn_context(&frame.current_session_context, effective_source_artifact, state,
                                    "request_user_input", effective_manifest_label, frame.effective_request_bytes, self.model_result_projection()?)?,
                                activity: CodexTurnOutput::client(TURN_OPERATOR_REQUEST_RESOLVED_LABEL, serde_json::json!({
                                    "request_id": response.request_id()?, "request_artifact": request,
                                    "response_artifact": effective_source_artifact, "thread_id": state.thread_id,
                                    "turn_id": pending_turn_id(pending), "recovery": "interrupted_question",
                                    "native_terminal": terminal,
                                })).with_native_terminal_events(events),
                                activity_already_admitted: false,
                            });
                            return Ok(Arrival::Continue);
                        }
                    }
                    continuations.operator = Some((response.clone(), pending.clone()));
                }
            }
            ProviderInput::Turn(_) | ProviderInput::Resume { .. } => {
                if self.has_pending_request(&scope).await {
                    return Err(InterfaceDriverError::FailedClosed(format!(
                        "run '{}' sent a new provider turn while an app-server request still awaits its artifact result",
                        runtime.run.as_str()
                    )));
                }
                if recovered
                    .as_ref()
                    .is_some_and(|state| state.pending.is_some())
                {
                    return Err(InterfaceDriverError::FailedClosed(
                        "provider life dispatched an operator turn before its durable pending result reconciled"
                            .to_string(),
                    ));
                }
            }
        }
        Ok(Arrival::Continue)
    }

    pub(crate) async fn arrive_tool_result(
        &self,
        frame: ResultArrival<'_, '_>,
        result: &ToolResultArtifact,
        continuations: &mut ArrivalContinuations,
    ) -> Result<Arrival, InterfaceDriverError> {
        let ResultArrival {
            turn_context,
            sink,
            runtime,
            lifecycle,
            lifecycle_payloads,
            lifecycle_resumed_provider_input,
            recovered,
            context_generation,
            current_session_context,
            effective_manifest_label,
            effective_request_bytes,
            effective_source_artifact,
            interrupted_after_artifact,
            ..
        } = frame;
        let ArrivalContinuations {
            tool: recovered_tool_continuation,
            cut_turn: cut_turn_continuation,
            ..
        } = continuations;
        if let Some(binding) = lifecycle.as_ref()
            && let Some(correlation_id) = result.correlation_id.as_deref()
            && let Some(event) = binding
                .bound_event_for_correlation(&lifecycle_payloads, correlation_id)
                .map_err(InterfaceDriverError::FailedClosed)?
        {
            binding
                .validate_command_result(&event, &result.tool)
                .map_err(InterfaceDriverError::FailedClosed)?;
            let output = binding
                .normalize_handler_result(
                    &event,
                    "command_result",
                    result.ok,
                    &serde_json::json!({
                        "ok": result.ok,
                        "body": result.body,
                        "error": result.error,
                    }),
                )
                .map_err(InterfaceDriverError::FailedClosed)?;
            sink.send_lifecycle(output)?;
            return Ok(Arrival::Settled);
        }
        if !lifecycle_resumed_provider_input && let Some(binding) = lifecycle.as_ref() {
            let post = LifecycleTrigger {
                event: LifecycleEventName::PostToolUse,
                trigger: "tool_result_arrival".to_string(),
                match_values: vec![result.tool.clone()],
                subject_artifact: effective_source_artifact.clone(),
                data: serde_json::json!({
                    "tool": result.tool,
                    "correlation_id": result.correlation_id,
                    "ok": result.ok,
                    "body": result.body,
                    "error": result.error,
                }),
                continuation: Some(LifecycleContinuation::ProviderInput {
                    manifest_label: effective_manifest_label.clone(),
                    payload: serde_json::from_slice(&effective_request_bytes).map_err(|error| {
                        InterfaceDriverError::Protocol(format!(
                            "post_tool_use continuation input is not JSON: {error}"
                        ))
                    })?,
                    source_artifact: effective_source_artifact.clone(),
                }),
            };
            if emit_lifecycle_trigger(&sink, Some(binding), runtime.run.as_str(), post)? {
                return Ok(Arrival::Settled);
            }
        }
        if self
            .resume_tool_result(
                turn_context,
                result.clone(),
                recovered.as_ref(),
                context_generation,
                interrupted_after_artifact.as_deref() == Some(TURN_DYNAMIC_TOOL_CALL_LABEL),
            )
            .await?
        {
            return Ok(Arrival::Settled);
        }
        let source = recovered.as_ref();
        let recovered = &result
            .correlation_id
            .as_ref()
            .and_then(|call| recovered.as_ref()?.focused(&format!("tool:{call}")))
            .or_else(|| recovered.clone());
        if let Some(interrupted) = recovered
            .as_ref()
            .and_then(|state| state.interrupted_pending.as_ref())
        {
            validate_recovered_tool_result(result, interrupted)?;
            let CodexPendingContinuity::DynamicTool {
                native_operation: _,
                turn_id,
                call_id,
                tool,
            } = interrupted
            else {
                unreachable!("validated interrupted tool result has dynamic-tool state")
            };
            let native_namespace = self.native_tool_namespace()?.name;
            let activity = (interrupted_after_artifact.as_deref()
                != Some(TURN_DYNAMIC_TOOL_CALL_LABEL))
            .then(|| {
                CodexTurnOutput::dynamic_tool_completed_after_interrupt(
                    native_namespace,
                    tool.clone(),
                    call_id.clone(),
                    recovered
                        .as_ref()
                        .expect("interrupted continuity exists")
                        .thread_id
                        .clone(),
                    turn_id.clone(),
                    result.ok,
                    result.body.clone(),
                    result.error.clone(),
                )
            });
            sink.bind_live_stream(
                &recovered
                    .as_ref()
                    .expect("interrupted continuity exists")
                    .thread_id,
                turn_id,
            )?;
            return sink
                .send_interrupted_result(
                    activity,
                    interrupted_after_artifact.as_deref() == Some(TURN_DYNAMIC_TOOL_CALL_LABEL),
                    CodexTurnOutput::interrupted_result_closed(
                        "tool_result",
                        call_id.clone(),
                        recovered
                            .as_ref()
                            .expect("interrupted continuity exists")
                            .thread_id
                            .clone(),
                        turn_id.clone(),
                        result.ok,
                        &effective_source_artifact,
                    ),
                )
                .map(|()| Arrival::Settled);
        }
        if let Some(pending) = recovered.as_ref().and_then(|state| state.pending.as_ref()) {
            validate_recovered_tool_result(result, pending)?;
            let state = recovered.as_ref().ok_or_else(|| {
                InterfaceDriverError::FailedClosed(
                    "cold tool result has no source continuity".into(),
                )
            })?;
            if state.context_generation != context_generation {
                return Err(InterfaceDriverError::FailedClosed(
                    "cold tool result has stale source context generation".into(),
                ));
            }
            let source = source.ok_or_else(|| {
                InterfaceDriverError::FailedClosed("cold tool result has no source continuity".into())
            })?;
            match self
                .complete_native_terminal_result(
                    turn_context,
                    source,
                    &pending_request_key(pending),
                    None,
                    CodexTurnOutput::dynamic_tool_completed_after_interrupt(
                        self.native_tool_namespace()?.name,
                        result.tool.clone(),
                        result.correlation_id.clone().ok_or_else(|| {
                            InterfaceDriverError::FailedClosed(
                                "cold result lost its correlation".into(),
                            )
                        })?,
                        state.thread_id.clone(),
                        pending_turn_id(pending).to_string(),
                        result.ok,
                        result.body.clone(),
                        result.error.clone(),
                    ),
                    interrupted_after_artifact.as_deref() == Some(TURN_DYNAMIC_TOOL_CALL_LABEL),
                    CodexTurnOutput::interrupted_result_closed(
                        "tool_result",
                        result.correlation_id.clone().ok_or_else(|| {
                            InterfaceDriverError::FailedClosed(
                                "cold result lost its correlation".into(),
                            )
                        })?,
                        state.thread_id.clone(),
                        pending_turn_id(pending).to_string(),
                        result.ok,
                        &effective_source_artifact,
                    ),
                    native_pending_operation(pending)?.is_none(),
                )
                .await?
            {
                NativeTerminalCheck::Closed => return Ok(Arrival::Settled),
                NativeTerminalCheck::CutByProcessDeath(activity) => {
                    *cut_turn_continuation = Some(CutTurnContinuation {
                        context: cut_turn_context(
                            &current_session_context,
                            &effective_source_artifact,
                            state,
                            &result.tool,
                            &effective_manifest_label,
                            &effective_request_bytes,
                            self.model_result_projection()?,
                        )?,
                        activity,
                        activity_already_admitted: interrupted_after_artifact
                            .as_deref()
                            .is_some_and(|label| label != PROVIDER_TRANSPORT_DISPATCH_LABEL),
                    });
                }
                NativeTerminalCheck::Open if source.outstanding_len() > 1 => {
                    return Err(lost_parallel_requests());
                }
                NativeTerminalCheck::Open => {}
            }
            let CodexPendingContinuity::DynamicTool {
                native_operation: _,
                turn_id,
                call_id,
                tool,
            } = pending
            else {
                unreachable!("validated recovered tool result has dynamic-tool state")
            };
            if let Some(operation) = native_pending_operation(pending)? {
                let receipt = serde_json::to_value(result)
                    .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
                match operation
                    .advance(&receipt)
                    .map_err(InterfaceDriverError::FailedClosed)?
                {
                    NativeStep::Effect {
                        pending: next,
                        outgoing_bindings,
                        ..
                    } => {
                        emit_native_effect(&sink, turn_id, next, outgoing_bindings, None)?;
                        return Ok(Arrival::Settled);
                    }
                    NativeStep::AmendmentEffect {
                        pending: next,
                        amendment,
                        ..
                    } => {
                        emit_native_effect(&sink, turn_id, next, Vec::new(), Some(amendment))?;
                        return Ok(Arrival::Settled);
                    }
                    NativeStep::Complete { result: value } => {
                        *recovered_tool_continuation = Some(RecoveredToolContinuation {
                            item: recovered_function_call_output(
                                &operation.call_id,
                                recovered_result_output(native_result_content(
                                    &value,
                                    Vec::new(),
                                )?)?,
                            ),
                            activity: CodexTurnOutput::dynamic_tool_completed(
                                self.native_tool_namespace()?.name,
                                operation.operation_name,
                                operation.call_id,
                                recovered
                                    .as_ref()
                                    .expect("recovered continuity")
                                    .thread_id
                                    .clone(),
                                turn_id.clone(),
                                native_result_success(&value),
                                Some(value),
                                None,
                            ),
                            activity_already_admitted: interrupted_after_artifact
                                .as_deref()
                                .is_some_and(|label| label != PROVIDER_TRANSPORT_DISPATCH_LABEL),
                        });
                    }
                    NativeStep::Wait { .. } => {
                        return Err(InterfaceDriverError::FailedClosed(
                            "recovered effect cannot become a new wait".into(),
                        ));
                    }
                }
            } else if cut_turn_continuation.is_none() {
                let namespace = self.native_tool_namespace()?.name;
                *recovered_tool_continuation = Some(RecoveredToolContinuation {
                    item: recovered_function_call_output(
                        call_id,
                        recovered_result_output(dynamic_tool_response_content(
                            result,
                            &effective_source_artifact,
                            self.model_result_projection()?,
                        )?)?,
                    ),
                    activity: CodexTurnOutput::dynamic_tool_completed(
                        namespace,
                        tool.clone(),
                        call_id.clone(),
                        recovered
                            .as_ref()
                            .expect("recovered tool continuity exists")
                            .thread_id
                            .clone(),
                        turn_id.clone(),
                        result.ok,
                        result.body.clone(),
                        result.error.clone(),
                    ),
                    activity_already_admitted: interrupted_after_artifact
                        .as_deref()
                        .is_some_and(|label| label != PROVIDER_TRANSPORT_DISPATCH_LABEL),
                });
            }
        }
        Ok(Arrival::Continue)
    }

    pub(crate) async fn arrive_interface_operation_result(
        &self,
        frame: ResultArrival<'_, '_>,
        result: &InterfaceOperationResultArtifact,
        continuations: &mut ArrivalContinuations,
    ) -> Result<Arrival, InterfaceDriverError> {
        let ResultArrival {
            turn_context,
            sink,
            runtime,
            lifecycle,
            lifecycle_payloads,
            lifecycle_resumed_provider_input,
            recovered,
            context_generation,
            current_session_context,
            effective_manifest_label,
            effective_request_bytes,
            effective_source_artifact,
            interrupted_after_artifact,
            source_interface,
            ..
        } = frame;
        let ArrivalContinuations {
            tool: recovered_tool_continuation,
            cut_turn: cut_turn_continuation,
            ..
        } = continuations;
        if let Some(binding) = lifecycle.as_ref()
            && let Some(event) = binding
                .bound_event_for_correlation(&lifecycle_payloads, &result.correlation_id)
                .map_err(InterfaceDriverError::FailedClosed)?
        {
            binding
                .validate_mcp_result(&event, source_interface.as_ref(), &result.operation)
                .map_err(InterfaceDriverError::FailedClosed)?;
            let output = binding
                .normalize_handler_result(&event, "mcp_result", result.ok, &result.value)
                .map_err(InterfaceDriverError::FailedClosed)?;
            sink.send_lifecycle(output)?;
            return Ok(Arrival::Settled);
        }
        if !lifecycle_resumed_provider_input && let Some(binding) = lifecycle.as_ref() {
            let pending_is_subagent = recovered
                .as_ref()
                .and_then(|state| state.outstanding_call(&format!("tool:{}", result.correlation_id)))
                .is_some_and(|pending| match pending {
                    CodexPendingContinuity::InterfaceOperation { interface, .. } => {
                        binding.is_subagent_interface(interface)
                    }
                    _ => false,
                });
            let continuation = LifecycleContinuation::ProviderInput {
                manifest_label: effective_manifest_label.clone(),
                payload: serde_json::from_slice(&effective_request_bytes).map_err(|error| {
                    InterfaceDriverError::Protocol(format!(
                        "interface-result lifecycle continuation is not JSON: {error}"
                    ))
                })?,
                source_artifact: effective_source_artifact.clone(),
            };
            let mut triggers = vec![LifecycleTrigger {
                event: LifecycleEventName::PostToolUse,
                trigger: "interface_operation_result_arrival".to_string(),
                match_values: vec![result.operation.clone()],
                subject_artifact: effective_source_artifact.clone(),
                data: serde_json::json!({
                    "operation": result.operation,
                    "correlation_id": result.correlation_id,
                    "ok": result.ok,
                    "result": result.value,
                }),
                continuation: Some(continuation.clone()),
            }];
            if pending_is_subagent {
                triggers.push(LifecycleTrigger {
                    event: LifecycleEventName::SubagentStop,
                    trigger: "system_worker_result_arrival".to_string(),
                    match_values: vec![result.operation.clone()],
                    subject_artifact: effective_source_artifact.clone(),
                    data: serde_json::json!({
                        "operation": result.operation,
                        "correlation_id": result.correlation_id,
                        "ok": result.ok,
                        "result": result.value,
                    }),
                    continuation: Some(continuation),
                });
            }
            if emit_lifecycle_trigger_set(&sink, Some(binding), runtime.run.as_str(), triggers)? {
                return Ok(Arrival::Settled);
            }
        }
        if self
            .resume_interface_operation_result(
                turn_context,
                result,
                recovered.as_ref(),
                context_generation,
                interrupted_after_artifact.as_deref() == Some(TURN_DYNAMIC_TOOL_CALL_LABEL),
            )
            .await?
        {
            return Ok(Arrival::Settled);
        }
        let source = recovered.as_ref();
        let recovered = &recovered
            .as_ref()
            .and_then(|state| state.focused(&format!("tool:{}", result.correlation_id)))
            .or_else(|| recovered.clone());
        if let Some(interrupted) = recovered
            .as_ref()
            .and_then(|state| state.interrupted_pending.as_ref())
        {
            validate_recovered_interface_operation_input(result, interrupted)?;
            let CodexPendingContinuity::InterfaceOperation {
                turn_id,
                call_id,
                operation,
                ..
            } = interrupted
            else {
                unreachable!("validated interrupted interface result has interface-operation state")
            };
            let activity = (interrupted_after_artifact.as_deref()
                != Some(TURN_DYNAMIC_TOOL_CALL_LABEL))
            .then(|| {
                CodexTurnOutput::interface_operation_completed_after_interrupt(
                    operation.clone(),
                    call_id.clone(),
                    recovered
                        .as_ref()
                        .expect("interrupted continuity exists")
                        .thread_id
                        .clone(),
                    turn_id.clone(),
                    result.ok,
                    result.value.clone(),
                )
            });
            sink.bind_live_stream(
                &recovered
                    .as_ref()
                    .expect("interrupted continuity exists")
                    .thread_id,
                turn_id,
            )?;
            return sink
                .send_interrupted_result(
                    activity,
                    interrupted_after_artifact.as_deref() == Some(TURN_DYNAMIC_TOOL_CALL_LABEL),
                    CodexTurnOutput::interrupted_result_closed(
                        "interface_operation_result",
                        call_id.clone(),
                        recovered
                            .as_ref()
                            .expect("interrupted continuity exists")
                            .thread_id
                            .clone(),
                        turn_id.clone(),
                        result.ok,
                        &effective_source_artifact,
                    ),
                )
                .map(|()| Arrival::Settled);
        }
        if let Some(pending) = recovered.as_ref().and_then(|state| state.pending.as_ref()) {
            validate_recovered_interface_operation_input(result, pending)?;
            let state = recovered.as_ref().ok_or_else(|| {
                InterfaceDriverError::FailedClosed(
                    "cold interface result has no source continuity".into(),
                )
            })?;
            if state.context_generation != context_generation {
                return Err(InterfaceDriverError::FailedClosed(
                    "cold interface result has stale source context generation".into(),
                ));
            }
            let source = source.ok_or_else(|| {
                InterfaceDriverError::FailedClosed("cold interface result has no source continuity".into())
            })?;
            match self
                .complete_native_terminal_result(
                    turn_context,
                    source,
                    &pending_request_key(pending),
                    None,
                    CodexTurnOutput::interface_operation_completed_after_interrupt(
                        result.operation.clone(),
                        result.correlation_id.clone(),
                        state.thread_id.clone(),
                        pending_turn_id(pending).to_string(),
                        result.ok,
                        result.value.clone(),
                    ),
                    interrupted_after_artifact.as_deref() == Some(TURN_DYNAMIC_TOOL_CALL_LABEL),
                    CodexTurnOutput::interrupted_result_closed(
                        "interface_operation_result",
                        result.correlation_id.clone(),
                        state.thread_id.clone(),
                        pending_turn_id(pending).to_string(),
                        result.ok,
                        &effective_source_artifact,
                    ),
                    true,
                )
                .await?
            {
                NativeTerminalCheck::Closed => return Ok(Arrival::Settled),
                NativeTerminalCheck::CutByProcessDeath(activity) => {
                    *cut_turn_continuation = Some(CutTurnContinuation {
                        context: cut_turn_context(
                            &current_session_context,
                            &effective_source_artifact,
                            state,
                            &result.operation,
                            &effective_manifest_label,
                            &effective_request_bytes,
                            self.model_result_projection()?,
                        )?,
                        activity,
                        activity_already_admitted: interrupted_after_artifact
                            .as_deref()
                            .is_some_and(|label| label != PROVIDER_TRANSPORT_DISPATCH_LABEL),
                    });
                }
                NativeTerminalCheck::Open if source.outstanding_len() > 1 => {
                    return Err(lost_parallel_requests());
                }
                NativeTerminalCheck::Open => {}
            }
            let CodexPendingContinuity::InterfaceOperation {
                turn_id,
                call_id,
                operation,
                ..
            } = pending
            else {
                unreachable!("validated recovered interface result has interface-operation state")
            };
            if cut_turn_continuation.is_none() {
                *recovered_tool_continuation = Some(RecoveredToolContinuation {
                    item: recovered_function_call_output(
                        call_id,
                        recovered_result_output(interface_operation_response_content(
                            result,
                            &effective_source_artifact,
                            self.model_result_projection()?,
                        )?)?,
                    ),
                    activity: CodexTurnOutput::interface_operation_completed(
                        operation.clone(),
                        call_id.clone(),
                        recovered
                            .as_ref()
                            .expect("recovered interface continuity exists")
                            .thread_id
                            .clone(),
                        turn_id.clone(),
                        result.ok,
                        result.value.clone(),
                    ),
                    activity_already_admitted: interrupted_after_artifact
                        .as_deref()
                        .is_some_and(|label| label != PROVIDER_TRANSPORT_DISPATCH_LABEL),
                });
            }
        }
        if recovered_tool_continuation.is_none() && cut_turn_continuation.is_none() {
            return Err(InterfaceDriverError::FailedClosed(
                "interface operation result has no live structured pending request".to_string(),
            ));
        }
        Ok(Arrival::Continue)
    }
}

fn lost_parallel_requests() -> InterfaceDriverError {
    InterfaceDriverError::FailedClosed(
        "the native turn still awaits several app-server requests the provider process lost; one late result cannot settle them"
            .to_string(),
    )
}
