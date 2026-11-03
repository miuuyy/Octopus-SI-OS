use super::*;

pub(crate) fn decode_tool_result_artifact(
    value: &serde_json::Value,
) -> Result<Option<ToolResultArtifact>, InterfaceDriverError> {
    let Some(object) = value.as_object() else {
        return Ok(None);
    };
    if !(object.contains_key("label") && object.contains_key("tool") && object.contains_key("ok")) {
        return Ok(None);
    }
    let result = serde_json::from_value::<ToolResultArtifact>(value.clone()).map_err(|error| {
        InterfaceDriverError::Protocol(format!(
            "tool result artifact is not a strict result envelope: {error}"
        ))
    })?;
    result.validate().map_err(InterfaceDriverError::Protocol)?;
    Ok(Some(result))
}

pub(crate) fn decode_interface_operation_result_artifact(
    value: &serde_json::Value,
    manifest_label: &str,
    source_interface: &InterfaceId,
    operations: &BTreeMap<String, InterfaceOperationAffordance>,
) -> Result<Option<InterfaceOperationResultArtifact>, InterfaceDriverError> {
    let object = value.as_object().ok_or_else(|| {
        InterfaceDriverError::Protocol(format!(
            "artifact from operation interface '{}' is not a JSON object",
            source_interface.as_str()
        ))
    })?;
    if !(object.contains_key("operation") || object.contains_key("ok")) {
        return Ok(None);
    }
    let operation = object
        .get("operation")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty() && value.trim() == *value)
        .ok_or_else(|| {
            InterfaceDriverError::Protocol(format!(
                "result from operation interface '{}' has no operation name",
                source_interface.as_str()
            ))
        })?;
    let affordance = operations.get(operation).ok_or_else(|| {
        InterfaceDriverError::FailedClosed(format!(
            "result from interface '{}' names operation '{}' outside the bound catalog",
            source_interface.as_str(),
            operation
        ))
    })?;
    if affordance.interface != *source_interface {
        return Err(InterfaceDriverError::FailedClosed(format!(
            "operation '{}' result arrived from interface '{}', bound owner is '{}'",
            operation,
            source_interface.as_str(),
            affordance.interface.as_str()
        )));
    }
    let payload_label = object
        .get("label")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty() && value.trim() == *value)
        .ok_or_else(|| {
            InterfaceDriverError::Protocol(format!(
                "operation '{}' result has no self-identifying label",
                operation
            ))
        })?;
    if payload_label != manifest_label || payload_label != affordance.output_label {
        return Err(InterfaceDriverError::FailedClosed(format!(
            "operation '{}' result label manifest='{}' payload='{}' expected='{}'",
            operation, manifest_label, payload_label, affordance.output_label
        )));
    }
    let correlation_id = object
        .get("correlation_id")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty() && value.trim() == *value)
        .map(str::to_string)
        .ok_or_else(|| {
            InterfaceDriverError::Protocol(format!(
                "operation '{}' result has no correlation_id",
                operation
            ))
        })?;
    let ok = object
        .get("ok")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| {
            InterfaceDriverError::Protocol(format!(
                "operation '{}' result has no boolean ok field",
                operation
            ))
        })?;
    match (ok, object.get("result"), object.get("error")) {
        (true, Some(_), None) | (false, None, Some(_)) => {}
        _ => {
            return Err(InterfaceDriverError::Protocol(format!(
                "operation '{}' result has inconsistent ok/result/error fields",
                operation
            )));
        }
    }
    Ok(Some(InterfaceOperationResultArtifact {
        label: payload_label.to_string(),
        operation: operation.to_string(),
        correlation_id,
        ok,
        value: value.clone(),
    }))
}

pub(crate) fn decode_operator_response_artifact(
    value: &serde_json::Value,
) -> Result<Option<OperatorResponseArtifact>, InterfaceDriverError> {
    let Some(object) = value.as_object() else {
        return Ok(None);
    };
    let mut responses = Vec::new();
    if let Some(value) = object.get("operator_approval_response") {
        let response = value.as_object().ok_or_else(|| {
            InterfaceDriverError::Protocol(
                "operator_approval_response must be a JSON object".to_string(),
            )
        })?;
        responses.push(OperatorResponseArtifact::Approval {
            request_id: response_string(response, "request_id")?,
            decision: response.get("decision").cloned().ok_or_else(|| {
                InterfaceDriverError::Protocol(
                    "operator_approval_response has no decision".to_string(),
                )
            })?,
        });
    }
    if let Some(value) = object.get("operator_elicitation_response") {
        let response = value.as_object().ok_or_else(|| {
            InterfaceDriverError::Protocol(
                "operator_elicitation_response must be a JSON object".to_string(),
            )
        })?;
        responses.push(OperatorResponseArtifact::Elicitation {
            request_id: response_string(response, "request_id")?,
            answer: response.get("answer").cloned().ok_or_else(|| {
                InterfaceDriverError::Protocol(
                    "operator_elicitation_response has no answer".to_string(),
                )
            })?,
        });
    }
    if let Some(value) = object.get("operator_user_input_response") {
        let response = value.as_object().ok_or_else(|| {
            InterfaceDriverError::Protocol(
                "operator_user_input_response must be a JSON object".to_string(),
            )
        })?;
        responses.push(OperatorResponseArtifact::UserInput {
            request_id: response_string(response, "request_id")?,
            answers: response.get("answers").cloned().ok_or_else(|| {
                InterfaceDriverError::Protocol(
                    "operator_user_input_response has no answers".to_string(),
                )
            })?,
        });
    }
    match responses.len() {
        0 => Ok(None),
        1 => Ok(responses.pop()),
        count => Err(InterfaceDriverError::Protocol(format!(
            "provider input carries {count} operator response bodies; exactly one is required"
        ))),
    }
}

pub(crate) fn response_string(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<String, InterfaceDriverError> {
    object
        .get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty() && value.trim() == *value)
        .map(str::to_string)
        .ok_or_else(|| {
            InterfaceDriverError::Protocol(format!(
                "operator response has no non-empty trimmed '{key}'"
            ))
        })
}

pub(crate) fn dynamic_tool_response_value(
    result: &ToolResultArtifact,
    source_artifact: &ArtifactRef,
    projection: ModelResultProjection,
) -> Result<serde_json::Value, InterfaceDriverError> {
    serde_json::to_value(DynamicToolCallResponse {
        content_items: dynamic_tool_response_content(result, source_artifact, projection)?,
        success: result.ok,
    })
    .map_err(|error| {
        InterfaceDriverError::Protocol(format!(
            "dynamic tool response could not be encoded: {error}"
        ))
    })
}

pub(crate) fn native_pending_operation(pending: &CodexPendingContinuity) -> Result<Option<NativePending>, InterfaceDriverError> {
    let CodexPendingContinuity::DynamicTool { native_operation: Some(value), tool, call_id, .. } = pending else { return Ok(None); };
    let operation: NativePending = serde_json::from_value(value.clone()).map_err(|error| InterfaceDriverError::FailedClosed(format!("native operation continuity is malformed: {error}")))?;
    if operation.effect.tool != *tool || operation.effect.correlation_id != *call_id {
        return Err(InterfaceDriverError::FailedClosed("native operation lost its exact pending file/transfer effect".into()));
    }
    Ok(Some(operation))
}

pub(crate) fn emit_native_effect(
    sink: &TurnSink<'_>, turn_id: &str, operation: NativePending,
    bindings: Vec<octopus_run_authoring::ProviderOutgoingBinding>,
    amendment: Option<octopus_run_authoring::ProviderCapabilityAmendment>,
) -> Result<(), InterfaceDriverError> {
    let value = serde_json::to_value(&operation).map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
    let effect = operation.effect;
    sink.replace_pending(CodexPendingContinuity::DynamicTool {
        turn_id: turn_id.into(), call_id: effect.correlation_id.clone(), tool: effect.tool.clone(), native_operation: Some(value),
    })?;
    let output = CodexTurnOutput::native_tool_call(effect.tool, effect.arguments, effect.correlation_id);
    if let Some(amendment) = amendment {
        if !bindings.is_empty() {
            return Err(InterfaceDriverError::FailedClosed("one native effect cannot carry two different contract amendments".into()));
        }
        sink.send_with_capability_amendment(output, amendment)
    } else if bindings.is_empty() { sink.send(output) } else { sink.send_with_outgoing_bindings(output, bindings) }
}

pub(crate) fn native_result_success(value: &serde_json::Value) -> bool {
    value.get("ok").and_then(serde_json::Value::as_bool).unwrap_or(true)
}

pub(crate) fn native_operation_response(value: &serde_json::Value) -> Result<serde_json::Value, InterfaceDriverError> {
    serde_json::to_value(DynamicToolCallResponse {
        content_items: native_result_content(value, Vec::new())?, success: native_result_success(value),
    }).map_err(|error| InterfaceDriverError::Protocol(error.to_string()))
}

pub(crate) fn dynamic_tool_response_content(
    result: &ToolResultArtifact,
    source_artifact: &ArtifactRef,
    projection: ModelResultProjection,
) -> Result<Vec<DynamicToolCallOutputContentItem>, InterfaceDriverError> {
    let mut value = serde_json::json!({
        "tool": result.tool,
        "correlation_id": result.correlation_id,
        "ok": result.ok,
        "body": result.body,
        "error": result.error,
    });
    let media = result.body.as_ref().map(crate::result_media::project).transpose()?.flatten();
    let images = match media {
        Some(media) => {
            value["body"] = media.value;
            media.images
        }
        None => Vec::new(),
    };
    native_artifact_result_content(&value, images, serde_json::json!({
        "tool": result.tool, "correlation_id": result.correlation_id, "ok": result.ok,
    }), source_artifact, projection)
}

pub(crate) fn native_artifact_result_content(
    value: &serde_json::Value,
    images: Vec<DynamicToolCallOutputContentItem>,
    identity: serde_json::Value,
    source_artifact: &ArtifactRef,
    projection: ModelResultProjection,
) -> Result<Vec<DynamicToolCallOutputContentItem>, InterfaceDriverError> {
    let mut text = serde_json::to_string(value)
        .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
    if crate::result_media::text_exceeds_limit(&text)? {
        let mut budget = projection.max_tokens;
        loop {
            text = model_result_text(value, identity.clone(), source_artifact, ModelResultProjection { max_tokens: budget })?;
            if !crate::result_media::text_exceeds_limit(&text)? { break; }
            if budget <= 512 {
                return Err(InterfaceDriverError::FailedClosed("native result projection cannot retain its evidence header within the transport text limit".into()));
            }
            budget = (budget / 2).max(512);
        }
    }
    let mut content = vec![DynamicToolCallOutputContentItem::InputText { text }];
    content.extend(images);
    crate::result_media::validate_content(&content)?;
    Ok(content)
}

pub(crate) fn native_result_content(
    value: &serde_json::Value,
    images: Vec<DynamicToolCallOutputContentItem>,
) -> Result<Vec<DynamicToolCallOutputContentItem>, InterfaceDriverError> {
    let text = serde_json::to_string(value).map_err(|error| {
        InterfaceDriverError::Protocol(format!(
            "interface result could not be encoded for native continuation: {error}"
        ))
    })?;
    let mut content = vec![DynamicToolCallOutputContentItem::InputText {
        text,
    }];
    content.extend(images);
    crate::result_media::validate_content(&content)?;
    Ok(content)
}

pub(crate) fn recovered_result_output(
    content: Vec<DynamicToolCallOutputContentItem>,
) -> Result<serde_json::Value, InterfaceDriverError> {
    if let [DynamicToolCallOutputContentItem::InputText { text }] = content.as_slice() {
        return Ok(serde_json::Value::String(text.clone()));
    }
    let items = content.into_iter().map(|item| {
        let item: octopus_protocol::dynamic_tools::DynamicToolCallOutputContentItem = item.into();
        octopus_protocol::models::FunctionCallOutputContentItem::from(item)
    }).collect::<Vec<_>>();
    serde_json::to_value(items).map_err(|error| {
        InterfaceDriverError::Protocol(format!("recovered tool content could not be encoded: {error}"))
    })
}

pub(crate) fn recovered_function_call_output(call_id: &str, output: impl Into<serde_json::Value>) -> serde_json::Value {
    let output = output.into();
    serde_json::json!({
        "type": "function_call_output",
        "call_id": call_id,
        "output": output,
    })
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RecoveredToolResultRolloutEvidence {
    pub(crate) output_persisted: bool,
    pub(crate) successor_turn_id: Option<String>,
    pub(crate) native_function_calls: usize,
    pub(crate) native_receipt: bool,
}

impl RecoveredToolResultRolloutEvidence {
    pub(crate) fn validate_injection(&self, expected: &serde_json::Value) -> Result<(), InterfaceDriverError> {
        let call_id = expected
            .get("call_id")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty() && value.trim() == *value)
            .ok_or_else(|| {
                InterfaceDriverError::Protocol(
                    "recovered function-call output has no canonical call_id".into(),
                )
            })?;
        if self.output_persisted || self.native_function_calls == 1 {
            return Ok(());
        }
        Err(InterfaceDriverError::FailedClosed(format!(
            "Codex result recovery cannot inject call_id '{}' without one corresponding function call in the current native history (found {}); the admitted result requires explicit continuity repair",
            call_id, self.native_function_calls,
        )))
    }
}

pub(crate) fn retain_native_recovery_call(
    payload: &serde_json::Value,
    expected_call_id: &str,
    count: &mut usize,
) -> Result<(), InterfaceDriverError> {
    if payload.get("type").and_then(serde_json::Value::as_str) != Some("function_call")
        || payload.get("call_id").and_then(serde_json::Value::as_str) != Some(expected_call_id)
    {
        return Ok(());
    }
    serde_json::from_value::<octopus_protocol::models::ResponseItem>(payload.clone()).map_err(
        |error| {
            InterfaceDriverError::FailedClosed(format!(
                "Codex recovery function call '{expected_call_id}' is malformed: {error}"
            ))
        },
    )?;
    *count += 1;
    Ok(())
}

pub(crate) fn recovered_tool_result_rollout_evidence(
    rollout_path: &Path,
    expected: &serde_json::Value,
    activity: Option<&serde_json::Value>,
) -> Result<RecoveredToolResultRolloutEvidence, InterfaceDriverError> {
    let expected_call_id = expected
        .get("call_id")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty() && value.trim() == *value)
        .ok_or_else(|| {
            InterfaceDriverError::Protocol(
                "recovered function-call output has no canonical call_id".to_string(),
            )
        })?;
    let expected_output = expected.get("output").ok_or_else(|| {
        InterfaceDriverError::Protocol(
            "recovered function-call output has no output body".to_string(),
        )
    })?;
    let metadata = std::fs::metadata(rollout_path).map_err(|error| {
        InterfaceDriverError::FailedClosed(format!(
            "Codex recovery rollout '{}' cannot be inspected: {error}",
            rollout_path.display()
        ))
    })?;
    if !metadata.is_file() {
        return Err(InterfaceDriverError::FailedClosed(format!(
            "Codex recovery rollout '{}' is not a regular file",
            rollout_path.display()
        )));
    }
    let file = std::fs::File::open(rollout_path).map_err(|error| {
        InterfaceDriverError::FailedClosed(format!(
            "Codex recovery rollout '{}' cannot be opened: {error}",
            rollout_path.display()
        ))
    })?;
    let mut reader = BufReader::new(file);
    let mut line = String::new();
    let mut exact = 0usize;
    let mut dynamic_exact = 0usize;
    let mut conflicting = 0usize;
    let mut after_exact = false;
    let mut successor_turn_ids = Vec::new();
    let mut native_function_calls = 0usize;
    let mut compacted_output_persisted = false;
    loop {
        line.clear();
        let mut bounded = (&mut reader).take((RECOVERY_ROLLOUT_MAX_LINE_BYTES + 1) as u64);
        let bytes = bounded.read_line(&mut line).map_err(|error| {
            InterfaceDriverError::FailedClosed(format!(
                "Codex recovery rollout '{}' cannot be read: {error}",
                rollout_path.display()
            ))
        })?;
        if bytes == 0 {
            break;
        }
        if bytes > RECOVERY_ROLLOUT_MAX_LINE_BYTES {
            return Err(InterfaceDriverError::FailedClosed(format!(
                "Codex recovery rollout '{}' contains a line larger than {} bytes",
                rollout_path.display(),
                RECOVERY_ROLLOUT_MAX_LINE_BYTES
            )));
        }
        if !line.ends_with('\n') {
            return Err(InterfaceDriverError::FailedClosed(format!(
                "Codex recovery rollout '{}' ends with a partial JSONL record",
                rollout_path.display()
            )));
        }
        let value: serde_json::Value = serde_json::from_str(line.trim_end()).map_err(|error| {
            InterfaceDriverError::FailedClosed(format!(
                "Codex recovery rollout '{}' contains malformed JSONL: {error}",
                rollout_path.display()
            ))
        })?;
        match value.get("type").and_then(serde_json::Value::as_str) {
            Some("response_item") => {
                let Some(payload) = value.get("payload") else {
                    return Err(InterfaceDriverError::FailedClosed(format!(
                        "Codex recovery rollout '{}' has a response_item without payload",
                        rollout_path.display()
                    )));
                };
                retain_native_recovery_call(payload, expected_call_id, &mut native_function_calls)?;
                if payload.get("type").and_then(serde_json::Value::as_str)
                    != Some("function_call_output")
                    || payload.get("call_id").and_then(serde_json::Value::as_str)
                        != Some(expected_call_id)
                {
                    continue;
                }
                if payload.get("output").is_some_and(|recorded| {
                    crate::result_media::matches_recorded_output(expected_output, recorded)
                }) {
                    exact += 1;
                    after_exact = true;
                } else {
                    conflicting += 1;
                }
            }
            Some("compacted") => {
                let payload = value.get("payload").ok_or_else(|| {
                    InterfaceDriverError::FailedClosed(format!(
                        "Codex recovery rollout '{}' has a compacted record without payload",
                        rollout_path.display()
                    ))
                })?;
                let compacted: octopus_protocol::protocol::CompactedItem =
                    serde_json::from_value(payload.clone()).map_err(|error| {
                        InterfaceDriverError::FailedClosed(format!(
                            "Codex recovery rollout '{}' has malformed compaction history: {error}",
                            rollout_path.display()
                        ))
                    })?;
                native_function_calls = 0;
                if let Some(history) = compacted.replacement_history {
                    let mut outputs = 0usize;
                    for item in history {
                        match item {
                            octopus_protocol::models::ResponseItem::FunctionCall { call_id, .. }
                                if call_id == expected_call_id => native_function_calls += 1,
                            octopus_protocol::models::ResponseItem::FunctionCallOutput {
                                call_id: Some(call_id), output, ..
                            } if call_id == expected_call_id => {
                                outputs += 1;
                                let output = serde_json::to_value(output).map_err(|error| {
                                    InterfaceDriverError::Protocol(format!(
                                        "Codex compacted result '{expected_call_id}' did not encode: {error}"
                                    ))
                                })?;
                                if outputs > 1
                                    || !crate::result_media::matches_recorded_output(expected_output, &output)
                                {
                                    return Err(InterfaceDriverError::FailedClosed(format!(
                                        "Codex compaction has ambiguous or conflicting output for call_id '{expected_call_id}'"
                                    )));
                                }
                                compacted_output_persisted = true;
                                after_exact = true;
                            }
                            _ => {}
                        }
                    }
                }
            }
            Some("event_msg") => {
                let payload = value.get("payload").ok_or_else(|| {
                    InterfaceDriverError::FailedClosed(format!(
                        "Codex recovery rollout '{}' has an event_msg without payload",
                        rollout_path.display()
                    ))
                })?;
                if let Some(activity) = activity
                    && payload.get("type").and_then(serde_json::Value::as_str) == Some("item_completed")
                    && let Some(item) = payload.get("item")
                    && item.get("type").and_then(serde_json::Value::as_str) == Some("DynamicToolCall")
                    && item.get("id").and_then(serde_json::Value::as_str) == Some(expected_call_id)
                {
                    if payload.get("thread_id") == activity.get("thread_id")
                        && payload.get("turn_id") == activity.get("turn_id")
                    {
                        dynamic_exact += 1;
                        after_exact = true;
                    } else {
                        conflicting += 1;
                    }
                }
                if !after_exact
                    || !matches!(
                        payload.get("type").and_then(serde_json::Value::as_str),
                        Some("task_started" | "turn_started")
                    )
                {
                    continue;
                }
                let turn_id = payload
                    .get("turn_id")
                    .and_then(serde_json::Value::as_str)
                    .filter(|value| !value.is_empty() && value.trim() == *value)
                    .ok_or_else(|| {
                        InterfaceDriverError::FailedClosed(format!(
                            "Codex recovery rollout '{}' has a successor TurnStarted without canonical turn_id",
                            rollout_path.display()
                        ))
                    })?;
                successor_turn_ids.push(turn_id.to_string());
            }
            _ => {}
        }
    }
    match (exact, dynamic_exact, conflicting, compacted_output_persisted) {
        (0, 0, 0, false) => Ok(RecoveredToolResultRolloutEvidence {
            native_function_calls,
            ..RecoveredToolResultRolloutEvidence::default()
        }),
        (0..=1, 0..=1, 0, _) if successor_turn_ids.len() <= 1 => Ok(RecoveredToolResultRolloutEvidence {
            output_persisted: true,
            successor_turn_id: successor_turn_ids.pop(),
            native_function_calls,
            native_receipt: dynamic_exact == 1,
        }),
        (0..=1, 0..=1, 0, _) => Err(InterfaceDriverError::FailedClosed(format!(
            "Codex recovery rollout '{}' carries {} successor TurnStarted receipts after call_id '{}'",
            rollout_path.display(),
            successor_turn_ids.len(),
            expected_call_id
        ))),
        _ => Err(InterfaceDriverError::FailedClosed(format!(
            "Codex recovery rollout '{}' carries ambiguous output history for call_id '{}' (exact={exact}, dynamic_exact={dynamic_exact}, conflicting={conflicting})",
            rollout_path.display(),
            expected_call_id
        ))),
    }
}

pub(crate) fn verify_native_tool_receipt(
    entry: &octopus_appserver_proto::ThreadItemEntry,
    expected: &serde_json::Value,
    activity: &serde_json::Value,
) -> Result<(), InterfaceDriverError> {
    let call_id = expected.get("call_id").and_then(serde_json::Value::as_str);
    let ThreadItem::DynamicToolCall { id, namespace, tool, status, content_items, success, .. } = &entry.item else {
        return Err(InterfaceDriverError::FailedClosed(format!(
            "Codex native receipt for {call_id:?} is not a dynamic tool call"
        )));
    };
    let content = content_items.clone().ok_or_else(|| InterfaceDriverError::FailedClosed(format!(
        "Codex native receipt for '{id}' carries no content items"
    )))?;
    let matches = Some(id.as_str()) == call_id
        && activity.get("turn_id").and_then(serde_json::Value::as_str) == Some(entry.turn_id.as_str())
        && activity.get("namespace").and_then(serde_json::Value::as_str) == namespace.as_deref()
        && activity.get("tool").and_then(serde_json::Value::as_str)
            .is_some_and(|exact| encode_tool_name(exact) == *tool)
        && activity.get("success").and_then(serde_json::Value::as_bool) == *success
        && activity.get("status") == Some(&serde_json::to_value(status)
            .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?)
        && expected.get("output") == Some(&recovered_result_output(content)?);
    if !matches {
        return Err(InterfaceDriverError::FailedClosed(format!(
            "Codex native receipt for '{id}' conflicts with the admitted tool result"
        )));
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn recovered_function_call_output_is_persisted(
    rollout_path: &Path,
    expected: &serde_json::Value,
) -> Result<bool, InterfaceDriverError> {
    recovered_tool_result_rollout_evidence(rollout_path, expected, None)
        .map(|evidence| evidence.output_persisted)
}

pub(crate) fn reconcile_recovered_successor_dispatch(
    rollout: &RecoveredToolResultRolloutEvidence,
    dispatches: &[RecoveredTurnStartDispatch],
) -> Result<(), InterfaceDriverError> {
    match (dispatches, rollout.successor_turn_id.as_deref()) {
        ([], None) => Ok(()),
        ([], Some(turn_id)) => Err(InterfaceDriverError::FailedClosed(format!(
            "Codex recovery rollout already started successor turn '{turn_id}' without matching durable turn/start dispatch evidence"
        ))),
        ([dispatch], Some(turn_id)) => Err(InterfaceDriverError::FailedClosed(format!(
            "Codex recovered tool-result continuation already started successor turn '{turn_id}' after durable turn/start dispatch movement {} (attempt {}, request {}); refusing a duplicate successor turn",
            dispatch.movement_seq, dispatch.attempt, dispatch.request_blake3
        ))),
        ([dispatch], None) => Err(InterfaceDriverError::FailedClosed(format!(
            "Codex recovered tool-result continuation has durable turn/start dispatch movement {} (attempt {}, artifact '{}', request {}) but no exact successor TurnStarted receipt; the outcome is ambiguous and a second successor turn is forbidden",
            dispatch.movement_seq,
            dispatch.attempt,
            dispatch.artifact.id.as_str(),
            dispatch.request_blake3
        ))),
        (dispatches, _) => Err(InterfaceDriverError::FailedClosed(format!(
            "Codex recovered tool-result continuation carries {} durable turn/start dispatches; successor execution is ambiguous",
            dispatches.len()
        ))),
    }
}

pub(crate) fn interface_operation_response_value(
    result: &InterfaceOperationResultArtifact,
    source_artifact: &ArtifactRef,
    projection: ModelResultProjection,
) -> Result<serde_json::Value, InterfaceDriverError> {
    serde_json::to_value(DynamicToolCallResponse {
        content_items: interface_operation_response_content(result, source_artifact, projection)?,
        success: result.ok,
    })
    .map_err(|error| {
        InterfaceDriverError::Protocol(format!(
            "interface operation response could not be encoded: {error}"
        ))
    })
}

pub(crate) fn interface_operation_response_content(
    result: &InterfaceOperationResultArtifact,
    source_artifact: &ArtifactRef,
    projection: ModelResultProjection,
) -> Result<Vec<DynamicToolCallOutputContentItem>, InterfaceDriverError> {
    let identity = serde_json::json!({"operation": result.operation, "correlation_id": result.correlation_id, "ok": result.ok});
    match crate::result_media::project(&result.value)? {
        Some(media) => native_artifact_result_content(&media.value, media.images, identity, source_artifact, projection),
        None => native_artifact_result_content(&result.value, Vec::new(), identity, source_artifact, projection),
    }
}

pub(crate) fn model_result_text(
    value: &serde_json::Value,
    identity: serde_json::Value,
    source_artifact: &ArtifactRef,
    projection: ModelResultProjection,
) -> Result<String, InterfaceDriverError> {
    let raw = serde_json::to_string(value).map_err(|error| {
        InterfaceDriverError::Protocol(format!(
            "interface result could not be rendered for app-server continuation: {error}"
        ))
    })?;
    let original_tokens = octopus_util_string::approx_token_count(&raw);
    if original_tokens <= projection.max_tokens {
        return Ok(raw);
    }

    let mut preview_budget = projection.max_tokens.saturating_sub(256);
    for _ in 0..8 {
        let (preview, _) =
            octopus_util_string::truncate_middle_with_token_budget(&raw, preview_budget);
        let retained_tokens = octopus_util_string::approx_token_count(&preview);
        let projected = serde_json::json!({
            "model_projection": "head_tail",
            "truncated": true,
            "identity": identity,
            "source_artifact": source_artifact,
            "rendered_json_blake3": blake3::hash(raw.as_bytes()).to_hex().to_string(),
            "original_bytes": raw.len(),
            "original_approx_tokens": original_tokens,
            "retained_approx_tokens": retained_tokens,
            "omitted_approx_tokens": original_tokens.saturating_sub(retained_tokens),
            "max_tokens": projection.max_tokens,
            "preview": preview,
        });
        let text = serde_json::to_string(&projected).map_err(|error| {
            InterfaceDriverError::Protocol(format!(
                "model result projection metadata could not be encoded: {error}"
            ))
        })?;
        let projected_tokens = octopus_util_string::approx_token_count(&text);
        if projected_tokens <= projection.max_tokens {
            return Ok(text);
        }
        let excess = projected_tokens.saturating_sub(projection.max_tokens);
        preview_budget = preview_budget.saturating_sub(excess.saturating_add(8));
    }
    Err(InterfaceDriverError::FailedClosed(format!(
        "model result projection could not fit descriptor max_tokens={} without losing its evidence header",
        projection.max_tokens
    )))
}
