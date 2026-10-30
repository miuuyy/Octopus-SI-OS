use super::*;

#[derive(Default)]
pub(crate) struct TurnDrain {
    pub(crate) message_phases: BTreeMap<String, Option<MessagePhase>>,
    pub(crate) completed_final_answer: Option<(String, String)>,
    pub(crate) latest_context_window_usage: Option<ContextWindowUsage>,
    pub(crate) latest_provider_token_usage: Option<ProviderTokenUsageEvidence>,
    pub(crate) terminal_error: Option<CodexTurnOutput>,
    pub(crate) pending_delta: PendingClientDelta,
    pub(crate) delta_seq: u64,
    pub(crate) flush_deadline: Option<tokio::time::Instant>,
}

pub(crate) enum DrainStep {
    Next,
    Done(TurnDrainOutcome),
}

impl CodexAppServerInterfaceDriver {
    pub(crate) async fn drain_notification(
        &self,
        context: TurnContext<'_, '_>,
        thread_id: &str,
        turn_id: &str,
        provider_account_events_enabled: bool,
        drain: &mut TurnDrain,
        notification: ServerNotification,
    ) -> Result<DrainStep, InterfaceDriverError> {
        let sink = context.sink;
        let control = context.control;
        let scope = context.scope;
        let TurnDrain {
            message_phases,
            completed_final_answer,
            latest_context_window_usage,
            latest_provider_token_usage,
            terminal_error,
            pending_delta,
            delta_seq,
            flush_deadline,
            ..
        } = drain;
        let _notification_guard = context.effect_gate.lock().await;
        if !server_notification_matches_turn(&notification, thread_id, turn_id)? {
            return Ok(DrainStep::Next);
        }
        match notification {
            ServerNotification::TurnStarted(turn) if turn.thread_id == thread_id => {
                flush_pending_delta(sink, pending_delta, delta_seq)?;
                *flush_deadline = None;
                sink.send_progress(serialized_client_output(TURN_STARTED_LABEL, &turn)?)?;
            }
            ServerNotification::AgentMessageDelta(delta) if delta.thread_id == thread_id => {
                let phase = message_phases.get(&delta.item_id).cloned().flatten();
                if queue_client_delta(
                    sink,
                    pending_delta,
                    delta_seq,
                    CodexTurnOutput::scoped_message_delta(
                        0,
                        delta.delta,
                        delta.turn_id,
                        delta.item_id,
                        phase,
                    ),
                    "text",
                )? {
                    *flush_deadline = Some(tokio::time::Instant::now() + DELTA_FLUSH_WINDOW);
                }
            }
            ServerNotification::ItemStarted(started) if started.thread_id == thread_id => {
                flush_pending_delta(sink, pending_delta, delta_seq)?;
                *flush_deadline = None;
                reject_native_effect_item(
                    &started.item,
                    "started",
                    self.native_web_search_mode()?,
                    self.native_mcp_resource_discovery()?,
                )?;
                if let ThreadItem::AgentMessage { id, phase, .. } = &started.item {
                    message_phases.insert(id.clone(), phase.clone());
                }
                sink.record_input_item(
                    &started.item,
                    &started.thread_id,
                    &started.turn_id,
                    InputRecordedSource::ItemStarted,
                    Some(started.started_at_ms),
                )?;
                if let Some(output) = started_item_output(
                    started.item,
                    &started.thread_id,
                    &started.turn_id,
                    started.started_at_ms,
                ) {
                    sink.send_progress(output)?;
                }
            }
            ServerNotification::PlanDelta(delta) if delta.thread_id == thread_id => {
                if queue_client_delta(
                    sink,
                    pending_delta,
                    delta_seq,
                    proposed_plan_delta_output(&delta)?,
                    "delta",
                )? {
                    *flush_deadline = Some(tokio::time::Instant::now() + DELTA_FLUSH_WINDOW);
                }
            }
            ServerNotification::ReasoningSummaryTextDelta(delta)
                if delta.thread_id == thread_id =>
            {
                if queue_client_delta(
                    sink,
                    pending_delta,
                    delta_seq,
                    serialized_client_output(TURN_REASONING_DELTA_LABEL, &delta)?,
                    "delta",
                )? {
                    *flush_deadline = Some(tokio::time::Instant::now() + DELTA_FLUSH_WINDOW);
                }
            }
            ServerNotification::ReasoningSummaryPartAdded(part) if part.thread_id == thread_id => {
                flush_pending_delta(sink, pending_delta, delta_seq)?;
                *flush_deadline = None;
                let mut output = serialized_client_output(TURN_REASONING_DELTA_LABEL, &part)?;
                insert_client_output_field(
                    &mut output,
                    "delta",
                    serde_json::Value::String(String::new()),
                );
                insert_client_output_field(
                    &mut output,
                    "section_break",
                    serde_json::Value::Bool(true),
                );
                sink.send_progress(output)?;
            }
            ServerNotification::ReasoningTextDelta(delta) if delta.thread_id == thread_id => {
                if queue_client_delta(
                    sink,
                    pending_delta,
                    delta_seq,
                    serialized_client_output(TURN_REASONING_DELTA_LABEL, &delta)?,
                    "delta",
                )? {
                    *flush_deadline = Some(tokio::time::Instant::now() + DELTA_FLUSH_WINDOW);
                }
            }
            ServerNotification::CommandExecutionOutputDelta(delta)
                if delta.thread_id == thread_id =>
            {
                if queue_client_delta(
                    sink,
                    pending_delta,
                    delta_seq,
                    serialized_client_output(TURN_EXEC_OUTPUT_DELTA_LABEL, &delta)?,
                    "delta",
                )? {
                    *flush_deadline = Some(tokio::time::Instant::now() + DELTA_FLUSH_WINDOW);
                }
            }
            ServerNotification::TerminalInteraction(delta) if delta.thread_id == thread_id => {
                flush_pending_delta(sink, pending_delta, delta_seq)?;
                *flush_deadline = None;
                sink.send_progress(serialized_client_output(
                    TURN_TERMINAL_INTERACTION_LABEL,
                    &delta,
                )?)?;
            }
            ServerNotification::FileChangeOutputDelta(delta) if delta.thread_id == thread_id => {
                if queue_client_delta(
                    sink,
                    pending_delta,
                    delta_seq,
                    serialized_client_output(TURN_FILE_CHANGE_DELTA_LABEL, &delta)?,
                    "delta",
                )? {
                    *flush_deadline = Some(tokio::time::Instant::now() + DELTA_FLUSH_WINDOW);
                }
            }
            ServerNotification::FileChangePatchUpdated(delta) if delta.thread_id == thread_id => {
                flush_pending_delta(sink, pending_delta, delta_seq)?;
                *flush_deadline = None;
                sink.send_progress(serialized_client_output(
                    TURN_FILE_CHANGE_DELTA_LABEL,
                    &delta,
                )?)?;
            }
            ServerNotification::McpToolCallProgress(progress)
                if progress.thread_id == thread_id =>
            {
                flush_pending_delta(sink, pending_delta, delta_seq)?;
                *flush_deadline = None;
                sink.send_progress(serialized_client_output(
                    TURN_MCP_PROGRESS_LABEL,
                    &progress,
                )?)?;
            }
            ServerNotification::TurnDiffUpdated(diff) if diff.thread_id == thread_id => {
                flush_pending_delta(sink, pending_delta, delta_seq)?;
                *flush_deadline = None;
                sink.send_progress(serialized_client_output(TURN_DIFF_UPDATED_LABEL, &diff)?)?;
            }
            ServerNotification::ItemCompleted(ItemCompletedNotification {
                item,
                thread_id: item_thread,
                turn_id,
                completed_at_ms,
            }) if item_thread == thread_id => {
                reject_native_effect_item(
                    &item,
                    "completed",
                    self.native_web_search_mode()?,
                    self.native_mcp_resource_discovery()?,
                )?;
                flush_pending_delta(sink, pending_delta, delta_seq)?;
                *flush_deadline = None;
                sink.record_input_item(
                    &item,
                    &item_thread,
                    &turn_id,
                    InputRecordedSource::ItemCompleted,
                    Some(completed_at_ms),
                )?;
                let completed_output = match item {
                    ThreadItem::AgentMessage { id, text, phase } => {
                        message_phases.remove(&id);
                        if phase == Some(MessagePhase::FinalAnswer)
                            && completed_final_answer
                                .replace((id.clone(), text.clone()))
                                .is_some()
                        {
                            return Err(InterfaceDriverError::FailedClosed(format!(
                                "Codex turn '{turn_id}' completed more than one final-answer item",
                            )));
                        }
                        Some(completed_agent_message_output(
                            &id,
                            &text,
                            phase,
                            item_thread.as_str(),
                            turn_id.as_str(),
                            completed_at_ms,
                        ))
                    }
                    item => completed_item_output(
                        item,
                        item_thread.as_str(),
                        turn_id.as_str(),
                        completed_at_ms,
                    ),
                };
                if let Some(output) = completed_output {
                    sink.send_progress(output)?;
                }
            }
            ServerNotification::TurnPlanUpdated(plan) if plan.thread_id == thread_id => {
                flush_pending_delta(sink, pending_delta, delta_seq)?;
                *flush_deadline = None;
                sink.send_progress(plan_update_output(&plan))?;
            }
            ServerNotification::ThreadTokenUsageUpdated(usage) if usage.thread_id == thread_id => {
                let (context_window_usage, provider_token_usage) =
                    terminal_usage_evidence(&usage.token_usage)?;
                sink.send_progress(CodexTurnOutput::client(TURN_TOKEN_USAGE_LABEL, serde_json::json!({
                    "thread_id": thread_id,
                    "turn_id": usage.turn_id,
                    "context_window_usage": context_window_usage,
                    "provider_token_usage": provider_token_usage,
                })))?;
                *latest_context_window_usage = Some(context_window_usage);
                *latest_provider_token_usage = Some(provider_token_usage);
            }
            ServerNotification::HookStarted(hook) if hook.thread_id == thread_id => {
                flush_pending_delta(sink, pending_delta, delta_seq)?;
                *flush_deadline = None;
                sink.send_progress(hook_result_output(
                    &hook.run,
                    "started",
                    hook.turn_id.as_deref(),
                ))?;
            }
            ServerNotification::HookCompleted(hook) if hook.thread_id == thread_id => {
                flush_pending_delta(sink, pending_delta, delta_seq)?;
                *flush_deadline = None;
                sink.send_progress(hook_result_output(
                    &hook.run,
                    "completed",
                    hook.turn_id.as_deref(),
                ))?;
            }
            ServerNotification::ItemGuardianApprovalReviewStarted(review)
                if review.thread_id == thread_id =>
            {
                flush_pending_delta(sink, pending_delta, delta_seq)?;
                *flush_deadline = None;
                sink.send_progress(serialized_client_output(
                    TURN_AUTO_REVIEW_STARTED_LABEL,
                    &review,
                )?)?;
            }
            ServerNotification::ItemGuardianApprovalReviewCompleted(review)
                if review.thread_id == thread_id =>
            {
                flush_pending_delta(sink, pending_delta, delta_seq)?;
                *flush_deadline = None;
                sink.send_progress(serialized_client_output(
                    TURN_AUTO_REVIEW_COMPLETED_LABEL,
                    &review,
                )?)?;
            }
            ServerNotification::ServerRequestResolved(request)
                if request.thread_id == thread_id =>
            {
                flush_pending_delta(sink, pending_delta, delta_seq)?;
                *flush_deadline = None;
                sink.send_progress(serialized_client_output(
                    TURN_OPERATOR_REQUEST_RESOLVED_LABEL,
                    &request,
                )?)?;
            }
            ServerNotification::McpServerStatusUpdated(status)
                if status.thread_id.as_deref() == Some(thread_id) =>
            {
                flush_pending_delta(sink, pending_delta, delta_seq)?;
                *flush_deadline = None;
                sink.send_progress(serialized_client_output(TURN_MCP_STATUS_LABEL, &status)?)?;
            }
            ServerNotification::ThreadGoalUpdated(goal) if goal.thread_id == thread_id => {
                flush_pending_delta(sink, pending_delta, delta_seq)?;
                *flush_deadline = None;
                sink.send_progress(goal_status_output(&goal.goal))?;
            }
            ServerNotification::ContextCompacted(compaction)
                if compaction.thread_id == thread_id =>
            {
                flush_pending_delta(sink, pending_delta, delta_seq)?;
                *flush_deadline = None;
            }
            ServerNotification::ModelRerouted(model) if model.thread_id == thread_id => {
                let selected_model =
                    self.resolve_private_config_string("model")?
                        .ok_or_else(|| {
                            InterfaceDriverError::FailedClosed(
                            "Codex model reroute arrived without a descriptor-bound selected model"
                                .to_string(),
                        )
                        })?;
                validate_model_reroute(&selected_model, &model.from_model, &model.to_model)?;
                flush_pending_delta(sink, pending_delta, delta_seq)?;
                *flush_deadline = None;
                sink.send_progress(serialized_client_output(TURN_MODEL_REROUTED_LABEL, &model)?)?;
            }
            ServerNotification::ModelVerification(model) if model.thread_id == thread_id => {
                flush_pending_delta(sink, pending_delta, delta_seq)?;
                *flush_deadline = None;
                sink.send_progress(serialized_client_output(
                    TURN_MODEL_VERIFICATION_LABEL,
                    &model,
                )?)?;
            }
            ServerNotification::AuthRecoveryStarted(recovery)
                if recovery.thread_id == thread_id && recovery.turn_id == turn_id =>
            {
                flush_pending_delta(sink, pending_delta, delta_seq)?;
                *flush_deadline = None;
                sink.send_progress(serialized_client_output(
                    TURN_AUTH_RECOVERY_STARTED_LABEL,
                    &recovery,
                )?)?;
            }
            ServerNotification::AuthRecoveryCompleted(recovery)
                if recovery.thread_id == thread_id && recovery.turn_id == turn_id =>
            {
                flush_pending_delta(sink, pending_delta, delta_seq)?;
                *flush_deadline = None;
                sink.send_progress(serialized_client_output(
                    TURN_AUTH_RECOVERY_COMPLETED_LABEL,
                    &recovery,
                )?)?;
            }
            ServerNotification::ModelSafetyBufferingUpdated(status)
                if status.thread_id == thread_id =>
            {
                flush_pending_delta(sink, pending_delta, delta_seq)?;
                *flush_deadline = None;
                sink.send_progress(model_safety_buffering_output(&status)?)?;
            }
            ServerNotification::Warning(warning)
                if warning
                    .thread_id
                    .as_deref()
                    .map_or(true, |warning_thread| warning_thread == thread_id) =>
            {
                flush_pending_delta(sink, pending_delta, delta_seq)?;
                *flush_deadline = None;
                sink.send_progress(serialized_client_output(TURN_WARNING_LABEL, &warning)?)?;
            }
            ServerNotification::GuardianWarning(warning) if warning.thread_id == thread_id => {
                flush_pending_delta(sink, pending_delta, delta_seq)?;
                *flush_deadline = None;
                sink.send_progress(serialized_client_output(TURN_WARNING_LABEL, &warning)?)?;
            }
            ServerNotification::DeprecationNotice(notice) => {
                flush_pending_delta(sink, pending_delta, delta_seq)?;
                *flush_deadline = None;
                sink.send_progress(CodexTurnOutput::client(
                    TURN_WARNING_LABEL,
                    serde_json::json!({
                        "message": notice.summary,
                        "detail": notice.details,
                        "source": "deprecation"
                    }),
                ))?;
            }
            ServerNotification::ConfigWarning(warning) => {
                flush_pending_delta(sink, pending_delta, delta_seq)?;
                *flush_deadline = None;
                sink.send_progress(CodexTurnOutput::client(
                    TURN_WARNING_LABEL,
                    serde_json::json!({
                        "message": warning.summary,
                        "detail": warning.details,
                        "source": "configuration",
                        "path": warning.path,
                        "range": warning.range
                    }),
                ))?;
            }
            ServerNotification::AccountLoginCompleted(notification)
                if provider_account_events_enabled =>
            {
                flush_pending_delta(sink, pending_delta, delta_seq)?;
                *flush_deadline = None;
                self.record_login_completion(&notification).await;
                sink.send_progress(account_login_completed_output(&notification))?;
            }
            ServerNotification::AccountUpdated(notification) if provider_account_events_enabled => {
                flush_pending_delta(sink, pending_delta, delta_seq)?;
                *flush_deadline = None;
                sink.send_progress(account_updated_output(&notification)?)?;
            }
            ServerNotification::AccountRateLimitsUpdated(limits) => {
                flush_pending_delta(sink, pending_delta, delta_seq)?;
                *flush_deadline = None;
                sink.send_progress(serialized_client_output(
                    if provider_account_events_enabled {
                        PROVIDER_ACCOUNT_RATE_LIMITS_UPDATED_LABEL
                    } else {
                        ACCOUNT_RATE_LIMITS_LABEL
                    },
                    &limits,
                )?)?;
            }
            ServerNotification::Error(error) if error.thread_id == thread_id => {
                flush_pending_delta(sink, pending_delta, delta_seq)?;
                *flush_deadline = None;
                let output = turn_error_output(&error);
                if provider_error_is_terminal(&error) {
                    if terminal_error.replace(output).is_some() {
                        return Err(InterfaceDriverError::FailedClosed(
                            "Codex emitted more than one terminal error for one native turn"
                                .to_string(),
                        ));
                    }
                } else {
                    sink.send_progress(output)?;
                }
            }
            ServerNotification::TurnCompleted(turn)
                if turn.thread_id == thread_id && turn.turn.id == turn_id =>
            {
                flush_pending_delta(sink, pending_delta, delta_seq)?;
                sink.set_pending(None);
                let completion = serialized_client_output(TURN_COMPLETED_LABEL, &turn)?;
                if turn.turn.status == TurnStatus::Interrupted && terminal_error.is_none() {
                    sink.send_terminal(completion)?;
                    control.finish();
                    return Ok(DrainStep::Done(TurnDrainOutcome::Complete));
                }
                sink.send_progress(completion)?;
                control.finish();
                if !terminal_turn_may_emit_reply(&turn.turn.status, turn.turn.error.as_ref())? {
                    let terminal_error = terminal_error.take().ok_or_else(|| {
                        InterfaceDriverError::FailedClosed(
                            "Codex terminal turn status has no exact error notification"
                                .to_string(),
                        )
                    })?;
                    sink.send_terminal(terminal_error)?;
                    return Ok(DrainStep::Done(TurnDrainOutcome::Complete));
                }
                if terminal_error.is_some() {
                    return Err(InterfaceDriverError::FailedClosed(
                        "Codex successful terminal status conflicts with an error notification"
                            .to_string(),
                    ));
                }
                let (item_id, text) =
                    completed_turn_reply(&turn.turn, completed_final_answer.as_ref())?;
                if let Some(event) = context.lifecycle_prompt {
                    let value: serde_json::Value = serde_json::from_str(text).map_err(|error| {
                        InterfaceDriverError::FailedClosed(format!(
                            "lifecycle prompt handler returned non-JSON output: {error}"
                        ))
                    })?;
                    let lifecycle = context.lifecycle.ok_or_else(|| {
                        InterfaceDriverError::FailedClosed(
                            "lifecycle prompt turn lost its exact connection".to_string(),
                        )
                    })?;
                    let output = lifecycle
                        .normalize_handler_result(event, "provider_prompt", true, &value)
                        .map_err(InterfaceDriverError::FailedClosed)?;
                    sink.send_terminal_lifecycle(output)?;
                    return Ok(DrainStep::Done(TurnDrainOutcome::Complete));
                }
                let terminal_usage = latest_context_window_usage.ok_or_else(|| {
                    InterfaceDriverError::FailedClosed(
                        "Codex turn completed without positive model-context-window telemetry"
                            .to_string(),
                    )
                })?;
                let provider_token_usage = latest_provider_token_usage.ok_or_else(|| {
                    InterfaceDriverError::FailedClosed(
                        "Codex turn completed without provider token-breakdown telemetry"
                            .to_string(),
                    )
                })?;
                let output = CodexTurnOutput::final_message(
                    &turn.thread_id,
                    &turn.turn.id,
                    item_id,
                    text,
                    Some(terminal_usage),
                )
                .with_provider_token_usage(provider_token_usage);
                if let Some(lifecycle) = context.lifecycle {
                    let continuation = hold_lifecycle_output(sink, output)?;
                    let subject_artifact = match &continuation {
                        LifecycleContinuation::HeldEmission { artifact, .. } => artifact.clone(),
                        _ => unreachable!(),
                    };
                    let triggers = vec![LifecycleTrigger {
                        event: LifecycleEventName::Stop,
                        trigger: "provider_turn_completed".to_string(),
                        match_values: Vec::new(),
                        subject_artifact,
                        data: serde_json::json!({
                            "thread_id": thread_id,
                            "turn_id": turn_id,
                            "status": turn.turn.status,
                        }),
                        continuation: Some(continuation.clone()),
                    }];
                    let lifecycle_blocked = emit_lifecycle_trigger_set(
                        sink,
                        Some(lifecycle),
                        match scope {
                            TransportStateScope::Run(run) => run,
                        },
                        triggers,
                    )?;
                    if !lifecycle_blocked {
                        sink.send_held_lifecycle(&continuation)?;
                    }
                } else {
                    sink.send_terminal(output)?;
                }
                return Ok(DrainStep::Done(TurnDrainOutcome::Complete));
            }
            notification if intentionally_unprojected_server_notification(&notification) => {}
            notification => {
                return Err(InterfaceDriverError::FailedClosed(format!(
                    "Codex emitted an unhandled app-server notification '{}'; every turn-scoped activity family must be projected or rejected explicitly",
                    notification
                )));
            }
        }
        Ok(DrainStep::Next)
    }

    pub(crate) async fn drain_server_request(
        &self,
        context: TurnContext<'_, '_>,
        thread_id: &str,
        turn_id: &str,
        drain: &mut TurnDrain,
        client: &mut AppServerClient,
        request: ServerRequest,
    ) -> Result<DrainStep, InterfaceDriverError> {
        let sink = context.sink;
        let control = context.control;
        let scope = context.scope;
        let interface_operations = context.interface_operations;
        let TurnDrain {
            pending_delta,
            delta_seq,
            ..
        } = drain;
        flush_pending_delta(sink, pending_delta, delta_seq)?;
        if self.reject_retired_context_request(sink, client, &request).await? {
            return Ok(DrainStep::Next);
        }
        let _effect_guard = context.effect_gate.clone().lock_owned().await;
        if context
            .turn_interrupted
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(InterfaceDriverError::FailedClosed(
                "Codex app-server issued a server request after this turn's interrupt was acknowledged"
                    .to_string(),
            ));
        }
        match request {
            ServerRequest::DynamicToolCall { request_id, params } => {
                if params.thread_id != thread_id {
                    return Err(InterfaceDriverError::FailedClosed(format!(
                        "app-server dynamic tool request belongs to thread '{}', active thread is '{}'",
                        params.thread_id, thread_id
                    )));
                }
                if params.turn_id != turn_id {
                    return Err(InterfaceDriverError::FailedClosed(format!(
                        "app-server dynamic tool request belongs to turn '{}', active turn is '{}'",
                        params.turn_id, turn_id
                    )));
                }
                let expected_namespace = self.native_tool_namespace()?;
                if params.namespace.as_deref() != Some(expected_namespace.name.as_str()) {
                    return Err(InterfaceDriverError::FailedClosed(format!(
                        "app-server dynamic tool request namespace {:?} differs from the descriptor-owned namespace '{}'",
                        params.namespace, expected_namespace.name
                    )));
                }
                let tool = context
                    .tool_names
                    .get(&params.tool)
                    .cloned()
                    .ok_or_else(|| {
                        InterfaceDriverError::FailedClosed(format!(
                            "model tool '{}' is not in the admitted catalog",
                            params.tool
                        ))
                    })?;
                if tool.trim().is_empty() || params.call_id.trim().is_empty() {
                    return Err(InterfaceDriverError::Protocol(
                        "app-server dynamic tool request has an empty tool or call id".to_string(),
                    ));
                }
                let call_id = params.call_id;
                let arguments = params.arguments;
                let lifecycle_arguments = arguments.clone();
                sink.send(CodexTurnOutput::dynamic_tool_started(
                    expected_namespace.name,
                    tool.clone(),
                    arguments.clone(),
                    call_id.clone(),
                    params.thread_id.clone(),
                    params.turn_id.clone(),
                ))?;
                if let Some(native) = context.run_invocation
                    && native
                        .affordances
                        .affordances()
                        .iter()
                        .any(|row| row.name.as_str() == tool)
                {
                    let baseline = match native.affordances.planning_baseline(&tool, &arguments) {
                        Ok(baseline) => baseline,
                        Err(error) => {
                            let result = serde_json::json!({"ok":false,"error":error});
                            sink.send(CodexTurnOutput::dynamic_tool_completed(
                                self.native_tool_namespace()?.name,
                                tool,
                                call_id,
                                thread_id.into(),
                                turn_id.into(),
                                false,
                                Some(result.clone()),
                                None,
                            ))?;
                            client
                                .resolve_server_request(
                                    request_id,
                                    native_operation_response(&result)?,
                                )
                                .await
                                .map_err(|error| {
                                    InterfaceDriverError::Transport(error.to_string())
                                })?;
                            return Ok(DrainStep::Next);
                        }
                    };
                    let checking_baseline = baseline.is_some();
                    let current_bytes = match sink.emit.current_run_record(baseline).await {
                        Ok(bytes) => bytes,
                        Err(InterfaceDriverError::FailedClosed(error)) if checking_baseline => {
                            let result = serde_json::json!({"ok":false,"error":error});
                            sink.send(CodexTurnOutput::dynamic_tool_completed(
                                self.native_tool_namespace()?.name,
                                tool,
                                call_id,
                                thread_id.into(),
                                turn_id.into(),
                                false,
                                Some(result.clone()),
                                None,
                            ))?;
                            client
                                .resolve_server_request(
                                    request_id,
                                    native_operation_response(&result)?,
                                )
                                .await
                                .map_err(|error| {
                                    InterfaceDriverError::Transport(error.to_string())
                                })?;
                            return Ok(DrainStep::Next);
                        }
                        Err(error) => return Err(error),
                    };
                    let current =
                        octopus_run_record::RunRecord::from_bytes(&current_bytes)
                            .map_err(|error| {
                                InterfaceDriverError::FailedClosed(error.to_string())
                            })?;
                    if current.id != native.current_record.id
                        || current.worker != native.current_record.worker
                    {
                        return Err(InterfaceDriverError::FailedClosed(
                            "checked native planning read changed run identity".into(),
                        ));
                    }
                    let step = native
                        .affordances
                        .plan_call(
                            native.metadata,
                            native.admitted_record,
                            &current,
                            &call_id,
                            &tool,
                            &arguments,
                        )
                        .unwrap_or_else(|error| NativeStep::Complete {
                            result: serde_json::json!({"ok": false, "error": error}),
                        });
                    match step {
                        NativeStep::Effect {
                            effect,
                            pending: operation,
                            outgoing_bindings,
                        } => {
                            let pending = PendingServerRequest {
                                request_id,
                                thread_id: thread_id.into(),
                                turn_id: turn_id.into(),
                                key: format!("tool:{}", effect.correlation_id),
                                kind: PendingServerRequestKind::DynamicTool {
                                    call_id: effect.correlation_id.clone(),
                                    tool: effect.tool.clone(),
                                },
                                effect_gate: context.effect_gate.clone(),
                                turn_interrupted: context.turn_interrupted.clone(),
                            };
                            self.record_drain_outcome(
                                scope,
                                TurnDrainOutcome::Suspended(pending.clone()),
                            )
                            .await?;
                            self.publish_native_effect(
                                context,
                                &pending,
                                operation,
                                outgoing_bindings,
                                None,
                                client.request_handle(),
                            )?;
                            return Ok(DrainStep::Done(TurnDrainOutcome::Suspended(pending)));
                        }
                        NativeStep::AmendmentEffect {
                            effect,
                            pending: operation,
                            amendment,
                        } => {
                            let pending = PendingServerRequest {
                                request_id,
                                thread_id: thread_id.into(),
                                turn_id: turn_id.into(),
                                key: format!("tool:{}", effect.correlation_id),
                                kind: PendingServerRequestKind::DynamicTool {
                                    call_id: effect.correlation_id.clone(),
                                    tool: effect.tool.clone(),
                                },
                                effect_gate: context.effect_gate.clone(),
                                turn_interrupted: context.turn_interrupted.clone(),
                            };
                            self.record_drain_outcome(
                                scope,
                                TurnDrainOutcome::Suspended(pending.clone()),
                            )
                            .await?;
                            self.publish_native_effect(
                                context,
                                &pending,
                                operation,
                                Vec::new(),
                                Some(amendment),
                                client.request_handle(),
                            )?;
                            return Ok(DrainStep::Done(TurnDrainOutcome::Suspended(pending)));
                        }
                        NativeStep::Complete { result } => {
                            sink.send(CodexTurnOutput::dynamic_tool_completed(
                                self.native_tool_namespace()?.name,
                                tool,
                                call_id,
                                thread_id.into(),
                                turn_id.into(),
                                native_result_success(&result),
                                Some(result.clone()),
                                None,
                            ))?;
                            client
                                .resolve_server_request(
                                    request_id,
                                    native_operation_response(&result)?,
                                )
                                .await
                                .map_err(|error| {
                                    InterfaceDriverError::Transport(error.to_string())
                                })?;
                            return Ok(DrainStep::Next);
                        }
                        NativeStep::Wait { request } => {
                            drop(_effect_guard);
                            let wait = self
                                .wait_for_peer_artifact(
                                    &current,
                                    &call_id,
                                    request.timeout_ms,
                                    context.turn_interrupted,
                                )
                                .await?;
                            if context
                                .turn_interrupted
                                .load(std::sync::atomic::Ordering::Acquire)
                            {
                                return Ok(DrainStep::Next);
                            }
                            let _guard = context.effect_gate.lock().await;
                            if context
                                .turn_interrupted
                                .load(std::sync::atomic::Ordering::Acquire)
                            {
                                return Ok(DrainStep::Next);
                            }
                            let result = &wait.result;
                            sink.send(CodexTurnOutput::dynamic_tool_completed(
                                self.native_tool_namespace()?.name,
                                tool,
                                call_id,
                                thread_id.into(),
                                turn_id.into(),
                                true,
                                Some(result.clone()),
                                None,
                            ))?;
                            client
                                .resolve_server_request(
                                    request_id,
                                    native_operation_response(&result)?,
                                )
                                .await
                                .map_err(|error| {
                                    InterfaceDriverError::Transport(error.to_string())
                                })?;
                            wait.commit().map_err(InterfaceDriverError::FailedClosed)?;
                            return Ok(DrainStep::Next);
                        }
                    }
                }
                let (kind, continuity, output) = match interface_operations.get(&tool) {
                    Some(operation) => (
                        PendingServerRequestKind::InterfaceOperation {
                            call_id: call_id.clone(),
                            operation: operation.name.clone(),
                            interface: operation.interface.clone(),
                            output_label: operation.output_label.clone(),
                        },
                        CodexPendingContinuity::InterfaceOperation {
                            turn_id: params.turn_id.clone(),
                            call_id: call_id.clone(),
                            operation: operation.name.clone(),
                            interface: operation.interface.clone(),
                            output_label: operation.output_label.clone(),
                        },
                        CodexTurnOutput::interface_operation_call(
                            operation,
                            arguments,
                            call_id.clone(),
                        ),
                    ),
                    None => (
                        PendingServerRequestKind::DynamicTool {
                            call_id: call_id.clone(),
                            tool: tool.clone(),
                        },
                        CodexPendingContinuity::DynamicTool {
                            native_operation: None,
                            turn_id: params.turn_id.clone(),
                            call_id: call_id.clone(),
                            tool: tool.clone(),
                        },
                        CodexTurnOutput::native_tool_call(tool.clone(), arguments, call_id.clone()),
                    ),
                };
                let subagent_interface = match (&kind, context.lifecycle) {
                    (
                        PendingServerRequestKind::InterfaceOperation { interface, .. },
                        Some(lifecycle),
                    ) if lifecycle.is_subagent_interface(interface) => Some(interface.clone()),
                    _ => None,
                };
                let pending = PendingServerRequest {
                    request_id,
                    thread_id: thread_id.to_string(),
                    turn_id: params.turn_id,
                    key: format!("tool:{call_id}"),
                    kind,
                    effect_gate: context.effect_gate.clone(),
                    turn_interrupted: context.turn_interrupted.clone(),
                };
                sink.set_pending(Some(continuity));
                if let Some(lifecycle) = context.lifecycle {
                    let held = hold_lifecycle_output(sink, output)?;
                    let subject_artifact = match &held {
                        LifecycleContinuation::HeldEmission { artifact, .. } => artifact.clone(),
                        _ => unreachable!(),
                    };
                    let mut triggers = vec![LifecycleTrigger {
                        event: LifecycleEventName::PreToolUse,
                        trigger: "dynamic_tool_request".to_string(),
                        match_values: vec![tool.clone()],
                        subject_artifact: subject_artifact.clone(),
                        data: serde_json::json!({
                            "tool": tool.clone(),
                            "arguments": lifecycle_arguments,
                            "call_id": call_id.clone(),
                            "thread_id": thread_id,
                            "turn_id": pending.turn_id.clone(),
                            "destination": match &pending.kind {
                                PendingServerRequestKind::DynamicTool { .. } => "tool",
                                PendingServerRequestKind::InterfaceOperation { .. } => "interface",
                                PendingServerRequestKind::Operator { .. } => unreachable!(),
                            }
                        }),
                        continuation: Some(held.clone()),
                    }];
                    if let Some(interface) = subagent_interface {
                        triggers.push(LifecycleTrigger {
                            event: LifecycleEventName::SubagentStart,
                            trigger: "system_worker_dispatch".to_string(),
                            match_values: vec![interface.as_str().to_string()],
                            subject_artifact,
                            data: serde_json::json!({
                                "interface": interface,
                                "operation": tool.clone(),
                                "call_id": call_id.clone(),
                                "thread_id": thread_id,
                                "turn_id": pending.turn_id.clone(),
                            }),
                            continuation: Some(held.clone()),
                        });
                    }
                    if !emit_lifecycle_trigger_set(
                        sink,
                        Some(lifecycle),
                        match scope {
                            TransportStateScope::Run(run) => run,
                        },
                        triggers,
                    )? {
                        sink.send_held_lifecycle(&held)?;
                    }
                } else {
                    sink.send(output)?;
                }
                self.record_drain_outcome(scope, TurnDrainOutcome::Suspended(pending.clone()))
                    .await?;
                control.publish(CodexTurnControlTarget::Suspended {
                    handle: client.request_handle(),
                    request_ids: self.next_request_id.clone(),
                    pending_requests: self.pending_server_requests.clone(),
                    scope: scope.clone(),
                    output: Arc::downgrade(sink.emit),
                    continuity: sink.continuity_snapshot(),
                    handoff_barrier: context.handoff_barrier.clone(),
                    effect_gate: pending.effect_gate.clone(),
                    turn_interrupted: pending.turn_interrupted.clone(),
                    thread_id: pending.thread_id.clone(),
                    turn_id: pending.turn_id.clone(),
                });
                return Ok(DrainStep::Done(TurnDrainOutcome::Suspended(pending)));
            }
            other => match classify_operator_request(&other, &self.operator_continuity()?)? {
                Some(output) => {
                    let pending = pending_operator_request(
                        &other,
                        thread_id,
                        turn_id,
                        context.effect_gate.clone(),
                        context.turn_interrupted.clone(),
                    )?;
                    let PendingServerRequestKind::Operator { method } = &pending.kind else {
                        unreachable!()
                    };
                    sink.set_pending(Some(CodexPendingContinuity::Operator {
                        turn_id: pending.turn_id.clone(),
                        request_key: pending.key.clone(),
                        method: method.clone(),
                    }));
                    if let Some(lifecycle) = context.lifecycle {
                        let held = hold_lifecycle_output(sink, output)?;
                        let subject_artifact = match &held {
                            LifecycleContinuation::HeldEmission { artifact, .. } => {
                                artifact.clone()
                            }
                            _ => unreachable!(),
                        };
                        let request_value = serde_json::to_value(&other).map_err(|error| {
                            InterfaceDriverError::Protocol(format!(
                                "permission_request source did not encode: {error}"
                            ))
                        })?;
                        let blocked = emit_lifecycle_trigger(
                            sink,
                            Some(lifecycle),
                            match scope {
                                TransportStateScope::Run(run) => run,
                            },
                            LifecycleTrigger {
                                event: LifecycleEventName::PermissionRequest,
                                trigger: "operator_approval_request".to_string(),
                                match_values: vec![method.clone()],
                                subject_artifact,
                                data: serde_json::json!({
                                    "method": method,
                                    "request": request_value,
                                    "thread_id": thread_id,
                                    "turn_id": turn_id,
                                }),
                                continuation: Some(held.clone()),
                            },
                        )?;
                        if !blocked {
                            sink.send_held_lifecycle(&held)?;
                        }
                    } else {
                        sink.send(output)?;
                    }
                    self.record_drain_outcome(scope, TurnDrainOutcome::Suspended(pending.clone()))
                        .await?;
                    control.publish(CodexTurnControlTarget::Suspended {
                        handle: client.request_handle(),
                        request_ids: self.next_request_id.clone(),
                        pending_requests: self.pending_server_requests.clone(),
                        scope: scope.clone(),
                        output: Arc::downgrade(sink.emit),
                        continuity: sink.continuity_snapshot(),
                        handoff_barrier: context.handoff_barrier.clone(),
                        effect_gate: pending.effect_gate.clone(),
                        turn_interrupted: pending.turn_interrupted.clone(),
                        thread_id: pending.thread_id.clone(),
                        turn_id: pending.turn_id.clone(),
                    });
                    return Ok(DrainStep::Done(TurnDrainOutcome::Suspended(pending)));
                }
                None => {
                    return Err(InterfaceDriverError::FailedClosed(format!(
                        "app-server issued an out-of-band server request mid-turn \
                             ({}); it is neither the native item/tool/call seam nor an \
                             operator approval/elicitation, failing closed",
                        server_request_method(&other)
                    )));
                }
            },
        }
    }
}
