use super::*;

pub(crate) struct LateCallWake {
    pub(crate) scope: TransportStateScope,
    pub(crate) thread_id: String,
    pub(crate) turn_id: String,
    pub(crate) source_movement_seq: u64,
    attempt: u32,
    provider_interface: InterfaceId,
    run: String,
    tool_names: BTreeMap<String, String>,
    native_tools: BTreeSet<String>,
    interface_operations: BTreeMap<String, InterfaceOperationAffordance>,
    effect_gate: Arc<tokio::sync::Mutex<()>>,
    turn_interrupted: Arc<std::sync::atomic::AtomicBool>,
    arrivals: std::sync::Mutex<BTreeMap<u64, InterfaceInvocationEnvelope>>,
    unhanded_controls: std::sync::Mutex<BTreeSet<String>>,
    withdrawn: std::sync::atomic::AtomicBool,
}

pub(crate) struct LateCallDrain<'a> {
    pub(crate) control: &'a Arc<CodexTurnControlBroker>,
    pub(crate) wake: &'a Arc<LateCallWake>,
    pub(crate) source: &'a InterfaceInvocationEnvelope,
    pub(crate) source_movement_seq: u64,
    pub(crate) emit: &'a Arc<ProviderOutputStream>,
    pub(crate) handoff_barrier: &'a Arc<ProviderHandoffBarrier>,
    pub(crate) output_root: &'a Path,
    pub(crate) live_stream_root: &'a Path,
}

impl LateCallWake {
    pub(crate) fn capture(
        context: TurnContext<'_, '_>,
        thread_id: &str,
        turn_id: &str,
        native_tools: &BTreeSet<String>,
        continuity: Option<&CodexContinuityState>,
    ) -> Option<Arc<Self>> {
        let state = continuity?;
        if context.lifecycle.is_some()
            || context.lifecycle_prompt.is_some()
            || state.interrupted_pending.is_some()
            || state.native_terminal.is_some()
            || state.outstanding_len() == 0
            || !state.outstanding().all(parallel_pending_admissible)
        {
            return None;
        }
        Some(Arc::new(Self {
            scope: context.scope.clone(),
            thread_id: thread_id.to_string(),
            turn_id: turn_id.to_string(),
            source_movement_seq: context.sink.source_movement_seq,
            attempt: context.sink.attempt,
            provider_interface: context.sink.provider_interface.clone(),
            run: context.sink.run.to_string(),
            tool_names: context.tool_names.clone(),
            native_tools: native_tools.clone(),
            interface_operations: context.interface_operations.clone(),
            effect_gate: context.effect_gate.clone(),
            turn_interrupted: context.turn_interrupted.clone(),
            arrivals: std::sync::Mutex::new(BTreeMap::new()),
            unhanded_controls: std::sync::Mutex::new(BTreeSet::new()),
            withdrawn: std::sync::atomic::AtomicBool::new(false),
        }))
    }

    pub(crate) fn record_arrival(&self, seq: u64, envelope: &InterfaceInvocationEnvelope) {
        self.arrivals.lock().expect("late call arrivals lock").insert(seq, envelope.clone());
    }

    pub(crate) fn arrival(&self, seq: u64) -> Option<InterfaceInvocationEnvelope> {
        self.arrivals.lock().expect("late call arrivals lock").get(&seq).cloned()
    }

    pub(crate) fn latest_arrival(&self) -> Option<(u64, InterfaceInvocationEnvelope)> {
        self.arrivals
            .lock()
            .expect("late call arrivals lock")
            .last_key_value()
            .map(|(seq, envelope)| (*seq, envelope.clone()))
    }

    pub(crate) fn has_arrival(&self) -> bool {
        !self.arrivals.lock().expect("late call arrivals lock").is_empty()
    }

    pub(crate) fn hold_for_control(&self, control_id: &str) {
        self.unhanded_controls.lock().expect("late call controls lock").insert(control_id.to_string());
    }

    fn release_control(&self, control_id: &str) -> bool {
        self.unhanded_controls.lock().expect("late call controls lock").remove(control_id)
    }

    pub(crate) fn withdraw(&self) {
        self.withdrawn.store(true, std::sync::atomic::Ordering::Release);
    }

    pub(crate) fn armed(&self) -> bool {
        !self.withdrawn.load(std::sync::atomic::Ordering::Acquire)
            && !self.turn_interrupted.load(std::sync::atomic::Ordering::Acquire)
            && self.unhanded_controls.lock().expect("late call controls lock").is_empty()
    }

    fn admits(&self, event: &AppServerEvent) -> bool {
        if !parallel_drain_admits(false, &self.native_tools, event) {
            return false;
        }
        match event {
            AppServerEvent::ServerNotification(ServerNotification::Error(_)) => false,
            AppServerEvent::ServerNotification(notification) => {
                server_notification_matches_turn(notification, &self.thread_id, &self.turn_id)
                    .unwrap_or(false)
            }
            AppServerEvent::ServerRequest(ServerRequest::DynamicToolCall { params, .. }) => {
                params.thread_id == self.thread_id && params.turn_id == self.turn_id
            }
            _ => false,
        }
    }

    fn buffered(&self, client: &mut AppServerClient) -> bool {
        let events = client.take_ready_events();
        let ready = self.ready(&events);
        client.restore_ready_events(events);
        ready
    }

    fn ready(&self, events: &[AppServerEvent]) -> bool {
        self.armed()
            && events.iter().take_while(|event| self.admits(event)).any(|event| {
                matches!(event, AppServerEvent::ServerRequest(ServerRequest::DynamicToolCall { .. }))
            })
    }
}

impl CodexTurnControlBroker {
    pub(crate) async fn release_late_calls_after_control(
        &self,
        control_id: &str,
        source_movement_seq: u64,
        source: &InterfaceInvocationEnvelope,
        continuity: Option<&CodexContinuityState>,
    ) {
        let Some(wake) = self.held_late_call_wake() else {
            return;
        };
        let _effect_guard = wake.effect_gate.clone().lock_owned().await;
        let mut target = self.state.borrow().clone();
        let CodexTurnControlTarget::Suspended { late_calls: Some(current), continuity: suspended, .. } =
            &mut target
        else {
            return;
        };
        if !Arc::ptr_eq(current, &wake) || !wake.release_control(control_id) {
            return;
        }
        wake.record_arrival(source_movement_seq, source);
        if let Some(continuity) = continuity {
            *suspended = Some(continuity.clone());
            self.publish(target);
        }
    }

    fn held_late_call_wake(&self) -> Option<Arc<LateCallWake>> {
        match &*self.state.borrow() {
            CodexTurnControlTarget::Suspended { late_calls, .. } => late_calls.clone(),
            _ => None,
        }
    }
}

impl CodexAppServerInterfaceDriver {
    pub(crate) async fn event_arrivals(&self) -> watch::Receiver<u64> {
        self.client.lock().await.event_arrivals()
    }

    pub(crate) async fn late_call_ready(&self, wake: &LateCallWake) -> bool {
        wake.buffered(&mut *self.client.lock().await)
    }

    pub(crate) async fn drain_late_calls(
        &self,
        late: LateCallDrain<'_>,
    ) -> Result<bool, InterfaceDriverError> {
        let LateCallDrain {
            control,
            wake,
            source,
            source_movement_seq,
            emit,
            handoff_barrier,
            output_root,
            live_stream_root,
        } = late;
        let context_output_labels = self.context_output_labels()?;
        let session_snapshot_labels = self.session_snapshot_labels()?;
        let client_projection_output_labels =
            self.client_projection_output_labels(&context_output_labels, &session_snapshot_labels)?;
        let live_stream_rules =
            LiveStreamRules::from_private_config_or_disabled(&self.interface_private_config)?;
        let effect_guard = wake.effect_gate.clone().lock_owned().await;
        let continuity = match &*control.state.borrow() {
            CodexTurnControlTarget::Suspended { late_calls: Some(current), continuity, .. }
                if Arc::ptr_eq(current, wake) && wake.armed() => continuity.clone(),
            _ => return Ok(false),
        };
        let handle = {
            let mut client = self.client.lock().await;
            if !wake.buffered(&mut client) {
                return Ok(false);
            }
            client.request_handle()
        };
        {
            let mut state = emit.continuity.lock().expect("continuity lock");
            state.continuity = continuity.clone();
            state.pending_input_context = false;
        }
        let sink = TurnSink {
            emit,
            output_root,
            live_stream_root,
            provider_interface: &wake.provider_interface,
            run: &wake.run,
            source_movement_seq,
            attempt: wake.attempt,
            context_output_labels: &context_output_labels,
            client_projection_output_labels: &client_projection_output_labels,
            live_stream_rules: &live_stream_rules,
            live_stream: std::sync::Mutex::new(None),
            continuity: emit.continuity.clone(),
        };
        control.publish(CodexTurnControlTarget::Draining {
            handle,
            request_ids: self.next_request_id.clone(),
            pending_requests: self.pending_server_requests.clone(),
            scope: wake.scope.clone(),
            output: Arc::downgrade(emit),
            continuity,
            handoff_barrier: handoff_barrier.clone(),
            effect_gate: wake.effect_gate.clone(),
            turn_interrupted: wake.turn_interrupted.clone(),
            thread_id: wake.thread_id.clone(),
            turn_id: wake.turn_id.clone(),
        });
        drop(effect_guard);
        let context = TurnContext {
            run_invocation: None,
            scope: &wake.scope,
            sink: &sink,
            control,
            interface_operations: &wake.interface_operations,
            tool_names: &wake.tool_names,
            source_artifact: &source.artifact,
            handoff_barrier,
            effect_gate: &wake.effect_gate,
            turn_interrupted: &wake.turn_interrupted,
            lifecycle: None,
            lifecycle_prompt: None,
        };
        self.drain_turn_with(context, &wake.thread_id, &wake.turn_id, &wake.native_tools, &|event| {
            wake.admits(event)
        })
        .await?;
        if let Some(next) = control.late_call_wake() {
            next.record_arrival(source_movement_seq, source);
        }
        Ok(true)
    }
}
