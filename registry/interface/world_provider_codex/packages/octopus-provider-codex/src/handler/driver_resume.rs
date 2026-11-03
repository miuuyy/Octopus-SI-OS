use super::*;

impl CodexAppServerInterfaceDriver {
    pub(crate) async fn resume_tool_result(
        &self,
        context: TurnContext<'_, '_>,
        result: ToolResultArtifact,
        recovered: Option<&CodexContinuityState>,
        context_generation: u64,
        interrupted_activity_already_admitted: bool,
    ) -> Result<bool, InterfaceDriverError> {
        result.validate().map_err(InterfaceDriverError::Protocol)?;
        let Some(correlation_id) = result.correlation_id.as_deref() else {
            if self.has_pending_request(context.scope).await {
                return Err(InterfaceDriverError::FailedClosed(
                    "tool result arrived without correlation_id while this run has a pending app-server request"
                        .to_string(),
                ));
            }
            return Ok(false);
        };
        let key = format!("tool:{correlation_id}");
        let Some(pending) = self.pending_request(context.scope, &key).await? else {
            return Ok(false);
        };
        context
            .sink
            .bind_live_stream(&pending.thread_id, &pending.turn_id)?;
        let effect_guard = pending.effect_gate.clone().lock_owned().await;
        self.ensure_pending_request_current(context.scope, &pending)
            .await?;
        match &pending.kind {
            PendingServerRequestKind::DynamicTool { call_id, tool }
                if call_id == correlation_id && tool == &result.tool => {}
            PendingServerRequestKind::DynamicTool { call_id, tool } => {
                return Err(InterfaceDriverError::FailedClosed(format!(
                    "tool result correlation resolved to call_id='{}' tool='{}', but artifact names correlation_id='{}' tool='{}'",
                    call_id, tool, correlation_id, result.tool
                )));
            }
            PendingServerRequestKind::InterfaceOperation { operation, .. } => {
                return Err(InterfaceDriverError::FailedClosed(format!(
                    "tool result key resolved to pending interface operation '{operation}'"
                )));
            }
            PendingServerRequestKind::Operator { method } => {
                return Err(InterfaceDriverError::FailedClosed(format!(
                    "tool result key resolved to pending operator method '{method}'"
                )));
            }
        }
        let interrupted_by_control = pending
            .turn_interrupted
            .load(std::sync::atomic::Ordering::Acquire);
        if interrupted_by_control
            || recovered.is_some_and(|state| state.interrupted_pending.is_some())
        {
            let interrupted = self.interrupted_result_source(context, recovered, &pending, "tool")?;
            validate_recovered_tool_result(
                &result,
                interrupted.outstanding_call(&pending.key).ok_or_else(|| {
                    InterfaceDriverError::FailedClosed(
                        "interrupted live tool result lost its durable pending correlation".to_string(),
                    )
                })?,
            )?;
            self.validate_pending_continuity(
                context.scope,
                recovered,
                &pending,
                context_generation,
            )
            .await?;
            let closure = CodexTurnOutput::interrupted_result_closed(
                "tool_result",
                correlation_id.to_string(),
                pending.thread_id.clone(),
                pending.turn_id.clone(),
                result.ok,
                context.source_artifact,
            );
            let closure = self
                .settle_interrupted_request(context, &interrupted, pending.clone(), effect_guard, closure)
                .await?;
            let native_namespace = self.native_tool_namespace()?.name;
            let activity = (!interrupted_activity_already_admitted).then(|| {
                CodexTurnOutput::dynamic_tool_completed_after_interrupt(
                    native_namespace,
                    result.tool.clone(),
                    correlation_id.to_string(),
                    pending.thread_id.clone(),
                    pending.turn_id.clone(),
                    result.ok,
                    result.body.clone(),
                    result.error.clone(),
                )
            });
            context.sink.send_interrupted_result(
                activity,
                interrupted_activity_already_admitted,
                closure,
            )?;
            return Ok(true);
        }
        self.validate_pending_continuity(context.scope, recovered, &pending, context_generation)
            .await?;
        let source = recovered.ok_or_else(|| InterfaceDriverError::FailedClosed("ordinary tool result has no source continuity".into()))?;
        if self.complete_native_terminal_result(
            context, source, &pending.key, Some(&pending),
            CodexTurnOutput::dynamic_tool_completed_after_interrupt(
                self.native_tool_namespace()?.name, result.tool.clone(), correlation_id.to_string(),
                pending.thread_id.clone(), pending.turn_id.clone(), result.ok, result.body.clone(), result.error.clone(),
            ), interrupted_activity_already_admitted,
            CodexTurnOutput::interrupted_result_closed("tool_result", correlation_id.to_string(), pending.thread_id.clone(), pending.turn_id.clone(), result.ok, context.source_artifact),
            false,
        ).await?.settled() { return Ok(true); }
        if let Some(operation) = source.outstanding_call(&pending.key).map(native_pending_operation).transpose()?.flatten() {
            let receipt = serde_json::to_value(&result).map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
            match operation.advance(&receipt).map_err(InterfaceDriverError::FailedClosed)? {
                NativeStep::Effect { effect, pending: next_operation, outgoing_bindings } => {
                    let mut next = pending.clone();
                    next.key = format!("tool:{}", effect.correlation_id);
                    next.kind = PendingServerRequestKind::DynamicTool { call_id: effect.correlation_id, tool: effect.tool };
                    self.remove_pending_request(context.scope, &pending).await?;
                    self.record_drain_outcome(context.scope, TurnDrainOutcome::Suspended(next.clone())).await?;
                    self.publish_native_effect(context, &next, next_operation, outgoing_bindings, None, self.client.lock().await.request_handle())?;
                    return Ok(true);
                }
                NativeStep::AmendmentEffect { effect, pending: next_operation, amendment } => {
                    let mut next = pending.clone();
                    next.key = format!("tool:{}", effect.correlation_id);
                    next.kind = PendingServerRequestKind::DynamicTool { call_id: effect.correlation_id, tool: effect.tool };
                    self.remove_pending_request(context.scope, &pending).await?;
                    self.record_drain_outcome(context.scope, TurnDrainOutcome::Suspended(next.clone())).await?;
                    self.publish_native_effect(context, &next, next_operation, Vec::new(), Some(amendment), self.client.lock().await.request_handle())?;
                    return Ok(true);
                }
                NativeStep::Complete { result } => {
                    context.sink.settle_pending(&pending.key);
                    context.sink.send(CodexTurnOutput::dynamic_tool_completed(
                        self.native_tool_namespace()?.name, operation.operation_name, operation.call_id,
                        pending.thread_id.clone(), pending.turn_id.clone(), native_result_success(&result), Some(result.clone()), None,
                    ))?;
                    self.resolve_and_continue(context, pending, native_operation_response(&result)?, effect_guard).await?;
                    return Ok(true);
                }
                NativeStep::Wait { .. } => return Err(InterfaceDriverError::FailedClosed("an admitted tool result cannot turn into an unrequested wait".into())),
            }
        }
        context.sink.send(CodexTurnOutput::dynamic_tool_completed(
            self.native_tool_namespace()?.name,
            result.tool.clone(),
            correlation_id.to_string(),
            pending.thread_id.clone(),
            pending.turn_id.clone(),
            result.ok,
            result.body.clone(),
            result.error.clone(),
        ))?;
        let response = dynamic_tool_response_value(&result, context.source_artifact, self.model_result_projection()?)?;
        self.resolve_and_continue(context, pending, response, effect_guard)
            .await?;
        Ok(true)
    }

    pub(crate) fn interrupted_result_source(
        &self,
        context: TurnContext<'_, '_>,
        recovered: Option<&CodexContinuityState>,
        pending: &PendingServerRequest,
        family: &str,
    ) -> Result<CodexContinuityState, InterfaceDriverError> {
        if let Some(state) = recovered.filter(|state| state.interrupted_pending.is_some()) {
            return Ok(state.clone());
        }
        let recovered = recovered.ok_or_else(|| {
            InterfaceDriverError::FailedClosed(format!(
                "interrupted live {family} result has no durable continuity artifact"
            ))
        })?;
        if recovered.outstanding_call(&pending.key).is_none() {
            return Err(InterfaceDriverError::FailedClosed(format!(
                "interrupted live {family} result lost its durable pending correlation"
            )));
        }
        let (continuity, retained) = interrupted_continuity(Some(recovered.clone()))
            .map_err(InterfaceDriverError::FailedClosed)?;
        if !retained {
            return Err(InterfaceDriverError::FailedClosed(format!(
                "interrupted live {family} result does not carry an external effect"
            )));
        }
        let continuity = continuity.expect("interrupted continuity remains durable");
        context.sink.set_continuity(continuity.clone());
        Ok(continuity)
    }

    pub(crate) async fn settle_interrupted_request(
        &self,
        context: TurnContext<'_, '_>,
        interrupted: &CodexContinuityState,
        pending: PendingServerRequest,
        effect_guard: tokio::sync::OwnedMutexGuard<()>,
        closure: CodexTurnOutput,
    ) -> Result<CodexTurnOutput, InterfaceDriverError> {
        if interrupted.outstanding_len() > 1 {
            self.remove_pending_request(context.scope, &pending).await?;
            drop(effect_guard);
            return Ok(closure);
        }
        let drain = self
            .drain_interrupted_result(context, pending, effect_guard)
            .await?;
        closure.with_interrupted_drain(drain)
    }

    pub(crate) fn publish_native_effect(
        &self,
        context: TurnContext<'_, '_>,
        pending: &PendingServerRequest,
        operation: NativePending,
        bindings: Vec<octopus_run_authoring::ProviderOutgoingBinding>,
        amendment: Option<octopus_run_authoring::ProviderCapabilityAmendment>,
        handle: AppServerRequestHandle,
    ) -> Result<(), InterfaceDriverError> {
        emit_native_effect(context.sink, &pending.turn_id, operation, bindings, amendment)?;
        context.control.publish(CodexTurnControlTarget::Suspended {
            handle, request_ids: self.next_request_id.clone(), pending_requests: self.pending_server_requests.clone(),
            scope: context.scope.clone(), output: Arc::downgrade(context.sink.emit), continuity: context.sink.continuity_snapshot(),
            handoff_barrier: context.handoff_barrier.clone(), effect_gate: pending.effect_gate.clone(),
            turn_interrupted: pending.turn_interrupted.clone(), thread_id: pending.thread_id.clone(), turn_id: pending.turn_id.clone(),
            late_calls: None,
        });
        Ok(())
    }

    pub(crate) async fn resume_interface_operation_result(
        &self,
        context: TurnContext<'_, '_>,
        result: &InterfaceOperationResultArtifact,
        recovered: Option<&CodexContinuityState>,
        context_generation: u64,
        interrupted_activity_already_admitted: bool,
    ) -> Result<bool, InterfaceDriverError> {
        let key = format!("tool:{}", result.correlation_id);
        let Some(pending) = self.pending_request(context.scope, &key).await? else {
            return Ok(false);
        };
        context
            .sink
            .bind_live_stream(&pending.thread_id, &pending.turn_id)?;
        let effect_guard = pending.effect_gate.clone().lock_owned().await;
        self.ensure_pending_request_current(context.scope, &pending)
            .await?;
        match &pending.kind {
            PendingServerRequestKind::InterfaceOperation {
                call_id,
                operation,
                interface,
                output_label,
            } if call_id == &result.correlation_id
                && operation == &result.operation
                && output_label == &result.label =>
            {
                let formed = context.interface_operations.get(operation).ok_or_else(|| {
                    InterfaceDriverError::FailedClosed(format!(
                        "pending interface operation '{}' disappeared from the formed catalog",
                        operation
                    ))
                })?;
                if formed.interface != *interface || formed.output_label != *output_label {
                    return Err(InterfaceDriverError::FailedClosed(format!(
                        "pending interface operation '{}' no longer matches its formed exact route",
                        operation
                    )));
                }
            }
            PendingServerRequestKind::InterfaceOperation {
                call_id,
                operation,
                interface,
                output_label,
            } => {
                return Err(InterfaceDriverError::FailedClosed(format!(
                    "interface result correlation resolved to call_id='{}' operation='{}' interface='{}' output_label='{}', but artifact names correlation_id='{}' operation='{}' label='{}'",
                    call_id,
                    operation,
                    interface.as_str(),
                    output_label,
                    result.correlation_id,
                    result.operation,
                    result.label
                )));
            }
            PendingServerRequestKind::DynamicTool { tool, .. } => {
                return Err(InterfaceDriverError::FailedClosed(format!(
                    "interface operation result key resolved to pending tool '{tool}'"
                )));
            }
            PendingServerRequestKind::Operator { method } => {
                return Err(InterfaceDriverError::FailedClosed(format!(
                    "interface operation result key resolved to pending operator method '{method}'"
                )));
            }
        }
        let interrupted_by_control = pending
            .turn_interrupted
            .load(std::sync::atomic::Ordering::Acquire);
        if interrupted_by_control
            || recovered.is_some_and(|state| state.interrupted_pending.is_some())
        {
            let interrupted = self.interrupted_result_source(context, recovered, &pending, "interface")?;
            validate_recovered_interface_operation_input(
                result,
                interrupted.outstanding_call(&pending.key).ok_or_else(|| {
                    InterfaceDriverError::FailedClosed(
                        "interrupted live interface result lost its durable pending correlation"
                            .to_string(),
                    )
                })?,
            )?;
            self.validate_pending_continuity(
                context.scope,
                recovered,
                &pending,
                context_generation,
            )
            .await?;
            let closure = CodexTurnOutput::interrupted_result_closed(
                "interface_operation_result",
                result.correlation_id.clone(),
                pending.thread_id.clone(),
                pending.turn_id.clone(),
                result.ok,
                context.source_artifact,
            );
            let closure = self
                .settle_interrupted_request(context, &interrupted, pending.clone(), effect_guard, closure)
                .await?;
            let activity = (!interrupted_activity_already_admitted).then(|| {
                CodexTurnOutput::interface_operation_completed_after_interrupt(
                    result.operation.clone(),
                    result.correlation_id.clone(),
                    pending.thread_id.clone(),
                    pending.turn_id.clone(),
                    result.ok,
                    result.value.clone(),
                )
            });
            context.sink.send_interrupted_result(
                activity,
                interrupted_activity_already_admitted,
                closure,
            )?;
            return Ok(true);
        }
        self.validate_pending_continuity(context.scope, recovered, &pending, context_generation)
            .await?;
        let source = recovered.ok_or_else(|| InterfaceDriverError::FailedClosed("ordinary interface result has no source continuity".into()))?;
        if self.complete_native_terminal_result(
            context, source, &pending.key, Some(&pending),
            CodexTurnOutput::interface_operation_completed_after_interrupt(
                result.operation.clone(), result.correlation_id.clone(), pending.thread_id.clone(), pending.turn_id.clone(), result.ok, result.value.clone(),
            ), interrupted_activity_already_admitted,
            CodexTurnOutput::interrupted_result_closed("interface_operation_result", result.correlation_id.clone(), pending.thread_id.clone(), pending.turn_id.clone(), result.ok, context.source_artifact),
            false,
        ).await?.settled() { return Ok(true); }
        context
            .sink
            .send(CodexTurnOutput::interface_operation_completed(
                result.operation.clone(),
                result.correlation_id.clone(),
                pending.thread_id.clone(),
                pending.turn_id.clone(),
                result.ok,
                result.value.clone(),
            ))?;
        let response = interface_operation_response_value(result, context.source_artifact, self.model_result_projection()?)?;
        self.resolve_and_continue(context, pending, response, effect_guard)
            .await?;
        Ok(true)
    }

    pub(crate) async fn read_pending_operator_turn(
        &self,
        client: &AppServerClient,
        thread_id: &str,
        turn_id: &str,
    ) -> Result<Vec<octopus_appserver_proto::ThreadItemEntry>, InterfaceDriverError> {
        let handle = client.request_handle();
        let turn = read_native_turn(&handle, &self.next_request_id, thread_id, turn_id).await?
            .ok_or_else(|| InterfaceDriverError::FailedClosed(format!(
                "thread/turns/list did not restore pending operator turn '{turn_id}' on thread '{thread_id}'",
            )))?;
        if turn.status != TurnStatus::InProgress {
            return Err(InterfaceDriverError::FailedClosed(format!(
                "thread/turns/list pending operator turn '{turn_id}' is not in progress",
            )));
        }
        native_history::read_native_turn_entries(&handle, &self.next_request_id, thread_id, turn_id).await
    }

    pub(crate) async fn resume_replayed_operator_response(
        &self,
        context: TurnContext<'_, '_>,
        response: OperatorResponseArtifact,
        recovered: &CodexContinuityState,
        durable_pending: &CodexPendingContinuity,
        context_generation: u64,
        model: &str,
        bound_instructions: &str,
        native_affordances: &[DynamicToolSpec],
        static_context_fingerprint: &str,
    ) -> Result<(), InterfaceDriverError> {
        let CodexPendingContinuity::Operator {
            turn_id,
            request_key,
            method,
        } = durable_pending
        else {
            return Err(InterfaceDriverError::FailedClosed(
                "operator response recovery names a non-operator pending request".to_string(),
            ));
        };
        let fingerprint = thread_shape_fingerprint(
            Some(model),
            bound_instructions,
            native_affordances,
            static_context_fingerprint,
        )?;
        if recovered.thread_shape_fingerprint != fingerprint
            || recovered.static_context_fingerprint != static_context_fingerprint
            || recovered.context_generation != context_generation
        {
            return Err(InterfaceDriverError::FailedClosed(
                "operator response replay conflicts with its durable thread coordinates"
                    .to_string(),
            ));
        }
        let continuity = self.operator_continuity()?;
        let resume = ClientRequest::ThreadResume {
            request_id: self.mint_request_id(),
            params: ThreadResumeParams {
                thread_id: recovered.thread_id.clone(),
                model: Some(model.to_string()),
                service_tier: self.bound_service_tier()?,
                config: Some(self.native_tool_thread_config()?),
                developer_instructions: Some(bound_instructions.to_string()),
                sandbox: Some(self.native_thread_sandbox()?),
                exclude_turns: true,
                ..ThreadResumeParams::default()
            },
        };
        let replayed = {
            let mut client = self.client.lock().await;
            let resumed: ThreadResumeResponse = client
                .request_typed::<ThreadResumeResponse>(resume)
                .await
                .map_err(typed_error)?;
            validate_native_service_tier_response(
                self.bound_service_tier()?.as_ref().and_then(Option::as_deref),
                resumed.service_tier.as_deref(),
            )?;
            if resumed.thread.id != recovered.thread_id {
                return Err(InterfaceDriverError::FailedClosed(format!(
                    "thread/resume did not restore pending operator turn '{}' on thread '{}'",
                    turn_id, recovered.thread_id
                )));
            }
            let entries = self.read_pending_operator_turn(&client, &resumed.thread.id, turn_id).await?;
            context.sink.bind_live_stream(&resumed.thread.id, turn_id)?;
            for entry in &entries {
                context.sink.record_input_item(
                    &entry.item,
                    &resumed.thread.id,
                    turn_id,
                    InputRecordedSource::ThreadResume,
                    entry.started_at_ms,
                )?;
            }

            let deadline = tokio::time::Instant::now()
                + std::time::Duration::from_millis(continuity.replay_timeout_ms);
            let mut buffered = Vec::new();
            loop {
                let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                if remaining.is_zero() {
                    client.restore_ready_events(buffered);
                    return Err(InterfaceDriverError::FailedClosed(format!(
                        "thread/resume did not replay exact operator request '{}' before the descriptor timeout",
                        request_key
                    )));
                }
                let event = match tokio::time::timeout(remaining, client.next_event()).await {
                    Ok(Some(event)) => event,
                    Ok(None) => {
                        client.restore_ready_events(buffered);
                        return Err(InterfaceDriverError::Transport(
                            "app-server event stream closed before pending operator replay"
                                .to_string(),
                        ));
                    }
                    Err(_) => {
                        client.restore_ready_events(buffered);
                        return Err(InterfaceDriverError::FailedClosed(format!(
                            "thread/resume did not replay exact operator request '{}' before the descriptor timeout",
                            request_key
                        )));
                    }
                };
                let AppServerEvent::ServerRequest(request) = event else {
                    buffered.push(event);
                    continue;
                };
                if classify_operator_request(&request, &continuity)?.is_none() {
                    buffered.push(AppServerEvent::ServerRequest(request));
                    continue;
                }
                let candidate = pending_operator_request(
                    &request,
                    &recovered.thread_id,
                    turn_id,
                    context.effect_gate.clone(),
                    context.turn_interrupted.clone(),
                )?;
                let candidate_method = match &candidate.kind {
                    PendingServerRequestKind::Operator { method } => method,
                    _ => unreachable!("pending_operator_request returned a non-operator kind"),
                };
                if candidate.key == *request_key && candidate_method == method {
                    client.restore_ready_events(buffered);
                    break candidate;
                }
                buffered.push(AppServerEvent::ServerRequest(request));
            }
        };

        {
            let mut transport_state = self.transport_state.lock().await;
            let state = transport_state.entry(context.scope.clone()).or_default();
            if state
                .thread_id
                .as_ref()
                .is_some_and(|thread_id| thread_id != &recovered.thread_id)
            {
                return Err(InterfaceDriverError::FailedClosed(
                    "replayed operator response conflicts with a live transport thread".to_string(),
                ));
            }
            state.context_generation = context_generation;
            state.thread_id = Some(recovered.thread_id.clone());
            state.thread_shape_fingerprint = Some(fingerprint);
            state.static_context_fingerprint = Some(static_context_fingerprint.to_string());
            state.hooks_catalog_emitted = true;
        }
        self.record_drain_outcome(context.scope, TurnDrainOutcome::Suspended(replayed))
            .await?;
        if !self
            .resume_operator_response(context, response, Some(recovered), context_generation)
            .await?
        {
            return Err(InterfaceDriverError::FailedClosed(
                "replayed operator request was not registered for exact continuation".to_string(),
            ));
        }
        Ok(())
    }

    pub(crate) async fn resume_operator_response(
        &self,
        context: TurnContext<'_, '_>,
        response: OperatorResponseArtifact,
        recovered: Option<&CodexContinuityState>,
        context_generation: u64,
    ) -> Result<bool, InterfaceDriverError> {
        let request_key = response.request_id()?;
        let key = format!("operator:{request_key}");
        let Some(pending) = self.pending_request(context.scope, &key).await? else {
            return Ok(false);
        };
        context
            .sink
            .bind_live_stream(&pending.thread_id, &pending.turn_id)?;
        let effect_guard = pending.effect_gate.clone().lock_owned().await;
        self.ensure_pending_request_current(context.scope, &pending)
            .await?;
        let method = match &pending.kind {
            PendingServerRequestKind::Operator { method } => method.clone(),
            PendingServerRequestKind::DynamicTool { tool, .. } => {
                return Err(InterfaceDriverError::FailedClosed(format!(
                    "operator response key resolved to pending dynamic tool '{tool}'"
                )));
            }
            PendingServerRequestKind::InterfaceOperation { operation, .. } => {
                return Err(InterfaceDriverError::FailedClosed(format!(
                    "operator response key resolved to pending interface operation '{operation}'"
                )));
            }
        };
        self.validate_pending_continuity(context.scope, recovered, &pending, context_generation)
            .await?;
        let result = operator_response_value(&method, &response)?;
        self.resolve_and_continue(context, pending, result, effect_guard)
            .await?;
        Ok(true)
    }

    pub(crate) async fn pending_request(
        &self,
        scope: &TransportStateScope,
        key: &str,
    ) -> Result<Option<PendingServerRequest>, InterfaceDriverError> {
        let requests = self.pending_server_requests.lock().await;
        let pending = requests.get(&(scope.clone(), key.to_string())).cloned();
        if pending.is_none() && requests.keys().any(|(candidate, _)| candidate == scope) {
            return Err(InterfaceDriverError::FailedClosed(format!(
                "provider result key '{key}' does not match any pending app-server request in this run"
            )));
        }
        Ok(pending)
    }

    pub(crate) async fn remove_pending_request(
        &self,
        scope: &TransportStateScope,
        pending: &PendingServerRequest,
    ) -> Result<(), InterfaceDriverError> {
        let removed = self
            .pending_server_requests
            .lock()
            .await
            .remove(&(scope.clone(), pending.key.clone()));
        if removed
            .as_ref()
            .is_none_or(|removed| removed.request_id != pending.request_id)
        {
            return Err(InterfaceDriverError::FailedClosed(format!(
                "pending app-server request '{}' changed before its exact result was admitted",
                pending.request_id
            )));
        }
        Ok(())
    }

    pub(crate) async fn ensure_pending_request_current(
        &self,
        scope: &TransportStateScope,
        pending: &PendingServerRequest,
    ) -> Result<(), InterfaceDriverError> {
        let requests = self.pending_server_requests.lock().await;
        let current = requests.get(&(scope.clone(), pending.key.clone()));
        if current.is_none_or(|current| {
            current.request_id != pending.request_id
                || !Arc::ptr_eq(&current.effect_gate, &pending.effect_gate)
        }) {
            return Err(InterfaceDriverError::FailedClosed(format!(
                "pending app-server request '{}' changed while exact-result arbitration waited",
                pending.request_id
            )));
        }
        Ok(())
    }

    pub(crate) async fn drain_interrupted_result(
        &self,
        context: TurnContext<'_, '_>,
        pending: PendingServerRequest,
        _effect_guard: tokio::sync::OwnedMutexGuard<()>,
    ) -> Result<InterruptedTurnDrainEvidence, InterfaceDriverError> {
        if let Some(terminal) = context.sink.continuity_snapshot().and_then(|state| state.native_terminal) {
            let mut client = self.client.lock().await;
            let events = take_native_terminal_events(&mut *client, &terminal, &pending.thread_id, &pending.turn_id)?;
            drop(client);
            self.remove_pending_request(context.scope, &pending).await?;
            self.publish_interrupted_drain_target(context)?;
            return Ok(InterruptedTurnDrainEvidence {
                disposition: "native_terminal_read_confirmed",
                provider_error: None,
                native_terminal_events: events,
                source_continuity: None,
            });
        }
        self.remove_pending_request(context.scope, &pending).await?;
        self.publish_interrupted_drain_target(context)?;
        let mut state =
            InterruptedTurnDrainState::new(pending.thread_id.clone(), pending.turn_id.clone());
        let mut client = self.client.lock().await;
        let handle = client.request_handle();
        loop {
            match state.observe(client.next_event().await) {
                InterruptedDrainDecision::Continue => {}
                InterruptedDrainDecision::Complete(mut evidence) => {
                    client.restore_ready_events(std::mem::take(&mut state.deferred_events));
                    evidence.native_terminal_events.append(&mut state.retained_events);
                    drop(client);
                    if !evidence.native_terminal_events.is_empty() {
                        let terminal = read_exact_terminal_turn(
                            &handle, &self.next_request_id, &pending.thread_id, &pending.turn_id,
                        ).await.map_err(InterfaceDriverError::FailedClosed)?
                            .ok_or_else(|| InterfaceDriverError::FailedClosed("drained requests have no exact closed native turn".into()))?;
                        evidence.native_terminal_events.extend(take_native_terminal_events(
                            &mut *self.client.lock().await, &terminal, &pending.thread_id, &pending.turn_id,
                        )?);
                        let mut continuity = context.sink.continuity_snapshot()
                            .ok_or_else(|| InterfaceDriverError::FailedClosed("drained requests lost their interrupted continuity".into()))?;
                        terminal.exact_turn(&continuity.thread_id, Some(&pending.turn_id))?;
                        evidence.source_continuity = Some(continuity.clone());
                        continuity.native_terminal = Some(terminal);
                        context.sink.set_continuity(continuity);
                    }
                    return Ok(evidence);
                }
            }
        }
    }

    pub(crate) fn publish_interrupted_drain_target(
        &self,
        context: TurnContext<'_, '_>,
    ) -> Result<(), InterfaceDriverError> {
        if context.control.is_starting() {
            let continuity = context.sink.continuity_snapshot().ok_or_else(|| {
                InterfaceDriverError::FailedClosed(
                    "interrupted exact-result drain lost its durable continuity".to_string(),
                )
            })?;
            context
                .control
                .publish_recovered_interrupted_pending(&continuity)
                .map_err(InterfaceDriverError::FailedClosed)?;
        }
        context.control.publish_interrupted_draining()
    }

    pub(crate) async fn resolve_and_continue(
        &self,
        context: TurnContext<'_, '_>,
        pending: PendingServerRequest,
        result: serde_json::Value,
        effect_guard: tokio::sync::OwnedMutexGuard<()>,
    ) -> Result<(), InterfaceDriverError> {
        let effect_gate = pending.effect_gate.clone();
        let turn_interrupted = pending.turn_interrupted.clone();
        let request_handle = {
            let client = self.client.lock().await;
            let request_handle = client.request_handle();
            client
                .resolve_server_request(pending.request_id.clone(), result)
                .await
                .map_err(|error| {
                    InterfaceDriverError::Transport(format!(
                        "app-server request '{}' response transport failed: {error}",
                        pending.request_id
                    ))
                })?;
            request_handle
        };
        self.remove_pending_request(context.scope, &pending).await?;
        context.sink.settle_pending(&pending.key);
        context.control.publish(CodexTurnControlTarget::Draining {
            handle: request_handle,
            request_ids: self.next_request_id.clone(),
            pending_requests: self.pending_server_requests.clone(),
            scope: context.scope.clone(),
            output: Arc::downgrade(context.sink.emit),
            continuity: context.sink.continuity_snapshot(),
            handoff_barrier: context.handoff_barrier.clone(),
            effect_gate: effect_gate.clone(),
            turn_interrupted: turn_interrupted.clone(),
            thread_id: pending.thread_id.clone(),
            turn_id: pending.turn_id.clone(),
        });
        drop(effect_guard);
        let resumed_context = TurnContext {
            effect_gate: &effect_gate,
            turn_interrupted: &turn_interrupted,
            ..context
        };
        let outcome = self
            .drain_turn(resumed_context, &pending.thread_id, &pending.turn_id)
            .await?;
        match outcome {
            TurnDrainOutcome::Suspended(_) | TurnDrainOutcome::AwaitingResults | TurnDrainOutcome::Complete => Ok(()),
        }
    }
}
