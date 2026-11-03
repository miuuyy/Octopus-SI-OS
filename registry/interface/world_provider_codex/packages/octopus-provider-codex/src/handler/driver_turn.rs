use super::*;

impl CodexAppServerInterfaceDriver {
    pub(crate) async fn drain_turn(
        &self,
        context: TurnContext<'_, '_>,
        thread_id: &str,
        turn_id: &str,
    ) -> Result<TurnDrainOutcome, InterfaceDriverError> {
        let sink = context.sink;
        sink.bind_live_stream(thread_id, turn_id)?;
        let control = context.control;
        let mut drain = TurnDrain::default();
        let provider_account_events_enabled = self.provider_operations()?.is_some();
        let mut client = self.client.lock().await;
        loop {
            if self.has_pending_request(context.scope).await {
                let event = next_ready_event(&mut client);
                let admitted = !context.turn_interrupted.load(std::sync::atomic::Ordering::Acquire)
                    && event.as_ref().is_some_and(|event| parallel_drain_admits(context, event));
                let step = match (event, admitted) {
                    (Some(AppServerEvent::ServerNotification(notification)), true) => {
                        self.drain_notification(context, thread_id, turn_id, provider_account_events_enabled, &mut drain, notification).await?
                    }
                    (Some(AppServerEvent::ServerRequest(request)), true) => {
                        self.drain_server_request(context, thread_id, turn_id, &mut drain, &mut client, request).await?
                    }
                    (deferred, _) => {
                        client.restore_ready_events(deferred.into_iter().collect());
                        flush_pending_delta(sink, &mut drain.pending_delta, &mut drain.delta_seq)?;
                        let _effect_guard = context.effect_gate.clone().lock_owned().await;
                        if context.turn_interrupted.load(std::sync::atomic::Ordering::Acquire) {
                            return Ok(TurnDrainOutcome::AwaitingResults);
                        }
                        control.publish(CodexTurnControlTarget::Suspended {
                            handle: client.request_handle(),
                            request_ids: self.next_request_id.clone(),
                            pending_requests: self.pending_server_requests.clone(),
                            scope: context.scope.clone(),
                            output: Arc::downgrade(sink.emit),
                            continuity: sink.continuity_snapshot(),
                            handoff_barrier: context.handoff_barrier.clone(),
                            effect_gate: context.effect_gate.clone(),
                            turn_interrupted: context.turn_interrupted.clone(),
                            thread_id: thread_id.to_string(),
                            turn_id: turn_id.to_string(),
                        });
                        return Ok(TurnDrainOutcome::AwaitingResults);
                    }
                };
                if let DrainStep::Done(outcome) = step {
                    return Ok(outcome);
                }
                continue;
            }
            let event = match drain.flush_deadline {
                Some(deadline) => tokio::select! {
                    event = client.next_event() => Some(event),
                    _ = tokio::time::sleep_until(deadline) => None,
                },
                None => Some(client.next_event().await),
            };
            let Some(event) = event else {
                flush_pending_delta(context.sink, &mut drain.pending_delta, &mut drain.delta_seq)?;
                drain.flush_deadline = None;
                continue;
            };
            let step = match event {
                Some(AppServerEvent::ServerNotification(notification)) => {
                    self.drain_notification(context, thread_id, turn_id, provider_account_events_enabled, &mut drain, notification).await?
                }
                Some(AppServerEvent::ServerRequest(request)) => {
                    self.drain_server_request(context, thread_id, turn_id, &mut drain, &mut client, request).await?
                }
                Some(AppServerEvent::UnknownServerNotification(notification)) => {
                    return Err(InterfaceDriverError::FailedClosed(format!(
                        "Codex emitted unknown app-server notification '{}'; provider protocol drift cannot be dropped silently",
                        notification.method
                    )));
                }
                Some(AppServerEvent::Disconnected { message }) => {
                    if let Some(error) = drain.terminal_error.take() {
                        sink.clear_pending();
                        control.finish();
                        sink.send_terminal(error)?;
                        return Ok(TurnDrainOutcome::Complete);
                    }
                    return Err(InterfaceDriverError::Transport(format!(
                        "app-server disconnected mid-turn: {message}"
                    )));
                }
                Some(AppServerEvent::Lagged { skipped }) => {
                    return Err(InterfaceDriverError::FailedClosed(format!(
                        "app-server event stream lagged ({skipped} skipped); turn integrity lost"
                    )));
                }
                None => {
                    if let Some(error) = drain.terminal_error.take() {
                        sink.clear_pending();
                        control.finish();
                        sink.send_terminal(error)?;
                        return Ok(TurnDrainOutcome::Complete);
                    }
                    return Err(InterfaceDriverError::Transport(
                        "app-server event stream closed before TurnCompleted".into(),
                    ));
                }
            };
            if let DrainStep::Done(outcome) = step {
                return Ok(outcome);
            }
        }
    }
}

pub(crate) fn next_ready_event(client: &mut AppServerClient) -> Option<AppServerEvent> {
    let mut ready = client.take_ready_events().into_iter();
    let event = ready.next();
    client.restore_ready_events(ready.collect());
    event
}

pub(crate) fn parallel_drain_admits(context: TurnContext<'_, '_>, event: &AppServerEvent) -> bool {
    match event {
        AppServerEvent::ServerNotification(ServerNotification::TurnCompleted(_)) => false,
        AppServerEvent::ServerNotification(ServerNotification::Error(error)) => {
            !provider_error_is_terminal(error)
        }
        AppServerEvent::ServerNotification(ServerNotification::ItemCompleted(completed)) => !matches!(
            &completed.item,
            ThreadItem::AgentMessage { phase: Some(MessagePhase::FinalAnswer), .. }
        ),
        AppServerEvent::ServerNotification(_) => true,
        AppServerEvent::ServerRequest(ServerRequest::DynamicToolCall { params, .. }) => {
            context.lifecycle.is_none()
                && !context.tool_names.get(&params.tool).is_some_and(|tool| {
                    context.run_invocation.is_some_and(|native| {
                        native.affordances.affordances().iter().any(|row| row.name.as_str() == tool)
                    })
                })
        }
        _ => false,
    }
}
