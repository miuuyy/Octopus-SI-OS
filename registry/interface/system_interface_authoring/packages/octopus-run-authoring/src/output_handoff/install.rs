use super::*;

impl ProviderOutputHandoff {
    pub(crate) fn authored_run_preimage(
        &self,
        current: &[u8],
        authored: &AuthoredRun,
    ) -> Result<Vec<u8>, String> {
        let hash = ContentHash::from_hex(&authored.before_hash)
            .ok_or("authored run has no exact preimage identity")?;
        let before = if ContentHash::of_bytes(current) == hash {
            current.to_vec()
        } else {
            pinned_run_record(&self.ctx, &hash, "retained authoring preimage")?.0
        };
        if apply_run_json_patch(&before, &authored.patch, &authored.after_hash, authored.json_format)?
            != authored.content
        {
            return Err("authored run carries a different exact attempt from its preimage and patch".into());
        }
        Ok(before)
    }

    pub(crate) async fn install_authored_run(
        &self,
        state: &mut HandoffState,
        authored: &AuthoredRun,
    ) -> Result<AuthoredRunInstallOutcome, String> {
        let before = RunRecord::from_bytes(&state.run_record_bytes).map_err(|error| error.to_string())?;
        let after = RunRecord::from_bytes(authored.content.as_bytes()).map_err(|error| error.to_string())?;
        let retained_publication = persisted_pending_handoff(&before, &self.config.provider_interface)?
            .filter(|prior| context_publication::is_publication_completion(prior, &authored.pending_handoff))
            .map(|prior| self.pending_handoff_evidence(&prior).map(|evidence| evidence.iter().all(Option::is_some)))
            .transpose()?.unwrap_or(false);
        let preimage = self.authored_run_preimage(&state.run_record_bytes, authored)?;
        let authored_before = RunRecord::from_bytes(&preimage)
            .map_err(|error| error.to_string())?;
        rejected_final::validate_saved_attempt(&self.ctx, &preimage, authored.content.as_bytes(), &self.config)?;
        if !retained_publication && !result_inputs::saved_definition_transition(&authored_before, &after, &self.config)? {
            self.require_settlement_output_pin(&authored.pending_handoff)?;
            source_withdrawal::require_validated_output(state, &authored.pending_handoff)?;
        }
        if self.config.writer_run.is_some() { return self.install_with_writer_run(state, authored).await; }
        let mut planned = authored.clone();
        let current_record = RunRecord::from_bytes(&state.run_record_bytes).map_err(|error| {
            format!(
                "provider run '{}' current authoring record is malformed: {error}",
                self.config.run.as_str()
            )
        })?;
        let mut call = match self
            .recover_authoring_attempt(&current_record, &state.run_record_bytes)?
        {
            AuthoringAttemptRecovery::InFlight(existing) => {
                if existing.authored != *authored {
                    return Err(format!(
                        "provider run '{}' authoring checkpoint already carries a different exact attempt",
                        self.config.run.as_str()
                    ));
                }
                if self.authoring_success_evidence(authored, None)?.is_some() {
                    if result_inputs::enabled(&self.config) || self
                        .config
                        .checkpoints
                        .iter()
                        .any(|cp| cp.owner_authoring.is_some())
                    {
                        state.run_record_evidence_seq =
                            Some(self.authoring_success_evidence(authored, None)?.ok_or(
                                "selected provider lost its actual admitted authoring result",
                            )?);
                    }
                    state.run_record_bytes = authored.content.as_bytes().to_vec();
                    return Ok(AuthoredRunInstallOutcome::Installed);
                }
                InterfaceArtifactFile::new(existing.manifest_path).map_err(|error| {
                    format!(
                        "provider run '{}' persisted authoring call is invalid: {error}",
                        self.config.run.as_str()
                    )
                })?
            }
            AuthoringAttemptRecovery::None => self.materialize_file_write_call(authored)?,
            AuthoringAttemptRecovery::OtherSource(owner) => return Err(format!(
                "provider authoring private attempt belongs to source movement {owner}, current movement is {}",
                self.source_movement_seq,
            )),
        };
        loop {
            let authored = &planned;
            let outcome = if let Some(rejected) =
                authoring_retry::rejected_write(&self.ctx, &self.config, authored)?
            {
                AuthoringWriteOutcome::Rejected(rejected)
            } else {
                let manifest_bytes =
                    tokio::fs::read(call.manifest_path())
                        .await
                        .map_err(|error| {
                            format!(
                                "interface authoring manifest '{}' cannot be read: {error}",
                                call.manifest_path().display()
                            )
                        })?;
                let admitted_seq = match self.send_authoring_manifest(&manifest_bytes).await? {
                    DeliveryVerdict::Admitted { seq } => Some(seq),
                    DeliveryVerdict::Refused { reason, .. }
                        if reason == octopus_interfaces::RefusedReason::CheckpointSpent
                            || reason == octopus_interfaces::RefusedReason::VerificationFailed =>
                    {
                        None
                    }
                    DeliveryVerdict::Refused { reason, detail } => {
                        return Err(format!(
                            "provider run authoring was refused as '{}': {}",
                            reason.as_str(),
                            detail
                        ));
                    }
                };
                self.wait_for_write_result(
                    admitted_seq,
                    &state.run_record_bytes,
                    authored,
                )
                .await?
            };
            match outcome {
                AuthoringWriteOutcome::Written => {
                    if result_inputs::enabled(&self.config) || self
                        .config
                        .checkpoints
                        .iter()
                        .any(|cp| cp.owner_authoring.is_some())
                    {
                        state.run_record_evidence_seq =
                            Some(self.authoring_success_evidence(authored, None)?.ok_or(
                                "selected provider lost its actual admitted authoring result",
                            )?);
                    }
                    state.run_record_bytes = authored.content.as_bytes().to_vec();
                    return Ok(AuthoredRunInstallOutcome::Installed);
                }
                AuthoringWriteOutcome::Rejected(rejected) => {
                    let before = if ContentHash::of_bytes(&state.run_record_bytes).to_hex()
                        == authored.before_hash
                    {
                        state.run_record_bytes.clone()
                    } else {
                        let hash = ContentHash::from_hex(&authored.before_hash)
                            .ok_or("rejected authoring preimage is malformed")?;
                        pinned_run_record(&self.ctx, &hash, "rejected authoring")?.0
                    };
                    let (current, chain) = await_checked_evidence(&self.ctx, || {
                        checked_live_run_advance(
                            &self.ctx,
                            &self.run_path,
                            &before,
                            authored.pending_handoff.source_movement_seq,
                            &self.config,
                        )
                    })
                    .await?;
                    if chain.is_empty()
                        || chain.iter().any(|transition| {
                            !transition.foreign && transition.writer.owner == self.config.provider_interface && !input_revision::checked_revision(transition)
                        })
                    {
                        return Err(format!(
                            "provider file.write rejected at {} without a proven independent advance: {}",
                            rejected.result_seq,
                            rejected.description()
                        ));
                    }
                    planned = authoring_retry::rebase_authored_run(
                        &before,
                        &current,
                        authored,
                        &self.config,
                        Some(JsonFormat::Compact),
                    )?;
                    state.run_record_evidence_seq = Some(chain.last().unwrap().call.seq.0);
                    state.run_record_bytes = current;
                    call = self.materialize_file_write_call(&planned)?;
                }
                AuthoringWriteOutcome::TerminalFailure {
                    failure_seq,
                    failure,
                } => {
                    let record =
                        RunRecord::from_bytes(&state.run_record_bytes).map_err(|error| {
                            format!(
                        "provider run '{}' current authoring-failure record is malformed: {error}",
                        self.config.run.as_str()
                    )
                        })?;
                    let outcome = hand_authoring_terminal_failure(
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
                    return Ok(AuthoredRunInstallOutcome::TerminalFailure(outcome));
                }
            }
        }
    }

    pub(crate) fn pending_handoff_evidence(
        &self,
        pending: &PendingHandoff,
    ) -> Result<Vec<Option<u64>>, String> {
        let mut evidence = Vec::with_capacity(pending.items.len());
        let mut missing = false;
        for item in &pending.items {
            let (_, expected_ref) = load_pending_artifact(&self.var_dir, item)?;
            let mut admitted =
                self.session
                    .index()?
                    .admitted_handoff_seq(PendingHandoffLedgerCoordinate {
                        interface: &self.ctx.interface,
                        run: &self.config.run,
                        worker: &self.config.worker,
                        artifact: &expected_ref,
                        required_evidence: pending_required_evidence(item)?,
                        checkpoint: &item.checkpoint.id,
                        epoch: item.checkpoint.epoch,
                    })?;
            if admitted.is_some() && !self.pending_delivery_complete(pending, item)? {
                admitted = None;
            }
            if admitted.is_none() {
                missing = true;
            } else if missing {
                return Err(format!(
                    "provider pending handoff for movement {} has out-of-order admitted evidence at artifact '{}'",
                    pending.source_movement_seq,
                    item.artifact_id.as_str()
                ));
            }
            evidence.push(admitted);
        }
        Ok(evidence)
    }

    pub(crate) async fn hand_pending_items(
        &self,
        pending: &PendingHandoff,
        installed_record: &[u8],
    ) -> Result<Vec<ProviderHandoffOutcome>, String> {
        let _ownership = self.dispatch_ownership().await?;
        self.require_settlement_output_pin(pending)?;
        self.validate_dispatch_frontier(pending, installed_record)?;
        let evidence = self.pending_handoff_evidence(pending)?;
        let mut outcomes = Vec::with_capacity(pending.items.len());
        for (index, item) in pending.items.iter().enumerate() {
            let (artifact, expected_ref) = load_pending_artifact(&self.var_dir, item)?;
            if let Some(seq) = evidence[index] {
                outcomes.push(ProviderHandoffOutcome::Admitted {
                    seq,
                    artifact: expected_ref,
                });
                continue;
            }
            let verdict = self.hand_retained_pending(pending, item, artifact).await?;
            let admitted =
                self.session
                    .index()?
                    .admitted_handoff_seq(PendingHandoffLedgerCoordinate {
                        interface: &self.ctx.interface,
                        run: &self.config.run,
                        worker: &self.config.worker,
                        artifact: &expected_ref,
                        required_evidence: pending_required_evidence(item)?,
                        checkpoint: &item.checkpoint.id,
                        epoch: item.checkpoint.epoch,
                    })?;
            match (verdict, admitted) {
                (DeliveryVerdict::Admitted { .. }, Some(seq)) => {
                    outcomes.push(ProviderHandoffOutcome::Admitted {
                        seq,
                        artifact: expected_ref,
                    });
                }
                (DeliveryVerdict::Admitted { .. }, None) => {
                    return Err(format!(
                        "provider pending handoff artifact '{}' was admitted without exact checkpoint evidence",
                        item.artifact_id.as_str()
                    ));
                }
                (DeliveryVerdict::Refused { .. }, Some(seq)) => {
                    outcomes.push(ProviderHandoffOutcome::Admitted {
                        seq,
                        artifact: expected_ref,
                    });
                }
                (DeliveryVerdict::Refused { reason, detail }, None) => {
                    return Err(format!(
                        "provider pending handoff artifact '{}' for movement {} was refused as '{}': {}",
                        item.artifact_id.as_str(),
                        self.source_movement_seq,
                        reason.as_str(),
                        detail
                    ));
                }
            }
        }
        Ok(outcomes)
    }

    pub(crate) fn ground_admitted_context_outputs(
        session_context: &mut ResolvedSessionContext,
        pending: &PendingHandoff,
        outcomes: &[ProviderHandoffOutcome],
        context_output_labels: &BTreeSet<String>,
    ) -> Result<(), String> {
        if pending.items.len() != outcomes.len() {
            return Err(
                "provider handoff outcomes do not match the pending artifact batch".to_string(),
            );
        }
        for (item, outcome) in pending.items.iter().zip(outcomes) {
            if !context_output_labels.contains(&item.artifact_label) {
                continue;
            }
            let seq = match outcome {
                ProviderHandoffOutcome::Admitted { seq, .. } => *seq,
                ProviderHandoffOutcome::TerminalFailure { .. } => {
                    return Err(format!(
                        "provider context output '{}' ended in an internal terminal failure",
                        item.artifact_id.as_str()
                    ));
                }
            };
            let content = ContentHash::from_hex(&item.artifact_content_hash).ok_or_else(|| {
                format!(
                    "provider context output '{}' has an invalid pending content hash",
                    item.artifact_id.as_str()
                )
            })?;
            let source = ArtifactRef {
                id: item.artifact_id.clone(),
                content,
            };
            let mut matched = None;
            for (index, artifact) in session_context.artifacts.iter().enumerate() {
                if artifact.source_artifact != source {
                    continue;
                }
                if matched.replace(index).is_some() {
                    return Err(format!(
                        "provider session context repeats handed output '{}'",
                        source.id.as_str()
                    ));
                }
            }
            let index = matched.ok_or_else(|| {
                format!(
                    "provider session context lost handed output '{}'",
                    source.id.as_str()
                )
            })?;
            let artifact = &mut session_context.artifacts[index];
            if artifact
                .source_movement_seq
                .is_some_and(|current| current != seq)
            {
                return Err(format!(
                    "provider session context output '{}' already names a different movement",
                    source.id.as_str()
                ));
            }
            artifact.source_movement_seq = Some(seq);
        }
        Ok(())
    }
}
