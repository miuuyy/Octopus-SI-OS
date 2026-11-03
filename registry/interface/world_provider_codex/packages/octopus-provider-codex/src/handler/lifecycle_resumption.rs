use super::*;

pub(crate) struct LifecycleResumption {
    pub(crate) request_bytes: Vec<u8>,
    pub(crate) manifest_label: String,
    pub(crate) source_artifact: ArtifactRef,
    pub(crate) start_turn: Option<Vec<UserInput>>,
    pub(crate) start_tool_output: Option<TurnToolOutput>,
    pub(crate) additional_context:
        Option<BTreeMap<String, octopus_appserver_proto::AdditionalContextEntry>>,
    pub(crate) resumed_provider_input: bool,
    pub(crate) resume_boundary: bool,
    pub(crate) prompt_event: Option<LifecycleEventArtifact>,
}

impl LifecycleResumption {
    pub(crate) fn of_input(
        request_bytes: Vec<u8>,
        manifest_label: String,
        source_artifact: ArtifactRef,
    ) -> Self {
        Self {
            request_bytes,
            manifest_label,
            source_artifact,
            start_turn: None,
            start_tool_output: None,
            additional_context: None,
            resumed_provider_input: false,
            resume_boundary: false,
            prompt_event: None,
        }
    }
}

pub(crate) struct LifecycleInboundFrame<'a, 's> {
    pub(crate) binding: &'a LifecycleBinding,
    pub(crate) sink: &'a TurnSink<'s>,
    pub(crate) runtime: &'a RuntimeInvocationMetadata,
    pub(crate) input: &'a InterfaceInvocationEnvelope,
    pub(crate) granted_tools: &'a BTreeSet<String>,
    pub(crate) granted_interfaces: &'a BTreeSet<InterfaceId>,
}

pub(crate) fn resume_lifecycle_inbound(
    frame: LifecycleInboundFrame<'_, '_>,
    inbound: LifecycleInbound,
    mut resumption: LifecycleResumption,
) -> Result<Option<LifecycleResumption>, InterfaceDriverError> {
    let LifecycleInboundFrame {
        binding,
        sink,
        runtime,
        input,
        granted_tools,
        granted_interfaces,
    } = frame;
    match inbound {
        LifecycleInbound::Event(event) => {
            restore_lifecycle_provider_continuity(sink, runtime, &event)?;
            let dispatch = binding
                .dispatch_handler(
                    runtime.run.as_str(),
                    &event,
                    granted_tools,
                    granted_interfaces,
                )
                .map_err(InterfaceDriverError::FailedClosed)?;
            if let Some(dispatch) = dispatch {
                sink.send_lifecycle(LifecyclePlannedOutput {
                    label: dispatch.label,
                    value: dispatch.value,
                    route: dispatch.route,
                })?;
            }
            return Ok(None);
        }
        LifecycleInbound::Decision(decision) => {
            let event = binding
                .validate_bound_event(&decision.event_artifact)
                .map(|_| decision.event_artifact.clone())
                .map_err(InterfaceDriverError::FailedClosed)?;
            restore_lifecycle_provider_continuity(sink, runtime, &event)?;
            let continuation = binding
                .validate_decision(runtime.run.as_str(), &decision, input.metadata()?)
                .map_err(InterfaceDriverError::FailedClosed)?;
            match continuation {
                LifecycleContinuation::HeldEmission { .. } => {
                    sink.send_held_lifecycle(&continuation)?;
                    return Ok(None);
                }
                LifecycleContinuation::HeldEmissionAndRecoverBoundary {
                    ref source_artifact,
                    ..
                } => {
                    sink.send_held_lifecycle(&continuation)?;
                    resumption.resume_boundary = true;
                    resumption.source_artifact = source_artifact.clone();
                }
                LifecycleContinuation::StartTurn {
                    turn_input,
                    additional_context,
                    source_artifact,
                } => {
                    let (turn_input, tool_output) = resumed_turn_start(turn_input)?;
                    resumption.start_turn = Some(turn_input);
                    resumption.start_tool_output = tool_output;
                    resumption.additional_context = Some(additional_context);
                    resumption.source_artifact = source_artifact;
                }
                LifecycleContinuation::ProviderInput {
                    manifest_label,
                    payload,
                    source_artifact,
                } => {
                    resumption.resumed_provider_input = true;
                    resumption.request_bytes = serde_json::to_vec(&payload).map_err(|error| {
                        InterfaceDriverError::Protocol(format!(
                            "lifecycle provider-input continuation did not encode: {error}"
                        ))
                    })?;
                    resumption.manifest_label = manifest_label;
                    resumption.source_artifact = source_artifact;
                }
                LifecycleContinuation::RecoverBoundary { source_artifact } => {
                    resumption.resume_boundary = true;
                    resumption.source_artifact = source_artifact;
                }
            }
        }
        LifecycleInbound::HandlerPrompt(value) => {
            start_handler_turn(
                binding,
                sink,
                runtime,
                &value,
                "prompt",
                LIFECYCLE_HANDLER_PROMPT_LABEL,
                &mut resumption,
            )?;
        }
        LifecycleInbound::HandlerTask(value) => {
            start_handler_turn(
                binding,
                sink,
                runtime,
                &value,
                "task",
                LIFECYCLE_HANDLER_TASK_LABEL,
                &mut resumption,
            )?;
        }
        LifecycleInbound::HandlerOutput(value) => {
            binding
                .validate_handler_output(&value)
                .map_err(InterfaceDriverError::FailedClosed)?;
            return Ok(None);
        }
    }
    Ok(Some(resumption))
}

fn start_handler_turn(
    binding: &LifecycleBinding,
    sink: &TurnSink<'_>,
    runtime: &RuntimeInvocationMetadata,
    value: &serde_json::Value,
    field: &str,
    label: &str,
    resumption: &mut LifecycleResumption,
) -> Result<(), InterfaceDriverError> {
    let event: LifecycleEventArtifact =
        serde_json::from_value(value.get("event").cloned().ok_or_else(|| {
            InterfaceDriverError::Protocol(format!("lifecycle handler {field} has no event"))
        })?)
        .map_err(|error| {
            InterfaceDriverError::Protocol(format!(
                "lifecycle handler {field} event is malformed: {error}"
            ))
        })?;
    binding
        .validate_bound_event(&event)
        .map_err(InterfaceDriverError::FailedClosed)?;
    binding
        .validate_handler_invocation(&event, value, label)
        .map_err(InterfaceDriverError::FailedClosed)?;
    restore_lifecycle_provider_continuity(sink, runtime, &event)?;
    let text = value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .filter(|text| !text.trim().is_empty() && text.trim() == *text)
        .ok_or_else(|| {
            InterfaceDriverError::Protocol(format!(
                "lifecycle handler {field} has no non-empty {field}"
            ))
        })?;
    resumption.start_turn = Some(vec![UserInput::Text {
        text: lifecycle_handler_model_prompt(text, &event)?,
        text_elements: Vec::new(),
    }]);
    resumption.source_artifact = event.subject_artifact.clone();
    resumption.prompt_event = Some(event);
    Ok(())
}
