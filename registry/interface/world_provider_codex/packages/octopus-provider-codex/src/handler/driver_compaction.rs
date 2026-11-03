use super::*;

impl CodexAppServerInterfaceDriver {
    pub(crate) async fn emit_hooks_catalog(&self, sink: &TurnSink<'_>) -> Result<(), InterfaceDriverError> {
        let request = ClientRequest::HooksList {
            request_id: self.mint_request_id(),
            params: HooksListParams { cwds: Vec::new() },
        };
        let response = {
            let client = self.client.lock().await;
            client.request_typed::<HooksListResponse>(request).await
        };
        match response {
            Ok(response) => {
                sink.send(hook_catalog_output(&response.data))?;
            }
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "hooks/list unavailable on this provider thread"
                );
                sink.send(hook_catalog_error_output(&error.to_string()))?;
            }
        }
        Ok(())
    }

    pub(crate) async fn run_manual_compaction(
        &self,
        context: TurnContext<'_, '_>,
        thread_id: &str,
    ) -> Result<(), InterfaceDriverError> {
        let request = ClientRequest::ThreadCompactStart {
            request_id: self.mint_request_id(),
            params: ThreadCompactStartParams {
                thread_id: thread_id.to_string(),
            },
        };
        self.require_local_process_transport("compact").await?;
        context
            .sink
            .admit_app_server_dispatch(&request, None)
            .await?;
        let request_handle = {
            let client = self.client.lock().await;
            let request_handle = client.request_handle();
            client
                .request_typed::<ThreadCompactStartResponse>(request)
                .await
                .map_err(typed_error)?;
            request_handle
        };
        let turn_id = self
            .await_manual_compaction_turn_started(context.sink, thread_id)
            .await?;
        context.control.publish(CodexTurnControlTarget::Draining {
            handle: request_handle,
            request_ids: self.next_request_id.clone(),
            pending_requests: self.pending_server_requests.clone(),
            scope: context.scope.clone(),
            output: Arc::downgrade(context.sink.emit),
            continuity: context.sink.continuity_snapshot(),
            handoff_barrier: context.handoff_barrier.clone(),
            effect_gate: context.effect_gate.clone(),
            turn_interrupted: context.turn_interrupted.clone(),
            thread_id: thread_id.to_string(),
            turn_id: turn_id.clone(),
        });
        self.drain_manual_compaction(context, thread_id, &turn_id)
            .await
    }

    pub(crate) async fn await_manual_compaction_turn_started(
        &self,
        sink: &TurnSink<'_>,
        thread_id: &str,
    ) -> Result<String, InterfaceDriverError> {
        let mut client = self.client.lock().await;
        loop {
            match client.next_event().await {
                Some(AppServerEvent::ServerNotification(ServerNotification::TurnStarted(turn)))
                    if turn.thread_id == thread_id =>
                {
                    if turn.turn.id.trim().is_empty() || turn.turn.status != TurnStatus::InProgress
                    {
                        return Err(InterfaceDriverError::FailedClosed(format!(
                            "manual compact started an invalid turn: id='{}' status={:?}",
                            turn.turn.id, turn.turn.status
                        )));
                    }
                    sink.bind_live_stream(thread_id, &turn.turn.id)?;
                    sink.send(serialized_client_output(TURN_STARTED_LABEL, &turn)?)?;
                    return Ok(turn.turn.id);
                }
                Some(AppServerEvent::ServerNotification(notification)) => {
                    if !server_notification_matches_thread(&notification, thread_id)?
                        || intentionally_unprojected_server_notification(&notification)
                    {
                        continue;
                    }
                    return Err(InterfaceDriverError::FailedClosed(format!(
                        "Codex emitted '{}' before the manual compact turn started",
                        notification
                    )));
                }
                Some(AppServerEvent::ServerRequest(request)) => {
                    return Err(InterfaceDriverError::FailedClosed(format!(
                        "Codex issued '{}' before the manual compact turn started",
                        server_request_method(&request)
                    )));
                }
                Some(AppServerEvent::UnknownServerNotification(notification)) => {
                    return Err(InterfaceDriverError::FailedClosed(format!(
                        "Codex emitted unknown app-server notification '{}' before manual compact started",
                        notification.method
                    )));
                }
                Some(AppServerEvent::Disconnected { message }) => {
                    return Err(InterfaceDriverError::Transport(format!(
                        "app-server disconnected before manual compact started: {message}"
                    )));
                }
                Some(AppServerEvent::Lagged { skipped }) => {
                    return Err(InterfaceDriverError::FailedClosed(format!(
                        "app-server event stream lagged ({skipped} skipped) before manual compact started"
                    )));
                }
                None => {
                    return Err(InterfaceDriverError::Transport(
                        "app-server event stream closed before manual compact started".to_string(),
                    ));
                }
            }
        }
    }

    pub(crate) async fn drain_manual_compaction(
        &self,
        context: TurnContext<'_, '_>,
        thread_id: &str,
        turn_id: &str,
    ) -> Result<(), InterfaceDriverError> {
        context.sink.bind_live_stream(thread_id, turn_id)?;
        let mut compaction_item_id: Option<String> = None;
        let mut compaction_completed = false;
        let mut compaction_output: Option<CodexTurnOutput> = None;
        let mut terminal_error: Option<CodexTurnOutput> = None;
        let provider_account_events_enabled = self.provider_operations()?.is_some();
        let mut client = self.client.lock().await;
        loop {
            match client.next_event().await {
                Some(AppServerEvent::ServerNotification(notification)) => {
                    let _notification_guard = context.effect_gate.lock().await;
                    if !server_notification_matches_turn(&notification, thread_id, turn_id)? {
                        continue;
                    }
                    match notification {
                        ServerNotification::ItemStarted(started)
                            if started.thread_id == thread_id =>
                        {
                            match started.item {
                                ThreadItem::ContextCompaction { id } => {
                                    if compaction_item_id.replace(id).is_some() {
                                        return Err(InterfaceDriverError::FailedClosed(
                                            "manual compact started more than one context-compaction item"
                                                .to_string(),
                                        ));
                                    }
                                }
                                ThreadItem::UserMessage { .. }
                                | ThreadItem::FunctionCallOutput { .. }
                                | ThreadItem::AgentMessage { .. }
                                | ThreadItem::Reasoning { .. } => {}
                                item => {
                                    return Err(InterfaceDriverError::FailedClosed(format!(
                                        "manual compact emitted unexpected item start '{item:?}'"
                                    )));
                                }
                            }
                        }
                        ServerNotification::ItemCompleted(ItemCompletedNotification {
                            item,
                            thread_id: item_thread,
                            turn_id: item_turn,
                            completed_at_ms,
                        }) if item_thread == thread_id => match item {
                            ThreadItem::ContextCompaction { id } => {
                                if compaction_completed
                                    || compaction_item_id.as_deref() != Some(id.as_str())
                                {
                                    return Err(InterfaceDriverError::FailedClosed(
                                            "manual compact completed without one matching started context-compaction item"
                                                .to_string(),
                                        ));
                                }
                                compaction_output = Some(manual_compaction_completed_output(
                                    id,
                                    item_thread.as_str(),
                                    item_turn.as_str(),
                                    completed_at_ms,
                                )?);
                                compaction_completed = true;
                            }
                            ThreadItem::UserMessage { .. }
                            | ThreadItem::FunctionCallOutput { .. }
                            | ThreadItem::AgentMessage { .. }
                            | ThreadItem::Reasoning { .. } => {}
                            item => {
                                return Err(InterfaceDriverError::FailedClosed(format!(
                                    "manual compact emitted unexpected item completion '{item:?}'"
                                )));
                            }
                        },
                        ServerNotification::AgentMessageDelta(_)
                        | ServerNotification::ReasoningSummaryTextDelta(_)
                        | ServerNotification::ReasoningSummaryPartAdded(_)
                        | ServerNotification::ReasoningTextDelta(_)
                        | ServerNotification::ThreadTokenUsageUpdated(_)
                        | ServerNotification::ContextCompacted(_) => {}
                        ServerNotification::Warning(warning)
                            if warning
                                .thread_id
                                .as_deref()
                                .map_or(true, |candidate| candidate == thread_id) =>
                        {
                            context
                                .sink
                                .send(serialized_client_output(TURN_WARNING_LABEL, &warning)?)?;
                        }
                        ServerNotification::GuardianWarning(warning)
                            if warning.thread_id == thread_id =>
                        {
                            context
                                .sink
                                .send(serialized_client_output(TURN_WARNING_LABEL, &warning)?)?;
                        }
                        ServerNotification::DeprecationNotice(notice) => {
                            context.sink.send(CodexTurnOutput::client(
                                TURN_WARNING_LABEL,
                                serde_json::json!({
                                    "message": notice.summary,
                                    "detail": notice.details,
                                    "source": "deprecation"
                                }),
                            ))?;
                        }
                        ServerNotification::ConfigWarning(warning) => {
                            context.sink.send(CodexTurnOutput::client(
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
                        ServerNotification::ModelRerouted(model)
                            if model.thread_id == thread_id =>
                        {
                            let selected_model = self
                                .resolve_private_config_string("model")?
                                .ok_or_else(|| {
                                    InterfaceDriverError::FailedClosed(
                                        "Codex model reroute arrived without a descriptor-bound selected model"
                                            .to_string(),
                                    )
                                })?;
                            validate_model_reroute(
                                &selected_model,
                                &model.from_model,
                                &model.to_model,
                            )?;
                            context.sink.send(serialized_client_output(
                                TURN_MODEL_REROUTED_LABEL,
                                &model,
                            )?)?;
                        }
                        ServerNotification::ModelVerification(model)
                            if model.thread_id == thread_id =>
                        {
                            context.sink.send(serialized_client_output(
                                TURN_MODEL_VERIFICATION_LABEL,
                                &model,
                            )?)?;
                        }
                        ServerNotification::AuthRecoveryStarted(recovery)
                            if recovery.thread_id == thread_id && recovery.turn_id == turn_id =>
                        {
                            context.sink.send(serialized_client_output(
                                TURN_AUTH_RECOVERY_STARTED_LABEL,
                                &recovery,
                            )?)?;
                        }
                        ServerNotification::AuthRecoveryCompleted(recovery)
                            if recovery.thread_id == thread_id && recovery.turn_id == turn_id =>
                        {
                            context.sink.send(serialized_client_output(
                                TURN_AUTH_RECOVERY_COMPLETED_LABEL,
                                &recovery,
                            )?)?;
                        }
                        ServerNotification::ModelSafetyBufferingUpdated(status)
                            if status.thread_id == thread_id =>
                        {
                            context.sink.send(model_safety_buffering_output(&status)?)?;
                        }
                        ServerNotification::AccountLoginCompleted(notification)
                            if provider_account_events_enabled =>
                        {
                            self.record_login_completion(&notification).await;
                            context
                                .sink
                                .send(account_login_completed_output(&notification))?;
                        }
                        ServerNotification::AccountUpdated(notification)
                            if provider_account_events_enabled =>
                        {
                            context.sink.send(account_updated_output(&notification)?)?;
                        }
                        ServerNotification::AccountRateLimitsUpdated(limits) => {
                            context.sink.send(serialized_client_output(
                                if provider_account_events_enabled {
                                    PROVIDER_ACCOUNT_RATE_LIMITS_UPDATED_LABEL
                                } else {
                                    ACCOUNT_RATE_LIMITS_LABEL
                                },
                                &limits,
                            )?)?;
                        }
                        ServerNotification::Error(error) if error.thread_id == thread_id => {
                            let output = turn_error_output(&error);
                            if provider_error_is_terminal(&error) {
                                if terminal_error.replace(output).is_some() {
                                    return Err(InterfaceDriverError::FailedClosed(
                                        "manual compact emitted more than one terminal error"
                                            .to_string(),
                                    ));
                                }
                            } else {
                                context.sink.send(output)?;
                            }
                        }
                        ServerNotification::TurnCompleted(turn)
                            if turn.thread_id == thread_id && turn.turn.id == turn_id =>
                        {
                            context.sink.clear_pending();
                            let completion = serialized_client_output(TURN_COMPLETED_LABEL, &turn)?;
                            if turn.turn.status == TurnStatus::Interrupted && terminal_error.is_none() {
                                context.sink.send_terminal(completion)?;
                                context.control.finish();
                                return Ok(());
                            }
                            context.sink.send(completion)?;
                            context.control.finish();
                            if terminal_turn_may_emit_reply(
                                &turn.turn.status,
                                turn.turn.error.as_ref(),
                            )? {
                                if terminal_error.is_some() || !compaction_completed {
                                    return Err(InterfaceDriverError::FailedClosed(
                                        "manual compact terminal state conflicts with its context-compaction item"
                                            .to_string(),
                                    ));
                                }
                                context.sink.send_terminal(
                                    compaction_output.take().ok_or_else(|| {
                                        InterfaceDriverError::FailedClosed(
                                            "manual compact lost its completed semantic output"
                                                .to_string(),
                                        )
                                    })?,
                                )?;
                            } else {
                                context.sink.send_terminal(terminal_error.ok_or_else(|| {
                                    InterfaceDriverError::FailedClosed(
                                        "manual compact failed without an exact error notification"
                                            .to_string(),
                                    )
                                })?)?;
                            }
                            return Ok(());
                        }
                        ServerNotification::TurnStarted(_) => {
                            return Err(InterfaceDriverError::FailedClosed(
                                "manual compact received a duplicate turn start".to_string(),
                            ));
                        }
                        notification
                            if intentionally_unprojected_server_notification(&notification) => {}
                        notification => {
                            return Err(InterfaceDriverError::FailedClosed(format!(
                                "Codex emitted unhandled manual compact notification '{}'",
                                notification
                            )));
                        }
                    }
                }
                Some(AppServerEvent::ServerRequest(request)) => {
                    return Err(InterfaceDriverError::FailedClosed(format!(
                        "manual compact issued unexpected server request '{}'",
                        server_request_method(&request)
                    )));
                }
                Some(AppServerEvent::UnknownServerNotification(notification)) => {
                    return Err(InterfaceDriverError::FailedClosed(format!(
                        "Codex emitted unknown app-server notification '{}' during manual compact",
                        notification.method
                    )));
                }
                Some(AppServerEvent::Disconnected { message }) => {
                    if let Some(error) = terminal_error.take() {
                        context.sink.clear_pending();
                        context.control.finish();
                        context.sink.send_terminal(error)?;
                        return Ok(());
                    }
                    return Err(InterfaceDriverError::Transport(format!(
                        "app-server disconnected during manual compact: {message}"
                    )));
                }
                Some(AppServerEvent::Lagged { skipped }) => {
                    return Err(InterfaceDriverError::FailedClosed(format!(
                        "app-server event stream lagged ({skipped} skipped) during manual compact"
                    )));
                }
                None => {
                    if let Some(error) = terminal_error.take() {
                        context.sink.clear_pending();
                        context.control.finish();
                        context.sink.send_terminal(error)?;
                        return Ok(());
                    }
                    return Err(InterfaceDriverError::Transport(
                        "app-server event stream closed before manual compact completed"
                            .to_string(),
                    ));
                }
            }
        }
    }
}
