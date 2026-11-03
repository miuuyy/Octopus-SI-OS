use super::*;

pub(super) fn last_output(pending: &PendingHandoff) -> Result<ArtifactRef, String> {
    let item = pending.items.last().ok_or("a source continuation has no completed output to continue")?;
    let content = ContentHash::from_hex(&item.artifact_content_hash)
        .filter(|hash| hash.to_hex() == item.artifact_content_hash)
        .ok_or("a source continuation predecessor has no canonical artifact hash")?;
    Ok(ArtifactRef { id: item.artifact_id.clone(), content })
}

pub(super) fn follows(before: &PendingHandoff, after: &PendingHandoff) -> bool {
    let Some(last) = before.items.last() else {
        return false;
    };
    before.terminal
        && after.terminal
        && before.source_movement_seq == after.source_movement_seq
        && !before.source_without_context
        && !after.source_without_context
        && !before.publication_unfinished()
        && before.settlement_of.is_none()
        && before.withdrawal.is_none()
        && before.replacement.is_none()
        && after.settlement_of.is_none()
        && after.withdrawal.is_none()
        && after.replacement.is_none()
        && after.dispatch_fenced
        && after.standing.is_empty()
        && !after.items.is_empty()
        && after.items.iter().all(|item| {
            before.items.iter().chain(&before.standing).all(|prior| prior.artifact_id != item.artifact_id)
        })
        && after.continuation_of.as_ref().is_some_and(|reference| {
            reference.id == last.artifact_id && reference.content.to_hex() == last.artifact_content_hash
        })
}

fn ordinary_output(role: RunAuthoringCheckpointRole) -> bool {
    matches!(
        role,
        RunAuthoringCheckpointRole::ProviderToolOutput
            | RunAuthoringCheckpointRole::ProviderInterfaceOutput
            | RunAuthoringCheckpointRole::ProviderArtifactOutput
    )
}

pub(super) fn continued_context(
    record: &RunRecord,
    config: &RunAuthoringConfig,
    prior: &PendingHandoff,
    context_artifacts: &[ArtifactId],
    outputs: &[PreparedHandoff],
) -> Result<Vec<ArtifactId>, String> {
    let mut context = match &prior.context_publication {
        Some(target) => target.clone(),
        None => current_context_artifacts(record, config)?,
    };
    for output in outputs.iter().filter(|output| context_artifacts.contains(&output.identity.artifact.id)) {
        append_artifact(&mut context, output.identity.artifact.id.clone());
    }
    Ok(context)
}

pub(super) fn validate_checked_transition(
    before: &RunRecord,
    after: &RunRecord,
    writer: &CheckedRunWriter,
) -> Result<(), String> {
    if persisted_pending_handoff(after, &writer.owner)?.is_none_or(|pending| pending.continuation_of.is_none()) {
        return Ok(());
    }
    let config = config_from_persisted_profile(before, &writer.owner, &RunAuthoringProfile {
        file_write_tool: writer.tool.clone(),
        file_write_label: writer.call_label.clone(),
        file_write_result_label: writer.result_label.clone(),
        result_timeout_ms: 1,
    })?;
    validate_transition(before, after, &config)
}

pub(super) fn validate_transition(
    before: &RunRecord,
    after: &RunRecord,
    config: &RunAuthoringConfig,
) -> Result<(), String> {
    let provider = &config.provider_interface;
    let Some(next) = persisted_pending_handoff(after, provider)? else {
        return Ok(());
    };
    if next.continuation_of.is_none() {
        return Ok(());
    }
    let prior = persisted_pending_handoff(before, provider)?
        .ok_or("source continuation has no completed predecessor")?;
    let prior_state = &before.interface_state[provider];
    let next_state = &after.interface_state[provider];
    let prior_generation = optional_u64(prior_state, "context_generation")?;
    let next_generation = optional_u64(next_state, "context_generation")?;
    if context_publication::is_publication_completion(&prior, &next) {
        if prior_generation != next_generation {
            return Err("source continuation publication cannot advance context again".into());
        }
        return Ok(());
    }
    if !follows(&prior, &next) {
        return Err("source continuation does not follow its exact completed predecessor".into());
    }
    let topology = |state: &BTreeMap<String, serde_json::Value>| -> Result<BoundRunAuthoringTopology, String> {
        serde_json::from_value(state.get(RUN_AUTHORING_BINDING).cloned().ok_or("source continuation lost its authored topology")?)
            .map_err(|error| error.to_string())
    };
    let old = topology(prior_state)?;
    let mut new = topology(next_state)?;
    new.checkpoints.retain(|template| {
        template.role != RunAuthoringCheckpointRole::ResultInput || old.checkpoints.contains(template)
    });
    if old != new {
        return Err("source continuation may add only the result waits of its own calls".into());
    }
    for template in old.checkpoints.iter().filter(|template| template.role.is_provider_input()) {
        let rows = |record: &RunRecord| {
            record.checkpoints.iter().filter(|checkpoint| {
                record.active_checkpoints.contains(&checkpoint.id)
                    && checkpoint_record_matches_template(checkpoint, template, &record.id)
            }).cloned().collect::<Vec<_>>()
        };
        if rows(before) != rows(after) {
            return Err("source continuation changed its input coordinates, context or artifact rights".into());
        }
    }
    for item in &next.items {
        if !old.checkpoints.iter().any(|template| ordinary_output(template.role) && pending_output_matches_template(item, template)) {
            return Err("source continuation may carry only declared calls and client projections".into());
        }
    }
    let mut live = current_mandatory_role_artifacts(before, config)?;
    for artifact in current_context_artifacts(before, config)? {
        append_artifact(&mut live, artifact);
    }
    let base = prior.context_publication.clone().unwrap_or_else(|| live.clone());
    let target = next.context_publication.clone().unwrap_or(live);
    if !target.starts_with(&base)
        || target[base.len()..].iter().any(|id| !next.items.iter().any(|item| &item.artifact_id == id))
        || (prior.deferred_publication && !next.deferred_publication)
    {
        return Err("source continuation may only extend its predecessor's context publication with its own calls".into());
    }
    if prior_generation.checked_add(u64::from(target.len() > base.len())) != Some(next_generation) {
        return Err("source continuation advances exactly one context generation when it carries context outputs, and none otherwise".into());
    }
    Ok(())
}

impl ProviderOutputHandoff {
    pub async fn hand_completed_source_continuation(
        &self,
        emissions: Vec<ProviderEmission>,
    ) -> Result<Vec<ProviderHandoffOutcome>, String> {
        if emissions.is_empty() {
            return Err("a source continuation carries no call".into());
        }
        if emissions.iter().any(|emission| matches!(emission.route, ProviderEmissionRoute::ClientProjectionWithVerifiedInputs { .. })) {
            return Err("a source continuation cannot expose an operator request".into());
        }
        let batches = split_handoff_batches_by_artifact_name(
            emissions.into_iter().map(Into::into).collect(),
            &self.session_snapshot_labels,
        )?;
        let mut outcomes = Vec::new();
        for batch in batches {
            let delivered = self
                .hand_rearm_batch_with_bindings(batch, true, &[], false, None, None, None, None, false, true)
                .await?;
            let failed = delivered.iter().any(|outcome| matches!(outcome, ProviderHandoffOutcome::TerminalFailure { .. }));
            outcomes.extend(delivered);
            if failed {
                break;
            }
        }
        Ok(outcomes)
    }

    pub(crate) fn author_completed_source_continuation(
        &self,
        current: &[u8],
        context_artifacts: &[ArtifactId],
        outputs: &[PreparedHandoff],
    ) -> Result<AuthoredRun, String> {
        let record = RunRecord::from_bytes(current).map_err(|error| error.to_string())?;
        let prior = self.completed_source_continuation_predecessor(&record, current)?;
        if outputs.iter().any(|output| {
            !ordinary_output(output.output_checkpoint.role)
                || !output.verified_inputs.is_empty()
                || self.session_snapshot_labels.contains(&output.identity.label)
        }) {
            return Err("a source continuation may carry only declared calls and client projections, never a session snapshot or operator request".into());
        }
        let contributes_context = outputs.iter().any(|output| context_artifacts.contains(&output.identity.artifact.id));
        let authored = self.author_run_with_generations(
            current, context_artifacts, outputs, false, u64::from(contributes_context), None, false, true, &[], Vec::new(), Some(&prior),
        )?;
        validate_transition(
            &record,
            &RunRecord::from_bytes(authored.content.as_bytes()).map_err(|error| error.to_string())?,
            &self.config,
        )?;
        Ok(authored)
    }

    fn completed_source_continuation_predecessor(
        &self,
        record: &RunRecord,
        current: &[u8],
    ) -> Result<PendingHandoff, String> {
        if !matches!(self.delivery_mode, HandoffDeliveryMode::Rearm)
            || self.failed_source
            || self.checked_settlement_output.is_some()
        {
            return Err("a source continuation requires the run-authoring handoff of an ordinary completed source".into());
        }
        let prior = persisted_pending_handoff(record, &self.config.provider_interface)?
            .ok_or("a source continuation has no completed handoff to continue")?;
        if prior.source_movement_seq != self.source_movement_seq
            || !prior.terminal
            || prior.source_without_context
            || prior.publication_unfinished()
            || prior.settlement_of.is_some()
            || prior.withdrawal.is_some()
            || prior.replacement.is_some()
        {
            return Err(format!(
                "source movement {} does not own a completed ordinary terminal handoff to continue",
                self.source_movement_seq
            ));
        }
        validate_pending_items_are_active(record, &self.config.provider_interface, &prior)?;
        if self.pending_handoff_evidence(&prior)?.iter().any(Option::is_none) {
            return Err("a source continuation precedes the complete delivery of its predecessor".into());
        }
        self.admitted_replay_handoff(&prior)?;
        if !matches!(self.recover_authoring_attempt(record, current)?, AuthoringAttemptRecovery::None) {
            return Err("a source continuation meets an unfinished authoring attempt".into());
        }
        if !self.standing_items_since_write(record)?.is_empty() {
            return Err("a source continuation meets standing deliveries after its completed handoff".into());
        }
        Ok(prior)
    }
}
