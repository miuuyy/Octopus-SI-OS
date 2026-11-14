use super::*;

pub struct PendingSourceSettlement {
    pub predecessor: ArtifactRef,
    pub predecessor_record: Vec<u8>,
    pub predecessor_outcomes: Vec<ProviderHandoffOutcome>,
    pub closure: Option<InterfaceArtifactFile>,
}

pub(super) fn follows_completed_source(before: &PendingHandoff, after: &PendingHandoff) -> bool {
    let [item] = before.items.as_slice() else {
        return false;
    };
    before.terminal
        && before.context_publication.is_none()
        && !before.source_without_context
        && after.terminal
        && !after.source_without_context
        && before.source_movement_seq == after.source_movement_seq
        && after.items.len() == 1
        && before.items != after.items
        && after.settlement_of.as_ref().is_some_and(|reference| {
            reference.id == item.artifact_id
                && reference.content.to_hex() == item.artifact_content_hash
        })
}

pub(super) fn validate_checked_transition(
    before: &RunRecord,
    after: &RunRecord,
    writer: &CheckedRunWriter,
) -> Result<(), String> {
    if persisted_pending_handoff(after, &writer.owner)?
        .is_none_or(|pending| pending.settlement_of.is_none())
    {
        return Ok(());
    }
    let config = config_from_persisted_profile(
        before,
        &writer.owner,
        &RunAuthoringProfile {
            file_write_tool: writer.tool.clone(),
            file_write_label: writer.call_label.clone(),
            file_write_result_label: writer.result_label.clone(),
        },
    )?;
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
    let Some(reference) = next.settlement_of.as_ref() else {
        return Ok(());
    };
    let prior = persisted_pending_handoff(before, provider)?
        .ok_or("source settlement has no predecessor")?;
    if !prior.terminal
        || !next.terminal
        || prior.source_without_context
        || next.source_without_context
    {
        return Err("source settlement must retain its completed context-bearing source".into());
    }
    let prior_state = &before.interface_state[provider];
    let next_state = &after.interface_state[provider];
    if prior_state.get(RUN_AUTHORING_BINDING) != next_state.get(RUN_AUTHORING_BINDING) {
        return Err("source settlement cannot change its input or output authority".into());
    }
    let publication = context_publication::is_publication_completion(&prior, &next);
    validate_input_rows(before, after, config, &prior, &next, publication)?;
    if publication {
        if optional_u64(prior_state, "context_generation")?
            != optional_u64(next_state, "context_generation")?
        {
            return Err("source settlement publication cannot advance context again".into());
        }
        return Ok(());
    }
    if !follows_completed_source(&prior, &next) {
        return Err("source settlement does not follow its exact completed predecessor".into());
    }
    if optional_u64(prior_state, "context_generation")?.checked_add(1)
        != Some(optional_u64(next_state, "context_generation")?)
    {
        return Err("source settlement must advance exactly one checked context generation".into());
    }
    if reference.id.as_str().trim().is_empty() {
        return Err("source settlement predecessor is empty".into());
    }
    let topology: BoundRunAuthoringTopology =
        serde_json::from_value(after.interface_state[provider][RUN_AUTHORING_BINDING].clone())
            .map_err(|error| error.to_string())?;
    for item in &next.items {
        if !topology.checkpoints.iter().any(|template| {
            let expected = pending_output_checkpoint(
                template,
                item.checkpoint.id.as_str(),
                &item.artifact_label,
                &[],
                &[],
            );
            template.role == RunAuthoringCheckpointRole::ProviderArtifactOutput
                && template.interface_binding.admits(&item.artifact_label)
                && item.checkpoint.interface_binding == expected.interface_binding
                && item.checkpoint.outbound_interface_bindings
                    == expected.outbound_interface_bindings
                && item.checkpoint.required_verified_artifacts
                    == expected.required_verified_artifacts
        }) {
            return Err("source settlement cannot dispatch an effect".into());
        }
    }
    Ok(())
}

pub(super) fn publication_bindings(
    record: &RunRecord,
    config: &RunAuthoringConfig,
    pending: &PendingHandoff,
) -> Result<Vec<serde_json::Value>, String> {
    let [item] = pending.items.as_slice() else {
        return Err("source settlement requires one context receipt".into());
    };
    let mut bindings = provider_context_bindings(record, &config.provider_interface)?;
    let predecessor = pending
        .settlement_of
        .as_ref()
        .ok_or("source settlement has no predecessor")?;
    if bindings
        .iter()
        .filter(|binding| binding.id == predecessor.id)
        .count()
        != 1
    {
        return Err("source settlement has no exact selected predecessor carrier".into());
    }
    if bindings
        .iter()
        .any(|binding| binding.id == item.artifact_id)
    {
        return Err("source settlement receipt is already selected".into());
    }
    bindings.retain(|binding| binding.id != predecessor.id);
    bindings.push(ArtifactBinding {
        id: item.artifact_id.clone(),
        access: Default::default(),
    });
    if pending.context_publication.as_ref()
        != Some(&bindings.iter().map(|binding| binding.id.clone()).collect())
    {
        return Err(
            "source settlement may only replace its exact predecessor in the selected context"
                .into(),
        );
    }
    bindings
        .iter()
        .map(serde_json::to_value)
        .collect::<Result<_, _>>()
        .map_err(|error| error.to_string())
}

fn validate_input_rows(
    before: &RunRecord,
    after: &RunRecord,
    config: &RunAuthoringConfig,
    prior: &PendingHandoff,
    next: &PendingHandoff,
    publication: bool,
) -> Result<(), String> {
    let bindings = publication_bindings(before, config, if publication { prior } else { next })?;
    let initial_bindings = provider_context_bindings(before, &config.provider_interface)?
        .iter()
        .map(serde_json::to_value)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    let prefix = checkpoint_prefix(
        &before.id,
        &next.items[0].artifact_id,
        optional_u64(
            &after.interface_state[&config.provider_interface],
            "authoring_generation",
        )?,
    );
    let mut expected = serde_json::to_value(before).map_err(|error| error.to_string())?;
    let root = expected
        .as_object_mut()
        .ok_or("source settlement input record is not an object")?;
    replace_context_checkpoints(
        root,
        config,
        &before.id,
        &prefix,
        if publication {
            &bindings
        } else {
            &initial_bindings
        },
    )?;
    if publication && !result_inputs::enabled(config) {
        replace_verified_input_checkpoints(root, config, config, &before.id, &prefix, &bindings)?;
    }
    let expected =
        RunRecord::from_bytes(&serde_json::to_vec(&expected).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
    for template in config
        .checkpoints
        .iter()
        .filter(|template| template.role.is_provider_input())
    {
        let rows = |record: &RunRecord| {
            record
                .checkpoints
                .iter()
                .filter(|checkpoint| {
                    record.active_checkpoints.contains(&checkpoint.id)
                        && checkpoint_record_matches_template(checkpoint, template, &record.id)
                })
                .cloned()
                .collect::<Vec<_>>()
        };
        if rows(&expected) != rows(after) {
            return Err(
                "source settlement changed its canonical input coordinates or artifact rights"
                    .into(),
            );
        }
    }
    Ok(())
}

impl ProviderHandoffSession {
    pub fn completed_source_candidates(
        &self,
        ctx: &InterfaceLiftContext,
    ) -> Result<Vec<(RunId, u64)>, String> {
        let runs = {
            let mut index = self.index()?;
            index.catch_up()?;
            let mut runs = BTreeMap::<String, BTreeMap<Option<String>, u64>>::new();
            for (coordinate, sources) in &index.coordinate_outbounds {
                let pins = runs.entry(coordinate.run.clone()).or_default();
                for source in sources {
                    pins.entry(source.run_record_hash.clone()).or_insert(source.seq);
                }
            }
            runs
        };
        let root = ctx.root_slot("run_records_root")?;
        let mut candidates = Vec::new();
        for (id, source_pins) in runs {
            if std::path::Path::new(&id).components().count() != 1 {
                return Err("provider source has an invalid run id".into());
            }
            let path = root.join(format!("{id}.json"));
            if !path.exists() {
                continue;
            }
            let bytes = octopus_interfaces::read_artifact_carrier_file_bounded(
                &path,
                ctx.required_host_io_limits()?.max_payload_bytes,
            )
            .map_err(|error| error.to_string())?;
            let record = RunRecord::from_bytes(&bytes).map_err(|error| error.to_string())?;
            if record.id.as_str() != id {
                return Err("completed source run identity changed".into());
            }
            if !record.interface_state.get(&ctx.interface).is_some_and(|state| state.contains_key(RUN_AUTHORING_BINDING)) {
                if record.interface_state.get(&ctx.interface).is_some_and(|state| state.contains_key("pending_handoff")) {
                    return Err("pending provider handoff has no declared authoring binding".into());
                }
                for (pin, source) in source_pins {
                    let hash = pin.as_deref().and_then(ContentHash::from_hex)
                        .ok_or_else(|| format!("provider source movement {source} has no pinned run record"))?;
                    let (_, original) = pinned_run_record(ctx, &hash, "completed source classification")?;
                    if original.id != record.id || original.interface_state.get(&ctx.interface)
                        .is_some_and(|state| state.contains_key(RUN_AUTHORING_BINDING) || state.contains_key("pending_handoff")) {
                        return Err("provider run lost its declared authoring state".into());
                    }
                }
                continue;
            }
            if let Some(pending) = persisted_pending_handoff(&record, &ctx.interface)? {
                if pending.terminal && !pending.source_without_context {
                    candidates.push((record.id, pending.source_movement_seq));
                }
            }
        }
        Ok(candidates)
    }
}

impl ProviderOutputHandoff {
    pub async fn recover_completed_source_settlement(
        &self,
        predecessor: &ArtifactRef,
        closure: &ArtifactRef,
    ) -> Result<(ProviderSourceRecovery, Vec<ProviderHandoffOutcome>), String> {
        let mut checked = self.clone();
        checked.checked_settlement_output = Some((predecessor.clone(), closure.clone()));
        checked.recover_completed_source_with_outcomes().await
    }

    pub(super) fn require_settlement_output_pin(
        &self,
        pending: &PendingHandoff,
    ) -> Result<(), String> {
        let Some((predecessor, expected)) = &self.checked_settlement_output else {
            return Ok(());
        };
        let [item] = pending.items.as_slice() else {
            return Err("checked settlement lost its single output".into());
        };
        if pending.settlement_of.as_ref() != Some(predecessor)
            || pending.source_movement_seq != self.source_movement_seq
            || item.artifact_id != expected.id
            || item.artifact_content_hash != expected.content.to_hex()
        {
            return Err(
                "saved settlement candidate changed after provider semantic validation".into(),
            );
        }
        Ok(())
    }

    pub async fn pending_source_settlement(
        &self,
    ) -> Result<Option<PendingSourceSettlement>, String> {
        self.observe_live_record(|handoff, state| handoff.pending_source_settlement_in(state)).await
    }

    fn pending_source_settlement_in(
        &self,
        state: &HandoffState,
    ) -> Result<Option<PendingSourceSettlement>, String> {
        let current =
            RunRecord::from_bytes(&state.run_record_bytes).map_err(|error| error.to_string())?;
        let chain = self.completed_source_chain(&state.run_record_bytes)?;
        let attempt = self.recover_authoring_attempt(&current, &state.run_record_bytes)?;
        let installed = persisted_pending_handoff(&current, &self.config.provider_interface)?
            .filter(|pending| {
                pending.source_movement_seq == self.source_movement_seq
                    && pending.settlement_of.is_some()
            });
        let (selected, before) = if let Some(pending) = installed {
            match attempt {
                AuthoringAttemptRecovery::None => {}
                AuthoringAttemptRecovery::OtherSource(_) => {
                    return Err(
                        "installed settlement has a different pending source attempt".into(),
                    );
                }
                AuthoringAttemptRecovery::InFlight(attempt) => {
                    if !context_publication::is_publication_completion(
                        &pending,
                        &attempt.authored.pending_handoff,
                    ) {
                        return Err(
                            "installed settlement has a conflicting successor attempt".into()
                        );
                    }
                    validate_transition(
                        &current,
                        &RunRecord::from_bytes(attempt.authored.content.as_bytes())
                            .map_err(|error| error.to_string())?,
                        &self.config,
                    )?;
                }
            }
            let mut matched = Vec::new();
            for transition in &chain {
                let before =
                    RunRecord::from_bytes(&transition.before).map_err(|error| error.to_string())?;
                let after =
                    RunRecord::from_bytes(&transition.after).map_err(|error| error.to_string())?;
                if let (Some(prior), Some(next)) = (
                    persisted_pending_handoff(&before, &self.config.provider_interface)?,
                    persisted_pending_handoff(&after, &self.config.provider_interface)?,
                ) {
                    if next.source_movement_seq == self.source_movement_seq
                        && next.settlement_of == pending.settlement_of
                        && next.items == pending.items
                        && follows_completed_source(&prior, &next)
                    {
                        matched.push(transition.before.clone());
                    }
                }
            }
            if matched.len() != 1 {
                return Err("saved settlement has no unique checked predecessor record".into());
            }
            (pending, matched.remove(0))
        } else {
            match attempt {
                AuthoringAttemptRecovery::None => {
                    let Some(prior) =
                        persisted_pending_handoff(&current, &self.config.provider_interface)?
                            .filter(|pending| {
                                pending.source_movement_seq == self.source_movement_seq
                                    && pending.terminal
                            })
                    else {
                        return Ok(None);
                    };
                    let predecessor_outcomes = self.admitted_replay_handoff(&prior)?;
                    let [ProviderHandoffOutcome::Admitted { artifact, .. }] =
                        predecessor_outcomes.as_slice()
                    else {
                        return Err(
                            "completed settlement source has no single admitted predecessor".into(),
                        );
                    };
                    return Ok(Some(PendingSourceSettlement {
                        predecessor: artifact.clone(),
                        predecessor_record: state.run_record_bytes.clone(),
                        predecessor_outcomes,
                        closure: None,
                    }));
                }
                AuthoringAttemptRecovery::OtherSource(_) => {
                    return Err("saved settlement attempt belongs to a different source".into());
                }
                AuthoringAttemptRecovery::InFlight(attempt) => {
                    if attempt.authored.pending_handoff.settlement_of.is_none() {
                        return Err("saved source attempt is not a completed settlement".into());
                    }
                    let before = if attempt.authored.before_hash
                        == ContentHash::of_bytes(&state.run_record_bytes).to_hex()
                    {
                        state.run_record_bytes.clone()
                    } else {
                        pinned_run_record(
                            &self.ctx,
                            &ContentHash::from_hex(&attempt.authored.before_hash)
                                .ok_or("saved settlement preimage hash is malformed")?,
                            "saved settlement predecessor",
                        )?
                        .0
                    };
                    let original =
                        RunRecord::from_bytes(&before).map_err(|error| error.to_string())?;
                    let candidate = RunRecord::from_bytes(attempt.authored.content.as_bytes())
                        .map_err(|error| error.to_string())?;
                    validate_transition(
                        &original,
                        &candidate,
                        &attempt_config(&original, &self.config)?,
                    )?;
                    (attempt.authored.pending_handoff, before)
                }
            }
        };
        let predecessor = selected
            .settlement_of
            .clone()
            .ok_or("saved settlement lost its predecessor")?;
        let before_record = RunRecord::from_bytes(&before).map_err(|error| error.to_string())?;
        let prior = persisted_pending_handoff(&before_record, &self.config.provider_interface)?
            .ok_or("saved settlement predecessor has no completed output")?;
        if !follows_completed_source(&prior, &selected) {
            return Err("saved settlement changed its exact completed predecessor".into());
        }
        let predecessor_outcomes = self.admitted_replay_handoff(&prior)?;
        let (closure, _) = load_pending_artifact(&self.var_dir, &selected.items[0])?;
        Ok(Some(PendingSourceSettlement {
            predecessor,
            predecessor_record: before,
            predecessor_outcomes,
            closure: Some(closure),
        }))
    }

    pub async fn completed_source_settlement_predecessor(
        &self, closure: &ArtifactRef,
    ) -> Result<(Vec<u8>, Vec<ProviderHandoffOutcome>), String> {
        self.observe_live_record(|handoff, state| handoff.completed_source_settlement_predecessor_in(state, closure)).await
    }

    fn completed_source_settlement_predecessor_in(
        &self, state: &HandoffState, closure: &ArtifactRef,
    ) -> Result<(Vec<u8>, Vec<ProviderHandoffOutcome>), String> {
        let chain = self.completed_source_chain(&state.run_record_bytes)?;
        let mut evidence = None;
        let mut selected: Option<PendingHandoff> = None;
        for transition in &chain {
            let before = RunRecord::from_bytes(&transition.before).map_err(|error| error.to_string())?;
            let after = RunRecord::from_bytes(&transition.after).map_err(|error| error.to_string())?;
            let prior = persisted_pending_handoff(&before, &self.config.provider_interface)?;
            let next = persisted_pending_handoff(&after, &self.config.provider_interface)?;
            if let Some(pending) = selected.as_ref() {
                if prior.as_ref() != Some(pending) {
                    return Err("completed settlement history changed its selected handoff".into());
                }
                if next != prior {
                    let publication = next.as_ref().is_some_and(|next|
                        context_publication::is_publication_completion(pending, next));
                    if pending.publication_unfinished() && !publication {
                        return Err("completed settlement was superseded before publication".into());
                    }
                    self.admitted_replay_handoff_before(pending, transition.call.seq.0)?;
                    selected = publication.then(|| next.clone().expect("checked publication"));
                }
            }
            let (Some(prior), Some(next)) = (prior, next) else { continue; };
            if next.source_movement_seq != self.source_movement_seq
                || !follows_completed_source(&prior, &next)
                || next.items.len() != 1 || next.items[0].artifact_id != closure.id
                || next.items[0].artifact_content_hash != closure.content.to_hex() {
                continue;
            }
            self.admitted_replay_handoff_before(&prior, transition.call.seq.0)?;
            self.admitted_replay_handoff(&next)?;
            if evidence.replace((transition.before.clone(), self.admitted_replay_handoff(&prior)?)).is_some() {
                return Err("completed settlement has multiple checked predecessors".into());
            }
            selected = Some(next);
        }
        if selected.as_ref().is_some_and(|pending| pending.publication_unfinished()) {
            return Err("completed settlement has unfinished publication".into());
        }
        evidence.ok_or("completed settlement has no exact checked predecessor".into())
    }

    pub(super) fn completed_source_chain(&self, current: &[u8]) -> Result<Vec<CheckedRunTransition>, String> {
        let (_, source) = self.session.index()?.exact_provider_source_outbound(
            &self.config.run,
            &self.config.provider_interface,
            self.source_movement_seq,
        )?;
        let hash = source
            .run_record_hash
            .as_deref()
            .and_then(ContentHash::from_hex)
            .ok_or("completed source has no pinned record")?;
        let (bytes, original) = pinned_run_record(&self.ctx, &hash, "completed source")?;
        let config = config_from_persisted_profile(
            &original,
            &self.config.provider_interface,
            &RunAuthoringProfile {
                file_write_tool: self.config.file_write_tool.clone(),
                file_write_label: self.config.file_write_label.clone(),
                file_write_result_label: self.config.file_write_result_label.clone(),
            },
        )?;
        let provider_writer = CheckedRunWriter {
            owner: config.provider_interface.clone(),
            source: config.authoring_source_interface.clone(),
            door: config
                .checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringResult)?
                .interface()
                .clone(),
            tool: config.file_write_tool.clone(),
            call_label: config.file_write_label.clone(),
            result_label: config.file_write_result_label.clone(),
        };
        let mut writers = config
            .checkpoints
            .iter()
            .filter_map(|checkpoint| checkpoint.owner_authoring.clone())
            .collect::<Vec<_>>();
        writers.push(provider_writer.clone());
        let writers = declared_writer_closure(&original, writers)?;
        let limits = self.ctx.required_host_io_limits()?;
        let evidence = CheckedRunEvidence {
            movements_path: &self.ctx.movements_path,
            ledger_index_path: &self.ctx.ledger_index_path,
            blobs_dir: &self.ctx.blobs_dir,
            max_control_frame_bytes: limits.max_control_frame_bytes,
            max_manifest_bytes: limits.max_manifest_bytes,
            max_payload_bytes: limits.max_payload_bytes,
        };
        let chain =
            stable_run_observation(&self.run_path, current, limits.max_payload_bytes, || {
                if bytes == current { return Ok(Vec::new()); }
                let start = concurrent_writer_evidence_start(
                    &evidence,
                    &self.run_path,
                    &original,
                    &bytes,
                    self.source_movement_seq,
                    &writers,
                )?;
                let mut reader = FileCheckedRunReader::open(&evidence, Some(start))?
                    .with_provider_context(&self.ctx);
                let scan_start = reader.next_sequence();
                checked_run_file_chain_with_reader(&mut reader, &self.run_path, &bytes,
                    CheckedRunTarget::Snapshot(current), start, scan_start, None, &writers,
                    &[], Some(&config), None)
            })
            .map_err(String::from)?;
        for transition in &chain {
            if transition.writer == provider_writer {
                validate_checked_provider_transition(transition)?;
            } else {
                validate_checked_owner_transition(transition)?;
            }
        }
        Ok(chain)
    }

    pub async fn recover_completed_source_with_outcomes(
        &self,
    ) -> Result<(ProviderSourceRecovery, Vec<ProviderHandoffOutcome>), String> {
        self.observe_live_record(|handoff, state| {
            let current = RunRecord::from_bytes(&state.run_record_bytes)
                .map_err(|error| error.to_string())?;
            let pending = persisted_pending_handoff(&current, &handoff.config.provider_interface)?
                .ok_or("completed source lost its pending handoff")?;
            if !pending.terminal || pending.source_movement_seq != handoff.source_movement_seq {
                return Err("completed source changed its checked terminal coordinate".into());
            }
            let chain = handoff.completed_source_chain(&state.run_record_bytes)?;
            handoff.restore_checked_context(state, &current, &chain)
        }).await?;
        self.recover_source_with_outcomes().await
    }

    pub async fn hand_completed_source_settlement(
        &self,
        predecessor: &ArtifactRef,
        context_generation: u64,
        emission: ProviderEmission,
    ) -> Result<ProviderHandoffOutcome, String> {
        let label = emission
            .artifact
            .read_manifest()
            .map_err(|error| error.to_string())?
            .label;
        if !matches!(self.delivery_mode, HandoffDeliveryMode::Rearm)
            || emission.route != ProviderEmissionRoute::ClientProjection
            || !self.context_output_labels.contains(&label)
            || !self.session_snapshot_labels.contains(&label)
        {
            return Err(
                "completed source settlement requires one declared client context snapshot".into(),
            );
        }
        self.hand_rearm_batch_with_bindings(
            vec![emission.into()],
            true,
            &[],
            false,
            None,
            None,
            Some((predecessor, context_generation)),
            None,
            false,
            false,
        )
        .await?
        .into_iter()
        .next()
        .ok_or("completed source settlement has no outcome".into())
    }

    pub(super) fn attach_source_settlement(
        &self,
        before: &[u8],
        authored: &mut AuthoredRun,
        predecessor: &ArtifactRef,
        context_generation: u64,
    ) -> Result<(), String> {
        let record = RunRecord::from_bytes(before).map_err(|error| error.to_string())?;
        if optional_u64(
            record
                .interface_state
                .get(&self.config.provider_interface)
                .ok_or("completed source settlement lost provider state")?,
            "context_generation",
        )? != context_generation
        {
            return Err(
                "completed source settlement changed its checked context generation".into(),
            );
        }
        let prior = persisted_pending_handoff(&record, &self.config.provider_interface)?
            .ok_or("completed source settlement has no checked predecessor")?;
        if prior.source_movement_seq != self.source_movement_seq {
            return Err("completed source settlement changed source".into());
        }
        self.admitted_replay_handoff(&prior)?;
        authored.pending_handoff.settlement_of = Some(predecessor.clone());
        if !follows_completed_source(&prior, &authored.pending_handoff) {
            return Err("completed source settlement differs from its admitted predecessor".into());
        }
        let mut value: serde_json::Value =
            serde_json::from_str(&authored.content).map_err(|error| error.to_string())?;
        value["interface_state"][self.config.provider_interface.as_str()]["pending_handoff"] =
            serde_json::to_value(&authored.pending_handoff).map_err(|error| error.to_string())?;
        let prefix = checkpoint_prefix(
            &record.id,
            &authored.pending_handoff.items[0].artifact_id,
            correlation_generation(&record.id, &authored.correlation_id)?,
        );
        let bindings = provider_context_bindings(&record, &self.config.provider_interface)?
            .iter()
            .map(serde_json::to_value)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        replace_context_checkpoints(
            value
                .as_object_mut()
                .ok_or("source settlement is not an object")?,
            &self.config,
            &record.id,
            &prefix,
            &bindings,
        )?;
        let bytes = authored
            .json_format
            .unwrap_or(JsonFormat::Compact)
            .encode(&value)?;
        validate_transition(
            &record,
            &RunRecord::from_bytes(&bytes).map_err(|error| error.to_string())?,
            &self.config,
        )?;
        authored.content = String::from_utf8(bytes).map_err(|error| error.to_string())?;
        authored.after_hash = ContentHash::of_bytes(authored.content.as_bytes()).to_hex();
        authored.patch = json_patch::diff(
            &serde_json::from_slice(before).map_err(|error| error.to_string())?,
            &value,
        );
        Ok(())
    }
}
