use super::*;

pub(crate) fn materialize_independent_input_receipt(
    output_root: &Path,
    source_movement_seq: u64,
    source_label: &str,
    source_artifact: &ArtifactRef,
) -> Result<InterfaceArtifactFile, InterfaceDriverError> {
    let value = serde_json::json!({
        "label": TURN_INPUT_ACKNOWLEDGED_LABEL,
        "action": "receive",
        "status": "acknowledged",
        "source_movement_seq": source_movement_seq,
        "source_label": source_label,
        "source_artifact": source_artifact.id.as_str(),
    });
    let bytes = serde_json::to_vec(&value)
        .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
    InterfaceArtifactFile::write(output_root, TURN_INPUT_ACKNOWLEDGED_LABEL, &bytes)
        .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))
}

pub(crate) fn materialize_turn_control_result(
    output_root: &Path,
    source_movement_seq: u64,
    result: CodexTurnControlResult,
) -> Result<InterfaceArtifactFile, InterfaceDriverError> {
    if result.continuity.is_some()
        && (!result.acknowledged || !matches!(result.action.as_str(), "interrupt" | "stop" | "steer"))
    {
        return Err(InterfaceDriverError::FailedClosed(
            "only acknowledged steering or interruption may author a control continuity revision".into(),
        ));
    }
    if result.action == "steer" && result.continuity.as_ref().is_some_and(|state| state.turn_input.is_none()) {
        return Err(InterfaceDriverError::FailedClosed("steering continuity lost its exact input artifact".into()));
    }
    let label = if result.acknowledged && result.continuity.is_none()
        && matches!(result.action.as_str(), "queue_message" | "steer") {
        TURN_INPUT_ACKNOWLEDGED_LABEL
    } else if result.acknowledged {
        TURN_CONTROL_ACKNOWLEDGED_LABEL
    } else {
        TURN_CONTROL_FAILED_LABEL
    };
    let mut value = serde_json::json!({
        "label": label,
        "control_id": result.control_id,
        "action": result.action,
        "status": if result.acknowledged { "acknowledged" } else { "failed" },
        "source_movement_seq": source_movement_seq,
        "thread_id": result.thread_id,
        "turn_id": result.turn_id,
        "detail": result.detail,
    });
    if let Some(continuity) = result.continuity {
        value
            .as_object_mut()
            .expect("turn-control result is an object")
            .insert(
                CODEX_CONTINUITY_FIELD.to_string(),
                serde_json::to_value(continuity).map_err(|error| {
                    InterfaceDriverError::Protocol(format!(
                        "turn-control continuity could not be encoded: {error}"
                    ))
                })?,
            );
    }
    let bytes = serde_json::to_vec(&value).map_err(|error| {
        InterfaceDriverError::Protocol(format!("turn-control result could not be encoded: {error}"))
    })?;
    materialize_codex_emitted_artifact(output_root, label, &bytes)
}

pub(crate) fn validate_durable_interrupted_continuity(
    state: &CodexContinuityState,
    provider_interface: &InterfaceId,
    run: &str,
) -> Result<(), String> {
    let value = serde_json::to_value(state)
        .map_err(|error| format!("durable interrupt continuity could not be encoded: {error}"))?;
    let parsed = parse_codex_continuity(&value, provider_interface, run)
        .map_err(|error| error.to_string())?;
    if &parsed != state
        || state.pending.is_some()
        || state
            .interrupted_pending
            .as_ref()
            .is_none_or(|pending| !interrupted_pending_has_external_result(pending))
    {
        return Err(
            "durable interrupt continuity does not retain exactly one external effect".to_string(),
        );
    }
    Ok(())
}

pub(crate) fn interrupted_turn_identity(state: &CodexContinuityState) -> Result<LiveTurnIdentity, String> {
    validate_durable_interrupted_continuity(state, &state.provider_interface, &state.run)?;
    Ok(LiveTurnIdentity {
        provider_interface: state.provider_interface.to_string(), run: state.run.clone(),
        thread_id: state.thread_id.clone(),
        turn_id: pending_turn_id(state.interrupted_pending.as_ref().ok_or("interrupted turn has no exact effect")?).to_string(),
    })
}

pub(crate) fn result_turn_identity(state: &CodexContinuityState) -> Result<LiveTurnIdentity, String> {
    if state.interrupted_pending.is_some() {
        return interrupted_turn_identity(state);
    }
    let pending = state.pending.as_ref().filter(|pending| interrupted_pending_has_external_result(pending))
        .ok_or("result stream recovery has no exact ordinary external pending")?;
    parse_codex_continuity(&serde_json::to_value(state).map_err(|error| error.to_string())?, &state.provider_interface, &state.run)
        .map_err(|error| error.to_string())?;
    Ok(LiveTurnIdentity {
        provider_interface: state.provider_interface.to_string(), run: state.run.clone(),
        thread_id: state.thread_id.clone(), turn_id: pending_turn_id(pending).to_string(),
    })
}

pub(crate) fn exact_interrupted_result_source(
    envelope: &InterfaceInvocationEnvelope,
    continuity: &CodexContinuityState,
) -> Result<bool, String> {
    if continuity.interrupted_pending.is_none() {
        return Ok(false);
    }
    let manifest = envelope
        .artifact_file
        .read_manifest()
        .map_err(|error| format!("interrupted result manifest cannot be read: {error}"))?;
    let bytes = envelope
        .artifact_file
        .read_payload_bytes()
        .map_err(|error| format!("interrupted result payload cannot be read: {error}"))?;
    exact_interrupted_result_payload(&manifest.label, &bytes, continuity)
}

pub(crate) fn exact_interrupted_result_payload(
    label: &str,
    bytes: &[u8],
    continuity: &CodexContinuityState,
) -> Result<bool, String> {
    let Some(interrupted) = continuity.interrupted_pending.as_ref() else {
        return Ok(false);
    };
    let value = match serde_json::from_slice::<serde_json::Value>(bytes) {
        Ok(value) => value,
        Err(_) => return Ok(false),
    };
    match interrupted {
        CodexPendingContinuity::DynamicTool { .. } => {
            let Some(result) =
                decode_tool_result_artifact(&value).map_err(|error| error.to_string())?
            else {
                return Ok(false);
            };
            validate_recovered_tool_result(&result, interrupted)
                .map_err(|error| error.to_string())?;
            Ok(true)
        }
        CodexPendingContinuity::InterfaceOperation {
            operation,
            interface,
            output_label,
            ..
        } => {
            if label != output_label {
                return Ok(false);
            }
            let operations = BTreeMap::from([(
                operation.clone(),
                InterfaceOperationAffordance {
                    name: operation.clone(),
                    interface: interface.clone(),
                    request_label: String::new(),
                    output_label: output_label.clone(),
                    input_labels: BTreeSet::new(),
                    operator_approval: false,
                },
            )]);
            let result = decode_interface_operation_result_artifact(
                &value,
                label,
                interface,
                &operations,
            )
            .map_err(|error| error.to_string())?
            .ok_or_else(|| {
                "interrupted interface-operation result has no strict result envelope".to_string()
            })?;
            validate_recovered_interface_operation_input(&result, interrupted)
                .map_err(|error| error.to_string())?;
            Ok(true)
        }
        CodexPendingContinuity::Operator { .. } => Err(
            "durable interrupted result source cannot correlate an operator request".to_string(),
        ),
    }
}

pub(crate) fn exact_interrupted_control_continuity(
    result: &CodexTurnControlResult,
) -> Result<Option<CodexContinuityState>, String> {
    if !result.acknowledged || !matches!(result.action.as_str(), "interrupt" | "stop") {
        return Ok(None);
    }
    let Some(continuity) = result.continuity.as_ref() else {
        return Ok(None);
    };
    if continuity.interrupted_pending.is_none() {
        if continuity.pending.is_some() {
            return Err(
                "acknowledged interrupt continuity retained an active pending request".to_string(),
            );
        }
        return Ok(None);
    }
    validate_durable_interrupted_continuity(
        continuity,
        &continuity.provider_interface,
        continuity.run.as_str(),
    )?;
    Ok(Some(continuity.clone()))
}

pub(crate) fn codex_continuity_retains_interrupted_effect(
    original: &CodexContinuityState, next: &CodexContinuityState,
) -> bool {
    if original.pending.is_some() || original.interrupted_pending.is_none()
        || next.context_generation < original.context_generation { return false; }
    let mut comparable = next.clone();
    comparable.context_generation = original.context_generation;
    &comparable == original
}

pub(crate) fn checked_interrupted_context_revision(
    original: &CodexContinuityState, context: &[SessionContextArtifact],
    labels: &BTreeSet<String>, before: Option<u64>,
) -> Result<CodexContinuityState, String> {
    let mut selected = original.clone();
    for entry in context {
        if !labels.contains(&entry.label) { continue; }
        let Some(raw) = entry.payload.get(CODEX_CONTINUITY_FIELD) else { continue; };
        if is_foreign_provider_payload(&entry.payload, &original.provider_interface).map_err(|error| error.to_string())? { continue; }
        let candidate = parse_codex_continuity(raw, &original.provider_interface, &original.run).map_err(|error| error.to_string())?;
        if !codex_continuity_retains_interrupted_effect(original, &candidate) { continue; }
        let sequence = entry.source_movement_seq.ok_or("interrupted context revision has no checked source movement")?;
        if before.is_some_and(|limit| sequence >= limit) { continue; }
        if candidate.context_generation > selected.context_generation { selected = candidate; }
    }
    Ok(selected)
}

pub(crate) fn interrupted_result_closure_ref(
    artifact: &InterfaceArtifactFile,
    interrupted: &CodexContinuityState,
    source_artifact: &ArtifactRef,
    context_output_labels: &BTreeSet<String>,
) -> Result<Option<ArtifactRef>, String> {
    let manifest = artifact
        .read_manifest()
        .map_err(|error| format!("provider closure manifest is malformed: {error}"))?;
    if manifest.label != TURN_INTERRUPTED_RESULT_LABEL {
        return Ok(None);
    }
    let bytes = artifact.read_payload_bytes()
        .map_err(|error| format!("provider closure payload cannot be read: {error}"))?;
    if interrupted_result_closure_payload(&manifest.label, &bytes, interrupted, source_artifact, context_output_labels)?.is_none() {
        return Ok(None);
    }
    let manifest_bytes = std::fs::read(artifact.manifest_path()).map_err(|error| {
        format!(
            "provider closure manifest '{}' cannot be read: {error}",
            artifact.manifest_path().display()
        )
    })?;
    let content = octopus_ledger::ContentHash::of_bytes(&manifest_bytes);
    Ok(Some(ArtifactRef {
        id: octopus_descriptors::ArtifactId::new(format!(
            "{}:{}",
            manifest.label,
            content.to_hex()
        )),
        content,
    }))
}

pub(crate) fn native_terminal_events(payload: &serde_json::Value) -> Result<&[serde_json::Value], InterfaceDriverError> {
    match payload.get("native_terminal_events") {
        None => Ok(&[]),
        Some(serde_json::Value::Array(events)) => Ok(events),
        Some(_) => Err(InterfaceDriverError::Protocol("native terminal events must be an array".into())),
    }
}

pub(crate) fn retained_native_output_payloads(payload: &serde_json::Value, context_output_labels: &BTreeSet<String>) -> Result<Vec<serde_json::Value>, String> {
    let state: CodexContinuityState = serde_json::from_value(payload.get(CODEX_CONTINUITY_FIELD)
        .ok_or("native result closure has no continuity")?.clone()).map_err(|error| error.to_string())?;
    let Some(terminal) = &state.native_terminal else {
        if payload.get("retained_native_outputs").is_some() || payload.get("native_terminal_events").is_some() {
            return Err("native outputs have no exact terminal evidence".into());
        }
        return Ok(Vec::new());
    };
    let events = native_terminal_events(payload).map_err(|error| error.to_string())?;
    let turn = terminal.exact_turn(&state.thread_id, payload.get("turn_id").and_then(serde_json::Value::as_str))
        .map_err(|error| error.to_string())?;
    if events.iter().any(|frame| !native_frame_matches_turn(
        frame.get("quarantined_server_request").unwrap_or(frame), &state.thread_id, &turn.id,
    )) {
        return Err("retained native events cross the exact terminal turn".into());
    }
    let expected = terminal.outputs(events).map_err(|error| error.to_string())?;
    let retained = payload.get("retained_native_outputs").and_then(serde_json::Value::as_array)
        .ok_or("native terminal closure lost its retained provider outputs")?;
    if retained.len() != expected.len() {
        return Err("native terminal output count differs from its typed snapshot".into());
    }
    let delta = u64::from(context_output_labels.contains(TURN_INTERRUPTED_RESULT_LABEL))
        + expected.iter().filter(|output| context_output_labels.contains(&output.label)).count() as u64;
    let mut generation = state.context_generation.checked_sub(delta)
        .ok_or("native terminal outputs precede their declared context generations")?;
    for (actual, mut expected) in retained.iter().zip(expected) {
        generation = generation.checked_add(u64::from(context_output_labels.contains(&expected.label)))
            .ok_or("native terminal output generation overflow")?;
        let mut observed = actual.clone();
        let continuation = observed.as_object_mut().ok_or("retained native output is not an object")?
            .remove(CODEX_CONTINUITY_FIELD).ok_or("retained native output has no continuity")?;
        let observed_state = parse_codex_continuity(&continuation, &state.provider_interface, &state.run)
            .map_err(|error| error.to_string())?;
        let mut expected_state = state.clone();
        expected_state.context_generation = generation;
        if observed_state != expected_state {
            return Err("retained native output conflicts with settled terminal continuity".into());
        }
        normalize_output_identity(&mut expected, state.turn_input.as_ref())
            .map_err(|error| error.to_string())?;
        if observed != expected.value {
            return Err("retained native output differs from its exact typed terminal snapshot".into());
        }
    }
    Ok(retained.clone())
}

pub(crate) fn native_terminal_result_closure_ref(
    closure: &InterfaceArtifactFile,
    source: &CodexContinuityState,
    input: &InterfaceInvocationEnvelope,
    context_output_labels: &BTreeSet<String>,
) -> Result<Option<ArtifactRef>, String> {
    if source.interrupted_pending.is_some() || source.pending.as_ref().is_none_or(|pending| !interrupted_pending_has_external_result(pending)) {
        return Err("control-free closure has no original ordinary external pending source".into());
    }
    let mut exact = source.clone(); exact.interrupted_pending = exact.pending.take();
    if !exact_interrupted_result_source(input, &exact)? {
        return Err("control-free closure changed its original correlated result source".into());
    }
    let input_payload: serde_json::Value = serde_json::from_slice(&input.read_artifact_payload().map_err(|error| error.to_string())?).map_err(|error| error.to_string())?;
    let closed_payload: serde_json::Value = serde_json::from_slice(&closure.read_payload_bytes().map_err(|error| error.to_string())?).map_err(|error| error.to_string())?;
    if input_payload.get("ok").and_then(serde_json::Value::as_bool).is_none()
        || closed_payload.get("ok") != input_payload.get("ok") {
        return Err("control-free closure changed the admitted result verdict".into());
    }
    interrupted_result_closure_ref(closure, source, &input.artifact, context_output_labels)
}

pub(crate) fn recover_retained_native_outputs(
    closure: &InterfaceArtifactFile,
    output_root: &Path,
    context_output_labels: &BTreeSet<String>,
) -> Result<Vec<ProviderEmission>, String> {
    let payload: serde_json::Value = serde_json::from_slice(&closure.read_payload_bytes().map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())?;
    retained_native_output_payloads(&payload, context_output_labels)?.into_iter().map(|value| {
        let label = value.get("label").and_then(serde_json::Value::as_str).ok_or("retained native output has no label")?;
        let artifact = materialize_codex_emitted_artifact(output_root, label, &serde_json::to_vec(&value).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
        Ok(ProviderEmission { artifact, route: ProviderEmissionRoute::ClientProjection })
    }).collect()
}

pub(crate) fn interrupted_result_closure_payload(
    label: &str,
    bytes: &[u8],
    interrupted: &CodexContinuityState,
    source_artifact: &ArtifactRef,
    context_output_labels: &BTreeSet<String>,
) -> Result<Option<CodexContinuityState>, String> {
    if label != TURN_INTERRUPTED_RESULT_LABEL {
        return Ok(None);
    }
    let payload: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|error| format!("provider closure payload is malformed: {error}"))?;
    let native_source;
    let ordinary = interrupted.pending.is_some() && interrupted.interrupted_pending.is_none();
    let enriched = interrupted.native_terminal.is_none()
        && payload.get("result_origin").and_then(serde_json::Value::as_str) == Some("interrupted_turn_completion");
    let interrupted = if ordinary || enriched {
        if payload.get("native_terminal_source_continuity") != Some(&serde_json::to_value(interrupted).map_err(|error| error.to_string())?)
            || payload.get("result_origin").and_then(serde_json::Value::as_str) != Some(if ordinary { "native_turn_completion" } else { "interrupted_turn_completion" })
            || payload.get("terminal_evidence_source").and_then(serde_json::Value::as_str) != Some("thread/read")
        {
            return Err("native result closure changed its original source continuity".into());
        }
        let settled = parse_codex_continuity(payload.get(CODEX_CONTINUITY_FIELD).ok_or("native result has no settled continuity")?, &interrupted.provider_interface, &interrupted.run)
            .map_err(|error| error.to_string())?;
        let terminal = settled.native_terminal.ok_or("native result closure has no exact terminal proof")?;
        let pending = if ordinary { interrupted.pending.as_ref() } else { interrupted.interrupted_pending.as_ref() }
            .filter(|pending| interrupted_pending_has_external_result(pending)).ok_or("native result closure has no exact external pending effect")?;
        terminal.exact_turn(&interrupted.thread_id, Some(pending_turn_id(pending))).map_err(|error| error.to_string())?;
        let mut retired = interrupted.clone();
        if ordinary { retired.interrupted_pending = retired.pending.take(); }
        retired.native_terminal = Some(terminal);
        native_source = retired;
        &native_source
    } else { interrupted };
    let pending = interrupted.interrupted_pending.as_ref().ok_or_else(|| {
        "provider closure arbitration lost its interrupted exact effect".to_string()
    })?;
    let (result_kind, correlation_id, turn_id) = match pending {
        CodexPendingContinuity::DynamicTool {
            turn_id, call_id, ..
        } => ("tool_result", call_id, turn_id),
        CodexPendingContinuity::InterfaceOperation {
            turn_id, call_id, ..
        } => ("interface_operation_result", call_id, turn_id),
        CodexPendingContinuity::Operator { .. } => {
            return Err("provider closure cannot arbitrate an operator request".to_string());
        }
    };
    let native_status = interrupted.native_terminal.as_ref().map(|terminal| terminal.exact_turn(
        &interrupted.thread_id, Some(turn_id),
    )).transpose().map_err(|error| error.to_string())?.map(|turn| turn.status);
    let native_interrupted = native_status.as_ref().map_or(true, |status| *status == TurnStatus::Interrupted);
    let disposition = if native_status.is_some() { "durably_admitted_after_native_terminal" } else { "durably_admitted_after_interrupt" };
    if payload.get("label").and_then(serde_json::Value::as_str)
        != Some(TURN_INTERRUPTED_RESULT_LABEL)
        || payload
            .get("result_kind")
            .and_then(serde_json::Value::as_str)
            != Some(result_kind)
        || payload
            .get("correlation_id")
            .and_then(serde_json::Value::as_str)
            != Some(correlation_id)
        || payload.get("thread_id").and_then(serde_json::Value::as_str)
            != Some(interrupted.thread_id.as_str())
        || payload.get("turn_id").and_then(serde_json::Value::as_str) != Some(turn_id.as_str())
        || payload
            .get("ok")
            .and_then(serde_json::Value::as_bool)
            .is_none()
        || payload
            .get("turn_interrupted")
            .and_then(serde_json::Value::as_bool)
            != Some(native_interrupted)
        || payload
            .get("returned_to_model")
            .and_then(serde_json::Value::as_bool)
            != Some(false)
        || payload
            .get("result_disposition")
            .and_then(serde_json::Value::as_str)
            != Some(disposition)
        || payload.get("source_artifact")
            != Some(
                &serde_json::to_value(source_artifact)
                    .map_err(|error| format!("source artifact could not be encoded: {error}"))?,
            )
    {
        return Err(
            "provider interrupted-result closure conflicts with durable arbitration".to_string(),
        );
    }
    let closure = payload.get(CODEX_CONTINUITY_FIELD).ok_or_else(|| {
        "provider interrupted-result closure has no continuity evidence".to_string()
    })?;
    let closure = parse_codex_continuity(
        closure,
        &interrupted.provider_interface,
        interrupted.run.as_str(),
    )
    .map_err(|error| error.to_string())?;
    if enriched {
        let mut expected = interrupted.clone();
        expected.interrupted_pending = None;
        expected.context_generation = closure.context_generation;
        if closure != expected { return Err("native terminal enrichment changed the interrupted source contract".into()); }
    }
    let retained = retained_native_output_payloads(&payload, context_output_labels)?;
    let minimum_generation = interrupted
        .context_generation
        .checked_add(1)
        .ok_or_else(|| "provider interrupted-result closure generation overflowed".to_string())?;
    let maximum_generation = minimum_generation
        .checked_add(1 + retained.len() as u64)
        .ok_or_else(|| "provider interrupted-result closure generation overflowed".to_string())?;
    let native_generation = if native_status.is_some() {
        let input_context = payload.get("native_terminal_input_context").and_then(serde_json::Value::as_bool)
            .ok_or("native terminal closure lost its exact input context consumption")?;
        let outputs = retained.iter().filter(|output| output.get("label").and_then(serde_json::Value::as_str)
            .is_some_and(|label| context_output_labels.contains(label))).count() as u64;
        Some(interrupted.context_generation.checked_add(u64::from(input_context))
            .and_then(|generation| generation.checked_add(outputs))
            .and_then(|generation| generation.checked_add(u64::from(context_output_labels.contains(TURN_INTERRUPTED_RESULT_LABEL))))
            .ok_or("native terminal closure generation overflow")?)
    } else { None };
    if closure.thread_id != interrupted.thread_id
        || closure.thread_shape_fingerprint != interrupted.thread_shape_fingerprint
        || closure.static_context_fingerprint != interrupted.static_context_fingerprint
        || closure.pending.is_some()
        || closure.interrupted_pending.is_some()
        || closure.native_terminal != interrupted.native_terminal
        || native_status.as_ref().is_some_and(|status| payload.get("native_terminal_status") != serde_json::to_value(status).ok().as_ref())
        || if let Some(generation) = native_generation { closure.context_generation != generation }
            else { !(minimum_generation..=maximum_generation).contains(&closure.context_generation) }
    {
        return Err(
            "provider interrupted-result closure does not close the durable continuity generation"
                .to_string(),
        );
    }
    Ok(Some(closure))
}
