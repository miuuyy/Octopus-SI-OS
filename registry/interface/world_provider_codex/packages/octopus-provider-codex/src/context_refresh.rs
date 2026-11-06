use super::*;

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ContextRefresh {
    source_thread: String,
    source_shape: String,
    source_static: String,
    source_generation: u64,
    pending: CodexPendingContinuity,
    result_artifact: ArtifactRef,
    result_movement: u64,
    completed_output: serde_json::Value,
    target_shape: String,
    target_static: String,
    terminal: Option<NativeTerminalEvidence>,
    retired: bool,
    replacement_thread: Option<String>,
    started_turn: Option<String>,
}

impl ContextRefresh {
    pub(super) fn validate(
        &self,
        state: &CodexContinuityState,
    ) -> Result<(), InterfaceDriverError> {
        if self.source_thread.trim().is_empty()
            || self.source_thread.trim() != self.source_thread
            || [
                &self.source_shape,
                &self.source_static,
                &self.target_shape,
                &self.target_static,
            ]
            .iter()
            .any(|hash| !is_sha256_hex(hash))
            || self.source_shape == self.target_shape
            || self.result_movement == 0
            || self.source_generation >= state.context_generation
        {
            return Err(InterfaceDriverError::FailedClosed(
                "provider context refresh has invalid source or target coordinates".into(),
            ));
        }
        validate_pending_continuity_coordinates(Some(&self.pending), "context refresh source")?;
        if !interrupted_pending_has_external_result(&self.pending) {
            return Err(InterfaceDriverError::FailedClosed(
                "provider context refresh requires one completed external effect".into(),
            ));
        }
        if let Some(terminal) = &self.terminal {
            terminal.exact_turn(&self.source_thread, Some(pending_turn_id(&self.pending)))?;
        }
        if (self.retired && self.terminal.is_none())
            || (self.replacement_thread.is_some() && !self.retired)
            || self.started_turn.as_ref().is_some_and(|turn| {
                self.replacement_thread.is_none() || turn.trim().is_empty() || turn.trim() != turn
            })
        {
            return Err(InterfaceDriverError::FailedClosed(
                "provider context refresh has an unproven transition phase".into(),
            ));
        }
        let call_id = match &self.pending {
            CodexPendingContinuity::DynamicTool { call_id, .. }
            | CodexPendingContinuity::InterfaceOperation { call_id, .. } => {
                native_pending_operation(&self.pending)?
                    .map(|operation| operation.call_id)
                    .unwrap_or_else(|| call_id.clone())
            }
            _ => unreachable!("external pending checked above"),
        };
        if self
            .completed_output
            .get("call_id")
            .and_then(serde_json::Value::as_str)
            != Some(call_id.as_str())
            || self
                .completed_output
                .get("type")
                .and_then(serde_json::Value::as_str)
                != Some("function_call_output")
        {
            return Err(InterfaceDriverError::FailedClosed(
                "context refresh output is detached from its exact completed call".into(),
            ));
        }
        serde_json::from_value::<octopus_protocol::models::ResponseItem>(
            self.completed_output.clone(),
        )
        .map_err(|error| {
            InterfaceDriverError::FailedClosed(format!(
                "context refresh output is malformed: {error}"
            ))
        })?;
        match &self.replacement_thread {
            None if state.thread_id == self.source_thread
                && state.thread_shape_fingerprint == self.source_shape
                && state.static_context_fingerprint == self.source_static
                && state.pending.is_none() =>
            {
                Ok(())
            }
            Some(thread)
                if self.terminal.is_some()
                    && thread != &self.source_thread
                    && thread == &state.thread_id
                    && state.thread_shape_fingerprint == self.target_shape
                    && state.static_context_fingerprint == self.target_static =>
            {
                Ok(())
            }
            _ => Err(InterfaceDriverError::FailedClosed(
                "provider context refresh is detached from its current private thread".into(),
            )),
        }
    }

    pub(super) fn connects(
        &self,
        source: &CodexContinuityState,
        target: &CodexContinuityState,
    ) -> bool {
        self.source_thread == source.thread_id
            && self.source_shape == source.thread_shape_fingerprint
            && self.source_static == source.static_context_fingerprint
            && self.source_generation >= source.context_generation
            && source.pending.as_ref() == Some(&self.pending)
            && self.validate(target).is_ok()
    }

    pub(super) fn settled_replacement(&self) -> bool {
        self.retired && self.replacement_thread.is_some() && self.started_turn.is_some()
    }

    pub(super) fn matches_thread(&self, thread: &str, shape: &str, static_context: &str) -> bool {
        self.replacement_thread.as_deref() == Some(thread)
            && self.target_shape == shape
            && self.target_static == static_context
    }
}

pub(super) struct CurrentNativeContext<'a> {
    pub model: &'a str,
    pub instructions: &'a str,
    pub tools: &'a [DynamicToolSpec],
    pub static_fingerprint: &'a str,
    pub conversation: Option<&'a str>,
    pub obligations: Option<&'a str>,
    pub checkpoint_bindings: Option<&'a str>,
    pub session_available: bool,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub reasoning_summary: ReasoningSummary,
    pub recovered_dispatches: &'a [RecoveredTurnStartDispatch],
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeHistoryRetirement {
    pub source_thread: String,
    pub source_generation: u64,
    pub replacement_thread: String,
    pub source_artifact: ArtifactRef,
    pub source_movement_seq: u64,
    pub proof: NativeHistoryProof,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeHistoryProof {
    pub thread_id: String,
    pub context_generation: u64,
    thread_shape: String,
    static_context: String,
    rollout_bytes: u64,
    rollout_blake3: String,
    calls: Vec<RetiredNativeCall>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct RetiredNativeCall {
    turn_id: String,
    call: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    interrupted_cell_wait: Option<InterruptedNativeCellWait>,
    pending: CodexPendingContinuity,
    closed_generation: u64,
    closure_artifact: ArtifactRef,
    closure_movement_seq: u64,
    result_artifact: ArtifactRef,
    result_movement_seq: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct InterruptedNativeCellWait {
    exec: serde_json::Value,
    yielded: serde_json::Value,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeCellWaitArgs {
    cell_id: String,
    #[serde(default, rename = "yield_time_ms")]
    _yield_time_ms: u64,
    #[serde(default, rename = "max_tokens")]
    _max_tokens: Option<usize>,
    #[serde(default)]
    terminate: bool,
}

fn native_wait_args(call: &serde_json::Value) -> Option<NativeCellWaitArgs> {
    let octopus_protocol::models::ResponseItem::FunctionCall { name, namespace, arguments, .. } =
        serde_json::from_value(call.clone()).ok()? else { return None; };
    if name != "wait" || namespace.as_deref().is_some_and(|value| value != "functions") {
        return None;
    }
    let args: NativeCellWaitArgs = serde_json::from_str(&arguments).ok()?;
    (!args.cell_id.is_empty() && args.cell_id.trim() == args.cell_id).then_some(args)
}

fn native_wait_cell(call: &serde_json::Value) -> Option<String> {
    native_wait_args(call).filter(|args| !args.terminate).map(|args| args.cell_id)
}

fn native_cell_status(output: &serde_json::Value) -> Option<String> {
    use octopus_protocol::models::{FunctionCallOutputBody, FunctionCallOutputContentItem, ResponseItem};
    let body = match serde_json::from_value(output.clone()).ok()? {
        ResponseItem::CustomToolCallOutput { output, .. } | ResponseItem::FunctionCallOutput { output, .. } => output.body,
        _ => return None,
    };
    let text = match &body {
        FunctionCallOutputBody::Text(text) => text,
        FunctionCallOutputBody::ContentItems(items) => match items.first()? {
            FunctionCallOutputContentItem::InputText { text } => text,
            _ => return None,
        },
    };
    let mut lines = text.lines();
    let status = lines.next()?;
    if !lines.next()?.starts_with("Wall time ")
        || lines.next()? != "Output:" { return None; }
    Some(status.to_owned())
}

fn native_yielded_cell(output: &serde_json::Value) -> Option<String> {
    let status = native_cell_status(output)?;
    let cell = status.strip_prefix("Script running with cell ID ")?;
    (!cell.is_empty() && cell.trim() == cell).then(|| cell.to_owned())
}

impl InterruptedNativeCellWait {
    fn matches(&self, call: &serde_json::Value) -> bool {
        let Some(cell) = native_wait_cell(call) else { return false; };
        let Ok(octopus_protocol::models::ResponseItem::CustomToolCall { name, call_id, .. }) =
            serde_json::from_value(self.exec.clone()) else { return false; };
        name == "exec" && !call_id.is_empty() && call_id.trim() == call_id
            && self.yielded["type"] == "custom_tool_call_output"
            && self.yielded["call_id"] == call_id && call["call_id"] != call_id
            && native_yielded_cell(&self.yielded).as_ref() == Some(&cell)
    }
}

fn pending_correlation(pending: &CodexPendingContinuity) -> Option<&str> {
    match pending {
        CodexPendingContinuity::DynamicTool { call_id, .. }
        | CodexPendingContinuity::InterfaceOperation { call_id, .. } => Some(call_id),
        CodexPendingContinuity::Operator { .. } => None,
    }
}

impl NativeHistoryProof {
    pub(super) fn validate(
        &self,
        source: &CodexContinuityState,
    ) -> Result<(), InterfaceDriverError> {
        if self.thread_id != source.thread_id
            || self.context_generation != source.context_generation
            || self.thread_shape != source.thread_shape_fingerprint
            || self.static_context != source.static_context_fingerprint
            || codex_continuity_waits_for_result(source)
            || source.tool_operator_request.is_some()
            || self.rollout_bytes == 0
            || !is_sha256_hex(&self.rollout_blake3)
            || self.calls.is_empty()
        {
            return Err(InterfaceDriverError::FailedClosed(
                "native history retirement has no exact settled source".into(),
            ));
        }
        let mut ids = BTreeSet::new();
        let mut turns = BTreeSet::new();
        for call in &self.calls {
            validate_pending_continuity_coordinates(Some(&call.pending), "native history source")?;
            let item: octopus_protocol::models::ResponseItem =
                serde_json::from_value(call.call.clone()).map_err(|error| {
                    InterfaceDriverError::FailedClosed(format!(
                        "retired native call is malformed: {error}"
                    ))
                })?;
            let call_id = match item {
                octopus_protocol::models::ResponseItem::CustomToolCall { call_id, .. }
                    if call.interrupted_cell_wait.is_none() => call_id,
                octopus_protocol::models::ResponseItem::FunctionCall { call_id, .. }
                    if call.interrupted_cell_wait.as_ref().map_or_else(
                        || pending_correlation(&call.pending) == Some(call_id.as_str()),
                        |wait| wait.matches(&call.call)) => call_id,
                _ => return Err(InterfaceDriverError::FailedClosed("unfinished native function call requires its exact pending correlation and admitted result".into())),
            };
            if call_id.is_empty()
                || call_id.trim() != call_id
                || !ids.insert(call_id)
                || call.turn_id.is_empty()
                || call.turn_id.trim() != call.turn_id
                || !turns.insert(&call.turn_id)
                || call.result_movement_seq >= call.closure_movement_seq
                || call.closed_generation > self.context_generation
                || pending_turn_id(&call.pending) != call.turn_id
                || !interrupted_pending_has_external_result(&call.pending)
            {
                return Err(InterfaceDriverError::FailedClosed(
                    "native history retirement has ambiguous call or result evidence".into(),
                ));
            }
        }
        Ok(())
    }
}

impl NativeHistoryRetirement {
    pub(super) fn connects(
        &self,
        source: &CodexContinuityState,
        target: &CodexContinuityState,
    ) -> bool {
        source.provider_interface == target.provider_interface
            && source.run == target.run
            && source.thread_id == self.source_thread
            && source.context_generation <= self.source_generation
            && source.thread_shape_fingerprint == self.proof.thread_shape
            && source.static_context_fingerprint == self.proof.static_context
            && source.tool_operator_request.is_none()
            && self.validate(target).is_ok()
            && source
                .pending
                .as_ref()
                .or(source.interrupted_pending.as_ref())
                .is_none_or(|pending| {
                    self.proof.calls.iter().any(|call| {
                        &call.pending == pending
                            && source.context_generation < call.closed_generation
                    })
                })
    }

    pub(super) fn validate(
        &self,
        current: &CodexContinuityState,
    ) -> Result<(), InterfaceDriverError> {
        if self.source_thread != self.proof.thread_id
            || self.source_generation != self.proof.context_generation
            || self.source_thread == self.replacement_thread
            || self.replacement_thread != current.thread_id
            || self.source_generation >= current.context_generation
        {
            return Err(InterfaceDriverError::FailedClosed(
                "native history retirement is detached from its replacement continuity".into(),
            ));
        }
        let mut source = current.clone();
        source.thread_id = self.source_thread.clone();
        source.context_generation = self.source_generation;
        source.thread_shape_fingerprint = self.proof.thread_shape.clone();
        source.static_context_fingerprint = self.proof.static_context.clone();
        source.pending = None;
        source.interrupted_pending = None;
        source.tool_operator_request = None;
        self.proof.validate(&source)
    }
}

fn native_history_orphans(
    path: &Path,
    thread: &str,
) -> Result<(u64, String, Vec<(String, serde_json::Value, Option<InterruptedNativeCellWait>)>), InterfaceDriverError> {
    let refused = |message: String| InterfaceDriverError::FailedClosed(message);
    let file = std::fs::File::open(path)
        .map_err(|error| refused(format!("native history cannot be read: {error}")))?;
    let metadata = file
        .metadata()
        .map_err(|error| refused(format!("native history metadata cannot be read: {error}")))?;
    if !metadata.is_file() {
        return Err(refused("native history is not a regular file".into()));
    }
    let mut reader = BufReader::new(file);
    let mut line = String::new();
    let mut hash = blake3::Hasher::new();
    let mut size = 0u64;
    let mut owner = None;
    let mut identity = false;
    let mut rolled_back = false;
    let mut known = BTreeMap::<String, (String, serde_json::Value)>::new();
    let mut open = BTreeMap::<String, (String, serde_json::Value)>::new();
    let mut cells = BTreeMap::<String, (String, InterruptedNativeCellWait)>::new();
    let mut seen_cells = BTreeSet::new();
    let mut wait_cells = BTreeMap::new();
    loop {
        line.clear();
        let count = (&mut reader)
            .take((RECOVERY_ROLLOUT_MAX_LINE_BYTES + 1) as u64)
            .read_line(&mut line)
            .map_err(|error| refused(format!("native history cannot be read: {error}")))?;
        if count == 0 {
            break;
        }
        if count > RECOVERY_ROLLOUT_MAX_LINE_BYTES || !line.ends_with('\n') {
            return Err(refused(
                "native history has an oversized or incomplete record".into(),
            ));
        }
        size += count as u64;
        hash.update(line.as_bytes());
        let value: serde_json::Value = serde_json::from_str(&line)
            .map_err(|error| refused(format!("native history record is malformed: {error}")))?;
        let payload = value
            .get("payload")
            .ok_or_else(|| refused("native history record has no payload".into()))?;
        let mut replacement = None;
        match value.get("type").and_then(serde_json::Value::as_str) {
            Some("session_meta") => {
                if identity || payload.get("id").and_then(serde_json::Value::as_str) != Some(thread)
                {
                    return Err(refused(
                        "native history belongs to a different or repeated thread".into(),
                    ));
                }
                identity = true;
            }
            Some("turn_context") => {
                owner = payload
                    .get("turn_id")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            }
            Some("event_msg")
                if matches!(
                    payload.get("type").and_then(serde_json::Value::as_str),
                    Some("task_started" | "turn_started")
                ) =>
            {
                owner = payload
                    .get("turn_id")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned);
            }
            Some("event_msg")
                if payload.get("type").and_then(serde_json::Value::as_str)
                    == Some("thread_rolled_back") =>
            {
                rolled_back = true
            }
            Some("compacted") => match payload.get("replacement_history") {
                Some(serde_json::Value::Array(items)) => {
                    open.clear();
                    replacement = Some(items.clone());
                }
                None | Some(serde_json::Value::Null) if open.is_empty() => {}
                _ => return Err(refused(
                    "native history compaction does not prove retention of its unfinished calls"
                        .into(),
                )),
            },
            Some("response_item") => replacement = Some(vec![payload.clone()]),
            _ => {}
        }
        let from_compaction =
            value.get("type").and_then(serde_json::Value::as_str) == Some("compacted");
        for item in replacement.into_iter().flatten() {
            let kind = item
                .get("type")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            if !matches!(
                kind,
                "custom_tool_call"
                    | "custom_tool_call_output"
                    | "function_call"
                    | "function_call_output"
            ) {
                continue;
            }
            let typed = serde_json::from_value::<octopus_protocol::models::ResponseItem>(item.clone())
                .map_err(|error| {
                    refused(format!("native history call/output is malformed: {error}"))
                })?;
            if matches!(&typed, octopus_protocol::models::ResponseItem::FunctionCallOutput {
                call_id: None, name: Some(name), ..
            } if !name.is_empty()) {
                continue;
            }
            let id = item
                .get("call_id")
                .and_then(serde_json::Value::as_str)
                .filter(|id| !id.is_empty() && id.trim() == *id)
                .ok_or_else(|| refused("native history call has no canonical identity".into()))?
                .to_owned();
            if kind.ends_with("_output") {
                if let Some((turn, call)) = open.remove(&id) {
                    if format!("{}_output", call["type"].as_str().unwrap_or_default()) != kind {
                        return Err(refused(
                            "native history output changed its call type".into(),
                        ));
                    }
                    if !from_compaction {
                        if call["type"] == "custom_tool_call" && call["name"] == "exec" {
                            if let Some(cell) = native_yielded_cell(&item) {
                                if !seen_cells.insert(cell.clone()) {
                                    return Err(refused("native history repeats a yielded cell identity".into()));
                                }
                                cells.insert(cell, (turn, InterruptedNativeCellWait { exec: call, yielded: item }));
                            }
                        } else if let Some(args) = native_wait_args(&call) {
                            if let Some(cell) = native_yielded_cell(&item) {
                                if cell != args.cell_id || !cells.contains_key(&cell) {
                                    return Err(refused("native cell wait yielded without its exact retained exec".into()));
                                }
                            } else if cells.contains_key(&args.cell_id) {
                                if !matches!(native_cell_status(&item).as_deref(), Some("Script completed" | "Script failed" | "Script terminated")) {
                                    return Err(refused("native cell wait has no exact yielded or terminal result".into()));
                                }
                                cells.remove(&args.cell_id);
                            }
                        }
                    }
                }
                continue;
            }
            let turn = if from_compaction {
                known
                    .get(&id)
                    .filter(|(_, original)| original == &item)
                    .map(|(turn, _)| turn.clone())
            } else {
                owner.clone()
            };
            let turn = turn.ok_or_else(|| {
                refused(format!(
                    "native call '{id}' has no exact retained turn ownership"
                ))
            })?;
            if !from_compaction {
                if let Some((_, evidence)) = native_wait_cell(&item).and_then(|cell| cells.get(&cell))
                    .filter(|(owner, _)| owner == &turn && cells.len() == 1) {
                    wait_cells.insert(id.clone(), evidence.clone());
                }
            }
            if open
                .insert(id.clone(), (turn.clone(), item.clone()))
                .is_some()
                || (!from_compaction && known.insert(id, (turn, item)).is_some())
            {
                return Err(refused("native history repeats a call identity".into()));
            }
        }
    }
    if !identity {
        return Err(refused(
            "native history has no exact thread identity".into(),
        ));
    }
    if rolled_back && !open.is_empty() {
        return Err(refused(
            "native history rollback prevents exact orphan retention proof".into(),
        ));
    }
    let after = reader
        .get_ref()
        .metadata()
        .map_err(|error| refused(format!("native history metadata cannot be read: {error}")))?;
    let modified = |metadata: &std::fs::Metadata| metadata.modified()
        .map_err(|error| refused(format!("native history modification time cannot be read: {error}")));
    if size != metadata.len()
        || after.len() != metadata.len()
        || modified(&after)? != modified(&metadata)?
    {
        return Err(refused(
            "native history changed while proving its settled calls".into(),
        ));
    }
    Ok((
        size,
        blake3::Hasher::finalize(&hash).to_hex().to_string(),
        open.into_values().map(|(turn, call)| {
            let wait = native_wait_cell(&call).and_then(|cell| cells.get(&cell))
                .filter(|(owner, _)| owner == &turn && cells.len() == 1)
                .filter(|(_, evidence)| call["call_id"].as_str().and_then(|id| wait_cells.get(id)) == Some(evidence))
                .map(|(_, evidence)| evidence.clone());
            (turn, call, wait)
        }).collect(),
    ))
}

pub(super) fn prove_native_history_retirement(
    path: &Path,
    source: &CodexContinuityState,
    session: &[SessionContextArtifact],
    labels: &BTreeSet<String>,
) -> Result<Option<NativeHistoryProof>, InterfaceDriverError> {
    if codex_continuity_waits_for_result(source) || source.tool_operator_request.is_some() {
        return Ok(None);
    }
    let (rollout_bytes, rollout_blake3, orphans) = native_history_orphans(path, &source.thread_id)?;
    if orphans.is_empty() {
        return Ok(None);
    }
    let refused = |message: &str| InterfaceDriverError::FailedClosed(message.into());
    let mut calls = Vec::new();
    for (turn_id, call, interrupted_cell_wait) in orphans {
        let mut proof = None;
        for entry in session.iter().filter(|entry| {
            entry.label == TURN_INTERRUPTED_RESULT_LABEL
                && entry.payload["thread_id"] == source.thread_id
                && entry.payload["turn_id"] == turn_id
        }) {
            if !labels.contains(&entry.label) {
                return Err(refused(
                    "native history closure is not a declared context output",
                ));
            }
            let original = parse_codex_continuity(
                entry
                    .payload
                    .get("native_terminal_source_continuity")
                    .ok_or_else(|| {
                        refused("native history closure has no exact original pending continuity")
                    })?,
                &source.provider_interface,
                &source.run,
            )?;
            let result_artifact: ArtifactRef =
                serde_json::from_value(entry.payload.get("source_artifact").cloned().ok_or_else(
                    || refused("native history closure has no exact result artifact"),
                )?)
                .map_err(|error| InterfaceDriverError::FailedClosed(error.to_string()))?;
            let results = session
                .iter()
                .filter(|result| result.source_artifact == result_artifact)
                .collect::<Vec<_>>();
            let [result] = results.as_slice() else {
                return Err(refused(
                    "native history retirement requires one admitted exact inner result",
                ));
            };
            let pending = original
                .pending
                .as_ref()
                .or(original.interrupted_pending.as_ref())
                .ok_or_else(|| refused("native history closure lost its original external wait"))?;
            if call["type"] == "function_call"
                && call["call_id"].as_str() != pending_correlation(pending)
                && !interrupted_cell_wait.as_ref().is_some_and(|wait| wait.matches(&call))
            {
                return Err(refused(
                    "unfinished native function call requires its exact pending correlation and admitted result",
                ));
            }
            let ok = match pending {
                CodexPendingContinuity::DynamicTool { .. } => {
                    let result = decode_tool_result_artifact(&result.payload)?
                        .ok_or_else(|| refused("native history result is not a tool result"))?;
                    validate_recovered_tool_result(&result, pending)?;
                    result.ok
                }
                CodexPendingContinuity::InterfaceOperation {
                    call_id,
                    operation,
                    output_label,
                    ..
                } => {
                    validate_recovered_interface_operation_result(
                        &result.label,
                        &result.payload,
                        call_id,
                        operation,
                        output_label,
                    )?;
                    result.payload["ok"].as_bool().ok_or_else(|| {
                        refused("native history operation result lost its outcome")
                    })?
                }
                _ => {
                    return Err(refused(
                        "native history retirement cannot settle an operator request",
                    ));
                }
            };
            if entry.payload["ok"] != ok {
                return Err(refused(
                    "native history closure changed the admitted result outcome",
                ));
            }
            let bytes = serde_json::to_vec(&entry.payload)
                .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
            let closed = interrupted_result_closure_payload(
                &entry.label,
                &bytes,
                &original,
                &result_artifact,
                labels,
            )
            .map_err(InterfaceDriverError::FailedClosed)?
            .ok_or_else(|| refused("native history has no admitted result closure"))?;
            let terminal = closed
                .native_terminal
                .as_ref()
                .ok_or_else(|| refused("native history closure has no native terminal proof"))?;
            if closed.thread_id != source.thread_id
                || closed.context_generation > source.context_generation
                || terminal
                    .exact_turn(&source.thread_id, Some(&turn_id))?
                    .status
                    != TurnStatus::Interrupted
            {
                return Err(refused(
                    "native history retirement is not a settled interrupted turn",
                ));
            }
            let closure_movement_seq = entry
                .source_movement_seq
                .ok_or_else(|| refused("native history closure has no admitted movement"))?;
            let result_movement_seq = result
                .source_movement_seq
                .ok_or_else(|| refused("native history result has no admitted movement"))?;
            if result_movement_seq >= closure_movement_seq || proof.is_some() {
                return Err(refused(
                    "native history has ambiguous or unordered result closure evidence",
                ));
            }
            proof = Some(RetiredNativeCall {
                turn_id: turn_id.clone(),
                call: call.clone(),
                interrupted_cell_wait: interrupted_cell_wait.clone(),
                pending: pending.clone(),
                closed_generation: closed.context_generation,
                closure_artifact: entry.source_artifact.clone(),
                closure_movement_seq,
                result_artifact,
                result_movement_seq,
            });
        }
        calls.push(proof.ok_or_else(|| refused(if call["type"] == "function_call" {
            "unfinished native function call has no exact admitted interrupted-result closure; explicit continuity repair is required"
        } else {
            "orphaned native wrapper has no admitted interrupted-result closure; explicit continuity repair is required"
        }))?);
    }
    let proof = NativeHistoryProof {
        thread_id: source.thread_id.clone(),
        context_generation: source.context_generation,
        thread_shape: source.thread_shape_fingerprint.clone(),
        static_context: source.static_context_fingerprint.clone(),
        rollout_bytes,
        rollout_blake3,
        calls,
    };
    proof.validate(source)?;
    Ok(Some(proof))
}
fn completed_effect(
    input: &ProviderInput,
    pending: &CodexPendingContinuity,
    namespace: &str,
    thread: &str,
    source_artifact: &ArtifactRef,
    projection: ModelResultProjection,
) -> Result<Option<(serde_json::Value, CodexTurnOutput)>, InterfaceDriverError> {
    let (call_id, name, ok, body, error, content) = match (input, pending) {
        (
            ProviderInput::ToolResult(result),
            CodexPendingContinuity::DynamicTool { call_id, tool, .. },
        ) => {
            validate_recovered_tool_result(result, pending)?;
            if let Some(operation) = native_pending_operation(pending)? {
                let receipt = serde_json::to_value(result)
                    .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
                match operation
                    .advance(&receipt)
                    .map_err(InterfaceDriverError::FailedClosed)?
                {
                    NativeStep::Complete { result } => (
                        operation.call_id,
                        operation.operation_name,
                        native_result_success(&result),
                        Some(result.clone()),
                        None,
                        native_result_content(&result, Vec::new())?,
                    ),
                    NativeStep::Effect { .. } | NativeStep::AmendmentEffect { .. } => return Ok(None),
                    NativeStep::Wait { .. } => {
                        return Err(InterfaceDriverError::FailedClosed(
                            "completed result cannot introduce a native wait".into(),
                        ));
                    }
                }
            } else {
                (
                    call_id.clone(),
                    tool.clone(),
                    result.ok,
                    result.body.clone(),
                    result.error.clone(),
                    dynamic_tool_response_content(result, source_artifact, projection)?,
                )
            }
        }
        (
            ProviderInput::InterfaceOperationResult(result),
            CodexPendingContinuity::InterfaceOperation {
                call_id,
                operation,
                output_label,
                ..
            },
        ) => {
            validate_recovered_interface_operation_result(
                &result.label,
                &result.value,
                call_id,
                operation,
                output_label,
            )?;
            (
                call_id.clone(),
                operation.clone(),
                result.ok,
                Some(result.value.clone()),
                None,
                interface_operation_response_content(result, source_artifact, projection)?,
            )
        }
        _ => {
            return Err(InterfaceDriverError::FailedClosed(
                "provider context refresh cannot replace its exact pending result".into(),
            ));
        }
    };
    let output = recovered_function_call_output(&call_id, recovered_result_output(content)?);
    let activity = CodexTurnOutput::dynamic_tool_completed(
        namespace.into(),
        name,
        call_id,
        thread.into(),
        pending_turn_id(pending).into(),
        ok,
        body,
        error,
    );
    Ok(Some((output, activity)))
}

impl CodexAppServerInterfaceDriver {
    pub(crate) async fn reject_retired_context_request(
        &self,
        sink: &TurnSink<'_>,
        client: &AppServerClient,
        request: &ServerRequest,
    ) -> Result<bool, InterfaceDriverError> {
        let Some(state) = sink.continuity_snapshot() else { return Ok(false); };
        let Some(refresh) = state.context_refresh.as_ref().filter(|refresh| refresh.retired) else {
            return Ok(false);
        };
        refresh.validate(&state)?;
        let frame = serde_json::to_value(request)
            .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
        if !native_frame_matches_turn(&frame, &refresh.source_thread, pending_turn_id(&refresh.pending)) {
            return Ok(false);
        }
        let request_id = serde_json::from_value(frame["id"].clone())
            .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
        let rejection = octopus_appserver_proto::JSONRPCErrorError {
            code: -32000,
            message: "The source turn has been retired; this request was not dispatched.".into(),
            data: Some(serde_json::json!({"external_effect_dispatched": false})),
        };
        let mut output = CodexTurnOutput::client(INTERFACE_CONTEXT_UPDATED_LABEL, serde_json::json!({
            "context_refresh_phase": "retired_request_rejected",
            "source_movement_seq": sink.source_movement_seq,
            "rejected_native_request": frame,
            "rejection": rejection,
        }));
        sink.normalize_output_label(&mut output)?;
        sink.admit_context_emissions(vec![sink.materialize_normalized(output, false)?]).await?;
        client.reject_server_request(request_id, rejection).await
            .map_err(|error| InterfaceDriverError::Transport(error.to_string()))?;
        Ok(true)
    }

    pub(super) async fn settled_native_history_proof(
        &self,
        scope: &TransportStateScope,
        thread: &str,
        source: Option<&CodexContinuityState>,
        session: &[SessionContextArtifact],
        sink: &TurnSink<'_>,
    ) -> Result<Option<NativeHistoryProof>, InterfaceDriverError> {
        let Some(source) = source.filter(|source| source.thread_id == thread) else {
            return Ok(None);
        };
        if codex_continuity_waits_for_result(source)
            || source.tool_operator_request.is_some()
            || self.has_pending_request(scope).await
        {
            return Ok(None);
        }
        let interrupted = source
            .native_terminal
            .as_ref()
            .map(|terminal| terminal.exact_turn(&source.thread_id, None))
            .transpose()?
            .is_some_and(|turn| turn.status == TurnStatus::Interrupted);
        let retained_closure = session.iter().any(|entry| {
            entry.label == TURN_INTERRUPTED_RESULT_LABEL
                && entry.payload["thread_id"] == source.thread_id
        });
        if !interrupted && !retained_closure {
            return Ok(None);
        }
        let path = self
            .transport_state
            .lock()
            .await
            .get(scope)
            .and_then(|state| state.rollout_path.clone());
        let Some(path) = path else {
            return Err(InterfaceDriverError::FailedClosed("settled native terminal history has no inspectable exact rollout; explicit continuity repair is required".into()));
        };
        let proof =
            prove_native_history_retirement(&path, source, session, sink.context_output_labels)?;
        if proof.is_some()
            && (!sink
                .context_output_labels
                .contains(INTERFACE_CONTEXT_UPDATED_LABEL)
                || !sink
                    .client_projection_output_labels
                    .contains(INTERFACE_CONTEXT_UPDATED_LABEL))
        {
            return Err(InterfaceDriverError::FailedClosed(
                "native history retirement requires its declared context output binding".into(),
            ));
        }
        Ok(proof)
    }

    pub(super) async fn refresh_native_context(
        &self,
        context: TurnContext<'_, '_>,
        input: &ProviderInput,
        recovered: Option<&CodexContinuityState>,
        generation: u64,
        current: CurrentNativeContext<'_>,
    ) -> Result<bool, InterfaceDriverError> {
        let Some(source) = recovered else {
            return Ok(false);
        };
        let retained = source.context_refresh.as_ref().filter(|refresh| {
            &refresh.result_artifact == context.source_artifact
                && refresh.result_movement == context.sink.source_movement_seq
        });
        let fingerprint = thread_shape_fingerprint(
            Some(current.model),
            current.instructions,
            current.tools,
            current.static_fingerprint,
        )?;
        if retained.is_none()
            && (source.thread_shape_fingerprint == fingerprint
                || source.pending.is_none()
                || source.interrupted_pending.is_some()
                || !matches!(
                    input,
                    ProviderInput::ToolResult(_) | ProviderInput::InterfaceOperationResult(_)
                ))
        {
            return Ok(false);
        }
        if !current.session_available
            || self.private_thread_cold_start(None)? == PrivateThreadColdStart::ResumeExact
        {
            return Err(InterfaceDriverError::FailedClosed("changed provider tools/instructions require a declared session snapshot and a provider context-reseed policy".into()));
        }
        let original = retained
            .map(|refresh| &refresh.pending)
            .or(source.pending.as_ref())
            .ok_or_else(|| {
                InterfaceDriverError::FailedClosed(
                    "context refresh lost its completed effect".into(),
                )
            })?;
        let namespace = self.native_tool_namespace()?.name;
        let source_thread = retained
            .map(|refresh| refresh.source_thread.as_str())
            .unwrap_or(&source.thread_id);
        let Some((completed_output, activity)) =
            completed_effect(input, original, &namespace, source_thread, context.source_artifact, self.model_result_projection()?)?
        else {
            return Ok(false);
        };
        let key = match original {
            CodexPendingContinuity::DynamicTool { call_id, .. } => format!("tool:{call_id}"),
            CodexPendingContinuity::InterfaceOperation { call_id, .. } => format!("tool:{call_id}"),
            _ => unreachable!("completed_effect accepts external results only"),
        };
        let pending = self.pending_request(context.scope, &key).await?;
        let effect_guard = if let Some(pending) = &pending {
            let guard = pending.effect_gate.clone().lock_owned().await;
            self.ensure_pending_request_current(context.scope, pending)
                .await?;
            if pending
                .turn_interrupted
                .load(std::sync::atomic::Ordering::Acquire)
            {
                return Ok(false);
            }
            if retained.is_none() {
                self.validate_pending_continuity(context.scope, Some(source), pending, generation)
                    .await?;
            }
            Some(guard)
        } else {
            None
        };
        let handle = self.client.lock().await.request_handle();
        let mut refresh = if let Some(refresh) = retained {
            refresh.validate(source)?;
            if refresh.completed_output != completed_output
                || refresh.target_shape != fingerprint
                || refresh.target_static != current.static_fingerprint
            {
                return Err(InterfaceDriverError::FailedClosed(
                    "admitted provider context refresh changed its result or target contract"
                        .into(),
                ));
            }
            refresh.clone()
        } else {
            if source.context_generation != generation {
                return Err(InterfaceDriverError::FailedClosed(
                    "context refresh result has stale source generation".into(),
                ));
            }
            if read_exact_terminal_turn(
                &handle,
                &self.next_request_id,
                source_thread,
                pending_turn_id(original),
            )
            .await
            .map_err(InterfaceDriverError::FailedClosed)?
            .is_some()
            {
                return Ok(false);
            }
            let refresh = ContextRefresh {
                source_thread: source.thread_id.clone(),
                source_shape: source.thread_shape_fingerprint.clone(),
                source_static: source.static_context_fingerprint.clone(),
                source_generation: source.context_generation,
                pending: original.clone(),
                result_artifact: context.source_artifact.clone(),
                result_movement: context.sink.source_movement_seq,
                completed_output,
                target_shape: fingerprint.clone(),
                target_static: current.static_fingerprint.into(),
                terminal: None,
                retired: false,
                replacement_thread: None,
                started_turn: None,
            };
            let mut prepared = source.clone();
            prepared.pending = None;
            prepared.tool_operator_request = None;
            prepared.context_refresh = Some(refresh.clone());
            context.sink.set_continuity(prepared);
            context
                .sink
                .admit_context_boundary("prepared", false, None)
                .await?;
            refresh
        };
        context
            .control
            .publish(CodexTurnControlTarget::InterruptedDraining {
                output: Arc::downgrade(context.sink.emit),
                durable_handoff: context.handoff_barrier.clone(),
                thread_id: refresh.source_thread.clone(),
                turn_id: pending_turn_id(&refresh.pending).into(),
            });
        if !refresh.retired {
            if let Some((state, emissions)) = context.sink.recover_context_retirement(&refresh)? {
                context.sink.admit_context_emissions(emissions).await?;
                refresh = state
                    .context_refresh
                    .clone()
                    .expect("validated refresh marker");
                context.sink.set_continuity(state);
            }
        }
        if !refresh.retired {
            let turn_id = pending_turn_id(&refresh.pending);
            let mut events = Vec::new();
            let mut terminal = read_exact_terminal_turn(
                &handle,
                &self.next_request_id,
                &refresh.source_thread,
                turn_id,
            )
            .await
            .map_err(InterfaceDriverError::FailedClosed)?;
            if terminal.is_none() {
                let interrupt = ClientRequest::TurnInterrupt {
                    request_id: self.mint_request_id(),
                    params: TurnInterruptParams {
                        thread_id: refresh.source_thread.clone(),
                        turn_id: turn_id.into(),
                    },
                };
                context
                    .sink
                    .admit_app_server_dispatch(&interrupt, None)
                    .await?;
                handle
                    .request_typed::<TurnInterruptResponse>(interrupt)
                    .await
                    .map_err(typed_error)?;
                let mut drain =
                    InterruptedTurnDrainState::new(refresh.source_thread.clone(), turn_id.into());
                let mut client = self.client.lock().await;
                loop {
                    if let InterruptedDrainDecision::Complete(evidence) =
                        drain.observe(client.next_event().await)
                    {
                        client.restore_ready_events(std::mem::take(&mut drain.deferred_events));
                        if evidence.disposition != "turn_completed"
                            || evidence.provider_error.is_some()
                        {
                            return Err(InterfaceDriverError::FailedClosed(format!(
                                "provider context retirement was not confirmed: {}",
                                evidence.disposition
                            )));
                        }
                        events.extend(evidence.native_terminal_events);
                        break;
                    }
                }
                drop(client);
                terminal = read_exact_terminal_turn(
                    &handle,
                    &self.next_request_id,
                    &refresh.source_thread,
                    turn_id,
                )
                .await
                .map_err(InterfaceDriverError::FailedClosed)?;
            }
            refresh.terminal = Some(terminal.ok_or_else(|| {
                InterfaceDriverError::FailedClosed(
                    "context refresh did not retire its original native turn".into(),
                )
            })?);
            events.extend(take_native_terminal_events(
                &mut *self.client.lock().await,
                refresh.terminal.as_ref().unwrap(),
                &refresh.source_thread,
                turn_id,
            )?);
            let requests = events.iter().filter_map(|event| event.get("quarantined_server_request"))
                .map(|frame| serde_json::from_value::<ServerRequest>(frame.clone())
                    .map_err(|error| InterfaceDriverError::Protocol(error.to_string())))
                .collect::<Result<Vec<_>, _>>()?;
            if let Some(pending) = &pending {
                self.remove_pending_request(context.scope, pending).await?;
            }
            let mut state = context.sink.continuity_snapshot().ok_or_else(|| {
                InterfaceDriverError::FailedClosed("refresh lost continuity".into())
            })?;
            state.context_refresh = Some(refresh.clone());
            context.sink.set_continuity(state);
            context
                .sink
                .bind_live_stream(&refresh.source_thread, turn_id)?;
            context
                .sink
                .admit_context_boundary("retired", true, Some(activity))
                .await?;
            for request in requests {
                if !self.reject_retired_context_request(context.sink, &*self.client.lock().await, &request).await? {
                    return Err(InterfaceDriverError::FailedClosed(
                        "quarantined request does not belong to the proven retired turn".into(),
                    ));
                }
            }
            refresh = context
                .sink
                .continuity_snapshot()
                .and_then(|state| state.context_refresh)
                .ok_or_else(|| {
                    InterfaceDriverError::FailedClosed(
                        "retirement lost its admitted transition".into(),
                    )
                })?;
        }
        if let Some(pending) = &pending {
            let retained_request = self
                .pending_server_requests
                .lock()
                .await
                .contains_key(&(context.scope.clone(), pending.key.clone()));
            if retained_request {
                self.remove_pending_request(context.scope, pending).await?;
            }
        }
        drop(effect_guard);
        if !current.recovered_dispatches.is_empty() {
            return Err(InterfaceDriverError::FailedClosed("provider context refresh already dispatched a successor turn; an uncertain successor is never replayed".into()));
        }
        let thread_id = if let Some(thread) = &refresh.replacement_thread {
            let response: ThreadResumeResponse = handle
                .request_typed(ClientRequest::ThreadResume {
                    request_id: self.mint_request_id(),
                    params: ThreadResumeParams {
                        thread_id: thread.clone(),
                        exclude_turns: true,
                        model: Some(current.model.into()),
                        service_tier: self.bound_service_tier()?,
                        config: Some(self.native_tool_thread_config()?),
                        developer_instructions: Some(current.instructions.into()),
                        sandbox: Some(self.native_thread_sandbox()?),
                        ..ThreadResumeParams::default()
                    },
                })
                .await
                .map_err(typed_error)?;
            if response.thread.id != *thread {
                return Err(InterfaceDriverError::FailedClosed(
                    "context refresh resumed a different replacement thread".into(),
                ));
            }
            validate_native_service_tier_response(
                self.bound_service_tier()?
                    .as_ref()
                    .and_then(Option::as_deref),
                response.service_tier.as_deref(),
            )?;
            self.transport_state.lock().await.insert(
                context.scope.clone(),
                TransportState {
                    context_generation: context
                        .sink
                        .continuity_snapshot()
                        .unwrap()
                        .context_generation,
                    hooks_catalog_emitted: true,
                    thread_id: Some(thread.clone()),
                    thread_shape_fingerprint: Some(fingerprint.clone()),
                    static_context_fingerprint: Some(current.static_fingerprint.into()),
                    rollout_path: response.thread.path,
                },
            );
            thread.clone()
        } else {
            let previous = context.sink.continuity_snapshot().ok_or_else(|| {
                InterfaceDriverError::FailedClosed("refresh lost retired continuity".into())
            })?;
            let (thread, _, _) = self
                .acquire_transport_thread(
                    context.scope,
                    ThreadAcquisition {
                        recovered: Some(&previous),
                        retired_history: None,
                        require_exact_resume: false,
                        context_generation: previous.context_generation.checked_add(1).ok_or_else(
                            || {
                                InterfaceDriverError::FailedClosed(
                                    "context refresh generation overflowed".into(),
                                )
                            },
                        )?,
                        covered_context_generation: previous.context_generation,
                        model: Some(current.model.into()),
                        bound_instructions: current.instructions,
                        native_affordances: current.tools,
                        static_context_fingerprint: current.static_fingerprint,
                        session_context_available: true,
                    },
                )
                .await?;
            refresh.replacement_thread = Some(thread.clone());
            let mut state = previous;
            state.thread_id = thread.clone();
            state.thread_shape_fingerprint = fingerprint;
            state.static_context_fingerprint = current.static_fingerprint.into();
            state.context_refresh = Some(refresh.clone());
            state.native_history_retirement = None;
            context.sink.set_continuity(state);
            context
                .sink
                .admit_context_boundary("ready", false, None)
                .await?;
            thread
        };
        let mut additional = native_turn_context(
            current.conversation.map(str::to_owned),
            current.obligations.map(str::to_owned),
            current.checkpoint_bindings.map(str::to_owned),
        );
        additional.insert("completed_effect".into(), AdditionalContextEntry { kind: AdditionalContextKind::Untrusted, value: serde_json::to_string(&serde_json::json!({
            "source_artifact": refresh.result_artifact, "source_movement_seq": refresh.result_movement,
            "source_thread": refresh.source_thread, "source_turn": pending_turn_id(&refresh.pending),
            "result": refresh.completed_output,
        })).map_err(|error| InterfaceDriverError::Protocol(error.to_string()))? });
        let request = ClientRequest::TurnStart {
            request_id: self.mint_request_id(),
            params: fresh_turn_start_params(
                thread_id.clone(),
                Vec::new(),
                current.model.into(),
                current.reasoning_effort,
                self.bound_service_tier()?,
                current.reasoning_summary,
                additional,
                "provider_context_refresh",
            ),
        };
        context
            .sink
            .admit_app_server_dispatch(&request, None)
            .await?;
        let response: octopus_appserver_proto::TurnStartResponse =
            handle.request_typed(request).await.map_err(typed_error)?;
        if response.turn.id.trim().is_empty() || response.turn.status != TurnStatus::InProgress {
            return Err(InterfaceDriverError::FailedClosed(
                "context refresh returned no active successor turn".into(),
            ));
        }
        let turn_id = response.turn.id;
        let mut started = context.sink.continuity_snapshot().ok_or_else(|| {
            InterfaceDriverError::FailedClosed("started refresh lost continuity".into())
        })?;
        started
            .context_refresh
            .as_mut()
            .ok_or_else(|| {
                InterfaceDriverError::FailedClosed("started refresh lost transition".into())
            })?
            .started_turn = Some(turn_id.clone());
        context.sink.set_continuity(started);
        context.control.publish(CodexTurnControlTarget::Draining {
            handle,
            request_ids: self.next_request_id.clone(),
            pending_requests: self.pending_server_requests.clone(),
            scope: context.scope.clone(),
            output: Arc::downgrade(context.sink.emit),
            continuity: context.sink.continuity_snapshot(),
            handoff_barrier: context.handoff_barrier.clone(),
            effect_gate: context.effect_gate.clone(),
            turn_interrupted: context.turn_interrupted.clone(),
            thread_id: thread_id.clone(),
            turn_id: turn_id.clone(),
        });
        self.drain_turn(context, &thread_id, &turn_id).await?;
        Ok(true)
    }
}

impl TurnSink<'_> {
    pub(super) async fn admit_native_history_retirement(&self) -> Result<(), InterfaceDriverError> {
        let state = self.continuity_snapshot().ok_or_else(|| {
            InterfaceDriverError::FailedClosed("native history retirement lost continuity".into())
        })?;
        let retirement = state.native_history_retirement.ok_or_else(|| {
            InterfaceDriverError::FailedClosed("native history retirement lost its proof".into())
        })?;
        let mut output = CodexTurnOutput::client(
            INTERFACE_CONTEXT_UPDATED_LABEL,
            serde_json::json!({
                "native_history_retired": true, "source_movement_seq": self.source_movement_seq,
                "source_artifact": retirement.source_artifact, "source_thread": retirement.source_thread,
                "replacement_thread": retirement.replacement_thread,
            }),
        );
        self.normalize_output_label(&mut output)?;
        let emission = self.materialize_normalized(output, false)?;
        self.admit_context_emissions(vec![emission]).await
    }

    pub(super) fn recover_context_retirement(
        &self,
        expected: &ContextRefresh,
    ) -> Result<Option<(CodexContinuityState, Vec<ProviderEmission>)>, InterfaceDriverError> {
        let identity = LiveTurnIdentity {
            provider_interface: self.provider_interface.as_str().into(),
            run: self.run.into(),
            thread_id: expected.source_thread.clone(),
            turn_id: pending_turn_id(&expected.pending).into(),
        };
        let Some((stream, payload_hash)) = crate::live_stream::recover_sealed_terminal_stream(
            self.live_stream_root,
            self.live_stream_rules,
            &identity,
            self.source_movement_seq,
            INTERFACE_CONTEXT_UPDATED_LABEL,
        )
        .map_err(InterfaceDriverError::FailedClosed)?
        else {
            return Ok(None);
        };
        let root = self
            .output_root
            .canonicalize()
            .map_err(|error| InterfaceDriverError::FailedClosed(error.to_string()))?;
        let slug = INTERFACE_CONTEXT_UPDATED_LABEL.replace('.', "_");
        let manifest = InterfaceArtifactManifest {
            label: INTERFACE_CONTEXT_UPDATED_LABEL.into(),
            payload_path: root.join(format!("{slug}-{payload_hash}.payload")),
            payload_content_hash: Some(payload_hash),
        };
        let bytes = serde_json::to_vec_pretty(&manifest)
            .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
        let hash = octopus_ledger::ContentHash::of_bytes(&bytes).to_hex();
        let path = root.join(format!("{slug}-{hash}.artifact.json"));
        if std::fs::read(&path)
            .map_err(|error| InterfaceDriverError::FailedClosed(error.to_string()))?
            != bytes
        {
            return Err(InterfaceDriverError::FailedClosed(
                "sealed context retirement changed its exact manifest".into(),
            ));
        }
        let boundary = InterfaceArtifactFile::new(path)
            .map_err(|error| InterfaceDriverError::FailedClosed(error.to_string()))?;
        let payload: serde_json::Value = serde_json::from_slice(
            &boundary
                .read_payload_bytes()
                .map_err(|error| InterfaceDriverError::FailedClosed(error.to_string()))?,
        )
        .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
        let state = parse_codex_continuity(
            &payload[CODEX_CONTINUITY_FIELD],
            self.provider_interface,
            self.run,
        )?;
        let refresh = state.context_refresh.as_ref().ok_or_else(|| {
            InterfaceDriverError::FailedClosed("sealed retirement has no exact transition".into())
        })?;
        let mut comparable = refresh.clone();
        comparable.terminal = expected.terminal.clone();
        comparable.retired = expected.retired;
        if payload["context_refresh_phase"] != "retired"
            || payload["source_movement_seq"] != self.source_movement_seq
            || !refresh.retired
            || refresh.replacement_thread.is_some()
            || &comparable != expected
            || self
                .continuity_snapshot()
                .is_some_and(|current| state.context_generation < current.context_generation)
        {
            return Err(InterfaceDriverError::FailedClosed(
                "sealed context retirement crossed its exact source, result or target".into(),
            ));
        }
        Ok(Some((
            state,
            vec![
                ProviderEmission {
                    artifact: stream,
                    route: ProviderEmissionRoute::ClientProjection,
                },
                ProviderEmission {
                    artifact: boundary,
                    route: ProviderEmissionRoute::ClientProjection,
                },
            ],
        )))
    }

    async fn admit_context_boundary(
        &self,
        phase: &str,
        seal: bool,
        activity: Option<CodexTurnOutput>,
    ) -> Result<(), InterfaceDriverError> {
        if !self
            .context_output_labels
            .contains(INTERFACE_CONTEXT_UPDATED_LABEL)
            || !self
                .client_projection_output_labels
                .contains(INTERFACE_CONTEXT_UPDATED_LABEL)
        {
            return Err(InterfaceDriverError::FailedClosed(
                "provider context refresh requires its declared context output binding".into(),
            ));
        }
        let mut emissions = Vec::new();
        if let Some(mut activity) = activity {
            self.normalize_output_label(&mut activity)?;
            if self.live_stream_rules.disposition(
                &activity.label,
                &activity.value,
                self.has_live_stream(),
            )? == LiveStreamDisposition::Stream
            {
                self.send(activity)?;
            } else {
                emissions.push(self.materialize_normalized(activity, true)?);
            }
        }
        if seal {
            let mut state = self.continuity_snapshot().ok_or_else(|| {
                InterfaceDriverError::FailedClosed("retirement lost continuity".into())
            })?;
            state
                .context_refresh
                .as_mut()
                .ok_or_else(|| {
                    InterfaceDriverError::FailedClosed("retirement lost transition".into())
                })?
                .retired = true;
            self.set_continuity(state);
        }
        if seal && self.has_live_stream() {
            self.advance_continuity(false)?;
        }
        let mut output = CodexTurnOutput::client(
            INTERFACE_CONTEXT_UPDATED_LABEL,
            serde_json::json!({"context_refresh_phase": phase, "source_movement_seq": self.source_movement_seq}),
        );
        self.normalize_output_label(&mut output)?;
        let boundary = self.materialize_normalized(output, false)?;
        if seal {
            let hash = artifact_payload_blake3(&boundary.artifact)
                .map_err(InterfaceDriverError::FailedClosed)?;
            if let Some(stream) = self.seal_live_stream(INTERFACE_CONTEXT_UPDATED_LABEL, &hash)? {
                emissions.push(stream);
            }
        }
        emissions.push(boundary);
        self.admit_context_emissions(emissions).await?;
        if seal {
            *self.live_stream.lock().expect("live stream lock") = None;
        }
        Ok(())
    }

    async fn admit_context_emissions(
        &self,
        emissions: Vec<ProviderEmission>,
    ) -> Result<(), InterfaceDriverError> {
        let (admitted, receipt) = tokio::sync::oneshot::channel();
        self.emit
            .sender
            .send(ProviderStreamEmission::ContextBoundary {
                emissions,
                admitted,
            })
            .map_err(|_| {
                InterfaceDriverError::Transport("context refresh output stream closed".into())
            })?;
        receipt
            .await
            .map_err(|_| {
                InterfaceDriverError::Transport("context refresh admission receipt lost".into())
            })?
            .map_err(InterfaceDriverError::FailedClosed)?;
        Ok(())
    }
}

pub(super) struct ResolvedModelContext {
    pub model: String,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub reasoning_summary: ReasoningSummary,
    pub static_context_fingerprint: String,
    pub bound_instructions: String,
    pub conversation_block: Option<String>,
    pub obligations_grant_block: Option<String>,
    pub checkpoint_bindings_block: Option<String>,
}

impl CodexAppServerInterfaceDriver {
    pub(super) async fn resolve_model_context(
        &self,
        input: &InterfaceInvocationEnvelope,
        assembled_context: &AssembledContext,
    ) -> Result<ResolvedModelContext, InterfaceDriverError> {
        let runtime = invocation_runtime_metadata(input)?;
        let model = self
            .resolve_private_config_string("model")?
            .ok_or_else(|| {
                InterfaceDriverError::FailedClosed(format!(
                    "no model bound to interface '{}'; its interface_driver_config.private_config \
                     carries no model, and the app-server may not pick a hidden default — bind the run \
                     to an interface descriptor whose folder config declares a model (constitution §3, \
                     fail-closed)",
                    runtime.interface.as_str()
                ))
            })?;
        let formed_models = required_formed_models(input)?;
        if !formed_models.contains(&model) {
            return Err(InterfaceDriverError::FailedClosed(format!(
                "Codex interface '{}' selected model '{}' outside its freshly formed interface policy models [{}]",
                runtime.interface.as_str(),
                model,
                formed_models.iter().cloned().collect::<Vec<_>>().join(", ")
            )));
        }
        let reasoning_effort = parse_reasoning_effort(
            self.resolve_private_config_string("reasoning_effort")?
                .as_deref(),
        )?;
        self.validate_native_service_tier(&model).await?;
        let reasoning_summary = parse_reasoning_summary(
            self.resolve_private_config_string("reasoning_summary")?
                .as_deref(),
        )?;

        let static_context_fingerprint = sha256_hex(&assembled_context.static_context_bytes);
        let assembled_role =
            (!assembled_context.role.is_empty()).then(|| assembled_context.role.join("\n\n"));
        let bound_instructions = resolve_bound_instructions(assembled_role.as_deref())?;
        let conversation_block = (!assembled_context.conversation.is_empty())
            .then(|| assembled_context.conversation.join("\n\n"));

        let obligations_grant_block = input.metadata.as_ref().and_then(render_model_context_block);
        let checkpoint_bindings_block = input.metadata.as_ref().and_then(render_checkpoint_bindings_block);

        Ok(ResolvedModelContext {
            model,
            reasoning_effort,
            reasoning_summary,
            static_context_fingerprint,
            bound_instructions,
            conversation_block,
            obligations_grant_block,
            checkpoint_bindings_block,
        })
    }
}
