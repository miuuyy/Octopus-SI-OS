use super::*;

#[derive(Debug)]
pub enum FailedSourceHandoffError {
    Unavailable(String),
    Interrupted(String),
}

pub fn payload_carries_session_context(payload: &[u8]) -> Result<bool, String> {
    let value: serde_json::Value = serde_json::from_slice(payload)
        .map_err(|error| format!("provider artifact payload is not JSON: {error}"))?;
    Ok(value.get(SESSION_CONTEXT_FIELD).is_some())
}

impl ProviderLedgerEvidenceIndex {
    fn latest_seq(&mut self) -> Result<Option<u64>, String> {
        self.catch_up()?;
        Ok(self.next_seq.checked_sub(1))
    }
}

impl ProviderHandoffSession {
    pub fn consumed_context_input_sources(
        &self,
        ctx: &InterfaceLiftContext,
        descriptor: &InterfaceDescriptor,
    ) -> Result<Vec<(RunId, u64)>, String> {
        let runs = {
            let mut index = self.index()?;
            index.catch_up()?;
            index
                .coordinate_outbounds
                .keys()
                .map(|coordinate| coordinate.run.clone())
                .collect::<BTreeSet<_>>()
        };
        let run_records_root = ctx.root_slot("run_records_root")?;
        let limit = ctx.required_host_io_limits()?.max_payload_bytes;
        let mut consumed = Vec::new();
        for run in runs {
            if RunId::new(run.as_str()).as_str() != run
                || std::path::Path::new(&run).components().count() != 1
            {
                continue;
            }
            let path = run_records_root.join(format!("{run}.json"));
            let Ok(record) = octopus_interfaces::read_artifact_carrier_file_bounded(&path, limit)
            else {
                continue;
            };
            if let Ok(found) = self.consumed_input_source_sequences(ctx, descriptor, &record) {
                consumed.extend(found);
            }
        }
        consumed.sort_by_key(|(_, source)| *source);
        Ok(consumed)
    }

    pub fn consumed_context_input_source(
        &self,
        ctx: &InterfaceLiftContext,
        descriptor: &InterfaceDescriptor,
        run_record: &[u8],
    ) -> Result<Option<(RunId, u64)>, String> {
        Ok(self.consumed_input_source_sequences(ctx, descriptor, run_record)?
            .into_iter().max_by_key(|(_, source)| *source))
    }

    fn consumed_input_source_sequences(
        &self,
        ctx: &InterfaceLiftContext,
        descriptor: &InterfaceDescriptor,
        run_record: &[u8],
    ) -> Result<Vec<(RunId, u64)>, String> {
        let record = RunRecord::from_bytes(run_record).map_err(|error| error.to_string())?;
        let Some(provider_state) = record
            .interface_state
            .get(&ctx.interface)
            .filter(|state| state.contains_key(RUN_AUTHORING_BINDING))
        else {
            return Ok(Vec::new());
        };
        optional_u64(provider_state, "authoring_generation")?;
        let profile = run_authoring_profile(descriptor)?;
        let (config, _) =
            resolve_profile_invocation(&profile, &ctx.interface, &record.id, &record, None)?;
        let pending_source = persisted_pending_handoff(&record, &config.provider_interface)?
            .map(|pending| pending.source_movement_seq);
        let result_sources = config
            .checkpoints
            .iter()
            .filter(|template| {
                matches!(
                    template.role,
                    RunAuthoringCheckpointRole::ProviderToolOutput
                        | RunAuthoringCheckpointRole::ProviderInterfaceOutput
                )
            })
            .flat_map(RunAuthoringCheckpoint::outbound_interfaces)
            .collect::<BTreeSet<_>>();
        let mut index = self.index()?;
        index.catch_up()?;
        let mut consumed = Vec::new();
        for template in config.checkpoints.iter().filter(|template| {
            matches!(template.role, RunAuthoringCheckpointRole::Stable | RunAuthoringCheckpointRole::IndependentInput)
                && !result_sources.contains(template.interface())
        }) {
            for checkpoint in record.checkpoints.iter().filter(|checkpoint| {
                record.active_checkpoints.contains(&checkpoint.id)
                    && checkpoint_record_matches_template(checkpoint, template, &record.id)
            }) {
                let coordinate = RuntimeCoordinateKey::new(
                    &record.id,
                    &record.worker,
                    &config.provider_interface,
                    &checkpoint.id,
                    checkpoint.epoch.0,
                );
                let sources = index
                    .coordinate_outbounds
                    .get(&coordinate)
                    .into_iter().flatten()
                    .filter(|outbound| outbound.source == *template.interface())
                    .map(|outbound| outbound.seq)
                    .collect::<Vec<_>>();
                if sources.len() > 1 {
                    return Err("consumed provider input has ambiguous source movements".into());
                }
                if let Some(source) = sources.first().filter(|source| pending_source != Some(**source)) {
                    consumed.push((record.id.clone(), *source));
                }
            }
        }
        consumed.sort_by_key(|(_, source)| *source);
        Ok(consumed)
    }

    pub fn failed_source_handoff(
        &self,
        ctx: &InterfaceLiftContext,
        descriptor: &InterfaceDescriptor,
        run: &RunId,
        input: &InterfaceInvocationEnvelope,
        source_movement_seq: u64,
    ) -> Result<ProviderOutputHandoff, String> {
        if descriptor.id != ctx.interface || self.interface != ctx.interface {
            return Err(format!(
                "failed provider source belongs to interface '{}', lifted interface is '{}'",
                descriptor.id.as_str(),
                ctx.interface.as_str()
            ));
        }
        let profile = run_authoring_profile(descriptor)?;
        let runtime = input.runtime().map_err(|error| error.to_string())?;
        if runtime.run != *run || runtime.interface != ctx.interface {
            return Err(format!(
                "failed provider source names run '{}' interface '{}', life drives run '{}' interface '{}'",
                runtime.run.as_str(),
                runtime.interface.as_str(),
                run.as_str(),
                ctx.interface.as_str()
            ));
        }
        let granted_tools = input
            .granted_tool_names()
            .map_err(|error| error.to_string())?
            .into_iter()
            .collect::<BTreeSet<_>>();
        let metadata = input.metadata().map_err(|error| error.to_string())?;
        let granted_interfaces = metadata
            .allowed_interfaces()
            .iter()
            .map(InterfaceId::new)
            .collect::<BTreeSet<_>>();
        let granted_mark_outcomes = metadata
            .grant()
            .mark_outcomes
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        let run_records_root = ctx
            .root_slot("run_records_root")?
            .canonicalize()
            .map_err(|error| {
                format!("failed provider source run_records_root cannot be canonicalized: {error}")
            })?;
        let hand_in_root = ctx.root_slot("interface_hand_in_root")?;
        if !hand_in_root.is_absolute() {
            return Err(format!(
                "failed provider source interface_hand_in_root is not absolute: {}",
                hand_in_root.display()
            ));
        }
        let run_path = run_records_root
            .join(format!("{}.json", run.as_str()))
            .canonicalize()
            .map_err(|error| {
                format!(
                    "failed provider source run '{}' record cannot be canonicalized: {error}",
                    run.as_str()
                )
            })?;
        if run_path.parent() != Some(run_records_root.as_path()) {
            return Err(format!(
                "failed provider source run '{}' record escapes the granted run_records_root",
                run.as_str()
            ));
        }
        let limit = ctx.required_host_io_limits()?.max_payload_bytes;
        let current = octopus_interfaces::read_artifact_carrier_file_bounded(&run_path, limit)
            .map_err(|error| error.to_string())?;
        let record = RunRecord::from_bytes(&current).map_err(|error| {
            format!(
                "failed provider source run '{}' current record is malformed: {error}",
                run.as_str()
            )
        })?;
        if record.id != *run || record.worker != runtime.worker {
            return Err(format!(
                "failed provider source run '{}' current record names another run or worker",
                run.as_str()
            ));
        }
        let (config, bound_topology) =
            resolve_profile_invocation(&profile, &ctx.interface, run, &record, None)?;
        let delivery_mode =
            derive_handoff_delivery_mode(input, &config, &record, &runtime.checkpoint, &granted_tools)?;
        if delivery_mode != HandoffDeliveryMode::Rearm {
            return Err(format!(
                "failed provider source run '{}' has no run re-arm authority",
                run.as_str()
            ));
        }
        let role_bindings = current_mandatory_role_bindings(&record, &config)?;
        let template = record
            .checkpoint(&runtime.checkpoint)
            .filter(|checkpoint| {
                checkpoint.epoch.0 == runtime.epoch
                    && record.active_checkpoints.contains(&checkpoint.id)
            })
            .and_then(|checkpoint| {
                let mut matches = config.checkpoints.iter().filter(|template| {
                    template.role.is_provider_input()
                        && checkpoint_role_bindings_match(checkpoint, &role_bindings, template)
                        && checkpoint_record_matches_template(checkpoint, template, &record.id)
                });
                let first = matches.next()?;
                matches.next().is_none().then_some(first)
            });
        let consumed_context_free_independent_input = template
            .filter(|template| {
                template.role == RunAuthoringCheckpointRole::IndependentInput
                    && !template.include_context
            })
            .cloned();
        let consumed_verified_input = template
            .is_some_and(|template| template.role == RunAuthoringCheckpointRole::VerifiedInput);
        let consumed_input = template.is_some()
            && !consumed_verified_input
            && consumed_context_free_independent_input.is_none();
        let current_bindings = if config.payload_artifact_bindings.is_empty() { Vec::new() }
            else { provider_context_bindings(&record, &config.provider_interface)? };
        let static_context_artifacts = config
            .payload_artifact_bindings
            .iter()
            .filter(|reference| current_bindings.iter().any(|binding| binding.id == reference.id && binding.access.read))
            .map(|reference| reference.id.clone())
            .collect::<Vec<_>>();
        let evidence_seq = self
            .index()?
            .latest_seq()?
            .map_or(source_movement_seq, |latest| latest.max(source_movement_seq));
        let observed = octopus_interfaces::read_artifact_carrier_file_bounded(&run_path, limit)
            .map_err(|error| error.to_string())?;
        if observed != current {
            return Err(format!(
                "failed provider source run '{}' changed while its current record was read",
                run.as_str()
            ));
        }
        let result_reader = LedgerTailReader::open_named(ctx, "run-authoring")?;
        let handoff = ProviderOutputHandoff {
            ctx: ctx.clone(),
            invocation_metadata: input.metadata().map_err(|error| error.to_string())?.clone(),
            run_path,
            hand_in_root: hand_in_root.join(config.authoring_source_interface.as_str()),
            var_dir: ctx.interface_var_dir(),
            source_movement_seq,
            source_carries_context: false,
            failed_source: true,
            checked_settlement_output: None,
            context_output_labels: BTreeSet::new(),
            session_snapshot_labels: BTreeSet::new(),
            standing_candidate_labels: BTreeSet::new(),
            granted_tools,
            granted_interfaces,
            granted_mark_outcomes,
            bound_topology,
            delivery_mode,
            session: self.clone(),
            invocation_session_snapshot: None,
            state: Arc::new(tokio::sync::Mutex::new(HandoffState {
                validated_withdrawal_output: None,
                context_artifacts: Vec::new(),
                static_context_artifacts,
                session_context: ResolvedSessionContext {
                    provider_interface: config.provider_interface.clone(),
                    artifacts: Vec::new(),
                },
                session_snapshot_head: None,
                run_record_bytes: current,
                run_record_evidence_seq: Some(evidence_seq),
                consumed_input,
                consumed_context_free_independent_input,
                consumed_verified_input,
                deferred_context_generations: 0,
                result_rebase: None,
            })),
            config,
            result_reader: Arc::new(tokio::sync::Mutex::new(result_reader)),
        };
        let published = handoff.published_context_artifacts(&record, &handoff.config)?;
        handoff.state.try_lock()
            .map_err(|_| "failed provider source handoff state is already held".to_string())?
            .context_artifacts = published;
        Ok(handoff)
    }
}

impl ProviderOutputHandoff {
    pub(super) fn validate_refused_source_handoff(
        &self,
        record: &RunRecord,
        pending: &PendingHandoff,
    ) -> Result<(), String> {
        if !self.failed_source || !pending.terminal || pending.source_without_context
            || pending.source_movement_seq != self.source_movement_seq || pending.items.is_empty()
        {
            return Err("failed source cannot supersede this pending handoff".into());
        }
        if !self.pending_source_fully_refused(record, pending)?
            && !self.pending_source_unsubmitted(record, pending)? {
            return Err("failed source pending handoff has no exact refusal for every artifact".into());
        }
        Ok(())
    }

    pub(super) fn pending_source_unsubmitted(
        &self, record: &RunRecord, pending: &PendingHandoff,
    ) -> Result<bool, String> {
        if !pending.terminal || !pending.dispatch_fenced || pending.source_without_context
            || pending.withdrawal.is_some() || pending.replacement.is_some()
            || pending.deferred_publication
        { return Ok(false); }
        validate_pending_items_are_active(record, &self.config.provider_interface, pending)?;
        if !pending.standing.is_empty() {
            let all = pending.with_standing_items();
            if self.pending_handoff_evidence(&all)?.iter().take(pending.standing.len()).any(Option::is_none) {
                return Ok(false);
            }
        }
        for item in &pending.items {
            if !self.config.checkpoints.iter().any(|template| {
                template.role == RunAuthoringCheckpointRole::ProviderArtifactOutput
                    && template.result_receiving.is_empty()
                    && pending_output_matches_template(item, template)
            }) { return Ok(false); }
            if source_withdrawal::dispatch_path(&self.ctx, record, item)
                .try_exists().map_err(|error| error.to_string())?
                || self.pending_output_has_submission(pending, item)?
            { return Ok(false); }
        }
        Ok(true)
    }

    // A failure sent through the standing provider route can predate a checked
    // failure write (for example, capture failed before any hand-in). It closes
    // only an exact, fenced, never-submitted final projection. A native tool
    // request, a dispatched result or an ambiguous outcome cannot use this proof.
    pub(super) fn unsubmitted_source_failure_before(
        &self, pending: &PendingHandoff, before_seq: u64,
    ) -> Result<bool, String> {
        if !pending.terminal || !pending.dispatch_fenced || pending.source_without_context
            || pending.deferred_publication || pending.source_movement_seq >= before_seq
        { return Ok(false); }
        for item in &pending.items {
            if self.pending_output_has_submission(pending, item)? { return Ok(false); }
        }
        let limits = self.ctx.required_host_io_limits()?;
        let evidence = CheckedRunEvidence {
            movements_path: &self.ctx.movements_path, ledger_index_path: &self.ctx.ledger_index_path,
            blobs_dir: &self.ctx.blobs_dir, max_control_frame_bytes: limits.max_control_frame_bytes,
            max_manifest_bytes: limits.max_manifest_bytes, max_payload_bytes: limits.max_payload_bytes,
        };
        let mut reader = FileCheckedRunReader::open(&evidence, Some(pending.source_movement_seq))?;
        while let Some(row) = reader.movement(reader.next_seq)? {
            if row.seq.0 >= before_seq { break; }
            if row.seq.0 <= pending.source_movement_seq || row.kind != MovementKind::InterfaceArrival
                || row.from != Endpoint::Interface(self.config.provider_interface.clone())
                || row.to != Endpoint::Interface(self.config.worker.clone())
                || row.run() != Some(&self.config.run)
            { continue; }
            if row.invocation_metadata.as_ref().and_then(|metadata| metadata.invocation())
                .is_none_or(|metadata| metadata.verification_failure.is_some()) { continue; }
            let Some(reference) = row.artifact.as_ref().filter(|reference| reference.id.as_str().starts_with("turn.error:")) else { continue; };
            let (manifest, payload) = read_pinned_artifact(&self.ctx, reference)?;
            if manifest.label != "turn.error" { continue; }
            let value: serde_json::Value = serde_json::from_slice(&payload).map_err(|error| error.to_string())?;
            if value.get("will_retry").and_then(serde_json::Value::as_bool) != Some(false)
                || value.pointer("/additional_details/source_movement_seq").and_then(serde_json::Value::as_u64) != Some(pending.source_movement_seq)
            { continue; }
            let hash = row.runtime().and_then(|runtime| runtime.run_record_hash.as_deref())
                .and_then(ContentHash::from_hex).ok_or("source failure has no exact pinned run record")?;
            let (_, record) = pinned_run_record(&self.ctx, &hash, "unsubmitted source failure")?;
            if record.id != self.config.run || record.worker != self.config.worker
                || persisted_pending_handoff(&record, &self.config.provider_interface)?.as_ref() != Some(pending)
            { continue; }
            if self.pending_source_unsubmitted(&record, pending)? { return Ok(true); }
        }
        Ok(false)
    }

    pub(super) fn pending_source_fully_refused(
        &self,
        record: &RunRecord,
        pending: &PendingHandoff,
    ) -> Result<bool, String> {
        validate_pending_items_are_active(record, &self.config.provider_interface, pending)?;
        let references = pending.items.iter().map(|item| {
            load_pending_artifact(&self.var_dir, item).map(|(_, reference)| reference)
        }).collect::<Result<Vec<_>, _>>()?;
        let limits = self.ctx.required_host_io_limits()?;
        let evidence = CheckedRunEvidence {
            movements_path: &self.ctx.movements_path,
            ledger_index_path: &self.ctx.ledger_index_path,
            blobs_dir: &self.ctx.blobs_dir,
            max_control_frame_bytes: limits.max_control_frame_bytes,
            max_manifest_bytes: limits.max_manifest_bytes,
            max_payload_bytes: limits.max_payload_bytes,
        };
        let mut reader = FileCheckedRunReader::open(&evidence, Some(self.source_movement_seq))?;
        let mut refused = BTreeSet::new();
        while let Some(movement) = reader.movement(reader.next_seq)? {
            if movement.seq.0 <= self.source_movement_seq || movement.run() != Some(&record.id) {
                continue;
            }
            let Some(index) = references.iter().position(|reference| movement.artifact.as_ref() == Some(reference)) else { continue; };
            if movement.from != Endpoint::Interface(self.config.provider_interface.clone()) { continue; }
            let failed_verification = movement.invocation_metadata.as_ref().and_then(|metadata| metadata.invocation())
                .is_some_and(|metadata| metadata.verification_failure.is_some());
            if movement.kind == MovementKind::InterfaceArrival && failed_verification { continue; }
            if matches!(movement.kind, MovementKind::InterfaceArrival | MovementKind::InterfaceOutbound) {
                return Ok(false);
            }
            let refusal = match movement.kind {
                MovementKind::DeliveryRefused => true,
                MovementKind::VerificationFailed if failed_verification => false,
                _ => continue,
            };
            let runtime = movement.runtime().ok_or("pending refusal has no exact runtime coordinate")?;
            let item = &pending.items[index];
            if runtime.checkpoint != item.checkpoint.id || runtime.epoch != item.checkpoint.epoch { continue; }
            if runtime.interface != self.config.provider_interface || runtime.worker != record.worker
                || movement.to != Endpoint::Interface(record.worker.clone())
                || (refusal && !matches!(movement.invocation_metadata.as_ref().and_then(|metadata| metadata.delivery_verdict()), Some(DeliveryVerdict::Refused { .. })))
            {
                return Err("pending refusal changed its exact interface or run scope".into());
            }
            if movement.invocation_metadata.as_ref().and_then(|metadata| metadata.delivery_verdict())
                .is_some_and(DeliveryVerdict::awaits_current_source)
            { continue; }
            let hash = runtime.run_record_hash.as_deref().and_then(ContentHash::from_hex)
                .ok_or("pending refusal has no pinned run snapshot")?;
            let bytes = reader.snapshot(&hash)?;
            if ContentHash::of_bytes(&bytes) != hash { return Err("pending refusal snapshot integrity mismatch".into()); }
            let snapshot = RunRecord::from_bytes(&bytes).map_err(|error| error.to_string())?;
            if snapshot.id != record.id || snapshot.worker != record.worker || snapshot.ee != runtime.ee
                || persisted_pending_handoff(&snapshot, &self.config.provider_interface)?.as_ref() != Some(pending)
            {
                return Err("pending refusal does not prove the current exact handoff".into());
            }
            validate_pending_items_are_active(&snapshot, &self.config.provider_interface, pending)?;
            refused.insert(index);
        }
        Ok(!pending.items.is_empty() && refused.len() == pending.items.len())
    }

    pub async fn hand_failed_source(
        &self,
        emission: ProviderEmission,
    ) -> Result<ProviderHandoffOutcome, FailedSourceHandoffError> {
        use FailedSourceHandoffError::{Interrupted, Unavailable};
        if !self.failed_source {
            return Err(Unavailable(
                "provider handoff was not formed for a failed source".to_string(),
            ));
        }
        {
            let state = self.state.lock().await;
            let record = RunRecord::from_bytes(&state.run_record_bytes)
                .map_err(|error| Unavailable(error.to_string()))?;
            let pending = persisted_pending_handoff(&record, &self.config.provider_interface)
                .map_err(Unavailable)?
                .filter(|pending| pending.source_movement_seq == self.source_movement_seq);
            if let Some(pending) = pending {
                if pending.terminal && pending.source_without_context {
                    return self
                        .hand_pending_items(&pending, &state.run_record_bytes)
                        .await
                        .map_err(Interrupted)?
                        .into_iter()
                        .next()
                        .ok_or_else(|| {
                            Interrupted("failed provider source pending handoff is empty".to_string())
                        });
                }
                self.validate_refused_source_handoff(&record, &pending).map_err(Unavailable)?;
            }
            self.recover_authoring_attempt(&record, &state.run_record_bytes).map_err(Unavailable)?;
            let identity =
                artifact_identity(&emission.artifact, &self.var_dir).map_err(Unavailable)?;
            let output_checkpoint = self
                .config
                .output_checkpoint(
                    &ProviderHandoffRoute::ClientProjection,
                    &self.granted_tools,
                    &self.granted_interfaces,
                    &identity.label,
                )
                .map_err(Unavailable)?;
            self.author_run_with_bindings(
                &state.run_record_bytes,
                &state.context_artifacts,
                &[PreparedHandoff {
                    identity,
                    output_checkpoint,
                    verified_inputs: Vec::new(),
                }],
                state.consumed_input,
                state.consumed_context_free_independent_input.as_ref(),
                state.consumed_verified_input,
                true,
                &[],
            )
            .map_err(Unavailable)?;
        }
        self.hand_final(emission).await.map_err(Interrupted)
    }
}
