use super::*;

#[derive(Clone)]
pub(crate) struct StoppedResultSettlement {
    pub(crate) source: CodexContinuityState,
    pub(crate) predecessor: ArtifactRef,
    pub(crate) acknowledgement: ArtifactRef,
    pub(crate) acknowledgement_seq: u64,
    pub(crate) control_source: u64,
    pub(crate) control_id: String,
    pub(crate) action: String,
    evidence: serde_json::Value,
}

impl StoppedResultSettlement {
    pub(crate) fn context_generation(&self) -> u64 {
        self.source.context_generation
    }
}

pub(crate) fn prove(
    descriptor: &InterfaceDescriptor,
    input: &InterfaceInvocationEnvelope,
    source_seq: u64,
    current: &[u8],
    session: &[SessionContextArtifact],
    outcomes: &[octopus_run_authoring::ProviderHandoffOutcome],
) -> Result<Option<StoppedResultSettlement>, InterfaceDriverError> {
    let runtime = invocation_runtime_metadata(input)?;
    let record: serde_json::Value = serde_json::from_slice(current)
        .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
    let pending = &record["interface_state"][runtime.interface.as_str()]["pending_handoff"];
    if pending["source_movement_seq"] != source_seq || pending["terminal"] != true {
        return Ok(None);
    }
    let [octopus_run_authoring::ProviderHandoffOutcome::Admitted { seq, artifact }] =
        outcomes
    else {
        return Ok(None);
    };
    let Some(error) = session.iter().find(|entry| {
        entry.source_artifact == *artifact && entry.source_movement_seq == Some(*seq)
    }) else {
        return Ok(None);
    };
    if error.label != TURN_ERROR_LABEL
        || error.payload["will_retry"] != false
        || error.payload["additional_details"]["source_movement_seq"] != source_seq
    {
        return Ok(None);
    }
    if !session.iter().any(|entry| {
        terminal_control_ack(entry)
            && entry
                .source_movement_seq
                .is_some_and(|ack_seq| source_seq < ack_seq && ack_seq < *seq)
    }) {
        return Ok(None);
    }
    let Some(raw) = error.payload.get(CODEX_CONTINUITY_FIELD) else {
        return Ok(None);
    };
    let state = parse_codex_continuity(raw, &runtime.interface, runtime.run.as_str())?;
    if state
        .pending
        .as_ref()
        .is_none_or(|wait| !interrupted_pending_has_external_result(wait))
    {
        return Ok(None);
    }
    let focused = result_call_key(input)?
        .and_then(|key| state.focused(&key))
        .unwrap_or_else(|| state.clone());
    let wait = focused.pending.as_ref().expect("ordinary continuity keeps its wait");
    let key = pending_request_key(wait);
    let fail = |message: &str| InterfaceDriverError::FailedClosed(message.into());
    if runtime.interface != descriptor.id
        || record["id"] != runtime.run.as_str()
        || state.context_generation != read_codex_context_generation(&record, &runtime.interface)?
        || state.interrupted_pending.is_some()
        || state.native_terminal.is_some()
        || focused.tool_operator_request.is_some()
    {
        return Err(fail(
            "stopped result error differs from the current checked pending state",
        ));
    }
    let mut interrupted = focused.clone();
    interrupted.interrupted_pending = interrupted.pending.take();
    if !exact_interrupted_result_source(input, &interrupted)
        .map_err(InterfaceDriverError::FailedClosed)?
    {
        return Err(fail(
            "stopped result differs from its retained exact pending effect",
        ));
    }
    let labels = codex_context_output_labels(descriptor)?;
    let original = session
        .iter()
        .filter_map(|entry| {
            let raw = entry.payload.get(CODEX_CONTINUITY_FIELD)?;
            (entry.label == TOOL_CALL_LABEL
                && entry
                    .source_movement_seq
                    .is_some_and(|seq| seq < source_seq))
            .then_some((entry, raw))
        })
        .filter_map(|(entry, raw)| {
            let original =
                parse_codex_continuity(raw, &runtime.interface, runtime.run.as_str()).ok()?;
            (original.outstanding_call(&key) == Some(wait)
                && entry.payload.get("correlation_id").and_then(serde_json::Value::as_str)
                    .is_some_and(|call| format!("tool:{call}") == key))
            .then_some((entry, original))
        })
        .last()
        .ok_or_else(|| fail("stopped result has no retained exact original effect"))?;
    let (effect, original) = original;
    let mut expected = original
        .focused(&key)
        .ok_or_else(|| fail("stopped result original effect lost its exact call"))?;
    expected.parallel_pending = focused.parallel_pending.clone();
    expected.context_generation = state.context_generation;
    if expected != focused {
        return Err(fail(
            "stopped result error changed its native plan or private thread",
        ));
    }
    if let Some(operation) = native_pending_operation(wait)?
        && effect.payload.get("arguments") != Some(&operation.effect.arguments)
    {
        return Err(fail(
            "stopped result original effect differs from its exact native plan",
        ));
    }
    let effect_seq = effect
        .source_movement_seq
        .expect("selected effect sequence");
    let advances = session
        .iter()
        .filter(|entry| {
            labels.contains(&entry.label)
                && entry
                    .source_movement_seq
                    .is_some_and(|seq| seq > effect_seq)
        })
        .collect::<Vec<_>>();
    let [ack, final_error] = advances.as_slice() else {
        return Err(fail(
            "stopped result has unexplained context changes after its pending effect",
        ));
    };
    if final_error.source_artifact != error.source_artifact
        || !terminal_control_ack(ack)
        || original.context_generation.checked_add(2) != Some(state.context_generation)
    {
        return Err(fail(
            "stopped result lacks its exact non-dispatched Stop and error context lineage",
        ));
    }
    let action = ack.payload["action"]
        .as_str()
        .filter(|action| matches!(*action, "stop" | "interrupt"))
        .ok_or_else(|| fail("stopped result acknowledgement is not a stopping control"))?
        .to_string();
    let control_id = ack.payload["control_id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .ok_or_else(|| fail("stopped result lost its control id"))?
        .to_string();
    let control_source = ack.payload["source_movement_seq"]
        .as_u64()
        .ok_or_else(|| fail("stopped result lost its control source"))?;
    let ack_seq = ack
        .source_movement_seq
        .ok_or_else(|| fail("stopped result acknowledgement has no admission"))?;
    if !(effect_seq < source_seq
        && source_seq < control_source
        && control_source < ack_seq
        && ack_seq < *seq)
    {
        return Err(fail(
            "stopped result control/effect admissions are out of order",
        ));
    }
    Ok(Some(StoppedResultSettlement {
        source: state,
        predecessor: artifact.clone(),
        acknowledgement: ack.source_artifact.clone(),
        acknowledgement_seq: ack_seq,
        control_source,
        control_id,
        action,
        evidence: serde_json::json!({
            "error_artifact": artifact, "error_movement_seq": seq,
            "effect_artifact": effect.source_artifact, "effect_movement_seq": effect_seq,
            "effect_context_generation": original.context_generation,
            "result_artifact": input.artifact, "result_movement_seq": source_seq,
            "stop_artifact": ack.source_artifact, "stop_movement_seq": ack_seq,
            "stop_source_movement_seq": control_source,
            "continuation_dispatched": false,
        }),
    }))
}

fn result_call_key(input: &InterfaceInvocationEnvelope) -> Result<Option<String>, InterfaceDriverError> {
    let result: serde_json::Value = serde_json::from_slice(&input.read_artifact_payload()?)
        .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
    Ok(result
        .get("correlation_id")
        .and_then(serde_json::Value::as_str)
        .map(|call| format!("tool:{call}")))
}

fn terminal_control_ack(entry: &SessionContextArtifact) -> bool {
    entry.label == TURN_CONTROL_ACKNOWLEDGED_LABEL
        && entry.payload.get(CODEX_CONTINUITY_FIELD).is_none()
        && entry.payload["status"] == "acknowledged"
        && matches!(entry.payload["action"].as_str(), Some("stop" | "interrupt"))
        && entry.payload["detail"]["already_terminal"] == true
        && entry.payload["detail"]["external_effect_dispatched"] == false
        && entry.payload["detail"]["linearization"] == "control_reader_terminal_fence"
        && entry.payload["thread_id"].is_null()
        && entry.payload["turn_id"].is_null()
}

pub(crate) struct InterruptedResultSource<'a> {
    pub(crate) continuity: &'a CodexContinuityState,
    pub(crate) control_source: u64,
    pub(crate) control_id: &'a str,
    pub(crate) acknowledgement: &'a ArtifactRef,
    pub(crate) acknowledgement_seq: u64,
}

pub(crate) fn prove_interrupted(
    descriptor: &InterfaceDescriptor,
    input: &InterfaceInvocationEnvelope,
    source_seq: u64,
    current: &[u8],
    session: &[SessionContextArtifact],
    outcomes: &[octopus_run_authoring::ProviderHandoffOutcome],
    journal: &InterruptedResultSource<'_>,
) -> Result<StoppedResultSettlement, InterfaceDriverError> {
    let fail = |message: &str| InterfaceDriverError::FailedClosed(message.into());
    let runtime = invocation_runtime_metadata(input)?;
    let record: serde_json::Value = serde_json::from_slice(current)
        .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
    let pending = &record["interface_state"][runtime.interface.as_str()]["pending_handoff"];
    let [octopus_run_authoring::ProviderHandoffOutcome::Admitted { seq, artifact }] = outcomes else {
        return Err(fail("interrupted settlement requires one admitted completed error"));
    };
    let errors = session.iter().filter(|entry| entry.source_artifact == *artifact
        && entry.source_movement_seq == Some(*seq)).collect::<Vec<_>>();
    let [error] = errors.as_slice() else { return Err(fail("interrupted settlement has no unique admitted predecessor")); };
    if runtime.interface != descriptor.id || record["id"] != runtime.run.as_str()
        || pending["source_movement_seq"] != source_seq || pending["terminal"] != true
        || error.label != TURN_ERROR_LABEL || error.payload["will_retry"] != false
        || error.payload["additional_details"]["source_movement_seq"] != source_seq
    { return Err(fail("interrupted settlement changed its completed error source")); }
    let source = parse_codex_continuity(&error.payload[CODEX_CONTINUITY_FIELD], &runtime.interface, runtime.run.as_str())?;
    let mut expected = journal.continuity.clone();
    expected.context_generation = expected.context_generation.checked_add(1)
        .ok_or_else(|| fail("interrupted settlement generation overflow"))?;
    if source != expected || source.context_generation != read_codex_context_generation(&record, &runtime.interface)? {
        return Err(fail("interrupted settlement error changed its exact interrupted continuity"));
    }
    validate_interrupted_source(input, journal.continuity)?;
    let acks = session.iter().filter(|entry| entry.source_artifact == *journal.acknowledgement
        && entry.source_movement_seq == Some(journal.acknowledgement_seq)).collect::<Vec<_>>();
    let [ack] = acks.as_slice() else { return Err(fail("interrupted settlement has no unique admitted Stop ACK")); };
    let wait = journal.continuity.interrupted_pending.as_ref().expect("validated interrupted source");
    let terminal = journal.continuity.native_terminal.as_ref().expect("validated native terminal");
    if ack.label != TURN_CONTROL_ACKNOWLEDGED_LABEL || ack.payload["status"] != "acknowledged"
        || !matches!(ack.payload["action"].as_str(), Some("stop" | "interrupt"))
        || ack.payload["control_id"] != journal.control_id
        || ack.payload["source_movement_seq"] != journal.control_source
        || ack.payload["thread_id"] != journal.continuity.thread_id
        || ack.payload["turn_id"] != pending_turn_id(wait)
        || ack.payload["detail"]["already_terminal"] != true
        || ack.payload["detail"]["exact_result_pending"] != true
        || ack.payload["detail"]["terminal_evidence_source"] != "thread/read"
        || ack.payload["detail"]["native_terminal"] != serde_json::to_value(terminal).map_err(|error| fail(&error.to_string()))?
        || parse_codex_continuity(&ack.payload[CODEX_CONTINUITY_FIELD], &runtime.interface, runtime.run.as_str())? != *journal.continuity
    { return Err(fail("interrupted settlement ACK differs from its durable actual Stop")); }
    let effects = session.iter().filter(|entry| entry.label == TOOL_CALL_LABEL
        && entry.source_movement_seq.is_some_and(|value| value < source_seq))
        .filter_map(|entry| entry.payload.get(CODEX_CONTINUITY_FIELD).map(|raw| (entry, raw)))
        .map(|(entry, raw)| Ok((entry, parse_codex_continuity(raw, &runtime.interface, runtime.run.as_str())?)))
        .collect::<Result<Vec<_>, InterfaceDriverError>>()?;
    let effects = effects.iter().filter(|(_, state)| state.pending.as_ref() == Some(wait)).collect::<Vec<_>>();
    let [(effect, original)] = effects.as_slice() else { return Err(fail("interrupted settlement has no unique exact original tool call")); };
    let CodexPendingContinuity::DynamicTool { call_id, tool, .. } = wait else { unreachable!() };
    let mut expected_ack = original.clone();
    expected_ack.interrupted_pending = expected_ack.pending.take();
    expected_ack.native_terminal = Some(terminal.clone());
    expected_ack.context_generation = expected_ack.context_generation.checked_add(1)
        .ok_or_else(|| fail("interrupted settlement ACK generation overflow"))?;
    if expected_ack != *journal.continuity || effect.payload["tool"] != *tool
        || effect.payload["correlation_id"] != *call_id || !effect.payload["arguments"].is_object()
    { return Err(fail("interrupted settlement Stop changed its original tool call")); }
    let effect_seq = effect.source_movement_seq.expect("selected effect admission");
    let labels = codex_context_output_labels(descriptor)?;
    let advances = session.iter().filter(|entry| labels.contains(&entry.label)
        && entry.source_movement_seq.is_some_and(|value| value > effect_seq)).collect::<Vec<_>>();
    if advances.len() != 2 || advances[0].source_artifact != ack.source_artifact
        || advances[1].source_artifact != error.source_artifact
        || !(effect_seq < journal.control_source && journal.control_source < journal.acknowledgement_seq
            && effect_seq < source_seq && source_seq < *seq && journal.acknowledgement_seq < *seq)
    { return Err(fail("interrupted settlement has unexplained context or admission order")); }
    Ok(StoppedResultSettlement {
        source, predecessor: artifact.clone(), acknowledgement: journal.acknowledgement.clone(),
        acknowledgement_seq: journal.acknowledgement_seq, control_source: journal.control_source,
        control_id: journal.control_id.into(), action: ack.payload["action"].as_str().expect("checked action").into(),
        evidence: serde_json::json!({
            "error_artifact":artifact,"error_movement_seq":seq,
            "effect_artifact":effect.source_artifact,"effect_movement_seq":effect_seq,
            "effect_context_generation":original.context_generation,
            "result_artifact":input.artifact,"result_movement_seq":source_seq,
            "stop_artifact":journal.acknowledgement,"stop_movement_seq":journal.acknowledgement_seq,
            "stop_source_movement_seq":journal.control_source,"continuation_dispatched":false,
        }),
    })
}

fn validate_interrupted_source(input: &InterfaceInvocationEnvelope, source: &CodexContinuityState) -> Result<(), InterfaceDriverError> {
    let fail = |message: &str| InterfaceDriverError::FailedClosed(message.into());
    let runtime = invocation_runtime_metadata(input)?;
    validate_durable_interrupted_continuity(source, &runtime.interface, runtime.run.as_str()).map_err(InterfaceDriverError::FailedClosed)?;
    let Some(CodexPendingContinuity::DynamicTool { turn_id, native_operation: None, .. }) = source.interrupted_pending.as_ref() else {
        return Err(fail("interrupted settlement requires an ordinary exact dynamic tool result"));
    };
    if source.tool_operator_request.is_some()
        || source.native_terminal.as_ref().ok_or_else(|| fail("interrupted settlement has no native terminal evidence"))?
            .exact_turn(&source.thread_id, Some(turn_id))?.status != TurnStatus::Interrupted
        || !exact_interrupted_result_source(input, source).map_err(InterfaceDriverError::FailedClosed)?
    { return Err(fail("interrupted settlement changed its exact native Interrupted result")); }
    Ok(())
}

pub(crate) fn materialize_interrupted(
    proof: &StoppedResultSettlement, input: &InterfaceInvocationEnvelope, output_root: &Path, labels: &BTreeSet<String>,
) -> Result<ProviderEmission, InterfaceDriverError> {
    validate_interrupted_source(input, &proof.source)?;
    materialize_closure(&proof.source, Some(&proof.evidence), input,
        proof.source.native_terminal.clone().expect("validated terminal"), Vec::new(), output_root, labels)
}

pub(crate) fn materialize_pending_interrupted(
    descriptor: &InterfaceDescriptor, source: &CodexContinuityState, input: &InterfaceInvocationEnvelope,
    source_seq: u64, output_root: &Path, stream_root: &Path, input_context: bool,
) -> Result<Option<Vec<ProviderEmission>>, InterfaceDriverError> {
    if !has_native_interrupted_result(source).map_err(InterfaceDriverError::FailedClosed)? { return Ok(None); }
    validate_interrupted_source(input, source)?;
    let config = &descriptor.interface_driver_config.as_ref().ok_or_else(||
        InterfaceDriverError::FailedClosed("interrupted result has no provider config".into()))?.private_config;
    let namespace = serde_json::from_value::<NativeToolNamespace>(config.get("native_tool_namespace").cloned()
        .ok_or_else(|| InterfaceDriverError::FailedClosed("interrupted result has no native tool namespace".into()))?)
        .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?.validate()?;
    let result: ToolResultArtifact = serde_json::from_slice(&input.read_artifact_payload()?)
        .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
    let CodexPendingContinuity::DynamicTool { turn_id, call_id, .. } = source.interrupted_pending.as_ref().expect("validated source") else { unreachable!() };
    let (emit, mut outputs) = ProviderOutputStream::channel();
    emit.continuity.lock().expect("continuity lock").continuity = Some(source.clone());
    emit.continuity.lock().expect("continuity lock").pending_input_context = input_context;
    let labels = codex_context_output_labels(descriptor)?;
    let projected = private_config_string_set(config, "client_projection_output_labels")?;
    let rules = LiveStreamRules::from_private_config(config)?;
    let sink = TurnSink { emit: &emit, output_root, live_stream_root: stream_root, provider_interface: &descriptor.id,
        run: &source.run, source_movement_seq: source_seq, attempt: 1, context_output_labels: &labels,
        client_projection_output_labels: &projected, live_stream_rules: &rules,
        live_stream: std::sync::Mutex::new(None), continuity: emit.continuity.clone() };
    sink.bind_live_stream(&source.thread_id, turn_id)?;
    sink.send_interrupted_result(Some(CodexTurnOutput::dynamic_tool_completed_after_interrupt(
        namespace.name, result.tool, call_id.clone(), source.thread_id.clone(), turn_id.clone(), result.ok, result.body, result.error,
    )), false, CodexTurnOutput::interrupted_result_closed("tool_result", call_id.clone(), source.thread_id.clone(), turn_id.clone(), result.ok, &input.artifact))?;
    let ProviderStreamEmission::InterruptedResult { activity, stream, retained_outputs, closure } = outputs.try_recv()
        .map_err(|error| InterfaceDriverError::FailedClosed(error.to_string()))? else {
        return Err(InterfaceDriverError::FailedClosed("interrupted result did not seal its exact result group".into()));
    };
    interrupted_result_closure_ref(&closure.artifact, source, &input.artifact, &labels)
        .map_err(InterfaceDriverError::FailedClosed)?.ok_or_else(|| InterfaceDriverError::FailedClosed("interrupted result closure did not validate".into()))?;
    let mut emissions = Vec::new();
    emissions.extend(activity); emissions.extend(stream); emissions.extend(retained_outputs); emissions.push(closure);
    Ok(Some(emissions))
}

pub(crate) fn has_native_interrupted_result(source: &CodexContinuityState) -> Result<bool, String> {
    let Some(CodexPendingContinuity::DynamicTool { turn_id, native_operation: None, .. }) = source.interrupted_pending.as_ref()
        else { return Ok(false); };
    source.native_terminal.as_ref().map(|terminal| terminal.exact_turn(&source.thread_id, Some(turn_id))
        .map(|turn| turn.status == TurnStatus::Interrupted).map_err(|error| error.to_string())).transpose()
        .map(|status| status == Some(true))
}

pub(crate) fn validate_saved_settlement(
    proof: &StoppedResultSettlement, input: &InterfaceInvocationEnvelope,
    value: &serde_json::Value, labels: &BTreeSet<String>,
) -> Result<(), String> {
    validate_interrupted_source(input, &proof.source).map_err(|error| error.to_string())?;
    let result: serde_json::Value = serde_json::from_slice(&input.read_artifact_payload().map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())?;
    if value["completed_source_settlement"] != proof.evidence
        || value["native_terminal_source_continuity"] != serde_json::to_value(&proof.source).map_err(|error| error.to_string())?
        || value["native_terminal_input_context"] != false || value["result_origin"] != "native_turn_completion"
        || value["terminal_evidence_source"] != "thread/read" || value["ok"] != result["ok"]
    { return Err("saved interrupted settlement changed its exact accepted proof".into()); }
    let closed = interrupted_result_closure_payload(TURN_INTERRUPTED_RESULT_LABEL,
        &serde_json::to_vec(value).map_err(|error| error.to_string())?, &proof.source, &input.artifact, labels)?
        .ok_or("saved interrupted settlement has no semantic closure")?;
    let mut expected = proof.source.clone();
    expected.interrupted_pending = None;
    expected.context_generation = expected.context_generation.checked_add(1).ok_or("saved interrupted settlement generation overflow")?;
    if closed != expected { return Err("saved interrupted settlement changed its settled native state".into()); }
    Ok(())
}

impl CodexAppServerInterfaceDriver {
    pub(crate) async fn stopped_result_settlement(
        &self,
        proof: &StoppedResultSettlement,
        input: &InterfaceInvocationEnvelope,
        output_root: &Path,
        labels: &BTreeSet<String>,
    ) -> Result<ProviderEmission, InterfaceDriverError> {
        let pending = proof.source.pending.as_ref().ok_or_else(|| {
            InterfaceDriverError::FailedClosed("stopped result lost its pending effect".into())
        })?;
        let terminal = read_exact_terminal_turn(&self.client.lock().await.request_handle(), &self.next_request_id,
            &proof.source.thread_id, pending_turn_id(pending)).await.map_err(InterfaceDriverError::FailedClosed)?
            .ok_or_else(|| InterfaceDriverError::FailedClosed("stopped result recovery requires exact native terminal evidence; the retained turn is still active".into()))?;
        let events = if proof.source.outstanding_len() > 1 {
            Vec::new()
        } else {
            take_native_terminal_events(
                &mut *self.client.lock().await,
                &terminal,
                &proof.source.thread_id,
                pending_turn_id(pending),
            )?
        };
        materialize(proof, input, terminal, events, output_root, labels)
    }
}

fn materialize(
    proof: &StoppedResultSettlement,
    input: &InterfaceInvocationEnvelope,
    terminal: NativeTerminalEvidence,
    events: Vec<serde_json::Value>,
    output_root: &Path,
    labels: &BTreeSet<String>,
) -> Result<ProviderEmission, InterfaceDriverError> {
    materialize_closure(&proof.source, Some(&proof.evidence), input, terminal, events, output_root, labels)
}

fn materialize_closure(
    source: &CodexContinuityState, evidence: Option<&serde_json::Value>, input: &InterfaceInvocationEnvelope,
    terminal: NativeTerminalEvidence, events: Vec<serde_json::Value>, output_root: &Path, labels: &BTreeSet<String>,
) -> Result<ProviderEmission, InterfaceDriverError> {
    if source.pending.is_some() && source.interrupted_pending.is_some() {
        return Err(InterfaceDriverError::FailedClosed("stopped result has conflicting pending effects".into()));
    }
    let key = result_call_key(input)?.ok_or_else(|| {
        InterfaceDriverError::FailedClosed("stopped result names no correlated call".into())
    })?;
    let focused = source.focused(&key).ok_or_else(|| {
        InterfaceDriverError::FailedClosed("stopped result lost its pending effect".into())
    })?;
    let pending = focused.pending.as_ref().or(focused.interrupted_pending.as_ref()).ok_or_else(|| {
        InterfaceDriverError::FailedClosed("stopped result lost its pending effect".into())
    })?;
    let siblings = source.outstanding_len() > 1;
    let turn = terminal.exact_turn(&source.thread_id, Some(pending_turn_id(pending)))?;
    let retained = if siblings { Vec::new() } else { terminal.outputs(&events)? };
    if turn.status != TurnStatus::Interrupted
        || retained.iter().any(|output| labels.contains(&output.label))
    {
        return Err(InterfaceDriverError::FailedClosed(
            "stopped result cannot discard a completed native answer".into(),
        ));
    }
    let result: serde_json::Value = serde_json::from_slice(&input.read_artifact_payload()?)
        .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
    let ok = result["ok"].as_bool().ok_or_else(|| {
        InterfaceDriverError::FailedClosed("stopped result lost its actual verdict".into())
    })?;
    let (kind, correlation) = match pending {
        CodexPendingContinuity::DynamicTool { call_id, .. } => ("tool_result", call_id),
        CodexPendingContinuity::InterfaceOperation { call_id, .. } => {
            ("interface_operation_result", call_id)
        }
        _ => {
            return Err(InterfaceDriverError::FailedClosed(
                "stopped result is not an external effect".into(),
            ));
        }
    };
    let mut closed = source.clone();
    closed.settle_outstanding(&key);
    if !siblings {
        closed.native_terminal = Some(terminal.clone());
    }
    let mut outputs = Vec::new();
    for mut output in retained {
        normalize_output_identity(&mut output, closed.turn_input.as_ref())?;
        output.value[CODEX_CONTINUITY_FIELD] = serde_json::to_value(&closed)
            .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
        outputs.push(output.value);
    }
    closed.context_generation = closed
        .context_generation
        .checked_add(u64::from(labels.contains(TURN_INTERRUPTED_RESULT_LABEL)))
        .ok_or_else(|| {
            InterfaceDriverError::FailedClosed("stopped result generation overflow".into())
        })?;
    let mut output = CodexTurnOutput::interrupted_result_closed(
        kind,
        correlation.clone(),
        source.thread_id.clone(),
        pending_turn_id(pending).to_string(),
        ok,
        &input.artifact,
    )
    .with_native_terminal_events(events);
    terminal.apply_disposition(&mut output)?;
    output.value["label"] = serde_json::json!(TURN_INTERRUPTED_RESULT_LABEL);
    output.value[CODEX_CONTINUITY_FIELD] = serde_json::to_value(closed)
        .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
    output.value["native_terminal_source_continuity"] = serde_json::to_value(source)
        .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
    output.value["result_origin"] = serde_json::json!("native_turn_completion");
    output.value["terminal_evidence_source"] = serde_json::json!("thread/read");
    output.value["native_terminal_input_context"] = serde_json::json!(false);
    if siblings {
        output.value["native_terminal"] = serde_json::to_value(&terminal)
            .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
    } else {
        output.value["retained_native_outputs"] = serde_json::json!(outputs);
    }
    if let Some(evidence) = evidence { output.value["completed_source_settlement"] = evidence.clone(); }
    let bytes = serde_json::to_vec(&output.value)
        .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
    interrupted_result_closure_payload(
        TURN_INTERRUPTED_RESULT_LABEL,
        &bytes,
        source,
        &input.artifact,
        labels,
    )
    .map_err(InterfaceDriverError::FailedClosed)?
    .ok_or_else(|| {
        InterfaceDriverError::FailedClosed("stopped result closure did not validate".into())
    })?;
    Ok(ProviderEmission {
        artifact: materialize_codex_emitted_artifact(
            output_root,
            TURN_INTERRUPTED_RESULT_LABEL,
            &bytes,
        )?,
        route: ProviderEmissionRoute::ClientProjection,
    })
}

#[cfg(test)]
pub(crate) fn materialize_for_test(
    proof: &StoppedResultSettlement,
    input: &InterfaceInvocationEnvelope,
    turn: serde_json::Value,
    output_root: &Path,
    labels: &BTreeSet<String>,
) -> Result<ProviderEmission, InterfaceDriverError> {
    materialize(
        proof,
        input,
        NativeTerminalEvidence {
            thread_id: proof.source.thread_id.clone(),
            turn,
        },
        Vec::new(),
        output_root,
        labels,
    )
}

pub(crate) struct PendingIntentControl {
    pub(crate) source: CodexContinuityState,
    pub(crate) intent: octopus_run_authoring::PendingSourceIntent,
    applied: serde_json::Value,
}

pub(crate) struct PendingIntentTransport {
    handle: AppServerRequestHandle,
    request_ids: Arc<std::sync::atomic::AtomicI64>,
}

impl CodexTurnControlBroker {
    pub(crate) fn pending_intent_transport(&self) -> Option<PendingIntentTransport> {
        match self.state.borrow().clone() {
            CodexTurnControlTarget::RecoveredPending { handle, request_ids, .. } => Some(PendingIntentTransport { handle, request_ids }),
            _ => None,
        }
    }
}

pub(crate) fn prove_intent(
    descriptor: &InterfaceDescriptor, input: &InterfaceInvocationEnvelope,
    intent: octopus_run_authoring::PendingSourceIntent, session: &[SessionContextArtifact],
) -> Result<PendingIntentControl, InterfaceDriverError> {
    let fail = |message: &str| InterfaceDriverError::FailedClosed(message.into());
    let runtime = invocation_runtime_metadata(input)?;
    let labels = codex_context_output_labels(descriptor)?;
    let original = recover_codex_continuity_from_current_session(session, std::slice::from_ref(&input.artifact), &runtime.interface, runtime.run.as_str(), &labels)?
        .ok_or_else(|| fail("unadmitted native intent has no original pending continuity"))?;
    let wait = original.pending.as_ref().ok_or_else(|| fail("unadmitted native intent has no original effect"))?;
    let operation = native_pending_operation(wait)?.ok_or_else(|| fail("unadmitted intent is not an exact native operation"))?;
    if !matches!(operation.state, crate::run_affordances::NativePhase::Applying { .. } | crate::run_affordances::NativePhase::PinnedApplying { .. } | crate::run_affordances::NativePhase::Writing { .. })
        || original.interrupted_pending.is_some() || original.tool_operator_request.is_some()
        || original.context_generation.checked_add(1) != Some(intent.context_generation)
    { return Err(fail("unadmitted native intent does not follow one applied native write")); }
    let mut exact = original.clone();
    exact.interrupted_pending = exact.pending.take();
    if !exact_interrupted_result_source(input, &exact).map_err(InterfaceDriverError::FailedClosed)? {
        return Err(fail("unadmitted native intent changed its exact original write result"));
    }
    let result: serde_json::Value = serde_json::from_slice(&input.read_artifact_payload()?).map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
    let NativeStep::Effect { pending: successor, effect, .. } = operation.advance(&result).map_err(InterfaceDriverError::FailedClosed)? else {
        return Err(fail("original applied write does not prove the saved native successor"));
    };
    if !matches!(successor.state, crate::run_affordances::NativePhase::AppliedSeeding { .. } | crate::run_affordances::NativePhase::PinnedAppliedSeeding { .. } | crate::run_affordances::NativePhase::Seeding { .. }) {
        return Err(fail("native withdrawal supports only an installed, unseeded run"));
    }
    let payload: serde_json::Value = serde_json::from_slice(&intent.payload).map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
    let next = parse_codex_continuity(payload.get(CODEX_CONTINUITY_FIELD).ok_or_else(|| fail("saved native intent has no exact continuity"))?, &runtime.interface, runtime.run.as_str())?;
    let mut expected = original.clone();
    expected.context_generation = intent.context_generation;
    expected.pending = Some(CodexPendingContinuity::DynamicTool { turn_id: pending_turn_id(wait).into(), call_id: effect.correlation_id.clone(), tool: effect.tool.clone(),
        native_operation: Some(serde_json::to_value(&successor).map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?) });
    if next != expected || payload["tool"] != effect.tool || payload["correlation_id"] != effect.correlation_id || payload["arguments"] != effect.arguments {
        return Err(fail("saved intent differs from the exact applied native operation"));
    }
    let mut source = original;
    source.context_generation = intent.context_generation;
    Ok(PendingIntentControl { source, intent, applied: serde_json::json!({
        "installed":true,"seed_admitted":false,"work_completed":false,
        "original_write_result":result,"unadmitted_native_operation":successor,
    }) })
}

impl PendingIntentTransport {
    pub(crate) async fn interrupted_control(&self, proof: &PendingIntentControl, control: &CodexTurnControl) -> Result<CodexTurnControlResult, InterfaceDriverError> {
        let fail = |message: &str| InterfaceDriverError::FailedClosed(message.into());
        if !matches!(control, CodexTurnControl::Interrupt { .. }) { return Err(fail("pending intent recovery requires its stopping control")); }
        let pending = proof.source.pending.as_ref().ok_or_else(|| fail("pending intent lost its native target"))?;
        let terminal = read_exact_terminal_turn(&self.handle, &self.request_ids, &proof.source.thread_id, pending_turn_id(pending)).await
            .map_err(InterfaceDriverError::FailedClosed)?.ok_or_else(|| fail("recorded Stop intent still has an unknown native outcome"))?;
        if terminal.exact_turn(&proof.source.thread_id, Some(pending_turn_id(pending)))?.status != TurnStatus::Interrupted {
            return Err(fail("recorded Stop intent has no exact native Interrupted outcome"));
        }
        let (mut continuity, _) = interrupted_continuity(Some(proof.source.clone())).map_err(InterfaceDriverError::FailedClosed)?;
        continuity.as_mut().ok_or_else(|| fail("pending intent recovery lost continuity"))?.native_terminal = Some(terminal);
        Ok(CodexTurnControlResult { control_id: control.control_id().into(), action: control.action().into(), acknowledged: true,
            thread_id: Some(proof.source.thread_id.clone()), turn_id: Some(pending_turn_id(pending).into()),
            detail: serde_json::json!({"already_terminal":true,"external_effect_dispatched":false,"terminal_evidence_source":"thread/read"}), continuity })
    }

    pub(crate) async fn closure(
        &self, proof: &PendingIntentControl, input: &InterfaceInvocationEnvelope,
        control_source: u64, control_artifact: &ArtifactRef, control: &CodexTurnControlResult,
        output_root: &Path, labels: &BTreeSet<String>,
    ) -> Result<ProviderEmission, InterfaceDriverError> {
        let fail = |message: &str| InterfaceDriverError::FailedClosed(message.into());
        let pending = proof.source.pending.as_ref().ok_or_else(|| fail("withdrawal lost its original pending effect"))?;
        if !control.acknowledged || !matches!(control.action.as_str(), "stop" | "interrupt")
            || control.thread_id.as_deref() != Some(proof.source.thread_id.as_str())
            || control.turn_id.as_deref() != Some(pending_turn_id(pending))
        { return Err(fail("withdrawal control changed its exact native target")); }
        let terminal = match control.continuity.as_ref().and_then(|continuity| continuity.native_terminal.clone()) {
            Some(terminal) => terminal,
            None => read_exact_terminal_turn(&self.handle, &self.request_ids, &proof.source.thread_id, pending_turn_id(pending)).await
                .map_err(InterfaceDriverError::FailedClosed)?.ok_or_else(|| fail("pending intent withdrawal awaits exact native Interrupted evidence"))?,
        };
        let emission = materialize_closure(&proof.source, None, input, terminal, Vec::new(), output_root, labels)?;
        let mut value: serde_json::Value = serde_json::from_slice(&emission.artifact.read_payload_bytes().map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?)
            .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
        value.as_object_mut().ok_or_else(|| fail("withdrawal closure is not an object"))?.remove("completed_source_settlement");
        value["withdrawn_source_intent"] = serde_json::json!({
            "source_movement_seq":proof.intent.source_movement_seq,"source_artifact":input.artifact,
            "intent_artifact":proof.intent.artifact,"control_source_movement_seq":control_source,"control_artifact":control_artifact,
            "control_id":control.control_id,"action":control.action,"applied_native_operation":proof.applied,
            "continuation_dispatched":false,
        });
        let bytes = serde_json::to_vec(&value).map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
        Ok(ProviderEmission { artifact: materialize_codex_emitted_artifact(output_root, TURN_INTERRUPTED_RESULT_LABEL, &bytes)?, route: ProviderEmissionRoute::ClientProjection })
    }
}

pub(crate) fn closure_continuity(value: &serde_json::Value, provider: &InterfaceId, run: &str) -> Result<CodexContinuityState, String> {
    parse_codex_continuity(value.get(CODEX_CONTINUITY_FIELD).ok_or("withdrawal closure lost continuity")?, provider, run).map_err(|error| error.to_string())
}

impl CodexContinuityState {
    pub(crate) fn advance_control_context(&mut self, descriptor: &InterfaceDescriptor) -> Result<(), String> {
        if codex_context_output_labels(descriptor).map_err(|error| error.to_string())?.contains(TURN_CONTROL_ACKNOWLEDGED_LABEL) {
            self.context_generation = self.context_generation.checked_add(1).ok_or("withdrawal control generation overflow")?;
        }
        Ok(())
    }
}


pub(crate) fn validate_withdrawal_closure(
    value: &serde_json::Value, input: &InterfaceInvocationEnvelope, source: u64, control_artifact: &ArtifactRef,
    control_source: u64, control: &CodexTurnControlResult,
) -> Result<(), String> {
    let runtime = invocation_runtime_metadata(input).map_err(|error| error.to_string())?;
    let evidence = &value["withdrawn_source_intent"];
    if evidence["source_movement_seq"] != source || evidence["source_artifact"] != serde_json::to_value(&input.artifact).map_err(|error| error.to_string())?
        || evidence["control_source_movement_seq"] != control_source || evidence["control_artifact"] != serde_json::to_value(control_artifact).map_err(|error| error.to_string())?
        || evidence["control_id"] != control.control_id || evidence["action"] != control.action || evidence["continuation_dispatched"] != false
    { return Err("withdrawal closure changed its admitted result or Stop input".into()); }
    let original = parse_codex_continuity(&value["native_terminal_source_continuity"], &runtime.interface, runtime.run.as_str()).map_err(|error| error.to_string())?;
    let closed = parse_codex_continuity(&value[CODEX_CONTINUITY_FIELD], &runtime.interface, runtime.run.as_str()).map_err(|error| error.to_string())?;
    let terminal = closed.native_terminal.as_ref().ok_or("withdrawal has no native terminal proof")?;
    let pending = original.pending.as_ref().ok_or("withdrawal lost its original effect")?;
    if terminal.exact_turn(&original.thread_id, Some(pending_turn_id(pending))).map_err(|error| error.to_string())?.status != TurnStatus::Interrupted
        || closed.pending.is_some() || closed.interrupted_pending.is_some()
    { return Err("withdrawal did not close its exact native Interrupted turn".into()); }
    let mut exact = original.clone(); exact.interrupted_pending = exact.pending.take();
    if !exact_interrupted_result_source(input, &exact)? { return Err("withdrawal closed a different original effect".into()); }
    Ok(())
}

pub(crate) fn validate_saved_intent_closure(
    proof: &PendingIntentControl, input: &InterfaceInvocationEnvelope, value: &serde_json::Value,
    control_artifact: &ArtifactRef, control_source: u64, control: &CodexTurnControl,
    labels: &BTreeSet<String>,
) -> Result<(), String> {
    if !matches!(control, CodexTurnControl::Interrupt { .. }) { return Err("withdrawal source is not an admitted stopping control".into()); }
    let runtime = invocation_runtime_metadata(input).map_err(|error| error.to_string())?;
    let source = parse_codex_continuity(&value["native_terminal_source_continuity"], &runtime.interface, runtime.run.as_str()).map_err(|error| error.to_string())?;
    if source != proof.source { return Err("persisted withdrawal changed its exact original native plan".into()); }
    let evidence = &value["withdrawn_source_intent"];
    if evidence["intent_artifact"] != serde_json::to_value(&proof.intent.artifact).map_err(|error| error.to_string())?
        || evidence["applied_native_operation"] != proof.applied
    { return Err("persisted withdrawal changed its unadmitted intent or applied write receipt".into()); }
    let result = CodexTurnControlResult { control_id: control.control_id().into(), action: control.action().into(), acknowledged: true,
        thread_id: Some(source.thread_id.clone()), turn_id: source.pending.as_ref().map(|pending| pending_turn_id(pending).into()), detail: serde_json::Value::Null, continuity: None };
    validate_withdrawal_closure(value, input, proof.intent.source_movement_seq, control_artifact, control_source, &result)?;
    let mut payload = value.clone(); payload.as_object_mut().ok_or("withdrawal closure is not an object")?.remove("session_context");
    interrupted_result_closure_payload(TURN_INTERRUPTED_RESULT_LABEL, &serde_json::to_vec(&payload).map_err(|error| error.to_string())?,
        &source, &input.artifact, labels)?.ok_or("persisted withdrawal is not the exact original result closure")?;
    Ok(())
}

pub(crate) fn intent_control_continuity(
    descriptor: &InterfaceDescriptor, input: &InterfaceInvocationEnvelope,
    intent: octopus_run_authoring::PendingSourceIntent, session: &[SessionContextArtifact],
) -> Result<CodexContinuityState, InterfaceDriverError> {
    let runtime = invocation_runtime_metadata(input)?;
    let value: serde_json::Value = serde_json::from_slice(&intent.payload).map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
    let state = parse_codex_continuity(&value[CODEX_CONTINUITY_FIELD], &runtime.interface, runtime.run.as_str())?;
    let pending = state.pending.as_ref().ok_or_else(|| InterfaceDriverError::FailedClosed("saved pending output has no native wait".into()))?;
    let CodexPendingContinuity::DynamicTool { tool, call_id, .. } = pending else {
        return Err(InterfaceDriverError::FailedClosed("saved tool output has a different native wait".into()));
    };
    if state.context_generation != intent.context_generation || value["tool"] != *tool || value["correlation_id"] != *call_id {
        return Err(InterfaceDriverError::FailedClosed("saved tool output changed its checked context or exact wait".into()));
    }
    if native_pending_operation(pending)?.is_some_and(|operation| matches!(operation.state,
        crate::run_affordances::NativePhase::AppliedSeeding { .. } | crate::run_affordances::NativePhase::PinnedAppliedSeeding { .. } | crate::run_affordances::NativePhase::Seeding { .. })) {
        return Ok(prove_intent(descriptor, input, intent, session)?.source);
    }
    Ok(state)
}
