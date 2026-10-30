use std::io::{BufRead, Read, Seek};
use std::sync::Arc;

use octopus_interface_transport::{InterfaceLiftContext, InterfaceLiftFn};
use octopus_interfaces::{DeliveryVerdict, InterfaceArtifactFile};
use octopus_ledger::{ContentHash, Endpoint, Movement, MovementKind, RefusedReason};
use octopus_ledger_store::DurableMovementDecoder;
use octopus_operator_chat_client::input_intake::{INPUT_INTAKE_DRIVER, InputIntakeConfig, InputNotificationAction};
use octopus_run_authoring::{
    checked_edit_verification_failure, checked_provider_run_advance, owner_writer_binding,
    OwnerWriterEvidence,
};
use octopus_run_record::RunRecord;
use octopus_tool_handlers::ToolResultArtifact;
use octopus_world_pyecho_life_runtime::LedgerTailReader;

use crate::intake_state::{
    InputDelivery, PendingInput, checked_input_result, load_state, prepare_input, reprepare_input,
    save_state,
};
use crate::run_affordances::{PEER_MESSAGE_LABEL, PeerAction, PeerMessage};

pub fn interface_life() -> InterfaceLiftFn {
    Arc::new(|ctx| Box::pin(run(ctx)))
}

fn is_internal_writer_receipt(
    config: &InputIntakeConfig,
    record: &RunRecord,
    movement: &Movement,
    label: &str,
    payload: &[u8],
) -> Result<bool, String> {
    if movement.kind != MovementKind::InterfaceOutbound
        || movement.from != Endpoint::Interface(config.writer.door.clone())
        || movement.to != Endpoint::Interface(config.writer.owner.clone())
        || label != config.writer.result_label
    {
        return Ok(false);
    }
    config.validate(&config.writer.owner)?;
    let declaration = record.interface_state.get(&config.writer.owner)
        .and_then(|state| state.get("input_intake"))
        .ok_or("intake receipt owner has no authored declaration")?;
    if record.id.as_str() != config.run || !config.matches_declaration(declaration)?
    {
        return Err("intake receipt owner differs from its authored declaration".into());
    }
    let binding = owner_writer_binding(record, &config.writer.owner)?;
    let writer_run = binding.as_ref().map_or(&record.id, |binding| &binding.run);
    let runtime = movement.invocation_metadata.as_ref()
        .and_then(|metadata| metadata.invocation())
        .and_then(|metadata| metadata.runtime.as_ref())
        .ok_or("intake writer receipt has no runtime evidence")?;
    if &runtime.run != writer_run {
        return Ok(false);
    }
    if runtime.interface != config.writer.owner || movement.artifact.is_none() {
        return Err("intake writer receipt differs from its admitted boundary".into());
    }
    let result: ToolResultArtifact = serde_json::from_slice(payload)
        .map_err(|error| error.to_string())?;
    result.validate()?;
    if result.tool != config.writer.tool || result.label != config.writer.result_label
        || result.correlation_id.as_deref().is_none_or(|id| id.is_empty() || id.trim() != id)
    {
        return Err("intake writer receipt differs from its exact tool evidence".into());
    }
    Ok(true)
}

fn input_message(
    config: &InputIntakeConfig,
    interface: &octopus_descriptors::InterfaceId,
    sequence: u64,
    label: &str,
    bytes: &[u8],
    artifact: &octopus_ledger::ArtifactRef,
) -> Result<PeerMessage, String> {
    let message = if label == PEER_MESSAGE_LABEL {
        serde_json::from_slice::<PeerMessage>(bytes).map_err(|e| e.to_string())?
    } else {
        PeerMessage {
            action: match config.notification_action {
                InputNotificationAction::Queue => PeerAction::Queue,
                InputNotificationAction::Followup => PeerAction::Followup,
            },
            control_id: format!("input:{interface}:{sequence}"),
            text: Some(crate::handler::peer_notification_text(label, bytes).map_err(|e| e.to_string())?),
            artifacts: vec![artifact.clone()],
        }
    };
    message.validate()?;
    Ok(message)
}

struct EvidenceTail {
    reader: std::io::BufReader<std::fs::File>,
    decoder: DurableMovementDecoder,
    next_seq: u64,
    rows: Vec<Movement>,
}

struct PreparedIntakeArtifact {
    file: InterfaceArtifactFile,
    hash: ContentHash,
}

impl PreparedIntakeArtifact {
    fn new(ctx: &InterfaceLiftContext, file: InterfaceArtifactFile) -> Result<Self, String> {
        let limits = ctx.required_host_io_limits()?;
        let snapshot = file
            .read_bounded(limits.max_manifest_bytes, limits.max_payload_bytes)
            .map_err(|error| error.to_string())?;
        Ok(Self {
            file,
            hash: ContentHash::of_bytes(&snapshot.manifest_bytes),
        })
    }
}

impl EvidenceTail {
    fn retain_movement(&mut self, movement: Movement) -> Result<(), String> {
        match self.rows.binary_search_by_key(&movement.seq, |row| row.seq) {
            Ok(index) if self.rows[index] != movement => {
                Err("intake evidence changed at an existing sequence".into())
            }
            Ok(_) => Ok(()),
            Err(index) => {
                self.rows.insert(index, movement);
                Ok(())
            }
        }
    }

    fn open(ctx: &InterfaceLiftContext) -> Result<Self, String> {
        Ok(Self {
            reader: std::io::BufReader::new(
                std::fs::File::open(&ctx.movements_path).map_err(|e| e.to_string())?,
            ),
            decoder: DurableMovementDecoder::for_log(&ctx.movements_path),
            next_seq: 0,
            rows: Vec::new(),
        })
    }

    fn catch_up(
        &mut self,
        ctx: &InterfaceLiftContext,
        completed: Option<u64>,
        pending_seq: u64,
        writer_door: &octopus_descriptors::InterfaceId,
    ) -> Result<(), String> {
        let maximum = ctx.required_host_io_limits()?.max_control_frame_bytes;
        if self
            .reader
            .get_ref()
            .metadata()
            .map_err(|e| e.to_string())?
            .len()
            < self.reader.stream_position().map_err(|e| e.to_string())?
        {
            return Err("intake ledger was truncated".into());
        }
        self.rows
            .retain(|row| completed.is_none_or(|seq| row.seq.0 > seq));
        loop {
            let start = self.reader.stream_position().map_err(|e| e.to_string())?;
            let mut bytes = Vec::new();
            self.reader
                .by_ref()
                .take(maximum + 1)
                .read_until(b'\n', &mut bytes)
                .map_err(|e| e.to_string())?;
            if bytes.len() as u64 > maximum {
                return Err("intake ledger row exceeds declared frame bound".into());
            }
            if bytes.is_empty() || bytes.last() != Some(&b'\n') {
                self.reader
                    .seek(std::io::SeekFrom::Start(start))
                    .map_err(|e| e.to_string())?;
                break;
            }
            let movement = self
                .decoder
                .decode(&bytes, self.next_seq as usize)
                .map_err(|e| e.to_string())?;
            if movement.seq.0 != self.next_seq {
                return Err("intake ledger has a sequence gap".into());
            }
            self.next_seq = self
                .next_seq
                .checked_add(1)
                .ok_or("intake sequence overflow")?;
            if completed.is_none_or(|seq| movement.seq.0 > seq)
                && (movement.from == Endpoint::Interface(ctx.interface.clone())
                    || (movement.to == Endpoint::Interface(ctx.interface.clone())
                        && (movement.seq.0 == pending_seq
                            || movement.from == Endpoint::Interface(writer_door.clone()))))
            {
                self.retain_movement(movement)?;
            }
        }
        Ok(())
    }

    fn delivered(
        &self,
        config: &InputIntakeConfig,
        hash: &ContentHash,
        after_seq: u64,
    ) -> Result<Option<u64>, String> {
        crate::intake_state::admitted_input(&self.rows, config, hash, after_seq)
    }
}

#[cfg(test)]
#[path = "../tests/support/input_tail.rs"]
mod tests;

async fn run(ctx: InterfaceLiftContext) -> Result<(), String> {
    let descriptor = ctx.resolved_interface_descriptor()?;
    if descriptor.interface_type != octopus_interfaces::InterfaceType::System
        || descriptor.role.0 != "service"
        || descriptor.interface_driver_ref != INPUT_INTAKE_DRIVER
    {
        return Err("input intake requires its exact declared System service descriptor".into());
    }
    let config: InputIntakeConfig = serde_json::from_value(
        descriptor
            .interface_driver_config
            .as_ref()
            .and_then(|c| c.private_config.get("input_intake"))
            .cloned()
            .ok_or("input intake is missing its concrete declaration")?,
    )
    .map_err(|e| e.to_string())?;
    config.validate(&ctx.interface)?;
    let path = ctx
        .root_slot("run_records_root")?
        .canonicalize()
        .map_err(|error| format!("intake run-record root is unavailable: {error}"))?
        .join(format!("{}.json", config.run));
    let limits = ctx.required_host_io_limits()?;
    let state_limit = limits
        .max_payload_bytes
        .checked_mul(4)
        .ok_or("intake private state limit overflow")?;
    std::fs::create_dir_all(&ctx.var_dir).map_err(|e| e.to_string())?;
    let state_path = ctx.var_dir.join("input-state.json");
    let mut state = load_state(&state_path, state_limit)?;
    let mut reader = LedgerTailReader::open_named(&ctx, "input")?;
    let mut evidence = EvidenceTail::open(&ctx)?;
    let writer_evidence = OwnerWriterEvidence::default();
    let output = ctx.var_dir.join("input-artifacts");
    let mut awaiting: Option<(u64, tokio::time::Instant)> = None;
    let mut validated_source = None;
    let mut validated_edit = false;
    let mut delivery_artifact: Option<PreparedIntakeArtifact> = None;
    let mut writer_artifact: Option<PreparedIntakeArtifact> = None;
    ctx.signal_ready().await?;
    loop {
        if ctx.cancel.is_cancelled() {
            return Ok(());
        }
        if let Some(pending) = &state.pending {
            evidence.catch_up(
                &ctx,
                state.completed_through,
                pending.source.seq.0,
                &config.writer.door,
            )?;
        }
        if state.pending.is_none() {
            let batch = reader.read_new_uncommitted_limit(1)?;
            if batch.is_empty() {
                reader.wait_for_change(&ctx.cancel).await?;
                continue;
            }
            for own in batch {
                if state
                    .completed_through
                    .is_some_and(|seq| own.movement.seq.0 <= seq)
                {
                    reader.commit_through(&own)?;
                    continue;
                }
                let input = reader.envelope(&own)?;
                let manifest = input.read_artifact_manifest().map_err(|e| e.to_string())?;
                let bytes = input.read_artifact_payload().map_err(|e| e.to_string())?;
                if own.movement.from == Endpoint::Interface(config.writer.door.clone())
                    && manifest.label == config.writer.result_label
                {
                    let record = RunRecord::from_bytes(
                        &octopus_interfaces::read_artifact_carrier_file_bounded(
                            &path, limits.max_payload_bytes,
                        ).map_err(|error| error.to_string())?,
                    ).map_err(|error| error.to_string())?;
                    if is_internal_writer_receipt(&config, &record, &own.movement, &manifest.label, &bytes)? {
                        reader.commit_through(&own)?;
                        continue;
                    }
                }
                let message = input_message(&config, &ctx.interface, own.movement.seq.0,
                    &manifest.label, &bytes, &input.artifact)?;
                evidence.retain_movement(own.movement.clone())?;
                state.pending = Some(PendingInput {
                    source: own.movement.clone(),
                    delivery: InputDelivery {
                        schema_version: 1,
                        source_movement_seq: own.movement.seq.0,
                        source_artifact: input.artifact,
                        message,
                    },
                    edit: None,
                });
                save_state(&state_path, &state, state_limit)?;
                reader.commit_through(&own)?;
            }
            continue;
        }
        let pending = state.pending.as_ref().ok_or("input pending disappeared")?;
        pending.delivery.validate()?;
        if pending.source.to != Endpoint::Interface(ctx.interface.clone())
            || pending.source.seq.0 != pending.delivery.source_movement_seq
            || pending.source.artifact.as_ref() != Some(&pending.delivery.source_artifact)
            || evidence.rows.binary_search_by_key(&pending.source.seq, |row| row.seq)
                .ok().is_none_or(|index| evidence.rows[index] != pending.source)
        {
            return Err("retained input differs from its own admitted source movement".into());
        }
        if validated_source != Some(pending.source.seq.0) {
            let reference = &pending.delivery.source_artifact;
            let original =
                InterfaceArtifactFile::new(ctx.blobs_dir.join(reference.content.to_hex()))
                    .map_err(|e| e.to_string())?
                    .with_payload_cache_root(&ctx.blobs_dir)
                    .read_bounded(limits.max_manifest_bytes, limits.max_payload_bytes)
                    .map_err(|e| e.to_string())?;
            if ContentHash::of_bytes(&original.manifest_bytes) != reference.content {
                return Err("retained input source manifest hash mismatch".into());
            }
            let message = input_message(&config, &ctx.interface, pending.source.seq.0,
                &original.manifest.label, &original.payload_bytes, reference)?;
            if message != pending.delivery.message {
                return Err(
                    "retained input message differs from its original admitted payload".into(),
                );
            }
            validated_source = Some(pending.source.seq.0);
        }
        if delivery_artifact.is_none() {
            let bytes = serde_json::to_vec(&pending.delivery).map_err(|e| e.to_string())?;
            delivery_artifact = Some(PreparedIntakeArtifact::new(
                &ctx,
                InterfaceArtifactFile::write(&output, &config.input_label, &bytes)
                    .map_err(|e| e.to_string())?,
            )?);
        }
        let prepared_delivery = delivery_artifact.as_ref().ok_or("input artifact disappeared")?;
        let hash = prepared_delivery.hash;
        let artifact = prepared_delivery.file.clone();
        if evidence
            .delivered(&config, &hash, pending.source.seq.0)?
            .is_some()
        {
            state.completed_through = Some(pending.source.seq.0);
            state.pending = None;
            save_state(&state_path, &state, state_limit)?;
            evidence.rows.clear();
            awaiting = None;
            validated_source = None;
            validated_edit = false;
            delivery_artifact = None;
            writer_artifact = None;
            continue;
        }
        if awaiting
            .as_ref()
            .is_none_or(|(id, _)| *id != pending.source.seq.0)
        {
            awaiting = Some((
                pending.source.seq.0,
                tokio::time::Instant::now()
                    + std::time::Duration::from_millis(config.profile.result_timeout_ms),
            ));
        }
        let deadline = awaiting
            .as_ref()
            .ok_or("intake evidence deadline disappeared")?
            .1;
        if tokio::time::Instant::now() >= deadline {
            return Err("input remains durably retained; checked write deadline expired".into());
        }
        let current =
            octopus_interfaces::read_artifact_carrier_file_bounded(&path, limits.max_payload_bytes)
                .map_err(|e| e.to_string())?;
        if pending.edit.is_none() {
            let edit = prepare_input(
                &config,
                path.clone(),
                &current,
                pending.source.seq.0,
                &pending.delivery.source_artifact,
                evidence.next_seq.saturating_sub(1),
                limits.max_payload_bytes,
            )?;
            state
                .pending
                .as_mut()
                .ok_or("pending input disappeared")?
                .edit = Some(edit);
            save_state(&state_path, &state, state_limit)?;
            continue;
        }
        let pending_edit = pending.edit.as_ref().ok_or("input edit disappeared")?;
        if !validated_edit {
            pending_edit.validate(&config, &path, pending.source.seq.0, &pending.delivery.source_artifact, limits.max_payload_bytes)?;
            validated_edit = true;
        }
        let chain = checked_provider_run_advance(
            &ctx,
            &path,
            pending_edit.before.as_bytes(),
            &current,
            pending_edit.after_seq,
            &config.provider_interface,
            &config.provider_profile,
        );
        if octopus_interfaces::read_artifact_carrier_file_bounded(&path, limits.max_payload_bytes)
            .map_err(|e| e.to_string())?
            != current
        {
            continue;
        }
        if let Ok(chain) = &chain {
            if let Some(transition) = chain.iter().find(|transition| {
                transition.writer == config.writer
                    && transition.before == pending_edit.before.as_bytes()
                    && transition.after == pending_edit.edit.content.as_bytes()
            }) {
                if !pending_edit.matches_call(&ctx, &config, &transition.call)? {
                    return Err("input effect proof belongs to another checked attempt".into());
                }
                match ctx.hand_arrival(artifact, Vec::new()).await? {
                    DeliveryVerdict::Admitted { .. } => {}
                    verdict => {
                        return Err(format!(
                            "prepared input was refused; retained for evidence-based recovery: {verdict:?}"
                        ));
                    }
                }
                evidence.catch_up(
                    &ctx,
                    state.completed_through,
                    pending.source.seq.0,
                    &config.writer.door,
                )?;
                if evidence
                    .delivered(&config, &hash, pending.source.seq.0)?
                    .is_none()
                {
                    return Err("input hand-in has no exact receiver outbound proof".into());
                }
                continue;
            }
        }
        let ordered_result = pending_edit.ordered.as_ref().map(|attempt|writer_evidence.result(&ctx,attempt)).transpose()?.flatten();
        let result = if pending_edit.ordered.is_some() { None } else {checked_input_result(&ctx, &config, pending_edit, &evidence.rows)?};
        if ordered_result == Some(true) || result.as_ref().is_some_and(|result| result.ok) {
            await_evidence(
                &mut reader,
                &ctx,
                deadline,
                chain
                    .err()
                    .unwrap_or_else(|| "exact write is absent".into()),
            )
            .await?;
            continue;
        }
        let rejected = ordered_result == Some(false) || result.is_some()
            || (pending_edit.ordered.is_none() && checked_edit_verification_failure(
                &ctx,
                &config.writer,
                &pending_edit.edit,
                pending_edit.after_seq,
            )?
            .is_some());
        if rejected && current == pending_edit.before.as_bytes() {
            return Err(format!(
                "input writer failed without an independent change: {result:?}"
            ));
        }
        if writer_artifact.is_none() {
            writer_artifact = Some(PreparedIntakeArtifact::new(
                &ctx,
                pending_edit.materialize(&output, &config)?,
            )?);
        }
        let prepared_writer = writer_artifact.as_ref().ok_or("input writer artifact disappeared")?;
        let call_hash = prepared_writer.hash;
        let file = prepared_writer.file.clone();
        let sent = evidence.rows.iter().any(|row| {
            row.kind == MovementKind::InterfaceOutbound
                && row.from == Endpoint::Interface(ctx.interface.clone())
                && row.to == Endpoint::Interface(config.writer.door.clone())
                && row
                    .artifact
                    .as_ref()
                    .is_some_and(|reference| reference.content == call_hash)
        });
        if rejected || (!sent && current != pending_edit.before.as_bytes()) {
            let chain = match chain {
                Ok(chain) => chain,
                Err(error) => {
                    await_evidence(&mut reader, &ctx, deadline, error).await?;
                    continue;
                }
            };
            let edit = reprepare_input(
                &config,
                pending,
                &current,
                evidence.next_seq.saturating_sub(1),
                &chain,
                limits.max_payload_bytes,
            )?;
            state
                .pending
                .as_mut()
                .ok_or("pending input disappeared")?
                .edit = Some(edit);
            save_state(&state_path, &state, state_limit)?;
            validated_edit = false;
            writer_artifact = None;
            continue;
        }
        if !sent {
            match ctx.hand_arrival(file, Vec::new()).await? {
                DeliveryVerdict::Admitted { .. } => {}
                DeliveryVerdict::Refused {
                    reason: RefusedReason::VerificationFailed,
                    ..
                } => {
                    if checked_edit_verification_failure(
                        &ctx,
                        &config.writer,
                        &pending_edit.edit,
                        pending_edit.after_seq,
                    )?
                    .is_none()
                    {
                        return Err(
                            "input writer refusal has no exact durable verification failure".into(),
                        );
                    }
                }
                verdict => return Err(format!("input writer hand-in refused: {verdict:?}")),
            }
            continue;
        }
        await_evidence(
            &mut reader,
            &ctx,
            deadline,
            "checked file result is absent".into(),
        )
        .await?;
    }
}

async fn await_evidence(
    reader: &mut LedgerTailReader,
    ctx: &InterfaceLiftContext,
    deadline: tokio::time::Instant,
    reason: String,
) -> Result<(), String> {
    tokio::select! {
        result = reader.wait_for_change(&ctx.cancel) => result,
        _ = tokio::time::sleep_until(deadline) => Err(format!("input remains durably retained; checked evidence did not become complete: {reason}")),
    }
}
