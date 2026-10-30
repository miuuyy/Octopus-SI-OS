use super::*;

pub(crate) fn assemble_current_context(
    payloads: &[octopus_interfaces::ArtifactBindingPayload],
    bound: &BoundSessionContexts,
    provider: &InterfaceId,
    snapshot_labels: &BTreeSet<String>,
    current: &[SessionContextArtifact],
    retained: &[SessionContextArtifact],
    input: &ArtifactRef,
    projection: ModelResultProjection,
) -> Result<AssembledContext, InterfaceDriverError> {
    let sources = retained.iter().chain(current).map(|entry| continuity_source_key(&entry.source_artifact)).collect::<BTreeSet<_>>();
    let statics = payloads.iter().filter(|payload| {
        let key = continuity_source_key(&payload.artifact);
        !sources.contains(&key) && (current.is_empty() || !matches!(bound.get(&key), Some(BoundSessionContext::Current(_))))
    }).cloned().collect::<Vec<_>>();
    let mut assembled = assemble_context_resolved(&statics, bound, provider, snapshot_labels, projection)?;
    for entry in current.iter().filter(|entry| &entry.source_artifact != input) {
        append_assembled_context(&mut assembled, Some(&entry.label), &entry.payload,
            entry.source_artifact.id.as_str(), &entry.source_artifact, projection)?;
        assembled.session_context_available = true;
    }
    Ok(assembled)
}

pub(crate) fn assemble_context_resolved(
    artifact_binding_payloads: &[octopus_interfaces::ArtifactBindingPayload],
    bound_session_contexts: &BoundSessionContexts,
    provider_interface: &InterfaceId,
    session_snapshot_labels: &BTreeSet<String>,
    projection: ModelResultProjection,
) -> Result<AssembledContext, InterfaceDriverError> {
    let mut assembled = AssembledContext::default();
    let mut session_sources = BTreeSet::new();
    for bound in artifact_binding_payloads {
        let bytes = bound.read_bytes()
            .map_err(|error| InterfaceDriverError::FailedClosed(error.to_string()))?;
        let mut value: serde_json::Value =
            serde_json::from_slice(bytes.as_ref()).map_err(|e| {
                InterfaceDriverError::Protocol(format!(
                    "artifact-turn bound artifact {} is not JSON: {e}",
                    bound.artifact.id.as_str()
                ))
            })?;
        if value.get(SESSION_CONTEXT_FIELD).is_some() {
            let label = continuity_carrier_label(&value, bound.artifact.id.as_str())?;
            if !session_snapshot_labels.contains(label) {
                return Err(InterfaceDriverError::FailedClosed(format!(
                    "artifact-turn session context {} is not on a descriptor-declared snapshot output",
                    bound.artifact.id.as_str()
                )));
            }
            let context = bound_session_contexts
                .get(&continuity_source_key(&bound.artifact))
                .ok_or_else(|| {
                    InterfaceDriverError::FailedClosed(format!(
                        "artifact-turn session context {} has no resolved snapshot",
                        bound.artifact.id.as_str()
                    ))
                })?;
            if matches!(context, BoundSessionContext::Foreign) {
                let label = label.to_string();
                let object = value.as_object_mut().ok_or_else(|| {
                    InterfaceDriverError::Protocol(format!(
                        "artifact-turn foreign terminal seed {} is not an object",
                        bound.artifact.id.as_str()
                    ))
                })?;
                object.remove(SESSION_CONTEXT_FIELD);
                object.remove(CODEX_CONTINUITY_FIELD);
                object.remove(CONTEXT_WINDOW_USAGE_FIELD);
                let source_key = format!(
                    "{}:{}:{}",
                    bound.artifact.id.as_str(),
                    bound.artifact.content.to_hex(),
                    label
                );
                if !session_sources.insert(source_key) {
                    return Err(InterfaceDriverError::Protocol(format!(
                        "artifact-turn foreign terminal seed {} repeats its immutable source",
                        bound.artifact.id.as_str()
                    )));
                }
                append_assembled_context(
                    &mut assembled,
                    Some(&label),
                    &value,
                    &format!("{} foreign terminal seed", bound.artifact.id.as_str()),
                    &bound.artifact,
                    projection,
                )?;
                continue;
            }
            let BoundSessionContext::Current(snapshot_artifacts) = context else {
                unreachable!();
            };
            assembled.session_context_available = true;
            if snapshot_artifacts.is_empty() {
                return Err(InterfaceDriverError::FailedClosed(format!(
                    "artifact-turn session context {} resolves no current terminal artifact",
                    bound.artifact.id.as_str()
                )));
            }
            for (index, artifact) in snapshot_artifacts.iter().enumerate() {
                if artifact.label.is_empty() || artifact.label.trim() != artifact.label {
                    return Err(InterfaceDriverError::Protocol(format!(
                        "artifact-turn session context {} artifact[{index}] has an invalid label",
                        bound.artifact.id.as_str()
                    )));
                }
                let source_key = format!(
                    "{}:{}:{}",
                    artifact.source_artifact.id.as_str(),
                    artifact.source_artifact.content.to_hex(),
                    artifact.label
                );
                if !session_sources.insert(source_key) {
                    return Err(InterfaceDriverError::Protocol(format!(
                        "artifact-turn session context {} repeats an artifact source across the invocation at index {index}",
                        bound.artifact.id.as_str()
                    )));
                }
                let foreign = is_foreign_provider_payload(&artifact.payload, provider_interface)?;
                if !foreign && artifact.payload.get(SESSION_CONTEXT_FIELD).is_some() {
                    return Err(InterfaceDriverError::Protocol(format!(
                        "artifact-turn session context {} artifact[{index}] nests reserved session context",
                        bound.artifact.id.as_str()
                    )));
                }
                append_assembled_context(
                    &mut assembled,
                    Some(&artifact.label),
                    &if foreign { visible_foreign_provider_payload(&artifact.payload) } else { artifact.payload.clone() },
                    &format!("{} session artifact {index}", bound.artifact.id.as_str()),
                    &artifact.source_artifact,
                    projection,
                )?;
            }
        } else {
            if value.get("label").and_then(serde_json::Value::as_str) == Some(CONTEXT_REFERENCE_LABEL) {
                append_static_context_source(&mut assembled.static_context_bytes, bound)?;
            }
            append_assembled_context(
                &mut assembled,
                value.get("label").and_then(serde_json::Value::as_str),
                &value,
                bound.artifact.id.as_str(),
                &bound.artifact,
                projection,
            )?;
        }
    }
    Ok(assembled)
}

#[cfg(test)]
pub(crate) fn test_bound_session_contexts(
    artifact_binding_payloads: &[octopus_interfaces::ArtifactBindingPayload],
    provider_interface: &InterfaceId,
    session_snapshot_labels: &BTreeSet<String>,
) -> Result<BoundSessionContexts, InterfaceDriverError> {
    let mut contexts = BTreeMap::new();
    let mut current_seen = false;
    for bound in artifact_binding_payloads {
        let bytes = bound.read_bytes()
            .map_err(|error| InterfaceDriverError::FailedClosed(error.to_string()))?;
        let mut value: serde_json::Value =
            serde_json::from_slice(bytes.as_ref()).map_err(|error| {
                InterfaceDriverError::Protocol(format!(
                    "test session carrier '{}' is not JSON: {error}",
                    bound.artifact.id
                ))
            })?;
        let Some(raw) = value.get(SESSION_CONTEXT_FIELD).cloned() else {
            continue;
        };
        let label = continuity_carrier_label(&value, bound.artifact.id.as_str())?.to_string();
        if !session_snapshot_labels.contains(&label) {
            return Err(InterfaceDriverError::FailedClosed(format!(
                "test session carrier '{}' is not descriptor-declared",
                bound.artifact.id
            )));
        }
        let snapshot: SessionContextSnapshot = serde_json::from_value(raw).map_err(|error| {
            InterfaceDriverError::Protocol(format!(
                "test session carrier '{}' is malformed: {error}",
                bound.artifact.id
            ))
        })?;
        if snapshot.schema_version != 3 || snapshot._parent.is_some() {
            return Err(InterfaceDriverError::FailedClosed(format!(
                "test session carrier '{}' requires unsupported snapshot coordinates",
                bound.artifact.id
            )));
        }
        let key = continuity_source_key(&bound.artifact);
        if snapshot.provider_interface != *provider_interface {
            contexts.insert(key, BoundSessionContext::Foreign);
            continue;
        }
        if current_seen {
            return Err(InterfaceDriverError::FailedClosed(
                "test invocation carries multiple current session snapshots".to_string(),
            ));
        }
        current_seen = true;
        value
            .as_object_mut()
            .ok_or_else(|| {
                InterfaceDriverError::Protocol(format!(
                    "test session carrier '{}' is not an object",
                    bound.artifact.id
                ))
            })?
            .remove(SESSION_CONTEXT_FIELD);
        let mut artifacts = snapshot._artifacts;
        artifacts.push(SessionContextArtifact {
            label,
            payload: value,
            source_artifact: bound.artifact.clone(),
            source_movement_seq: None,
        });
        contexts.insert(key, BoundSessionContext::Current(Arc::new(artifacts)));
    }
    Ok(contexts)
}

#[cfg(test)]
pub(crate) fn recover_codex_continuity(
    artifact_binding_payloads: &[octopus_interfaces::ArtifactBindingPayload],
    provider_interface: &InterfaceId,
    run: &str,
    context_output_labels: &BTreeSet<String>,
    session_snapshot_labels: &BTreeSet<String>,
) -> Result<Option<CodexContinuityState>, InterfaceDriverError> {
    let contexts = test_bound_session_contexts(
        artifact_binding_payloads,
        provider_interface,
        session_snapshot_labels,
    )?;
    recover_codex_continuity_resolved(
        artifact_binding_payloads,
        &contexts,
        provider_interface,
        run,
        context_output_labels,
        session_snapshot_labels,
        &[],
    )
}

#[cfg(test)]
pub(crate) fn assemble_context(
    artifact_binding_payloads: &[octopus_interfaces::ArtifactBindingPayload],
    interface_id: &str,
    session_snapshot_labels: &BTreeSet<String>,
) -> Result<AssembledContext, InterfaceDriverError> {
    let provider_interface = InterfaceId::new(interface_id);
    let contexts = test_bound_session_contexts(
        artifact_binding_payloads,
        &provider_interface,
        session_snapshot_labels,
    )?;
    assemble_context_resolved(
        artifact_binding_payloads,
        &contexts,
        &provider_interface,
        session_snapshot_labels,
        ModelResultProjection { max_tokens: 10_000 },
    )
}

pub(crate) fn append_assembled_context(
    assembled: &mut AssembledContext,
    label: Option<&str>,
    value: &serde_json::Value,
    source: &str,
    source_artifact: &ArtifactRef,
    projection: ModelResultProjection,
) -> Result<(), InterfaceDriverError> {
    let label = label.map(str::trim).filter(|label| !label.is_empty());
    let text = value.get("text").and_then(serde_json::Value::as_str);
    if label == Some(CONTEXT_REFERENCE_LABEL) {
        let text = text.ok_or_else(|| {
            InterfaceDriverError::Protocol(format!(
                "artifact-turn role artifact {source} (label context.reference) has no string 'text' field"
            ))
        })?;
        assembled.role.push(text.to_string());
        return Ok(());
    }
    let speaker = match label {
        Some(OPERATOR_MESSAGE_LABEL) => "User",
        Some(TURN_AGENT_MESSAGE_LABEL | INTERFACE_ARTIFACT_OUTPUT_LABEL) => "Assistant",
        Some(TURN_PLAN_UPDATE_LABEL) => "Assistant plan",
        _ => "Artifact",
    };
    if label == Some(TURN_REASONING_LABEL) {
        let summary: Vec<String> = serde_json::from_value(value.get("summary").cloned().unwrap_or(serde_json::Value::Null))
            .map_err(|error| InterfaceDriverError::Protocol(format!("reasoning summary in {source} is malformed: {error}")))?;
        if !summary.is_empty() {
            assembled.conversation.push(format!("Assistant reasoning summary\n{}", summary.join("\n")));
        }
    } else if let Some(text) = text.filter(|text| !text.trim().is_empty()) {
        assembled.conversation.push(format!("{speaker}\n{text}"));
    } else if let Some(prompt) = value
        .get("prompt")
        .and_then(serde_json::Value::as_str)
        .filter(|prompt| !prompt.trim().is_empty())
    {
        assembled.conversation.push(format!("{speaker}\n{prompt}"));
    } else {
        let mut visible = crate::result_media::without_embedded_payloads(value);
        if let Some(object) = visible.as_object_mut() {
            for field in [CODEX_CONTINUITY_FIELD, SESSION_CONTEXT_FIELD, CONTEXT_WINDOW_USAGE_FIELD,
                "provider_token_usage", "thread_id", "turn_id", "item_id", "completed_at_ms", "lifecycle"] {
                object.remove(field);
            }
        }
        let payload = model_result_text(
            &visible,
            serde_json::json!({"kind": "context_artifact", "label": label}),
            source_artifact,
            projection,
        )?;
        assembled
            .conversation
            .push(format!("artifact {source}\n{payload}"));
    }
    Ok(())
}

pub(crate) fn extract_prompt_from_value(value: &serde_json::Value) -> Result<String, InterfaceDriverError> {
    let object = value.as_object().ok_or_else(|| {
        InterfaceDriverError::Protocol(
            "interface input artifact payload must be a JSON object".into(),
        )
    })?;
    let prompt = object
        .get("prompt")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            InterfaceDriverError::Protocol(
                "interface input artifact payload has no non-empty 'prompt' field".into(),
            )
        })?;
    Ok(prompt)
}

pub(crate) fn extract_turn_input_from_value(
    value: &serde_json::Value,
    selected_files: Option<&SelectedFileProjection>,
) -> Result<Vec<UserInput>, InterfaceDriverError> {
    let object = value.as_object().ok_or_else(|| {
        InterfaceDriverError::Protocol(
            "interface input artifact payload must be a JSON object".into(),
        )
    })?;
    if let Some(source_artifact) = object.get(SOURCE_ARTIFACT_FIELD) {
        return turn_input_from_source_artifact(source_artifact);
    }
    if object.contains_key("text") || object.contains_key("attachments") {
        return turn_input_from_operator_payload(value, selected_files);
    }
    if let Some(task) = object.get("task") {
        let text = match task {
            serde_json::Value::String(s) => s.clone(),
            other => serde_json::to_string_pretty(other).map_err(|e| {
                InterfaceDriverError::Protocol(format!("waking task field failed to render: {e}"))
            })?,
        };
        if text.trim().is_empty() {
            return Err(InterfaceDriverError::Protocol(
                "waking task is empty; refusing to fabricate input (fail-closed)".into(),
            ));
        }
        return Ok(vec![UserInput::Text {
            text,
            text_elements: Vec::new(),
        }]);
    }
    let prompt = extract_prompt_from_value(value)?;
    Ok(vec![UserInput::Text {
        text: prompt,
        text_elements: Vec::new(),
    }])
}

pub(crate) fn turn_input_from_source_artifact(
    source_artifact: &serde_json::Value,
) -> Result<Vec<UserInput>, InterfaceDriverError> {
    let manifest: InterfaceArtifactManifest = serde_json::from_value(source_artifact.clone())
        .map_err(|e| {
            InterfaceDriverError::Protocol(format!(
                "interface input artifact source_artifact is not an interface artifact manifest: {e}"
            ))
        })?;
    if manifest.label != OPERATOR_MESSAGE_LABEL {
        return Err(InterfaceDriverError::Protocol(format!(
            "codex_app_server interface does not support source artifact label '{}'",
            manifest.label
        )));
    }
    let payload_bytes = read_artifact_payload_checked(
        manifest.label.as_str(),
        &manifest.payload_path,
        manifest.payload_content_hash.as_deref(),
    )?;
    let payload: serde_json::Value = serde_json::from_slice(&payload_bytes).map_err(|e| {
        InterfaceDriverError::Protocol(format!(
            "source artifact '{}' payload is not JSON: {e}",
            manifest.label
        ))
    })?;
    turn_input_from_operator_payload(&payload, None)
}

pub(crate) fn turn_input_from_operator_payload(
    payload: &serde_json::Value,
    selected_files: Option<&SelectedFileProjection>,
) -> Result<Vec<UserInput>, InterfaceDriverError> {
    let mut input = Vec::new();
    if let Some(context) = payload.get("client_context") {
        input.push(UserInput::Text { text: render_operator_client_context(context)?, text_elements: Vec::new() });
    }
    let context_items = input.len();
    if let Some(text) = payload
        .get("text")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.trim().is_empty())
    {
        input.push(UserInput::Text {
            text: text.to_string(),
            text_elements: Vec::new(),
        });
    }

    if let Some(attachments) = payload
        .get("attachments")
        .and_then(serde_json::Value::as_array)
    {
        for (index, attachment) in attachments.iter().enumerate() {
            push_attachment_input(
                &mut input,
                index,
                attachment,
                selected_files.is_some_and(|files| files.current_attachments.contains(&index)),
            )?;
        }
    }

    if input.len() == context_items
        && !selected_files.is_some_and(|files| !files.current_attachments.is_empty())
    {
        return Err(InterfaceDriverError::Protocol(
            "operator content contains neither text nor supported attachments".into(),
        ));
    }
    Ok(input)
}

pub(crate) const OPERATOR_CLIENT_CONTEXT_MAX_ENTRIES: usize = 4;
pub(crate) const OPERATOR_CLIENT_CONTEXT_MAX_BYTES: usize = 16 * 1024;

pub(crate) fn render_operator_client_context(value: &serde_json::Value) -> Result<String, InterfaceDriverError> {
    let invalid = |detail: &str| InterfaceDriverError::Protocol(format!("operator client_context {detail}"));
    let entries = value.as_array().ok_or_else(|| invalid("must be an array"))?;
    if entries.is_empty() || entries.len() > OPERATOR_CLIENT_CONTEXT_MAX_ENTRIES {
        return Err(invalid("must carry 1 to 4 entries"));
    }
    let mut names = BTreeSet::new();
    let mut bytes = 0usize;
    let mut blocks = Vec::with_capacity(entries.len());
    for entry in entries {
        let object = entry.as_object().filter(|object| object.len() == 2)
            .ok_or_else(|| invalid("entries must be exact {name, text} objects"))?;
        let name = object.get("name").and_then(serde_json::Value::as_str)
            .filter(|name| !name.is_empty() && name.len() <= 64
                && name.bytes().next().is_some_and(|byte| byte.is_ascii_lowercase())
                && name.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'))
            .ok_or_else(|| invalid("entry name must be a lowercase token"))?;
        if !names.insert(name) {
            return Err(invalid("repeats an entry name"));
        }
        let text = object.get("text").and_then(serde_json::Value::as_str)
            .filter(|text| !text.trim().is_empty() && text.trim() == *text)
            .ok_or_else(|| invalid("entry text must be nonempty and trimmed"))?;
        bytes += text.len();
        if bytes > OPERATOR_CLIENT_CONTEXT_MAX_BYTES {
            return Err(invalid("exceeds its byte limit"));
        }
        blocks.push(format!("<client_context name=\"{name}\">\n{text}\n</client_context>"));
    }
    Ok(blocks.join("\n"))
}

pub(crate) fn push_attachment_input(
    input: &mut Vec<UserInput>,
    index: usize,
    attachment: &serde_json::Value,
    selected_file: bool,
) -> Result<(), InterfaceDriverError> {
    let object = attachment.as_object().ok_or_else(|| {
        InterfaceDriverError::Protocol(format!(
            "operator.message attachment[{index}] must be a JSON object"
        ))
    })?;
    let label = object
        .get("label")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("operator.attachment");
    let mime_type = object
        .get("mime_type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("application/octet-stream");
    let expected_hash = object
        .get("payload_content_hash")
        .and_then(serde_json::Value::as_str);

    if label == "operator.media.image" || mime_type.starts_with("image/") {
        let payload_path = path_from_field(object.get("payload_path"), index)?;
        let _ = read_artifact_payload_checked(label, &payload_path, expected_hash)?;
        input.push(UserInput::LocalImage {
            detail: None,
            path: payload_path,
        });
        return Ok(());
    }

    if label == "operator.file.text" || mime_type.starts_with("text/") {
        let payload_path = path_from_field(object.get("payload_path"), index)?;
        let bytes = read_artifact_payload_checked(label, &payload_path, expected_hash)?;
        let text = String::from_utf8(bytes).map_err(|e| {
            InterfaceDriverError::Protocol(format!(
                "operator.message attachment[{index}] text payload is not UTF-8: {e}"
            ))
        })?;
        let name = object
            .get("name")
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("attached text file");
        input.push(UserInput::Text {
            text: format!("Attachment: {name}\n\n{text}"),
            text_elements: Vec::new(),
        });
        return Ok(());
    }

    if selected_file {
        return Ok(());
    }

    Err(InterfaceDriverError::Protocol(format!(
        "codex_app_server interface does not support operator.message attachment[{index}] label \
         '{label}' with mime_type '{mime_type}'"
    )))
}

pub(crate) fn path_from_field(
    value: Option<&serde_json::Value>,
    attachment_index: usize,
) -> Result<PathBuf, InterfaceDriverError> {
    let path = value
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| {
            InterfaceDriverError::Protocol(format!(
                "operator.message attachment[{attachment_index}] has no payload_path"
            ))
        })?;
    let path = PathBuf::from(path);
    if !path.is_absolute() {
        return Err(InterfaceDriverError::Protocol(format!(
            "operator.message attachment[{attachment_index}] payload_path must be absolute: {}",
            path.display()
        )));
    }
    Ok(path)
}

pub(crate) fn read_artifact_payload_checked(
    label: &str,
    payload_path: &PathBuf,
    expected_hash: Option<&str>,
) -> Result<Vec<u8>, InterfaceDriverError> {
    if payload_path.as_os_str().is_empty() {
        return Err(InterfaceDriverError::Protocol(format!(
            "source artifact '{label}' payload_path is empty"
        )));
    }
    if !payload_path.is_absolute() {
        return Err(InterfaceDriverError::Protocol(format!(
            "source artifact '{label}' payload_path must be absolute: {}",
            payload_path.display()
        )));
    }
    let bytes = std::fs::read(payload_path).map_err(|e| {
        InterfaceDriverError::Protocol(format!(
            "source artifact '{label}' payload read failed at {}: {e}",
            payload_path.display()
        ))
    })?;
    if let Some(expected) = expected_hash.filter(|s| !s.trim().is_empty()) {
        let actual = sha256_hex(&bytes);
        if expected != actual {
            return Err(InterfaceDriverError::Protocol(format!(
                "source artifact '{label}' payload hash mismatch at {}: manifest={} actual={}",
                payload_path.display(),
                expected,
                actual
            )));
        }
    }
    Ok(bytes)
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(&mut out, "{byte:02x}").expect("writing to a String cannot fail");
    }
    out
}

pub(crate) fn required_formed_models(
    input: &InterfaceInvocationEnvelope,
) -> Result<BTreeSet<String>, InterfaceDriverError> {
    let policy = input
        .interface_policy()
        .map_err(|error| {
            InterfaceDriverError::FailedClosed(format!(
                "Codex formed interface policy is invalid: {error}"
            ))
        })?
        .ok_or_else(|| {
            InterfaceDriverError::FailedClosed(
                "Codex invocation carries no formed interface policy".to_string(),
            )
        })?;
    let values = policy
        .get("models")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            InterfaceDriverError::FailedClosed(
                "Codex formed interface policy models must be a string array".to_string(),
            )
        })?;
    if values.is_empty() {
        return Err(InterfaceDriverError::FailedClosed(
            "Codex formed interface policy models is empty".to_string(),
        ));
    }
    let mut models = BTreeSet::new();
    for (index, value) in values.iter().enumerate() {
        let model = value.as_str().filter(|model| {
            !model.is_empty() && model.trim() == *model && !model.contains(['\r', '\n', '\0'])
        });
        let Some(model) = model else {
            return Err(InterfaceDriverError::FailedClosed(format!(
                "Codex formed interface policy models[{index}] is not a trimmed string"
            )));
        };
        if !models.insert(model.to_string()) {
            return Err(InterfaceDriverError::FailedClosed(format!(
                "Codex formed interface policy models repeats '{model}'"
            )));
        }
    }
    Ok(models)
}

pub(crate) fn catalog_model_allowed(model: &str, formed_models: &BTreeSet<String>) -> bool {
    formed_models.contains(model)
}

pub(crate) fn validate_model_reroute(
    selected_model: &str,
    from_model: &str,
    to_model: &str,
) -> Result<(), InterfaceDriverError> {
    if from_model != selected_model {
        return Err(InterfaceDriverError::FailedClosed(format!(
            "Codex model reroute started from '{from_model}', but the descriptor selected '{selected_model}'"
        )));
    }
    if to_model != selected_model {
        return Err(InterfaceDriverError::FailedClosed(format!(
            "Codex attempted to reroute descriptor-selected model '{selected_model}' to '{to_model}'; hidden provider fallback is forbidden"
        )));
    }
    Ok(())
}

pub(crate) fn typed_error(error: TypedRequestError) -> InterfaceDriverError {
    match error {
        TypedRequestError::Transport { method, source } => {
            InterfaceDriverError::Transport(format!("{method}: {source}"))
        }
        TypedRequestError::Server { method, source } => InterfaceDriverError::Protocol(format!(
            "{method} failed: {} (code {})",
            source.message, source.code
        )),
        TypedRequestError::Deserialize { method, source } => {
            InterfaceDriverError::Protocol(format!("{method} response decode error: {source}"))
        }
    }
}

pub fn into_interface_driver(client: AppServerClient) -> Arc<CodexAppServerInterfaceDriver> {
    Arc::new(CodexAppServerInterfaceDriver::new(client))
}

pub fn into_interface_driver_with_config(
    client: AppServerClient,
    interface_driver_config: Option<&InterfaceDriverConfig>,
) -> Arc<CodexAppServerInterfaceDriver> {
    Arc::new(CodexAppServerInterfaceDriver::new_with_config(
        client,
        interface_driver_config,
    ))
}
