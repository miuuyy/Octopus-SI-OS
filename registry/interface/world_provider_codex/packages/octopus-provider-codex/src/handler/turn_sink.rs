use super::*;

pub(crate) const DELTA_FLUSH_WINDOW: std::time::Duration = std::time::Duration::from_millis(150);

pub(crate) struct ContinuityEmitterState {
    pub(crate) continuity: Option<CodexContinuityState>,
    pub(crate) pending_input_context: bool,
}

impl Default for ContinuityEmitterState {
    fn default() -> Self {
        Self {
            continuity: None,
            pending_input_context: false,
        }
    }
}

pub(crate) struct TurnSink<'a> {
    pub(crate) emit: &'a Arc<ProviderOutputStream>,
    pub(crate) output_root: &'a Path,
    pub(crate) live_stream_root: &'a Path,
    pub(crate) provider_interface: &'a InterfaceId,
    pub(crate) run: &'a str,
    pub(crate) source_movement_seq: u64,
    pub(crate) attempt: u32,
    pub(crate) context_output_labels: &'a BTreeSet<String>,
    pub(crate) client_projection_output_labels: &'a BTreeSet<String>,
    pub(crate) live_stream_rules: &'a LiveStreamRules,
    pub(crate) live_stream: std::sync::Mutex<Option<LiveTurnStream>>,
    pub(crate) continuity: Arc<std::sync::Mutex<ContinuityEmitterState>>,
}

#[derive(Clone, Copy)]
pub(crate) struct TurnContext<'context, 'sink> {
    pub(crate) run_invocation: Option<NativeRunInvocation<'context>>,
    pub(crate) scope: &'context TransportStateScope,
    pub(crate) sink: &'context TurnSink<'sink>,
    pub(crate) control: &'context Arc<CodexTurnControlBroker>,
    pub(crate) interface_operations: &'context BTreeMap<String, InterfaceOperationAffordance>,
    pub(crate) tool_names: &'context BTreeMap<String, String>,
    pub(crate) source_artifact: &'context ArtifactRef,
    pub(crate) handoff_barrier: &'context Arc<ProviderHandoffBarrier>,
    pub(crate) effect_gate: &'context Arc<tokio::sync::Mutex<()>>,
    pub(crate) turn_interrupted: &'context Arc<std::sync::atomic::AtomicBool>,
    pub(crate) lifecycle: Option<&'context LifecycleBinding>,
    pub(crate) lifecycle_prompt: Option<&'context LifecycleEventArtifact>,
}

#[derive(Clone, Copy)]
pub(crate) struct NativeRunInvocation<'a> {
    pub(crate) affordances: &'a RunAffordances,
    pub(crate) metadata: &'a InvocationMetadata,
    pub(crate) admitted_record: &'a octopus_run_record::RunRecord,
    pub(crate) current_record: &'a octopus_run_record::RunRecord,
}

pub(crate) fn normalize_output_identity(
    output: &mut CodexTurnOutput,
    turn_input: Option<&ArtifactRef>,
) -> Result<(), InterfaceDriverError> {
    let object = output.value.as_object_mut().ok_or_else(|| {
        InterfaceDriverError::Protocol(
            "Codex provider output must be a self-identifying JSON object".to_string(),
        )
    })?;
    match object.get("label") {
        Some(serde_json::Value::String(label)) if label == &output.label => {}
        Some(_) => {
            return Err(InterfaceDriverError::FailedClosed(format!(
                "Codex provider output payload conflicts with manifest label '{}'",
                output.label
            )));
        }
        None => {
            object.insert(
                "label".to_string(),
                serde_json::Value::String(output.label.to_string()),
            );
        }
    }
    if matches!(output.route, ProviderEmissionRoute::ClientProjection | ProviderEmissionRoute::ClientProjectionWithVerifiedInputs { .. })
        && let Some(source) = turn_input
    {
        let source = serde_json::to_value(source)
            .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
        if object.get("in_reply_to").is_some_and(|value| value != &source) {
            return Err(InterfaceDriverError::FailedClosed("Codex output conflicts with its admitted turn input".into()));
        }
        object.insert("in_reply_to".into(), source);
    }
    Ok(())
}

impl TurnSink<'_> {
    pub(crate) fn bind_live_stream(&self, thread_id: &str, turn_id: &str) -> Result<(), InterfaceDriverError> {
        if !self.live_stream_rules.enabled() {
            return Ok(());
        }
        let identity = LiveTurnIdentity {
            provider_interface: self.provider_interface.as_str().to_string(),
            run: self.run.to_string(),
            thread_id: thread_id.to_string(),
            turn_id: turn_id.to_string(),
        };
        let mut current = self.live_stream.lock().expect("live stream lock");
        if let Some(current) = current.as_ref() {
            if current.identity() != &identity {
                return Err(InterfaceDriverError::FailedClosed(
                    "Codex invocation attempted to bind two native turns to one live stream sink"
                        .to_string(),
                ));
            }
            return Ok(());
        }
        *current = Some(
            LiveTurnStream::open(
                self.live_stream_root,
                self.live_stream_rules,
                identity,
                self.source_movement_seq,
            )
            .map_err(InterfaceDriverError::FailedClosed)?,
        );
        Ok(())
    }

    pub(crate) fn has_live_stream(&self) -> bool {
        self.live_stream.lock().expect("live stream lock").is_some()
    }

    pub(crate) fn record_input_item(
        &self,
        item: &ThreadItem,
        thread_id: &str,
        turn_id: &str,
        source: InputRecordedSource,
        native_event_at_ms: Option<i64>,
    ) -> Result<(), InterfaceDriverError> {
        let ThreadItem::UserMessage { id, client_id: Some(client_id), .. } = item else {
            return Ok(());
        };
        if !self.live_stream_rules.enabled() {
            return Ok(());
        }
        self.live_stream.lock().expect("live stream lock")
            .as_mut()
            .ok_or_else(|| InterfaceDriverError::FailedClosed(
                "Codex recorded input has no exact native turn stream".to_string(),
            ))?
            .append_input_recorded(
                self.source_movement_seq,
                InputRecordedObservation {
                    thread_id, turn_id, item_id: id, client_id, source_event: source,
                    native_event_at_ms,
                },
            )
            .map(|_| ())
            .map_err(InterfaceDriverError::FailedClosed)
    }

    pub(crate) async fn admit_app_server_dispatch(
        &self,
        request: &ClientRequest,
        credential_handle_fingerprint: Option<String>,
    ) -> Result<(), InterfaceDriverError> {
        let frame = codex_app_server_request_frame(request)?;
        let artifact = materialize_codex_transport_dispatch(
            self.output_root,
            self.provider_interface,
            self.source_movement_seq,
            self.attempt,
            &frame.method,
            &frame.bytes,
            credential_handle_fingerprint,
        )?;
        self.emit.clone().hand_transport_dispatch(artifact).await?;
        self.advance_continuity(false)?;
        Ok(())
    }

    pub(crate) fn advance_continuity(
        &self,
        context_output: bool,
    ) -> Result<Option<CodexContinuityState>, InterfaceDriverError> {
        let mut state = self.continuity.lock().expect("continuity lock");
        let advance = state.pending_input_context || context_output;
        state.pending_input_context = false;
        if let Some(continuity) = state.continuity.as_mut()
            && advance
        {
            continuity.context_generation = continuity
                .context_generation
                .checked_add(1)
                .ok_or_else(|| {
                    InterfaceDriverError::FailedClosed(
                        "Codex continuity context generation overflowed".to_string(),
                    )
                })?;
        }
        Ok(state.continuity.clone())
    }

    pub(crate) fn set_continuity(&self, continuity: CodexContinuityState) {
        self.continuity.lock().expect("continuity lock").continuity = Some(continuity);
    }

    pub(crate) fn set_pending(&self, pending: Option<CodexPendingContinuity>) {
        if let Some(continuity) = self
            .continuity
            .lock()
            .expect("continuity lock")
            .continuity
            .as_mut()
        {
            if continuity.pending != pending {
                continuity.tool_operator_request = None;
            }
            continuity.pending = pending;
        }
    }

    pub(crate) fn set_interrupted_pending(&self, pending: Option<CodexPendingContinuity>) {
        if let Some(continuity) = self
            .continuity
            .lock()
            .expect("continuity lock")
            .continuity
            .as_mut()
        {
            continuity.interrupted_pending = pending;
        }
    }

    pub(crate) fn continuity_snapshot(&self) -> Option<CodexContinuityState> {
        self.continuity
            .lock()
            .expect("continuity lock")
            .continuity
            .clone()
    }

    pub(crate) fn projection_admits(&self, output: &CodexTurnOutput) -> bool {
        if matches!(
            &output.route,
            ProviderEmissionRoute::ClientProjection
                | ProviderEmissionRoute::ClientProjectionWithVerifiedInputs { .. }
        ) && !self.client_projection_output_labels.contains(&output.label)
        {
            return false;
        }
        true
    }

    pub(crate) fn normalize_output_label(
        &self,
        output: &mut CodexTurnOutput,
    ) -> Result<(), InterfaceDriverError> {
        let continuity = self.continuity_snapshot();
        normalize_output_identity(output, continuity.as_ref().and_then(|state| state.turn_input.as_ref()))
    }

    pub(crate) fn materialize_normalized(
        &self,
        mut output: CodexTurnOutput,
        append_boundary: bool,
    ) -> Result<ProviderEmission, InterfaceDriverError> {
        let continuity = self.advance_continuity(self.context_output_labels.contains(&output.label))?;
        let continuity = continuity
            .map(|continuity| {
                serde_json::to_value(continuity).map_err(|error| {
                    InterfaceDriverError::Protocol(format!(
                        "Codex continuity state could not be encoded: {error}"
                    ))
                })
            })
            .transpose()?;
        if let Some(continuity) = continuity.filter(|_| !lifecycle_payload_label(&output.label)) {
            output
                .value
                .as_object_mut()
                .ok_or_else(|| {
                    InterfaceDriverError::Protocol(
                        "Codex provider output carrying continuity must be a JSON object"
                            .to_string(),
                    )
                })?
                .insert(CODEX_CONTINUITY_FIELD.to_string(), continuity);
        }
        let output_bytes = serde_json::to_vec(&output.value).map_err(|e| {
            InterfaceDriverError::Protocol(format!("failed to encode provider output: {e}"))
        })?;
        let file =
            materialize_codex_emitted_artifact(self.output_root, &output.label, &output_bytes)?;
        if append_boundary
            && let Some(stream) = self.live_stream.lock().expect("live stream lock").as_mut()
        {
            let artifact_payload_blake3 =
                artifact_payload_blake3(&file).map_err(InterfaceDriverError::FailedClosed)?;
            stream
                .append_boundary(
                    self.source_movement_seq,
                    &output.label,
                    &artifact_payload_blake3,
                )
                .map_err(InterfaceDriverError::FailedClosed)?;
        }
        Ok(ProviderEmission {
            artifact: file,
            route: output.route,
        })
    }

    pub(crate) fn materialize_with_boundary(
        &self,
        mut output: CodexTurnOutput,
        append_boundary: bool,
    ) -> Result<Option<ProviderEmission>, InterfaceDriverError> {
        if !self.projection_admits(&output) {
            return Ok(None);
        }
        self.normalize_output_label(&mut output)?;
        if self.live_stream_rules.disposition(
            &output.label,
            &output.value,
            self.has_live_stream(),
        )? == LiveStreamDisposition::Stream
        {
            return Err(InterfaceDriverError::FailedClosed(format!(
                "Codex transient output '{}' reached immutable artifact materialization",
                output.label
            )));
        }
        self.materialize_normalized(output, append_boundary)
            .map(Some)
    }

    pub(crate) fn materialize(
        &self,
        output: CodexTurnOutput,
    ) -> Result<Option<ProviderEmission>, InterfaceDriverError> {
        self.materialize_with_boundary(output, true)
    }

    pub(crate) fn materialize_without_boundary(
        &self,
        output: CodexTurnOutput,
    ) -> Result<Option<ProviderEmission>, InterfaceDriverError> {
        self.materialize_with_boundary(output, false)
    }

    pub(crate) fn send(&self, output: CodexTurnOutput) -> Result<(), InterfaceDriverError> {
        self.send_observation(output, false)
    }

    pub(crate) fn send_progress(&self, output: CodexTurnOutput) -> Result<(), InterfaceDriverError> {
        if output.route != ProviderEmissionRoute::ClientProjection {
            return Err(InterfaceDriverError::FailedClosed(
                "turn progress requires an ordinary client projection".into(),
            ));
        }
        self.send_observation(output, true)
    }

    fn send_observation(
        &self,
        mut output: CodexTurnOutput,
        progress: bool,
    ) -> Result<(), InterfaceDriverError> {
        if !self.projection_admits(&output) {
            return Ok(());
        }
        self.normalize_output_label(&mut output)?;
        match self.live_stream_rules.disposition(
            &output.label,
            &output.value,
            self.has_live_stream(),
        )? {
            LiveStreamDisposition::Stream => {
                let mut live_stream = self.live_stream.lock().expect("live stream lock");
                live_stream
                    .as_mut()
                    .ok_or_else(|| {
                        InterfaceDriverError::FailedClosed(format!(
                            "Codex transient output '{}' has no native turn stream",
                            output.label
                        ))
                    })?
                    .append_event(self.source_movement_seq, &output.label, &output.value)
                    .map_err(InterfaceDriverError::FailedClosed)
            }
            LiveStreamDisposition::Artifact => {
                let emission = self.materialize_normalized(output, true)?;
                if progress {
                    self.emit.send_progress(emission)
                } else {
                    self.emit.send_turn(emission)
                }
            }
        }
    }

    pub(crate) fn send_with_outgoing_bindings(
        &self,
        mut output: CodexTurnOutput,
        bindings: Vec<octopus_run_authoring::ProviderOutgoingBinding>,
    ) -> Result<(), InterfaceDriverError> {
        if !matches!(output.route, ProviderEmissionRoute::Tool { .. }) {
            return Err(InterfaceDriverError::FailedClosed("outgoing binding authoring requires an ordinary tool emission".into()));
        }
        self.normalize_output_label(&mut output)?;
        let emission = self.materialize_normalized(output, true)?;
        self.emit.sender.send(ProviderStreamEmission::WithOutgoingBindings { emission, bindings })
            .map_err(|_| InterfaceDriverError::Transport("provider binding emission lost its interface life".into()))
    }

    pub(crate) fn send_with_capability_amendment(
        &self,
        mut output: CodexTurnOutput,
        amendment: octopus_run_authoring::ProviderCapabilityAmendment,
    ) -> Result<(), InterfaceDriverError> {
        if !matches!(output.route, ProviderEmissionRoute::Tool { .. }) {
            return Err(InterfaceDriverError::FailedClosed("a capability amendment requires an ordinary tool emission".into()));
        }
        self.normalize_output_label(&mut output)?;
        let emission = self.materialize_normalized(output, true)?;
        self.emit.sender.send(ProviderStreamEmission::WithCapabilityAmendment { emission, amendment })
            .map_err(|_| InterfaceDriverError::Transport("provider amendment emission lost its interface life".into()))
    }

    pub(crate) fn seal_live_stream(
        &self,
        terminal_label: &str,
        terminal_artifact_payload_blake3: &str,
    ) -> Result<Option<ProviderEmission>, InterfaceDriverError> {
        let mut live_stream = self.live_stream.lock().expect("live stream lock");
        let Some(live_stream) = live_stream.as_mut() else {
            return Ok(None);
        };
        let artifact = live_stream
            .seal_terminal(
                self.source_movement_seq,
                terminal_label,
                terminal_artifact_payload_blake3,
            )
            .map_err(InterfaceDriverError::FailedClosed)?;
        Ok(Some(ProviderEmission {
            artifact,
            route: ProviderEmissionRoute::ClientProjection,
        }))
    }

    pub(crate) fn send_terminal(&self, mut output: CodexTurnOutput) -> Result<(), InterfaceDriverError> {
        if !self.projection_admits(&output) {
            return Err(InterfaceDriverError::FailedClosed(
                "descriptor projection removed the native turn terminal output".to_string(),
            ));
        }
        self.normalize_output_label(&mut output)?;
        if self.live_stream_rules.disposition(
            &output.label,
            &output.value,
            self.has_live_stream(),
        )? != LiveStreamDisposition::Artifact
        {
            return Err(InterfaceDriverError::FailedClosed(
                "native turn terminal output cannot be stream-only".to_string(),
            ));
        }
        let terminal_label = output.label.clone();
        if self.has_live_stream() {
            self.advance_continuity(false)?;
        }
        let terminal = self.materialize_normalized(output, false)?;
        let terminal_artifact_payload_blake3 = artifact_payload_blake3(&terminal.artifact)
            .map_err(InterfaceDriverError::FailedClosed)?;
        match self.seal_live_stream(&terminal_label, &terminal_artifact_payload_blake3)? {
            Some(stream) => self.emit.send_terminal_group(stream, terminal),
            None => self.emit.send_turn(terminal),
        }
    }

    pub(crate) fn send_lifecycle(&self, output: LifecyclePlannedOutput) -> Result<(), InterfaceDriverError> {
        self.send(CodexTurnOutput::lifecycle(output))
    }

    pub(crate) fn send_terminal_lifecycle(
        &self,
        output: LifecyclePlannedOutput,
    ) -> Result<(), InterfaceDriverError> {
        self.send_terminal(CodexTurnOutput::lifecycle(output))
    }

    pub(crate) fn send_held_lifecycle(
        &self,
        continuation: &LifecycleContinuation,
    ) -> Result<(), InterfaceDriverError> {
        let (artifact, route) = validate_held_file(self.output_root, continuation)
            .map_err(InterfaceDriverError::FailedClosed)?;
        let manifest = artifact.read_manifest().map_err(|error| {
            InterfaceDriverError::FailedClosed(format!(
                "lifecycle-held terminal manifest is unreadable: {error}"
            ))
        })?;
        let emission = ProviderEmission {
            artifact,
            route: provider_route_from_lifecycle(route),
        };
        if manifest.label != INTERFACE_ARTIFACT_OUTPUT_LABEL {
            return self.emit.send_turn(emission);
        }
        let terminal_artifact_payload_blake3 = artifact_payload_blake3(&emission.artifact)
            .map_err(InterfaceDriverError::FailedClosed)?;
        match self.seal_live_stream(&manifest.label, &terminal_artifact_payload_blake3)? {
            Some(stream) => self.emit.send_terminal_group(stream, emission),
            None => self.emit.send_turn(emission),
        }
    }

    pub(crate) fn send_interrupted_result(
        &self,
        mut activity: Option<CodexTurnOutput>,
        activity_already_admitted: bool,
        mut closure: CodexTurnOutput,
    ) -> Result<(), InterfaceDriverError> {
        let source_continuity = self.continuity_snapshot();
        let native_terminal = source_continuity.as_ref().and_then(|state| state.native_terminal.clone());
        if native_terminal.is_some() {
            if closure.value.get("native_terminal_source_continuity").is_none() {
                closure.value["native_terminal_source_continuity"] = serde_json::to_value(&source_continuity)
                    .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
            }
            closure.value["native_terminal_input_context"] = serde_json::json!(
                self.continuity.lock().expect("continuity lock").pending_input_context
            );
        }
        if self.has_live_stream() {
            self.advance_continuity(false)?;
        }
        if let Some(terminal) = &native_terminal {
            terminal.apply_disposition(&mut closure)?;
            if let Some(activity) = &mut activity {
                terminal.apply_disposition(activity)?;
            }
        }
        let activity = match (activity, activity_already_admitted) {
            (Some(_), true) => {
                return Err(InterfaceDriverError::FailedClosed(
                    "interrupted-result recovery cannot both repeat and acknowledge its activity"
                        .to_string(),
                ));
            }
            (Some(output), false) => {
                self.send(output)?;
                None
            }
            (None, true) => {
                None
            }
            (None, false) => {
                return Err(InterfaceDriverError::FailedClosed(
                    "interrupted-result source has neither a new nor an admitted activity"
                        .to_string(),
                ));
            }
        };
        self.set_interrupted_pending(None);
        let mut retained_outputs = Vec::new();
        if let Some(terminal) = &native_terminal {
            let events = native_terminal_events(&closure.value)?;
            let mut payloads = Vec::new();
            for mut output in terminal.outputs(events)? {
                if !self.projection_admits(&output) {
                    return Err(InterfaceDriverError::FailedClosed("descriptor projection removed retained native completion evidence".into()));
                }
                self.normalize_output_label(&mut output)?;
                let emission = self.materialize_normalized(output, false)?;
                payloads.push(serde_json::from_slice::<serde_json::Value>(
                    &emission.artifact.read_payload_bytes().map_err(|error| InterfaceDriverError::FailedClosed(error.to_string()))?
                ).map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?);
                retained_outputs.push(emission);
            }
            closure.value["retained_native_outputs"] = serde_json::Value::Array(payloads);
        }
        if !self.projection_admits(&closure) {
            return Err(InterfaceDriverError::FailedClosed(
                "descriptor projection removed the required interrupted-result closure".to_string(),
            ));
        }
        self.normalize_output_label(&mut closure)?;
        let closure_label = closure.label.clone();
        let closure = self.materialize_normalized(closure, false)?;
        let closure_artifact_payload_blake3 = artifact_payload_blake3(&closure.artifact)
            .map_err(InterfaceDriverError::FailedClosed)?;
        let stream = self.seal_live_stream(&closure_label, &closure_artifact_payload_blake3)?;
        if stream.is_none() && activity.is_none() {
            return Err(InterfaceDriverError::FailedClosed(
                "interrupted-result closure has neither a sealed stream nor legacy activity"
                    .to_string(),
            ));
        }
        self.emit.send_interrupted_result(activity, stream, retained_outputs, closure)
    }
}

#[cfg(test)]
pub(crate) async fn native_history_receipt_for_test(
    descriptor: &InterfaceDescriptor, output_root: &Path, history: &Path,
    source: CodexContinuityState, session: &[SessionContextArtifact],
    input: &ArtifactRef, source_seq: u64,
) -> ProviderEmission {
    let config = &descriptor.interface_driver_config.as_ref().unwrap().private_config;
    let labels = private_config_string_set(config, "context_output_labels").unwrap();
    let projected = private_config_string_set(config, "client_projection_output_labels").unwrap();
    let rules = LiveStreamRules::from_private_config(config).unwrap();
    let proof = context_refresh::prove_native_history_retirement(history, &source, session, &labels).unwrap().unwrap();
    let mut replacement = source.clone();
    replacement.thread_id = "history-replacement".into();
    replacement.native_terminal = None;
    replacement.native_history_retirement = Some(NativeHistoryRetirement {
        source_thread: source.thread_id, source_generation: source.context_generation,
        replacement_thread: replacement.thread_id.clone(), source_artifact: input.clone(),
        source_movement_seq: source_seq, proof,
    });
    let (emit, mut outputs) = ProviderOutputStream::channel();
    let sink = TurnSink { emit: &emit, output_root, live_stream_root: output_root, provider_interface: &descriptor.id,
        run: &source.run, source_movement_seq: source_seq, attempt: 1, context_output_labels: &labels,
        client_projection_output_labels: &projected, live_stream_rules: &rules,
        live_stream: std::sync::Mutex::new(None), continuity: emit.continuity.clone() };
    sink.set_continuity(replacement);
    let collect = async {
        let ProviderStreamEmission::ContextBoundary { mut emissions, admitted } = outputs.recv().await.unwrap() else { panic!("native history boundary") };
        assert_eq!(emissions.len(), 1);
        admitted.send(Ok(())).unwrap();
        emissions.remove(0)
    };
    let (result, emission) = tokio::join!(sink.admit_native_history_retirement(), collect);
    result.unwrap();
    emission
}

#[cfg(test)]
pub(crate) fn native_terminal_group_for_test(
    descriptor: &InterfaceDescriptor, output_root: &Path, stream_root: &Path,
    original: &CodexContinuityState, native_turn: Option<serde_json::Value>,
    source: &ArtifactRef, source_seq: u64, with_stop: bool,
    input_changes_context: bool,
) -> ProviderStreamEmission {
    let mut retired = original.clone();
    if !with_stop { retired.interrupted_pending = retired.pending.take(); }
    retired.native_terminal = native_turn.map(|turn| NativeTerminalEvidence { thread_id: retired.thread_id.clone(), turn });
    let enriched = with_stop && original.native_terminal.is_none() && retired.native_terminal.is_some();
    let (emit, mut outputs) = ProviderOutputStream::channel();
    emit.continuity.lock().unwrap().continuity = Some(retired);
    emit.continuity.lock().unwrap().pending_input_context = input_changes_context;
    let config = &descriptor.interface_driver_config.as_ref().unwrap().private_config;
    let labels = private_config_string_set(config, "context_output_labels").unwrap();
    let projected = private_config_string_set(config, "client_projection_output_labels").unwrap();
    let rules = LiveStreamRules::from_private_config(config).unwrap();
    let sink = TurnSink { emit: &emit, output_root, live_stream_root: stream_root, provider_interface: &descriptor.id,
        run: &original.run, source_movement_seq: source_seq, attempt: 1, context_output_labels: &labels,
        client_projection_output_labels: &projected, live_stream_rules: &rules, live_stream: std::sync::Mutex::new(None), continuity: emit.continuity.clone() };
    sink.bind_live_stream(&original.thread_id, "turn-a").unwrap();
    let mut closure = CodexTurnOutput::interrupted_result_closed("tool_result", "call-a".into(), original.thread_id.clone(), "turn-a".into(), false, source);
    if !with_stop || enriched {
        closure.value["native_terminal_source_continuity"] = serde_json::to_value(original).unwrap();
        closure.value["result_origin"] = serde_json::json!(if enriched { "interrupted_turn_completion" } else { "native_turn_completion" });
        closure.value["terminal_evidence_source"] = serde_json::json!("thread/read");
    }
    sink.send_interrupted_result(Some(CodexTurnOutput::dynamic_tool_completed_after_interrupt("formed_interfaces".into(), "coder.exec".into(), "call-a".into(), original.thread_id.clone(), "turn-a".into(), false, None,
        Some(octopus_tool_handlers::ToolResultError { code: "tool_effect_indeterminate".into(), message: "uncertain exact effect".into(), details: None }))), false, closure).unwrap();
    outputs.try_recv().unwrap()
}

pub(crate) fn emit_lifecycle_trigger(
    sink: &TurnSink<'_>,
    lifecycle: Option<&LifecycleBinding>,
    emitter_run: &str,
    trigger: LifecycleTrigger,
) -> Result<bool, InterfaceDriverError> {
    let Some(lifecycle) = lifecycle else {
        return Ok(false);
    };
    let trigger = attach_lifecycle_provider_continuity(sink, emitter_run, trigger)?;
    let plan = lifecycle
        .plan_trigger(emitter_run, trigger)
        .map_err(InterfaceDriverError::FailedClosed)?;
    for output in plan.reactive {
        sink.send_lifecycle(output)?;
    }
    if let Some(output) = plan.blocking {
        sink.send_lifecycle(output)?;
        return Ok(true);
    }
    Ok(false)
}

pub(crate) fn attach_lifecycle_provider_continuity(
    sink: &TurnSink<'_>,
    emitter_run: &str,
    mut trigger: LifecycleTrigger,
) -> Result<LifecycleTrigger, InterfaceDriverError> {
    let continuity = sink.continuity_snapshot().ok_or_else(|| {
        InterfaceDriverError::FailedClosed(format!(
            "run '{emitter_run}' cannot emit lifecycle event '{}' without durable provider continuity",
            trigger.event.as_str()
        ))
    })?;
    if continuity.schema_version != 1
        || continuity.run != emitter_run
        || continuity.provider_interface != *sink.provider_interface
    {
        return Err(InterfaceDriverError::FailedClosed(format!(
            "run '{emitter_run}' lifecycle trigger continuity conflicts with its provider invocation"
        )));
    }
    let data = trigger.data.as_object_mut().ok_or_else(|| {
        InterfaceDriverError::Protocol("lifecycle trigger data must be an object".to_string())
    })?;
    if data.contains_key(LIFECYCLE_PROVIDER_CONTINUITY_FIELD) {
        return Err(InterfaceDriverError::FailedClosed(format!(
            "lifecycle trigger '{}' attempted to override provider continuity",
            trigger.event.as_str()
        )));
    }
    data.insert(
        LIFECYCLE_PROVIDER_CONTINUITY_FIELD.to_string(),
        serde_json::to_value(continuity).map_err(|error| {
            InterfaceDriverError::Protocol(format!(
                "lifecycle provider continuity did not encode: {error}"
            ))
        })?,
    );
    Ok(trigger)
}

pub(crate) fn restore_lifecycle_provider_continuity(
    sink: &TurnSink<'_>,
    runtime: &RuntimeInvocationMetadata,
    event: &LifecycleEventArtifact,
) -> Result<(), InterfaceDriverError> {
    if event.emitter_run != runtime.run.as_str() {
        return Ok(());
    }
    let continuity: CodexContinuityState = serde_json::from_value(
        event
            .data
            .get(LIFECYCLE_PROVIDER_CONTINUITY_FIELD)
            .cloned()
            .ok_or_else(|| {
                InterfaceDriverError::FailedClosed(format!(
                    "lifecycle event '{}' has no provider continuity evidence",
                    event.event_id
                ))
            })?,
    )
    .map_err(|error| {
        InterfaceDriverError::Protocol(format!(
            "lifecycle event '{}' provider continuity is malformed: {error}",
            event.event_id
        ))
    })?;
    if continuity.schema_version != 1
        || continuity.run != runtime.run.as_str()
        || continuity.provider_interface != runtime.interface
    {
        return Err(InterfaceDriverError::FailedClosed(format!(
            "lifecycle event '{}' provider continuity conflicts with its emitter invocation",
            event.event_id
        )));
    }
    sink.set_continuity(continuity);
    Ok(())
}

pub(crate) fn lifecycle_handler_model_prompt(
    instruction: &str,
    event: &LifecycleEventArtifact,
) -> Result<String, InterfaceDriverError> {
    let contract = match event.event.positive_decision() {
        Some(decision) if matches!(event.mode, crate::lifecycle::LifecycleMode::Sync) => {
            serde_json::json!({
                "decision": decision,
                "reason": "optional non-empty string",
                "additional_context_artifacts": [],
                "replacement_artifact": null,
            })
        }
        _ => serde_json::json!({
            "result": "JSON value",
        }),
    };
    let contract = serde_json::to_string(&contract).map_err(|error| {
        InterfaceDriverError::Protocol(format!(
            "lifecycle handler model contract did not encode: {error}"
        ))
    })?;
    Ok(format!(
        "{instruction}\n\nReturn exactly one JSON object and no surrounding text. Output contract: {contract}"
    ))
}

pub(crate) fn emit_lifecycle_trigger_set(
    sink: &TurnSink<'_>,
    lifecycle: Option<&LifecycleBinding>,
    emitter_run: &str,
    triggers: Vec<LifecycleTrigger>,
) -> Result<bool, InterfaceDriverError> {
    let Some(lifecycle) = lifecycle else {
        return Ok(false);
    };
    let mut blocking = None;
    for trigger in triggers {
        let trigger = attach_lifecycle_provider_continuity(sink, emitter_run, trigger)?;
        let plan = lifecycle
            .plan_trigger(emitter_run, trigger)
            .map_err(InterfaceDriverError::FailedClosed)?;
        for output in plan.reactive {
            sink.send_lifecycle(output)?;
        }
        if let Some(output) = plan.blocking {
            if blocking.replace(output).is_some() {
                return Err(InterfaceDriverError::FailedClosed(format!(
                    "run '{emitter_run}' selected more than one sync lifecycle handler for one physical continuation; author an explicit aggregate decision edge"
                )));
            }
        }
    }
    if let Some(output) = blocking {
        sink.send_lifecycle(output)?;
        return Ok(true);
    }
    Ok(false)
}

pub(crate) fn hold_lifecycle_output(
    sink: &TurnSink<'_>,
    output: CodexTurnOutput,
) -> Result<LifecycleContinuation, InterfaceDriverError> {
    let terminal = output.label == INTERFACE_ARTIFACT_OUTPUT_LABEL;
    let emission = if terminal {
        sink.materialize_without_boundary(output)?
    } else {
        sink.materialize(output)?
    }
    .ok_or_else(|| {
        InterfaceDriverError::FailedClosed(
            "descriptor projection removed a lifecycle-held provider output".to_string(),
        )
    })?;
    let artifact =
        artifact_ref_for_file(&emission.artifact).map_err(InterfaceDriverError::FailedClosed)?;
    Ok(LifecycleContinuation::HeldEmission {
        artifact,
        manifest_path: emission.artifact.manifest_path().to_path_buf(),
        route: lifecycle_route_from_provider(emission.route)?,
    })
}

pub(crate) fn lifecycle_route_from_provider(
    route: ProviderEmissionRoute,
) -> Result<LifecycleRoute, InterfaceDriverError> {
    Ok(match route {
        ProviderEmissionRoute::ClientProjection => LifecycleRoute::ClientProjection,
        ProviderEmissionRoute::ClientProjectionWithVerifiedInputs { inputs } => {
            if inputs
                .iter()
                .any(|input| input.also_deliver_to_tool.is_some() || input.also_deliver_to_interface.is_some())
            {
                return Err(InterfaceDriverError::FailedClosed(
                    "lifecycle-held projection cannot discard a verified second recipient"
                        .to_string(),
                ));
            }
            LifecycleRoute::ClientProjectionWithVerifiedInputs {
                inputs: inputs
                    .into_iter()
                    .map(|input| LifecycleVerifiedInput {
                        artifact_label: input.artifact_label,
                        required_verified_artifact: input.required_verified_artifact,
                    })
                    .collect(),
            }
        }
        ProviderEmissionRoute::Tool { tool } => LifecycleRoute::Tool { tool },
        ProviderEmissionRoute::Interface { interface } => LifecycleRoute::Interface { interface },
        ProviderEmissionRoute::Operation { interface, result_label } => {
            LifecycleRoute::Operation { interface, result_label }
        }
    })
}

#[derive(Default)]
pub(crate) struct PendingClientDelta {
    pub(crate) label: Option<String>,
    pub(crate) text_field: Option<&'static str>,
    pub(crate) payload: serde_json::Map<String, serde_json::Value>,
    pub(crate) text: String,
}

pub(crate) fn flush_pending_delta(
    sink: &TurnSink<'_>,
    pending: &mut PendingClientDelta,
    seq: &mut u64,
) -> Result<(), InterfaceDriverError> {
    if pending.text.is_empty() {
        return Ok(());
    }
    let mut chunk = std::mem::take(pending);
    let label = chunk.label.ok_or_else(|| {
        InterfaceDriverError::Protocol("pending client delta has no output label".to_string())
    })?;
    let text_field = chunk.text_field.ok_or_else(|| {
        InterfaceDriverError::Protocol("pending client delta has no text field".to_string())
    })?;
    chunk.payload.insert(
        text_field.to_string(),
        serde_json::Value::String(chunk.text),
    );
    chunk
        .payload
        .insert("seq".to_string(), serde_json::Value::Number((*seq).into()));
    *seq += 1;
    sink.send_progress(CodexTurnOutput::client(
        label,
        serde_json::Value::Object(chunk.payload),
    ))
}

pub(crate) fn queue_client_delta(
    sink: &TurnSink<'_>,
    pending: &mut PendingClientDelta,
    seq: &mut u64,
    output: CodexTurnOutput,
    text_field: &'static str,
) -> Result<bool, InterfaceDriverError> {
    if output.route != ProviderEmissionRoute::ClientProjection {
        return Err(InterfaceDriverError::Protocol(
            "only client-projection outputs may enter the live delta coalescer".to_string(),
        ));
    }
    let label = output.label;
    let mut payload = output.value.as_object().cloned().ok_or_else(|| {
        InterfaceDriverError::Protocol(format!(
            "client delta '{label}' must serialize as a JSON object"
        ))
    })?;
    let text = payload
        .remove(text_field)
        .and_then(|value| value.as_str().map(str::to_string))
        .ok_or_else(|| {
            InterfaceDriverError::Protocol(format!(
                "client delta '{label}' must carry string field '{text_field}'"
            ))
        })?;
    payload.remove("seq");
    payload.insert(
        "lifecycle".to_string(),
        serde_json::Value::String("streaming".to_string()),
    );
    let same_stream = pending.label.as_deref() == Some(label.as_str())
        && pending.text_field == Some(text_field)
        && pending.payload == payload;
    if !pending.text.is_empty() && !same_stream {
        flush_pending_delta(sink, pending, seq)?;
    }
    let started = pending.text.is_empty();
    if started {
        pending.label = Some(label);
        pending.text_field = Some(text_field);
        pending.payload = payload;
    }
    pending.text.push_str(&text);
    Ok(started && !text.is_empty())
}

pub(crate) fn server_notification_matches_turn(
    notification: &ServerNotification,
    thread_id: &str,
    turn_id: &str,
) -> Result<bool, InterfaceDriverError> {
    let value = serde_json::to_value(notification).map_err(|error| {
        InterfaceDriverError::Protocol(format!(
            "app-server notification could not be inspected for turn provenance: {error}"
        ))
    })?;
    if let Some(value) = value.pointer("/params/threadId") {
        let candidate = value.as_str().ok_or_else(|| {
            InterfaceDriverError::Protocol(
                "app-server notification carries a non-string threadId".to_string(),
            )
        })?;
        if candidate != thread_id {
            return Ok(false);
        }
    }
    for pointer in ["/params/turnId", "/params/turn/id"] {
        let Some(value) = value.pointer(pointer) else {
            continue;
        };
        if value.is_null() {
            continue;
        }
        let candidate = value.as_str().ok_or_else(|| {
            InterfaceDriverError::Protocol(format!(
                "app-server notification carries a non-string turn id at '{pointer}'"
            ))
        })?;
        return Ok(candidate == turn_id);
    }
    Ok(true)
}

pub(crate) fn native_frame_matches_turn(frame: &serde_json::Value, thread_id: &str, turn_id: &str) -> bool {
    frame.pointer("/params/threadId").and_then(serde_json::Value::as_str) == Some(thread_id)
        && frame.pointer("/params/turnId").or_else(|| frame.pointer("/params/turn/id"))
            .and_then(serde_json::Value::as_str) == Some(turn_id)
}

pub(crate) fn server_notification_matches_thread(
    notification: &ServerNotification,
    thread_id: &str,
) -> Result<bool, InterfaceDriverError> {
    let value = serde_json::to_value(notification).map_err(|error| {
        InterfaceDriverError::Protocol(format!(
            "app-server notification could not be inspected for thread provenance: {error}"
        ))
    })?;
    let Some(value) = value.pointer("/params/threadId") else {
        return Ok(true);
    };
    let candidate = value.as_str().ok_or_else(|| {
        InterfaceDriverError::Protocol(
            "app-server notification carries a non-string threadId".to_string(),
        )
    })?;
    Ok(candidate == thread_id)
}

pub(crate) fn intentionally_unprojected_server_notification(notification: &ServerNotification) -> bool {
    matches!(
        notification,
        ServerNotification::ThreadStarted(_)
            | ServerNotification::ThreadStatusChanged(_)
            | ServerNotification::ThreadArchived(_)
            | ServerNotification::ThreadDeleted(_)
            | ServerNotification::ThreadUnarchived(_)
            | ServerNotification::ThreadClosed(_)
            | ServerNotification::SkillsChanged(_)
            | ServerNotification::ThreadNameUpdated(_)
            | ServerNotification::ThreadGoalCleared(_)
            | ServerNotification::ThreadSettingsUpdated(_)
            | ServerNotification::RawResponseItemCompleted(_)
            | ServerNotification::TurnModerationMetadata(_)
            | ServerNotification::AppListUpdated(_)
            | ServerNotification::RemoteControlStatusChanged(_)
            | ServerNotification::ExternalAgentConfigImportProgress(_)
            | ServerNotification::ExternalAgentConfigImportCompleted(_)
            | ServerNotification::FsChanged(_)
            | ServerNotification::FuzzyFileSearchSessionUpdated(_)
            | ServerNotification::FuzzyFileSearchSessionCompleted(_)
            | ServerNotification::AccountLoginCompleted(_)
            | ServerNotification::AccountUpdated(_)
    )
}
