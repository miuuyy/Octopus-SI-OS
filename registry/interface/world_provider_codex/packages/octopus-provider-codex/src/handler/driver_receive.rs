use super::*;

impl CodexAppServerInterfaceDriver {
    pub(crate) fn resolve_bound_session_contexts(
        artifact_binding_payloads: &[octopus_interfaces::ArtifactBindingPayload],
        provider_interface: &InterfaceId,
        session_snapshot_labels: &BTreeSet<String>,
        resolved_session_snapshot: Option<&ProviderInvocationSessionSnapshot>,
    ) -> Result<BoundSessionContexts, InterfaceDriverError> {
        let mut resolved = BTreeMap::new();
        let mut current_seen = false;
        let mut grounded_snapshot_used = false;
        for bound in artifact_binding_payloads {
            let bytes = bound.read_bytes()
                .map_err(|error| InterfaceDriverError::FailedClosed(error.to_string()))?;
            let value: serde_json::Value =
                serde_json::from_slice(bytes.as_ref()).map_err(|error| {
                    InterfaceDriverError::Protocol(format!(
                        "Codex session carrier '{}' is not JSON: {error}",
                        bound.artifact.id
                    ))
                })?;
            let Some(raw) = value.get(SESSION_CONTEXT_FIELD) else {
                continue;
            };
            let label = continuity_carrier_label(&value, bound.artifact.id.as_str())?;
            if !session_snapshot_labels.contains(label) {
                return Err(InterfaceDriverError::FailedClosed(format!(
                    "Codex session carrier '{}' is not a descriptor-declared snapshot output",
                    bound.artifact.id
                )));
            }
            let snapshot: SessionContextSnapshot =
                serde_json::from_value(raw.clone()).map_err(|error| {
                    InterfaceDriverError::Protocol(format!(
                        "Codex session carrier '{}' is malformed: {error}",
                        bound.artifact.id
                    ))
                })?;
            if snapshot.schema_version != 3 {
                return Err(InterfaceDriverError::FailedClosed(format!(
                    "Codex session carrier '{}' has unsupported schema_version {}",
                    bound.artifact.id, snapshot.schema_version
                )));
            }
            let key = continuity_source_key(&bound.artifact);
            if snapshot.provider_interface != *provider_interface {
                if resolved.insert(key, BoundSessionContext::Foreign).is_some() {
                    return Err(InterfaceDriverError::FailedClosed(
                        "Codex invocation repeats one foreign session carrier".to_string(),
                    ));
                }
                continue;
            }
            if current_seen {
                return Err(InterfaceDriverError::FailedClosed(
                    "Codex invocation carries multiple current session snapshots".to_string(),
                ));
            }
            current_seen = true;
            let snapshot = resolved_session_snapshot.ok_or_else(|| {
                InterfaceDriverError::FailedClosed(
                    "Codex current session carrier has no handoff-grounded ledger view".to_string(),
                )
            })?;
            if snapshot.carrier != bound.artifact {
                return Err(InterfaceDriverError::FailedClosed(
                    "Codex handoff-grounded session carrier differs from the invocation binding"
                        .to_string(),
                ));
            }
            grounded_snapshot_used = true;
            if resolved
                .insert(
                    key,
                    BoundSessionContext::Current(snapshot.artifacts.clone()),
                )
                .is_some()
            {
                return Err(InterfaceDriverError::FailedClosed(
                    "Codex invocation repeats one current session carrier".to_string(),
                ));
            }
        }
        if resolved_session_snapshot.is_some() && !grounded_snapshot_used {
            return Err(InterfaceDriverError::FailedClosed(
                "Codex handoff grounded a session carrier absent from the invocation bindings"
                    .to_string(),
            ));
        }
        Ok(resolved)
    }

    pub(crate) fn receive_artifact_and_stream_outputs<'a>(
        &'a self,
        invocation: CodexProviderInvocation<'a>,
    ) -> std::pin::Pin<Box<impl std::future::Future<Output = Result<(), InterfaceDriverError>> + 'a>> {
        Box::pin(self.receive_artifact_and_stream_outputs_inner(invocation))
    }

    pub(crate) async fn receive_artifact_and_stream_outputs_inner(
        &self,
        invocation: CodexProviderInvocation<'_>,
    ) -> Result<(), InterfaceDriverError> {
        let CodexProviderInvocation {
            callable_tool_names,
            input,
            emit,
            resolved_session_snapshot,
            current_session_context,
            output_root,
            live_stream_root,
            source_movement_seq,
            attempt,
            admitted_run_record,
            current_run_record,
            deferred_context_generations,
            input_changes_context,
            interrupted_after_artifact,
            recovered_handoff_artifacts,
            durable_interrupted_continuity,
            reconciled_result_continuity,
            control,
            handoff_barrier,
        } = invocation;
        let request_bytes = input.read_artifact_payload()?;

        let runtime = invocation_runtime_metadata(&input)?;
        if interrupted_after_artifact.is_none() && !recovered_handoff_artifacts.is_empty() {
            return Err(InterfaceDriverError::FailedClosed(
                "Codex invocation carries recovered handoff artifacts without an interrupted source frontier"
                    .to_string(),
            ));
        }
        let recovered_turn_start_dispatches = recovered_turn_start_dispatches(
            &recovered_handoff_artifacts,
            &runtime.interface,
            source_movement_seq,
        )?;
        let run_record = parse_run_record_json(current_run_record, runtime.run.as_str())?;
        let granted_interfaces = input
            .allowed_interfaces()
            .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?
            .iter()
            .map(|interface| InterfaceId::new(interface.clone()))
            .collect::<BTreeSet<_>>();
        let grant = input
            .grant()
            .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
        let granted_tools = grant.tools.iter().cloned().collect::<BTreeSet<_>>();
        let granted_marks = grant.mark_outcomes.iter().cloned().collect::<BTreeSet<_>>();
        let scope = TransportStateScope::from_run(&runtime.run);
        let context_output_labels = self.context_output_labels()?;
        let session_snapshot_labels = self.session_snapshot_labels()?;
        let client_projection_output_labels =
            self.client_projection_output_labels(&context_output_labels, &session_snapshot_labels)?;
        let live_stream_rules =
            LiveStreamRules::from_private_config_or_disabled(&self.interface_private_config)?;
        live_stream_rules.validate_projection_labels(
            &context_output_labels,
            &session_snapshot_labels,
            &client_projection_output_labels,
        )?;
        let sink = TurnSink {
            emit: &emit,
            output_root,
            live_stream_root,
            provider_interface: &runtime.interface,
            run: runtime.run.as_str(),
            source_movement_seq,
            attempt,
            context_output_labels: &context_output_labels,
            client_projection_output_labels: &client_projection_output_labels,
            live_stream_rules: &live_stream_rules,
            live_stream: std::sync::Mutex::new(None),
            continuity: emit.continuity.clone(),
        };
        sink.continuity.lock().expect("continuity lock").pending_input_context = input_changes_context;
        let manifest = input.read_artifact_manifest()?;
        let peer_message = admitted_peer_message(&input, self.interface_private_config.get("input_intake"))?;
        let lifecycle_inbound = decode_lifecycle_inbound(&manifest.label, &request_bytes)
            .map_err(InterfaceDriverError::Protocol)?;
        let thread_compact_requested = peer_message.is_none() && self.configured_thread_compact_request(&manifest.label)?;
        if !thread_compact_requested
            && peer_message.is_none()
            && let Some(operation) = self.configured_provider_operation(&manifest.label)?
        {
            return self
                .handle_provider_operation(operation, &runtime.interface, &request_bytes, &sink)
                .await;
        }
        if thread_compact_requested
            && serde_json::from_slice::<EmptyProviderOperationRequest>(&request_bytes).is_err()
        {
            return sink.send(provider_operation_error(
                PROVIDER_THREAD_COMPACT_START_LABEL,
                "operation_payload_malformed",
                "protocol",
                false,
                "thread compact payload must be an empty object",
            ));
        }
        if peer_message.is_none() && is_undeclared_provider_operation(&manifest.label, thread_compact_requested) {
            return sink.send(provider_operation_error(
                &manifest.label,
                "operation_not_declared",
                "unsupported",
                false,
                "the exact provider interface descriptor does not declare this operation artifact",
            ));
        }
        let admitted_record = octopus_run_record::RunRecord::from_bytes(admitted_run_record)
            .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
        let (context_payloads, payload_selection) = admitted_payload_context(&admitted_record, &input)?;
        let continuity_sources = continuity_input_sources(&admitted_record, &input)?;
        let complete_context_payloads = context_payloads;
        let instructions = resolve_instruction_context(input.metadata()?, &complete_context_payloads)
            .map_err(InterfaceDriverError::FailedClosed)?;
        let context_payloads = &instructions.remaining_payloads;
        let bound_session_contexts = Self::resolve_bound_session_contexts(
            &context_payloads,
            &runtime.interface,
            &session_snapshot_labels,
            resolved_session_snapshot.as_ref(),
        )?;
        let lifecycle_payloads = lifecycle_context_payloads(
            &context_payloads,
            resolved_session_snapshot.as_ref(),
        )?;
        let lifecycle = LifecycleBinding::resolve(
            &lifecycle_payloads,
            &run_record,
            runtime,
            &granted_interfaces,
            &granted_marks,
        )
        .map_err(InterfaceDriverError::FailedClosed)?;
        if lifecycle_inbound.is_some() && lifecycle.is_none() {
            return Err(InterfaceDriverError::FailedClosed(format!(
                "run '{}' received lifecycle artifact '{}' without an exact connected lifecycle.connection",
                runtime.run.as_str(),
                manifest.label
            )));
        }
        let continuity_payloads = continuity_context_payloads(&admitted_record, &input, context_payloads)?;
        let recovered = recover_codex_continuity_resolved(
            &continuity_payloads,
            &bound_session_contexts,
            &runtime.interface,
            runtime.run.as_str(),
            &context_output_labels,
            &session_snapshot_labels,
            &continuity_sources,
        )?;
        let bound_continuity = recovered.clone();
        let recovered = reconcile_input_session_continuity(
            &admitted_record,
            &input,
            recovered,
            recover_codex_continuity_from_current_session(
                &current_session_context,
                &continuity_sources,
                &runtime.interface,
                runtime.run.as_str(),
                &context_output_labels,
            )?,
            &current_session_context,
        )?;
        let recovered = reconcile_delivered_interrupted_continuity(
            bound_continuity.as_ref(), recovered, reconciled_result_continuity,
        )?;
        let (mut recovered, durable_interrupt_override) =
            reconcile_durable_interrupted_continuity(recovered, durable_interrupted_continuity)?;
        if manifest.label == PROVIDER_MODELS_LIST_LABEL {
            if self.has_pending_request(&scope).await
                || recovered.as_ref().is_some_and(|state| {
                    state.pending.is_some() || state.interrupted_pending.is_some()
                })
            {
                return Err(InterfaceDriverError::FailedClosed(
                    "Codex model catalog request cannot replace an unresolved app-server request"
                        .to_string(),
                ));
            }
            return self
                .emit_model_catalog(&request_bytes, &sink, &required_formed_models(&input)?)
                .await;
        }
        if let Some(state) = recovered.clone() {
            sink.set_continuity(state);
        }
        self.enforce_native_web_search_policy(&input)?;
        self.native_mcp_resource_discovery()?;
        let hidden_tools = model_hidden_tool_names(&self.interface_private_config)
            .map_err(InterfaceDriverError::Protocol)?;
        let hidden_tools = model_tool_exclusions(input.metadata()?, &callable_tool_names, &hidden_tools);
        let native_namespace = self.native_tool_namespace()?;
        let run_affordances = RunAffordances::from_invocation(&input, &serde_json::Value::Object(self.interface_private_config.clone()))
            .map_err(InterfaceDriverError::FailedClosed)?;
        let native_catalog =
            extract_native_affordance_catalog(&input, &hidden_tools, &native_namespace, run_affordances.as_ref(), &self.interface_private_config)?;
        let source_interface = if native_catalog.interface_operations.is_empty() {
            None
        } else {
            Some(
                input
                    .metadata()?
                    .checkpoint_binding_policy
                    .as_ref()
                    .ok_or_else(|| {
                        InterfaceDriverError::FailedClosed(
                            "provider input has no admitted checkpoint binding snapshot"
                                .to_string(),
                        )
                    })?
                    .inbound
                    .interface
                    .clone(),
            )
        };
        let mut resumption = LifecycleResumption::of_input(request_bytes.clone(), manifest.label.clone(), input.artifact.clone());
        if let Some(inbound) = lifecycle_inbound {
            let frame = LifecycleInboundFrame {
                binding: lifecycle.as_ref().expect("lifecycle inbound has a connection"),
                sink: &sink,
                runtime,
                input: &input,
                granted_tools: &granted_tools,
                granted_interfaces: &granted_interfaces,
            };
            let Some(resumed) = resume_lifecycle_inbound(frame, inbound, resumption)? else {
                return Ok(());
            };
            resumption = resumed;
        }
        let LifecycleResumption {
            request_bytes: mut effective_request_bytes,
            manifest_label: mut effective_manifest_label,
            source_artifact: effective_source_artifact,
            start_turn: resumed_start_turn,
            additional_context: resumed_additional_context,
            resumed_provider_input: lifecycle_resumed_provider_input,
            resume_boundary: lifecycle_resume_boundary,
            prompt_event: lifecycle_prompt_event,
        } = resumption;
        let lifecycle_start_resumed = resumed_start_turn.is_some();
        let selected_files = selected_file_projection(
            &admitted_record,
            &input,
            &effective_source_artifact,
            &effective_manifest_label,
            &effective_request_bytes,
            &bound_session_contexts,
            &current_session_context,
            self.model_result_projection()?,
        )?;
        let mut provider_input = match resumed_start_turn {
            Some(turn_input) => ProviderInput::Turn(turn_input),
            None if lifecycle_resume_boundary => ProviderInput::Turn(Vec::new()),
            None if thread_compact_requested => ProviderInput::Turn(Vec::new()),
            None if peer_message.is_some() => {
                let peer = peer_message.as_ref().expect("peer message selected");
                if peer.action == PeerAction::Interrupt {
                    return Err(InterfaceDriverError::FailedClosed("peer interrupt must enter the admitted turn-control reader".into()));
                }
                ProviderInput::Turn(vec![UserInput::Text { text: peer_input_text(peer, &input.artifact)?, text_elements: Vec::new() }])
            }
            None => decode_provider_input(
                &effective_request_bytes,
                &effective_manifest_label,
                source_interface.as_ref(),
                &native_catalog.interface_operations,
                Some(&selected_files),
                &self.interface_private_config,
            )?,
        };
        if matches!(provider_input, ProviderInput::Resume { .. }) && recovered.is_none() {
            let ctx = self.life_context.get().ok_or_else(|| InterfaceDriverError::FailedClosed(
                "Resume has no declared provider life for its stopping receipt".into()))?;
            let stopped = crate::life::resume_control_continuity(ctx, &runtime.run, source_movement_seq,
                read_codex_context_generation(&run_record, &runtime.interface)?)
                .map_err(InterfaceDriverError::FailedClosed)?;
            recovered = reconcile_current_session_continuity(Some(stopped), recovered)?;
            if let Some(state) = recovered.clone() { sink.set_continuity(state); }
        }
        let revised_history = revised_message_history(&current_session_context, &effective_source_artifact,
            &effective_manifest_label, &effective_request_bytes)?;
        if revised_history.replaces.is_some() && (self.has_pending_request(&scope).await
            || recovered.as_ref().is_some_and(codex_continuity_waits_for_result)) {
            return Err(InterfaceDriverError::FailedClosed("A message cannot be edited while its work is unresolved".into()));
        }
        let mut assembled_context = assemble_current_context(
            &context_payloads,
            &bound_session_contexts,
            &runtime.interface,
            &session_snapshot_labels,
            &revised_history.artifacts,
            &current_session_context,
            &effective_source_artifact,
            self.model_result_projection()?,
        )?;
        let recovered_result_context = match (&provider_input, recovered.as_ref()) {
            (ProviderInput::Turn(_) | ProviderInput::Resume { .. }, Some(state)) if !codex_continuity_waits_for_result(state) => {
                resumed_model_context(&revised_history.artifacts, &effective_source_artifact, state, self.model_result_projection()?)?
            }
            _ => None,
        };
        let instruction_artifacts = instruction_artifact_selection(&self.interface_private_config)?;
        let instruction_text = instructions.render(&instruction_artifacts)
            .map_err(InterfaceDriverError::FailedClosed)?;
        assembled_context.role = instruction_text;
        assembled_context.static_context_bytes.clear();
        for bound in &complete_context_payloads {
            if instructions.source_artifacts.contains(&bound.artifact) {
                append_static_context_source(&mut assembled_context.static_context_bytes, bound)?;
            }
        }
        assembled_context.static_context_bytes.extend_from_slice(
            &serde_json::to_vec(&native_catalog.tool_names).map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?,
        );
        if let Some(revision) = &revised_history.revision {
            assembled_context.static_context_bytes.extend_from_slice(
                &serde_json::to_vec(&serde_json::json!({"message_revision":revision}))
                    .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?);
            assembled_context.session_context_available = true;
        }
        let authored_context_generation = read_codex_context_generation(&run_record, &runtime.interface)?
            .checked_add(deferred_context_generations)
            .ok_or_else(|| InterfaceDriverError::FailedClosed("Codex authored context generation overflowed".to_string()))?;
        let context_generation = if durable_interrupt_override {
            recovered
                .as_ref()
                .map(|state| state.context_generation)
                .unwrap_or(authored_context_generation)
        } else {
            authored_context_generation
        };
        let invocation_context_generation = if input_changes_context {
            context_generation.checked_add(1).ok_or_else(|| {
                InterfaceDriverError::FailedClosed(
                    "Codex invocation context generation overflowed".to_string(),
                )
            })?
        } else {
            context_generation
        };
        let approval = OperatorApprovalFrame {
            recovered: &recovered,
            input: &input,
            run_record: &run_record,
            current_session_context: &current_session_context,
            hidden_tools: &hidden_tools,
            payload_selection: &payload_selection,
            runtime,
            sink: &sink,
            scope: &scope,
            control,
            handoff_barrier: &handoff_barrier,
        };
        if let Arrival::Settled = self
            .route_operator_approval(approval, &mut provider_input, &mut effective_request_bytes, &mut effective_manifest_label)
            .await?
        {
            return Ok(());
        }
        let early_model_context = if recovered.as_ref().is_some_and(|state| state.interrupted_pending.is_none() && (state.pending.is_some() || state.context_refresh.is_some()))
            && matches!(&provider_input, ProviderInput::ToolResult(_) | ProviderInput::InterfaceOperationResult(_)) {
            Some(self.resolve_model_context(&input, &assembled_context).await?)
        } else { None };
        let effect_gate = Arc::new(tokio::sync::Mutex::new(()));
        let turn_interrupted = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let native_parent_record = octopus_run_record::RunRecord::from_bytes(current_run_record)
            .map_err(|error| InterfaceDriverError::FailedClosed(error.to_string()))?;
        let turn_context = TurnContext {
            run_invocation: run_affordances.as_ref().map(|affordances| NativeRunInvocation {
                affordances,
                metadata: input.metadata().expect("validated invocation metadata"),
                admitted_record: &admitted_record,
                current_record: &native_parent_record,
            }),
            scope: &scope,
            sink: &sink,
            control,
            interface_operations: &native_catalog.interface_operations,
            tool_names: &native_catalog.tool_names,
            source_artifact: &effective_source_artifact,
            handoff_barrier: &handoff_barrier,
            effect_gate: &effect_gate,
            turn_interrupted: &turn_interrupted,
            lifecycle: lifecycle.as_ref(),
            lifecycle_prompt: lifecycle_prompt_event.as_ref(),
        };
        if let Some(model_context) = &early_model_context
            && self.refresh_native_context(
                turn_context, &provider_input, recovered.as_ref(), context_generation,
                CurrentNativeContext {
                    model: &model_context.model, instructions: &model_context.bound_instructions, tools: &native_catalog.specs,
                    static_fingerprint: &model_context.static_context_fingerprint,
                    conversation: model_context.conversation_block.as_deref(), obligations: model_context.obligations_grant_block.as_deref(),
                    session_available: assembled_context.session_context_available,
                    reasoning_effort: model_context.reasoning_effort.clone(), reasoning_summary: model_context.reasoning_summary.clone(),
                    recovered_dispatches: &recovered_turn_start_dispatches,
                },
            ).await? { return Ok(()); }
        if durable_interrupt_override {
            emit_lifecycle_trigger(
                &sink,
                lifecycle.as_ref(),
                runtime.run.as_str(),
                LifecycleTrigger {
                    event: LifecycleEventName::Interrupt,
                    trigger: "durable_interrupt_recovery".to_string(),
                    match_values: Vec::new(),
                    subject_artifact: effective_source_artifact.clone(),
                    data: serde_json::json!({
                        "interrupted_after_artifact": interrupted_after_artifact.clone(),
                    }),
                    continuation: None,
                },
            )?;
        }
        let mut continuations = ArrivalContinuations::default();
        let interrupted_result_recovery = matches!(
            (
                &provider_input,
                recovered
                    .as_ref()
                    .and_then(|state| state.interrupted_pending.as_ref())
            ),
            (
                ProviderInput::ToolResult(_),
                Some(CodexPendingContinuity::DynamicTool { .. })
            ) | (
                ProviderInput::InterfaceOperationResult(_),
                Some(CodexPendingContinuity::InterfaceOperation { .. })
            )
        );
        let pending_result_recovery = matches!(
            (
                &provider_input,
                recovered.as_ref().and_then(|state| state.pending.as_ref())
            ),
            (
                ProviderInput::ToolResult(_),
                Some(CodexPendingContinuity::DynamicTool { .. })
            ) | (
                ProviderInput::InterfaceOperationResult(_),
                Some(CodexPendingContinuity::InterfaceOperation { .. })
            )
        );
        if recovered
            .as_ref()
            .is_some_and(|state| state.interrupted_pending.is_some())
            && !interrupted_result_recovery
        {
            control.finish();
            return Err(InterfaceDriverError::FailedClosed(format!(
                "Codex input '{}' cannot replace a durably interrupted exact effect before its correlated result is admitted",
                manifest.label
            )));
        }
        if let Some(last_artifact_label) = interrupted_after_artifact.as_deref() {
            let Some(recovered) = recovered.as_ref() else {
                return Err(InterfaceDriverError::FailedClosed(
                    "Codex source recovery has no durable continuity evidence".to_string(),
                ));
            };
            if !interrupted_result_recovery && !pending_result_recovery {
                control.finish();
                return sink.send(interrupted_transport_output(
                    last_artifact_label,
                    recovered.pending.as_ref(),
                ));
            }
        }
        let frame = ResultArrival {
            turn_context,
            sink: &sink,
            runtime,
            input: &input,
            scope: &scope,
            lifecycle: &lifecycle,
            lifecycle_payloads: &lifecycle_payloads,
            lifecycle_resumed_provider_input,
            recovered: &recovered,
            context_generation,
            current_session_context: &current_session_context,
            effective_manifest_label: &effective_manifest_label,
            effective_request_bytes: &effective_request_bytes,
            effective_source_artifact: &effective_source_artifact,
            interrupted_after_artifact: &interrupted_after_artifact,
            source_interface: &source_interface,
        };
        if let Arrival::Settled = self.arrive(frame, &provider_input, &mut continuations).await? {
            return Ok(());
        }
        let ArrivalContinuations {
            tool: recovered_tool_continuation,
            cut_turn: cut_turn_continuation,
            operator: recovered_operator_continuation,
        } = continuations;

        let context_refresh::ResolvedModelContext { model, reasoning_effort, reasoning_summary,
            static_context_fingerprint, bound_instructions, conversation_block, obligations_grant_block } = match early_model_context {
                Some(context) => context,
                None => self.resolve_model_context(&input, &assembled_context).await?,
            };
        let native_affordances = &native_catalog.specs;

        if let Some((response, pending)) = recovered_operator_continuation {
            let recovered = recovered.as_ref().ok_or_else(|| {
                InterfaceDriverError::FailedClosed(
                    "operator response recovery lost its durable continuity".to_string(),
                )
            })?;
            self.resume_replayed_operator_response(
                turn_context,
                response,
                recovered,
                &pending,
                context_generation,
                &model,
                &bound_instructions,
                native_affordances,
                &static_context_fingerprint,
            )
            .await?;
            return Ok(());
        }

        let resume_control_id = match &provider_input {
            ProviderInput::Resume { control_id } => Some(control_id.clone()),
            _ => None,
        };
        let inspect_settled_history = matches!(&provider_input, ProviderInput::Turn(_))
            && revised_history.replaces.is_none()
            && !thread_compact_requested
            && peer_message.as_ref().is_none_or(|peer| peer.action != PeerAction::Queue)
            && recovered_turn_start_dispatches.is_empty();
        let turn_input = if recovered_tool_continuation.is_some() || cut_turn_continuation.is_some() {
            Vec::new()
        } else {
            provider_input.into_turn_input()?
        };

        let TurnThread { thread_id, cold: cold_thread, first_turn_of_run, retirement } = self
            .acquire_turn_thread(
                &scope,
                ThreadAcquisition {
                    recovered: recovered.as_ref(),
                    retired_history: None,
                    require_exact_resume: thread_compact_requested
                        || resume_control_id.is_some()
                        || cut_turn_continuation.is_some(),
                    context_generation: invocation_context_generation,
                    input_changes_context,
                    model: Some(model.clone()),
                    bound_instructions: &bound_instructions,
                    native_affordances,
                    static_context_fingerprint: &static_context_fingerprint,
                    session_context_available: assembled_context.session_context_available,
                },
                inspect_settled_history.then_some(SettledHistory {
                    session: &current_session_context,
                    sink: &sink,
                    source_artifact: &effective_source_artifact,
                    source_movement_seq,
                }),
            )
            .await?;

        let thread_shape_fingerprint = thread_shape_fingerprint(
            Some(&model),
            &bound_instructions,
            native_affordances,
            &static_context_fingerprint,
        )?;
        let retained_refresh = recovered.as_ref().and_then(|state| state.context_refresh.as_ref())
            .filter(|refresh| refresh.matches_thread(&thread_id, &thread_shape_fingerprint, &static_context_fingerprint)).cloned();
        let retiring_history = retirement.is_some();
        let native_history_retirement = retirement.or_else(|| recovered.as_ref()
            .and_then(|state| state.native_history_retirement.as_ref())
            .filter(|retirement| retirement.replacement_thread == thread_id).cloned());
        sink.set_continuity(CodexContinuityState {
            schema_version: 1,
            provider_interface: runtime.interface.clone(),
            run: runtime.run.as_str().to_string(),
            thread_id: thread_id.clone(),
            thread_shape_fingerprint,
            context_generation,
            static_context_fingerprint,
            turn_input: if recovered_tool_continuation.is_some() || cut_turn_continuation.is_some()
                || resume_control_id.is_some() || thread_compact_requested
            {
                recovered.as_ref().and_then(|state| state.turn_input.clone())
            } else {
                Some(effective_source_artifact.clone())
            },
            pending: recovered_tool_continuation
                .as_ref()
                .and_then(|_| recovered.as_ref().and_then(|state| state.pending.clone())),
            interrupted_pending: None,
            native_terminal: None,
            context_refresh: retained_refresh,
            native_history_retirement,
            tool_operator_request: recovered_tool_continuation.as_ref().and_then(|_| {
                recovered
                    .as_ref()
                    .and_then(|state| state.tool_operator_request.clone())
            }),
        });
        if retiring_history {
            sink.admit_native_history_retirement().await?;
        }
        if let Some(replaces) = &revised_history.replaces {
            sink.send(CodexTurnOutput::client(INTERFACE_CONTEXT_UPDATED_LABEL, serde_json::json!({
                "phase":"message_revised", "replaces_message":replaces,
                "source_artifact":effective_source_artifact,
            })))?;
        }

        if let Some(peer) = peer_message.as_ref().filter(|peer| peer.action == PeerAction::Queue) {
            let mut items = Vec::new();
            if cold_thread && let Some(context) = conversation_block.as_ref() { items.push(queued_peer_item(context.clone())); }
            items.push(queued_peer_item(peer_input_text(peer, &input.artifact)?));
            let request = ClientRequest::ThreadInjectItems {
                request_id: self.mint_request_id(), params: ThreadInjectItemsParams { thread_id: thread_id.clone(), items },
            };
            sink.admit_app_server_dispatch(&request, None).await?;
            self.client.lock().await.request_typed::<ThreadInjectItemsResponse>(request).await.map_err(typed_error)?;
            return sink.send(CodexTurnOutput::client(INTERFACE_CONTEXT_UPDATED_LABEL, serde_json::json!({
                "source_artifact": input.artifact, "peer_control_id": peer.control_id,
                "queued": true, "turn_triggered": false,
            })));
        }

        if let Some(control_id) = resume_control_id.as_ref() {
            sink.send(CodexTurnOutput::client(
                TURN_CONTROL_ACKNOWLEDGED_LABEL,
                serde_json::json!({
                    "control_id": control_id,
                    "action": "resume",
                    "status": "acknowledged",
                    "acknowledged": true,
                    "source_movement_seq": source_movement_seq,
                    "thread_id": thread_id,
                    "turn_id": serde_json::Value::Null,
                    "detail": {
                        "turn_trigger": TURN_TRIGGER_RESUME_INTERRUPTED_TASK,
                        "prompt_replayed": false,
                    },
                }),
            ))?;
        }

        if thread_compact_requested {
            return self.run_manual_compaction(turn_context, &thread_id).await;
        }

        let recovered_rollout_evidence = self
            .recovered_rollout_evidence(
                &scope,
                recovered_tool_continuation.as_ref(),
                interrupted_after_artifact.as_deref(),
                &recovered_handoff_artifacts,
                &recovered_turn_start_dispatches,
                &native_catalog.tool_names,
            )
            .await?;
        let recovered_item_already_injected = recovered_rollout_evidence.output_persisted;

        let additional_context = if let Some(context) = resumed_additional_context {
            context
        } else if let Some(cut) = cut_turn_continuation.as_ref() {
            native_turn_context(Some(cut.context.clone()), obligations_grant_block)
        } else if recovered_tool_continuation.is_some() || resume_control_id.is_some() {
            BTreeMap::new()
        } else {
            native_turn_context(if cold_thread { conversation_block } else { recovered_result_context }, obligations_grant_block)
        };
        if !thread_compact_requested
            && recovered_tool_continuation.is_none()
            && cut_turn_continuation.is_none()
            && resume_control_id.is_none()
            && lifecycle_prompt_event.is_none()
            && !lifecycle_start_resumed
        {
            let start = TurnStartTriggers {
                sink: &sink,
                lifecycle: lifecycle.as_ref(),
                run: runtime.run.as_str(),
                source_artifact: &effective_source_artifact,
                thread_id: &thread_id,
                cold_thread,
                first_turn_of_run,
            };
            if start.held(&turn_input, &additional_context)? {
                return Ok(());
            }
        }
        if first_turn_of_run && recovered_tool_continuation.is_none() && cut_turn_continuation.is_none() {
            self.emit_hooks_catalog(&sink).await?;
        }
        if let Some(continuation) = recovered_tool_continuation.as_ref()
            && !recovered_item_already_injected
        {
            self.inject_recovered_item(&sink, &recovered_rollout_evidence, &thread_id, &continuation.item).await?;
        }
        let turn = ClientRequest::TurnStart {
            request_id: self.mint_request_id(),
            params: fresh_turn_start_params(
                thread_id.clone(),
                turn_input,
                model.clone(),
                reasoning_effort.clone(),
                self.bound_service_tier()?,
                reasoning_summary,
                additional_context,
                if resume_control_id.is_some() || cut_turn_continuation.is_some() {
                    TURN_TRIGGER_RESUME_INTERRUPTED_TASK
                } else if recovered_tool_continuation.is_some() {
                    TURN_TRIGGER_TOOL_RESULT_RECOVERY
                } else {
                    TURN_TRIGGER_INPUT
                },
            ),
        };
        self.require_local_process_transport("turn").await?;
        sink.admit_app_server_dispatch(&turn, None).await?;
        let (turn_response, request_handle) = {
            let client = self.client.lock().await;
            let request_handle = client.request_handle();
            let response = client
                .request_typed::<octopus_appserver_proto::TurnStartResponse>(turn)
                .await
                .map_err(typed_error)?;
            (response, request_handle)
        };
        if turn_response.turn.id.trim().is_empty()
            || turn_response.turn.status != TurnStatus::InProgress
        {
            return Err(InterfaceDriverError::FailedClosed(format!(
                "turn/start returned an invalid active turn: id='{}' status={:?}",
                turn_response.turn.id, turn_response.turn.status
            )));
        }
        let turn_id = turn_response.turn.id;
        if let Some((activity, activity_already_admitted)) = recovered_tool_continuation
            .map(|continuation| (continuation.activity, continuation.activity_already_admitted))
            .or(cut_turn_continuation.map(|cut| (cut.activity, cut.activity_already_admitted)))
        {
            if !activity_already_admitted && !live_stream_rules.enabled() {
                sink.send(activity)?;
            }
            if first_turn_of_run {
                self.emit_hooks_catalog(&sink).await?;
            }
        }
        control.publish(CodexTurnControlTarget::Draining {
            handle: request_handle,
            request_ids: self.next_request_id.clone(),
            pending_requests: self.pending_server_requests.clone(),
            scope: scope.clone(),
            output: Arc::downgrade(&emit),
            continuity: sink.continuity_snapshot(),
            handoff_barrier: handoff_barrier.clone(),
            effect_gate: effect_gate.clone(),
            turn_interrupted: turn_interrupted.clone(),
            thread_id: thread_id.clone(),
            turn_id: turn_id.clone(),
        });

        let outcome = self.drain_turn(turn_context, &thread_id, &turn_id).await?;
        match outcome {
            TurnDrainOutcome::Suspended(_) | TurnDrainOutcome::Complete => Ok(()),
        }
    }
}
