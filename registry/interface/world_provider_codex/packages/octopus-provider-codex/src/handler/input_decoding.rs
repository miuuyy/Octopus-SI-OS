use super::*;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub(crate) struct SelectedFileAlias {
    pub(crate) artifact_ref: ArtifactRef,
    pub(crate) filename: String,
    pub(crate) source_message_id: String,
    pub(crate) source_artifact: ArtifactRef,
    pub(crate) mime_type: String,
    pub(crate) size_bytes: u64,
}

pub(crate) struct SelectedFileProjection {
    pub(crate) text: String,
    pub(crate) current_attachments: BTreeSet<usize>,
    pub(crate) source_label: String,
    pub(crate) source_payload_sha256: String,
}

pub(crate) fn admitted_payload_context(
    record: &octopus_run_record::RunRecord,
    input: &InterfaceInvocationEnvelope,
) -> Result<(Vec<octopus_interfaces::ArtifactBindingPayload>, Vec<ArtifactRef>), InterfaceDriverError> {
    let metadata = input.metadata()?;
    let runtime = invocation_runtime_metadata(input)?;
    let context = octopus_run_authoring::context_artifact_payloads(
        record, metadata, &input.artifact_binding_payloads,
    ).map_err(InterfaceDriverError::FailedClosed)?;
    let selected = octopus_run_authoring::payload_artifact_bindings(record, &runtime.interface)
        .map_err(InterfaceDriverError::FailedClosed)?.into_iter()
        .filter(|reference| metadata.artifact_binding_refs.contains(reference)
            && metadata.grant.artifact_access.get(reference.id.as_str()).is_some_and(|access| access.read))
        .collect();
    Ok((context, selected))
}

pub(crate) fn continuity_input_sources(
    record: &octopus_run_record::RunRecord,
    input: &InterfaceInvocationEnvelope,
) -> Result<Vec<ArtifactRef>, InterfaceDriverError> {
    let metadata = input.metadata()?;
    let runtime = invocation_runtime_metadata(input)?;
    let mut sources = vec![input.artifact.clone()];
    if let Some(source) = octopus_run_authoring::input_artifacts::result_source(record, &runtime.interface, &runtime.checkpoint)
        .map_err(InterfaceDriverError::FailedClosed)? {
        let bound = input.artifact_binding_payloads.iter().find(|bound| bound.artifact == source)
            .ok_or_else(|| InterfaceDriverError::FailedClosed("result invocation lost its admitted source payload".into()))?;
        if !metadata.artifact_binding_refs.contains(&source)
            || metadata.grant.artifact_access.get(source.id.as_str()) != Some(&octopus_runtime::ArtifactAccess::read_only()) {
            return Err(InterfaceDriverError::FailedClosed("result invocation lost its exact source read grant".into()));
        }
        let original: serde_json::Value = serde_json::from_slice(&bound.read_bytes().map_err(|error| InterfaceDriverError::FailedClosed(error.to_string()))?)
            .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
        let incoming: serde_json::Value = serde_json::from_slice(&input.read_artifact_payload()?)
            .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
        if original != incoming {
            return Err(InterfaceDriverError::FailedClosed("result invocation changed its admitted source payload".into()));
        }
        sources.push(source);
    }
    Ok(sources)
}

pub(crate) fn continuity_context_payloads(
    record: &octopus_run_record::RunRecord,
    input: &InterfaceInvocationEnvelope,
    context: &[octopus_interfaces::ArtifactBindingPayload],
) -> Result<Vec<octopus_interfaces::ArtifactBindingPayload>, InterfaceDriverError> {
    let runtime = invocation_runtime_metadata(input)?;
    let mut payloads = context.to_vec();
    if let Some(call) = octopus_run_authoring::input_artifacts::result_call(record, &runtime.interface, &runtime.checkpoint)
        .map_err(InterfaceDriverError::FailedClosed)? {
        let metadata = input.metadata()?;
        if !metadata.artifact_binding_refs.contains(&call)
            || metadata.grant.artifact_access.get(call.id.as_str()) != Some(&octopus_runtime::ArtifactAccess::read_only()) {
            return Err(InterfaceDriverError::FailedClosed("result continuity lost its exact call evidence read grant".into()));
        }
        let bound = input.artifact_binding_payloads.iter().find(|bound| bound.artifact == call)
            .ok_or_else(|| InterfaceDriverError::FailedClosed("result continuity lost its exact call evidence payload".into()))?;
        if !payloads.iter().any(|payload| payload.artifact == call) { payloads.push(bound.clone()); }
    }
    Ok(payloads)
}

pub(crate) fn reconcile_input_session_continuity(
    record: &octopus_run_record::RunRecord,
    input: &InterfaceInvocationEnvelope,
    bound: Option<CodexContinuityState>,
    current: Option<CodexContinuityState>,
    current_session: &[SessionContextArtifact],
) -> Result<Option<CodexContinuityState>, InterfaceDriverError> {
    let runtime = invocation_runtime_metadata(input)?;
    if let (Some(bound), Some(current)) = (&bound, &current)
        && bound.context_generation >= current.context_generation
        && let Some(call) = octopus_run_authoring::input_artifacts::result_call(record, &runtime.interface, &runtime.checkpoint)
            .map_err(InterfaceDriverError::FailedClosed)? {
        let evidence = continuity_context_payloads(record, input, &[])?;
        let payload = evidence.iter().find(|payload| payload.artifact == call)
            .ok_or_else(|| InterfaceDriverError::FailedClosed("result continuity has no admitted call evidence".into()))?;
        let value: serde_json::Value = serde_json::from_slice(&payload.read_bytes()
            .map_err(|error| InterfaceDriverError::FailedClosed(error.to_string()))?)
            .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
        if value.get(CODEX_CONTINUITY_FIELD).map(|raw| parse_codex_continuity(raw, &runtime.interface, runtime.run.as_str())).transpose()?.as_ref() == Some(bound)
            && bound.pending.is_some() {
            if bound.context_generation > current.context_generation {
                return reconcile_current_session_continuity(Some(current.clone()), Some(bound.clone()));
            }
            let mut recorded_generation = false;
            for entry in current_session {
                if is_foreign_provider_payload(&entry.payload, &runtime.interface)? { continue; }
                if let Some(raw) = entry.payload.get(CODEX_CONTINUITY_FIELD) {
                    let state = parse_codex_continuity(raw, &runtime.interface, runtime.run.as_str())?;
                    recorded_generation |= state.context_generation >= bound.context_generation;
                }
            }
            let mut settled = bound.clone();
            settled.pending = None;
            settled.tool_operator_request = None;
            if !recorded_generation && &settled == current {
                return Ok(Some(bound.clone()));
            }
        }
    }
    reconcile_current_session_continuity(bound, current)
}

pub(crate) fn selected_file_projection(
    record: &octopus_run_record::RunRecord,
    input: &InterfaceInvocationEnvelope,
    source_artifact: &ArtifactRef,
    source_label: &str,
    source_bytes: &[u8],
    contexts: &BoundSessionContexts,
    current_context: &[SessionContextArtifact],
    projection: ModelResultProjection,
) -> Result<SelectedFileProjection, InterfaceDriverError> {
    let metadata = input.metadata()?;
    let runtime = invocation_runtime_metadata(input)?;
    let context_payloads = octopus_run_authoring::context_artifact_payloads(
        record,
        metadata,
        &input.artifact_binding_payloads,
    )
    .map_err(InterfaceDriverError::FailedClosed)?;
    let context_payloads = resolve_instruction_context(metadata, &context_payloads)
        .map_err(InterfaceDriverError::FailedClosed)?
        .remaining_payloads;
    let selected =
        octopus_run_authoring::payload_artifact_bindings(record, &runtime.interface)
            .map_err(InterfaceDriverError::FailedClosed)?;
    if selected.is_empty() {
        return Ok(SelectedFileProjection {
            text: "{\"selected_files\":[]}".into(),
            current_attachments: BTreeSet::new(),
            source_label: source_label.to_string(),
            source_payload_sha256: sha256_hex(source_bytes),
        });
    }
    let mut payloads = BTreeMap::<(String, u64), Vec<&ArtifactRef>>::new();
    for bound in &input.artifact_binding_payloads {
        if selected.contains(&bound.artifact) {
            let bytes = bound.read_bytes()
                .map_err(|error| InterfaceDriverError::FailedClosed(error.to_string()))?;
            payloads
                .entry((sha256_hex(bytes.as_ref()), bytes.len() as u64))
                .or_default()
                .push(&bound.artifact);
        }
    }
    let mut sources = BTreeMap::<String, (ArtifactRef, serde_json::Value)>::new();
    let mut insert = |reference: &ArtifactRef,
                      label: &str,
                      value: &serde_json::Value|
     -> Result<(), InterfaceDriverError> {
        if label != OPERATOR_MESSAGE_LABEL {
            return Ok(());
        }
        let key = continuity_source_key(reference);
        if let Some((previous, body)) = sources.get(&key) {
            if previous != reference || body != value {
                return Err(InterfaceDriverError::FailedClosed(
                    "selected file context repeats an immutable message with different bytes"
                        .into(),
                ));
            }
        } else {
            sources.insert(key, (reference.clone(), value.clone()));
        }
        Ok(())
    };
    if source_label == OPERATOR_MESSAGE_LABEL {
        let value = serde_json::from_slice(source_bytes).map_err(|error| {
            InterfaceDriverError::Protocol(format!(
                "selected file operator message is not JSON: {error}"
            ))
        })?;
        insert(source_artifact, source_label, &value)?;
    }
    for artifact in current_context {
        insert(
            &artifact.source_artifact,
            &artifact.label,
            &artifact.payload,
        )?;
    }
    for context in contexts.values() {
        if let BoundSessionContext::Current(artifacts) = context {
            for artifact in artifacts.iter() {
                insert(
                    &artifact.source_artifact,
                    &artifact.label,
                    &artifact.payload,
                )?;
            }
        }
    }
    for bound in &context_payloads {
        let bytes = bound.read_bytes()
            .map_err(|error| InterfaceDriverError::FailedClosed(error.to_string()))?;
        let value: serde_json::Value =
            serde_json::from_slice(bytes.as_ref()).map_err(|error| {
                InterfaceDriverError::Protocol(format!(
                    "selected file bound context '{}' is not JSON: {error}",
                    bound.artifact.id
                ))
            })?;
        if value.get(SESSION_CONTEXT_FIELD).is_some() {
            continue;
        }
        let label = value
            .get("label")
            .and_then(serde_json::Value::as_str)
            .or_else(|| {
                bound
                    .artifact
                    .id
                    .as_str()
                    .rsplit_once(':')
                    .and_then(|(label, hash)| {
                        (hash == bound.artifact.content.to_hex()).then_some(label)
                    })
            });
        if let Some(label) = label {
            insert(&bound.artifact, label, &value)?;
        }
    }
    let mut files = Vec::<SelectedFileAlias>::new();
    let mut current_attachments = BTreeSet::new();
    for (_, (reference, body)) in sources {
        let Some(attachments) = body.get("attachments") else {
            continue;
        };
        let attachments = attachments.as_array().ok_or_else(|| {
            InterfaceDriverError::Protocol(
                "selected file source attachments must be an array".into(),
            )
        })?;
        if attachments.is_empty() {
            continue;
        }
        let message_id = body
            .get("message_id")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty() && value.trim() == *value)
            .ok_or_else(|| {
                InterfaceDriverError::Protocol(
                    "selected file source has no exact message_id".into(),
                )
            })?;
        for (index, attachment) in attachments.iter().enumerate() {
            let hash = attachment
                .get("payload_content_hash")
                .and_then(serde_json::Value::as_str)
                .filter(|value| is_sha256_hex(value))
                .ok_or_else(|| {
                    InterfaceDriverError::Protocol(
                        "selected file attachment has no SHA256 payload pin".into(),
                    )
                })?;
            let size = attachment
                .get("size")
                .and_then(serde_json::Value::as_u64)
                .ok_or_else(|| {
                    InterfaceDriverError::Protocol(
                        "selected file attachment has no exact byte length".into(),
                    )
                })?;
            let Some(references) = payloads.get(&(hash.to_string(), size)) else {
                continue;
            };
            let filename = attachment
                .get("name")
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| {
                    InterfaceDriverError::Protocol(
                        "selected file attachment has no filename".into(),
                    )
                })?;
            let mime_type = attachment
                .get("mime_type")
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| {
                    InterfaceDriverError::Protocol(
                        "selected file attachment has no MIME type".into(),
                    )
                })?;
            for selected_ref in references {
                let alias = SelectedFileAlias {
                    artifact_ref: (*selected_ref).clone(),
                    filename: filename.to_string(),
                    source_message_id: message_id.to_string(),
                    source_artifact: reference.clone(),
                    mime_type: mime_type.to_string(),
                    size_bytes: size,
                };
                if !files.contains(&alias) {
                    files.push(alias);
                }
            }
            if &reference == source_artifact {
                current_attachments.insert(index);
            }
        }
    }
    let mut visible = serde_json::json!({"selected_files": files});
    let unnamed = input.artifact_binding_payloads.iter()
        .filter(|bound| selected.contains(&bound.artifact)
            && !files.iter().any(|file| file.artifact_ref == bound.artifact))
        .map(|bound| {
            let bytes = bound.read_bytes()
                .map_err(|error| InterfaceDriverError::FailedClosed(error.to_string()))?;
            Ok(serde_json::json!({
                "artifact_ref": bound.artifact,
                "label": bound.artifact.id.as_str().rsplit_once(':')
                    .filter(|(_, hash)| *hash == bound.artifact.content.to_hex())
                    .map_or(bound.artifact.id.as_str(), |(label, _)| label),
                "size_bytes": bytes.len(),
            }))
        }).collect::<Result<Vec<_>, InterfaceDriverError>>()?;
    if !unnamed.is_empty() {
        visible["selected_artifacts"] = serde_json::json!(unnamed);
    }
    let text =
        serde_json::to_string(&visible).map_err(|error| {
            InterfaceDriverError::Protocol(format!(
                "selected file metadata did not encode: {error}"
            ))
        })?;
    if octopus_util_string::approx_token_count(&text) > projection.validate()?.max_tokens {
        return Err(InterfaceDriverError::FailedClosed(
            "selected file metadata exceeds model_result_projection.max_tokens; no partial file identity was emitted".into(),
        ));
    }
    Ok(SelectedFileProjection {
        text,
        current_attachments,
        source_label: source_label.to_string(),
        source_payload_sha256: sha256_hex(source_bytes),
    })
}

pub(crate) fn decode_provider_input(
    bytes: &[u8],
    manifest_label: &str,
    source_interface: Option<&InterfaceId>,
    interface_operations: &BTreeMap<String, InterfaceOperationAffordance>,
    selected_files: Option<&SelectedFileProjection>,
    private_config: &serde_json::Map<String, serde_json::Value>,
) -> Result<ProviderInput, InterfaceDriverError> {
    if selected_files.is_some_and(|files| {
        files.source_label != manifest_label || files.source_payload_sha256 != sha256_hex(bytes)
    }) {
        return Err(InterfaceDriverError::FailedClosed(
            "selected file projection belongs to a different immutable input".into(),
        ));
    }
    if let Some(message) = decode_input_message(private_config, manifest_label, bytes, selected_files)? {
        return Ok(ProviderInput::Turn(prepend_context_block_to_turn_input(
            selected_files.map(|files| files.text.clone()), message.input,
        )));
    }
    if manifest_label == OPERATOR_TURN_CONTROL_LABEL {
        let control =
            serde_json::from_slice::<ExplicitTurnControlArtifact>(bytes).map_err(|error| {
                InterfaceDriverError::Protocol(format!(
                    "operator turn-control artifact is not a strict control envelope: {error}"
                ))
            })?;
        return match control {
            ExplicitTurnControlArtifact::Resume { control_id } => Ok(ProviderInput::Resume {
                control_id: validated_control_id(control_id)?,
            }),
            _ => Err(InterfaceDriverError::FailedClosed(
                "only an inactive explicit Resume may enter the provider turn path".to_string(),
            )),
        };
    }
    match serde_json::from_slice::<serde_json::Value>(bytes) {
        Ok(value) => {
            if let Some(result) = decode_tool_result_artifact(&value)? {
                return Ok(ProviderInput::ToolResult(result));
            }
            let source_is_operation_interface = interface_operations.values().any(|operation| {
                source_interface == Some(&operation.interface)
                    && !operation.input_labels.contains(manifest_label)
            });
            if source_is_operation_interface {
                let source_interface = source_interface.ok_or_else(|| {
                    InterfaceDriverError::Protocol(
                        "operation-interface input lost its source interface".to_string(),
                    )
                })?;
                let result_label = interface_operations.values().any(|operation| {
                    operation.interface == *source_interface
                        && operation.output_label == manifest_label
                });
                if result_label {
                    let result = decode_interface_operation_result_artifact(
                        &value,
                        manifest_label,
                        source_interface,
                        interface_operations,
                    )?
                    .ok_or_else(|| {
                        InterfaceDriverError::Protocol(format!(
                            "artifact '{}' from operation interface '{}' uses a result label without a strict result envelope",
                            manifest_label,
                            source_interface.as_str()
                        ))
                    })?;
                    return Ok(ProviderInput::InterfaceOperationResult(result));
                }
                return Ok(ProviderInput::InterfaceContext(InterfaceContextArtifact {
                    interface: source_interface.clone(),
                    label: manifest_label.to_string(),
                    content: value,
                }));
            }
            if let Some(response) = decode_operator_response_artifact(&value)? {
                return Ok(ProviderInput::OperatorResponse(response));
            }
            let turn_envelope = manifest_label == OPERATOR_MESSAGE_LABEL
                || value.as_object().is_some_and(|object| {
                    [SOURCE_ARTIFACT_FIELD, "text", "attachments", "task", "prompt"]
                        .iter().any(|key| object.contains_key(*key))
                });
            let input = if turn_envelope {
                extract_turn_input_from_value(&value, selected_files)?
            } else {
                let (payload, images) = match crate::result_media::project(&value)? {
                    Some(media) => (media.value, media.images),
                    None => (value, Vec::new()),
                };
                let mut input = vec![UserInput::Text {
                    text: serde_json::to_string_pretty(&serde_json::json!({
                        "artifact": {
                            "label": manifest_label,
                            "source_interface": source_interface,
                            "payload": payload,
                        }
                    })).map_err(|error| InterfaceDriverError::Protocol(
                        format!("interface input artifact failed to render: {error}")
                    ))?,
                    text_elements: Vec::new(),
                }];
                input.extend(images.into_iter().filter_map(|image| match image {
                    DynamicToolCallOutputContentItem::InputImage { image_url } => {
                        Some(UserInput::Image { detail: None, url: image_url })
                    }
                    _ => None,
                }));
                input
            };
            Ok({
                ProviderInput::Turn(prepend_context_block_to_turn_input(
                    selected_files.map(|files| files.text.clone()),
                    input,
                ))
            })
        }
        Err(_) => {
            let text = std::str::from_utf8(bytes).map_err(|error| {
                InterfaceDriverError::Protocol(format!(
                    "interface input artifact is neither JSON nor utf-8 text; this provider interface renders text input only: {error}"
                ))
            })?;
            if text.trim().is_empty() {
                return Err(InterfaceDriverError::Protocol(
                    "interface input artifact is empty; refusing to fabricate a prompt".to_string(),
                ));
            }
            Ok(ProviderInput::Turn(prepend_context_block_to_turn_input(
                selected_files.map(|files| files.text.clone()),
                vec![UserInput::Text {
                    text: text.to_string(),
                    text_elements: Vec::new(),
                }],
            )))
        }
    }
}

#[derive(serde::Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum ExplicitTurnControlArtifact {
    Interrupt { control_id: String },
    Stop { control_id: String },
    Pause { control_id: String },
    Resume { control_id: String },
}

pub(crate) fn is_explicit_turn_control_label(label: &str) -> bool {
    label == OPERATOR_TURN_CONTROL_LABEL
}

pub(crate) fn is_inflight_turn_control_candidate_label(label: &str, descriptor: &InterfaceDescriptor) -> bool {
    label == OPERATOR_MESSAGE_LABEL || is_declared_input_message(descriptor, label)
        || is_explicit_turn_control_label(label) || peer_notification_label(label)
}

pub(crate) fn peer_notification_label(label: &str) -> bool {
    label == PEER_MESSAGE_LABEL || label == INTERFACE_ARTIFACT_OUTPUT_LABEL
        || label.starts_with("turn.") || label.starts_with("provider.")
        || label.starts_with("hook.") || label.starts_with("interface.context.")
}


pub(crate) fn admitted_peer_message(input: &InterfaceInvocationEnvelope, selection: Option<&serde_json::Value>) -> Result<Option<PeerMessage>, InterfaceDriverError> {
    let metadata = input.metadata()?;
    let runtime = metadata.runtime.as_ref().ok_or_else(|| InterfaceDriverError::FailedClosed("peer message has no runtime identity".into()))?;
    let inbound = metadata.checkpoint_binding_policy.as_ref().map(|binding| &binding.inbound.interface);
    let label = input.read_artifact_manifest()?.label;
    if label == octopus_operator_chat_client::input_intake::INPUT_INTAKE_LABEL {
        let endpoint = selection.and_then(|selection| selection.get("interface_id")).and_then(serde_json::Value::as_str).filter(|id| !id.is_empty()).ok_or_else(|| InterfaceDriverError::FailedClosed("intake delivery has no selected concrete endpoint".into()))?;
        if inbound.is_none_or(|id| id.as_str() != endpoint) { return Err(InterfaceDriverError::FailedClosed("intake delivery did not arrive through its exact selected endpoint".into())); }
        let delivery: crate::intake_state::InputDelivery = serde_json::from_slice(&input.read_artifact_payload()?).map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
        delivery.validate().map_err(InterfaceDriverError::Protocol)?;
        return Ok(Some(delivery.message));
    }
    if inbound != Some(&runtime.worker) { return Ok(None); }
    let bytes = input.read_artifact_payload()?;
    if label == PEER_MESSAGE_LABEL {
        return serde_json::from_slice(&bytes).map(Some).map_err(|error| InterfaceDriverError::Protocol(format!("peer message is malformed: {error}")));
    }
    if !peer_notification_label(&label) { return Ok(None); }
    let text = peer_notification_text(&label, &bytes)?;
    Ok(Some(PeerMessage { action: PeerAction::Queue, control_id: input.artifact.id.as_str().into(), text: Some(text), artifacts: vec![input.artifact.clone()] }))
}

pub(crate) fn peer_notification_text(label: &str, bytes: &[u8]) -> Result<String, InterfaceDriverError> {
    if !matches!(label, INTERFACE_ARTIFACT_OUTPUT_LABEL | TURN_ERROR_LABEL | TURN_CONTROL_ACKNOWLEDGED_LABEL | TURN_CONTROL_FAILED_LABEL | INTERFACE_CONTEXT_UPDATED_LABEL) {
        return Ok(format!("Connected execution recorded {label}."));
    }
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
    let text = match label {
        INTERFACE_ARTIFACT_OUTPUT_LABEL => value.get("text").and_then(serde_json::Value::as_str).ok_or_else(|| InterfaceDriverError::Protocol("peer terminal artifact has no text".into()))?.to_string(),
        TURN_ERROR_LABEL | TURN_CONTROL_ACKNOWLEDGED_LABEL | TURN_CONTROL_FAILED_LABEL | INTERFACE_CONTEXT_UPDATED_LABEL => {
            let mut visible = value.as_object().ok_or_else(|| InterfaceDriverError::Protocol("peer outcome must be an object".into()))?.clone();
            visible.remove(CODEX_CONTINUITY_FIELD);
            visible.remove(SESSION_CONTEXT_FIELD);
            serde_json::to_string(&visible).map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?
        }
        _ => format!("Connected execution recorded {label}."),
    };
    Ok(text)
}

pub(crate) fn peer_input_text(message: &PeerMessage, artifact: &ArtifactRef) -> Result<String, InterfaceDriverError> {
    serde_json::to_string(&serde_json::json!({
        "received_system_artifact": artifact, "text": message.text, "artifact_references": message.artifacts,
    })).map_err(|error| InterfaceDriverError::Protocol(error.to_string()))
}

pub(crate) fn queued_peer_item(text: String) -> serde_json::Value {
    serde_json::json!({"type":"message", "role":"user", "content":[{"type":"input_text", "text":text}]})
}

pub(crate) fn is_admitted_peer_interrupt(input: &InterfaceInvocationEnvelope, descriptor: &InterfaceDescriptor) -> bool {
    matches!(admitted_peer_message(input, descriptor.interface_driver_config.as_ref().and_then(|c| c.private_config.get("input_intake"))), Ok(Some(PeerMessage { action: PeerAction::Interrupt, .. })))
}

pub(crate) fn decode_inflight_turn_control(
    descriptor: &InterfaceDescriptor,
    input: &InterfaceInvocationEnvelope,
    pinned_run_record: Option<&[u8]>,
) -> Result<Option<CodexTurnControl>, InterfaceDriverError> {
    let manifest = input.artifact_file.read_manifest().map_err(|error| {
        InterfaceDriverError::Protocol(format!(
            "turn-control source artifact manifest cannot be read: {error}"
        ))
    })?;
    let bytes = input.read_artifact_payload()?;
    let input_message = manifest.label == OPERATOR_MESSAGE_LABEL || is_declared_input_message(descriptor, &manifest.label);
    if !input_message && let Some(peer) = admitted_peer_message(input, descriptor.interface_driver_config.as_ref().and_then(|c| c.private_config.get("input_intake")))? {
        return Ok(Some(match peer.action {
            PeerAction::Queue => CodexTurnControl::Inject {
                control_id: peer.control_id.clone(), items: vec![queued_peer_item(peer_input_text(&peer, &input.artifact)?)],
            },
            PeerAction::Followup => CodexTurnControl::Steer {
                control_id: peer.control_id.clone(), input: vec![UserInput::Text { text: peer_input_text(&peer, &input.artifact)?, text_elements: Vec::new() }],
                source_artifact: None,
            },
            PeerAction::Interrupt => CodexTurnControl::Interrupt { control_id: peer.control_id, action: "interrupt".into() },
        }));
    }
    if input_message {
        let pinned_run_record = pinned_run_record.ok_or_else(|| InterfaceDriverError::FailedClosed(
            "operator steering has no source-pinned run record".into(),
        ))?;
        let runtime = invocation_runtime_metadata(input)?;
        let record = octopus_run_record::RunRecord::from_bytes(pinned_run_record)
            .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
        if runtime.interface != descriptor.id || runtime.run != record.id
            || runtime.ee != record.ee || runtime.worker != record.worker
            || runtime.run_record_hash.as_deref() != Some(octopus_ledger::ContentHash::of_bytes(pinned_run_record).to_hex().as_str())
            || !record.active_checkpoints.contains(&runtime.checkpoint)
            || !record.checkpoint(&runtime.checkpoint).is_some_and(|checkpoint| checkpoint.epoch.0 == runtime.epoch)
        {
            return Err(InterfaceDriverError::FailedClosed(
                "operator steering differs from its exact source-pinned run/checkpoint".into(),
            ));
        }
        let config = descriptor.interface_driver_config.as_ref().ok_or_else(|| InterfaceDriverError::FailedClosed(
            "operator steering provider declares no interface_driver_config".into(),
        ))?;
        let context_payloads = octopus_run_authoring::context_artifact_payloads(
            &record, input.metadata()?, &input.artifact_binding_payloads,
        ).map_err(InterfaceDriverError::FailedClosed)?;
        let context_payloads = resolve_instruction_context(input.metadata()?, &context_payloads)
            .map_err(InterfaceDriverError::FailedClosed)?
            .remaining_payloads;
        for bound in &context_payloads {
            let bytes = bound.read_bytes()
                .map_err(|error| InterfaceDriverError::FailedClosed(error.to_string()))?;
            serde_json::from_slice::<serde_json::Value>(bytes.as_ref()).map_err(|error| {
                InterfaceDriverError::Protocol(format!("operator steering context '{}' is not JSON: {error}", bound.artifact.id))
            })?;
        }
        let selected_files = selected_file_projection(
            &record, input, &input.artifact, &manifest.label, &bytes, &BTreeMap::new(), &[],
            ModelResultProjection::from_private_config(&config.private_config)?,
        )?;
        let declared = decode_input_message(&config.private_config, &manifest.label, &bytes, Some(&selected_files))?;
        let control_id = match &declared {
            Some(message) => message.control_id.clone(),
            None => operator_message_control_id(&bytes),
        };
        if manifest.label == OPERATOR_MESSAGE_LABEL && serde_json::from_slice::<serde_json::Value>(&bytes)
            .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?.get("replaces_message").is_some() {
            return Ok(Some(CodexTurnControl::Unsupported {
                control_id: control_id.unwrap_or_else(|| input.artifact.id.to_string()),
                action: "edit_message".into(),
            }));
        }
        let provider_input = match declared {
            Some(message) => ProviderInput::Turn(prepend_context_block_to_turn_input(Some(selected_files.text.clone()), message.input)),
            None => decode_provider_input(
                &bytes, &manifest.label, None, &BTreeMap::new(), Some(&selected_files), &config.private_config,
            )?,
        };
        let ProviderInput::Turn(turn_input) = provider_input else {
            return Ok(None);
        };
        return Ok(Some(CodexTurnControl::Steer {
            control_id: control_id.unwrap_or_else(|| input.artifact.id.as_str().to_string()),
            input: turn_input,
            source_artifact: Some(input.artifact.clone()),
        }));
    }
    if manifest.label != OPERATOR_TURN_CONTROL_LABEL {
        return Ok(None);
    }
    let control =
        serde_json::from_slice::<ExplicitTurnControlArtifact>(&bytes).map_err(|error| {
            InterfaceDriverError::Protocol(format!(
                "operator turn-control artifact is not a strict control envelope: {error}"
            ))
        })?;
    let control = match control {
        ExplicitTurnControlArtifact::Interrupt { control_id } => CodexTurnControl::Interrupt {
            control_id: validated_control_id(control_id)?,
            action: "interrupt".to_string(),
        },
        ExplicitTurnControlArtifact::Stop { control_id } => CodexTurnControl::Interrupt {
            control_id: validated_control_id(control_id)?,
            action: "stop".to_string(),
        },
        ExplicitTurnControlArtifact::Pause { control_id } => CodexTurnControl::Unsupported {
            control_id: validated_control_id(control_id)?,
            action: "pause".to_string(),
        },
        ExplicitTurnControlArtifact::Resume { control_id } => CodexTurnControl::Unsupported {
            control_id: validated_control_id(control_id)?,
            action: "resume".to_string(),
        },
    };
    Ok(Some(control))
}

pub(crate) fn operator_message_control_id(bytes: &[u8]) -> Option<String> {
    let value = serde_json::from_slice::<serde_json::Value>(bytes).ok()?;
    value
        .get("message_id")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty() && value.trim() == *value)
        .map(str::to_string)
}

pub(crate) fn validated_control_id(control_id: String) -> Result<String, InterfaceDriverError> {
    if control_id.trim().is_empty() || control_id.trim() != control_id || control_id.len() > 256 {
        return Err(InterfaceDriverError::Protocol(
            "operator turn-control artifact has an empty, untrimmed, or oversized control_id"
                .to_string(),
        ));
    }
    Ok(control_id)
}
