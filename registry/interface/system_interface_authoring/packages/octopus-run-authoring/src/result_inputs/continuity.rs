use super::*;

fn content_ref(id: &ArtifactId) -> Option<ArtifactRef> {
    let (label, hex) = id.as_str().rsplit_once(':')?;
    let content = ContentHash::from_hex(hex)?;
    (!label.trim().is_empty() && content.to_hex() == hex).then(|| ArtifactRef { id: id.clone(), content })
}

fn instruction_ids(
    record: &RunRecord,
    config: &RunAuthoringConfig,
    template: &RunAuthoringCheckpoint,
    context: &[ArtifactBinding],
    call: &ArtifactRef,
) -> Result<BTreeSet<ArtifactId>, String> {
    let mut ids = template.static_artifact_bindings.iter().cloned().collect::<BTreeSet<_>>();
    ids.extend(result_static_bindings(config, Some(context), call));
    ids.extend(current_mandatory_role_artifacts(record, config)?);
    ids.extend(config.payload_artifact_bindings.iter().map(|reference| reference.id.clone()));
    ids.remove(&call.id);
    Ok(ids)
}

pub(crate) fn result_rebase(
    record: &RunRecord,
    config: &RunAuthoringConfig,
    template: &RunAuthoringCheckpoint,
    row: &octopus_run_record::CheckpointRecord,
) -> Result<Vec<ArtifactRef>, String> {
    let call = correspondence(template)?.call_artifact;
    let evidence = template.static_artifact_bindings.contains(&call.id);
    let context = effective_context_bindings(record, &config.checkpoints, &config.provider_interface)?;
    let instructions = instruction_ids(record, config, template, &context, &call)?;
    let admitted = row
        .artifact_bindings
        .iter()
        .filter(|binding| !(evidence && binding.id == call.id) && !instructions.contains(&binding.id))
        .map(|binding| &binding.id)
        .collect::<Vec<_>>();
    let mut position = 0;
    let mut rebase = Vec::new();
    for binding in context.iter().filter(|binding| !instructions.contains(&binding.id)) {
        if admitted.get(position) == Some(&&binding.id) {
            position += 1;
            continue;
        }
        if admitted.contains(&&binding.id) {
            return Err(format!("result for call '{}' finds its admitted context reordered", call.id.as_str()));
        }
        rebase.push(content_ref(&binding.id).ok_or_else(|| {
            format!("context '{}' added while call '{}' waited has no content address", binding.id.as_str(), call.id.as_str())
        })?);
    }
    if let Some(lost) = admitted.get(position) {
        if position == 0 && !rebase.is_empty() {
            return Ok(rebase);
        }
        return Err(format!(
            "context '{}' left the run while call '{}' waited for its result",
            lost.as_str(),
            call.id.as_str()
        ));
    }
    Ok(rebase)
}

fn retired<'a>(
    before: &RunRecord,
    after: &RunRecord,
    old: &'a RunAuthoringConfig,
    next: &RunAuthoringConfig,
) -> Vec<&'a RunAuthoringCheckpoint> {
    old.checkpoints
        .iter()
        .filter(|template| {
            template.role == RunAuthoringCheckpointRole::ResultInput
                && (!next.checkpoints.contains(template)
                    || active_rows(before, template) != active_rows(after, template))
        })
        .collect()
}

pub(crate) fn expected_rebase(
    before: &RunRecord,
    after: &RunRecord,
    config: &RunAuthoringConfig,
) -> Result<Vec<ArtifactRef>, String> {
    let provider = &config.provider_interface;
    let Some(next_pending) = persisted_pending_handoff(after, provider)? else {
        return Ok(Vec::new());
    };
    if next_pending.source_without_context {
        return Ok(Vec::new());
    }
    let prior = persisted_pending_handoff(before, provider)?;
    let authored = before.interface_state.get(provider).is_some_and(|state| state.contains_key(RUN_AUTHORING_BINDING));
    if authored && next_pending.withdrawal.is_none() {
        let old = attempt_config(before, config)?;
        let next = attempt_config(after, config)?;
        if let [template] = retired(before, after, &old, &next).as_slice() {
            let rows = active_rows(before, template);
            let [row] = rows.as_slice() else {
                return Err("consumed result input has no unique receiving row".into());
            };
            return result_rebase(before, &old, template, row);
        }
    }
    Ok(prior
        .filter(|prior| prior.source_movement_seq == next_pending.source_movement_seq)
        .map(|prior| prior.rebase)
        .unwrap_or_default())
}

pub(crate) fn validate_continuity(
    before: &RunRecord,
    after: &RunRecord,
    config: &RunAuthoringConfig,
) -> Result<(), String> {
    let Some(next_pending) = persisted_pending_handoff(after, &config.provider_interface)? else {
        return Ok(());
    };
    if next_pending.rebase != expected_rebase(before, after, config)? {
        return Err("provider write changed the recorded rebase of its consumed result".into());
    }
    Ok(())
}

pub(crate) fn seal_pending(
    before: &RunRecord,
    value: &mut serde_json::Value,
    config: &RunAuthoringConfig,
) -> Result<PendingHandoff, String> {
    let provider = config.provider_interface.as_str();
    let draft = RunRecord::from_bytes(&serde_json::to_vec(value).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())?;
    let mut pending = persisted_pending_handoff(&draft, &config.provider_interface)?
        .ok_or("sealed provider write has no pending handoff")?;
    pending.rebase = expected_rebase(before, &draft, config)?;
    value["interface_state"][provider]["pending_handoff"] =
        serde_json::to_value(&pending).map_err(|error| error.to_string())?;
    Ok(pending)
}

pub(crate) fn validate_structure(pending: &PendingHandoff) -> Result<(), String> {
    if pending.rebase.iter().map(|reference| &reference.id).collect::<BTreeSet<_>>().len() != pending.rebase.len()
        || (!pending.rebase.is_empty() && pending.source_without_context)
    {
        return Err("provider pending handoff carries a malformed result rebase".into());
    }
    Ok(())
}

impl ProviderOutputHandoff {
    pub(crate) fn result_input_template(&self) -> Result<Option<RunAuthoringCheckpoint>, String> {
        if self.failed_source || !enabled(&self.config) {
            return Ok(None);
        }
        let Some(runtime) = self.invocation_metadata.runtime.as_ref() else {
            return Ok(None);
        };
        let hash = runtime
            .run_record_hash
            .as_deref()
            .and_then(ContentHash::from_hex)
            .ok_or("result source has no pinned invocation snapshot")?;
        let (_, record) = pinned_run_record(&self.ctx, &hash, "result input")?;
        let config = attempt_config(&record, &self.config)?;
        let checkpoint = record
            .checkpoint(&runtime.checkpoint)
            .ok_or("result source checkpoint is absent")?;
        let templates = config
            .checkpoints
            .iter()
            .filter(|template| {
                template.role == RunAuthoringCheckpointRole::ResultInput
                    && checkpoint_record_matches_template(checkpoint, template, &record.id)
            })
            .cloned()
            .collect::<Vec<_>>();
        match templates.as_slice() {
            [] => Ok(None),
            [template] => Ok(Some(template.clone())),
            _ => Err("result source has multiple receiving templates".into()),
        }
    }

    pub(crate) fn require_carried_admitted_context(&self, state: &HandoffState, rebase: &[ArtifactRef]) -> Result<(), String> {
        let Some(first) = rebase.first() else { return Ok(()); };
        if state.session_context.artifacts.iter().any(|entry| &entry.source_artifact == first) {
            return Ok(());
        }
        let (manifest, _) = read_pinned_artifact_from_cache(&self.ctx.blobs_dir, self.ctx.required_host_io_limits()?, first)?;
        if self.session_snapshot_labels.contains(&manifest.label) {
            return Ok(());
        }
        let mut next = state.clone();
        take_unpublished_context_inputs(&mut next, self.source_movement_seq)?;
        if !next.session_context.artifacts.is_empty()
            && !next.session_context.artifacts.iter().any(|entry| next.context_artifacts.contains(&entry.source_artifact.id))
        {
            return Err("the admitted context left the run while its call waited and no session snapshot replaced it".into());
        }
        Ok(())
    }

    pub(crate) fn apply_result_rebase(
        &self,
        state: &mut HandoffState,
        rebase: &[ArtifactRef],
    ) -> Result<(), String> {
        if rebase.is_empty() {
            state.result_rebase = Some(Vec::new());
            return Ok(());
        }
        let mut next = state.clone();
        let inputs = take_unpublished_context_inputs(&mut next, self.source_movement_seq)?;
        let provenance = self.session.index()?.bound_provenance(
            &self.config.run,
            &self.config.provider_interface,
            rebase.iter(),
        )?;
        let limits = self.ctx.required_host_io_limits()?;
        for (index, reference) in rebase.iter().enumerate() {
            if next
                .session_context
                .artifacts
                .iter()
                .any(|entry| &entry.source_artifact == reference)
            {
                continue;
            }
            let (manifest, payload) = read_pinned_artifact_from_cache(&self.ctx.blobs_dir, limits, reference)?;
            if self.session_snapshot_labels.contains(&manifest.label) {
                if index != 0 {
                    return Err("a result rebase carries a session snapshot only as the context it replaces".into());
                }
                replace_admitted_session_context(
                    &self.ctx.blobs_dir,
                    limits,
                    &mut next.static_context_artifacts,
                    &mut next.session_context,
                    &mut next.session_snapshot_head,
                    reference,
                    &payload,
                    provenance
                        .get(&artifact_ref_key(reference))
                        .cloned()
                        .unwrap_or_default(),
                    &self.session_snapshot_labels,
                    &self.config.provider_interface,
                )?;
                continue;
            }
            collect_bound_context(
                &self.ctx,
                &mut next.static_context_artifacts,
                &mut next.session_context,
                &mut next.session_snapshot_head,
                reference,
                &payload,
                provenance
                    .get(&artifact_ref_key(reference))
                    .cloned()
                    .unwrap_or_default(),
                &self.session_snapshot_labels,
                &self.config.provider_interface,
            )?;
        }
        ground_session_context_movements(
            &mut next.session_context,
            &self.session,
            &self.config.run,
            &self.config,
        )?;
        validate_session_context_evidence(
            &mut next.session_context,
            SessionContextEvidenceScope {
                blobs_dir: &self.ctx.blobs_dir,
                limits,
                run: &self.config.run,
                provider: &self.config.provider_interface,
                context_output_labels: &self.context_output_labels,
                session_snapshot_labels: &self.session_snapshot_labels,
            },
            &self.session,
        )?;
        restore_consumed_context_inputs(&mut next, inputs)?;
        next.result_rebase = Some(rebase.to_vec());
        *state = next;
        Ok(())
    }

    pub(crate) async fn rebase_result_input(&self, state: &mut HandoffState) -> Result<bool, String> {
        let Some(original) = self.result_input_template()? else {
            return Ok(false);
        };
        self.reconcile_checked_run_state(state).await?;
        let mut current =
            RunRecord::from_bytes(&state.run_record_bytes).map_err(|error| error.to_string())?;
        let runtime = self
            .invocation_metadata
            .runtime
            .as_ref()
            .ok_or("result source coordinate absent")?;
        let Some(checkpoint) = current
            .checkpoint(&runtime.checkpoint)
            .filter(|_| current.active_checkpoints.contains(&runtime.checkpoint))
        else {
            return Ok(false);
        };
        if !checkpoint_record_matches_template(checkpoint, &original, &current.id) {
            return Err("result no longer has its admitted receiving contract".into());
        }
        let awaited = correspondence(&original)?;
        if persisted_pending_handoff(&current, &self.config.provider_interface)?.is_some_and(|pending| {
            pending.deferred_publication
                && pending.items.iter().any(|item| item.artifact_id == awaited.call_artifact.id)
        }) {
            return Ok(false);
        }
        let mut active = self.clone();
        active.config = attempt_config(&current, &self.config)?;
        active.bound_topology.checkpoints = active.config.checkpoints.clone();
        match active.recover_authoring_attempt(&current, &state.run_record_bytes)? {
            AuthoringAttemptRecovery::InFlight(_) => return Ok(false),
            AuthoringAttemptRecovery::OtherSource(owner) => {
                let pending = persisted_pending_handoff(&current, &active.config.provider_interface)?
                    .ok_or("result found an unrelated private authoring attempt")?;
                let mut publisher = active.clone();
                publisher.source_movement_seq = owner;
                let AuthoringAttemptRecovery::InFlight(attempt) =
                    publisher.recover_authoring_attempt(&current, &state.run_record_bytes)?
                else {
                    return Err("result cannot attribute the prior authoring attempt".into());
                };
                if !context_publication::is_publication_completion(&pending, &attempt.authored.pending_handoff)
                    || publisher.pending_handoff_evidence(&pending)?.iter().any(Option::is_none)
                {
                    return Err("result awaits the prior source's checked authoring completion".into());
                }
                if !matches!(
                    publisher.install_authored_run(state, &attempt.authored).await?,
                    AuthoredRunInstallOutcome::Installed
                ) {
                    return Err("prior context publication failed before the result".into());
                }
                current = RunRecord::from_bytes(&state.run_record_bytes).map_err(|error| error.to_string())?;
                active.config = attempt_config(&current, &active.config)?;
            }
            AuthoringAttemptRecovery::None => {
                if let Some(pending) = persisted_pending_handoff(&current, &active.config.provider_interface)? {
                    if active.pending_handoff_evidence(&pending)?.iter().any(Option::is_none) {
                        source_withdrawal::require_validated_output(state, &pending)?;
                        let mut publisher = active.clone();
                        publisher.source_movement_seq = pending.source_movement_seq;
                        publisher.hand_pending_items(&pending, &state.run_record_bytes).await?;
                    }
                    if pending.context_publication.is_some() {
                        let mut publisher = active.clone();
                        publisher.source_movement_seq = pending.source_movement_seq;
                        publisher.publish_admitted_context(state).await?;
                        current = RunRecord::from_bytes(&state.run_record_bytes).map_err(|error| error.to_string())?;
                        active.config = attempt_config(&current, &active.config)?;
                    }
                }
            }
        }
        if active.recover_authoring_failure(&current, &state.run_record_bytes)?.is_some() {
            return Ok(false);
        }
        let checkpoint = current
            .checkpoint(&runtime.checkpoint)
            .ok_or("result lost its receiving row")?
            .clone();
        let rebase = result_rebase(&current, &active.config, &original, &checkpoint)?;
        self.require_carried_admitted_context(state, &rebase)?;
        self.apply_result_rebase(state, &rebase)?;
        Ok(true)
    }
}
