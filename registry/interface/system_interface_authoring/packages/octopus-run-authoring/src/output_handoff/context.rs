use super::*;

impl ProviderOutputHandoff {
    pub(crate) fn checked_failed_source_contexts(
        &self,
        current: &[u8],
        observed_chain: &[CheckedRunTransition],
        requested: Option<u64>,
    ) -> Result<BTreeMap<u64, (ResolvedSessionContext, Vec<ArtifactRef>)>, String> {
        if self.failed_source || !self.source_carries_context { return Ok(BTreeMap::new()); }
        let limits = self.ctx.required_host_io_limits()?;
        stable_run_observation(&self.run_path, current, limits.max_payload_bytes, || {
            let current_record = RunRecord::from_bytes(current).map_err(|error| error.to_string())?;
            let mut candidates = BTreeMap::new();
            if let Some(sequence) = requested {
                candidates.insert(sequence, None);
            } else {
                let mut seeds = vec![current_record.clone()];
                for transition in observed_chain {
                    seeds.push(RunRecord::from_bytes(&transition.before).map_err(|error| error.to_string())?);
                    seeds.push(RunRecord::from_bytes(&transition.after).map_err(|error| error.to_string())?);
                }
                let has_source = {
                    let mut index = self.session.index()?;
                    index.catch_up()?;
                    index.coordinate_outbounds.iter().any(|(coordinate, rows)| {
                        coordinate.run == self.config.run.as_str()
                            && coordinate.worker == self.config.worker.as_str()
                            && coordinate.interface == self.config.provider_interface.as_str()
                            && rows.iter().any(|row| row.seq == self.source_movement_seq)
                    })
                };
                if has_source {
                    let (_, source) = self.session.index()?.exact_provider_source_outbound(
                        &self.config.run, &self.config.worker, &self.config.provider_interface, self.source_movement_seq,
                    )?;
                    seeds.push(indexed_source_run_record(&self.ctx, &source, self.source_movement_seq)?);
                }
                for record in seeds {
                    if record.id != current_record.id || record.worker != current_record.worker {
                        return Err("failed context history changed run scope".into());
                    }
                    if let Some(pending) = persisted_pending_handoff(&record, &self.config.provider_interface)? {
                        if pending.source_without_context && pending.terminal {
                            if candidates.insert(pending.source_movement_seq, Some(pending.clone()))
                                .is_some_and(|previous| previous.as_ref() != Some(&pending)) {
                                return Err("failed context source has conflicting terminal handoffs".into());
                            }
                        } else if pending.terminal
                            && !candidates.contains_key(&pending.source_movement_seq)
                            && self.unsubmitted_source_failure_before(&pending, self.source_movement_seq)? {
                            candidates.insert(pending.source_movement_seq, Some(pending));
                        }
                    }
                }
            }
            let profile = RunAuthoringProfile {
                file_write_tool: self.config.file_write_tool.clone(),
                file_write_label: self.config.file_write_label.clone(),
                file_write_result_label: self.config.file_write_result_label.clone(),
            };
            let mut recovered = BTreeMap::new();
            for (sequence, expected_pending) in candidates {
                let (coordinate, source) = self.session.index()?.exact_provider_source_outbound(
                    &self.config.run, &self.config.worker, &self.config.provider_interface, sequence,
                )?;
                let hash = source.run_record_hash.as_deref().and_then(ContentHash::from_hex)
                    .ok_or("failed context source has no pinned run snapshot")?;
                let (before, original) = pinned_run_record(&self.ctx, &hash, "failed context source")?;
                let checkpoint_id = CheckpointId::new(&coordinate.checkpoint);
                let (config, _) = resolve_run_authoring_invocation(
                    &profile, &self.config.provider_interface, &self.config.run, &original, &checkpoint_id,
                )?;
                let checkpoint = original.checkpoint(&checkpoint_id)
                    .ok_or("failed context source lost its pinned checkpoint")?;
                let templates = config.checkpoints.iter().filter(|template| {
                    template.role.is_provider_input() && template.interface() == &source.source
                        && checkpoint_record_matches_template(checkpoint, template, &original.id)
                }).collect::<Vec<_>>();
                if templates.len() != 1 || !original.active_checkpoints.contains(&checkpoint_id)
                    || checkpoint.epoch.0 != coordinate.epoch
                    || !indexed_evidence_matches_checkpoint(&source, templates[0]) {
                    return Err("failed context source has no exact pinned input contract".into());
                }
                if !templates[0].include_context { continue; }
                let chain = checked_provider_run_advance_state(
                    &self.ctx, &self.run_path, &before, current, sequence, &config,
                )?;
                let mut terminal = None;
                let mut pending: Option<PendingHandoff> = None;
                let mut superseded_at = None;
                for (position, transition) in chain.iter().enumerate() {
                    let record = RunRecord::from_bytes(&transition.after).map_err(|error| error.to_string())?;
                    let next = persisted_pending_handoff(&record, &config.provider_interface)?;
                    if !transition.placement && transition.writer.owner == config.provider_interface && next.as_ref().is_some_and(|next| {
                        next.source_movement_seq == sequence && next.terminal && next.source_without_context
                    }) {
                        if pending.as_ref().is_some_and(|pending| Some(pending) != next.as_ref()) {
                            return Err("checked failed context source changed its terminal handoff".into());
                        }
                        terminal.get_or_insert(position);
                        pending = next;
                    } else if terminal.is_some() && superseded_at.is_none()
                        && next.as_ref().is_none_or(|next| pending.as_ref().is_none_or(|pending| next.items != pending.items)) {
                        superseded_at = Some(transition.call.seq.0);
                    }
                }
                if terminal.is_none() {
                    if let Some(expected) = expected_pending.as_ref().filter(|pending| !pending.source_without_context) {
                        for (position, transition) in chain.iter().enumerate() {
                            let record = RunRecord::from_bytes(&transition.after).map_err(|error| error.to_string())?;
                            let next = persisted_pending_handoff(&record, &config.provider_interface)?;
                            if !transition.placement && transition.writer.owner == config.provider_interface
                                && next.as_ref() == Some(expected) {
                                terminal.get_or_insert(position);
                                pending = next;
                            } else if terminal.is_some() && superseded_at.is_none() {
                                superseded_at = Some(transition.call.seq.0);
                            }
                        }
                    }
                }
                let terminal = terminal.ok_or("failed context source has no checked terminal boundary")?;
                let pending = pending.ok_or("failed context source lost its checked terminal failure")?;
                if expected_pending.as_ref().is_some_and(|expected| expected != &pending) {
                    return Err("observed failed handoff differs from its checked terminal failure".into());
                }
                let unsubmitted_failure = !pending.source_without_context
                    && self.unsubmitted_source_failure_before(&pending, superseded_at.unwrap_or(self.source_movement_seq))?;
                let outcomes = if unsubmitted_failure { Vec::new() } else { self.admitted_replay_handoff(&pending)? };
                if !unsubmitted_failure && (outcomes.is_empty() || outcomes.iter().any(|outcome| match outcome {
                    ProviderHandoffOutcome::Admitted { seq, .. } => {
                        *seq <= chain[terminal].call.seq.0
                            || superseded_at.is_some_and(|superseded| *seq >= superseded)
                    }
                    _ => true,
                })) {
                    return Err("failed context terminal admission has invalid causality".into());
                }
                let evidence = CheckedRunEvidence {
                    movements_path: &self.ctx.movements_path, ledger_index_path: &self.ctx.ledger_index_path,
                    blobs_dir: &self.ctx.blobs_dir, max_control_frame_bytes: limits.max_control_frame_bytes,
                    max_manifest_bytes: limits.max_manifest_bytes, max_payload_bytes: limits.max_payload_bytes,
                };
                let correction = input_revision::checked_failed_source_static_context(
                    &evidence, &self.run_path, &chain, &config.provider_interface, &source,
                )?;
                let (context, _, _) = recover_source_session_context_with_static_correction(
                    &self.ctx.blobs_dir, limits, &self.session, &original, &config, sequence, &pending.rebase,
                    &self.context_output_labels, &self.session_snapshot_labels, correction.as_ref(),
                )?;
                recovered.insert(sequence, (context, pending.rebase.clone()));
            }
            Ok(recovered)
        }).map_err(Into::into)
    }

    pub(crate) fn restore_failed_context(
        &self, state: &mut HandoffState, chain: &[CheckedRunTransition],
    ) -> Result<(), String> {
        let recovered = self.checked_failed_source_contexts(&state.run_record_bytes, chain, None)?;
        if recovered.is_empty() { return Ok(()); }
        let mut next = state.clone();
        for (sequence, (context, rebase)) in recovered {
            let pinned = unpublished_context_start(&next)?;
            let recovered = context.artifacts.iter()
                .filter(|entry| !next.session_context.artifacts[..pinned].iter().any(|kept| kept.source_artifact == entry.source_artifact))
                .cloned().collect::<Vec<_>>();
            let merged = merge_failed_source_context(&next.session_context.artifacts[pinned..], &recovered, sequence, Some(self.source_movement_seq), &rebase)?;
            next.session_context.artifacts.truncate(pinned);
            next.session_context.artifacts.extend(merged);
            reconcile_resolved_session_snapshot_head(&next.session_context, &mut next.session_snapshot_head)?;
        }
        validate_session_context_evidence(&mut next.session_context, SessionContextEvidenceScope {
            blobs_dir: &self.ctx.blobs_dir, limits: self.ctx.required_host_io_limits()?,
            run: &self.config.run, provider: &self.config.provider_interface,
            context_output_labels: &self.context_output_labels, session_snapshot_labels: &self.session_snapshot_labels,
        }, &self.session)?;
        for entry in &next.session_context.artifacts {
            append_artifact(&mut next.context_artifacts, entry.source_artifact.id.clone());
        }
        *state = next;
        Ok(())
    }

    pub(crate) fn restore_checked_context(
        &self, state: &mut HandoffState, current: &RunRecord, chain: &[CheckedRunTransition],
    ) -> Result<(), String> {
        if self.result_input_template()?.is_some() {
            let published = self.published_context_artifacts(current, &self.config)?;
            reattach_unpublished_context_inputs(state, self.source_movement_seq, published)?;
            return self.extend_result_rebase(state, current);
        }
        let mut next = state.clone();
        self.restore_replayed_context(&mut next, current, chain)?;
        self.restore_failed_context(&mut next, chain)?;
        *state = next;
        Ok(())
    }

    fn extend_result_rebase(&self, state: &mut HandoffState, current: &RunRecord) -> Result<(), String> {
        if !result_inputs::retains_invocation(self, current) {
            return Ok(());
        }
        let Some(recorded) = state.result_rebase.clone() else {
            return Ok(());
        };
        let runtime = self.invocation_metadata.runtime.as_ref().ok_or("result source coordinate absent")?;
        let row = current.checkpoint(&runtime.checkpoint).ok_or("result rebase lost its receiving row")?;
        let config = attempt_config(current, &self.config)?;
        let templates = config.checkpoints.iter().filter(|template| {
            template.role == RunAuthoringCheckpointRole::ResultInput
                && checkpoint_record_matches_template(row, template, &current.id)
        }).collect::<Vec<_>>();
        let [template] = templates.as_slice() else {
            return Err("result rebase has no unique receiving contract".into());
        };
        let extended = result_inputs::result_rebase(current, &config, template, row)?;
        if !extended.starts_with(&recorded) {
            return Err("an advance before the consuming write changed the recorded result rebase".into());
        }
        if extended == recorded {
            return Ok(());
        }
        self.require_carried_admitted_context(state, &extended)?;
        self.apply_result_rebase(state, &extended)
    }

    pub(crate) fn checked_context_publications(
        &self,
        chain: &[CheckedRunTransition],
        current: &[u8],
    ) -> Result<ContextPublications, String> {
        let mut publications = BTreeMap::new();
        let mut sources = BTreeSet::new();
        let profile = RunAuthoringProfile {
            file_write_tool: self.config.file_write_tool.clone(),
            file_write_label: self.config.file_write_label.clone(),
            file_write_result_label: self.config.file_write_result_label.clone(),
        };
        for transition in chain {
            let authored = RunRecord::from_bytes(&transition.after)
                .map_err(|error| error.to_string())?;
            let Some(mut pending) =
                persisted_pending_handoff(&authored, &self.config.provider_interface)?
                    .map(|pending| pending.with_standing_items())
            else {
                continue;
            };
            let authored_config = config_from_persisted_profile(&authored, &self.config.provider_interface, &profile)?;
            let visible = self.published_context_artifacts(&authored, &authored_config)?
                .into_iter().collect::<BTreeSet<_>>();
            pending.items.retain(|item| visible.contains(&item.artifact_id)
                && self.context_output_labels.contains(&item.artifact_label));
            self.extend_admitted_context_publications(
                &authored, &authored_config, &pending, &mut sources, &mut publications,
            )?;
        }
        for (_, (context, _)) in self.checked_failed_source_contexts(current, chain, None)? {
            for entry in context.artifacts {
                let sequence = entry.source_movement_seq.ok_or("recovered failed context has no admitted movement")?;
                self.record_context_publication(&mut publications, &entry.source_artifact, sequence)?;
            }
        }
        Ok(publications)
    }

    pub(crate) fn extend_admitted_context_publications(
        &self, record: &RunRecord, config: &RunAuthoringConfig, pending: &PendingHandoff,
        sources: &mut BTreeSet<u64>, publications: &mut ContextPublications,
    ) -> Result<(), String> {
        if pending.items.iter().any(|item| self.session_snapshot_labels.contains(&item.artifact_label))
            && !pending.source_without_context
            && sources.insert(pending.source_movement_seq)
            && !self.source_is_context_free(pending.source_movement_seq)?
        {
            let (context, _) = recover_source_session_context(
                &self.ctx, &self.session, record, config,
                pending.source_movement_seq, &pending.rebase, &self.context_output_labels,
                &self.session_snapshot_labels,
            )?;
            for entry in context.artifacts {
                let sequence = entry.source_movement_seq
                    .ok_or("checked context source has no admitted movement")?;
                self.record_context_publication(publications, &entry.source_artifact, sequence)?;
            }
        }
        let evidence = self.pending_handoff_evidence(pending)?;
        for (item, sequence) in pending.items.iter().zip(evidence) {
            let sequence = sequence
                .ok_or("published context has no admitted provider output evidence")?;
            let (_, source) = load_pending_artifact(&self.var_dir, item)?;
            self.record_context_publication(publications, &source, sequence)?;
        }
        Ok(())
    }

    pub(crate) fn record_context_publication(
        &self, publications: &mut ContextPublications, artifact: &ArtifactRef, sequence: u64,
    ) -> Result<(), String> {
        let sequences = publications.entry(artifact_ref_key(artifact)).or_default();
        if let Some(previous) = sequences.first().copied().filter(|_| !sequences.contains(&sequence)) {
            let mut same_delivery = false;
            for known in sequences.iter().copied() {
                if self.session.index()?.retained_delivery.same_output_delivery(
                    &self.config.run, &self.config.worker, artifact, known, sequence,
                )? { same_delivery = true; break; }
            }
            if !same_delivery {
                return Err(format!("checked context source '{}' changed its admitted movement from {previous} to {sequence}", artifact.id));
            }
        }
        sequences.insert(sequence);
        Ok(())
    }

    pub(crate) fn restore_published_context(
        &self,
        state: &mut HandoffState,
        current: &RunRecord,
        authored: &RunRecord,
        publications: &ContextPublications,
    ) -> Result<(), String> {
        let Some(mut pending) = persisted_pending_handoff(authored, &self.config.provider_interface)?
            .map(|pending| pending.with_standing_items()) else {
            return Ok(());
        };
        let visible = self.published_context_artifacts(current, &self.config)?.into_iter().collect::<BTreeSet<_>>();
        pending.items.retain(|item| visible.contains(&item.artifact_id)
            && self.context_output_labels.contains(&item.artifact_label));
        if pending.items.is_empty() { return Ok(()); }
        if self.pending_handoff_evidence(&pending)?.iter().any(Option::is_none) {
            return Err("published context has no admitted provider output evidence".into());
        }
        self.restore_pending_context_with_publications(state, current, &pending, publications)
    }

    pub(crate) fn published_context_artifacts(&self, record: &RunRecord, config: &RunAuthoringConfig) -> Result<Vec<ArtifactId>, String> {
        let deferred = persisted_pending_handoff(record, &config.provider_interface)?.filter(|pending| pending.deferred_publication);
        match deferred {
            Some(pending) if self.pending_handoff_evidence(&pending)?.iter().all(Option::is_some) => {
                effective_context_artifacts(record, config)
            }
            _ => current_context_artifacts(record, config),
        }
    }

    pub(crate) fn restore_pending_context(
        &self,
        state: &mut HandoffState,
        record: &RunRecord,
        pending: &PendingHandoff,
    ) -> Result<(), String> {
        self.restore_pending_context_with_publications(state, record, &pending.with_standing_items(), &BTreeMap::new())
    }

    pub(crate) fn source_is_context_free(&self, source: u64) -> Result<bool, String> {
        if source == self.source_movement_seq {
            return Ok(!self.source_carries_context);
        }
        let (coordinate, evidence) = self.session.index()?.exact_provider_source_outbound(
            &self.config.run, &self.config.worker, &self.config.provider_interface, source,
        )?;
        let record = indexed_source_run_record(&self.ctx, &evidence, source)?;
        let checkpoint = record.checkpoint(&CheckpointId::new(&coordinate.checkpoint))
            .ok_or("context recovery source lost its admitted checkpoint")?;
        let profile = RunAuthoringProfile {
            file_write_tool: self.config.file_write_tool.clone(),
            file_write_label: self.config.file_write_label.clone(),
            file_write_result_label: self.config.file_write_result_label.clone(),
        };
        let (config, _) = resolve_run_authoring_invocation(
            &profile, &self.config.provider_interface, &self.config.run, &record, &checkpoint.id,
        )?;
        let templates = config.checkpoints.iter().filter(|template| {
            template.role.is_provider_input()
                && checkpoint_record_matches_template(checkpoint, template, &record.id)
        }).collect::<Vec<_>>();
        let [template] = templates.as_slice() else {
            return Err("context recovery source has no unique provider input contract".into());
        };
        if record.worker != self.config.worker
            || !record.active_checkpoints.contains(&checkpoint.id)
            || checkpoint.epoch.0 != coordinate.epoch
            || template.interface() != &evidence.source
            || !indexed_evidence_matches_checkpoint(&evidence, template)
        {
            return Err("context recovery source differs from its exact admitted checkpoint".into());
        }
        Ok(!template.include_context)
    }

    pub(crate) fn restore_pending_context_with_publications(
        &self,
        state: &mut HandoffState,
        record: &RunRecord,
        pending: &PendingHandoff,
        publications: &ContextPublications,
    ) -> Result<(), String> {
        if pending.source_movement_seq != self.source_movement_seq && self.result_input_template()?.is_some() {
            let inputs = take_unpublished_context_inputs(state, self.source_movement_seq)?;
            state.context_artifacts = self.published_context_artifacts(record, &self.config)?;
            restore_consumed_context_inputs(state, inputs)?;
            return Ok(());
        }
        if pending.source_movement_seq == self.source_movement_seq {
            self.apply_result_rebase(state, &pending.rebase)?;
        }
        let mut context = state.session_context.clone();
        let mut snapshot_head = state.session_snapshot_head.clone();
        let visible = current_context_artifacts(record, &self.config)?
            .into_iter().collect::<BTreeSet<_>>();
        for item in &pending.items {
            if !self.context_output_labels.contains(&item.artifact_label) {
                continue;
            }
            if pending.context_publication.is_none() && !visible.contains(&item.artifact_id)
                && (pending.source_without_context || self.source_is_context_free(pending.source_movement_seq)?)
            {
                continue;
            }
            let (artifact, source) = load_pending_artifact(&self.var_dir, item)?;
            let payload = artifact.read_payload_bytes().map_err(|error| {
                format!("provider recovered context payload cannot be read: {error}")
            })?;
            if self.session_snapshot_labels.contains(&item.artifact_label) {
                let mut recovered = resolve_session_snapshot_chain(
                    &self.ctx.blobs_dir,
                    self.ctx.required_host_io_limits()?,
                    &source,
                    &payload,
                    None,
                    &self.session_snapshot_labels,
                    &self.config.provider_interface,
                )?;
                let snapshot_artifact_count = recovered.len();
                let mut current_publications = std::borrow::Cow::Borrowed(publications);
                if !current_publications.contains_key(&artifact_ref_key(&source))
                    && {
                        let recovered_refs = recovered.iter().map(|entry| artifact_ref_key(&entry.source_artifact)).collect::<BTreeSet<_>>();
                        let existing_refs = context.artifacts.iter().map(|entry| artifact_ref_key(&entry.source_artifact)).collect::<BTreeSet<_>>();
                        !recovered_refs.is_subset(&existing_refs)
                            && context.artifacts.iter().zip(&recovered)
                                .any(|(existing, candidate)| existing.source_artifact != candidate.source_artifact)
                    }
                    && persisted_pending_handoff(record, &self.config.provider_interface)?
                        .map(|persisted| persisted.with_standing_items()).as_ref() == Some(pending)
                {
                    let mut owner = self.clone();
                    owner.source_carries_context = !self.source_is_context_free(pending.source_movement_seq)?;
                    owner.source_movement_seq = pending.source_movement_seq;
                    let chain = owner.completed_source_chain(&state.run_record_bytes)?;
                    current_publications = std::borrow::Cow::Owned(owner.checked_context_publications(&chain, &state.run_record_bytes)?);
                    self.extend_admitted_context_publications(
                        record, &self.config, pending, &mut BTreeSet::new(), current_publications.to_mut(),
                    )?;
                }
                if reconcile_recovered_session_snapshot(
                    &context.artifacts, &mut recovered, &current_publications, &source,
                )? {
                    context.artifacts = recovered;
                    snapshot_head = if context.artifacts.len() == snapshot_artifact_count {
                        Some(session_snapshot_head_after_output(source, &context)?)
                    } else {
                        None
                    };
                }
            } else {
                let payload: serde_json::Value = serde_json::from_slice(&payload)
                    .map_err(|error| format!("provider recovered output is not JSON: {error}"))?;
                if let Some(existing) = context
                    .artifacts
                    .iter()
                    .find(|entry| entry.source_artifact == source)
                {
                    if existing.label != item.artifact_label || existing.payload != payload {
                        return Err(format!(
                            "provider recovered output '{}' differs from existing session context",
                            source.id.as_str()
                        ));
                    }
                } else {
                    context.artifacts.push(SessionContextArtifact {
                        label: item.artifact_label.clone(),
                        payload,
                        source_artifact: source,
                        source_movement_seq: None,
                    });
                }
            }
        }
        validate_session_context_evidence(
            &mut context,
            SessionContextEvidenceScope {
                blobs_dir: &self.ctx.blobs_dir,
                limits: self.ctx.required_host_io_limits()?,
                run: &self.config.run,
                provider: &self.config.provider_interface,
                context_output_labels: &self.context_output_labels,
                session_snapshot_labels: &self.session_snapshot_labels,
            },
            &self.session,
        )?;
        if let Some(head) = snapshot_head.as_mut() {
            if context
                .artifacts
                .get(head.covered_artifact_count)
                .map(|entry| &entry.source_artifact)
                != Some(&head.artifact)
            {
                return Err(format!(
                    "provider recovered context snapshot '{}' expected at {}, found at {:?} among {} artifacts",
                    head.artifact.id, head.covered_artifact_count,
                    context.artifacts.iter().position(|entry| entry.source_artifact == head.artifact),
                    context.artifacts.len(),
                ));
            }
            head.covered_fingerprint =
                session_snapshot_prefix_fingerprint(&context.artifacts[..head.covered_artifact_count])?;
        }
        state.context_artifacts = self.published_context_artifacts(record, &self.config)?;
        if self.source_carries_context {
            // A prior publication predates this invocation's admitted read bindings.
            // Keep those static sources available to tools without inserting their
            // payloads into the recovered conversation snapshot.
            for artifact in &state.static_context_artifacts {
                append_artifact(&mut state.context_artifacts, artifact.clone());
            }
        }
        state.session_context = context;
        state.session_snapshot_head = snapshot_head;
        Ok(())
    }
}
