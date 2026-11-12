use super::*;

pub fn checked_failed_run_write(
    evidence: &CheckedRunEvidence<'_>, writer: &CheckedRunWriter,
    edit: &CheckedRunEdit, call_seq: u64, coauthors: Vec<CheckedRunWriter>,
) -> Result<Option<Movement>, CheckedRunError> {
    failed_run_write_evidence(evidence, writer, edit, call_seq, coauthors)
        .map(|proof| proof.map(|(movement, _)| movement))
}

fn failed_run_write_evidence(
    evidence: &CheckedRunEvidence<'_>, writer: &CheckedRunWriter,
    edit: &CheckedRunEdit, call_seq: u64, mut coauthors: Vec<CheckedRunWriter>,
) -> Result<Option<(Movement, Vec<u8>)>, CheckedRunError> {
    let mut reader = FileCheckedRunReader::open(evidence, call_seq.checked_sub(1))?;
    let call = loop {
        let row = reader.movement(reader.next_seq)?.ok_or("failed write has no admitted call")?;
        if row.seq.0 == call_seq { break row; }
        if row.seq.0 > call_seq { return Err("failed write has no exact admitted call".into()); }
    };
    let metadata = call.invocation_metadata.as_ref().and_then(|value| value.invocation())
        .ok_or("failed write call has no invocation metadata")?;
    let runtime = metadata.runtime.as_ref().ok_or("failed write call has no run coordinate")?;
    let intended = RunRecord::from_bytes(edit.content.as_bytes()).map_err(|error| error.to_string())?;
    let (manifest, payload) = reader.artifact(call.artifact.as_ref().ok_or("failed write call has no artifact")?)?;
    let envelope: FileWriteCallEnvelope = serde_json::from_slice(&payload).map_err(|error| error.to_string())?;
    if call.kind != MovementKind::InterfaceOutbound || call.from != Endpoint::Interface(writer.source.clone())
        || call.to != Endpoint::Interface(writer.door.clone()) || runtime.interface != writer.door
        || runtime.run != intended.id || runtime.worker != intended.worker
        || manifest.label != writer.call_label || envelope.tool != writer.tool || envelope.correlation_id != edit.correlation_id
        || envelope.arguments.path != edit.path || envelope.arguments.patch != edit.patch
        || envelope.arguments.json_format != edit.json_format
        || envelope.arguments.expected_before_hash != edit.before_hash || envelope.arguments.expected_after_hash != edit.after_hash
        || !metadata.grant.tools.contains(&writer.tool) || metadata.grant.hard_blocked_tools.contains(&writer.tool)
        || !metadata.grant.interfaces.contains(&writer.door.to_string()) || metadata.verification_failure.is_some() {
        return Err("failed write differs from its exact admitted call and grant".into());
    }
    loop {
        let Some(movement) = reader.movement(reader.next_seq)? else { return Ok(None); };
        if movement.from != Endpoint::Interface(writer.door.clone()) || movement.run() != Some(&intended.id)
            || !matches!(movement.kind, MovementKind::InterfaceOutbound | MovementKind::DeliveryRefused) { continue; }
        let (manifest, payload) = reader.artifact(movement.artifact.as_ref().ok_or("write result has no artifact")?)?;
        if manifest.label != writer.result_label { continue; }
        let result: ToolResultArtifact = serde_json::from_slice(&payload).map_err(|error| error.to_string())?;
        if result.correlation_id.as_deref() != Some(&edit.correlation_id) { continue; }
        if result.tool != writer.tool || result.label != writer.result_label { return Err("failed write result changed its exact tool or label".into()); }
        if result.ok { return Ok(None); }
        if result.body.is_some() || result.error.as_ref().is_none_or(|error| error.code != "tool_execution_failed" || error.details.is_some()) {
            return Err(match result.error.as_ref() {
                Some(error) => format!("write failure does not prove an ordinary rejected file effect: {}: {}", error.code, error.message),
                None => "write failure does not prove an ordinary rejected file effect: it carries no error".to_string(),
            }.into());
        }
        let runtime = movement.runtime().ok_or("failed write result has no run coordinate")?;
        if runtime.worker != intended.worker { return Err("failed write result changed its run worker".into()); }
        let hash = runtime.run_record_hash.as_deref().and_then(ContentHash::from_hex).ok_or("failed write result has no pinned snapshot")?;
        let snapshot = if movement.kind == MovementKind::DeliveryRefused {
            let admitted = call.runtime().and_then(|runtime| runtime.run_record_hash.as_deref())
                .and_then(ContentHash::from_hex).ok_or("failed write call has no admitted preimage")?;
            let before = reader.snapshot(&admitted)?;
            if ContentHash::of_bytes(&before) != admitted {
                return Err("failed write call differs from its exact admitted preimage".into());
            }
            coauthors.push(writer.clone());
            let snapshot = checked_declared_run_snapshot(evidence, &edit.path, &before, call_seq,
                &hash.to_hex(), movement.seq.0, coauthors)?;
            validate_unchanged_admitted_checkpoint(&before, &snapshot, &runtime.checkpoint)?;
            snapshot
        } else {
            reader.snapshot(&hash)?
        };
        if ContentHash::of_bytes(&snapshot) != hash { return Err("failed write result snapshot integrity mismatch".into()); }
        let record = RunRecord::from_bytes(&snapshot).map_err(|error| error.to_string())?;
        let checkpoint = record.checkpoint(&runtime.checkpoint).ok_or("failed write result checkpoint absent")?;
        if record.id != intended.id || record.ee != runtime.ee || record.worker != intended.worker
            || !record.active_checkpoints.contains(&checkpoint.id) || checkpoint.epoch.0 != runtime.epoch
            || checkpoint.inbound_interface() != Some(&writer.door)
            || checkpoint.outbound_interfaces().collect::<Vec<_>>() != [&writer.owner] {
            return Err("failed write result has no exact declared return edge".into());
        }
        if movement.kind == MovementKind::DeliveryRefused {
            if movement.to != Endpoint::Interface(intended.worker.clone()) || runtime.interface != writer.door
                || !matches!(movement.invocation_metadata.as_ref().and_then(|value| value.delivery_verdict()),
                    Some(DeliveryVerdict::Refused { reason: octopus_ledger::RefusedReason::CheckpointSpent, .. })) {
                return Err("failed write refusal is not its exact spent return edge".into());
            }
        } else {
            let metadata = movement.invocation_metadata.as_ref().and_then(|value| value.invocation()).ok_or("failed write result metadata absent")?;
            if movement.to != Endpoint::Interface(writer.owner.clone()) || runtime.interface != writer.owner
                || metadata.verification_failure.is_some()
                || !metadata.checkpoint_admission.as_ref().is_some_and(|value| value.complete && value.artifact_label == writer.result_label) {
                return Err("failed write result is not admitted on its owner edge".into());
            }
        }
        return Ok(Some((movement, snapshot)));
    }
}

pub(super) struct RejectedWrite {
    pub result_seq: u64,
    pub evidence: RejectionEvidence,
}

pub(super) enum RejectionEvidence {
    ToolResult {
        call_seq: u64,
        error: octopus_tool_handlers::ToolResultError,
    },
    Verification(octopus_ledger::VerificationFailureMetadata),
}

impl RejectedWrite {
    pub fn description(&self) -> String {
        match &self.evidence {
            RejectionEvidence::ToolResult { call_seq, error } => {
                format!("call {call_seq}: {}", error.message)
            }
            RejectionEvidence::Verification(failure) => format!("verification: {}", failure.error),
        }
    }
}

pub(super) fn rejected_write(
    ctx: &InterfaceLiftContext,
    config: &RunAuthoringConfig,
    authored: &AuthoredRun,
) -> Result<Option<RejectedWrite>, String> {
    if let Some(result) = rejected_write_through(ctx, config, authored, None)? {
        return Ok(Some(result));
    }
    let writer = CheckedRunWriter {
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
    let edit = CheckedRunEdit {
        json_format: authored.json_format,
        path: ctx
            .root_slot("run_records_root")?
            .canonicalize()
            .map_err(|error| error.to_string())?
            .join(format!("{}.json", config.run)),
        before_hash: authored.before_hash.clone(),
        after_hash: authored.after_hash.clone(),
        correlation_id: authored.correlation_id.clone(),
        content: authored.content.clone(),
        patch: authored.patch.clone(),
    };
    checked_edit_verification_failure(
        ctx,
        &writer,
        &edit,
        authored.pending_handoff.source_movement_seq,
    )?
    .map(|movement| {
        let failure = movement
            .invocation_metadata
            .as_ref()
            .and_then(|m| m.invocation())
            .and_then(|m| m.verification_failure.clone())
            .ok_or("checked verification failure disappeared")?;
        Ok(RejectedWrite {
            result_seq: movement.seq.0,
            evidence: RejectionEvidence::Verification(failure),
        })
    })
    .transpose()
}

fn rejected_write_through(
    ctx: &InterfaceLiftContext,
    config: &RunAuthoringConfig,
    authored: &AuthoredRun,
    through_seq: Option<u64>,
) -> Result<Option<RejectedWrite>, String> {
    let limits = ctx.required_host_io_limits()?;
    let evidence = CheckedRunEvidence {
        movements_path: &ctx.movements_path,
        ledger_index_path: &ctx.ledger_index_path,
        blobs_dir: &ctx.blobs_dir,
        max_control_frame_bytes: limits.max_control_frame_bytes,
        max_manifest_bytes: limits.max_manifest_bytes,
        max_payload_bytes: limits.max_payload_bytes,
    };
    let before_hash = ContentHash::from_hex(&authored.before_hash)
        .ok_or("rejected authoring write has no canonical preimage")?;
    let mut reader =
        FileCheckedRunReader::open(&evidence, Some(authored.pending_handoff.source_movement_seq))?;
    let template = config.checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringResult)?;
    let mut pending: Option<(u64, bool, PathBuf)> = None;
    let mut rejected = None;
    loop {
        let seq = reader.next_seq;
        if through_seq.is_some_and(|end| seq > end) {
            break;
        }
        let Some(movement) = reader.movement(seq)? else {
            break;
        };
        if movement.kind == MovementKind::DeliveryRefused
            && movement.from == Endpoint::Interface(template.interface().clone())
            && movement.run() == Some(&config.run)
        {
            let (manifest, payload) = reader.artifact(movement.artifact.as_ref()
                .ok_or("refused authoring result has no artifact")?)?;
            if manifest.label != config.file_write_result_label { continue; }
            let result: ToolResultArtifact = serde_json::from_slice(&payload).map_err(|error| error.to_string())?;
            if result.correlation_id.as_deref() != Some(&authored.correlation_id) { continue; }
            let Some((call_seq, exact, path)) = pending.take() else { continue; };
            if !exact { continue; }
            let writer = CheckedRunWriter {
                owner: config.provider_interface.clone(), source: config.authoring_source_interface.clone(),
                door: template.interface().clone(), tool: config.file_write_tool.clone(),
                call_label: config.file_write_label.clone(), result_label: config.file_write_result_label.clone(),
            };
            let edit = CheckedRunEdit {
        json_format: authored.json_format,
                path: ctx.root_slot("run_records_root")?.canonicalize().map_err(|error| error.to_string())?
                    .join(format!("{}.json", config.run)),
                before_hash: authored.before_hash.clone(), after_hash: authored.after_hash.clone(),
                correlation_id: authored.correlation_id.clone(), content: authored.content.clone(), patch: authored.patch.clone(),
            };
            let (failed, snapshot) = match failed_run_write_evidence(&evidence, &writer, &edit, call_seq, Vec::new()) {
                Ok(Some(proof)) => proof,
                Ok(None) | Err(CheckedRunError::AwaitingResults { .. }) => return Ok(None),
                Err(error) => return Err(error.into()),
            };
            if path != edit.path || failed != movement {
                return Err("refused authoring result does not prove its exact failed write".into());
            }
            let before = reader.snapshot(&before_hash)?;
            if ContentHash::of_bytes(&before) != before_hash { return Err("refused authoring preimage failed integrity".into()); }
            let original = RunRecord::from_bytes(&before).map_err(|error| error.to_string())?;
            let checkpoint = active_checkpoint_for_role(&original, config, RunAuthoringCheckpointRole::AuthoringResult)?;
            let runtime = movement.runtime().ok_or("refused authoring result has no run coordinate")?;
            if runtime.checkpoint != checkpoint.id || runtime.epoch != checkpoint.epoch.0 {
                return Err("refused authoring result changed its original return checkpoint".into());
            }
            validate_unchanged_admitted_checkpoint(&before, &snapshot, &checkpoint.id)?;
            rejected = Some(RejectedWrite {
                result_seq: movement.seq.0,
                evidence: RejectionEvidence::ToolResult {
                    call_seq, error: result.error.ok_or("refused authoring result has no error")?,
                },
            });
            continue;
        }
        if movement.kind != MovementKind::InterfaceOutbound {
            continue;
        }
        let call_edge = movement.from
            == Endpoint::Interface(config.authoring_source_interface.clone())
            && movement.to == Endpoint::Interface(template.interface().clone());
        let result_edge = movement.from == Endpoint::Interface(template.interface().clone())
            && movement.to == Endpoint::Interface(config.provider_interface.clone());
        if !call_edge && !result_edge {
            continue;
        }
        let metadata = movement
            .invocation_metadata
            .as_ref()
            .and_then(|metadata| metadata.invocation())
            .ok_or("authoring rejection has no admission metadata")?;
        let runtime = metadata
            .runtime
            .as_ref()
            .ok_or("authoring rejection has no run coordinate")?;
        if runtime.run != config.run {
            continue;
        }
        if runtime.worker != config.worker || metadata.verification_failure.is_some() {
            return Err("authoring rejection changed the admitted run scope".into());
        }
        let reference = movement
            .artifact
            .as_ref()
            .ok_or("authoring rejection has no artifact")?;
        let (manifest, payload) = reader.artifact(reference)?;
        if call_edge && manifest.label == config.file_write_label {
            let call: FileWriteCallEnvelope =
                serde_json::from_slice(&payload).map_err(|error| error.to_string())?;
            if call.tool != config.file_write_tool || call.correlation_id != authored.correlation_id
            {
                continue;
            }
            if pending.is_some() {
                return Err(
                    "authoring retry has overlapping calls for one logical generation".into(),
                );
            }
            if runtime.interface != *template.interface()
                || !metadata.grant.tools.contains(&config.file_write_tool)
                || metadata
                    .grant
                    .hard_blocked_tools
                    .contains(&config.file_write_tool)
                || !metadata
                    .grant
                    .interfaces
                    .contains(&template.interface().to_string())
            {
                return Err(
                    "rejected authoring call was not admitted under its exact writer".into(),
                );
            }
            let exact = call.arguments.json_format == authored.json_format && call.arguments.patch == authored.patch
                && call.arguments.expected_before_hash == authored.before_hash
                && call.arguments.expected_after_hash == authored.after_hash;
            pending = Some((
                movement.seq.0,
                exact,
                call.arguments.path,
            ));
        } else if result_edge && manifest.label == config.file_write_result_label {
            let result: ToolResultArtifact =
                serde_json::from_slice(&payload).map_err(|error| error.to_string())?;
            if result.tool != config.file_write_tool
                || result.correlation_id.as_deref() != Some(&authored.correlation_id)
            {
                continue;
            }
            let Some((call_seq, exact, call_path)) = pending.take() else {
                continue;
            };
            if !exact || result.ok {
                continue;
            }
            if result
                .error
                .as_ref()
                .is_some_and(|error| error.details.is_some())
            {
                file_write_failure_result(&result, config, movement.seq.0)?;
                continue;
            }
            let before = reader.snapshot(&before_hash)?;
            if ContentHash::of_bytes(&before) != before_hash {
                return Err("rejected authoring preimage failed integrity".into());
            }
            let original = RunRecord::from_bytes(&before).map_err(|error| error.to_string())?;
            let checkpoint = active_checkpoint_for_role(
                &original,
                config,
                RunAuthoringCheckpointRole::AuthoringResult,
            )?;
            let path = ctx
                .root_slot("run_records_root")?
                .canonicalize()
                .map_err(|error| error.to_string())?
                .join(format!("{}.json", config.run));
            if call_path != path {
                return Err("authoring rejection changed its original path".into());
            }
            if result.body.is_some()
                || result.label != manifest.label
                || call_seq >= movement.seq.0
                || runtime.interface != config.provider_interface
                || runtime.checkpoint != checkpoint.id
                || runtime.epoch != checkpoint.epoch.0
                || !metadata_matches_checkpoint(metadata, template)
                || !artifact_refs_match_bindings(
                    &metadata.artifact_binding_refs,
                    &checkpoint.artifact_bindings,
                )
            {
                return Err(
                    "authoring rejection differs from its exact admitted call/result boundary"
                        .into(),
                );
            }
            let snapshot_hash = runtime
                .run_record_hash
                .as_deref()
                .and_then(ContentHash::from_hex)
                .ok_or("authoring rejection has no admitted snapshot")?;
            let snapshot = reader.snapshot(&snapshot_hash)?;
            if ContentHash::of_bytes(&snapshot) != snapshot_hash {
                return Err("authoring rejection snapshot failed integrity".into());
            }
            validate_unchanged_admitted_checkpoint(&before, &snapshot, &checkpoint.id)?;
            rejected = Some(RejectedWrite {
                result_seq: movement.seq.0,
                evidence: RejectionEvidence::ToolResult {
                    call_seq,
                    error: result
                        .error
                        .ok_or("rejected authoring write has no error")?,
                },
            });
        }
    }
    Ok(rejected)
}

pub fn checked_edit_verification_failure(
    ctx: &InterfaceLiftContext,
    writer: &CheckedRunWriter,
    edit: &CheckedRunEdit,
    after_seq: u64,
) -> Result<Option<Movement>, String> {
    let limits = ctx.required_host_io_limits()?;
    let evidence = CheckedRunEvidence {
        movements_path: &ctx.movements_path,
        ledger_index_path: &ctx.ledger_index_path,
        blobs_dir: &ctx.blobs_dir,
        max_control_frame_bytes: limits.max_control_frame_bytes,
        max_manifest_bytes: limits.max_manifest_bytes,
        max_payload_bytes: limits.max_payload_bytes,
    };
    let mut reader = FileCheckedRunReader::open(&evidence, Some(after_seq))?;
    let first_seq = reader.next_seq;
    checked_edit_verification_failure_with_reader(&mut reader, writer, edit, after_seq, first_seq)
}

fn checked_edit_verification_failure_with_reader(
    reader: &mut impl CheckedRunEvidenceReader,
    writer: &CheckedRunWriter,
    edit: &CheckedRunEdit,
    after_seq: u64,
    first_seq: u64,
) -> Result<Option<Movement>, String> {
    ContentHash::from_hex(&edit.before_hash).ok_or("checked edit has no canonical preimage")?;
    let original =
        RunRecord::from_bytes(edit.content.as_bytes()).map_err(|error| error.to_string())?;
    if ContentHash::of_bytes(edit.content.as_bytes()).to_hex() != edit.after_hash
        || !edit.path.is_absolute()
        || edit.path.file_name().and_then(|s| s.to_str()) != Some(&format!("{}.json", original.id))
        || writer.source == writer.door
        || writer.owner == writer.door
        || edit.correlation_id.is_empty()
    {
        return Err("checked edit has inconsistent identity, path or prospective bytes".into());
    }
    let mut arrival = None;
    let mut failed = None;
    let mut seq = first_seq;
    loop {
        let Some(movement) = reader.movement(seq)? else {
            break;
        };
        seq = movement
            .seq
            .0
            .checked_add(1)
            .ok_or("checked failure sequence overflow")?;
        if movement.seq.0 <= after_seq
            || movement.from != Endpoint::Interface(writer.source.clone())
            || movement.run() != Some(&original.id)
            || movement.to != Endpoint::Interface(original.worker.clone())
            || !matches!(
                movement.kind,
                MovementKind::InterfaceArrival | MovementKind::VerificationFailed
            )
        {
            continue;
        }
        let metadata = movement
            .invocation_metadata
            .as_ref()
            .and_then(|m| m.invocation())
            .ok_or("checked failure has no invocation metadata")?;
        if metadata.verification_failure.is_none() {
            continue;
        }
        let reference = movement
            .artifact
            .as_ref()
            .ok_or("checked failure has no artifact")?;
        let (manifest, payload) = reader.artifact(reference)?;
        if manifest.label != writer.call_label {
            continue;
        }
        let call: FileWriteCallEnvelope =
            serde_json::from_slice(&payload).map_err(|error| error.to_string())?;
        if call.tool != writer.tool
            || call.correlation_id != edit.correlation_id
            || call.arguments.path != edit.path
            || call.arguments.patch != edit.patch
            || call.arguments.json_format != edit.json_format
            || call.arguments.expected_before_hash != edit.before_hash
            || call.arguments.expected_after_hash != edit.after_hash
        {
            continue;
        }
        let runtime = metadata
            .runtime
            .as_ref()
            .ok_or("checked failure has no runtime coordinate")?;
        let hash = runtime
            .run_record_hash
            .as_deref()
            .and_then(ContentHash::from_hex)
            .ok_or("checked failure has no admission snapshot")?;
        let snapshot = reader.snapshot(&hash)?;
        if ContentHash::of_bytes(&snapshot) != hash {
            return Err("checked failure snapshot failed integrity".into());
        }
        let record = RunRecord::from_bytes(&snapshot).map_err(|error| error.to_string())?;
        let checkpoint = record
            .checkpoint(&runtime.checkpoint)
            .ok_or("checked failure checkpoint is absent")?;
        let binding = CheckpointBindingPolicySnapshot {
            inbound: checkpoint
                .interface_binding
                .clone()
                .ok_or("checked failure has no inbound binding")?,
            outbound: checkpoint.outbound_interface_bindings.clone(),
        };
        let failure = metadata
            .verification_failure
            .as_ref()
            .ok_or("checked failure metadata disappeared")?;
        let matching = checkpoint
            .required_verified_artifacts
            .iter()
            .filter(|required| required.artifact_label == writer.call_label)
            .collect::<Vec<_>>();
        let [required] = matching.as_slice() else {
            return Err("checked failure has no unique declared requirement".into());
        };
        if record.id != original.id
            || record.worker != original.worker
            || record.trace != original.trace
            || runtime.run != record.id
            || runtime.ee != record.ee
            || runtime.worker != record.worker
            || runtime.interface != writer.source
            || runtime.epoch != checkpoint.epoch.0
            || !record.active_checkpoints.contains(&checkpoint.id)
            || binding.inbound.interface != writer.source
            || binding.outbound.len() != 1
            || binding.outbound[0].interface != writer.door
            || metadata.checkpoint_binding_policy.as_ref() != Some(&binding)
            || metadata.outbound_interface_bindings != [writer.door.clone()]
            || metadata.required_verified_artifacts != checkpoint.required_verified_artifacts
            || !artifact_refs_match_bindings(
                &metadata.artifact_binding_refs,
                &checkpoint.artifact_bindings,
            )
            || metadata
                .checkpoint_admission
                .as_ref()
                .is_none_or(|admission| {
                    admission.artifact_label != writer.call_label
                        || admission
                            .required_artifact_labels
                            .iter()
                            .collect::<BTreeSet<_>>()
                            != checkpoint
                                .required_verified_artifacts
                                .iter()
                                .map(|r| &r.artifact_label)
                                .collect::<BTreeSet<_>>()
                        || admission.required_artifact_labels.len()
                            != checkpoint.required_verified_artifacts.len()
                })
            || !metadata.grant.tools.contains(&writer.tool)
            || metadata.grant.hard_blocked_tools.contains(&writer.tool)
            || !metadata.grant.interfaces.contains(&writer.door.to_string())
            || !metadata
                .grant
                .mark_outcomes
                .contains(&required.mark.to_string())
            || failure.run.as_ref().is_some_and(|run| run != &record.id)
            || failure
                .checkpoint
                .as_ref()
                .is_some_and(|id| id != &checkpoint.id)
            || failure.verifier.as_ref() != Some(&required.verifier)
            || failure.mark.as_ref() != Some(&required.mark)
            || failure
                .artifact_label
                .as_deref()
                .is_some_and(|label| label != writer.call_label)
        {
            return Err(
                "checked verification failure differs from its exact authored write boundary"
                    .into(),
            );
        }
        if movement.kind == MovementKind::InterfaceArrival {
            if arrival.is_some() {
                return Err("checked failure has ambiguous arrivals".into());
            }
            arrival = Some(movement);
        } else {
            let preceding = arrival
                .take()
                .ok_or("checked failure has no actual preceding arrival")?;
            if preceding.seq >= movement.seq
                || preceding.artifact != movement.artifact
                || preceding.trace != movement.trace
                || preceding.invocation_metadata != movement.invocation_metadata
                || failed.is_some()
            {
                return Err("checked failure differs from its exact preceding arrival".into());
            }
            failed = Some(movement);
        }
    }
    Ok(failed)
}

pub(super) fn validate_rejected_result(
    ctx: &InterfaceLiftContext,
    config: &RunAuthoringConfig,
    movement: &Movement,
    envelope: &InterfaceInvocationEnvelope,
    record: &RunRecord,
    result: &ToolResultArtifact,
) -> Result<(), String> {
    let template = config.checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringResult)?;
    validate_authoring_failure_movement(
        &config.provider_interface,
        movement,
        envelope,
        record,
        config,
        template,
    )?;
    if movement
        .invocation_metadata
        .as_ref()
        .and_then(|metadata| metadata.invocation())
        != Some(envelope.metadata().map_err(|error| error.to_string())?)
        || result.label != config.file_write_result_label
        || result.tool != config.file_write_tool
        || result.ok
        || result.body.is_some()
        || result
            .error
            .as_ref()
            .is_none_or(|error| error.details.is_some())
    {
        return Err(match result.error.as_ref() {
            Some(error) => format!("provider rejected result changed its ordinary failed-write contract ({}: {})", error.code, error.message),
            None => "provider rejected result changed its ordinary failed-write contract".into(),
        });
    }
    let correlation = result
        .correlation_id
        .as_deref()
        .ok_or("provider rejected result has no correlation")?;
    let hand_in_root = ctx
        .root_slot("interface_hand_in_root")?
        .join(config.authoring_source_interface.as_str());
    let mut found = false;
    for (path, call) in private_authoring_calls(ctx, &hand_in_root, config)? {
        if call.correlation_id != correlation {
            continue;
        }
        let hash = ContentHash::from_hex(&call.arguments.expected_before_hash)
            .ok_or("provider rejected result has no canonical preimage")?;
        let (before, original) = pinned_run_record(ctx, &hash, "provider rejected result")?;
        let attempt = validate_authoring_attempt_source(AuthoringAttemptSourceValidation {
            ctx,
            record: &original,
            record_bytes: &before,
            config,
            call,
            manifest_path: path,
        })?;
        if rejected_write_through(ctx, config, &attempt.authored, Some(movement.seq.0))?
            .is_some_and(|rejection| rejection.result_seq == movement.seq.0)
        {
            if found {
                return Err("provider rejected result proves more than one private attempt".into());
            }
            found = true;
        }
    }
    if !found {
        return Err("provider rejected result has no exact admitted private attempt".into());
    }
    Ok(())
}

fn merge_object_changes(
    before: &serde_json::Map<String, serde_json::Value>,
    planned: &serde_json::Map<String, serde_json::Value>,
    current: &mut serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    for key in before.keys().chain(planned.keys()).collect::<BTreeSet<_>>() {
        if before.get(key) == planned.get(key) {
            continue;
        }
        if current.get(key) != before.get(key) {
            return Err(format!(
                "concurrent authoring changed the same field '{key}'"
            ));
        }
        match planned.get(key) {
            Some(value) => {
                current.insert(key.clone(), value.clone());
            }
            None => {
                current.remove(key);
            }
        }
    }
    Ok(())
}

fn keyed_checkpoints(
    value: &serde_json::Value,
) -> Result<BTreeMap<String, serde_json::Value>, String> {
    let rows = value
        .as_array()
        .ok_or("authoring checkpoints are not an array")?;
    let mut by_id = BTreeMap::new();
    for row in rows {
        let id = row
            .get("id")
            .and_then(serde_json::Value::as_str)
            .ok_or("authoring checkpoint has no id")?;
        if by_id.insert(id.to_string(), row.clone()).is_some() {
            return Err("authoring checkpoint ids repeat".into());
        }
    }
    Ok(by_id)
}

pub(super) fn validate_rebased_authored_run(
    before: &[u8],
    current: &[u8],
    previous: &AuthoredRun,
    next: &AuthoredRun,
    config: &RunAuthoringConfig,
) -> Result<(), String> {
    if rebase_authored_run(before, current, previous, config, next.json_format)? != *next {
        return Err("provider authoring retry changed its original intent or independent write evidence".into());
    }
    Ok(())
}

pub(super) fn rebase_authored_run(
    before: &[u8],
    current: &[u8],
    authored: &AuthoredRun,
    config: &RunAuthoringConfig,
    json_format: Option<JsonFormat>,
) -> Result<AuthoredRun, String> {
    if ContentHash::of_bytes(before).to_hex() != authored.before_hash {
        return Err("authoring retry lost the original exact preimage".into());
    }
    if authored.pending_handoff.replacement.is_some() && before != current {
        return Err("rejected final replacement requires fresh proof after an independent record advance; refusing rebase before write".into());
    }
    let original = RunRecord::from_bytes(before).map_err(|error| error.to_string())?;
    let observed = RunRecord::from_bytes(current).map_err(|error| error.to_string())?;
    let extension = if original.interface_state.get(&config.provider_interface)
        != observed.interface_state.get(&config.provider_interface) {
        output_connections::extension(before, current, config)?
    } else { None };
    let revision = input_revision::revision_delta(before, current, config)?;
    if extension.is_some() && revision.is_some() { return Err("authoring retry has competing definition changes".into()); }
    let protected = revision.as_ref().map_or_else(|| extension.as_ref().map_or(current, |extension| extension.protected.as_slice()), |revision| revision.protected.as_slice());
    let actual = RunRecord::from_bytes(protected).map_err(|error| error.to_string())?;
    if original.id != actual.id
        || original.worker != actual.worker
        || original.trace != actual.trace
        || original.interface_state.get(&config.provider_interface)
            != actual.interface_state.get(&config.provider_interface)
    {
        return Err("authoring retry changed run identity or provider-owned state".into());
    }
    let mut before: serde_json::Value =
        serde_json::from_slice(before).map_err(|error| error.to_string())?;
    let mut planned: serde_json::Value =
        serde_json::from_str(&authored.content).map_err(|error| error.to_string())?;
    let mut merged: serde_json::Value =
        serde_json::from_slice(protected).map_err(|error| error.to_string())?;
    let old_checkpoints = keyed_checkpoints(&before["checkpoints"])?;
    let new_checkpoints = keyed_checkpoints(&planned["checkpoints"])?;
    let live_checkpoints = keyed_checkpoints(&merged["checkpoints"])?;
    let changed = old_checkpoints
        .keys()
        .chain(new_checkpoints.keys())
        .filter(|id| old_checkpoints.get(*id) != new_checkpoints.get(*id))
        .cloned()
        .collect::<BTreeSet<_>>();
    for id in &changed {
        if live_checkpoints.get(id) != old_checkpoints.get(id) {
            return Err(format!("concurrent authoring changed checkpoint '{id}'"));
        }
    }
    let checkpoints = merged["checkpoints"]
        .as_array_mut()
        .ok_or("current checkpoints are not an array")?;
    checkpoints.retain(|row| !changed.contains(row["id"].as_str().unwrap_or_default()));
    checkpoints.extend(
        planned["checkpoints"]
            .as_array()
            .ok_or("planned checkpoints are not an array")?
            .iter()
            .filter(|row| changed.contains(row["id"].as_str().unwrap_or_default()))
            .cloned(),
    );
    let old_active: BTreeSet<CheckpointId> =
        serde_json::from_value(before["active_checkpoints"].clone())
            .map_err(|error| error.to_string())?;
    let new_active: BTreeSet<CheckpointId> =
        serde_json::from_value(planned["active_checkpoints"].clone())
            .map_err(|error| error.to_string())?;
    let mut active: Vec<CheckpointId> =
        serde_json::from_value(merged["active_checkpoints"].clone())
            .map_err(|error| error.to_string())?;
    active.retain(|id| !old_active.contains(id) || new_active.contains(id));
    for id in serde_json::from_value::<Vec<CheckpointId>>(planned["active_checkpoints"].clone())
        .map_err(|error| error.to_string())?
    {
        if !old_active.contains(&id) {
            if active.contains(&id) {
                return Err("authoring retry duplicated a concurrent active checkpoint".into());
            }
            active.push(id);
        }
    }
    merged["active_checkpoints"] =
        serde_json::to_value(active).map_err(|error| error.to_string())?;
    merge_object_changes(
        before["interface_state"]
            .as_object()
            .ok_or("original interface state is not an object")?,
        planned["interface_state"]
            .as_object()
            .ok_or("planned interface state is not an object")?,
        merged["interface_state"]
            .as_object_mut()
            .ok_or("current interface state is not an object")?,
    )?;
    for key in ["checkpoints", "active_checkpoints", "interface_state"] {
        before.as_object_mut().unwrap().remove(key);
        planned.as_object_mut().unwrap().remove(key);
    }
    merge_object_changes(
        before.as_object().unwrap(),
        planned.as_object().unwrap(),
        merged.as_object_mut().unwrap(),
    )?;
    let mut declared_config = if extension.is_some() {
        attempt_config(&observed, config)?
    } else {
        config.clone()
    };
    if let Some(extension) = &extension {
        extension.restore(&mut merged, &config.provider_interface)?;
    }
    let revised_generation = revision.as_ref().map(|revision| revision.restore(&mut merged, &config.provider_interface)).transpose()?;
    let content = String::from_utf8(json_format.unwrap_or(JsonFormat::Pretty).encode(&merged)?).map_err(|error| error.to_string())?;
    let candidate = RunRecord::from_bytes(content.as_bytes()).map_err(|error| error.to_string())?;
    validate_pending_items_are_active(
        &candidate,
        &config.provider_interface,
        &authored.pending_handoff,
    )?;
    if revision.is_some() {
        let topology: BoundRunAuthoringTopology = serde_json::from_value(merged["interface_state"][config.provider_interface.as_str()][RUN_AUTHORING_BINDING].clone()).map_err(|error| error.to_string())?;
        declared_config.checkpoints = topology.checkpoints;
    }
    let stepped = match capability_amendment::saved_ordinary(protected, content.as_bytes(), config)? {
        Some(step) => {
            declared_config = attempt_config(&actual, config)?;
            RunRecord::from_bytes(&step).map_err(|error| error.to_string())?
        }
        None => candidate.clone(),
    };
    let effective_config = result_inputs::saved_attempt_config(&candidate, &stepped, &declared_config)?;
    validate_bound_run_authoring_topology(&candidate, &effective_config, None)?;
    if !result_inputs::saved_definition_transition(&actual, &stepped, &effective_config)? {
        result_inputs::validate_transition(&actual, &stepped, &effective_config)?;
    }
    let mut rebased = authored.clone();
    if let Some(generation) = revised_generation { rebased.correlation_id = format!("provider-run-authoring:{}:{generation}", config.run); }
    rebased.before_hash = ContentHash::of_bytes(current).to_hex();
    rebased.after_hash = ContentHash::of_bytes(content.as_bytes()).to_hex();
    rebased.patch = json_patch::diff(
        &serde_json::from_slice(current).map_err(|error| error.to_string())?,
        &merged,
    );
    rebased.content = content;
    rebased.json_format = json_format;
    if apply_run_json_patch(current, &rebased.patch, &rebased.after_hash, rebased.json_format)? != rebased.content {
        return Err("authoring retry did not reconstruct its exact planned bytes".into());
    }
    Ok(rebased)
}
