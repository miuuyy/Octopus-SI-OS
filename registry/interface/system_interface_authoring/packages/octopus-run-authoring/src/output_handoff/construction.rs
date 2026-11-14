use super::*;

impl ProviderOutputHandoff {
    pub(crate) fn contributed_context_output_labels(&self) -> BTreeSet<String> {
        // A context-free input can still append a declared response to the
        // existing bindings. It cannot replace history with a snapshot built
        // without that history (for example, an independent failure response).
        self.context_output_labels.iter()
            .filter(|label| self.source_carries_context || !self.session_snapshot_labels.contains(*label))
            .cloned().collect()
    }

    pub async fn input_changes_context(&self) -> bool {
        self.state.lock().await.consumed_input
    }

    pub fn validate_client_output_routes(&self, labels: &BTreeSet<String>) -> Result<(), String> {
        for label in labels {
            self.config.output_checkpoint(
                &ProviderHandoffRoute::ClientProjection,
                &self.granted_tools,
                &self.granted_interfaces,
                label,
            )?;
        }
        Ok(())
    }

    pub fn validate_model_tool_routes(
        &self,
        model_visible_tools: &BTreeSet<String>,
    ) -> Result<(), String> {
        for tool in model_visible_tools {
            self.config.output_checkpoint(
                &ProviderHandoffRoute::Tool { tool: tool.clone() },
                &self.granted_tools,
                &self.granted_interfaces,
                &self.config.file_write_label,
            )?;
        }
        Ok(())
    }

    pub fn callable_tool_names(&self, input: &InterfaceInvocationEnvelope) -> Result<BTreeSet<String>, String> {
        let metadata = input.metadata().map_err(|error| error.to_string())?;
        if metadata != &self.invocation_metadata { return Err("callable tool projection requires its exact accepted invocation metadata".into()); }
        let runtime = input.runtime().map_err(|error| error.to_string())?;
        if runtime.run != self.config.run || runtime.worker != self.config.worker || runtime.interface != self.config.provider_interface {
            return Err("callable tool projection differs from its accepted run, worker or provider".into());
        }
        self.config.callable_tool_names(metadata)
    }

    pub(crate) fn from_invocation_with_session(
        session: ProviderHandoffSession,
        ctx: &InterfaceLiftContext,
        descriptor: &InterfaceDescriptor,
        run: &RunId,
        input: &InterfaceInvocationEnvelope,
        source_movement_seq: u64,
        pinned_run_record: &[u8],
    ) -> Result<Self, CheckedRunError> {
        if descriptor.id != ctx.interface {
            return Err(format!(
                "provider descriptor '{}' does not match lifted interface '{}'",
                descriptor.id.as_str(),
                ctx.interface.as_str()
            ).into());
        }
        let profile = run_authoring_profile(descriptor)?;
        let runtime = input.runtime().map_err(|error| error.to_string())?;
        let granted_tools = input
            .granted_tool_names()
            .map_err(|error| error.to_string())?
            .into_iter()
            .collect::<BTreeSet<_>>();
        let granted_interfaces = input
            .metadata()
            .map_err(|error| error.to_string())?
            .allowed_interfaces()
            .iter()
            .map(InterfaceId::new)
            .collect::<BTreeSet<_>>();
        let granted_mark_outcomes = input
            .metadata()
            .map_err(|error| error.to_string())?
            .grant()
            .mark_outcomes
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        let (context_output_labels, session_snapshot_labels) =
            provider_context_label_sets(descriptor)?;
        let run_records_root = ctx.root_slot("run_records_root")?;
        if !run_records_root.is_absolute() {
            return Err(format!(
                "provider interface '{}' run_records_root is not absolute: {}",
                descriptor.id.as_str(),
                run_records_root.display()
            ).into());
        }
        let run_records_root = run_records_root.canonicalize().map_err(|error| {
            format!(
                "provider interface '{}' run_records_root '{}' cannot be canonicalized: {error}",
                descriptor.id.as_str(),
                run_records_root.display()
            )
        })?;
        let hand_in_root = ctx.root_slot("interface_hand_in_root")?;
        if !hand_in_root.is_absolute() {
            return Err(format!(
                "provider interface '{}' interface_hand_in_root is not absolute: {}",
                descriptor.id.as_str(),
                hand_in_root.display()
            ).into());
        }
        let initial_record = RunRecord::from_bytes(pinned_run_record).map_err(|error| {
            format!(
                "provider run '{}' pinned record is malformed: {error}",
                run.as_str()
            )
        })?;
        if initial_record.id != *run
            || runtime.run != *run
            || initial_record.worker != runtime.worker
            || runtime.interface != ctx.interface
        {
            return Err(format!(
                "provider pinned invocation topology has run='{}' worker='{}', metadata has run='{}' worker='{}' interface='{}', lifted interface is '{}'",
                initial_record.id.as_str(),
                initial_record.worker.as_str(),
                runtime.run.as_str(),
                runtime.worker.as_str(),
                runtime.interface.as_str(),
                ctx.interface.as_str()
            ).into());
        }
        let (config, bound_topology) = resolve_run_authoring_invocation(
            &profile,
            &ctx.interface,
            run,
            &initial_record,
            &runtime.checkpoint,
        )?;
        let mandatory_role_bindings = current_mandatory_role_bindings(&initial_record, &config)?;
        let consumed_checkpoint =
            initial_record
                .checkpoint(&runtime.checkpoint)
                .ok_or_else(|| {
                    format!(
                        "provider invocation names checkpoint '{}' absent from pinned run '{}'",
                        runtime.checkpoint.as_str(),
                        initial_record.id.as_str()
                    )
                })?;
        let consumed_templates = config
            .checkpoints
            .iter()
            .filter(|template| {
                template.role.is_provider_input()
                    && checkpoint_role_bindings_match(
                        consumed_checkpoint,
                        &mandatory_role_bindings,
                        template,
                    )
                    && consumed_checkpoint.interface_binding.as_ref()
                        == Some(&template.interface_binding)
                    && consumed_checkpoint.outbound_interface_bindings
                        == template.outbound_interface_bindings
                    && consumed_checkpoint.required_verified_artifacts
                        == template.required_verified_artifacts
            })
            .cloned()
            .collect::<Vec<_>>();
        if consumed_templates.len() > 1 {
            return Err(format!(
                "provider invocation checkpoint '{}' matches {} provider-input run_authoring templates",
                runtime.checkpoint.as_str(),
                consumed_templates.len()
            ).into());
        }
        if consumed_templates.is_empty() {
            return Err(format!(
                "provider invocation checkpoint '{}' is not one bound run_authoring input",
                runtime.checkpoint.as_str()
            ).into());
        }
        result_inputs::validate_invocation(input, &consumed_templates[0])?;
        let consumed_context_free_independent_input = (consumed_templates[0].role
            == RunAuthoringCheckpointRole::IndependentInput
            && !consumed_templates[0].include_context)
            .then(|| consumed_templates[0].clone());
        let consumed_verified_input =
            consumed_templates[0].role == RunAuthoringCheckpointRole::VerifiedInput;
        if consumed_checkpoint.epoch.0 != runtime.epoch {
            return Err(format!(
                "provider invocation checkpoint '{}' epoch {} differs from attested epoch {}",
                runtime.checkpoint.as_str(),
                consumed_checkpoint.epoch.0,
                runtime.epoch
            ).into());
        }
        let requested_run_path = run_records_root.join(format!("{}.json", run.as_str()));
        let run_path = requested_run_path.canonicalize().map_err(|error| {
            format!(
                "provider run '{}' current record '{}' cannot be canonicalized: {error}",
                run.as_str(),
                requested_run_path.display()
            )
        })?;
        if run_path.parent() != Some(run_records_root.as_path()) {
            return Err(format!(
                "provider run '{}' record '{}' escapes the granted run_records_root '{}'",
                run.as_str(),
                run_path.display(),
                run_records_root.display()
            ).into());
        }
        input_revision::validate_initial_history(ctx, &run_path, pinned_run_record, &config, source_movement_seq)?;
        let current_run_record = std::fs::read(&run_path).map_err(|error| {
            format!(
                "provider run '{}' current record '{}' cannot be read: {error}",
                run.as_str(),
                run_path.display()
            )
        })?;
        let current_record = RunRecord::from_bytes(&current_run_record).map_err(|error| {
            format!(
                "provider run '{}' current record is malformed: {error}",
                run.as_str()
            )
        })?;
        if current_record.id != config.run || current_record.worker != config.worker {
            return Err(format!(
                "provider current run topology has run='{}' worker='{}', config requires run='{}' worker='{}'",
                current_record.id.as_str(),
                current_record.worker.as_str(),
                config.run.as_str(),
                config.worker.as_str()
            ).into());
        }
        let (current_config, current_topology) = resolve_profile_invocation(
            &profile,
            &ctx.interface,
            run,
            &current_record,
            current_record
                .interface_state
                .get(&ctx.interface)
                .and_then(|state| state.get(RUN_AUTHORING_BINDING))
                .is_none()
                .then_some(&runtime.checkpoint),
        )?;
        let independent_input_unchanged = consumed_context_free_independent_input.is_some()
            && current_record.checkpoint(&runtime.checkpoint) == Some(consumed_checkpoint)
            && current_record.active_checkpoints.contains(&runtime.checkpoint)
            && bound_topology.payload_artifact_bindings == current_topology.payload_artifact_bindings
            && !input_revision::is_payload_revision(&initial_record, &current_record, &config.provider_interface)?
            && bound_topology.checkpoints.iter().filter(|template| template.role == RunAuthoringCheckpointRole::OwnerInput)
                .eq(current_topology.checkpoints.iter().filter(|template| template.role == RunAuthoringCheckpointRole::OwnerInput))
            && bound_topology.checkpoints.iter().filter(|template| template.role == RunAuthoringCheckpointRole::ResultInput)
                .eq(current_topology.checkpoints.iter().filter(|template| template.role == RunAuthoringCheckpointRole::ResultInput));
        let (coauthor_chain, run_record_evidence_seq) = stable_run_observation(
            &run_path, &current_run_record, ctx.required_host_io_limits()?.max_payload_bytes, || {
                if independent_input_unchanged {
                    validate_unchanged_admitted_checkpoint(
                        pinned_run_record, &current_run_record, &runtime.checkpoint,
                    )?;
                    if runtime.run_record_hash.as_deref() != Some(ContentHash::of_bytes(pinned_run_record).to_hex().as_str())
                        || runtime.ee != initial_record.ee
                        || !initial_record.active_checkpoints.contains(&runtime.checkpoint)
                        || current_config.checkpoints.iter().filter(|template| *template == &consumed_templates[0]).count() != 1
                        || current_mandatory_role_bindings(&current_record, &current_config)? != mandatory_role_bindings
                    {
                        return Err("context-free independent input changed its admitted contract or scope".into());
                    }
                    let mut index = session.index()?;
                    let (coordinate, evidence) = index.exact_provider_source_outbound(
                        run, &runtime.worker, &ctx.interface, source_movement_seq,
                    )?;
                    let metadata = input.metadata().map_err(|error| error.to_string())?;
                    if coordinate.checkpoint != runtime.checkpoint.as_str() || coordinate.epoch != runtime.epoch
                        || evidence.artifact != input.artifact || evidence.run_record_hash != runtime.run_record_hash
                        || evidence.source != *consumed_templates[0].interface()
                        || !indexed_evidence_matches_checkpoint(&evidence, &consumed_templates[0])
                        || !metadata_matches_checkpoint(metadata, &consumed_templates[0])
                        || evidence.artifact_binding_refs != metadata.artifact_binding_refs
                        || !artifact_refs_match_bindings(&evidence.artifact_binding_refs, &consumed_checkpoint.artifact_bindings)
                    {
                        return Err("context-free independent input has no exact admitted source".into());
                    }
                    Ok((Vec::new(), index.next_seq.saturating_sub(1).max(source_movement_seq)))
                } else {
                    let chain = checked_selected_run_advance_state(
                        ctx, &run_path, pinned_run_record, &current_run_record, source_movement_seq, &config,
                    )?;
                    let evidence_seq = chain.last().map(|transition| transition.call.seq.0).unwrap_or(source_movement_seq);
                    Ok((chain, evidence_seq))
                }
            },
        )?;
        let changed_outputs = output_connections::is_extension(&bound_topology, &current_topology)
            || output_connections::is_withdrawal(&bound_topology, &current_topology);
        let revised_inputs = input_revision::is_revision(&bound_topology, &current_topology);
        let revised_payloads = coauthor_chain.iter().any(|transition| {
            !transition.foreign && transition.writer.owner == config.provider_interface
                && input_revision::validate_payload_revision(&transition.before, &transition.after, &transition.writer).is_ok()
        });
        let amended_capabilities = capability_amendment::checked_topology_advance(
            &config.provider_interface, &bound_topology, &current_topology, &coauthor_chain,
        )?;
        let advanced_result_inputs = result_inputs::checked_topology_advance(
            &initial_record, &current_record, &bound_topology, &current_topology,
            &consumed_templates[0], &runtime.checkpoint, source_movement_seq, &coauthor_chain,
        )?;
        let retired_verified_input = !independent_input_unchanged && !changed_outputs && !revised_inputs && !revised_payloads && !amended_capabilities && !advanced_result_inputs
            && (current_config != config || current_topology != bound_topology);
        if retired_verified_input {
            validate_retired_verified_input(
                ctx,
                &session,
                &initial_record,
                &current_record,
                &config,
                &current_config,
                &bound_topology,
                &current_topology,
                &consumed_templates[0],
                source_movement_seq,
            )?;
        }
        let config = current_config;
        let bound_topology = current_topology;
        let effective_record = &current_record;
        let effective_record_bytes = current_run_record;
        let mut context_artifacts = current_context_artifacts(effective_record, &config)?;
        let mut static_context_artifacts = config.payload_artifact_bindings.iter().filter(|reference| context_artifacts.contains(&reference.id)).map(|reference| reference.id.clone()).collect::<Vec<_>>();
        let mut session_context = ResolvedSessionContext {
            provider_interface: config.provider_interface.clone(),
            artifacts: Vec::new(),
        };
        let mut session_snapshot_head = None;
        let consumes_context = consumed_templates[0].include_context;
        if !consumes_context {
            let payload_ids = input
                .artifact_binding_payloads
                .iter()
                .map(|bound| &bound.artifact.id)
                .collect::<Vec<_>>();
            let mut mandatory_ids = mandatory_role_bindings
                .iter()
                .map(|binding| &binding.id)
                .collect::<Vec<_>>();
            for id in &consumed_templates[0].static_artifact_bindings {
                if !mandatory_ids.contains(&id) { mandatory_ids.push(id); }
            }
            if payload_ids != mandatory_ids {
                return Err(format!(
                    "provider context-free invocation checkpoint '{}' does not carry exactly its mandatory role bindings",
                    runtime.checkpoint.as_str()
                ).into());
            }
        }
        let bound_provenance = session.index()?.bound_provenance(
            &config.run,
            &config.provider_interface,
            input
                .artifact_binding_payloads
                .iter()
                .map(|bound| &bound.artifact),
        )?;
        let input_sources = input_artifacts::owner_sources(&initial_record, &config.provider_interface)?;
        for bound in &input.artifact_binding_payloads {
            if input_sources.contains(&bound.artifact) && consumes_context {
                append_artifact(&mut context_artifacts, bound.artifact.id.clone());
                append_artifact(&mut static_context_artifacts, bound.artifact.id.clone());
            }
        }
        let context_payloads = context_artifact_payloads(&initial_record, input.metadata().map_err(|error| error.to_string())?, &input.artifact_binding_payloads)?;
        let instruction_sources = bound_instruction_sources(&context_payloads, &[])?;
        let declared_static = declared_static_context(&config, &consumed_templates[0]);
        for bound in &context_payloads {
            if consumes_context && consumed_templates[0].role != RunAuthoringCheckpointRole::ResultInput {
                append_artifact(&mut context_artifacts, bound.artifact.id.clone());
            }
            if input.metadata().map_err(|error| error.to_string())?.grant.role_artifacts.iter()
                .any(|id| id == bound.artifact.id.as_str())
                || instruction_sources.contains(&bound.artifact.id)
                || declared_static.contains(&bound.artifact.id) {
                append_artifact(&mut static_context_artifacts, bound.artifact.id.clone());
                continue;
            }
            collect_bound_context(
                ctx,
                &mut static_context_artifacts,
                &mut session_context,
                &mut session_snapshot_head,
                &bound.artifact,
                &bound.read_bytes().map_err(|error| error.to_string())?,
                bound_provenance
                    .get(&artifact_ref_key(&bound.artifact))
                    .cloned()
                    .unwrap_or_default(),
                &session_snapshot_labels,
                &config.provider_interface,
            )?;
        }
        ground_session_context_movements(&mut session_context, &session, &config.run, &config)?;
        validate_session_context_evidence(
            &mut session_context,
            SessionContextEvidenceScope {
                blobs_dir: &ctx.blobs_dir,
                limits: ctx.required_host_io_limits()?,
                run: &config.run,
                provider: &config.provider_interface,
                context_output_labels: &context_output_labels,
                session_snapshot_labels: &session_snapshot_labels,
            },
            &session,
        )?;
        let invocation_snapshot_head = session_snapshot_head.clone();
        reconcile_resolved_session_snapshot_head(&session_context, &mut session_snapshot_head)?;
        let invocation_session_snapshot =
            invocation_snapshot_head.map(|head| ProviderInvocationSessionSnapshot {
                carrier: head.artifact,
                artifacts: Arc::new(
                    session_context.artifacts[..=head.covered_artifact_count].to_vec(),
                ),
            });
        let consumed_input =
            if consumed_verified_input || consumed_context_free_independent_input.is_some() {
                false
            } else {
                let already_bound = context_artifacts
                    .iter()
                    .any(|artifact| artifact == &input.artifact.id);
                append_artifact(&mut context_artifacts, input.artifact.id.clone());
                let input_manifest = input
                    .read_artifact_manifest()
                    .map_err(|error| error.to_string())?;
                let input_payload = input
                    .read_artifact_payload()
                    .map_err(|error| error.to_string())?;
                let appended = append_session_payload(
                    &mut session_context,
                    &input_manifest.label,
                    &input_payload,
                    &input.artifact,
                    Some(source_movement_seq),
                    input.artifact.id.as_str(),
                )?;
                !already_bound && appended
            };
        let delivery_mode = derive_handoff_delivery_mode(
            input,
            &config,
            effective_record,
            &runtime.checkpoint,
            &granted_tools,
        )?;
        if consumed_templates[0].role == RunAuthoringCheckpointRole::ResultInput
            && !matches!(delivery_mode, HandoffDeliveryMode::Rearm)
        {
            return Err("result input has no current authority for its checked invocation transition".into());
        }
        if consumed_context_free_independent_input.is_some()
            && !matches!(delivery_mode, HandoffDeliveryMode::Rearm)
        {
            return Err(format!(
                "provider context-free independent input '{}' has no run re-arm authority",
                runtime.checkpoint.as_str()
            ).into());
        }
        let result_reader = LedgerTailReader::open_named(ctx, "run-authoring")?;
        let handoff = Self {
            ctx: ctx.clone(),
            invocation_metadata: input.metadata().map_err(|error| error.to_string())?.clone(),
            run_path,
            hand_in_root: hand_in_root.join(config.authoring_source_interface.as_str()),
            var_dir: ctx.interface_var_dir(),
            source_movement_seq,
            source_carries_context: consumes_context,
            failed_source: false,
            checked_settlement_output: None,
            config,
            context_output_labels,
            session_snapshot_labels,
            standing_candidate_labels: standing_routes::standing_candidate_labels(descriptor)?,
            granted_tools,
            granted_interfaces,
            granted_mark_outcomes,
            bound_topology,
            delivery_mode,
            session,
            invocation_session_snapshot,
            state: Arc::new(tokio::sync::Mutex::new(HandoffState {
            validated_withdrawal_output: None,
                context_artifacts,
                static_context_artifacts,
                session_context,
                session_snapshot_head,
                run_record_bytes: effective_record_bytes,
                run_record_evidence_seq: Some(run_record_evidence_seq),
                consumed_input,
                consumed_context_free_independent_input,
                consumed_verified_input,
                deferred_context_generations: 0,
                result_rebase: None,
            })),
            result_reader: Arc::new(tokio::sync::Mutex::new(result_reader)),
        };
        let observed_record = handoff.state.try_lock()
            .map_err(|_| "new handoff state is already borrowed".to_string())?
            .run_record_bytes.clone();
        stable_run_observation(&handoff.run_path, &observed_record, ctx.required_host_io_limits()?.max_payload_bytes, || {
            if let Some(target) = persisted_pending_handoff(&current_record, &handoff.config.provider_interface)?
                .filter(|pending| pending.deferred_publication)
                .map(|pending| handoff.pending_handoff_evidence(&pending).map(|evidence| (pending, evidence)))
                .transpose()?
                .filter(|(_, evidence)| evidence.iter().all(Option::is_some))
                .and_then(|(pending, _)| pending.context_publication)
            {
                let mut state = handoff.state.try_lock()
                    .map_err(|_| "new handoff state is already borrowed".to_string())?;
                for id in target { append_artifact(&mut state.context_artifacts, id); }
            }
            if !coauthor_chain.is_empty() && consumed_templates[0].role != RunAuthoringCheckpointRole::ResultInput {
                let mut state = handoff.state.try_lock()
                    .map_err(|_| "new handoff state is already borrowed".to_string())?;
                let publications = handoff.checked_context_publications(&coauthor_chain, &state.run_record_bytes)?;
                let input_entries = take_unpublished_context_inputs(&mut state, source_movement_seq)?;
                for transition in &coauthor_chain {
                    let authored = RunRecord::from_bytes(&transition.after).map_err(|error| error.to_string())?;
                    handoff.restore_published_context(&mut state, &current_record, &authored, &publications)?;
                }
                restore_consumed_context_inputs(&mut state, input_entries)?;
            }
            if retired_verified_input {
                let pending =
                    persisted_pending_handoff(&current_record, &handoff.config.provider_interface)?
                        .ok_or_else(|| {
                            "retired verified input lost its terminal handoff".to_string()
                        })?;
                if handoff
                    .pending_handoff_evidence(&pending)?
                    .iter()
                    .any(Option::is_none)
                {
                    return Err(
                        "queued tool result precedes the admitted provider decision handoff".into(),
                    );
                }
                let mut state = handoff
                    .state
                    .try_lock()
                    .map_err(|_| "new handoff state is already borrowed".to_string())?;
                handoff.restore_pending_context(&mut state, &current_record, &pending)?;
                if state.consumed_input {
                    append_artifact(&mut state.context_artifacts, input.artifact.id.clone());
                }
            }
            if consumed_templates[0].role != RunAuthoringCheckpointRole::ResultInput {
                let mut state = handoff.state.try_lock()
                    .map_err(|_| "new handoff state is already borrowed".to_string())?;
                handoff.restore_failed_context(&mut state, &coauthor_chain)?;
            }
            Ok(())
        })?;
        Ok(handoff)
    }

    pub fn invocation_session_snapshot(&self) -> Option<ProviderInvocationSessionSnapshot> {
        self.invocation_session_snapshot.clone()
    }

    pub async fn recovered_failed_context_sources(&self) -> Result<Vec<u64>, String> {
        self.observe_live_record(|handoff, state| {
            let recovered = handoff.checked_failed_source_contexts(&state.run_record_bytes, &[], None)?;
            Self::restored_failed_context_sources(&state.session_context, recovered)
        }).await
    }

    pub async fn recovered_failed_context_source(&self, sequence: u64) -> Result<bool, String> {
        self.observe_live_record(|handoff, state| {
            let recovered = handoff.checked_failed_source_contexts(&state.run_record_bytes, &[], Some(sequence))?;
            Ok(Self::restored_failed_context_sources(&state.session_context, recovered)?.contains(&sequence))
        }).await
    }

    pub(crate) fn restored_failed_context_sources(
        restored: &ResolvedSessionContext, recovered: BTreeMap<u64, (ResolvedSessionContext, Vec<ArtifactRef>)>,
    ) -> Result<Vec<u64>, String> {
        let mut sources = Vec::new();
        for (source, (context, _)) in recovered {
            let mut previous = None;
            for entry in &context.artifacts {
                let position = restored.artifacts.iter()
                    .position(|existing| existing.source_artifact == entry.source_artifact)
                    .ok_or("checked failed source context was not restored into this handoff")?;
                if previous.is_some_and(|prior| prior >= position)
                    || !session_context_entries_agree(&restored.artifacts[position], entry)
                {
                    return Err("checked failed source context conflicts with this handoff".into());
                }
                previous = Some(position);
            }
            sources.push(source);
        }
        Ok(sources)
    }

    pub async fn current_run_record_bytes(&self) -> Vec<u8> {
        self.state.lock().await.run_record_bytes.clone()
    }

    pub async fn deferred_context_generations(&self) -> u64 {
        self.state.lock().await.deferred_context_generations
    }

    pub async fn context_generation(&self) -> Result<u64, String> {
        let state = self.state.lock().await;
        let record = RunRecord::from_bytes(&state.run_record_bytes).map_err(|error| error.to_string())?;
        optional_u64(
            record.interface_state.get(&self.config.provider_interface).ok_or("provider run has no provider state")?,
            "context_generation",
        )?
        .checked_add(state.deferred_context_generations)
        .ok_or_else(|| "provider context generation overflowed".to_string())
    }

    pub async fn reconciled_run_record_bytes(&self) -> Result<Vec<u8>, String> {
        let mut state = self.state.lock().await;
        self.reconcile_checked_run_state(&mut state).await?;
        Ok(state.run_record_bytes.clone())
    }

    pub async fn consumes_context_free_independent_input(&self) -> bool {
        self.state
            .lock()
            .await
            .consumed_context_free_independent_input
            .is_some()
    }

    pub async fn current_session_context_artifacts(&self) -> Arc<Vec<SessionContextArtifact>> {
        Arc::new(self.state.lock().await.session_context.artifacts.clone())
    }
}
