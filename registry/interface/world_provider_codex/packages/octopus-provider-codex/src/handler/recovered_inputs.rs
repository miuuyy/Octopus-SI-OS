use super::*;

static EMITTED_PAYLOAD_LIMIT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub(crate) fn bind_emitted_payload_limit(max_payload_bytes: u64) -> Result<(), String> {
    if max_payload_bytes == 0 {
        return Err("host io limits declare no payload capacity for provider outputs".into());
    }
    EMITTED_PAYLOAD_LIMIT.store(max_payload_bytes, std::sync::atomic::Ordering::SeqCst);
    Ok(())
}

pub(crate) fn materialize_codex_emitted_artifact(
    output_root: &Path,
    label: &str,
    payload_bytes: &[u8],
) -> Result<InterfaceArtifactFile, InterfaceDriverError> {
    let limit = EMITTED_PAYLOAD_LIMIT.load(std::sync::atomic::Ordering::SeqCst);
    if limit > 0 && payload_bytes.len() as u64 > limit {
        return Err(InterfaceDriverError::FailedClosed(format!(
            "provider output '{label}' is {} bytes, above the host payload limit {limit}; it was not emitted",
            payload_bytes.len()
        )));
    }
    InterfaceArtifactFile::write(output_root, label, payload_bytes)
        .map_err(|e| InterfaceDriverError::Protocol(e.to_string()))
}

impl OperatorResponseArtifact {
    pub(crate) fn request_id(&self) -> Result<String, InterfaceDriverError> {
        let value = match self {
            Self::Approval { request_id, .. }
            | Self::Elicitation { request_id, .. }
            | Self::UserInput { request_id, .. } => request_id,
        };
        if value.trim().is_empty() || value.trim() != value {
            return Err(InterfaceDriverError::Protocol(
                "operator response artifact has an empty or untrimmed request_id".to_string(),
            ));
        }
        Ok(value.clone())
    }
}

pub(crate) fn validate_recovered_tool_result(
    result: &ToolResultArtifact,
    pending: &CodexPendingContinuity,
) -> Result<(), InterfaceDriverError> {
    result.validate().map_err(InterfaceDriverError::Protocol)?;
    let CodexPendingContinuity::DynamicTool { call_id, tool, .. } = pending else {
        return Err(InterfaceDriverError::FailedClosed(
            "tool result conflicts with the kind of the durable pending request".to_string(),
        ));
    };
    if result.correlation_id.as_deref() != Some(call_id) || result.tool != *tool {
        return Err(InterfaceDriverError::FailedClosed(format!(
            "tool result conflicts with the durable pending correlation: expected call_id='{call_id}' tool='{tool}', got correlation_id={:?} tool='{}'",
            result.correlation_id, result.tool
        )));
    }
    Ok(())
}

pub(crate) fn validate_recovered_interface_operation_input(
    result: &InterfaceOperationResultArtifact,
    pending: &CodexPendingContinuity,
) -> Result<(), InterfaceDriverError> {
    let CodexPendingContinuity::InterfaceOperation {
        call_id,
        operation,
        output_label,
        ..
    } = pending
    else {
        return Err(InterfaceDriverError::FailedClosed(
            "interface operation result conflicts with the kind of the durable pending request"
                .to_string(),
        ));
    };
    validate_recovered_interface_operation_result(
        &result.label,
        &result.value,
        call_id,
        operation,
        output_label,
    )
}

pub(crate) fn validate_recovered_interface_operation_result(
    source_label: &str,
    value: &serde_json::Value,
    call_id: &str,
    operation: &str,
    output_label: &str,
) -> Result<(), InterfaceDriverError> {
    let object = value.as_object().ok_or_else(|| {
        InterfaceDriverError::Protocol(
            "durable interface-operation result is not a JSON object".to_string(),
        )
    })?;
    let payload_label = object.get("label").and_then(serde_json::Value::as_str);
    let payload_operation = object.get("operation").and_then(serde_json::Value::as_str);
    let correlation_id = object
        .get("correlation_id")
        .and_then(serde_json::Value::as_str);
    let ok = object.get("ok").and_then(serde_json::Value::as_bool);
    if source_label != output_label
        || payload_label != Some(output_label)
        || payload_operation != Some(operation)
        || correlation_id != Some(call_id)
        || ok.is_none()
    {
        return Err(InterfaceDriverError::FailedClosed(
            "interface operation result conflicts with the durable pending coordinates".to_string(),
        ));
    }
    match (ok, object.get("result"), object.get("error")) {
        (Some(true), Some(_), None) | (Some(false), None, Some(_)) => Ok(()),
        _ => Err(InterfaceDriverError::Protocol(
            "durable interface-operation result has inconsistent ok/result/error fields"
                .to_string(),
        )),
    }
}

pub(crate) fn validate_recovered_operator_response(
    response: &OperatorResponseArtifact,
    pending: &CodexPendingContinuity,
) -> Result<(), InterfaceDriverError> {
    let CodexPendingContinuity::Operator {
        request_key,
        method,
        ..
    } = pending
    else {
        return Err(InterfaceDriverError::FailedClosed(
            "operator response conflicts with the kind of the durable pending request".to_string(),
        ));
    };
    let request_id = response.request_id()?;
    if format!("operator:{request_id}") != *request_key {
        return Err(InterfaceDriverError::FailedClosed(
            "operator response conflicts with the durable pending correlation".to_string(),
        ));
    }
    operator_response_value(method, response).map(|_| ())
}

pub(crate) fn pending_turn_id(pending: &CodexPendingContinuity) -> &str {
    match pending {
        CodexPendingContinuity::DynamicTool { turn_id, .. }
        | CodexPendingContinuity::InterfaceOperation { turn_id, .. }
        | CodexPendingContinuity::Operator { turn_id, .. } => turn_id,
    }
}

pub(crate) fn exact_pending_question(
    session: &[SessionContextArtifact],
    state: &CodexContinuityState,
    response: &OperatorResponseArtifact,
) -> Result<ArtifactRef, InterfaceDriverError> {
    let Some(pending @ CodexPendingContinuity::Operator { method, .. }) = state.pending.as_ref() else {
        return Err(InterfaceDriverError::FailedClosed("question response has no pending operator request".into()));
    };
    if method != "item/tool/requestUserInput" || !matches!(response, OperatorResponseArtifact::UserInput { .. }) {
        return Err(InterfaceDriverError::FailedClosed("interrupted question recovery cannot resolve another operator operation".into()));
    }
    validate_recovered_operator_response(response, pending)?;
    let request_id = response.request_id()?;
    let mut exact = None;
    for entry in session.iter().filter(|entry| entry.label == OPERATOR_USER_INPUT_REQUEST_LABEL
        && entry.payload.get("request_id").and_then(serde_json::Value::as_str) == Some(request_id.as_str())) {
        let Some(raw) = entry.payload.get(CODEX_CONTINUITY_FIELD) else { continue; };
        let request = parse_codex_continuity(raw, &state.provider_interface, &state.run)?;
        if request.thread_id != state.thread_id || request.pending.as_ref() != Some(pending) { continue; }
        if entry.source_movement_seq.is_none() || exact.replace(entry.source_artifact.clone()).is_some() {
            return Err(InterfaceDriverError::FailedClosed("interrupted question has ambiguous or unadmitted request evidence".into()));
        }
    }
    exact.ok_or_else(|| InterfaceDriverError::FailedClosed("interrupted question has no exact admitted request artifact".into()))
}

pub(crate) fn interrupted_transport_output(
    last_artifact_label: &str,
    pending: Option<&CodexPendingContinuity>,
) -> CodexTurnOutput {
    CodexTurnOutput::client(
        TURN_ERROR_LABEL,
        serde_json::json!({
            "message": "Codex provider restarted after a non-terminal artifact. The external model call was not replayed; any exact pending result remains an ordinary future artifact.",
            "will_retry": false,
            "additional_details": {
                "code": "transport_turn_interrupted",
                "last_artifact_label": last_artifact_label,
                "pending_turn_id": pending.map(pending_turn_id),
            },
        }),
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PendingGeneration {
    Current,
    Behind,
}

pub(crate) fn pending_generation(
    stored: &CodexContinuityState,
    admitted_generation: u64,
    live_generation: Option<u64>,
) -> Result<PendingGeneration, InterfaceDriverError> {
    if stored.context_generation > admitted_generation {
        return Err(InterfaceDriverError::FailedClosed(format!(
            "Codex pending continuity generation {} is ahead of its admitted context generation {admitted_generation}",
            stored.context_generation
        )));
    }
    if live_generation.is_some_and(|live| live > stored.context_generation) {
        return Err(InterfaceDriverError::FailedClosed(format!(
            "Codex pending continuity generation {} regressed behind the live transport context generation",
            stored.context_generation
        )));
    }
    Ok(if stored.context_generation < admitted_generation {
        PendingGeneration::Behind
    } else {
        PendingGeneration::Current
    })
}

pub(crate) fn validate_live_pending_continuity(
    recovered: Option<&CodexContinuityState>,
    pending: &PendingServerRequest,
    context_generation: u64,
    live: &TransportState,
) -> Result<PendingGeneration, InterfaceDriverError> {
    let refused = |message: &str| Err(InterfaceDriverError::FailedClosed(message.to_string()));
    if live.thread_id.as_deref() != Some(pending.thread_id.as_str()) {
        return refused("Codex pending request names a thread other than the live transport thread");
    }
    let (Some(live_shape), Some(live_static)) = (
        live.thread_shape_fingerprint.as_deref(),
        live.static_context_fingerprint.as_deref(),
    ) else {
        return refused("Codex live transport has no thread fingerprints for its pending request");
    };
    if live.context_generation > context_generation {
        return refused("Codex live transport is ahead of the admitted context generation");
    }
    let Some(recovered) = recovered else {
        return refused("Codex pending result has no immutable continuity artifact");
    };
    if recovered.thread_id != pending.thread_id {
        return refused("Codex pending continuity names a thread other than its live request");
    }
    if live_shape != recovered.thread_shape_fingerprint
        || live_static != recovered.static_context_fingerprint
    {
        return refused("Codex pending continuity fingerprints differ from the live transport thread");
    }
    let generation = pending_generation(recovered, context_generation, Some(live.context_generation))?;
    if !pending_coordinates_match(recovered.outstanding_call(&pending.key), pending) {
        return refused("Codex durable pending coordinates do not match the live app-server request");
    }
    Ok(generation)
}

/// A tool or interface result closes the call it answers even when checked
/// authoring advanced the provider context while that call was in flight.
/// The result continues from the admitted generation; the thread, its
/// fingerprints and the pending call remain exactly those the call left with.
pub(crate) fn adopt_admitted_result_generation(
    stored: &CodexContinuityState,
    key: &str,
    admitted_generation: u64,
    pending: Option<&PendingServerRequest>,
    live: Option<&TransportState>,
) -> Result<Option<CodexContinuityState>, InterfaceDriverError> {
    if stored.context_generation >= admitted_generation
        || stored.interrupted_pending.is_some()
        || stored.native_terminal.is_some()
        || stored.outstanding_call(key).is_none()
    {
        return Ok(None);
    }
    let generation = match (pending, live) {
        (Some(pending), _)
            if pending
                .turn_interrupted
                .load(std::sync::atomic::Ordering::Acquire) =>
        {
            return Ok(None);
        }
        (Some(pending), Some(live)) => {
            validate_live_pending_continuity(Some(stored), pending, admitted_generation, live)?
        }
        (Some(_), None) => {
            return Err(InterfaceDriverError::FailedClosed(
                "Codex pending request has no live transport coordinates".to_string(),
            ));
        }
        (None, live) => pending_generation(
            stored,
            admitted_generation,
            live.filter(|live| live.thread_id.is_some())
                .map(|live| live.context_generation),
        )?,
    };
    Ok((generation == PendingGeneration::Behind).then(|| {
        let mut adopted = stored.clone();
        adopted.context_generation = admitted_generation;
        adopted
    }))
}

pub(crate) fn require_admitted_generation(
    state: &CodexContinuityState,
    admitted_generation: u64,
    family: &str,
) -> Result<(), InterfaceDriverError> {
    match pending_generation(state, admitted_generation, None)
        .map_err(|error| InterfaceDriverError::FailedClosed(format!("{family}: {error}")))?
    {
        PendingGeneration::Current => Ok(()),
        PendingGeneration::Behind => Err(InterfaceDriverError::FailedClosed(format!(
            "{family} continuity generation {} is behind its admitted context generation {admitted_generation} and was not adopted",
            state.context_generation
        ))),
    }
}

pub(crate) fn pending_coordinates_match(durable: Option<&CodexPendingContinuity>, pending: &PendingServerRequest) -> bool {
    match (durable, &pending.kind) {
        (
            Some(CodexPendingContinuity::DynamicTool {
                native_operation: _,
                turn_id,
                call_id,
                tool,
            }),
            PendingServerRequestKind::DynamicTool {
                call_id: pending_call_id,
                tool: pending_tool,
            },
        ) => {
            turn_id == &pending.turn_id
                && call_id == pending_call_id
                && tool == pending_tool
                && pending.key == format!("tool:{call_id}")
        }
        (
            Some(CodexPendingContinuity::InterfaceOperation {
                turn_id,
                call_id,
                operation,
                interface,
                output_label,
            }),
            PendingServerRequestKind::InterfaceOperation {
                call_id: pending_call_id,
                operation: pending_operation,
                interface: pending_interface,
                output_label: pending_output_label,
            },
        ) => {
            turn_id == &pending.turn_id
                && call_id == pending_call_id
                && operation == pending_operation
                && interface == pending_interface
                && output_label == pending_output_label
                && pending.key == format!("tool:{call_id}")
        }
        (
            Some(CodexPendingContinuity::Operator {
                turn_id,
                request_key,
                method,
            }),
            PendingServerRequestKind::Operator {
                method: pending_method,
            },
        ) => turn_id == &pending.turn_id && request_key == &pending.key && method == pending_method,
        _ => false,
    }
}

impl ProviderInput {
    pub(crate) fn into_turn_input(self) -> Result<Vec<UserInput>, InterfaceDriverError> {
        match self {
            Self::Turn(input) => Ok(input),
            Self::Resume { .. } => Ok(Vec::new()),
            Self::ToolResult(_)
            | Self::InterfaceOperationResult(_)
            | Self::InterfaceContext(_)
            | Self::OperatorResponse(_) => {
                Err(InterfaceDriverError::FailedClosed(
                    "provider result has no live structured pending request and cannot become a new semantic turn"
                        .to_string(),
                ))
            }
        }
    }
}
