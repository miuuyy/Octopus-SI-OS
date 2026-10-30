use super::*;

pub(crate) const CONTEXT_REFERENCE_LABEL: &str = "context.reference";
pub(crate) const SESSION_CONTEXT_FIELD: &str = "session_context";

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SessionContextSnapshot {
    pub(crate) schema_version: u32,
    pub(crate) provider_interface: InterfaceId,
    #[serde(default, rename = "parent")]
    pub(crate) _parent: Option<ArtifactRef>,
    #[serde(rename = "artifacts")]
    pub(crate) _artifacts: Vec<SessionContextArtifact>,
}

pub(crate) enum BoundSessionContext {
    Current(Arc<Vec<SessionContextArtifact>>),
    Foreign,
}

pub(crate) type BoundSessionContexts = BTreeMap<String, BoundSessionContext>;

pub(crate) struct ContinuityCandidate {
    pub(crate) current: bool,
    pub(crate) source_movement_seq: Option<u64>,
    pub(crate) state: CodexContinuityState,
}

pub(crate) struct SessionContinuityEvidence {
    pub(crate) label: String,
    pub(crate) payload: serde_json::Value,
    pub(crate) source_movement_seq: Option<u64>,
    pub(crate) source_key: String,
}

#[derive(Default)]
pub(crate) struct AssembledContext {
    pub(crate) role: Vec<String>,
    pub(crate) conversation: Vec<String>,
    pub(crate) static_context_bytes: Vec<u8>,
    pub(crate) session_context_available: bool,
}

pub(crate) fn recover_codex_continuity_resolved(
    artifact_binding_payloads: &[octopus_interfaces::ArtifactBindingPayload],
    bound_session_contexts: &BoundSessionContexts,
    provider_interface: &InterfaceId,
    run: &str,
    context_output_labels: &BTreeSet<String>,
    session_snapshot_labels: &BTreeSet<String>,
    input_sources: &[ArtifactRef],
) -> Result<Option<CodexContinuityState>, InterfaceDriverError> {
    let mut candidates = Vec::new();
    let mut session_evidence = Vec::new();
    let mut sources = BTreeSet::new();
    let mut current_snapshot_seen = false;
    for bound in artifact_binding_payloads {
        if input_sources.contains(&bound.artifact) { continue; }
        let bytes = bound.read_bytes()
            .map_err(|error| InterfaceDriverError::FailedClosed(error.to_string()))?;
        let value: serde_json::Value =
            serde_json::from_slice(bytes.as_ref()).map_err(|error| {
                InterfaceDriverError::Protocol(format!(
                    "Codex continuity carrier '{}' is not JSON: {error}",
                    bound.artifact.id
                ))
            })?;
        let session_context = if value.get(SESSION_CONTEXT_FIELD).is_some() {
            Some(
                bound_session_contexts
                    .get(&continuity_source_key(&bound.artifact))
                    .ok_or_else(|| {
                        InterfaceDriverError::FailedClosed(format!(
                            "Codex session carrier '{}' has no resolved snapshot",
                            bound.artifact.id
                        ))
                    })?,
            )
        } else {
            None
        };
        if matches!(session_context, Some(BoundSessionContext::Foreign)) {
            continue;
        }
        let is_current_snapshot = matches!(session_context, Some(BoundSessionContext::Current(_)));
        if is_current_snapshot && current_snapshot_seen {
            return Err(InterfaceDriverError::FailedClosed(
                "Codex invocation carries multiple current session snapshots".to_string(),
            ));
        }
        if is_current_snapshot {
            current_snapshot_seen = true;
        }
        if let Some(raw) = value.get(CODEX_CONTINUITY_FIELD) {
            let label = continuity_carrier_label(&value, bound.artifact.id.as_str())?;
            if !context_output_labels.contains(label) {
                return Err(InterfaceDriverError::FailedClosed(format!(
                    "Codex continuity carrier '{}' is not a descriptor-declared provider context output",
                    bound.artifact.id
                )));
            }
            if !sources.insert(continuity_source_key(&bound.artifact)) {
                return Err(InterfaceDriverError::FailedClosed(
                    "Codex continuity repeats one immutable outer artifact source".to_string(),
                ));
            }
            let state = parse_codex_continuity(raw, provider_interface, run)?;
            candidates.push(ContinuityCandidate {
                current: true,
                source_movement_seq: None,
                state,
            });
        }
        let Some(BoundSessionContext::Current(session_artifacts)) = session_context else {
            continue;
        };
        let label = continuity_carrier_label(&value, bound.artifact.id.as_str())?;
        if !session_snapshot_labels.contains(label) {
            return Err(InterfaceDriverError::FailedClosed(format!(
                "Codex session context carrier '{}' is not a descriptor-declared snapshot output",
                bound.artifact.id
            )));
        }
        let historical_len = session_artifacts.len().checked_sub(1).ok_or_else(|| {
            InterfaceDriverError::FailedClosed(format!(
                "Codex session carrier '{}' resolves no current terminal artifact",
                bound.artifact.id
            ))
        })?;
        for artifact in &session_artifacts[..historical_len] {
            if input_sources.contains(&artifact.source_artifact) { continue; }
            if is_foreign_provider_payload(&artifact.payload, provider_interface)? {
                continue;
            }
            let source_key = continuity_source_key(&artifact.source_artifact);
            if let Some(raw) = artifact.payload.get(CODEX_CONTINUITY_FIELD) {
                if !context_output_labels.contains(&artifact.label) {
                    return Err(InterfaceDriverError::FailedClosed(format!(
                        "Codex historical continuity label '{}' is not a descriptor-declared provider context output",
                        artifact.label
                    )));
                }
                if !sources.insert(source_key.clone()) {
                    return Err(InterfaceDriverError::FailedClosed(
                        "Codex continuity repeats one immutable historical artifact source"
                            .to_string(),
                    ));
                }
                let source_movement_seq = artifact.source_movement_seq.ok_or_else(|| {
                    InterfaceDriverError::FailedClosed(
                        "Codex historical continuity has no source movement sequence".to_string(),
                    )
                })?;
                candidates.push(ContinuityCandidate {
                    current: false,
                    source_movement_seq: Some(source_movement_seq),
                    state: parse_codex_continuity(raw, provider_interface, run)?,
                });
            }
            session_evidence.push(SessionContinuityEvidence {
                label: artifact.label.clone(),
                payload: artifact.payload.clone(),
                source_movement_seq: artifact.source_movement_seq,
                source_key,
            });
        }
    }
    candidates.sort_by_key(|candidate| candidate.state.context_generation);
    if candidates
        .windows(2)
        .any(|pair| pair[0].state.context_generation == pair[1].state.context_generation)
    {
        return Err(InterfaceDriverError::FailedClosed(
            "Codex continuity has an ambiguous context generation".to_string(),
        ));
    }
    if let Some(outer) = candidates.iter().rev().find(|candidate| candidate.current) {
        if candidates.last().map(|candidate| &candidate.state) != Some(&outer.state) {
            return Err(InterfaceDriverError::FailedClosed(
                "Codex current continuity is stale against its embedded history".to_string(),
            ));
        }
        let pending = outer
            .state
            .pending
            .as_ref()
            .or(outer.state.interrupted_pending.as_ref());
        let source_movement_seq = pending.and_then(|pending| {
            candidates
                .iter()
                .filter(|candidate| {
                    candidate.state.pending.as_ref() == Some(pending)
                        || candidate.state.interrupted_pending.as_ref() == Some(pending)
                })
                .filter_map(|candidate| candidate.source_movement_seq)
                .min()
        });
        return close_recovered_pending_from_session(
            ContinuityCandidate {
                current: true,
                source_movement_seq: outer.source_movement_seq,
                state: outer.state.clone(),
            },
            &session_evidence,
            source_movement_seq,
        )
        .map(Some);
    }
    let Some(candidate) = candidates.pop() else {
        return Ok(None);
    };
    let pending_source = if let Some(pending) = candidate.state.pending.as_ref().or(candidate.state.interrupted_pending.as_ref()) {
        candidates.iter()
            .filter(|prior| prior.state.pending.as_ref() == Some(pending) || prior.state.interrupted_pending.as_ref() == Some(pending))
            .filter_map(|prior| prior.source_movement_seq)
            .chain(candidate.source_movement_seq)
            .min()
    } else { candidate.source_movement_seq };
    close_recovered_pending_from_session(candidate, &session_evidence, pending_source).map(Some)
}

pub(crate) fn recover_codex_continuity_from_current_session(
    session_artifacts: &[SessionContextArtifact],
    input_sources: &[ArtifactRef],
    provider_interface: &InterfaceId,
    run: &str,
    context_output_labels: &BTreeSet<String>,
) -> Result<Option<CodexContinuityState>, InterfaceDriverError> {
    let mut candidates = Vec::new();
    let mut session_evidence = Vec::new();
    let mut sources = BTreeSet::new();
    for artifact in session_artifacts {
        if input_sources.contains(&artifact.source_artifact) {
            continue;
        }
        if is_foreign_provider_payload(&artifact.payload, provider_interface)? {
            continue;
        }
        let source_key = continuity_source_key(&artifact.source_artifact);
        if !sources.insert(source_key.clone()) {
            return Err(InterfaceDriverError::FailedClosed(
                "Codex current session repeats one immutable artifact source".to_string(),
            ));
        }
        if let Some(raw) = artifact.payload.get(CODEX_CONTINUITY_FIELD) {
            if !context_output_labels.contains(&artifact.label) {
                return Err(InterfaceDriverError::FailedClosed(format!(
                    "Codex current session continuity label '{}' is not a descriptor-declared provider context output",
                    artifact.label
                )));
            }
            let source_movement_seq = artifact.source_movement_seq.ok_or_else(|| {
                InterfaceDriverError::FailedClosed(format!(
                    "Codex current session continuity '{}' has no source movement sequence",
                    source_key
                ))
            })?;
            candidates.push(ContinuityCandidate {
                current: true,
                source_movement_seq: Some(source_movement_seq),
                state: parse_codex_continuity(raw, provider_interface, run)?,
            });
        }
        session_evidence.push(SessionContinuityEvidence {
            label: artifact.label.clone(),
            payload: artifact.payload.clone(),
            source_movement_seq: artifact.source_movement_seq,
            source_key,
        });
    }
    candidates.sort_by_key(|candidate| candidate.state.context_generation);
    if candidates
        .windows(2)
        .any(|pair| pair[0].state.context_generation == pair[1].state.context_generation)
    {
        return Err(InterfaceDriverError::FailedClosed(
            "Codex current session has an ambiguous context generation".to_string(),
        ));
    }
    let Some(candidate) = candidates.pop() else {
        return Ok(None);
    };
    let pending_source = if let Some(pending) = candidate.state.pending.as_ref().or(candidate.state.interrupted_pending.as_ref()) {
        candidates.iter()
            .filter(|prior| prior.state.pending.as_ref() == Some(pending) || prior.state.interrupted_pending.as_ref() == Some(pending))
            .filter_map(|prior| prior.source_movement_seq)
            .chain(candidate.source_movement_seq)
            .min()
    } else { candidate.source_movement_seq };
    close_recovered_pending_from_session(candidate, &session_evidence, pending_source).map(Some)
}

pub(crate) fn reconcile_operator_session_continuity(
    descriptor: &InterfaceDescriptor,
    input: &InterfaceInvocationEnvelope,
    bound: Option<CodexContinuityState>,
    session: &[SessionContextArtifact],
) -> Result<Option<CodexContinuityState>, InterfaceDriverError> {
    let runtime = invocation_runtime_metadata(input)?;
    if runtime.interface != descriptor.id {
        return Err(InterfaceDriverError::FailedClosed(
            "operator continuity crossed provider invocation coordinates".into(),
        ));
    }
    let config = descriptor.interface_driver_config.as_ref().ok_or_else(|| {
        InterfaceDriverError::FailedClosed("operator continuity has no provider config".into())
    })?;
    let labels = private_config_string_set(&config.private_config, "context_output_labels")?;
    reconcile_current_session_continuity(bound, recover_codex_continuity_from_current_session(
        session, std::slice::from_ref(&input.artifact), &runtime.interface, runtime.run.as_str(), &labels,
    )?)
}

pub(crate) fn recovered_failed_result_source(
    descriptor: &InterfaceDescriptor,
    input: &InterfaceInvocationEnvelope,
    bound: &CodexContinuityState,
    session: &[SessionContextArtifact],
    failed_sources: &[u64],
) -> Result<Option<u64>, InterfaceDriverError> {
    let Some(pending) = bound.pending.as_ref() else { return Ok(None); };
    let mut matched = None;
    for entry in session {
        let Some(sequence) = entry.source_movement_seq.filter(|seq| failed_sources.contains(seq)) else { continue; };
        let evidence = SessionContinuityEvidence {
            label: entry.label.clone(), payload: entry.payload.clone(),
            source_movement_seq: Some(sequence), source_key: continuity_source_key(&entry.source_artifact),
        };
        if session_evidence_targets_pending(&evidence, pending)? && matched.replace(sequence).is_some() {
            return Err(InterfaceDriverError::FailedClosed("failed provider context contains multiple results for one pending request".into()));
        }
    }
    if matched.is_some() {
        let settled = reconcile_operator_session_continuity(descriptor, input, Some(bound.clone()), session)?;
        if !codex_continuity_successor_is_settled(Some(bound), settled.as_ref()).map_err(InterfaceDriverError::FailedClosed)? {
            return Err(InterfaceDriverError::FailedClosed("failed provider source did not settle its exact pending result".into()));
        }
    }
    Ok(matched)
}

pub(crate) fn cut_turn_context(
    session: &[SessionContextArtifact],
    input: &ArtifactRef,
    cut: &CodexContinuityState,
    call: &str,
    label: &str,
    payload: &[u8],
    projection: ModelResultProjection,
) -> Result<String, InterfaceDriverError> {
    let payload: serde_json::Value = serde_json::from_slice(payload).map_err(|error| {
        InterfaceDriverError::Protocol(format!("cut turn result {} is not JSON: {error}", input.id.as_str()))
    })?;
    let mut assembled = AssembledContext::default();
    append_assembled_context(&mut assembled, Some(label), &payload, input.id.as_str(), input, projection)?;
    let result = assembled.conversation.join("\n\n");
    let heading = format!(
        "Your previous turn ended when the process running it stopped, before the result of its {call} call reached you. That result has since arrived:"
    );
    Ok(match resumed_model_context(session, input, cut, projection)? {
        Some(earlier) => format!("{earlier}\n\n{heading}\n{result}"),
        None => format!("{heading}\n{result}"),
    })
}

pub(crate) fn resumed_model_context(
    session: &[SessionContextArtifact],
    input: &ArtifactRef,
    resumed: &CodexContinuityState,
    projection: ModelResultProjection,
) -> Result<Option<String>, InterfaceDriverError> {
    let mut start = 0;
    for (position, entry) in session.iter().enumerate().rev() {
        if !matches!(entry.label.as_str(), TURN_AGENT_MESSAGE_LABEL | INTERFACE_ARTIFACT_OUTPUT_LABEL | TOOL_CALL_LABEL)
            || is_foreign_provider_payload(&entry.payload, &resumed.provider_interface)?
        {
            continue;
        }
        let Some(raw) = entry.payload.get(CODEX_CONTINUITY_FIELD) else { continue; };
        let state = parse_codex_continuity(raw, &resumed.provider_interface, &resumed.run)?;
        if state.thread_id == resumed.thread_id {
            start = position + 1;
            break;
        }
    }
    let mut assembled = AssembledContext::default();
    for entry in session[start..].iter().filter(|entry| &entry.source_artifact != input) {
        append_assembled_context(&mut assembled, Some(&entry.label), &entry.payload,
            entry.source_artifact.id.as_str(), &entry.source_artifact, projection)?;
    }
    Ok((!assembled.conversation.is_empty()).then(|| assembled.conversation.join("\n\n")))
}

pub(crate) fn reconcile_delivered_interrupted_continuity(
    bound: Option<&CodexContinuityState>,
    current: Option<CodexContinuityState>,
    delivered: Option<CodexContinuityState>,
) -> Result<Option<CodexContinuityState>, InterfaceDriverError> {
    let Some(delivered) = delivered else { return Ok(current); };
    let bound = bound.filter(|state| state.interrupted_pending.is_some()).ok_or_else(|| {
        InterfaceDriverError::FailedClosed("delivered interruption has no exact bound interrupted request".into())
    })?;
    if !codex_continuity_successor_is_settled(Some(bound), Some(&delivered))
        .map_err(InterfaceDriverError::FailedClosed)?
        || delivered.context_generation <= bound.context_generation {
        return Err(InterfaceDriverError::FailedClosed("delivered interruption is not a settled native successor".into()));
    }
    let current = current.ok_or_else(|| InterfaceDriverError::FailedClosed(
        "delivered interruption has no recovered native continuity".into()))?;
    codex_continuity_successor_is_settled(Some(bound), Some(&current))
        .map_err(InterfaceDriverError::FailedClosed)?;
    if current.context_generation >= delivered.context_generation {
        if codex_continuity_waits_for_result(&current) {
            return Err(InterfaceDriverError::FailedClosed("delivered interruption cannot settle a later pending request".into()));
        }
        return Ok(Some(current));
    }
    if current.pending.is_some()
        || current.interrupted_pending.as_ref().is_some_and(|pending| Some(pending) != bound.interrupted_pending.as_ref()) {
        return Err(InterfaceDriverError::FailedClosed("delivered interruption differs from the recovered pending request".into()));
    }
    reconcile_current_session_continuity(Some(current), Some(delivered))
}

pub(crate) fn reconcile_current_session_continuity(
    bound: Option<CodexContinuityState>,
    current: Option<CodexContinuityState>,
) -> Result<Option<CodexContinuityState>, InterfaceDriverError> {
    let (bound, current) = match (bound, current) {
        (Some(bound), Some(current)) => (bound, current),
        (Some(bound), None) => return Ok(Some(bound)),
        (None, Some(current)) => return Ok(Some(current)),
        (None, None) => return Ok(None),
    };
    if current.provider_interface != bound.provider_interface
        || current.run != bound.run
        || current.context_generation < bound.context_generation
    {
        return Err(InterfaceDriverError::FailedClosed(
            "Codex current session continuity regressed or crossed invocation coordinates"
                .to_string(),
        ));
    }
    if current.context_generation == bound.context_generation {
        let clears_active = bound.pending.is_some()
            && current.pending.is_none()
            && bound.interrupted_pending == current.interrupted_pending;
        let clears_interrupted = bound.interrupted_pending.is_some()
            && current.interrupted_pending.is_none()
            && bound.pending == current.pending;
        if current != bound && !clears_active && !clears_interrupted {
            return Err(InterfaceDriverError::FailedClosed(
                "Codex current session continuity conflicts at one context generation".to_string(),
            ));
        }
        return Ok(Some(current));
    }
    if codex_continuity_waits_for_result(&bound)
        && (current.thread_id != bound.thread_id
            || current.thread_shape_fingerprint != bound.thread_shape_fingerprint
            || current.static_context_fingerprint != bound.static_context_fingerprint)
        && !current.context_refresh.as_ref().is_some_and(|refresh| refresh.connects(&bound, &current))
        && !current.native_history_retirement.as_ref().is_some_and(|retirement| retirement.connects(&bound, &current))
    {
        return Err(InterfaceDriverError::FailedClosed(
            "Codex current session detached a pending result from its exact private thread"
                .to_string(),
        ));
    }
    Ok(Some(current))
}

pub(crate) fn terminal_output_continuity(
    descriptor: &InterfaceDescriptor,
    input: &InterfaceInvocationEnvelope,
    current_session_context: &[SessionContextArtifact],
    expected_run: &octopus_descriptors::RunId,
    current_run_record: &[u8],
    deferred_context_generations: u64,
) -> Result<Option<CodexContinuityState>, InterfaceDriverError> {
    let runtime = invocation_runtime_metadata(input)?;
    if &runtime.run != expected_run || runtime.interface != descriptor.id {
        return Err(InterfaceDriverError::FailedClosed(
            "terminal Codex settlement crossed provider invocation coordinates".to_string(),
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
    let Some(mut continuity) = recover_codex_continuity_from_current_session(
        current_session_context,
        std::slice::from_ref(&input.artifact),
        &runtime.interface,
        runtime.run.as_str(),
        &context_output_labels,
    )?
    else {
        return Ok(None);
    };
    if let Some(pending) = continuity.pending.as_ref() {
        let manifest = input.read_artifact_manifest()?;
        let payload = input.read_artifact_payload()?;
        if let Ok(payload) = serde_json::from_slice::<serde_json::Value>(&payload) {
            let evidence = SessionContinuityEvidence {
                label: manifest.label,
                payload,
                source_movement_seq: None,
                source_key: continuity_source_key(&input.artifact),
            };
            if session_evidence_targets_pending(&evidence, pending)? {
                continuity.pending = None;
            }
        }
    }
    let record: serde_json::Value = serde_json::from_slice(current_run_record)
        .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
    if record.get("id").and_then(serde_json::Value::as_str) != Some(expected_run.as_str()) {
        return Err(InterfaceDriverError::FailedClosed(
            "terminal Codex settlement lost its current run record".into(),
        ));
    }
    let generation = read_codex_context_generation(&record, &runtime.interface)?
        .checked_add(deferred_context_generations)
        .ok_or_else(|| InterfaceDriverError::FailedClosed(
            "terminal Codex continuity context generation overflowed".into(),
        ))?;
    if continuity.context_generation > generation {
        return Err(InterfaceDriverError::FailedClosed(
            "terminal Codex continuity is ahead of its authored context".into(),
        ));
    }
    continuity.context_generation =
        generation
            .checked_add(1)
            .ok_or_else(|| {
                InterfaceDriverError::FailedClosed(
                    "terminal Codex continuity context generation overflowed".to_string(),
                )
            })?;
    Ok(Some(continuity))
}

pub(crate) fn close_recovered_pending_from_session(
    candidate: ContinuityCandidate,
    session_evidence: &[SessionContinuityEvidence],
    pending_source_movement_seq: Option<u64>,
) -> Result<CodexContinuityState, InterfaceDriverError> {
    let ContinuityCandidate {
        source_movement_seq,
        mut state,
        ..
    } = candidate;
    let (pending, interrupted) = match (state.pending.as_ref(), state.interrupted_pending.as_ref())
    {
        (Some(pending), None) => (pending, false),
        (None, Some(pending)) => (pending, true),
        (None, None) => return Ok(state),
        (Some(_), Some(_)) => {
            return Err(InterfaceDriverError::FailedClosed(
                "Codex recovered continuity carries active and interrupted pending requests"
                    .to_string(),
            ));
        }
    };
    let Some(pending_seq) = pending_source_movement_seq else {
        return Ok(state);
    };
    let mut closing_source = None;
    for evidence in session_evidence {
        match evidence.source_movement_seq {
            Some(evidence_seq) if evidence_seq <= pending_seq => continue,
            Some(_) => {}
            None => {
                if session_evidence_targets_pending(evidence, pending)? {
                    return Err(InterfaceDriverError::FailedClosed(format!(
                        "Codex pending result '{}' has no source movement sequence",
                        evidence.source_key
                    )));
                }
                continue;
            }
        }
        if !session_evidence_targets_pending(evidence, pending)? {
            continue;
        }
        if closing_source
            .replace((evidence.source_key.as_str(), evidence.source_movement_seq.expect("validated result movement")))
            .is_some()
        {
            return Err(InterfaceDriverError::FailedClosed(
                "Codex session context contains multiple results for one pending request"
                    .to_string(),
            ));
        }
    }
    if let Some((_, result_seq)) = closing_source {
        if source_movement_seq.is_some_and(|continuity_seq| result_seq > continuity_seq) {
            state.context_generation = state.context_generation.checked_add(1).ok_or_else(|| {
                InterfaceDriverError::FailedClosed(
                    "Codex admitted result context generation overflowed".to_string(),
                )
            })?;
        }
        state.tool_operator_request = None;
        if interrupted {
            state.interrupted_pending = None;
        } else {
            state.pending = None;
        }
    }
    Ok(state)
}

pub(crate) fn session_evidence_targets_pending(
    evidence: &SessionContinuityEvidence,
    pending: &CodexPendingContinuity,
) -> Result<bool, InterfaceDriverError> {
    match pending {
        CodexPendingContinuity::DynamicTool { call_id, tool, .. } => {
            if evidence
                .payload
                .get("correlation_id")
                .and_then(serde_json::Value::as_str)
                != Some(call_id)
            {
                return Ok(false);
            }
            let result = decode_tool_result_artifact(&evidence.payload)?.ok_or_else(|| {
                InterfaceDriverError::FailedClosed(format!(
                    "Codex session artifact '{}' reuses pending correlation '{}' without a strict tool-result envelope",
                    evidence.source_key, call_id
                ))
            })?;
            validate_recovered_tool_result(&result, pending)?;
            if ToolOperatorRequest::from_result(&result)?.is_some() {
                return Ok(false);
            }
            if evidence.label != result.label {
                return Err(InterfaceDriverError::FailedClosed(format!(
                    "Codex session tool-result source label '{}' conflicts with payload label '{}'",
                    evidence.label, result.label
                )));
            }
            if result.tool != *tool {
                return Err(InterfaceDriverError::FailedClosed(
                    "Codex session tool result conflicts with its durable pending tool".to_string(),
                ));
            }
            if let Some(operation) = native_pending_operation(pending)? {
                return operation.advance(&evidence.payload)
                    .map(|step| matches!(step, NativeStep::Complete { .. }))
                    .map_err(InterfaceDriverError::FailedClosed);
            }
            Ok(true)
        }
        CodexPendingContinuity::InterfaceOperation {
            call_id,
            operation,
            output_label,
            ..
        } => {
            if evidence
                .payload
                .get("correlation_id")
                .and_then(serde_json::Value::as_str)
                != Some(call_id)
            {
                return Ok(false);
            }
            validate_recovered_interface_operation_result(
                &evidence.label,
                &evidence.payload,
                call_id,
                operation,
                output_label,
            )?;
            Ok(true)
        }
        CodexPendingContinuity::Operator {
            request_key,
            method,
            ..
        } => {
            let Some(response) = decode_operator_response_artifact(&evidence.payload)? else {
                return Ok(false);
            };
            let request_id = response.request_id()?;
            if format!("operator:{request_id}") != *request_key {
                return Ok(false);
            }
            operator_response_value(method, &response)?;
            Ok(true)
        }
    }
}

pub(crate) fn continuity_carrier_label<'a>(
    value: &'a serde_json::Value,
    source: &str,
) -> Result<&'a str, InterfaceDriverError> {
    value
        .get("label")
        .and_then(serde_json::Value::as_str)
        .filter(|label| !label.trim().is_empty() && label.trim() == *label)
        .ok_or_else(|| {
            InterfaceDriverError::FailedClosed(format!(
                "Codex continuity carrier '{source}' has no exact artifact label"
            ))
        })
}

pub(crate) fn continuity_source_key(artifact: &ArtifactRef) -> String {
    format!("{}:{}", artifact.id, artifact.content.to_hex())
}

pub(crate) fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(crate) fn parse_codex_continuity(
    value: &serde_json::Value,
    provider_interface: &InterfaceId,
    run: &str,
) -> Result<CodexContinuityState, InterfaceDriverError> {
    let state: CodexContinuityState = serde_json::from_value(value.clone()).map_err(|error| {
        InterfaceDriverError::Protocol(format!("{CODEX_CONTINUITY_MALFORMED_PREFIX}{error}"))
    })?;
    if state.schema_version != 1
        || state.provider_interface != *provider_interface
        || state.run != run
        || state.thread_id.trim().is_empty()
        || state.thread_id.trim() != state.thread_id
        || !is_sha256_hex(&state.thread_shape_fingerprint)
        || !is_sha256_hex(&state.static_context_fingerprint)
    {
        return Err(InterfaceDriverError::FailedClosed(
            "Codex continuity state has forged, cross-provider, or incomplete coordinates"
                .to_string(),
        ));
    }
    if state.pending.is_some() && state.interrupted_pending.is_some() {
        return Err(InterfaceDriverError::FailedClosed(
            "Codex continuity cannot carry active and interrupted pending requests together"
                .to_string(),
        ));
    }
    if let Some(refresh) = &state.context_refresh { refresh.validate(&state)?; }
    if let Some(retirement) = &state.native_history_retirement { retirement.validate(&state)?; }
    validate_pending_continuity_coordinates(state.pending.as_ref(), "pending")?;
    validate_pending_continuity_coordinates(
        state.interrupted_pending.as_ref(),
        "interrupted pending",
    )?;
    if let Some(terminal) = &state.native_terminal {
        let turn_id = match state.interrupted_pending.as_ref() {
            Some(CodexPendingContinuity::DynamicTool { turn_id, .. } | CodexPendingContinuity::InterfaceOperation { turn_id, .. }) => Some(turn_id.as_str()),
            Some(CodexPendingContinuity::Operator { .. }) => return Err(InterfaceDriverError::FailedClosed("native terminal evidence cannot retain an operator request".into())),
            None => None,
        };
        if state.pending.is_some() {
            return Err(InterfaceDriverError::FailedClosed("native terminal evidence cannot carry an active native request".into()));
        }
        terminal.exact_turn(&state.thread_id, turn_id)?;
    }
    for pending in state.pending.iter().chain(state.interrupted_pending.iter()) {
        if let Some(operation) = native_pending_operation(pending)? {
            if operation.source_run != state.run
                || [&operation.call_id, &operation.operation_name].iter().any(|id| id.trim().is_empty() || id.trim() != id.as_str())
            {
                return Err(InterfaceDriverError::FailedClosed("native operation continuity crossed its source run or lost its call identity".into()));
            }
        }
    }
    if let Some(request) = &state.tool_operator_request {
        octopus_run_authoring::validate_payload_artifact_selection(&request.payload_artifact_bindings).map_err(InterfaceDriverError::FailedClosed)?;
        request.request.validate()?;
        request.request.validate_pending(&state)?;
    }
    Ok(state)
}

pub(crate) fn codex_continuity_incompatibility(error: &InterfaceDriverError) -> Option<&str> {
    match error {
        InterfaceDriverError::Protocol(message) => {
            message.strip_prefix(CODEX_CONTINUITY_MALFORMED_PREFIX)
        }
        InterfaceDriverError::Transport(_) | InterfaceDriverError::FailedClosed(_) => None,
    }
}

pub(crate) fn validate_pending_continuity_coordinates(
    pending: Option<&CodexPendingContinuity>,
    lifecycle: &str,
) -> Result<(), InterfaceDriverError> {
    match pending {
        Some(CodexPendingContinuity::DynamicTool {
            native_operation: _,
            turn_id,
            call_id,
            tool,
        }) if [turn_id, call_id, tool]
            .into_iter()
            .any(|value| value.trim().is_empty() || value.trim() != value) =>
        {
            return Err(InterfaceDriverError::FailedClosed(format!(
                "Codex continuity has malformed {lifecycle} dynamic-tool coordinates"
            )));
        }
        Some(CodexPendingContinuity::InterfaceOperation {
            turn_id,
            call_id,
            operation,
            interface,
            output_label,
        }) if [
            turn_id.as_str(),
            call_id.as_str(),
            operation.as_str(),
            interface.as_str(),
            output_label.as_str(),
        ]
        .into_iter()
        .any(|value| value.trim().is_empty() || value.trim() != value) =>
        {
            return Err(InterfaceDriverError::FailedClosed(format!(
                "Codex continuity has malformed {lifecycle} interface-operation coordinates"
            )));
        }
        Some(CodexPendingContinuity::Operator {
            turn_id,
            request_key,
            method,
        }) if turn_id.trim().is_empty()
            || turn_id.trim() != turn_id
            || method.trim().is_empty()
            || method.trim() != method
            || request_key
                .strip_prefix("operator:")
                .is_none_or(|key| key.trim().is_empty() || key.trim() != key) =>
        {
            return Err(InterfaceDriverError::FailedClosed(format!(
                "Codex continuity has malformed {lifecycle} operator coordinates"
            )));
        }
        _ => {}
    }
    Ok(())
}

pub(crate) fn append_static_context_source(bytes: &mut Vec<u8>, bound: &ArtifactBindingPayload) -> Result<(), InterfaceDriverError> {
    let payload_bytes = bound.read_bytes()
        .map_err(|error| InterfaceDriverError::FailedClosed(error.to_string()))?;
    let id = bound.artifact.id.as_str().as_bytes();
    bytes.extend_from_slice(&(id.len() as u64).to_le_bytes());
    bytes.extend_from_slice(id);
    bytes.extend_from_slice(&(payload_bytes.len() as u64).to_le_bytes());
    bytes.extend_from_slice(payload_bytes.as_ref());
    Ok(())
}

pub(crate) fn is_foreign_provider_payload(
    value: &serde_json::Value,
    provider_interface: &InterfaceId,
) -> Result<bool, InterfaceDriverError> {
    let continuity_owner = value.get(CODEX_CONTINUITY_FIELD).map(|raw| {
        raw.get("provider_interface").and_then(serde_json::Value::as_str)
            .filter(|id| !id.is_empty() && id.trim() == *id)
            .map(InterfaceId::new)
            .ok_or_else(|| InterfaceDriverError::Protocol("Codex continuity has no exact provider owner".into()))
    }).transpose()?;
    let snapshot_owner = value.get(SESSION_CONTEXT_FIELD).map(|raw| {
        let snapshot: SessionContextSnapshot = serde_json::from_value(raw.clone())
            .map_err(|error| InterfaceDriverError::Protocol(format!("nested provider snapshot is malformed: {error}")))?;
        if snapshot.schema_version != 3 {
            return Err(InterfaceDriverError::FailedClosed("nested provider snapshot has an unsupported schema".into()));
        }
        Ok(snapshot.provider_interface)
    }).transpose()?;
    if let (Some(left), Some(right)) = (&continuity_owner, &snapshot_owner) {
        if left != right {
            return Err(InterfaceDriverError::FailedClosed("provider artifact has conflicting private-state owners".into()));
        }
    }
    Ok(continuity_owner.or(snapshot_owner).is_some_and(|owner| owner != *provider_interface))
}

pub(crate) fn visible_foreign_provider_payload(value: &serde_json::Value) -> serde_json::Value {
    let mut visible = value.clone();
    if let Some(object) = visible.as_object_mut() {
        object.remove(SESSION_CONTEXT_FIELD);
        object.remove(CODEX_CONTINUITY_FIELD);
        object.remove(CONTEXT_WINDOW_USAGE_FIELD);
    }
    visible
}
