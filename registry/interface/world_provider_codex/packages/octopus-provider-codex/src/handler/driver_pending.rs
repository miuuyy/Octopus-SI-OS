use super::*;

impl CodexAppServerInterfaceDriver {
    pub(crate) async fn retain_tool_operator_wait(
        &self,
        scope: &TransportStateScope,
        sink: &TurnSink<'_>,
        control: &CodexTurnControlBroker,
        handoff_barrier: &Arc<ProviderHandoffBarrier>,
        call: &str,
        output: CodexTurnOutput,
    ) -> Result<(), InterfaceDriverError> {
        let state = sink.continuity_snapshot().ok_or_else(|| {
            InterfaceDriverError::FailedClosed("application wait lost continuity".to_string())
        })?;
        let request = state.operator_request_for(call).ok_or_else(|| {
            InterfaceDriverError::FailedClosed("application wait lost its request".to_string())
        })?;
        let Some(pending) = self
            .pending_request(scope, &format!("tool:{}", request.request.call_id))
            .await?
        else {
            return sink.send(output);
        };
        self.validate_pending_continuity(scope, Some(&state), &pending, state.context_generation)
            .await?;
        let guard = pending.effect_gate.lock().await;
        self.ensure_pending_request_current(scope, &pending).await?;
        if pending
            .turn_interrupted
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(InterfaceDriverError::FailedClosed(
                "application wait follows native interruption".to_string(),
            ));
        }
        sink.send(output)?;
        control.publish(CodexTurnControlTarget::Suspended {
            handle: self.client.lock().await.request_handle(),
            request_ids: self.next_request_id.clone(),
            pending_requests: self.pending_server_requests.clone(),
            scope: scope.clone(),
            output: Arc::downgrade(sink.emit),
            continuity: sink.continuity_snapshot(),
            handoff_barrier: handoff_barrier.clone(),
            effect_gate: pending.effect_gate.clone(),
            turn_interrupted: pending.turn_interrupted.clone(),
            thread_id: pending.thread_id.clone(),
            turn_id: pending.turn_id.clone(),
            late_calls: None,
        });
        drop(guard);
        Ok(())
    }

    pub(crate) async fn has_pending_request(&self, scope: &TransportStateScope) -> bool {
        self.pending_server_requests
            .lock()
            .await
            .keys()
            .any(|(candidate, _)| candidate == scope)
    }

    pub(crate) async fn validate_pending_continuity(
        &self,
        scope: &TransportStateScope,
        recovered: Option<&CodexContinuityState>,
        pending: &PendingServerRequest,
        context_generation: u64,
    ) -> Result<(), InterfaceDriverError> {
        let transport_state = self.transport_state.lock().await;
        let live = transport_state.get(scope).ok_or_else(|| {
            InterfaceDriverError::FailedClosed(
                "Codex pending request has no live transport coordinates".to_string(),
            )
        })?;
        validate_live_pending_continuity(recovered, pending, context_generation, live)
    }

    pub(crate) async fn record_drain_outcome(
        &self,
        scope: &TransportStateScope,
        outcome: TurnDrainOutcome,
    ) -> Result<(), InterfaceDriverError> {
        let TurnDrainOutcome::Suspended(pending) = outcome else {
            return Ok(());
        };
        let key = (scope.clone(), pending.key.clone());
        let mut requests = self.pending_server_requests.lock().await;
        if requests.insert(key, pending).is_some() {
            return Err(InterfaceDriverError::FailedClosed(
                "app-server repeated one unresolved request key inside the same run".to_string(),
            ));
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) async fn seed_interrupted_dynamic_tool_request_for_test(
        &self,
        continuity: &CodexContinuityState,
    ) -> Result<(), InterfaceDriverError> {
        validate_durable_interrupted_continuity(
            continuity,
            &continuity.provider_interface,
            &continuity.run,
        )
        .map_err(InterfaceDriverError::FailedClosed)?;
        let CodexPendingContinuity::DynamicTool {
            native_operation: _,
            turn_id,
            call_id,
            tool,
        } = continuity.interrupted_pending.as_ref().ok_or_else(|| {
            InterfaceDriverError::FailedClosed(
                "test interrupted request has no exact pending effect".to_string(),
            )
        })?
        else {
            return Err(InterfaceDriverError::FailedClosed(
                "test interrupted request is not a dynamic tool effect".to_string(),
            ));
        };
        let scope = TransportStateScope::Run(continuity.run.clone());
        let effect_gate = Arc::new(tokio::sync::Mutex::new(()));
        let turn_interrupted = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let pending = PendingServerRequest {
            request_id: RequestId::Integer(4_242),
            thread_id: continuity.thread_id.clone(),
            turn_id: turn_id.clone(),
            key: format!("tool:{call_id}"),
            kind: PendingServerRequestKind::DynamicTool {
                call_id: call_id.clone(),
                tool: tool.clone(),
            },
            effect_gate,
            turn_interrupted,
        };
        let mut requests = self.pending_server_requests.lock().await;
        if requests
            .insert((scope.clone(), pending.key.clone()), pending)
            .is_some()
        {
            return Err(InterfaceDriverError::FailedClosed(
                "test interrupted request replaced an existing pending effect".to_string(),
            ));
        }
        drop(requests);
        let mut transport = self.transport_state.lock().await;
        if transport
            .insert(
                scope,
                TransportState {
                    context_generation: continuity.context_generation,
                    hooks_catalog_emitted: true,
                    thread_id: Some(continuity.thread_id.clone()),
                    thread_shape_fingerprint: Some(continuity.thread_shape_fingerprint.clone()),
                    static_context_fingerprint: Some(continuity.static_context_fingerprint.clone()),
                    rollout_path: None,
                },
            )
            .is_some()
        {
            return Err(InterfaceDriverError::FailedClosed(
                "test interrupted request replaced live transport state".to_string(),
            ));
        }
        Ok(())
    }

    pub(crate) async fn complete_native_terminal_result(
        &self,
        context: TurnContext<'_, '_>,
        recovered: &CodexContinuityState,
        call: &str,
        pending: Option<&PendingServerRequest>,
        mut activity: CodexTurnOutput,
        activity_already_admitted: bool,
        mut closure: CodexTurnOutput,
        continue_cut_turn: bool,
    ) -> Result<NativeTerminalCheck, InterfaceDriverError> {
        if recovered.pending.is_none() {
            return Ok(NativeTerminalCheck::Open);
        }
        let Some(exact_pending) = recovered.outstanding_call(call).filter(|pending| interrupted_pending_has_external_result(pending)) else {
            return Ok(NativeTerminalCheck::Open);
        };
        if recovered.interrupted_pending.is_some() || recovered.native_terminal.is_some()
            || recovered.provider_interface != *context.sink.provider_interface
            || recovered.run != context.sink.run
        {
            return Err(InterfaceDriverError::FailedClosed("ordinary result terminal check crossed its exact source continuity".into()));
        }
        let handle = self.client.lock().await.request_handle();
        let turn_id = pending_turn_id(exact_pending);
        let Some(terminal) = read_exact_terminal_turn(&handle, &self.next_request_id, &recovered.thread_id, turn_id)
            .await.map_err(InterfaceDriverError::FailedClosed)? else {
            return Ok(NativeTerminalCheck::Open);
        };
        if recovered.outstanding_len() > 1 {
            context.sink.bind_live_stream(&recovered.thread_id, turn_id)?;
            closure.value["native_terminal_source_continuity"] = serde_json::to_value(recovered)
                .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
            closure.value["result_origin"] = serde_json::json!("native_turn_completion");
            closure.value["terminal_evidence_source"] = serde_json::json!("thread/read");
            closure.value["native_terminal"] = serde_json::to_value(&terminal)
                .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
            terminal.apply_disposition(&mut closure)?;
            terminal.apply_disposition(&mut activity)?;
            if let Some(pending) = pending {
                self.remove_pending_request(context.scope, pending).await?;
            }
            context.control.publish(CodexTurnControlTarget::InterruptedDraining {
                output: Arc::downgrade(context.sink.emit), durable_handoff: context.handoff_barrier.clone(),
                thread_id: recovered.thread_id.clone(), turn_id: turn_id.to_string(),
            });
            context.sink.set_continuity(recovered.clone());
            context.sink.send_interrupted_result(
                (!activity_already_admitted).then_some(activity), activity_already_admitted, closure,
            )?;
            return Ok(NativeTerminalCheck::Closed);
        }
        if continue_cut_turn && terminal.exact_turn(&recovered.thread_id, Some(turn_id))?.status == TurnStatus::Interrupted {
            let events = take_native_terminal_events(&mut *self.client.lock().await, &terminal, &recovered.thread_id, turn_id)?;
            return Ok(NativeTerminalCheck::CutByProcessDeath(activity.with_native_terminal_events(events)));
        }
        context.sink.bind_live_stream(&recovered.thread_id, turn_id)?;
        let events = take_native_terminal_events(&mut *self.client.lock().await, &terminal, &recovered.thread_id, turn_id)?;
        let (retired, retained) = interrupted_continuity(Some(recovered.clone())).map_err(InterfaceDriverError::FailedClosed)?;
        if !retained {
            return Err(InterfaceDriverError::FailedClosed("ordinary terminal result lost its exact external pending identity".into()));
        }
        let mut retired = retired.ok_or_else(|| InterfaceDriverError::FailedClosed("ordinary terminal result has no retained continuity".into()))?;
        retired.context_generation = recovered.context_generation;
        retired.native_terminal = Some(terminal);
        closure.value["native_terminal_source_continuity"] = serde_json::to_value(recovered)
            .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
        closure.value["result_origin"] = serde_json::json!("native_turn_completion");
        closure.value["terminal_evidence_source"] = serde_json::json!("thread/read");
        if let Some(pending) = pending {
            self.remove_pending_request(context.scope, pending).await?;
        }
        context.control.publish(CodexTurnControlTarget::InterruptedDraining {
            output: Arc::downgrade(context.sink.emit), durable_handoff: context.handoff_barrier.clone(),
            thread_id: recovered.thread_id.clone(), turn_id: turn_id.to_string(),
        });
        context.sink.set_continuity(retired);
        context.sink.send_interrupted_result(
            (!activity_already_admitted).then_some(activity), activity_already_admitted,
            closure.with_native_terminal_events(events),
        )?;
        Ok(NativeTerminalCheck::Closed)
    }
}
