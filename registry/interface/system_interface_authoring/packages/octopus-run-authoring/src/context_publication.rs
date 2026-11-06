use super::*;

pub(super) fn is_publication_completion(before: &PendingHandoff, after: &PendingHandoff) -> bool {
    before.context_publication.is_some()
        && after.context_publication.is_none()
        && !before.deferred_publication
        && !after.deferred_publication
        && before.source_movement_seq == after.source_movement_seq
        && before.items == after.items
        && before.terminal == after.terminal
        && before.settlement_of == after.settlement_of
        && before.continuation_of == after.continuation_of
        && before.dispatch_fenced == after.dispatch_fenced
        && before.withdrawal == after.withdrawal
        && before.replacement == after.replacement
        && before.standing == after.standing
        && before.rebase == after.rebase
}

impl ProviderOutputHandoff {
    pub(super) async fn publish_admitted_context(&self, state: &mut HandoffState) -> Result<(), String> {
        self.reconcile_checked_run_state(state).await?;
        let record = RunRecord::from_bytes(&state.run_record_bytes).map_err(|error| error.to_string())?;
        let Some(pending) = pending_handoff(&record, &self.config.provider_interface)? else { return Ok(()); };
        if pending.context_publication.is_none() || pending.deferred_publication { return Ok(()); }
        if self.pending_handoff_evidence(&pending)?.iter().any(Option::is_none) {
            return Err("provider context publication precedes its admitted output artifacts".into());
        }
        let authored = self.author_context_publication(&state.run_record_bytes, &pending)?;
        match self.install_authored_run(state, &authored).await? {
            AuthoredRunInstallOutcome::Installed => Ok(()),
            AuthoredRunInstallOutcome::TerminalFailure(_) => Err("provider context publication file effect failed".into()),
        }
    }

    pub(super) fn author_context_publication(&self, current: &[u8], pending: &PendingHandoff) -> Result<AuthoredRun, String> {
        let target = pending.context_publication.as_ref().ok_or("provider has no pending context publication")?;
        let mut seen = BTreeSet::new();
        for id in target {
            non_empty_string(id.as_str(), "published context artifact")?;
            if !seen.insert(id) { return Err("published context repeats an artifact".into()); }
        }
        let record = RunRecord::from_bytes(current).map_err(|error| error.to_string())?;
        let state = record.interface_state.get(&self.config.provider_interface).ok_or("provider publication has no state")?;
        let topology: BoundRunAuthoringTopology = serde_json::from_value(
            state.get(RUN_AUTHORING_BINDING).ok_or("provider publication has no topology")?.clone()
        ).map_err(|error| error.to_string())?;
        topology.validate_identity(&record, &self.config.provider_interface)?;
        let mut config = self.config.clone();
        config.checkpoints = topology.checkpoints.clone();
        validate_bound_run_authoring_topology(&record, &config, None)?;
        if pending_handoff(&record, &config.provider_interface)?.as_ref() != Some(pending) {
            return Err("provider publication changed its pending outputs".into());
        }
        let generation = optional_u64(state, "authoring_generation")?.checked_add(1).ok_or("provider authoring generation overflow")?;
        let context_generation = optional_u64(state, "context_generation")?;
        let prefix = checkpoint_prefix(&record.id, &pending.items.last().ok_or("provider publication has no output")?.artifact_id, generation);
        let bindings = if pending.withdrawal.is_some() {
            source_withdrawal::publication_bindings(&record, &config, pending)?
        } else if pending.settlement_of.is_some() {
            source_settlement::publication_bindings(&record, &config, pending)?
        } else {
            let previous = current_context_bindings(&record, &config.checkpoints)?;
            target.iter().map(|id| serde_json::to_value(previous.iter().find(|binding| &binding.id == id).cloned()
                .unwrap_or_else(|| ArtifactBinding { id: id.clone(), access: Default::default() }))
                .map_err(|error| error.to_string())).collect::<Result<Vec<_>, _>>()?
        };
        let roles = current_mandatory_role_bindings(&record, &config)?;
        let roles = roles.iter().map(serde_json::to_value).collect::<Result<Vec<_>, _>>().map_err(|error| error.to_string())?;
        let mut value: serde_json::Value = serde_json::from_slice(current).map_err(|error| error.to_string())?;
        let before = value.clone();
        let root = value.as_object_mut().ok_or("provider publication record is not an object")?;
        replace_context_checkpoints(root, &config, &record.id, &prefix, &bindings)?;
        if !result_inputs::enabled(&config) {
            replace_verified_input_checkpoints(root, &config, &config, &record.id, &prefix, &bindings)?;
        }
        let mut frontier = frontier_checkpoint_ids(state)?.into_iter().collect::<Vec<_>>();
        let mut replaced = BTreeSet::new();
        let mut replacements = Vec::new();
        let mut replacement_ids = Vec::new();
        let mut result_id = None;
        for role in [RunAuthoringCheckpointRole::AuthoringCall, RunAuthoringCheckpointRole::AuthoringResult].into_iter().filter(|_|config.writer_run.is_none()) {
            let template = config.checkpoint_for_role(role)?;
            let old = active_checkpoint_for_role(&record, &config, role)?;
            replaced.insert(old.id.to_string());
            let id = format!("{prefix}_{}", template.name);
            replacements.push(materialize_checkpoint(template, &id, &record.id, &roles));
            replacement_ids.push(id.clone());
            if role == RunAuthoringCheckpointRole::AuthoringResult { result_id = Some(CheckpointId::new(id)); }
        }
        replace_owned_frontier(root, &replaced, replacements, &replacement_ids)?;
        frontier.retain(|id| !replaced.contains(id));
        frontier.extend(replacement_ids);
        let mut published = pending.clone();
        published.context_publication = None;
        let provider_state = root.get_mut("interface_state").and_then(serde_json::Value::as_object_mut)
            .and_then(|states| states.get_mut(config.provider_interface.as_str())).and_then(serde_json::Value::as_object_mut)
            .ok_or("provider publication lost its state")?;
        provider_state.insert("authoring_generation".into(), generation.into());
        provider_state.insert("context_generation".into(), context_generation.into());
        provider_state.insert(FRONTIER_CHECKPOINT_IDS.into(), serde_json::json!(frontier));
        provider_state.insert("pending_handoff".into(), serde_json::to_value(&published).map_err(|error| error.to_string())?);
        let bytes = JsonFormat::Compact.encode(&value)?;
        let candidate = RunRecord::from_bytes(&bytes).map_err(|error| error.to_string())?;
        validate_bound_run_authoring_topology(&candidate, &config, None)?;
        validate_persisted_profile_binding(&candidate, &config, &topology)?;
        let content = String::from_utf8(bytes).map_err(|error| error.to_string())?;
        let after_hash = ContentHash::of_bytes(content.as_bytes()).to_hex();
        let patch = json_patch::diff(&before, &value);
        if apply_run_json_patch(current, &patch, &after_hash, Some(JsonFormat::Compact))? != content { return Err("provider publication patch changed its bytes".into()); }
        let mut authored = AuthoredRun {
            json_format: Some(JsonFormat::Compact),
            before_hash: ContentHash::of_bytes(current).to_hex(), after_hash, content, patch,
            correlation_id: format!("provider-run-authoring:{}:{generation}", record.id),
            authoring_result_checkpoint: match result_id { Some(id) => id, None if config.writer_run.is_some() => CheckpointId::new(format!("{prefix}_writer_result")), None => return Err("provider publication has no authoring result".into()) },
            authoring_result_interface: config.checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringResult)?.interface().clone(),
            pending_handoff: published,
        };
        self.seal_authored_run(current, &mut authored, None)?;
        Ok(authored)
    }
}
