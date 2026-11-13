use super::*;

impl ProviderOutputHandoff {
    pub(crate) fn recover_authoring_attempt(
        &self,
        record: &RunRecord,
        record_bytes: &[u8],
    ) -> Result<AuthoringAttemptRecovery, String> {
        let recover_predecessors = self.config.checkpoints.iter().any(|checkpoint| {
            checkpoint.role == RunAuthoringCheckpointRole::OwnerInput
        }) && !self.settled_input_definition(record, record_bytes)?;
        if self.config.writer_run.is_some() { return self.recover_writer_attempt(record, record_bytes, recover_predecessors); }
        let attempt = persisted_authoring_attempt(
            &self.ctx,
            &self.hand_in_root,
            record,
            record_bytes,
            &self.config,
            recover_predecessors,
        )?;
        Ok(match attempt {
            None => AuthoringAttemptRecovery::None,
            Some(attempt) if attempt.provider_source_movement_seq == self.source_movement_seq => {
                AuthoringAttemptRecovery::InFlight(attempt)
            }
            Some(attempt) => AuthoringAttemptRecovery::OtherSource(attempt.provider_source_movement_seq),
        })
    }

    pub(crate) fn recover_authoring_failure(
        &self,
        record: &RunRecord,
        record_bytes: &[u8],
    ) -> Result<Option<(u64, InterfaceFailureEnvelope, Vec<ArtifactRef>)>, String> {
        if self.config.writer_run.is_some() { return Ok(None); }
        let template = self
            .config
            .checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringResult)?;
        let checkpoint = active_checkpoint_for_role(
            record,
            &self.config,
            RunAuthoringCheckpointRole::AuthoringResult,
        )?;
        let coordinate = RuntimeCoordinateKey::new(
            &record.id,
            &record.worker,
            &self.config.provider_interface,
            &checkpoint.id,
            checkpoint.epoch.0,
        );
        let mut failed = None;
        for evidence in self.session.index()?.outbound_artifacts(
            &coordinate,
            template.interface(),
            Some(self.source_movement_seq),
        )? {
            let (manifest, payload) = read_pinned_artifact(&self.ctx, &evidence.artifact)?;
            if manifest.label == self.config.file_write_result_label {
                let result: ToolResultArtifact =
                    serde_json::from_slice(&payload).map_err(|error| error.to_string())?;
                if result.label != manifest.label || result.tool != self.config.file_write_tool {
                    return Err(
                        "provider authoring result changed its declared tool or label".into(),
                    );
                }
                continue;
            }
            if failed.replace((evidence, manifest, payload)).is_some() {
                return Err(
                    "provider authoring checkpoint has repeated interface failure evidence".into(),
                );
            }
        }
        let Some((evidence, manifest, payload)) = failed else {
            return Ok(None);
        };
        let record_hash = ContentHash::of_bytes(record_bytes).to_hex();
        if evidence.run_record_hash.as_deref() != Some(&record_hash) {
            return Err(
                "provider recovery authoring-result evidence differs from its current checkpoint"
                    .into(),
            );
        }
        if !indexed_evidence_matches_checkpoint(&evidence, template)
            || !artifact_refs_match_bindings(
                &evidence.artifact_binding_refs,
                &checkpoint.artifact_bindings,
            )
        {
            return Err(
                "provider recovery authoring-result evidence differs from its current checkpoint"
                    .to_string(),
            );
        }
        let failure_label = format!("{}.failure", template.interface().as_str());
        if manifest.label != failure_label {
            return Err(format!(
                "provider recovery authoring-result checkpoint delivered unexpected artifact '{}'",
                manifest.label
            ));
        }
        let failure: InterfaceFailureEnvelope =
            serde_json::from_slice(&payload).map_err(|error| {
                format!("provider recovery authoring failure payload is malformed: {error}")
            })?;
        if failure.failure_class.trim().is_empty()
            || failure.failure_class.trim() != failure.failure_class
            || failure.source_movement_seq >= evidence.seq
        {
            return Err(
                "provider recovery authoring failure has invalid class or source movement coordinate"
                    .to_string(),
            );
        }
        if completed_authoring_failure_duplicate(
            &self.ctx,
            &self.session,
            record,
            record_bytes,
            &self.config,
            &failure,
            evidence.seq,
        )? {
            return Ok(None);
        }
        let validated = validate_authoring_failure_source(AuthoringFailureSourceValidation {
            ctx: &self.ctx,
            record,
            record_bytes,
            config: &self.config,
            failure: &failure,
            failure_seq: evidence.seq,
            after_seq: Some(self.source_movement_seq),
        })?;
        Ok(Some((evidence.seq, failure, validated.rebase)))
    }

    pub(crate) fn authoring_success_evidence(
        &self,
        authored: &AuthoredRun,
        after_seq: Option<u64>,
    ) -> Result<Option<u64>, String> {
        authoring_success_evidence(&self.ctx, &self.session, &self.config, authored, after_seq)
    }

    pub async fn recover_source(&self) -> Result<ProviderSourceRecovery, String> {
        self.recover_source_with_outcomes()
            .await
            .map(|(recovery, _)| recovery)
    }

    pub fn recover_source_with_outcomes(
        &self,
    ) -> std::pin::Pin<Box<impl std::future::Future<Output = Result<(ProviderSourceRecovery, Vec<ProviderHandoffOutcome>), String>> + '_>> {
        Box::pin(self.recover_source_with_outcomes_inner())
    }

    pub(crate) async fn recover_source_with_outcomes_inner(
        &self,
    ) -> Result<(ProviderSourceRecovery, Vec<ProviderHandoffOutcome>), String> {
        if let HandoffDeliveryMode::OneShot(plan) = &self.delivery_mode {
            let _state = self.state.lock().await;
            return self
                .recover_one_shot(plan)
                .await
                .map(|recovery| (recovery, Vec::new()));
        }
        let mut state = self.state.lock().await;
        loop {
            let retained = (*state).clone();
            match self.recover_source_attempt(&mut state).await {
                Ok(recovered) => return Ok(recovered),
                Err(error) => self.adopt_live_record(&mut state, retained, error).await?,
            }
        }
    }

    async fn recover_source_attempt(
        &self,
        state: &mut HandoffState,
    ) -> Result<(ProviderSourceRecovery, Vec<ProviderHandoffOutcome>), String> {
        if self.rebase_result_input(state).await? {
            return Ok((ProviderSourceRecovery::Process, Vec::new()));
        }
        let mut record = RunRecord::from_bytes(&state.run_record_bytes).map_err(|error| {
            format!(
                "provider run '{}' current recovery record is malformed: {error}",
                self.config.run.as_str()
            )
        })?;
        if let Some((failure_seq, failure, rebase)) =
            self.recover_authoring_failure(&record, &state.run_record_bytes)?
        {
            if self.checked_settlement_output.is_some() {
                return Err("checked settlement recovery cannot replace its closure with an authoring failure".into());
            }
            self.apply_result_rebase(state, &rebase)?;
            hand_authoring_terminal_failure(
                &self.ctx,
                &self.session,
                &record,
                &self.config,
                failure_seq,
                &failure,
                &state.session_context,
                state.session_snapshot_head.as_ref(),
                &self.session_snapshot_labels,
            )
            .await?;
            return Ok((ProviderSourceRecovery::Completed, Vec::new()));
        }
        let attempt = self.recover_authoring_attempt(&record, &state.run_record_bytes)?;
        if let AuthoringAttemptRecovery::OtherSource(owner) = attempt {
            if let Some(pending) = persisted_pending_handoff(&record, &self.config.provider_interface)?
                .filter(|pending| owner > self.source_movement_seq && !pending.publication_unfinished()) {
                let completed = if pending.source_movement_seq == self.source_movement_seq && pending.terminal {
                    validate_pending_items_are_active(&record, &self.config.provider_interface, &pending)?;
                    Some((ProviderSourceRecovery::Completed, self.admitted_replay_handoff(&pending)?))
                } else if pending.source_movement_seq > self.source_movement_seq {
                    let recovered = self.recover_overtaken_source(&state.run_record_bytes, &pending)?;
                    if recovered.0 == ProviderSourceRecovery::Completed {
                        self.restore_checked_context(state, &record, &recovered.2)?;
                        Some((recovered.0, recovered.1))
                    } else { None }
                } else {
                    None
                };
                if let Some(completed) = completed {
                    self.restore_pending_context(state, &record, &pending)?;
                    state.consumed_input = false;
                    state.consumed_context_free_independent_input = None;
                    state.consumed_verified_input = false;
                    return Ok(completed);
                }
            }
            return Err(format!(
                "provider authoring private attempt belongs to source movement {owner}, current movement is {}",
                self.source_movement_seq,
            ));
        }
        if let AuthoringAttemptRecovery::InFlight(attempt) = attempt {
            let withdrawal_guard = if attempt.authored.pending_handoff.withdrawal.is_some() { Some(self.dispatch_ownership().await?) } else { None };
            if let Some(prior) =
                persisted_pending_handoff(&record, &self.config.provider_interface)?
            {
                if prior.source_movement_seq > self.source_movement_seq {
                    self.recover_overtaken_source(&state.run_record_bytes, &prior)?;
                }
                if (prior.source_movement_seq < self.source_movement_seq && !prior.terminal)
                    || (prior.source_movement_seq == self.source_movement_seq && prior.terminal
                        && !context_publication::is_publication_completion(&prior, &attempt.authored.pending_handoff)
                        && !source_settlement::follows_completed_source(&prior, &attempt.authored.pending_handoff)
                        && !source_continuation::follows(&prior, &attempt.authored.pending_handoff)
                        && !source_withdrawal::follows(&prior, &attempt.authored.pending_handoff))
                {
                    return Err(format!(
                        "provider authoring attempt for movement {} cannot supersede pending movement {} (terminal={})",
                        self.source_movement_seq, prior.source_movement_seq, prior.terminal
                    ));
                }
                if prior.source_movement_seq == self.source_movement_seq {
                    validate_pending_items_are_active(
                        &record,
                        &self.config.provider_interface,
                        &prior,
                    )?;
                }
                let withdrawing = source_withdrawal::follows(&prior, &attempt.authored.pending_handoff);
                let evidence = self.pending_handoff_evidence(&prior)?;
                if let Some((index, _)) = evidence
                    .iter()
                    .enumerate()
                    .find(|(_, admitted)| !withdrawing && admitted.is_none())
                {
                    return Err(format!(
                        "provider authoring attempt for movement {} cannot supersede pending artifact '{}' without admitted ledger evidence",
                        self.source_movement_seq,
                        prior.items[index].artifact_id.as_str()
                    ));
                }
                if prior.source_movement_seq == self.source_movement_seq && !withdrawing {
                    self.restore_pending_context(state, &record, &prior)?;
                }
            }
            if attempt.authored.pending_handoff.source_movement_seq == self.source_movement_seq
                && attempt.authored.pending_handoff.withdrawal.is_none()
            {
                self.apply_result_rebase(state, &attempt.authored.pending_handoff.rebase)?;
            }
            match self
                .install_authored_run(state, &attempt.authored)
                .await?
            {
                AuthoredRunInstallOutcome::Installed => {
                    record = RunRecord::from_bytes(&state.run_record_bytes).map_err(|error| {
                        format!(
                            "provider run '{}' recovered authored record is malformed: {error}",
                            self.config.run.as_str()
                        )
                    })?;
                }
                AuthoredRunInstallOutcome::TerminalFailure(_) => {
                    return Ok((ProviderSourceRecovery::Completed, Vec::new()));
                }
            }
            if withdrawal_guard.is_some() { self.validate_installed_withdrawal(&state.run_record_bytes)?; }
            drop(withdrawal_guard);
        }
        let journal = self.standing_journal_for_source(&record)?;
        let pending = persisted_pending_handoff(&record, &self.config.provider_interface)?;
        let Some(pending) = pending else {
            if journal.is_some() {
                return Err("provider standing journal has no persisted handoff boundary".into());
            }
            return Ok((ProviderSourceRecovery::Process, Vec::new()));
        };
        if pending.source_movement_seq < self.source_movement_seq {
            if !pending.terminal {
                return Err(format!(
                    "provider reader advanced to movement {} while movement {} retains a non-terminal pending handoff",
                    self.source_movement_seq, pending.source_movement_seq
                ));
            }
            let evidence = self.pending_handoff_evidence(&pending)?;
            if let Some((index, _)) = evidence
                .iter()
                .enumerate()
                .find(|(_, admitted)| admitted.is_none())
            {
                if self.unsubmitted_source_failure_before(&pending, self.source_movement_seq)? {
                    self.restore_failed_context(state, &[])?;
                    return Ok((ProviderSourceRecovery::Process, Vec::new()));
                }
                return Err(format!(
                    "provider reader advanced to movement {} while terminal pending artifact '{}' from movement {} has no admitted ledger evidence",
                    self.source_movement_seq,
                    pending.items[index].artifact_id.as_str(),
                    pending.source_movement_seq
                ));
            }
            let input_entries = take_unpublished_context_inputs(state, self.source_movement_seq)?;
            if pending.context_publication.is_some() {
                self.publish_admitted_context(state).await?;
            }
            let published = RunRecord::from_bytes(&state.run_record_bytes).map_err(|error| error.to_string())?;
            self.restore_pending_context(state, &published, &pending)?;
            restore_consumed_context_inputs(state, input_entries)?;
            if let Some(journal) = journal {
                return self.recover_standing_journal(state, journal).await;
            }
            return Ok((ProviderSourceRecovery::Process, Vec::new()));
        }
        if pending.source_movement_seq > self.source_movement_seq {
            let recovered = self.recover_overtaken_source(&state.run_record_bytes, &pending)?;
            let input_entries = take_unpublished_context_inputs(state, self.source_movement_seq)?;
            if pending.context_publication.is_some() {
                let mut publisher = self.clone();
                publisher.source_movement_seq = pending.source_movement_seq;
                publisher.publish_admitted_context(state).await?;
            }
            let published = RunRecord::from_bytes(&state.run_record_bytes).map_err(|error| error.to_string())?;
            self.restore_checked_context(state, &published, &recovered.2)?;
            self.restore_pending_context(state, &published, &pending)?;
            restore_consumed_context_inputs(state, input_entries)?;
            if recovered.0 != ProviderSourceRecovery::Process {
                state.consumed_input = false;
                state.consumed_context_free_independent_input = None;
                if recovered.0 == ProviderSourceRecovery::Completed {
                    state.consumed_verified_input = false;
                }
            }
            return Ok((recovered.0, recovered.1));
        }
        if self.pending_handoff_evidence(&pending)?.iter().any(Option::is_none) {
            source_withdrawal::require_validated_output(&state, &pending)?;
        }
        validate_pending_items_are_active(&record, &self.config.provider_interface, &pending)?;
        if pending.items.is_empty() {
            return Err(format!(
                "provider pending handoff for movement {} contains no response artifacts",
                self.source_movement_seq
            ));
        }
        let outcomes = self.hand_pending_items(&pending, &state.run_record_bytes).await?;
        self.publish_admitted_context(state).await?;
        let record = RunRecord::from_bytes(&state.run_record_bytes).map_err(|error| error.to_string())?;
        self.restore_pending_context(state, &record, &pending)?;
        state.consumed_input = false;
        state.consumed_context_free_independent_input = None;
        if pending.terminal {
            state.consumed_verified_input = false;
        }
        if pending.terminal {
            return Ok((ProviderSourceRecovery::Completed, outcomes));
        }
        Self::ground_admitted_context_outputs(
            &mut state.session_context,
            &pending,
            &outcomes,
            &self.contributed_context_output_labels(),
        )?;
        if let Some(journal) = journal {
            return self.recover_standing_journal(state, journal).await;
        }
        Ok((
            ProviderSourceRecovery::Interrupted {
                last_artifact_label: pending
                    .items
                    .last()
                    .map(|item| item.artifact_label.clone())
                    .ok_or_else(|| "provider pending handoff batch is empty".to_string())?,
            },
            outcomes,
        ))
    }
}
