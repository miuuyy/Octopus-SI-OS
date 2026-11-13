use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ContextWindowUsage {
    pub(crate) used_tokens: u64,
    pub(crate) context_window: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub(crate) struct TokenUsageBreakdownEvidence {
    pub(crate) total_tokens: u64,
    pub(crate) input_tokens: u64,
    pub(crate) cached_input_tokens: u64,
    pub(crate) uncached_input_tokens: u64,
    pub(crate) output_tokens: u64,
    pub(crate) reasoning_output_tokens: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub(crate) struct ProviderTokenUsageEvidence {
    pub(crate) cumulative: TokenUsageBreakdownEvidence,
    pub(crate) current_context: TokenUsageBreakdownEvidence,
    pub(crate) context_window: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ModelResultProjection {
    pub(crate) max_tokens: usize,
}

impl ModelResultProjection {
    pub(crate) fn from_private_config(
        config: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Self, InterfaceDriverError> {
        let value = config
            .get("model_result_projection")
            .cloned()
            .ok_or_else(|| {
                InterfaceDriverError::FailedClosed(
                    "codex_app_server descriptor declares no model_result_projection".into(),
                )
            })?;
        serde_json::from_value::<Self>(value).map_err(|error| {
            InterfaceDriverError::Protocol(format!("interface_driver_config.private_config.model_result_projection is malformed: {error}"))
        })?.validate()
    }

    pub(crate) fn validate(self) -> Result<Self, InterfaceDriverError> {
        if self.max_tokens < 512 {
            return Err(InterfaceDriverError::FailedClosed(
                "interface_driver_config.private_config.model_result_projection.max_tokens must be at least 512"
                    .to_string(),
            ));
        }
        Ok(self)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum TransportStateScope {
    Run(String),
}

impl TransportStateScope {
    pub(crate) fn from_run(run: &octopus_interfaces::RunId) -> TransportStateScope {
        TransportStateScope::Run(run.as_str().to_string())
    }
}

#[derive(Default)]
pub(crate) struct TransportState {
    pub(crate) context_generation: u64,
    pub(crate) hooks_catalog_emitted: bool,
    pub(crate) thread_id: Option<String>,
    pub(crate) thread_shape_fingerprint: Option<String>,
    pub(crate) static_context_fingerprint: Option<String>,
    pub(crate) rollout_path: Option<PathBuf>,
}

pub(crate) const CODEX_CONTINUITY_FIELD: &str = "codex_continuity";
pub(crate) const LIFECYCLE_PROVIDER_CONTINUITY_FIELD: &str = "provider_continuity";

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CodexContinuityState {
    pub(crate) schema_version: u32,
    pub(crate) provider_interface: InterfaceId,
    pub(crate) run: String,
    pub(crate) thread_id: String,
    pub(crate) thread_shape_fingerprint: String,
    pub(crate) context_generation: u64,
    pub(crate) static_context_fingerprint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) turn_input: Option<ArtifactRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) pending: Option<CodexPendingContinuity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) interrupted_pending: Option<CodexPendingContinuity>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) parallel_pending: BTreeMap<String, ParallelPendingCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) tool_operator_request: Option<ToolOperatorContinuity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) native_terminal: Option<NativeTerminalEvidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) context_refresh: Option<ContextRefresh>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) native_history_retirement: Option<NativeHistoryRetirement>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ParallelPendingCall {
    pub(crate) pending: CodexPendingContinuity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) tool_operator_request: Option<ToolOperatorContinuity>,
}

pub(crate) fn pending_request_key(pending: &CodexPendingContinuity) -> String {
    match pending {
        CodexPendingContinuity::DynamicTool { call_id, .. }
        | CodexPendingContinuity::InterfaceOperation { call_id, .. } => format!("tool:{call_id}"),
        CodexPendingContinuity::Operator { request_key, .. } => request_key.clone(),
    }
}

impl CodexContinuityState {
    pub(crate) fn outstanding(&self) -> impl Iterator<Item = &CodexPendingContinuity> {
        self.pending
            .iter()
            .chain(self.interrupted_pending.iter())
            .chain(self.parallel_pending.values().map(|call| &call.pending))
    }

    pub(crate) fn outstanding_call(&self, key: &str) -> Option<&CodexPendingContinuity> {
        self.outstanding().find(|pending| pending_request_key(pending) == key)
    }

    pub(crate) fn outstanding_len(&self) -> usize {
        self.outstanding().count()
    }

    pub(crate) fn outstanding_calls(&self) -> (bool, BTreeMap<String, ParallelPendingCall>) {
        let mut calls = self.parallel_pending.clone();
        if let Some(head) = self.pending.as_ref().or(self.interrupted_pending.as_ref()) {
            calls.insert(
                pending_request_key(head),
                ParallelPendingCall {
                    pending: head.clone(),
                    tool_operator_request: self.tool_operator_request.clone(),
                },
            );
        }
        (self.interrupted_pending.is_some(), calls)
    }

    pub(crate) fn store_outstanding(
        &mut self,
        interrupted: bool,
        mut calls: BTreeMap<String, ParallelPendingCall>,
    ) {
        self.pending = None;
        self.interrupted_pending = None;
        self.tool_operator_request = None;
        if let Some((_, head)) = calls.pop_first() {
            if interrupted {
                self.interrupted_pending = Some(head.pending);
            } else {
                self.pending = Some(head.pending);
            }
            self.tool_operator_request = head.tool_operator_request;
        }
        self.parallel_pending = calls;
    }

    pub(crate) fn canonical(mut self) -> Self {
        let (interrupted, calls) = self.outstanding_calls();
        self.store_outstanding(interrupted, calls);
        self
    }

    pub(crate) fn admit_outstanding(
        &mut self,
        pending: CodexPendingContinuity,
    ) -> Result<(), InterfaceDriverError> {
        if self.interrupted_pending.is_some() {
            return Err(InterfaceDriverError::FailedClosed(
                "a native call cannot join an interrupted native turn".to_string(),
            ));
        }
        if let Some(head) = self.pending.as_ref()
            && (!parallel_pending_admissible(head)
                || !parallel_pending_admissible(&pending)
                || pending_turn_id(head) != pending_turn_id(&pending))
        {
            return Err(InterfaceDriverError::FailedClosed(
                "only ordinary external calls of one native turn may wait together".to_string(),
            ));
        }
        let (_, mut calls) = self.outstanding_calls();
        if calls
            .insert(
                pending_request_key(&pending),
                ParallelPendingCall {
                    pending,
                    tool_operator_request: None,
                },
            )
            .is_some()
        {
            return Err(InterfaceDriverError::FailedClosed(
                "app-server repeated one unresolved native call inside the same turn".to_string(),
            ));
        }
        self.store_outstanding(false, calls);
        Ok(())
    }

    pub(crate) fn release_ended_turn(&mut self) {
        if self.interrupted_pending.is_none() {
            self.store_outstanding(false, BTreeMap::new());
        }
    }

    pub(crate) fn settle_outstanding(&mut self, key: &str) -> Option<CodexPendingContinuity> {
        let (interrupted, mut calls) = self.outstanding_calls();
        let settled = calls.remove(key)?;
        self.store_outstanding(interrupted, calls);
        Some(settled.pending)
    }

    pub(crate) fn focused(&self, key: &str) -> Option<Self> {
        let (interrupted, mut calls) = self.outstanding_calls();
        let focus = calls.remove(key)?;
        let mut state = self.clone();
        state.parallel_pending = calls;
        state.pending = None;
        state.interrupted_pending = None;
        if interrupted {
            state.interrupted_pending = Some(focus.pending);
        } else {
            state.pending = Some(focus.pending);
        }
        state.tool_operator_request = focus.tool_operator_request;
        Some(state)
    }

    pub(crate) fn operator_request_for(&self, key: &str) -> Option<&ToolOperatorContinuity> {
        match self.pending.as_ref().or(self.interrupted_pending.as_ref()) {
            Some(head) if pending_request_key(head) == key => self.tool_operator_request.as_ref(),
            _ => self
                .parallel_pending
                .get(key)
                .and_then(|call| call.tool_operator_request.as_ref()),
        }
    }

    pub(crate) fn held_operator_call(
        &self,
        held: impl Fn(&ToolOperatorContinuity) -> bool,
    ) -> Option<String> {
        let (_, calls) = self.outstanding_calls();
        calls.into_iter().find_map(|(key, call)| {
            call.tool_operator_request.as_ref().is_some_and(&held).then_some(key)
        })
    }

    pub(crate) fn set_operator_request(
        &mut self,
        key: &str,
        request: Option<ToolOperatorContinuity>,
    ) -> Result<(), InterfaceDriverError> {
        let (interrupted, mut calls) = self.outstanding_calls();
        calls
            .get_mut(key)
            .ok_or_else(|| {
                InterfaceDriverError::FailedClosed(
                    "operator continuation names no outstanding native call".to_string(),
                )
            })?
            .tool_operator_request = request;
        self.store_outstanding(interrupted, calls);
        Ok(())
    }

    pub(crate) fn validate_parallel_pending(&self) -> Result<(), InterfaceDriverError> {
        if self.parallel_pending.is_empty() {
            return Ok(());
        }
        let refused = |message: &str| InterfaceDriverError::FailedClosed(message.to_string());
        let head = self
            .pending
            .as_ref()
            .or(self.interrupted_pending.as_ref())
            .ok_or_else(|| refused("Codex parallel calls have no native turn head"))?;
        let head_key = pending_request_key(head);
        if !parallel_pending_admissible(head) || self.native_terminal.is_some() {
            return Err(refused(
                "Codex parallel calls cannot share a native operation or a native terminal proof",
            ));
        }
        for (key, call) in &self.parallel_pending {
            validate_pending_continuity_coordinates(Some(&call.pending), "parallel pending")?;
            if *key <= head_key
                || *key != pending_request_key(&call.pending)
                || !parallel_pending_admissible(&call.pending)
                || pending_turn_id(&call.pending) != pending_turn_id(head)
            {
                return Err(refused(
                    "Codex parallel call is not one ordered external call of the head native turn",
                ));
            }
        }
        Ok(())
    }
}

pub(crate) fn parallel_pending_admissible(pending: &CodexPendingContinuity) -> bool {
    match pending {
        CodexPendingContinuity::DynamicTool { native_operation, .. } => native_operation.is_none(),
        CodexPendingContinuity::InterfaceOperation { .. } => true,
        CodexPendingContinuity::Operator { .. } => false,
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeTerminalEvidence {
    pub(crate) thread_id: String,
    pub(crate) turn: serde_json::Value,
}

impl NativeTerminalEvidence {
    pub(crate) fn exact_turn(
        &self,
        thread_id: &str,
        turn_id: Option<&str>,
    ) -> Result<octopus_appserver_proto::Turn, InterfaceDriverError> {
        let turn: octopus_appserver_proto::Turn = serde_json::from_value(self.turn.clone())
            .map_err(|error| InterfaceDriverError::Protocol(format!("native terminal turn is malformed: {error}")))?;
        if self.thread_id != thread_id
            || turn.id.trim().is_empty()
            || turn_id.is_some_and(|expected| turn.id != expected)
            || turn.status == TurnStatus::InProgress
            || turn.items_view != TurnItemsView::Full
        {
            return Err(InterfaceDriverError::FailedClosed(
                "native terminal evidence does not prove the exact closed turn with loaded items".into(),
            ));
        }
        Ok(turn)
    }

    pub(crate) fn outputs(&self, events: &[serde_json::Value]) -> Result<Vec<CodexTurnOutput>, InterfaceDriverError> {
        let turn = self.exact_turn(&self.thread_id, None)?;
        let status = serde_json::to_value(&turn.status).map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
        if events.iter().any(|frame| frame["method"] == "turn/completed" && frame.pointer("/params/turn/status") != Some(&status)) {
            return Err(InterfaceDriverError::FailedClosed("native terminal notification conflicts with the exact thread/read evidence".into()));
        }
        let mut outputs = Vec::new();
        let finals = turn.items.iter().filter_map(|item| match item {
            ThreadItem::AgentMessage { id, text, phase: Some(MessagePhase::FinalAnswer) } => Some((id, text)),
            _ => None,
        }).collect::<Vec<_>>();
        if !finals.is_empty() || turn.status == TurnStatus::Completed {
            let (id, text) = completed_turn_final_message(&turn)?;
            let mut output = CodexTurnOutput::final_message(&self.thread_id, &turn.id, id, text, None);
            if let Some(completed_at_ms) = self.turn.get("items").and_then(serde_json::Value::as_array)
                .and_then(|items| items.iter().find(|item| item.get("id").and_then(serde_json::Value::as_str) == Some(id)))
                .and_then(|item| item.get("completedAtMs"))
                .and_then(serde_json::Value::as_i64)
            {
                output.value["completed_at_ms"] = serde_json::json!(completed_at_ms);
            }
            output.value["native_terminal_status"] = serde_json::to_value(turn.status)
                .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
            output.value["native_completed_at"] = serde_json::to_value(turn.completed_at)
                .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
            outputs.push(output);
        }
        outputs.push(CodexTurnOutput::client(TURN_COMPLETED_LABEL, serde_json::json!({
            "threadId": self.thread_id,
            "turn": self.turn,
            "retained_native_events": events,
        })));
        Ok(outputs)
    }

    pub(crate) fn apply_disposition(&self, output: &mut CodexTurnOutput) -> Result<(), InterfaceDriverError> {
        let turn = self.exact_turn(&self.thread_id, output.value.get("turn_id").and_then(serde_json::Value::as_str))?;
        output.value["turn_interrupted"] = serde_json::Value::Bool(turn.status == TurnStatus::Interrupted);
        output.value["native_terminal_status"] = serde_json::to_value(turn.status)
            .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
        output.value["result_disposition"] = serde_json::json!("durably_admitted_after_native_terminal");
        Ok(())
    }
}

pub(crate) async fn read_exact_terminal_turn(
    handle: &AppServerRequestHandle,
    request_ids: &std::sync::atomic::AtomicI64,
    thread_id: &str,
    turn_id: &str,
) -> Result<Option<NativeTerminalEvidence>, String> {
    let response: ThreadReadResponse = handle.request_typed(ClientRequest::ThreadRead {
        request_id: RequestId::Integer(request_ids.fetch_add(1, std::sync::atomic::Ordering::SeqCst)),
        params: ThreadReadParams { thread_id: thread_id.into(), include_turns: false },
    }).await.map_err(|error| format!("exact terminal thread/read failed: {error}"))?;
    if response.thread.id != thread_id {
        return Err("terminal thread/read returned a different thread".into());
    }
    let mut turn = read_native_turn(handle, request_ids, thread_id, turn_id)
        .await.map_err(|error| error.to_string())?
        .ok_or("terminal thread/turns/list did not contain the exact turn")?;
    if turn.status == TurnStatus::InProgress {
        return Ok(None);
    }
    let entries = native_history::read_native_turn_entries(handle, request_ids, thread_id, turn_id)
        .await.map_err(|error| error.to_string())?;
    let timings = entries.iter()
        .map(|entry| (entry.started_at_ms, entry.completed_at_ms))
        .collect::<Vec<_>>();
    turn.items = entries.into_iter().map(|entry| entry.item).collect();
    turn.items_view = TurnItemsView::Full;
    let mut turn = serde_json::to_value(turn).map_err(|error| error.to_string())?;
    let items = turn.get_mut("items").and_then(serde_json::Value::as_array_mut)
        .ok_or("terminal turn encoded without items")?;
    for (item, (started_at_ms, completed_at_ms)) in items.iter_mut().zip(timings) {
        let item = item.as_object_mut().ok_or("terminal turn item encoded as a non-object")?;
        item.insert("startedAtMs".into(), serde_json::json!(started_at_ms));
        item.insert("completedAtMs".into(), serde_json::json!(completed_at_ms));
    }
    let evidence = NativeTerminalEvidence {
        thread_id: thread_id.into(),
        turn,
    };
    evidence.exact_turn(thread_id, Some(turn_id)).map_err(|error| error.to_string())?;
    Ok(Some(evidence))
}

pub(crate) fn take_native_terminal_events(
    client: &mut AppServerClient,
    terminal: &NativeTerminalEvidence,
    thread_id: &str,
    turn_id: &str,
) -> Result<Vec<serde_json::Value>, InterfaceDriverError> {
    let turn = terminal.exact_turn(thread_id, Some(turn_id))?;
    let mut deferred = Vec::new();
    let mut events = Vec::new();
    let mut terminal_conflict = false;
    for event in client.take_ready_events() {
        match event {
            AppServerEvent::ServerNotification(notification) => {
                let frame = serde_json::to_value(&notification).map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
                if !native_frame_matches_turn(&frame, thread_id, turn_id) {
                    deferred.push(AppServerEvent::ServerNotification(notification));
                    continue;
                }
                if let ServerNotification::TurnCompleted(completed) = &notification {
                    if completed.turn.status != turn.status {
                        terminal_conflict = true;
                        deferred.push(AppServerEvent::ServerNotification(notification));
                        continue;
                    }
                }
                events.push(frame);
            }
            AppServerEvent::ServerRequest(request) => {
                let frame = serde_json::to_value(&request).map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
                if native_frame_matches_turn(&frame, thread_id, turn_id) {
                    events.push(serde_json::json!({"quarantined_server_request": frame}));
                } else {
                    deferred.push(AppServerEvent::ServerRequest(request));
                }
            }
            event => deferred.push(event),
        }
    }
    client.restore_ready_events(deferred);
    if terminal_conflict {
        return Err(InterfaceDriverError::FailedClosed("native terminal notification conflicts with the exact thread/read evidence".into()));
    }
    Ok(events)
}
