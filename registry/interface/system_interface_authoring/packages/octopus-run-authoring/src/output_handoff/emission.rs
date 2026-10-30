use super::*;

impl ProviderOutputHandoff {
    pub async fn hand(&self, emission: ProviderEmission) -> Result<ProviderHandoffOutcome, String> {
        self.hand_batch_with_terminal(vec![emission.into()], false)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| "provider handoff produced no outcome".to_string())
    }

    pub async fn hand_transport_dispatch(
        &self,
        artifact: InterfaceArtifactFile,
    ) -> Result<ProviderHandoffOutcome, String> {
        let manifest = artifact.read_manifest().map_err(|error| {
            format!("provider transport dispatch manifest cannot be read: {error}")
        })?;
        if manifest.label != PROVIDER_TRANSPORT_DISPATCH_LABEL {
            return Err(format!(
                "provider transport dispatch manifest label '{}' differs from '{}'",
                manifest.label, PROVIDER_TRANSPORT_DISPATCH_LABEL
            ));
        }
        let payload = artifact.read_payload_bytes().map_err(|error| {
            format!("provider transport dispatch payload cannot be read: {error}")
        })?;
        let dispatch = validate_transport_dispatch_payload(
            &payload,
            &self.ctx.interface,
            self.source_movement_seq,
        )?;
        match &self.delivery_mode {
            HandoffDeliveryMode::Rearm => self
                .hand_batch_with_terminal(
                    vec![ProviderHandoffEmission {
                        artifact,
                        route: ProviderHandoffRoute::TransportDispatch {
                            attempt: dispatch.attempt,
                        },
                    }],
                    false,
                )
                .await?
                .into_iter()
                .next()
                .ok_or_else(|| "provider transport dispatch produced no outcome".to_string()),
            HandoffDeliveryMode::OneShot(plan) => {
                let _state = self.state.lock().await;
                let slot = plan.dispatch.get(&dispatch.attempt).ok_or_else(|| {
                    format!(
                        "provider one-shot topology has no preauthored transport slot for attempt {}",
                        dispatch.attempt
                    )
                })?;
                let completion_coordinate = RuntimeCoordinateKey::new(
                    &self.config.run,
                    &self.config.worker,
                    &self.config.provider_interface,
                    &plan.completion.checkpoint,
                    plan.completion.epoch,
                );
                let completion = self.session.index()?.admitted_slot_artifacts(
                    &completion_coordinate,
                    &plan.completion.required_mark,
                    &plan.completion.required_verifier,
                )?;
                if !completion.is_empty() {
                    return Err(
                        "provider one-shot transport dispatch follows completion evidence"
                            .to_string(),
                    );
                }
                if !self.one_shot_terminal_evidence(plan)?.is_empty() {
                    return Err(
                        "provider one-shot transport dispatch follows terminal evidence"
                            .to_string(),
                    );
                }
                self.hand_one_shot_artifact(artifact, slot).await
            }
        }
    }

    pub async fn hand_final(
        &self,
        emission: ProviderEmission,
    ) -> Result<ProviderHandoffOutcome, String> {
        self.hand_batch_with_terminal(vec![emission.into()], true)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| "provider handoff produced no outcome".to_string())
    }

    pub async fn hand_batch(
        &self,
        emissions: Vec<ProviderEmission>,
    ) -> Result<Vec<ProviderHandoffOutcome>, String> {
        self.hand_batch_with_terminal(emissions.into_iter().map(Into::into).collect(), true)
            .await
    }

    pub async fn hand_progress_batch(
        &self,
        emissions: Vec<ProviderEmission>,
    ) -> Result<Vec<ProviderHandoffOutcome>, String> {
        self.hand_batch_with_terminal(emissions.into_iter().map(Into::into).collect(), false)
            .await
    }

    pub async fn hand_observation(
        &self,
        emission: ProviderEmission,
        terminal: bool,
    ) -> Result<ProviderHandoffOutcome, String> {
        let label = emission.artifact.read_manifest().map_err(|error| error.to_string())?.label;
        if self.context_output_labels.contains(&label) || self.session_snapshot_labels.contains(&label) {
            return Err("a progress observation cannot change the selected context".into());
        }
        self.hand_batch_with_bindings(vec![emission.into()], terminal, &[], false, None, None).await?
            .into_iter().next().ok_or_else(|| "provider observation produced no outcome".into())
    }

    pub async fn hand_from_input(
        &self,
        emission: ProviderEmission,
        terminal: bool,
        source_movement_seq: u64,
        consume_context: bool,
    ) -> Result<ProviderHandoffOutcome, String> {
        if !matches!(self.delivery_mode, HandoffDeliveryMode::Rearm) {
            return Err("a response consuming an independent input requires run-authoring authority".into());
        }
        let label = emission
            .artifact
            .read_manifest()
            .map_err(|error| error.to_string())?
            .label;
        if !consume_context
            && (self.context_output_labels.contains(&label)
                || self.session_snapshot_labels.contains(&label))
        {
            return Err("a progress observation cannot change the selected context".into());
        }
        self.hand_batch_with_bindings(
            vec![emission.into()],
            terminal,
            &[],
            consume_context,
            Some(source_movement_seq),
            None,
        )
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| "provider input response produced no outcome".into())
    }

    pub(crate) fn independent_input_at_source(
        &self,
        current: &[u8],
        source_movement_seq: u64,
    ) -> Result<Option<RunAuthoringCheckpoint>, String> {
        let (coordinate, evidence) = self.session.index()?.exact_provider_source_outbound(
            &self.config.run,
            &self.config.worker,
            &self.config.provider_interface,
            source_movement_seq,
        )?;
        let hash = evidence
            .run_record_hash
            .as_deref()
            .and_then(ContentHash::from_hex)
            .ok_or("independent input has no exact admitted run snapshot")?;
        let bytes = octopus_interfaces::read_artifact_carrier_file_bounded(
            &self.ctx.blobs_dir.join(hash.to_hex()),
            self.ctx.required_host_io_limits()?.max_payload_bytes,
        ).map_err(|error| error.to_string())?;
        if ContentHash::of_bytes(&bytes) != hash {
            return Err("independent input snapshot hash differs".into());
        }
        let pinned = RunRecord::from_bytes(&bytes).map_err(|error| error.to_string())?;
        if pinned.id != self.config.run || pinned.worker != self.config.worker {
            return Err("independent input snapshot changed its run or worker".into());
        }
        let id = CheckpointId::new(&coordinate.checkpoint);
        let checkpoint = pinned
            .checkpoint(&id)
            .ok_or("independent input snapshot lost its checkpoint")?;
        let templates = self
            .config
            .checkpoints
            .iter()
            .filter(|template| {
                template.role.is_provider_input()
                    && checkpoint_record_matches_template(checkpoint, template, &self.config.run)
            })
            .collect::<Vec<_>>();
        if templates.len() != 1
            || checkpoint.epoch.0 != coordinate.epoch
            || !pinned.active_checkpoints.contains(&id)
            || !indexed_evidence_matches_checkpoint(&evidence, templates[0])
            || !artifact_refs_match_bindings(
                &evidence.artifact_binding_refs,
                &checkpoint.artifact_bindings,
            )
        {
            return Err(
                "independent input source differs from its exact admitted checkpoint".into(),
            );
        }
        let template = templates[0];
        if template.role != RunAuthoringCheckpointRole::IndependentInput || template.include_context
        {
            return Ok(None);
        }
        validate_unchanged_admitted_checkpoint(&bytes, current, &id)?;
        Ok(Some(template.clone()))
    }

    pub async fn hand_batch_with_outgoing_bindings(
        &self,
        emissions: Vec<ProviderEmission>,
        terminal: bool,
        bindings: &[ProviderOutgoingBinding],
    ) -> Result<Vec<ProviderHandoffOutcome>, String> {
        self.hand_batch_with_bindings(emissions.into_iter().map(Into::into).collect(), terminal, bindings, true, None, None).await
    }

    pub async fn hand_batch_with_capability_amendment(
        &self,
        emissions: Vec<ProviderEmission>,
        terminal: bool,
        amendment: &ProviderCapabilityAmendment,
    ) -> Result<Vec<ProviderHandoffOutcome>, String> {
        let emissions = emissions.into_iter().map(Into::into).collect::<Vec<_>>();
        if emissions.len() != 1 {
            return Err("a capability amendment requires one exact tool effect".into());
        }
        capability_amendment::validate_emission(amendment, &emissions[0])?;
        self.hand_batch_with_bindings(emissions, terminal, &[], true, None, Some(amendment)).await
    }

    pub(crate) async fn hand_batch_with_terminal(
        &self,
        emissions: Vec<ProviderHandoffEmission>,
        terminal: bool,
    ) -> Result<Vec<ProviderHandoffOutcome>, String> {
        self.hand_batch_with_bindings(emissions, terminal, &[], true, None, None).await
    }

    pub(crate) async fn hand_batch_with_bindings(
        &self,
        emissions: Vec<ProviderHandoffEmission>,
        terminal: bool,
        bindings: &[ProviderOutgoingBinding],
        consume_input: bool,
        independent_source: Option<u64>,
        amendment: Option<&ProviderCapabilityAmendment>,
    ) -> Result<Vec<ProviderHandoffOutcome>, String> {
        if emissions.is_empty() {
            return Err("provider response batch contains no emissions".to_string());
        }
        if let HandoffDeliveryMode::OneShot(plan) = &self.delivery_mode {
            if !bindings.is_empty() || amendment.is_some() {
                return Err("an outgoing binding requires a granted file-authoring continuation".into());
            }
            return self.hand_one_shot_batch(emissions, plan, terminal).await;
        }
        let batches = split_handoff_batches_by_artifact_name(emissions, &self.session_snapshot_labels)?;
        let batch_count = batches.len();
        let mut outcomes = Vec::new();
        for (index, batch) in batches.into_iter().enumerate() {
            let delivered = self.hand_rearm_batch_with_bindings(batch, terminal && index + 1 == batch_count, bindings, consume_input, independent_source.filter(|_| index == 0), amendment, None, None, !terminal)
                .await?;
            let failed = delivered.iter().any(|outcome| matches!(outcome, ProviderHandoffOutcome::TerminalFailure { .. }));
            outcomes.extend(delivered);
            if failed {
                break;
            }
        }
        Ok(outcomes)
    }

    pub(crate) async fn hand_rearm_batch_with_bindings(
        &self,
        emissions: Vec<ProviderHandoffEmission>,
        terminal: bool,
        bindings: &[ProviderOutgoingBinding],
        consume_input: bool,
        independent_source: Option<u64>,
        amendment: Option<&ProviderCapabilityAmendment>,
        settlement_of: Option<(&ArtifactRef, u64)>,
        withdrawal: Option<&source_withdrawal::Withdrawal>,
        standing: bool,
    ) -> Result<Vec<ProviderHandoffOutcome>, String> {
        let mut state = self.state.lock().await;
        let withdrawal_guard = if withdrawal.is_some() { Some(self.dispatch_ownership().await?) } else { None };
        self.reconcile_checked_run_state(&mut state).await?;
        let mut active = self.clone();
        let record = RunRecord::from_bytes(&state.run_record_bytes).map_err(|error| error.to_string())?;
        if let Some((predecessor, _)) = settlement_of {
            let prior = persisted_pending_handoff(&record, &self.config.provider_interface)?.ok_or("source settlement has no predecessor")?;
            if !self.source_carries_context || prior.items.len() != 1
                || !self.session_snapshot_labels.contains(&prior.items[0].artifact_label)
                || state.session_snapshot_head.as_ref().map(|head| &head.artifact) != Some(predecessor) {
                return Err("source settlement requires its exact currently selected parent snapshot".into());
            }
        }
        let topology: BoundRunAuthoringTopology = serde_json::from_value(
            record.interface_state.get(&self.config.provider_interface)
                .and_then(|state| state.get(RUN_AUTHORING_BINDING)).cloned()
                .ok_or("provider handoff lost its authored topology")?
        ).map_err(|error| error.to_string())?;
        topology.validate_identity(&record, &self.config.provider_interface)?;
        active.config.checkpoints = topology.checkpoints.clone();
        active.config.payload_artifact_bindings = topology.payload_artifact_bindings.clone();
        active.bound_topology = topology;
        let mut next_session_context = state.session_context.clone();
        let mut next_session_snapshot_head = state.session_snapshot_head.clone();
        let mut next_context = state.context_artifacts.clone();
        let mut next_static_context = state.static_context_artifacts.clone();
        let contributed_context_labels = active.contributed_context_output_labels();
        for binding in bindings {
            for artifact in &binding.artifact_bindings {
                append_artifact(&mut next_context, artifact.clone());
                append_artifact(&mut next_static_context, artifact.clone());
            }
        }
        let mut prepared = Vec::with_capacity(emissions.len());
        let mut standing_checkpoints_by_output = Vec::with_capacity(emissions.len());
        for emission in emissions {
            let ProviderHandoffEmission {
                mut artifact,
                route,
            } = emission;
            let manifest = artifact.read_manifest().map_err(|error| {
                format!("provider output artifact manifest cannot be read: {error}")
            })?;
            let context_output = contributed_context_labels.contains(&manifest.label);
            let snapshot_output = active.source_carries_context && active.session_snapshot_labels.contains(&manifest.label);
            let payload = if context_output {
                Some(artifact.read_payload_bytes().map_err(|error| {
                    format!("provider output artifact payload cannot be read: {error}")
                })?)
            } else {
                None
            };
            if snapshot_output {
                result_inputs::validate_snapshot_context(&active.config, &next_context, &next_static_context, &next_session_context)?;
                artifact = materialize_session_snapshot(
                    &artifact,
                    &next_session_context,
                    next_session_snapshot_head.as_ref(),
                )?;
                next_context = next_static_context.clone();
            }
            let identity = artifact_identity(&artifact, &active.var_dir)?;
            if context_output {
                append_session_payload(
                    &mut next_session_context,
                    &manifest.label,
                    payload
                        .as_deref()
                        .ok_or_else(|| "provider context output lost its payload".to_string())?,
                    &identity.artifact,
                    None,
                    "provider output",
                )?;
                append_artifact(&mut next_context, identity.artifact.id.clone());
            }
            if snapshot_output {
                next_session_snapshot_head = Some(session_snapshot_head_after_output(
                    identity.artifact.clone(),
                    &next_session_context,
                )?);
            }
            let mut output_checkpoint = active.config.output_checkpoint(
                &route,
                &active.granted_tools,
                &active.granted_interfaces,
                &manifest.label,
            )?;
            result_inputs::select_call_result(&mut output_checkpoint, &route)?;
            let verified_inputs = active.config.verified_input_checkpoints(
                &route,
                &output_checkpoint,
                &next_static_context,
                &active.granted_mark_outcomes,
                &active.granted_tools,
                &active.granted_interfaces,
            )?;
            standing_checkpoints_by_output.push(if standing && !snapshot_output && verified_inputs.is_empty() {
                active.standing_route_for(&record, &route, &output_checkpoint, &manifest.label)?
            } else {
                None
            });
            prepared.push(PreparedHandoff {
                identity,
                output_checkpoint,
                verified_inputs,
            });
        }
        let additional_input = independent_source.map(|source| active.independent_input_at_source(&state.run_record_bytes, source)).transpose()?.flatten();
        if let (Some(own), Some(additional)) = (&state.consumed_context_free_independent_input, &additional_input) {
            if own != additional { return Err("one response cannot consume two independent input contracts".into()); }
        }
        let independent_input = additional_input.as_ref().or(state.consumed_context_free_independent_input.as_ref());
        let context_changed = next_context != state.context_artifacts;
        let own_refresh = (consume_input && state.consumed_input) || context_changed || amendment.is_some();
        let standing_checkpoints = if standing
            && !terminal
            && bindings.is_empty()
            && amendment.is_none()
            && settlement_of.is_none()
            && withdrawal.is_none()
            && additional_input.is_none()
            && !self.failed_source
            && self.checked_settlement_output.is_none()
            && persisted_pending_handoff(&record, &self.config.provider_interface)?.is_some_and(|pending| {
                pending.withdrawal.is_none()
                    && !pending.publication_unfinished()
                    && if pending.terminal {
                        pending.source_movement_seq < self.source_movement_seq
                    } else {
                        pending.source_movement_seq == self.source_movement_seq
                    }
            })
        {
            standing_checkpoints_by_output.into_iter().collect::<Option<Vec<_>>>()
        } else {
            None
        };
        if let Some(checkpoints) = standing_checkpoints {
            let items = prepared
                .iter()
                .zip(checkpoints)
                .map(|(output, checkpoint)| PendingHandoffItem {
                    artifact_id: output.identity.artifact.id.clone(),
                    artifact_content_hash: output.identity.artifact.content.to_hex(),
                    artifact_label: output.identity.label.clone(),
                    manifest_relative_path: output.identity.manifest_relative_path.clone(),
                    checkpoint,
                })
                .collect();
            let (pending, outcomes) = active.hand_standing_batch(&record, items, u64::from(own_refresh)).await?;
            Self::ground_admitted_context_outputs(
                &mut next_session_context,
                &pending,
                &outcomes,
                &contributed_context_labels,
            )?;
            state.static_context_artifacts = next_static_context;
            state.session_context = next_session_context;
            state.session_snapshot_head = next_session_snapshot_head;
            state.context_artifacts = next_context;
            state.deferred_context_generations = state
                .deferred_context_generations
                .checked_add(u64::from(own_refresh))
                .ok_or("provider deferred context generation overflowed")?;
            if consume_input {
                state.consumed_input = false;
            }
            return Ok(outcomes);
        }
        let refresh_context = own_refresh || state.deferred_context_generations > 0;
        let standing_since_write = active.standing_items_since_write(&record)?;
        let mut authored = active.author_run_with_generations(
            &state.run_record_bytes,
            &next_context,
            &prepared,
            refresh_context,
            u64::from(own_refresh) + state.deferred_context_generations,
            independent_input,
            consume_input && state.consumed_verified_input,
            terminal,
            bindings,
            standing_since_write,
        )?;
        if let Some(amendment) = amendment {
            let topology = capability_amendment::apply_to_authored_run(
                &state.run_record_bytes, &mut authored, amendment, &active.config, &active.var_dir,
            )?;
            active.config.checkpoints = topology.checkpoints.clone();
            active.config.payload_artifact_bindings = topology.payload_artifact_bindings.clone();
            active.bound_topology = topology;
            let amended = RunRecord::from_bytes(authored.content.as_bytes()).map_err(|error| error.to_string())?;
            extend_amendment_context_bindings(
                &amended, &active.config.provider_interface, &mut next_context, &mut next_static_context,
            )?;
        }
        if let Some((predecessor, context_generation)) = settlement_of {
            active.attach_source_settlement(&state.run_record_bytes, &mut authored, predecessor, context_generation)?;
        }
        if let Some(proof) = withdrawal {
            active.attach_source_withdrawal(&state.run_record_bytes, &mut authored, proof)?;
            state.validated_withdrawal_output = Some(source_withdrawal::closure_ref(&authored.pending_handoff)?);
        }
        let pending = authored.pending_handoff.clone();
        match active.install_authored_run(&mut state, &authored).await? {
            AuthoredRunInstallOutcome::Installed => {}
            AuthoredRunInstallOutcome::TerminalFailure(outcome) => {
                return Ok(vec![outcome]);
            }
        }
        if withdrawal.is_some() { active.validate_installed_withdrawal(&state.run_record_bytes)?; }
        drop(withdrawal_guard);
        state.deferred_context_generations = 0;
        if consume_input {
            state.consumed_input = false;
        }
        state.consumed_context_free_independent_input = None;
        if terminal {
            state.consumed_verified_input = false;
        }
        let outcomes = active.hand_pending_items(&pending, &state.run_record_bytes).await?;
        Self::ground_admitted_context_outputs(
            &mut next_session_context,
            &pending,
            &outcomes,
            &contributed_context_labels,
        )?;
        state.static_context_artifacts = next_static_context;
        state.session_context = next_session_context;
        state.session_snapshot_head = next_session_snapshot_head;
        active.publish_admitted_context(&mut state).await?;
        if refresh_context {
            state.context_artifacts = active.published_context_artifacts(
                &RunRecord::from_bytes(&state.run_record_bytes).map_err(|error| error.to_string())?, &active.config,
            )?;
        }
        Ok(outcomes)
    }

    pub(crate) async fn reconcile_checked_run_state(&self, state: &mut HandoffState) -> Result<(), String> {
        let has_owner = self
            .config
            .checkpoints
            .iter()
            .any(|cp| cp.owner_authoring.is_some());
        if !has_owner && self.config.payload_artifact_bindings.is_empty() && !result_inputs::enabled(&self.config) {
            let current = octopus_interfaces::read_artifact_carrier_file_bounded(
                &self.run_path, self.ctx.required_host_io_limits()?.max_payload_bytes,
            ).map_err(|error| error.to_string())?;
            let current = RunRecord::from_bytes(&current).map_err(|error| error.to_string())?;
            if payload_artifact_bindings(&current, &self.config.provider_interface)?.is_empty() {
                return Ok(());
            }
        }
        {
            let after_seq = state
                .run_record_evidence_seq
                .ok_or("selected provider lost its admitted run snapshot provenance")?;
            let (current, chain) =
                await_checked_evidence(&self.ctx, self.config.result_timeout_ms, || {
                    checked_live_run_advance(
                        &self.ctx,
                        &self.run_path,
                        &state.run_record_bytes,
                        after_seq,
                        &self.config,
                    )
                })
                .await?;
            if current != state.run_record_bytes {
                let record = RunRecord::from_bytes(&current).map_err(|error| error.to_string())?;
                let mut advanced = state.clone();
                let before = RunRecord::from_bytes(&state.run_record_bytes).map_err(|error| error.to_string())?;
                result_inputs::validate_source_advance(self, &before, &record, &current)?;
                let selected = payload_artifact_bindings(&record, &self.config.provider_interface)?;
                let classified = payload_artifact_bindings(&before, &self.config.provider_interface)?.into_iter()
                    .chain(selected.iter().cloned())
                    .map(|reference| reference.id).collect::<BTreeSet<_>>();
                if !classified.is_empty() {
                    let readable = provider_context_bindings(&record, &self.config.provider_interface)?.into_iter()
                        .filter(|binding| binding.access.read).map(|binding| binding.id).collect::<BTreeSet<_>>();
                    // Attachment classification is not a future read grant. Keep
                    // non-payload static sources and the original invocation intact.
                    advanced.static_context_artifacts.retain(|id| !classified.contains(id) || readable.contains(id));
                    advanced.context_artifacts.retain(|id| !classified.contains(id) || readable.contains(id));
                    for reference in selected.iter().filter(|reference| readable.contains(&reference.id)) {
                        append_artifact(&mut advanced.static_context_artifacts, reference.id.clone());
                        append_artifact(&mut advanced.context_artifacts, reference.id.clone());
                    }
                }
                if let Some(last) = chain.last() {
                    advanced.run_record_evidence_seq = Some(last.call.seq.0);
                }
                advanced.run_record_bytes = current;
                self.restore_checked_context(&mut advanced, &record, &chain)?;
                *state = advanced;
            }
        }
        Ok(())
    }
}
