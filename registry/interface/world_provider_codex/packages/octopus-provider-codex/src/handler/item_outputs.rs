use super::*;

pub(crate) fn turn_error_output(error: &octopus_appserver_proto::ErrorNotification) -> CodexTurnOutput {
    let authentication_failed =
        provider_authentication_failed(&error.error.message, error.error.codex_error_info.as_ref());
    let mut value = serde_json::json!({
        "message": error.error.message,
        "will_retry": error.will_retry && !authentication_failed,
        "additional_details": error.error.additional_details,
    });
    if authentication_failed {
        value["failure_code"] =
            serde_json::Value::String(PROVIDER_AUTHENTICATION_FAILURE_CODE.to_string());
    }
    CodexTurnOutput::client(TURN_ERROR_LABEL, value)
}

pub(crate) fn provider_error_is_terminal(error: &octopus_appserver_proto::ErrorNotification) -> bool {
    !error.will_retry
        || provider_authentication_failed(
            &error.error.message,
            error.error.codex_error_info.as_ref(),
        )
}

pub(crate) fn provider_authentication_failed(
    message: &str,
    error_info: Option<&octopus_appserver_proto::CodexErrorInfo>,
) -> bool {
    matches!(
        error_info,
        Some(octopus_appserver_proto::CodexErrorInfo::Unauthorized)
    ) || message.contains("Your access token could not be refreshed")
}

pub(crate) fn terminal_turn_may_emit_reply(
    status: &TurnStatus,
    error: Option<&octopus_appserver_proto::TurnError>,
) -> Result<bool, InterfaceDriverError> {
    match status {
        TurnStatus::Completed => Ok(true),
        TurnStatus::Interrupted => Ok(false),
        TurnStatus::Failed => {
            let message = error
                .map(|error| error.message.trim())
                .filter(|message| !message.is_empty())
                .unwrap_or("app-server reported no failure message");
            let details = error
                .and_then(|error| error.additional_details.as_deref())
                .map(str::trim)
                .filter(|details| !details.is_empty())
                .map(|details| format!(" ({details})"))
                .unwrap_or_default();
            Err(InterfaceDriverError::FailedClosed(format!(
                "Codex turn failed: {message}{details}"
            )))
        }
        TurnStatus::InProgress => Err(InterfaceDriverError::Protocol(
            "app-server emitted TurnCompleted with status 'inProgress'".to_string(),
        )),
    }
}

pub(crate) fn serialized_client_output<T: serde::Serialize>(
    label: &'static str,
    value: &T,
) -> Result<CodexTurnOutput, InterfaceDriverError> {
    let value = serde_json::to_value(value).map_err(|error| {
        InterfaceDriverError::Protocol(format!(
            "failed to encode app-server product event '{label}': {error}"
        ))
    })?;
    Ok(CodexTurnOutput::client(label, value))
}

pub(crate) fn proposed_plan_delta_output(
    delta: &octopus_appserver_proto::PlanDeltaNotification,
) -> Result<CodexTurnOutput, InterfaceDriverError> {
    let mut output = serialized_client_output(TURN_PLAN_DELTA_LABEL, delta)?;
    insert_client_output_field(
        &mut output,
        "plan_kind",
        serde_json::Value::String("proposed_plan".to_string()),
    );
    insert_client_output_field(
        &mut output,
        "octopus_authority",
        serde_json::Value::String("informational".to_string()),
    );
    insert_client_output_field(
        &mut output,
        "model_stream_authority",
        serde_json::Value::String("delta_non_authoritative".to_string()),
    );
    insert_client_output_field(
        &mut output,
        "execution_authority",
        serde_json::Value::String("octopus_checkpoints".to_string()),
    );
    Ok(output)
}

pub(crate) fn insert_client_output_field(output: &mut CodexTurnOutput, key: &str, value: serde_json::Value) {
    if let Some(object) = output.value.as_object_mut() {
        object.insert(key.to_string(), value);
    }
}

pub(crate) fn forbidden_native_effect_item(
    item: &ThreadItem,
    web_search: WebSearchMode,
    native_mcp_resource_discovery: bool,
) -> bool {
    if matches!(item, ThreadItem::WebSearch { .. }) {
        return web_search == WebSearchMode::Disabled;
    }
    if let ThreadItem::McpToolCall { server, tool, arguments, .. } = item {
        return !(native_mcp_resource_discovery
            && server == "codex"
            && matches!(tool.as_str(), "list_mcp_resources" | "list_mcp_resource_templates")
            && arguments.as_object().is_some_and(serde_json::Map::is_empty));
    }
    matches!(
        item,
        ThreadItem::HookPrompt { .. }
            | ThreadItem::CommandExecution { .. }
            | ThreadItem::FileChange { .. }
            | ThreadItem::CollabAgentToolCall { .. }
            | ThreadItem::SubAgentActivity { .. }
            | ThreadItem::ImageView { .. }
            | ThreadItem::Sleep { .. }
            | ThreadItem::ImageGeneration { .. }
    )
}

pub(crate) fn reject_native_effect_item(
    item: &ThreadItem,
    phase: &str,
    web_search: WebSearchMode,
    native_mcp_resource_discovery: bool,
) -> Result<(), InterfaceDriverError> {
    if !forbidden_native_effect_item(item, web_search, native_mcp_resource_discovery) {
        return Ok(());
    }
    let value = serde_json::to_value(item).map_err(|error| {
        InterfaceDriverError::Protocol(format!("native effect item cannot be serialized: {error}"))
    })?;
    let kind = value.get("type").and_then(serde_json::Value::as_str).ok_or_else(|| {
        InterfaceDriverError::Protocol("native effect item has no type".to_string())
    })?;
    Err(InterfaceDriverError::FailedClosed(format!(
        "Codex {phase} native effect '{kind}' (item '{}') outside its declared interface capabilities",
        item.id(),
    )))
}

pub(crate) fn started_item_output(
    item: ThreadItem,
    thread_id: &str,
    turn_id: &str,
    started_at_ms: i64,
) -> Option<CodexTurnOutput> {
    if matches!(
        &item,
        ThreadItem::UserMessage { .. }
            | ThreadItem::FunctionCallOutput { .. }
            | ThreadItem::HookPrompt { .. }
            | ThreadItem::AgentMessage { .. }
            | ThreadItem::ContextCompaction { .. }
    ) {
        return None;
    }
    let item_id = item.id().to_string();
    let mut output = rich_item_output(item)?;
    insert_client_output_field(
        &mut output,
        "thread_id",
        serde_json::Value::String(thread_id.to_string()),
    );
    insert_client_output_field(
        &mut output,
        "turn_id",
        serde_json::Value::String(turn_id.to_string()),
    );
    insert_client_output_field(&mut output, "item_id", serde_json::Value::String(item_id));
    insert_client_output_field(
        &mut output,
        "started_at_ms",
        serde_json::Value::Number(started_at_ms.into()),
    );
    insert_client_output_field(
        &mut output,
        "lifecycle",
        serde_json::Value::String("started".to_string()),
    );
    Some(output)
}

pub(crate) fn completed_item_output(
    item: ThreadItem,
    thread_id: &str,
    turn_id: &str,
    completed_at_ms: i64,
) -> Option<CodexTurnOutput> {
    let item_id = item.id().to_string();
    let mut output = rich_item_output(item)?;
    insert_client_output_field(
        &mut output,
        "thread_id",
        serde_json::Value::String(thread_id.to_string()),
    );
    insert_client_output_field(
        &mut output,
        "turn_id",
        serde_json::Value::String(turn_id.to_string()),
    );
    insert_client_output_field(&mut output, "item_id", serde_json::Value::String(item_id));
    insert_client_output_field(
        &mut output,
        "completed_at_ms",
        serde_json::Value::Number(completed_at_ms.into()),
    );
    insert_client_output_field(
        &mut output,
        "lifecycle",
        serde_json::Value::String("completed".to_string()),
    );
    Some(output)
}

pub(crate) fn manual_compaction_completed_output(
    id: String,
    thread_id: &str,
    turn_id: &str,
    completed_at_ms: i64,
) -> Result<CodexTurnOutput, InterfaceDriverError> {
    let mut output = completed_item_output(
        ThreadItem::ContextCompaction { id },
        thread_id,
        turn_id,
        completed_at_ms,
    )
    .ok_or_else(|| {
        InterfaceDriverError::FailedClosed(
            "manual compact produced no context-compaction projection".to_string(),
        )
    })?;
    insert_client_output_field(
        &mut output,
        "source",
        serde_json::Value::String("manual".to_string()),
    );
    insert_client_output_field(&mut output, "automatic", serde_json::Value::Bool(false));
    Ok(output)
}

pub(crate) fn completed_agent_message_output(
    item_id: &str,
    text: &str,
    phase: Option<MessagePhase>,
    thread_id: &str,
    turn_id: &str,
    completed_at_ms: i64,
) -> CodexTurnOutput {
    CodexTurnOutput::client(
        TURN_AGENT_MESSAGE_LABEL,
        serde_json::json!({
            "thread_id": thread_id,
            "turn_id": turn_id,
            "item_id": item_id,
            "text": text,
            "phase": phase,
            "completed_at_ms": completed_at_ms,
            "lifecycle": "completed",
        }),
    )
}

pub(crate) fn completed_turn_final_message(
    turn: &octopus_appserver_proto::Turn,
) -> Result<(&str, &str), InterfaceDriverError> {
    if turn.items_view == TurnItemsView::NotLoaded {
        return Err(InterfaceDriverError::FailedClosed(format!(
            "Codex completed turn '{}' has no loaded terminal items",
            turn.id,
        )));
    }
    let mut final_message = None;
    for item in &turn.items {
        if let ThreadItem::AgentMessage {
            id,
            text,
            phase: Some(MessagePhase::FinalAnswer),
        } = item
        {
            if final_message.replace((id.as_str(), text.as_str())).is_some() {
                return Err(InterfaceDriverError::FailedClosed(format!(
                    "Codex completed turn '{}' has multiple explicit final-answer items",
                    turn.id,
                )));
            }
            if id.trim().is_empty() {
                return Err(InterfaceDriverError::FailedClosed(format!(
                    "Codex completed turn '{}' has a final-answer item without identity",
                    turn.id,
                )));
            }
        }
    }
    final_message.ok_or_else(|| {
        InterfaceDriverError::FailedClosed(format!(
            "Codex completed turn '{}' has no explicit final-answer item",
            turn.id,
        ))
    })
}

pub(crate) fn completed_turn_reply<'a>(
    turn: &'a octopus_appserver_proto::Turn,
    completed_final_answer: Option<&'a (String, String)>,
) -> Result<(&'a str, &'a str), InterfaceDriverError> {
    if turn.items_view != TurnItemsView::NotLoaded {
        return completed_turn_final_message(turn);
    }
    match completed_final_answer {
        Some((id, text)) if !id.trim().is_empty() => Ok((id.as_str(), text.as_str())),
        _ => completed_turn_final_message(turn),
    }
}

pub(crate) fn rich_item_output(item: ThreadItem) -> Option<CodexTurnOutput> {
    let wire = serde_json::to_value(&item).ok()?;
    match item {
        ThreadItem::Reasoning {
            id,
            summary,
            content,
        } => Some(CodexTurnOutput::reasoning(id, summary, content)),
        ThreadItem::UserMessage { .. } | ThreadItem::FunctionCallOutput { .. } => None,
        ThreadItem::CommandExecution {
            id,
            command,
            cwd,
            process_id,
            source,
            status,
            command_actions,
            aggregated_output,
            exit_code,
            duration_ms,
        } => Some(CodexTurnOutput::exec_command(
            id,
            command,
            cwd.to_string(),
            process_id.filter(|p| !p.is_empty()),
            command_actions
                .into_iter()
                .map(|action| command_action_wire(action.into_core()))
                .collect(),
            exec_source_wire(source),
            exec_status_wire(status),
            exit_code.map(i64::from),
            duration_ms,
            aggregated_output.filter(|o| !o.is_empty()),
        )),
        ThreadItem::HookPrompt { .. } => {
            Some(CodexTurnOutput::client(TURN_HOOK_PROMPT_LABEL, wire))
        }
        ThreadItem::FileChange { .. } => {
            Some(CodexTurnOutput::client(TURN_FILE_CHANGE_LABEL, wire))
        }
        ThreadItem::McpToolCall { .. } => {
            Some(CodexTurnOutput::client(TURN_MCP_TOOL_CALL_LABEL, wire))
        }
        ThreadItem::DynamicToolCall { .. } => None,
        ThreadItem::CollabAgentToolCall { .. } => {
            Some(CodexTurnOutput::client(TURN_COLLABORATION_LABEL, wire))
        }
        ThreadItem::SubAgentActivity { .. } => {
            Some(CodexTurnOutput::client(TURN_SUBAGENT_ACTIVITY_LABEL, wire))
        }
        ThreadItem::Plan { id, text } => Some(CodexTurnOutput::proposed_plan(id, text)),
        ThreadItem::WebSearch {
            id, query, action, results,
        } => Some(web_search_output(id, query, action, results)),
        ThreadItem::ImageView { .. } => Some(CodexTurnOutput::client(TURN_IMAGE_VIEW_LABEL, wire)),
        ThreadItem::Sleep { .. } => Some(CodexTurnOutput::client(TURN_SLEEP_LABEL, wire)),
        ThreadItem::ImageGeneration { .. } => {
            Some(CodexTurnOutput::client(TURN_IMAGE_GENERATION_LABEL, wire))
        }
        ThreadItem::EnteredReviewMode { .. } => {
            Some(CodexTurnOutput::client(TURN_REVIEW_ENTERED_LABEL, wire))
        }
        ThreadItem::ExitedReviewMode { .. } => {
            Some(CodexTurnOutput::client(TURN_REVIEW_EXITED_LABEL, wire))
        }
        ThreadItem::ContextCompaction { .. } => {
            Some(CodexTurnOutput::client(TURN_CONTEXT_COMPACTION_LABEL, wire))
        }
        _ => None,
    }
}

pub(crate) fn web_search_output(
    item_id: String,
    query: String,
    action: Option<WebSearchAction>,
    results: Option<Vec<serde_json::Value>>,
) -> CodexTurnOutput {
    let action = action.map(|action| match action {
        WebSearchAction::Search { query, queries } => serde_json::json!({
            "kind": "search",
            "query": query,
            "queries": queries.unwrap_or_default(),
        }),
        WebSearchAction::OpenPage { url } => serde_json::json!({
            "kind": "open_page",
            "url": url,
        }),
        WebSearchAction::FindInPage { url, pattern } => serde_json::json!({
            "kind": "find_in_page",
            "url": url,
            "pattern": pattern,
        }),
        WebSearchAction::Other => serde_json::json!({ "kind": "other" }),
    });
    let mut value = serde_json::json!({
        "item_id": item_id,
        "query": query,
        "action": action,
    });
    if let Some(results) = results {
        value["results"] = serde_json::json!(results);
    }
    CodexTurnOutput::client(TURN_WEB_SEARCH_LABEL, value)
}

pub(crate) fn materialize_incompatible_continuity_turn_error(
    output_root: &Path,
    source_movement_seq: u64,
    continuity_error: &str,
    terminal_failure: &str,
) -> Result<InterfaceArtifactFile, InterfaceDriverError> {
    materialize_terminal_turn_error_payload(
        output_root,
        &format!(
            "Codex cannot continue this task because its saved continuity state is incompatible with the current provider interface. Create a new task. Technical detail: {continuity_error}"
        ),
        None,
        serde_json::json!({
            "code": PROVIDER_CONTINUITY_INCOMPATIBLE_CODE,
            "source_movement_seq": source_movement_seq,
            "continuity_error": continuity_error,
            "terminal_failure": terminal_failure,
        }),
    )
}

pub(crate) fn materialize_terminal_turn_error_payload(
    output_root: &Path,
    message: &str,
    continuity: Option<&CodexContinuityState>,
    additional_details: serde_json::Value,
) -> Result<InterfaceArtifactFile, InterfaceDriverError> {
    let mut value = serde_json::json!({
        "label": TURN_ERROR_LABEL,
        "message": message,
        "will_retry": false,
        "additional_details": additional_details,
    });
    if provider_authentication_failed(message, None) {
        value["failure_code"] =
            serde_json::Value::String(PROVIDER_AUTHENTICATION_FAILURE_CODE.to_string());
    }
    if let Some(continuity) = continuity {
        value[CODEX_CONTINUITY_FIELD] = serde_json::to_value(continuity).map_err(|error| {
            InterfaceDriverError::Protocol(format!(
                "failed to encode terminal Codex continuity: {error}"
            ))
        })?;
    }
    let bytes = serde_json::to_vec(&value).map_err(|e| {
        InterfaceDriverError::Protocol(format!("failed to encode terminal turn.error: {e}"))
    })?;
    materialize_codex_emitted_artifact(output_root, TURN_ERROR_LABEL, &bytes)
}

pub(crate) fn exec_source_wire(source: CommandExecutionSource) -> &'static str {
    match source {
        CommandExecutionSource::Agent => "agent",
        CommandExecutionSource::UserShell => "userShell",
        CommandExecutionSource::UnifiedExecStartup => "unifiedExecStartup",
        CommandExecutionSource::UnifiedExecInteraction => "unifiedExecInteraction",
    }
}

pub(crate) fn command_action_wire(
    action: octopus_protocol::parse_command::ParsedCommand,
) -> serde_json::Value {
    use octopus_protocol::parse_command::ParsedCommand;
    match action {
        ParsedCommand::Read { cmd, name, path } => serde_json::json!({
            "kind": "read",
            "cmd": cmd,
            "name": name,
            "path": path.display().to_string(),
        }),
        ParsedCommand::ListFiles { cmd, path } => serde_json::json!({
            "kind": "list_files",
            "cmd": cmd,
            "path": path,
        }),
        ParsedCommand::Search { cmd, query, path } => serde_json::json!({
            "kind": "search",
            "cmd": cmd,
            "query": query,
            "path": path,
        }),
        ParsedCommand::Unknown { cmd } => serde_json::json!({
            "kind": "unknown",
            "cmd": cmd,
        }),
    }
}

pub(crate) fn exec_status_wire(status: CommandExecutionStatus) -> &'static str {
    match status {
        CommandExecutionStatus::InProgress => "in_progress",
        CommandExecutionStatus::Completed => "completed",
        CommandExecutionStatus::Failed => "failed",
        CommandExecutionStatus::Declined => "declined",
    }
}

pub(crate) fn plan_update_output(plan: &TurnPlanUpdatedNotification) -> CodexTurnOutput {
    let mut lines: Vec<String> = Vec::new();
    if let Some(explanation) = plan
        .explanation
        .as_deref()
        .map(str::trim)
        .filter(|e| !e.is_empty())
    {
        lines.push(explanation.to_string());
    }
    for step in &plan.plan {
        lines.push(format!("{} {}", plan_step_glyph(step), step.step));
    }
    CodexTurnOutput::agent_checklist(plan, lines.join("\n"))
}

pub(crate) fn model_safety_buffering_output(
    status: &ModelSafetyBufferingUpdatedNotification,
) -> Result<CodexTurnOutput, InterfaceDriverError> {
    let mut output = serialized_client_output(TURN_WARNING_LABEL, status)?;
    insert_client_output_field(
        &mut output,
        "message",
        serde_json::Value::String(if status.show_buffering_ui {
            "The model is taking extra time to review this request before responding.".to_string()
        } else {
            "The model's additional review period has ended.".to_string()
        }),
    );
    insert_client_output_field(
        &mut output,
        "source",
        serde_json::Value::String("model_safety_buffering".to_string()),
    );
    Ok(output)
}

pub(crate) fn plan_step_glyph(step: &TurnPlanStep) -> &'static str {
    use octopus_appserver_proto::TurnPlanStepStatus;
    match step.status {
        TurnPlanStepStatus::Completed => "[x]",
        TurnPlanStepStatus::InProgress => "[~]",
        TurnPlanStepStatus::Pending => "[ ]",
    }
}

pub(crate) fn terminal_usage_evidence(
    usage: &ThreadTokenUsage,
) -> Result<(ContextWindowUsage, ProviderTokenUsageEvidence), InterfaceDriverError> {
    let context_window = usage.model_context_window.ok_or_else(|| {
        InterfaceDriverError::FailedClosed(
            "app-server token usage omitted the positive model context window required by terminal usage evidence"
                .to_string(),
        )
    })?;
    let cumulative = token_usage_breakdown_evidence(&usage.total, "cumulative")?;
    let current_context = token_usage_breakdown_evidence(&usage.last, "current-context")?;
    let used_tokens = current_context.total_tokens;
    let context_window = u64::try_from(context_window)
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| {
            InterfaceDriverError::Protocol(
                "app-server token usage carries a non-positive context window".to_string(),
            )
        })?;
    Ok((
        ContextWindowUsage {
            used_tokens,
            context_window,
        },
        ProviderTokenUsageEvidence {
            cumulative,
            current_context,
            context_window,
        },
    ))
}

pub(crate) fn token_usage_breakdown_evidence(
    usage: &TokenUsageBreakdown,
    scope: &str,
) -> Result<TokenUsageBreakdownEvidence, InterfaceDriverError> {
    let nonnegative = |field: &str, value: i64| {
        u64::try_from(value).map_err(|_| {
            InterfaceDriverError::Protocol(format!(
                "app-server {scope} token usage carries a negative {field}"
            ))
        })
    };
    let total_tokens = nonnegative("total_tokens", usage.total_tokens)?;
    let input_tokens = nonnegative("input_tokens", usage.input_tokens)?;
    let cached_input_tokens = nonnegative("cached_input_tokens", usage.cached_input_tokens)?;
    let output_tokens = nonnegative("output_tokens", usage.output_tokens)?;
    let reasoning_output_tokens =
        nonnegative("reasoning_output_tokens", usage.reasoning_output_tokens)?;
    let uncached_input_tokens = input_tokens
        .checked_sub(cached_input_tokens)
        .ok_or_else(|| {
            InterfaceDriverError::Protocol(format!(
                "app-server {scope} token usage reports more cached input than total input"
            ))
        })?;
    Ok(TokenUsageBreakdownEvidence {
        total_tokens,
        input_tokens,
        cached_input_tokens,
        uncached_input_tokens,
        output_tokens,
        reasoning_output_tokens,
    })
}

pub(crate) fn resolve_bound_instructions(role: Option<&str>) -> Result<String, InterfaceDriverError> {
    role.map(str::trim)
        .filter(|r| !r.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            InterfaceDriverError::FailedClosed(
                "provider worker context is missing: bound_instructions must be resolved from bound \
                 artifacts; refusing a Rust fallback or codex ambient context"
                    .into(),
            )
        })
}
