use super::*;

pub(crate) struct CodexTurnOutput {
    pub(crate) label: String,
    pub(crate) value: serde_json::Value,
    pub(crate) route: ProviderEmissionRoute,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct InterruptedTurnDrainEvidence {
    pub(crate) disposition: &'static str,
    pub(crate) provider_error: Option<serde_json::Value>,
    pub(crate) native_terminal_events: Vec<serde_json::Value>,
    pub(crate) source_continuity: Option<CodexContinuityState>,
}

impl InterruptedTurnDrainEvidence {
    pub(crate) fn transport(disposition: &'static str, provider_error: Option<serde_json::Value>) -> Self {
        Self {
            disposition,
            provider_error,
            native_terminal_events: Vec::new(),
            source_continuity: None,
        }
    }
}

pub(crate) enum InterruptedDrainDecision {
    Continue,
    Complete(InterruptedTurnDrainEvidence),
}

pub(crate) struct InterruptedTurnDrainState {
    pub(crate) thread_id: String,
    pub(crate) turn_id: String,
    pub(crate) provider_error: Option<serde_json::Value>,
    pub(crate) retained_events: Vec<serde_json::Value>,
    pub(crate) deferred_events: Vec<AppServerEvent>,
}

impl InterruptedTurnDrainState {
    pub(crate) fn new(thread_id: String, turn_id: String) -> Self {
        Self {
            thread_id,
            turn_id,
            provider_error: None,
            retained_events: Vec::new(),
            deferred_events: Vec::new(),
        }
    }

    pub(crate) fn observe(&mut self, event: Option<AppServerEvent>) -> InterruptedDrainDecision {
        match event {
            Some(AppServerEvent::ServerNotification(notification)) => {
                let frame = match serde_json::to_value(&notification) {
                    Ok(frame) => frame,
                    Err(error) => {
                        return InterruptedDrainDecision::Complete(
                            InterruptedTurnDrainEvidence::transport(
                                "protocol_drift",
                                Some(serde_json::json!({"error": error.to_string()})),
                            ),
                        );
                    }
                };
                if !native_frame_matches_turn(&frame, &self.thread_id, &self.turn_id) {
                    self.deferred_events.push(AppServerEvent::ServerNotification(notification));
                    return InterruptedDrainDecision::Continue;
                }
                match notification {
                    ServerNotification::Error(error) if error.thread_id == self.thread_id => {
                        match serde_json::to_value(error) {
                            Ok(error) => {
                                self.provider_error = Some(error);
                                InterruptedDrainDecision::Continue
                            }
                            Err(error) => InterruptedDrainDecision::Complete(
                                InterruptedTurnDrainEvidence::transport(
                                    "protocol_drift",
                                    Some(serde_json::json!({"error": error.to_string()})),
                                ),
                            ),
                        }
                    }
                    ServerNotification::TurnCompleted(turn)
                        if turn.thread_id == self.thread_id && turn.turn.id == self.turn_id =>
                    {
                        if !self.retained_events.is_empty() { self.retained_events.push(frame); }
                        let disposition = if self.provider_error.is_some() {
                            "turn_completed_after_provider_error"
                        } else {
                            "turn_completed"
                        };
                        InterruptedDrainDecision::Complete(InterruptedTurnDrainEvidence {
                            disposition,
                            provider_error: self.provider_error.take(),
                            native_terminal_events: std::mem::take(&mut self.retained_events),
                            source_continuity: None,
                        })
                    }
                    _ => InterruptedDrainDecision::Continue,
                }
            }
            Some(AppServerEvent::ServerRequest(request)) => {
                match serde_json::to_value(&request) {
                    Ok(frame) if native_frame_matches_turn(&frame, &self.thread_id, &self.turn_id) => {
                        self.retained_events.push(serde_json::json!({"quarantined_server_request": frame}));
                        InterruptedDrainDecision::Continue
                    }
                    Ok(_) => {
                        self.deferred_events.push(AppServerEvent::ServerRequest(request));
                        InterruptedDrainDecision::Continue
                    }
                    Err(error) => InterruptedDrainDecision::Complete(InterruptedTurnDrainEvidence::transport(
                        "protocol_drift", Some(serde_json::json!({"error":error.to_string()})),
                    )),
                }
            }
            Some(AppServerEvent::UnknownServerNotification(notification)) => {
                InterruptedDrainDecision::Complete(InterruptedTurnDrainEvidence::transport(
                    "unknown_server_notification",
                    Some(serde_json::json!({"method": notification.method})),
                ))
            }
            Some(AppServerEvent::Disconnected { message }) => {
                InterruptedDrainDecision::Complete(InterruptedTurnDrainEvidence::transport(
                    "disconnected",
                    Some(serde_json::json!({
                        "message": message,
                        "provider_error": self.provider_error.take(),
                    })),
                ))
            }
            Some(AppServerEvent::Lagged { skipped }) => {
                InterruptedDrainDecision::Complete(InterruptedTurnDrainEvidence::transport(
                    "event_stream_lagged",
                    Some(serde_json::json!({
                        "skipped": skipped,
                        "provider_error": self.provider_error.take(),
                    })),
                ))
            }
            None => InterruptedDrainDecision::Complete(InterruptedTurnDrainEvidence::transport(
                "event_stream_closed",
                self.provider_error.take(),
            )),
        }
    }
}

impl CodexTurnOutput {
    pub(crate) fn client(label: impl Into<String>, value: serde_json::Value) -> Self {
        Self {
            label: label.into(),
            value,
            route: ProviderEmissionRoute::ClientProjection,
        }
    }

    pub(crate) fn with_verified_inputs(mut self, inputs: Vec<ProviderVerifiedInput>) -> Self {
        self.route = ProviderEmissionRoute::ClientProjectionWithVerifiedInputs { inputs };
        self
    }

    pub(crate) fn lifecycle(output: LifecyclePlannedOutput) -> Self {
        Self {
            label: output.label,
            value: output.value,
            route: provider_route_from_lifecycle(output.route),
        }
    }

    pub(crate) fn final_message(
        thread_id: &str,
        turn_id: &str,
        item_id: &str,
        text: &str,
        context_window_usage: Option<ContextWindowUsage>,
    ) -> Self {
        let mut value = serde_json::json!({
            "thread_id": thread_id,
            "turn_id": turn_id,
            "item_id": item_id,
            "text": text,
            "phase": MessagePhase::FinalAnswer,
            "lifecycle": "completed",
        });
        if let Some(usage) = context_window_usage {
            value
                .as_object_mut()
                .expect("terminal text output is an object")
                .insert(
                    CONTEXT_WINDOW_USAGE_FIELD.to_string(),
                    serde_json::to_value(usage).expect("context window usage is serializable"),
                );
        }
        Self::client(INTERFACE_ARTIFACT_OUTPUT_LABEL, value)
    }

    pub(crate) fn with_provider_token_usage(mut self, usage: ProviderTokenUsageEvidence) -> Self {
        self.value
            .as_object_mut()
            .expect("terminal text output is an object")
            .insert(
                PROVIDER_TOKEN_USAGE_FIELD.to_string(),
                serde_json::to_value(usage).expect("provider token usage is serializable"),
            );
        self
    }

    pub(crate) fn native_tool_call(
        tool: String,
        arguments: serde_json::Value,
        correlation_id: String,
    ) -> Self {
        let route = ProviderEmissionRoute::Tool { tool: tool.clone() };
        Self {
            label: TOOL_CALL_LABEL.to_string(),
            value: serde_json::json!({
                "tool": tool,
                "arguments": arguments,
                "correlation_id": correlation_id,
            }),
            route,
        }
    }

    pub(crate) fn interface_operation_call(
        operation: &InterfaceOperationAffordance,
        arguments: serde_json::Value,
        correlation_id: String,
    ) -> Self {
        Self {
            label: operation.request_label.clone(),
            value: serde_json::json!({
                "operation": operation.name,
                "arguments": arguments,
                "correlation_id": correlation_id,
            }),
            route: ProviderEmissionRoute::Operation {
                interface: operation.interface.clone(),
                result_label: operation.output_label.clone(),
            },
        }
    }

    pub(crate) fn dynamic_tool_started(
        namespace: String,
        tool: String,
        arguments: serde_json::Value,
        call_id: String,
        thread_id: String,
        turn_id: String,
    ) -> Self {
        Self::client(
            TURN_DYNAMIC_TOOL_CALL_LABEL,
            serde_json::json!({
                "item_id": call_id,
                "call_id": call_id,
                "thread_id": thread_id,
                "turn_id": turn_id,
                "namespace": namespace,
                "tool": tool,
                "arguments": arguments,
                "status": "started",
                "lifecycle": "started",
            }),
        )
    }

    pub(crate) fn dynamic_tool_completed(
        namespace: String,
        tool: String,
        call_id: String,
        thread_id: String,
        turn_id: String,
        ok: bool,
        body: Option<serde_json::Value>,
        error: Option<octopus_tool_handlers::ToolResultError>,
    ) -> Self {
        Self::client(
            TURN_DYNAMIC_TOOL_CALL_LABEL,
            serde_json::json!({
                "item_id": call_id,
                "call_id": call_id,
                "thread_id": thread_id,
                "turn_id": turn_id,
                "namespace": namespace,
                "tool": tool,
                "ok": ok,
                "success": ok,
                "body": body,
                "error": error,
                "status": if ok { "completed" } else { "failed" },
                "lifecycle": "completed",
            }),
        )
    }

    pub(crate) fn dynamic_tool_completed_after_interrupt(
        namespace: String,
        tool: String,
        call_id: String,
        thread_id: String,
        turn_id: String,
        ok: bool,
        body: Option<serde_json::Value>,
        error: Option<octopus_tool_handlers::ToolResultError>,
    ) -> Self {
        Self::client(
            TURN_DYNAMIC_TOOL_CALL_LABEL,
            serde_json::json!({
                "item_id": call_id,
                "call_id": call_id,
                "thread_id": thread_id,
                "turn_id": turn_id,
                "namespace": namespace,
                "tool": tool,
                "ok": ok,
                "success": ok,
                "body": body,
                "error": error,
                "status": if ok { "completed" } else { "failed" },
                "lifecycle": "completed",
                "turn_interrupted": true,
                "returned_to_model": false,
                "result_disposition": "durably_admitted_after_interrupt",
            }),
        )
    }

    pub(crate) fn interface_operation_completed(
        operation: String,
        call_id: String,
        thread_id: String,
        turn_id: String,
        ok: bool,
        result: serde_json::Value,
    ) -> Self {
        Self::client(
            TURN_DYNAMIC_TOOL_CALL_LABEL,
            serde_json::json!({
                "item_id": call_id,
                "call_id": call_id,
                "thread_id": thread_id,
                "turn_id": turn_id,
                "tool": operation,
                "ok": ok,
                "success": ok,
                "result": result,
                "status": if ok { "completed" } else { "failed" },
                "lifecycle": "completed",
            }),
        )
    }

    pub(crate) fn interface_operation_completed_after_interrupt(
        operation: String,
        call_id: String,
        thread_id: String,
        turn_id: String,
        ok: bool,
        result: serde_json::Value,
    ) -> Self {
        Self::client(
            TURN_DYNAMIC_TOOL_CALL_LABEL,
            serde_json::json!({
                "item_id": call_id,
                "call_id": call_id,
                "thread_id": thread_id,
                "turn_id": turn_id,
                "tool": operation,
                "operation": operation,
                "ok": ok,
                "success": ok,
                "result": result,
                "status": if ok { "completed" } else { "failed" },
                "lifecycle": "completed",
                "turn_interrupted": true,
                "returned_to_model": false,
                "result_disposition": "durably_admitted_after_interrupt",
            }),
        )
    }

    pub(crate) fn interrupted_result_closed(
        result_kind: &'static str,
        correlation_id: String,
        thread_id: String,
        turn_id: String,
        ok: bool,
        source_artifact: &ArtifactRef,
    ) -> Self {
        Self::client(
            TURN_INTERRUPTED_RESULT_LABEL,
            serde_json::json!({
                "result_kind": result_kind,
                "correlation_id": correlation_id,
                "thread_id": thread_id,
                "turn_id": turn_id,
                "ok": ok,
                "turn_interrupted": true,
                "returned_to_model": false,
                "result_disposition": "durably_admitted_after_interrupt",
                "source_artifact": source_artifact,
            }),
        )
    }

    pub(crate) fn with_native_terminal_events(mut self, events: Vec<serde_json::Value>) -> Self {
        if !events.is_empty() {
            self.value["native_terminal_events"] = serde_json::Value::Array(events);
        }
        self
    }

    pub(crate) fn with_interrupted_drain(mut self, drain: InterruptedTurnDrainEvidence) -> Result<Self, InterfaceDriverError> {
        self = self.with_native_terminal_events(drain.native_terminal_events);
        if let Some(source) = drain.source_continuity {
            self.value["native_terminal_source_continuity"] = serde_json::to_value(source)
                .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
            self.value["result_origin"] = serde_json::json!("interrupted_turn_completion");
            self.value["terminal_evidence_source"] = serde_json::json!("thread/read");
        }
        Ok(self)
    }

    pub(crate) fn approval_request(
        label: &str,
        request_id: String,
        kind: &'static str,
        title: String,
        detail: serde_json::Value,
        method: String,
    ) -> Self {
        Self::client(
            label,
            serde_json::json!({
                "request_id": request_id,
                "kind": kind,
                "title": title,
                "detail": detail,
                "method": method,
            }),
        )
    }

    pub(crate) fn elicitation_request(
        label: &str,
        request_id: String,
        message: String,
        requested_schema: serde_json::Value,
        method: String,
    ) -> Self {
        Self::client(
            label,
            serde_json::json!({
                "request_id": request_id,
                "message": message,
                "requested_schema": requested_schema,
                "method": method,
            }),
        )
    }

    pub(crate) fn user_input_request(
        label: &str,
        request_id: String,
        questions: serde_json::Value,
        auto_resolution_ms: Option<u64>,
        is_blocking: bool,
        method: String,
    ) -> Self {
        let title = questions
            .as_array()
            .and_then(|rows| rows.first())
            .and_then(|row| row.get("header"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("Input requested")
            .to_string();
        let message = questions
            .as_array()
            .and_then(|rows| rows.first())
            .and_then(|row| row.get("question"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();
        Self::client(
            label,
            serde_json::json!({
                "request_id": request_id,
                "title": title,
                "message": message,
                "questions": questions,
                "auto_resolution_ms": auto_resolution_ms,
                "is_blocking": is_blocking,
                "method": method,
            }),
        )
    }

    pub(crate) fn scoped_message_delta(
        seq: u64,
        text: String,
        turn_id: String,
        item_id: String,
        phase: Option<MessagePhase>,
    ) -> Self {
        Self::client(
            AGENT_MESSAGE_DELTA_LABEL,
            serde_json::json!({
                "seq": seq,
                "text": text,
                "turn_id": turn_id,
                "item_id": item_id,
                "phase": phase,
                "lifecycle": "streaming",
            }),
        )
    }

    pub(crate) fn reasoning(item_id: String, summary: Vec<String>, content: Vec<String>) -> Self {
        Self::client(
            TURN_REASONING_LABEL,
            serde_json::json!({
                "item_id": item_id,
                "summary": summary,
                "content": content,
            }),
        )
    }

    pub(crate) fn proposed_plan(item_id: String, text: String) -> Self {
        Self::client(
            TURN_PLAN_UPDATE_LABEL,
            serde_json::json!({
                "item_id": item_id,
                "text": text,
                "plan_kind": "proposed_plan",
                "octopus_authority": "informational",
                "model_stream_authority": "completed_item",
                "execution_authority": "octopus_checkpoints",
            }),
        )
    }

    pub(crate) fn agent_checklist(plan: &TurnPlanUpdatedNotification, text: String) -> Self {
        Self::client(
            TURN_PLAN_UPDATE_LABEL,
            serde_json::json!({
                "item_id": plan.turn_id,
                "thread_id": plan.thread_id,
                "turn_id": plan.turn_id,
                "text": text,
                "explanation": plan.explanation,
                "steps": plan.plan,
                "plan_kind": "agent_checklist",
                "octopus_authority": "informational",
                "model_stream_authority": "tool_snapshot",
                "execution_authority": "octopus_checkpoints",
            }),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn exec_command(
        item_id: String,
        command: String,
        cwd: String,
        process_id: Option<String>,
        command_actions: Vec<serde_json::Value>,
        source: &'static str,
        status: &'static str,
        exit_code: Option<i64>,
        duration_ms: Option<i64>,
        aggregated_output: Option<String>,
    ) -> Self {
        Self::client(
            TURN_EXEC_COMMAND_LABEL,
            serde_json::json!({
                "item_id": item_id,
                "command": command,
                "cwd": cwd,
                "process_id": process_id,
                "command_actions": command_actions,
                "source": source,
                "status": status,
                "exit_code": exit_code,
                "duration_ms": duration_ms,
                "aggregated_output": aggregated_output,
            }),
        )
    }
}

pub(crate) fn provider_route_from_lifecycle(route: LifecycleRoute) -> ProviderEmissionRoute {
    match route {
        LifecycleRoute::ClientProjection => ProviderEmissionRoute::ClientProjection,
        LifecycleRoute::ClientProjectionWithVerifiedInputs { inputs } => {
            ProviderEmissionRoute::ClientProjectionWithVerifiedInputs {
                inputs: inputs
                    .into_iter()
                    .map(|input| ProviderVerifiedInput {
                        also_deliver_to_tool: None,
                        also_deliver_to_interface: None,
                        artifact_label: input.artifact_label,
                        required_verified_artifact: input.required_verified_artifact,
                    })
                    .collect(),
            }
        }
        LifecycleRoute::Tool { tool } => ProviderEmissionRoute::Tool { tool },
        LifecycleRoute::Interface { interface } => ProviderEmissionRoute::Interface { interface },
        LifecycleRoute::Operation { interface, result_label } => {
            ProviderEmissionRoute::Operation { interface, result_label }
        }
    }
}

pub(crate) fn lifecycle_payload_label(label: &str) -> bool {
    matches!(
        label,
        LIFECYCLE_CONNECTION_LABEL
            | LIFECYCLE_EVENT_LABEL
            | LIFECYCLE_DECISION_LABEL
            | LIFECYCLE_HANDLER_PROMPT_LABEL
            | LIFECYCLE_HANDLER_TASK_LABEL
            | LIFECYCLE_HANDLER_OUTPUT_LABEL
    )
}

pub(crate) fn lifecycle_context_payloads(
    bound: &[ArtifactBindingPayload],
    snapshot: Option<&ProviderInvocationSessionSnapshot>,
) -> Result<Vec<ArtifactBindingPayload>, InterfaceDriverError> {
    let mut payloads = bound.to_vec();
    let Some(snapshot) = snapshot else {
        return Ok(payloads);
    };
    for artifact in snapshot.artifacts.iter().filter(|artifact| {
        matches!(
            artifact.label.as_str(),
            LIFECYCLE_CONNECTION_LABEL | LIFECYCLE_EVENT_LABEL
        )
    }) {
        let bytes = serde_json::to_vec(&artifact.payload).map_err(|error| {
            InterfaceDriverError::Protocol(format!(
                "lifecycle session artifact '{}' did not encode: {error}",
                artifact.source_artifact.id.as_str()
            ))
        })?;
        if let Some(existing) = payloads
            .iter()
            .find(|bound| bound.artifact == artifact.source_artifact)
        {
            let existing_bytes = existing.read_bytes()
                .map_err(|error| InterfaceDriverError::FailedClosed(error.to_string()))?;
            if existing_bytes.as_ref() != bytes.as_slice() {
                return Err(InterfaceDriverError::FailedClosed(format!(
                    "lifecycle artifact '{}' has conflicting bound and session bytes",
                    artifact.source_artifact.id.as_str()
                )));
            }
            continue;
        }
        payloads.push(ArtifactBindingPayload::new(
            artifact.source_artifact.clone(),
            bytes,
        ));
    }
    Ok(payloads)
}
