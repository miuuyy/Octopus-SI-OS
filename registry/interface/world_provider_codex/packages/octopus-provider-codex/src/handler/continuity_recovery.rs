use super::*;

pub(crate) fn recover_provider_turn_continuity(
    descriptor: &InterfaceDescriptor,
    input: &InterfaceInvocationEnvelope,
    resolved_session_snapshot: Option<&ProviderInvocationSessionSnapshot>,
    expected_run: &octopus_descriptors::RunId,
    pinned_run_record: &[u8],
) -> Result<Option<CodexContinuityState>, InterfaceDriverError> {
    let manifest = input.read_artifact_manifest()?;
    let peer = admitted_peer_message(input, descriptor.interface_driver_config.as_ref().and_then(|c| c.private_config.get("input_intake")))?;
    if !is_provider_turn_input(descriptor, input)? {
        return Err(InterfaceDriverError::FailedClosed(format!(
            "deferred provider turn source '{}' is not a new provider turn",
            manifest.label
        )));
    }
    let bytes = input.read_artifact_payload()?;
    let runtime = invocation_runtime_metadata(input)?;
    if &runtime.run != expected_run || runtime.interface != descriptor.id {
        return Err(InterfaceDriverError::FailedClosed(
            "deferred operator message runtime coordinates differ from its provider run"
                .to_string(),
        ));
    }
    let config = descriptor.interface_driver_config.as_ref().ok_or_else(|| {
        InterfaceDriverError::FailedClosed(format!(
            "provider interface '{}' has no interface_driver_config",
            descriptor.id.as_str()
        ))
    })?;
    let context_output_labels =
        private_config_string_set(&config.private_config, "context_output_labels")?;
    let session_snapshot_labels =
        private_config_string_set(&config.private_config, "session_snapshot_labels")?;
    let record = octopus_run_record::RunRecord::from_bytes(pinned_run_record)
        .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
    let context_payloads = octopus_run_authoring::context_artifact_payloads(
        &record,
        input.metadata()?,
        &input.artifact_binding_payloads,
    )
    .map_err(InterfaceDriverError::FailedClosed)?;
    let context_payloads = resolve_instruction_context(input.metadata()?, &context_payloads)
        .map_err(InterfaceDriverError::FailedClosed)?
        .remaining_payloads;
    let contexts = CodexAppServerInterfaceDriver::resolve_bound_session_contexts(
        &context_payloads,
        &runtime.interface,
        &session_snapshot_labels,
        resolved_session_snapshot,
    )?;
    let selected_files =
        if octopus_run_authoring::payload_artifact_bindings(&record, &runtime.interface)
            .map_err(InterfaceDriverError::FailedClosed)?
            .is_empty()
        {
            None
        } else {
            Some(selected_file_projection(
                &record,
                input,
                &input.artifact,
                &manifest.label,
                &bytes,
                &contexts,
                &[],
                ModelResultProjection::from_private_config(&config.private_config)?,
            )?)
        };
    if peer.is_none() && !matches!(
        decode_provider_input(
            &bytes,
            &manifest.label,
            None,
            &BTreeMap::new(),
            selected_files.as_ref(),
            &config.private_config,
        )?,
        ProviderInput::Turn(_)
    ) {
        return Err(InterfaceDriverError::FailedClosed(
            "deferred operator message is not an ordinary provider turn".into(),
        ));
    }
    recover_codex_continuity_resolved(
        &context_payloads,
        &contexts,
        &runtime.interface,
        runtime.run.as_str(),
        &context_output_labels,
        &session_snapshot_labels,
        std::slice::from_ref(&input.artifact),
    )
}

pub(crate) fn recover_provider_result_continuity(
    descriptor: &InterfaceDescriptor,
    input: &InterfaceInvocationEnvelope,
    snapshot: Option<&ProviderInvocationSessionSnapshot>,
    current_session: &[SessionContextArtifact],
    run: &octopus_descriptors::RunId,
    pinned: &[u8],
) -> Result<Option<CodexContinuityState>, InterfaceDriverError> {
    let payload: serde_json::Value = serde_json::from_slice(&input.read_artifact_payload()?)
        .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
    if payload.get("correlation_id").is_none() { return Ok(None); }
    let runtime = invocation_runtime_metadata(input)?;
    if runtime.run != *run || runtime.interface != descriptor.id {
        return Err(InterfaceDriverError::FailedClosed("result continuity crossed its admitted provider/run source".into()));
    }
    let config = &descriptor.interface_driver_config.as_ref().ok_or_else(|| InterfaceDriverError::FailedClosed("result continuity has no provider config".into()))?.private_config;
    let labels = private_config_string_set(config, "context_output_labels")?;
    let snapshots = private_config_string_set(config, "session_snapshot_labels")?;
    let record = octopus_run_record::RunRecord::from_bytes(pinned)
        .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
    let payloads = octopus_run_authoring::context_artifact_payloads(&record, input.metadata()?, &input.artifact_binding_payloads)
        .map_err(InterfaceDriverError::FailedClosed)?;
    let payloads = resolve_instruction_context(input.metadata()?, &payloads)
        .map_err(InterfaceDriverError::FailedClosed)?.remaining_payloads;
    let contexts = CodexAppServerInterfaceDriver::resolve_bound_session_contexts(&payloads, &runtime.interface, &snapshots, snapshot)?;
    let sources = continuity_input_sources(&record, input)?;
    let continuity_payloads = continuity_context_payloads(&record, input, &payloads)?;
    let bound = recover_codex_continuity_resolved(&continuity_payloads, &contexts, &runtime.interface, run.as_str(), &labels, &snapshots, &sources)?;
    let current = recover_codex_continuity_from_current_session(current_session, &sources, &runtime.interface, run.as_str(), &labels)?;
    let Some(source) = reconcile_input_session_continuity(&record, input, bound, current, current_session)? else { return Ok(None); };
    if source.interrupted_pending.is_some() || source.pending.as_ref().is_none_or(|pending| !interrupted_pending_has_external_result(pending)) {
        return Ok(None);
    }
    let mut exact = source.clone();
    exact.interrupted_pending = exact.pending.take();
    if !exact_interrupted_result_source(input, &exact).map_err(InterfaceDriverError::FailedClosed)? {
        return Ok(None);
    }
    Ok(Some(source))
}


pub(crate) fn completed_source_pending_continuity(
    descriptor: &InterfaceDescriptor, input: &InterfaceInvocationEnvelope,
    current: &[u8], session: &[SessionContextArtifact],
) -> Result<Option<CodexContinuityState>, InterfaceDriverError> {
    let runtime = invocation_runtime_metadata(input)?;
    let labels = codex_context_output_labels(descriptor)?;
    let state = recover_codex_continuity_from_current_session(session, std::slice::from_ref(&input.artifact), &runtime.interface, runtime.run.as_str(), &labels)?;
    let Some(state) = state.filter(|state| state.pending.as_ref().is_some_and(interrupted_pending_has_external_result)) else { return Ok(None); };
    let record: serde_json::Value = serde_json::from_slice(current).map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
    if record.get("id").and_then(serde_json::Value::as_str) != Some(runtime.run.as_str())
        || state.context_generation != read_codex_context_generation(&record, &runtime.interface)?
    { return Err(InterfaceDriverError::FailedClosed("completed source retained a pending effect without exact checked context coordinates".into())); }
    Ok(Some(state))
}

impl CodexAppServerInterfaceDriver {
    pub(crate) async fn restore_pending_control(&self, control: &CodexTurnControlBroker, continuity: CodexContinuityState) {
        control.publish(CodexTurnControlTarget::RecoveredPending {
            handle: self.client.lock().await.request_handle(), request_ids: self.next_request_id.clone(),
            continuity, effect_gate: Arc::new(tokio::sync::Mutex::new(())),
        });
    }
}

pub(crate) fn recovered_tool_handoff_receipt(
    descriptor: &InterfaceDescriptor,
    input: &InterfaceInvocationEnvelope,
    source_movement_seq: u64,
    current_record: &[u8],
    session: &[SessionContextArtifact],
    outcomes: &[octopus_run_authoring::ProviderHandoffOutcome],
    output_root: &Path,
) -> Result<Option<ProviderEmission>, InterfaceDriverError> {
    use octopus_run_authoring::ProviderHandoffOutcome;
    let Some(ProviderHandoffOutcome::Admitted { seq, artifact }) = outcomes.last() else {
        return Ok(None);
    };
    let matches = session.iter().filter(|entry| entry.source_artifact == *artifact).collect::<Vec<_>>();
    let [effect] = matches.as_slice() else { return Ok(None); };
    if effect.label != TOOL_CALL_LABEL { return Ok(None); }
    let refused = |message: &str| InterfaceDriverError::FailedClosed(message.to_string());
    let runtime = invocation_runtime_metadata(input)?;
    if runtime.interface != descriptor.id || *seq <= source_movement_seq
        || effect.source_movement_seq != Some(*seq)
        || effect.payload.get("label").and_then(serde_json::Value::as_str) != Some(TOOL_CALL_LABEL)
    {
        return Err(refused("recovered tool handoff has conflicting source admission evidence"));
    }
    let raw = effect.payload.get(CODEX_CONTINUITY_FIELD)
        .ok_or_else(|| refused("recovered tool handoff has no pending continuity"))?;
    let mut state = parse_codex_continuity(raw, &runtime.interface, runtime.run.as_str())?;
    let Some(CodexPendingContinuity::DynamicTool { call_id, tool, .. }) = state.pending.as_ref() else {
        return Err(refused("recovered tool handoff has no exact pending tool"));
    };
    if state.interrupted_pending.is_some()
        || effect.payload.get("correlation_id").and_then(serde_json::Value::as_str) != Some(call_id.as_str())
        || effect.payload.get("tool").and_then(serde_json::Value::as_str) != Some(tool.as_str())
        || !effect.payload.get("arguments").is_some_and(serde_json::Value::is_object)
    {
        return Err(refused("recovered tool handoff differs from its durable pending effect"));
    }
    if let Some(operation) = native_pending_operation(state.pending.as_ref().expect("checked above"))?
        && effect.payload.get("arguments") != Some(&operation.effect.arguments)
    {
        return Err(refused("recovered native handoff changed its declared effect arguments"));
    }
    if reconcile_operator_session_continuity(descriptor, input, Some(state.clone()), session)? != Some(state.clone()) {
        return Err(refused("recovered tool handoff is not the current pending continuity"));
    }
    let record = octopus_run_record::RunRecord::from_bytes(current_record)
        .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
    if record.id != runtime.run || record.worker != runtime.worker
        || record.interface_state.get(&runtime.interface).and_then(|state| state.get("context_generation"))
            .and_then(serde_json::Value::as_u64) != Some(state.context_generation)
    {
        return Err(refused("recovered tool handoff differs from its checked run generation"));
    }
    state.context_generation = state.context_generation.checked_add(1)
        .ok_or_else(|| refused("recovered tool handoff context generation overflowed"))?;
    let payload = serde_json::json!({
        "label": INTERFACE_CONTEXT_UPDATED_LABEL,
        "code": "recovered_tool_handoff",
        "source_movement_seq": source_movement_seq,
        "source_artifact": input.artifact,
        "effect_artifact": artifact,
        "effect_movement_seq": seq,
        CODEX_CONTINUITY_FIELD: state,
    });
    let artifact = materialize_codex_emitted_artifact(output_root, INTERFACE_CONTEXT_UPDATED_LABEL,
        &serde_json::to_vec(&payload).map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?)?;
    Ok(Some(ProviderEmission { artifact, route: ProviderEmissionRoute::ClientProjection }))
}

pub(crate) fn correlated_tool_result(
    input: &InterfaceInvocationEnvelope,
) -> Result<Option<ToolResultArtifact>, InterfaceDriverError> {
    let payload = serde_json::from_slice(&input.read_artifact_payload()?)
        .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
    let Some(result) = decode_tool_result_artifact(&payload)? else { return Ok(None); };
    if result.label != input.read_artifact_manifest()?.label {
        return Err(InterfaceDriverError::FailedClosed("tool result changed its admitted manifest label".into()));
    }
    Ok(result.correlation_id.is_some().then_some(result))
}

/// A second admitted route may deliver the same immutable tool result. The
/// caller must first prove completion of its earlier input through checked run
/// history. A non-context acknowledgement preserves the current native wait,
/// context generation and live control target without submitting the result twice.
pub(crate) fn duplicate_tool_result_receipt(
    descriptor: &InterfaceDescriptor,
    input: &InterfaceInvocationEnvelope,
    source_movement_seq: u64,
    completed_source: u64,
    current_record: &[u8],
    session: &[SessionContextArtifact],
    output_root: &Path,
) -> Result<ProviderEmission, InterfaceDriverError> {
    let refused = |message: &str| InterfaceDriverError::FailedClosed(message.into());
    let result = correlated_tool_result(input)?
        .ok_or_else(|| refused("duplicate result has no exact tool correlation"))?;
    let prior = session.iter().filter(|entry| entry.source_artifact == input.artifact).collect::<Vec<_>>();
    let payload: serde_json::Value = serde_json::from_slice(&input.read_artifact_payload()?)
        .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
    if completed_source >= source_movement_seq || prior.len() > 1 {
        return Err(refused("duplicate result differs from its completed input"));
    }
    // A completed input need not have been retained as context. Completion is
    // proved by the handoff's ledger query, while any retained copy must agree.
    if prior.first().is_some_and(|prior| prior.label != result.label || prior.payload != payload
        || prior.source_movement_seq.is_none_or(|sequence| sequence > source_movement_seq)) {
        return Err(refused("duplicate result conflicts with its checked context copy"));
    }
    let runtime = invocation_runtime_metadata(input)?;
    let state = reconcile_operator_session_continuity(descriptor, input, None, session)?
        .ok_or_else(|| refused("duplicate result has no current checked continuity"))?;
    if state.pending.iter().chain(state.interrupted_pending.iter()).any(|pending| {
        matches!(pending, CodexPendingContinuity::DynamicTool { call_id, .. }
            if Some(call_id.as_str()) == result.correlation_id.as_deref())
    }) {
        return Err(refused("completed duplicate result still owns the current native wait"));
    }
    let record = octopus_run_record::RunRecord::from_bytes(current_record)
        .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
    if record.id != runtime.run || record.worker != runtime.worker
        || record.interface_state.get(&runtime.interface).and_then(|state| state.get("context_generation"))
            .and_then(serde_json::Value::as_u64) != Some(state.context_generation)
    {
        return Err(refused("duplicate result differs from its checked run generation"));
    }
    let payload = serde_json::json!({
        "label": TURN_INPUT_ACKNOWLEDGED_LABEL, "code": "duplicate_tool_result",
        "action": "receive", "status": "acknowledged",
        "source_movement_seq": source_movement_seq, "source_artifact": input.artifact,
        "completed_source_movement_seq": completed_source,
        "context_generation": state.context_generation,
    });
    let artifact = materialize_codex_emitted_artifact(output_root, TURN_INPUT_ACKNOWLEDGED_LABEL,
        &serde_json::to_vec(&payload).map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?)?;
    Ok(ProviderEmission { artifact, route: ProviderEmissionRoute::ClientProjection })
}

pub(crate) fn codex_continuity_waits_for_result(state: &CodexContinuityState) -> bool {
    state.pending.is_some() || state.interrupted_pending.is_some()
}

pub(crate) fn codex_continuity_has_interrupted_pending(state: &CodexContinuityState) -> bool {
    state.interrupted_pending.is_some()
}

pub(crate) fn codex_continuity_successor_is_settled(
    source: Option<&CodexContinuityState>,
    rebased: Option<&CodexContinuityState>,
) -> Result<bool, String> {
    let rebased = rebased.ok_or_else(|| {
        "deferred operator turn release has no Codex continuity coordinates".to_string()
    })?;
    let Some(source) = source else {
        return Ok(!codex_continuity_waits_for_result(rebased)
            && rebased.context_refresh.as_ref().is_none_or(|refresh| refresh.settled_replacement()));
    };
    if source.provider_interface != rebased.provider_interface
        || source.run != rebased.run
        || rebased.context_generation < source.context_generation
        || ((source.thread_id != rebased.thread_id
            || source.thread_shape_fingerprint != rebased.thread_shape_fingerprint
            || source.static_context_fingerprint != rebased.static_context_fingerprint)
            && !rebased.context_refresh.as_ref().is_some_and(|refresh| refresh.connects(source, rebased))
            && !rebased.native_history_retirement.as_ref().is_some_and(|retirement| retirement.connects(source, rebased)))
    {
        return Err(
            "deferred operator turn release is not a monotonic successor of its source continuity"
                .to_string(),
        );
    }
    Ok(!codex_continuity_waits_for_result(rebased)
        && rebased.context_refresh.as_ref().is_none_or(|refresh| refresh.settled_replacement()))
}

pub(crate) fn validate_durable_interruption_output_labels(
    context_output_labels: &BTreeSet<String>,
    session_snapshot_labels: &BTreeSet<String>,
) -> Result<(), InterfaceDriverError> {
    let required_context = [
        TURN_CONTROL_ACKNOWLEDGED_LABEL,
        TURN_INTERRUPTED_RESULT_LABEL,
        TURN_AGENT_MESSAGE_LABEL,
        TURN_REASONING_LABEL,
        TURN_PLAN_UPDATE_LABEL,
    ];
    let missing_context = required_context
        .into_iter()
        .filter(|label| !context_output_labels.contains(*label))
        .collect::<Vec<_>>();
    if !missing_context.is_empty() {
        return Err(InterfaceDriverError::FailedClosed(format!(
            "codex_app_server context_output_labels omit durable interruption outputs: {}",
            missing_context.join(", ")
        )));
    }
    if !session_snapshot_labels.contains(TURN_INTERRUPTED_RESULT_LABEL) {
        return Err(InterfaceDriverError::FailedClosed(format!(
            "codex_app_server session_snapshot_labels omit durable interrupted closure '{TURN_INTERRUPTED_RESULT_LABEL}'"
        )));
    }
    if !context_output_labels.contains(TURN_CONTEXT_COMPACTION_LABEL) {
        return Err(InterfaceDriverError::FailedClosed(format!(
            "codex_app_server context_output_labels omit active history mutation '{TURN_CONTEXT_COMPACTION_LABEL}'"
        )));
    }
    Ok(())
}

pub(crate) fn provider_operations_from_private_config(
    private_config: &serde_json::Map<String, serde_json::Value>,
) -> Result<Option<ProviderOperations>, InterfaceDriverError> {
    let Some(value) = private_config.get("provider_operations").cloned() else {
        return Ok(None);
    };
    let operations: ProviderOperations = serde_json::from_value(value).map_err(|error| {
        InterfaceDriverError::Protocol(format!(
            "codex_app_server provider_operations is malformed: {error}"
        ))
    })?;
    validate_provider_operations(&operations)?;
    Ok(Some(operations))
}

pub(crate) fn configured_provider_operation(
    private_config: &serde_json::Map<String, serde_json::Value>,
    label: &str,
) -> Result<Option<ProviderOperation>, InterfaceDriverError> {
    let Some(operations) = provider_operations_from_private_config(private_config)? else {
        return Ok(None);
    };
    Ok([
        (
            ProviderOperation::AccountLoginStart,
            operations.account_login_start.request_label.as_str(),
        ),
        (
            ProviderOperation::AccountLoginCancel,
            operations.account_login_cancel.request_label.as_str(),
        ),
        (
            ProviderOperation::AccountLoginStatusRead,
            operations.account_login_status_read.request_label.as_str(),
        ),
        (
            ProviderOperation::AccountLogout,
            operations.account_logout.request_label.as_str(),
        ),
        (
            ProviderOperation::AccountRead,
            operations.account_read.request_label.as_str(),
        ),
        (
            ProviderOperation::AccountRateLimitsRead,
            operations.account_rate_limits_read.request_label.as_str(),
        ),
        (
            ProviderOperation::AccountRateLimitResetConsume,
            operations
                .account_rate_limit_reset_consume
                .request_label
                .as_str(),
        ),
        (
            ProviderOperation::AccountUsageRead,
            operations.account_usage_read.request_label.as_str(),
        ),
        (
            ProviderOperation::AccountWorkspaceMessagesRead,
            operations
                .account_workspace_messages_read
                .request_label
                .as_str(),
        ),
        (
            ProviderOperation::CapabilitiesRead,
            operations.capabilities_read.request_label.as_str(),
        ),
    ]
    .into_iter()
    .find_map(|(operation, request_label)| (request_label == label).then_some(operation)))
}
