use super::*;

#[derive(Clone)]
pub struct RunAuthoringResultIdentity {
    pub(crate) provider_interface: InterfaceId,
    pub(crate) profile: RunAuthoringProfile,
    pub(crate) context_output_labels: BTreeSet<String>,
    pub(crate) session_snapshot_labels: BTreeSet<String>,
}

impl RunAuthoringResultIdentity {
    pub fn from_descriptor(descriptor: &InterfaceDescriptor) -> Result<Self, String> {
        let profile = run_authoring_profile(descriptor)?;
        let (context_output_labels, session_snapshot_labels) =
            provider_context_label_sets(descriptor)?;
        profile.validate(&descriptor.id)?;
        Ok(Self {
            provider_interface: descriptor.id.clone(),
            profile,
            context_output_labels,
            session_snapshot_labels,
        })
    }

    pub fn artifact_label(&self) -> &str {
        &self.profile.file_write_result_label
    }

    pub async fn consume_if_matches(
        &self,
        ctx: &InterfaceLiftContext,
        session: &ProviderHandoffSession,
        movement: &Movement,
        envelope: &InterfaceInvocationEnvelope,
        artifact_label: &str,
    ) -> Result<bool, String> {
        if ctx.interface != self.provider_interface {
            return Err(format!(
                "run authoring result identity belongs to '{}', lifted interface is '{}'",
                self.provider_interface.as_str(),
                ctx.interface.as_str()
            ));
        }
        if artifact_label == self.artifact_label() {
            session.validate_context(ctx)?;
            let payload = envelope.read_artifact_payload().map_err(|error| {
                format!(
                    "provider movement {} authoring-result payload cannot be read: {error}",
                    movement.seq.0
                )
            })?;
            let pinned_run_record = ctx.pinned_run_record_bytes(envelope)?;
            let consumed = async {
                let record = RunRecord::from_bytes(&pinned_run_record).map_err(|error| {
                    format!("provider authoring result pinned run record is malformed: {error}")
                })?;
                if writer_run::consume_result(ctx, &session.writer_history, &self.profile, movement, &record)? { return Ok(()); }
                let config = self.config_for_record(&record)?;
                let result: FileWriteResultEnvelope =
                    serde_json::from_slice(&payload).map_err(|error| {
                        format!("provider authoring result payload is malformed: {error}")
                    })?;
                if !result.ok && result.error.as_ref().is_some_and(|error| error.details.is_none()) {
                    return authoring_retry::validate_rejected_result(
                        ctx, &config, movement, envelope, &record, &result,
                    );
                }
                if let Some(failed) = file_write_failure_result(&result, &config, movement.seq.0)? {
                    let authoring_result =
                        config.checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringResult)?;
                    validate_authoring_failure_movement(
                        &self.provider_interface,
                        movement,
                        envelope,
                        &record,
                        &config,
                        authoring_result,
                    )?;
                    let session = ProviderHandoffSession::new(ctx)?;
                    hand_validated_authoring_failure(
                        ctx,
                        &session,
                        &record,
                        &pinned_run_record,
                        &config,
                        &failed.failure,
                        movement.seq.0,
                        Some(&failed.correlation_id),
                        &self.context_output_labels,
                        &self.session_snapshot_labels,
                    )
                    .await
                } else {
                    self.validate(movement, envelope, &pinned_run_record, &payload)
                }
            }
            .await;
            if let Err(error) = consumed {
                ctx.hand_admitted_failure(
                    movement.seq.0,
                    "malformed_provider_authoring_result",
                    serde_json::json!({ "error": error }),
                )
                .await?;
            }
            return Ok(true);
        }
        if !artifact_label.ends_with(".failure") {
            return Ok(false);
        }
        let pinned_run_record = ctx.pinned_run_record_bytes(envelope)?;
        let record = RunRecord::from_bytes(&pinned_run_record).map_err(|error| {
            format!("provider authoring failure pinned run record is malformed: {error}")
        })?;
        let config = self.config_for_record(&record)?;
        let authoring_result =
            config.checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringResult)?;
        let expected_label = format!("{}.failure", authoring_result.interface().as_str());
        if artifact_label != expected_label {
            return Ok(false);
        }
        let payload = envelope.read_artifact_payload().map_err(|error| {
            format!(
                "provider movement {} authoring-failure payload cannot be read: {error}",
                movement.seq.0
            )
        })?;
        match self.validate_failure(
            movement,
            envelope,
            &record,
            &config,
            authoring_result,
            &payload,
        ) {
            Ok(failure) => {
                let session = ProviderHandoffSession::new(ctx)?;
                hand_validated_authoring_failure(
                    ctx,
                    &session,
                    &record,
                    &pinned_run_record,
                    &config,
                    &failure,
                    movement.seq.0,
                    None,
                    &self.context_output_labels,
                    &self.session_snapshot_labels,
                )
                .await?;
            }
            Err(error) => {
                ctx.hand_admitted_failure(
                    movement.seq.0,
                    "malformed_provider_authoring_failure",
                    serde_json::json!({ "error": error }),
                )
                .await?;
            }
        }
        Ok(true)
    }

    pub fn validate(
        &self,
        movement: &Movement,
        envelope: &InterfaceInvocationEnvelope,
        pinned_run_record: &[u8],
        payload: &[u8],
    ) -> Result<(), String> {
        let runtime = envelope.runtime().map_err(|error| error.to_string())?;
        let record = RunRecord::from_bytes(pinned_run_record).map_err(|error| {
            format!("provider authoring result pinned run record is malformed: {error}")
        })?;
        if record.id != runtime.run || record.worker != runtime.worker {
            return Err("provider authoring result pinned the wrong run topology".to_string());
        }
        let config = self.config_for_record(&record)?;
        config.validate(&self.provider_interface, &record.id)?;
        let authoring_result =
            config.checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringResult)?;
        let authoring_result_checkpoint = active_checkpoint_for_role(
            &record,
            &config,
            RunAuthoringCheckpointRole::AuthoringResult,
        )?;
        if movement.kind != MovementKind::InterfaceOutbound
            || movement.from != Endpoint::Interface(authoring_result.interface().clone())
            || movement.to != Endpoint::Interface(self.provider_interface.clone())
            || movement.artifact.as_ref() != Some(&envelope.artifact)
        {
            return Err("provider authoring result has invalid movement provenance".to_string());
        }
        if runtime.interface != self.provider_interface
            || runtime.run != record.id
            || runtime.worker != record.worker
            || runtime.checkpoint != authoring_result_checkpoint.id
            || runtime.epoch != 0
        {
            return Err("provider authoring result has invalid runtime coordinate".to_string());
        }
        let metadata = envelope.metadata().map_err(|error| error.to_string())?;
        if metadata.outbound_interface_bindings
            != authoring_result
                .outbound_interfaces()
                .cloned()
                .collect::<Vec<_>>()
            || metadata.checkpoint_binding_policy.as_ref()
                != Some(&authoring_result.binding_policy_snapshot())
            || metadata.required_verified_artifacts != authoring_result.required_verified_artifacts
            || !artifact_refs_match_bindings(
                &metadata.artifact_binding_refs,
                &authoring_result_checkpoint.artifact_bindings,
            )
        {
            return Err(
                "provider authoring result metadata does not match its authored checkpoint"
                    .to_string(),
            );
        }
        let result: FileWriteResultEnvelope = serde_json::from_slice(payload)
            .map_err(|error| format!("provider authoring result payload is malformed: {error}"))?;
        if result.label != config.file_write_result_label || result.tool != config.file_write_tool {
            return Err(
                "provider authoring result payload names the wrong label or tool".to_string(),
            );
        }
        let correlation_id = result
            .correlation_id
            .as_deref()
            .ok_or_else(|| "provider authoring result payload has no correlation_id".to_string())?;
        let generation = correlation_generation(&record.id, correlation_id)?;
        let provider_state = record
            .interface_state
            .get(&self.provider_interface)
            .ok_or_else(|| {
                "provider authoring result pinned a run without provider state".to_string()
            })?;
        if optional_u64(provider_state, "authoring_generation")? != generation {
            return Err(
                "provider authoring result correlation does not match the run generation"
                    .to_string(),
            );
        }
        let pending = pending_handoff(&record, &self.provider_interface)?.ok_or_else(|| {
            "provider authoring result pinned a run without pending handoff evidence".to_string()
        })?;
        pending.items.first().ok_or_else(|| {
            "provider authoring result pinned an empty pending handoff batch".to_string()
        })?;
        validate_pending_items_are_active(&record, &self.provider_interface, &pending)?;
        let frontier = frontier_checkpoint_ids(provider_state)?;
        if !frontier.contains(runtime.checkpoint.as_str())
            || !record.active_checkpoints.contains(&runtime.checkpoint)
        {
            return Err(
                "provider authoring result arrived outside the active provider-owned frontier"
                    .to_string(),
            );
        }
        let checkpoint = record.checkpoint(&runtime.checkpoint).ok_or_else(|| {
            "provider authoring result checkpoint is absent from the pinned run".to_string()
        })?;
        if checkpoint.epoch.0 != runtime.epoch {
            return Err(
                "provider authoring result checkpoint epoch differs from its runtime coordinate"
                    .to_string(),
            );
        }
        if !checkpoint_record_matches_template(checkpoint, authoring_result, &record.id) {
            return Err(
                "provider authoring result checkpoint topology differs from its descriptor template"
                    .to_string(),
            );
        }
        Ok(())
    }

    pub(crate) fn config_for_record(&self, record: &RunRecord) -> Result<RunAuthoringConfig, String> {
        config_from_persisted_profile(record, &self.provider_interface, &self.profile)
    }

    pub(crate) fn validate_failure(
        &self,
        movement: &Movement,
        envelope: &InterfaceInvocationEnvelope,
        record: &RunRecord,
        config: &RunAuthoringConfig,
        authoring_result: &RunAuthoringCheckpoint,
        payload: &[u8],
    ) -> Result<InterfaceFailureEnvelope, String> {
        validate_authoring_failure_delivery(
            &self.provider_interface,
            movement,
            envelope,
            record,
            config,
            authoring_result,
            payload,
        )
    }
}

pub(crate) fn validate_authoring_failure_delivery(
    provider_interface: &InterfaceId,
    movement: &Movement,
    envelope: &InterfaceInvocationEnvelope,
    record: &RunRecord,
    config: &RunAuthoringConfig,
    authoring_result: &RunAuthoringCheckpoint,
    payload: &[u8],
) -> Result<InterfaceFailureEnvelope, String> {
    let failure: InterfaceFailureEnvelope = serde_json::from_slice(payload)
        .map_err(|error| format!("provider authoring failure payload is malformed: {error}"))?;
    validate_authoring_failure_movement(
        provider_interface,
        movement,
        envelope,
        record,
        config,
        authoring_result,
    )?;
    validate_authoring_failure_fields(&failure, movement.seq.0)?;
    Ok(failure)
}

pub(crate) fn validate_authoring_failure_movement(
    provider_interface: &InterfaceId,
    movement: &Movement,
    envelope: &InterfaceInvocationEnvelope,
    record: &RunRecord,
    config: &RunAuthoringConfig,
    authoring_result: &RunAuthoringCheckpoint,
) -> Result<(), String> {
    let runtime = envelope.runtime().map_err(|error| error.to_string())?;
    if record.id != runtime.run || record.worker != runtime.worker {
        return Err("provider authoring failure pinned the wrong run topology".to_string());
    }
    config.validate(provider_interface, &record.id)?;
    if movement.kind != MovementKind::InterfaceOutbound
        || movement.from != Endpoint::Interface(authoring_result.interface().clone())
        || movement.to != Endpoint::Interface(provider_interface.clone())
        || movement.artifact.as_ref() != Some(&envelope.artifact)
    {
        return Err("provider authoring failure has invalid movement provenance".to_string());
    }
    if runtime.interface != *provider_interface
        || runtime.run != record.id
        || runtime.worker != record.worker
        || runtime.checkpoint
            != active_checkpoint_for_role(
                record,
                config,
                RunAuthoringCheckpointRole::AuthoringResult,
            )?
            .id
            .clone()
        || runtime.epoch != 0
    {
        return Err("provider authoring failure has invalid runtime coordinate".to_string());
    }
    let metadata = envelope.metadata().map_err(|error| error.to_string())?;
    if metadata.outbound_interface_bindings
        != authoring_result
            .outbound_interfaces()
            .cloned()
            .collect::<Vec<_>>()
        || metadata.checkpoint_binding_policy.as_ref()
            != Some(&authoring_result.binding_policy_snapshot())
        || metadata.required_verified_artifacts != authoring_result.required_verified_artifacts
    {
        return Err(
            "provider authoring failure metadata does not match its authored checkpoint"
                .to_string(),
        );
    }
    let frontier =
        frontier_checkpoint_ids(record.interface_state.get(provider_interface).ok_or_else(
            || "provider authoring failure pinned a run without provider state".to_string(),
        )?)?;
    if !frontier.contains(runtime.checkpoint.as_str())
        || !record.active_checkpoints.contains(&runtime.checkpoint)
    {
        return Err(
            "provider authoring failure arrived outside the active provider-owned frontier"
                .to_string(),
        );
    }
    let checkpoint = record.checkpoint(&runtime.checkpoint).ok_or_else(|| {
        "provider authoring failure checkpoint is absent from the pinned run".to_string()
    })?;
    if checkpoint.epoch.0 != runtime.epoch
        || !checkpoint_record_matches_template(checkpoint, authoring_result, &record.id)
        || !artifact_refs_match_bindings(
            &metadata.artifact_binding_refs,
            &checkpoint.artifact_bindings,
        )
    {
        return Err(
            "provider authoring failure checkpoint differs from its descriptor template"
                .to_string(),
        );
    }
    Ok(())
}

pub(crate) fn validate_authoring_failure_fields(
    failure: &InterfaceFailureEnvelope,
    movement_seq: u64,
) -> Result<(), String> {
    if failure.failure_class.trim().is_empty()
        || failure.failure_class.trim() != failure.failure_class
        || failure.source_movement_seq >= movement_seq
    {
        return Err(
            "provider authoring failure has invalid class or source movement coordinate"
                .to_string(),
        );
    }
    Ok(())
}

pub(crate) fn file_write_failure_result(
    result: &FileWriteResultEnvelope,
    config: &RunAuthoringConfig,
    movement_seq: u64,
) -> Result<Option<FileWriteFailureResult>, String> {
    if result.label != config.file_write_result_label || result.tool != config.file_write_tool {
        return Err(format!(
            "provider run '{}' authoring result names label='{}' tool='{}'",
            config.run.as_str(),
            result.label,
            result.tool
        ));
    }
    match (result.ok, result.error.as_ref()) {
        (true, None) => Ok(None),
        (false, Some(error)) if result.body.is_none() => {
            let correlation_id = result.correlation_id.clone().ok_or_else(|| {
                "provider failed authoring result has no correlation_id".to_string()
            })?;
            let details = error.details.as_ref().ok_or_else(|| {
                "provider failed authoring result error has no structured details".to_string()
            })?;
            let source_movement_seq = details
                .get("source_movement_seq")
                .and_then(serde_json::Value::as_u64)
                .ok_or_else(|| {
                    "provider failed authoring result error has no source_movement_seq".to_string()
                })?;
            let failure = InterfaceFailureEnvelope {
                source_movement_seq,
                failure_class: error.code.clone(),
                details: details.get("failure_details").cloned().ok_or_else(|| {
                    "provider failed authoring result error has no failure_details".to_string()
                })?,
            };
            validate_authoring_failure_fields(&failure, movement_seq)?;
            Ok(Some(FileWriteFailureResult {
                correlation_id,
                failure,
            }))
        }
        (false, Some(_)) => {
            Err("provider failed authoring result unexpectedly carries a body".to_string())
        }
        (false, None) => Err("provider failed authoring result has no error detail".to_string()),
        (true, Some(_)) => Err("provider successful authoring result carries an error".to_string()),
    }
}

pub(crate) fn active_checkpoint_for_role<'a>(
    record: &'a RunRecord,
    config: &RunAuthoringConfig,
    role: RunAuthoringCheckpointRole,
) -> Result<&'a octopus_run_record::CheckpointRecord, String> {
    let template = config.checkpoint_for_role(role)?;
    active_checkpoint_for_template(record, config, template)
}

pub(crate) fn active_checkpoint_for_template<'a>(
    record: &'a RunRecord,
    config: &RunAuthoringConfig,
    template: &RunAuthoringCheckpoint,
) -> Result<&'a octopus_run_record::CheckpointRecord, String> {
    let provider_state = record
        .interface_state
        .get(&config.provider_interface)
        .ok_or_else(|| {
            format!(
                "provider run '{}' has no interface_state for '{}'",
                record.id.as_str(),
                config.provider_interface.as_str()
            )
        })?;
    let frontier = frontier_checkpoint_ids(provider_state)?;
    let mut matches = frontier.iter().filter_map(|checkpoint_id| {
        let checkpoint_id = CheckpointId::new(checkpoint_id);
        record
            .active_checkpoints
            .contains(&checkpoint_id)
            .then(|| record.checkpoint(&checkpoint_id))
            .flatten()
            .filter(|checkpoint| {
                checkpoint_record_matches_template(checkpoint, template, &record.id)
            })
    });
    let checkpoint = matches.next().ok_or_else(|| {
        format!(
            "provider run '{}' frontier has no {:?} checkpoint",
            record.id.as_str(),
            template.name
        )
    })?;
    if matches.next().is_some() {
        return Err(format!(
            "provider run '{}' frontier has repeated {:?} checkpoints",
            record.id.as_str(),
            template.name
        ));
    }
    Ok(checkpoint)
}

pub(crate) fn validate_authoring_failure_source(
    validation: AuthoringFailureSourceValidation<'_>,
) -> Result<ValidatedAuthoringFailureSource, String> {
    let AuthoringFailureSourceValidation {
        ctx,
        record,
        record_bytes,
        config,
        failure,
        failure_seq,
        after_seq,
    } = validation;
    let hand_in_root = ctx
        .root_slot("interface_hand_in_root")?
        .join(config.authoring_source_interface.as_str());
    let attempt = persisted_authoring_attempt(
        ctx,
        &hand_in_root,
        record,
        record_bytes,
        config,
        true,
    )?
        .ok_or_else(|| {
            format!(
                "provider authoring failure movement {failure_seq} has no exact private authoring attempt"
            )
        })?;
    if after_seq.is_some_and(|source| attempt.provider_source_movement_seq != source)
        || failure.source_movement_seq <= attempt.provider_source_movement_seq
        || failure.source_movement_seq >= failure_seq
    {
        return Err(format!(
            "provider authoring failure movement {failure_seq} has invalid source movement {} for provider movement {}",
            failure.source_movement_seq, attempt.provider_source_movement_seq
        ));
    }
    Ok(ValidatedAuthoringFailureSource {
        provider_source_movement_seq: attempt.provider_source_movement_seq,
        correlation_id: attempt.authored.correlation_id,
    })
}

pub(crate) async fn hand_validated_authoring_failure(
    ctx: &InterfaceLiftContext,
    session: &ProviderHandoffSession,
    record: &RunRecord,
    record_bytes: &[u8],
    config: &RunAuthoringConfig,
    failure: &InterfaceFailureEnvelope,
    failure_seq: u64,
    result_correlation_id: Option<&str>,
    context_output_labels: &BTreeSet<String>,
    session_snapshot_labels: &BTreeSet<String>,
) -> Result<(), String> {
    if completed_authoring_failure_duplicate(
        ctx,
        session,
        record,
        record_bytes,
        config,
        failure,
        failure_seq,
    )? {
        return Ok(());
    }
    let validated = validate_authoring_failure_source(AuthoringFailureSourceValidation {
        ctx,
        record,
        record_bytes,
        config,
        failure,
        failure_seq,
        after_seq: None,
    })?;
    if result_correlation_id
        .is_some_and(|correlation| correlation != validated.correlation_id.as_str())
    {
        return Err("provider failed authoring result names the wrong correlation_id".to_string());
    }
    let (session_context, session_snapshot_head) = recover_source_session_context(
        ctx,
        session,
        record,
        config,
        validated.provider_source_movement_seq,
        context_output_labels,
        session_snapshot_labels,
    )?;
    hand_authoring_terminal_failure(
        ctx,
        session,
        record,
        config,
        failure_seq,
        failure,
        &session_context,
        session_snapshot_head.as_ref(),
        session_snapshot_labels,
    )
    .await?;
    Ok(())
}

pub(crate) fn validate_authoring_attempt_source(
    validation: AuthoringAttemptSourceValidation<'_>,
) -> Result<ValidatedAuthoringAttempt, String> {
    let AuthoringAttemptSourceValidation {
        ctx,
        record,
        record_bytes,
        config,
        call,
        manifest_path,
    } = validation;
    let authoring_result =
        config.checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringResult)?;
    let record_hash = ContentHash::of_bytes(record_bytes).to_hex();
    let current_generation = optional_u64(
        record
            .interface_state
            .get(&config.provider_interface)
            .ok_or_else(|| {
                "provider authoring failure source run has no provider state".to_string()
            })?,
        "authoring_generation",
    )?;
    let expected_generation = current_generation
        .checked_add(1)
        .ok_or_else(|| "provider authoring generation overflowed".to_string())?;
    let call_generation = correlation_generation(&record.id, &call.correlation_id)?;
    let expected_path = ctx
        .root_slot("run_records_root")?
        .canonicalize()
        .map_err(|error| format!("provider run-record root cannot be canonicalized: {error}"))?
        .join(format!("{}.json", record.id.as_str()));
    if call.tool != config.file_write_tool
        || call_generation != expected_generation
        || call.arguments.path != expected_path
        || call.arguments.expected_before_hash != record_hash
    {
        return Err(
            "provider authoring failure source call differs from the next exact file-write generation"
                .to_string(),
        );
    }
    let content = apply_run_json_patch(
        record_bytes,
        &call.arguments.patch,
        &call.arguments.expected_after_hash, call.arguments.json_format
    )?;
    let candidate = RunRecord::from_bytes(content.as_bytes()).map_err(|error| {
        format!("provider authoring failure source call carries a malformed run: {error}")
    })?;
    if !result_inputs::saved_definition_transition(record, &candidate, config)? {
        source_settlement::validate_transition(record, &candidate, config)?;
        source_withdrawal::validate_saved_attempt(ctx, record_bytes, content.as_bytes(), config)?;
        rejected_final::validate_saved_attempt(ctx, record_bytes, content.as_bytes(), config)?;
    }
    if candidate.id != record.id || candidate.worker != record.worker {
        return Err(
            "provider authoring failure source call carries the wrong run topology".to_string(),
        );
    }
    let candidate_generation = optional_u64(
        candidate
            .interface_state
            .get(&config.provider_interface)
            .ok_or_else(|| {
                "provider authoring failure source candidate has no provider state".to_string()
            })?,
        "authoring_generation",
    )?;
    if candidate_generation != expected_generation {
        return Err(
            "provider authoring failure source candidate has the wrong generation".to_string(),
        );
    }
    let pending = pending_handoff(&candidate, &config.provider_interface)?
        .ok_or_else(|| "provider authoring source candidate has no pending handoff".to_string())?;
    validate_pending_handoff_delivery_isolation(&candidate, &pending)?;
    if pending.items.is_empty() {
        return Err("provider authoring source candidate has invalid pending evidence".to_string());
    }
    validate_pending_items_are_active(&candidate, &config.provider_interface, &pending)?;
    let candidate_config = result_inputs::saved_attempt_config(&candidate, config)?;
    validate_bound_run_authoring_topology(&candidate, &candidate_config, None)?;
    result_inputs::validate_saved(ctx, record, &candidate, config)?;
    let authoring_result_checkpoint = active_checkpoint_for_role(
        &candidate,
        &candidate_config,
        RunAuthoringCheckpointRole::AuthoringResult,
    )?
    .id
    .clone();
    let after_hash = ContentHash::of_bytes(content.as_bytes()).to_hex();
    Ok(ValidatedAuthoringAttempt {
        manifest_path,
        provider_source_movement_seq: result_inputs::definition_source(record, &candidate, config)?
            .unwrap_or(pending.source_movement_seq),
        authored: AuthoredRun {
            json_format: call.arguments.json_format,
            content,
            patch: call.arguments.patch,
            before_hash: record_hash,
            after_hash,
            correlation_id: call.correlation_id,
            authoring_result_checkpoint,
            authoring_result_interface: authoring_result.interface().clone(),
            pending_handoff: pending,
        },
    })
}
