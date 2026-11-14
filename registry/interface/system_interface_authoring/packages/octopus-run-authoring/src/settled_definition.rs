use super::*;

impl ProviderOutputHandoff {
    pub(super) fn settled_input_definition(
        &self,
        record: &RunRecord,
        current: &[u8],
    ) -> Result<bool, String> {
        let Some(pending) = persisted_pending_handoff(record, &self.config.provider_interface)?
        else {
            return Ok(false);
        };
        if !pending.terminal
            || pending.context_publication.is_some()
            || pending.items.is_empty()
            || pending.source_movement_seq >= self.source_movement_seq
        {
            return Ok(false);
        }
        let Some((applied_seq, accepted)) = self.last_applied_definition(record)? else {
            return Ok(false);
        };
        if applied_seq >= self.source_movement_seq {
            return Ok(false);
        }
        let accepted = RunRecord::from_bytes(&accepted).map_err(|error| error.to_string())?;
        if !preserves_settled_continuity(&accepted, record, &self.config.provider_interface)? {
            return Ok(false);
        }
        let (coordinate, source) = self.session.index()?.exact_provider_source_outbound(
            &record.id,
            &self.config.provider_interface,
            self.source_movement_seq,
        )?;
        if source.run_record_hash.as_deref()
            != Some(ContentHash::of_bytes(current).to_hex().as_str())
        {
            return Ok(false);
        }
        let checkpoint = record
            .checkpoint(&CheckpointId::new(&coordinate.checkpoint))
            .ok_or("new definition input lost its admitted checkpoint")?;
        if !record.active_checkpoints.contains(&checkpoint.id)
            || coordinate.epoch != checkpoint.epoch.0
            || !self.config.checkpoints.iter().any(|template| {
                template.role == RunAuthoringCheckpointRole::Stable
                    && template.include_context
                    && checkpoint_record_matches_template(checkpoint, template, &record.id)
                    && indexed_evidence_matches_checkpoint(&source, template)
                    && artifact_refs_match_bindings(
                        &source.artifact_binding_refs,
                        &checkpoint.artifact_bindings,
                    )
            })
        {
            return Ok(false);
        }
        Ok(self.pending_handoff_evidence(&pending)?.iter().all(|seq| {
            seq.is_some_and(|seq| {
                seq > pending.source_movement_seq && seq < self.source_movement_seq
            })
        }))
    }

    fn last_applied_definition(
        &self,
        record: &RunRecord,
    ) -> Result<Option<(u64, Vec<u8>)>, String> {
        if self.config.writer_run.is_some() {
            return writer_run::last_applied_target(&self.ctx, &self.session, &self.config);
        }
        let generation = optional_u64(
            record
                .interface_state
                .get(&self.config.provider_interface)
                .ok_or("settled provider state is absent")?,
            "authoring_generation",
        )?;
        let mut accepted = None;
        for (path, call) in private_authoring_calls(&self.ctx, &self.hand_in_root, &self.config)? {
            if call.correlation_id != format!("provider-run-authoring:{}:{generation}", record.id) {
                continue;
            }
            let hash = ContentHash::from_hex(&call.arguments.expected_before_hash)
                .ok_or("settled provider write has no exact preimage")?;
            let (before, original) = pinned_run_record(&self.ctx, &hash, "settled provider write")?;
            let config = attempt_config(&original, &self.config)?;
            let attempt = validate_authoring_attempt_source(AuthoringAttemptSourceValidation {
                ctx: &self.ctx,
                record: &original,
                record_bytes: &before,
                config: &config,
                call,
                manifest_path: path,
            })?;
            if let Some(seq) = authoring_success_evidence(
                &self.ctx,
                &self.session,
                &config,
                &attempt.authored,
                Some(attempt.provider_source_movement_seq),
            )? {
                if accepted
                    .replace((seq, attempt.authored.content.into_bytes()))
                    .is_some()
                {
                    return Err(
                        "settled provider generation has ambiguous admitted definitions".into(),
                    );
                }
            }
        }
        Ok(accepted)
    }
}

fn preserves_settled_continuity(
    accepted: &RunRecord,
    current: &RunRecord,
    provider: &InterfaceId,
) -> Result<bool, String> {
    if accepted.id != current.id
        || accepted.worker != current.worker
        || accepted.trace != current.trace
    {
        return Ok(false);
    }
    let protected = |record: &RunRecord| -> Result<BTreeMap<String, serde_json::Value>, String> {
        let mut state = record
            .interface_state
            .get(provider)
            .ok_or("settled definition lost its provider state")?
            .clone();
        let mut topology: BoundRunAuthoringTopology = serde_json::from_value(
            state
                .get(RUN_AUTHORING_BINDING)
                .ok_or("settled definition lost its authoring binding")?
                .clone(),
        )
        .map_err(|error| error.to_string())?;
        topology.checkpoints.retain(|template| {
            matches!(
                template.role,
                RunAuthoringCheckpointRole::AuthoringCall
                    | RunAuthoringCheckpointRole::AuthoringResult
            )
        });
        state.insert(
            RUN_AUTHORING_BINDING.into(),
            serde_json::to_value(topology).map_err(|error| error.to_string())?,
        );
        state.remove(FRONTIER_CHECKPOINT_IDS);
        Ok(state)
    };
    Ok(protected(accepted)? == protected(current)?)
}

#[cfg(test)]
#[path = "../tests/support/settled_definition.rs"]
mod tests;
