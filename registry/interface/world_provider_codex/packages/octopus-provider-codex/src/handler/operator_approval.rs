use super::*;

pub(crate) struct OperatorApprovalFrame<'a, 's> {
    pub(crate) recovered: &'a Option<CodexContinuityState>,
    pub(crate) input: &'a InterfaceInvocationEnvelope,
    pub(crate) run_record: &'a serde_json::Value,
    pub(crate) current_session_context: &'a Arc<Vec<SessionContextArtifact>>,
    pub(crate) hidden_tools: &'a Vec<String>,
    pub(crate) payload_selection: &'a Vec<ArtifactRef>,
    pub(crate) runtime: &'a RuntimeInvocationMetadata,
    pub(crate) sink: &'a TurnSink<'s>,
    pub(crate) scope: &'a TransportStateScope,
    pub(crate) control: &'a Arc<CodexTurnControlBroker>,
    pub(crate) handoff_barrier: &'a Arc<ProviderHandoffBarrier>,
}

impl CodexAppServerInterfaceDriver {
    pub(crate) async fn route_operator_approval(
        &self,
        frame: OperatorApprovalFrame<'_, '_>,
        provider_input: &mut ProviderInput,
        effective_request_bytes: &mut Vec<u8>,
        effective_manifest_label: &mut String,
    ) -> Result<Arrival, InterfaceDriverError> {
        let OperatorApprovalFrame {
            recovered,
            input,
            run_record,
            current_session_context,
            hidden_tools,
            payload_selection,
            runtime,
            sink,
            scope,
            control,
            handoff_barrier,
        } = frame;
        let outstanding = |call: &str| {
            recovered
                .as_ref()
                .and_then(|state| state.outstanding_call(&format!("tool:{call}")))
        };
        if let Some(request) = OperationOperatorRequest::from_payload(effective_request_bytes)? {
            let pending = outstanding(&request.call_id);
            request.validate_source(input, pending)?;
            let output = request.projection(&self.operator_continuity()?);
            self.retain_operation_operator_wait(scope, sink, control, handoff_barrier, pending, &request.call_id, output)
                .await?;
            return Ok(Arrival::Settled);
        }
        if let ProviderInput::OperatorResponse(OperatorResponseArtifact::Approval { request_id, .. }) = &*provider_input
            && recovered.as_ref().is_none_or(|state| {
                state.held_operator_call(|hold| hold.request.request_id == *request_id).is_none()
            })
            && let Some(call_id) = operation_decision_call(input, request_id)?
        {
            let output = CodexTurnOutput::client(
                INTERFACE_CONTEXT_UPDATED_LABEL,
                serde_json::json!({"operator_response_artifact": input.artifact}),
            );
            self.retain_operation_operator_wait(scope, sink, control, handoff_barrier, outstanding(&call_id), &call_id, output)
                .await?;
            return Ok(Arrival::Settled);
        }
        if let ProviderInput::ToolResult(result) = &*provider_input
            && let Some(request) = ToolOperatorRequest::from_result(result)?
        {
            let mut state = recovered.clone().ok_or_else(|| {
                InterfaceDriverError::FailedClosed(
                    "application request has no durable native DynamicTool".to_string(),
                )
            })?;
            let call = format!("tool:{}", request.call_id);
            let focused = state.focused(&call).ok_or_else(|| {
                InterfaceDriverError::FailedClosed(
                    "application request no longer names an outstanding native DynamicTool".to_string(),
                )
            })?;
            request.validate_pending(&focused)?;
            request.validate_original(
                &input,
                &run_record,
                &current_session_context,
                &hidden_tools,
            )?;
            if state.interrupted_pending.is_some() || state.operator_request_for(&call).is_some() {
                return Err(InterfaceDriverError::FailedClosed(
                    "application request repeats or follows an interrupted native call".to_string(),
                ));
            }
            let config = self.operator_continuity()?;
            let operator_request = ToolOperatorContinuity {
                request: request.clone(),
                payload_artifact_bindings: payload_selection.clone(),
                request_artifact: input.artifact.clone(),
                operator_interface: octopus_run_authoring::bound_client_interface(
                    &run_record,
                    &runtime.interface,
                )
                .map_err(InterfaceDriverError::FailedClosed)?,
                phase: ToolOperatorPhase::AwaitingOperator,
            };
            let output = request.projection(&config);
            state.set_operator_request(&call, Some(operator_request))?;
            sink.set_continuity(state);
            self.retain_tool_operator_wait(&scope, &sink, control, &handoff_barrier, &call, output)
                .await?;
            return Ok(Arrival::Settled);
        }
        if let Some(mut state) = recovered.clone()
            && let Some(call) = held_operator_input_call(&state, provider_input)
        {
            let mut request = state.operator_request_for(&call).cloned().expect("held call has its operator request");
            request.request.validate_pending(&state.focused(&call).expect("held call is outstanding"))?;
            if request.payload_artifact_bindings != *payload_selection {
                return Err(InterfaceDriverError::FailedClosed(
                    "pending operator continuation changed its exact payload selection".into(),
                ));
            }
            match &*provider_input {
                ProviderInput::OperatorResponse(_) => {
                    if state.interrupted_pending.is_some() {
                        return Err(InterfaceDriverError::FailedClosed(
                            "application approval cannot resume an interrupted native call"
                                .to_string(),
                        ));
                    }
                    request.admit(&input, &self.operator_continuity()?)?;
                    state.set_operator_request(&call, Some(request))?;
                    sink.set_continuity(state);
                    self.retain_tool_operator_wait(
                        &scope,
                        &sink,
                        control,
                        &handoff_barrier,
                        &call,
                        CodexTurnOutput::client(
                            INTERFACE_CONTEXT_UPDATED_LABEL,
                            serde_json::json!({
                                "operator_response_artifact": input.artifact,
                            }),
                        ),
                    )
                    .await?;
                    return Ok(Arrival::Settled);
                }
                ProviderInput::ToolResult(result) => {
                    let result = request.resume_result(result, &input)?;
                    *effective_request_bytes = serde_json::to_vec(&result)
                        .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
                    *effective_manifest_label = result.label.clone();
                    *provider_input = ProviderInput::ToolResult(result);
                }
                _ => {
                    return Err(InterfaceDriverError::FailedClosed(
                        "input cannot replace a native application approval continuation"
                            .to_string(),
                    ));
                }
            }
        }
        Ok(Arrival::Continue)
    }
}

pub(crate) fn held_operator_input_call(
    state: &CodexContinuityState,
    input: &ProviderInput,
) -> Option<String> {
    let first = state.held_operator_call(|_| true)?;
    if let Some(call) = state.held_operator_call(|hold| match input {
        ProviderInput::OperatorResponse(response) => response
            .request_id()
            .is_ok_and(|request_id| request_id == hold.request.request_id),
        ProviderInput::ToolResult(result) => {
            result.tool == hold.request.resume.tool
                && result.correlation_id.as_deref() == Some(hold.request.resume.correlation_id.as_str())
        }
        _ => false,
    }) {
        return Some(call);
    }
    let other = match input {
        ProviderInput::ToolResult(result) => result.correlation_id.as_ref().map(|call| format!("tool:{call}")),
        ProviderInput::InterfaceOperationResult(result) => Some(format!("tool:{}", result.correlation_id)),
        _ => None,
    };
    match other {
        Some(call) if state.outstanding_call(&call).is_some() && state.operator_request_for(&call).is_none() => None,
        _ => Some(first),
    }
}
