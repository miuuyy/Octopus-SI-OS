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
        if let ProviderInput::ToolResult(result) = &*provider_input
            && let Some(request) = ToolOperatorRequest::from_result(result)?
        {
            let mut state = recovered.clone().ok_or_else(|| {
                InterfaceDriverError::FailedClosed(
                    "application request has no durable native DynamicTool".to_string(),
                )
            })?;
            request.validate_pending(&state)?;
            request.validate_original(
                &input,
                &run_record,
                &current_session_context,
                &hidden_tools,
            )?;
            if state.interrupted_pending.is_some() || state.tool_operator_request.is_some() {
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
            state.tool_operator_request = Some(operator_request);
            sink.set_continuity(state);
            self.retain_tool_operator_wait(&scope, &sink, control, &handoff_barrier, output)
                .await?;
            return Ok(Arrival::Settled);
        }
        if let Some(mut state) = recovered.clone()
            && let Some(mut request) = state.tool_operator_request.clone()
        {
            request.request.validate_pending(&state)?;
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
                    state.tool_operator_request = Some(request);
                    sink.set_continuity(state);
                    self.retain_tool_operator_wait(
                        &scope,
                        &sink,
                        control,
                        &handoff_barrier,
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
