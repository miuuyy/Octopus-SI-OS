use octopus_run_authoring::JsonFormat;
use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};

use octopus_interface_transport::InterfaceLiftContext;
use octopus_interfaces::{BoundedInterfaceArtifactSnapshot, InterfaceArtifactFile};
use octopus_ledger::{
    ArtifactRef, CheckpointBindingPolicySnapshot, ContentHash, Endpoint, Movement, MovementKind,
};
use octopus_operator_chat_client::input_intake::{InputIntakeConfig, initial_checkpoints};
use octopus_run_authoring::{
    CheckedRunEdit, CheckedRunTransition, OwnerWriterEdit, owner_writer_binding, provider_context_bindings,
    validate_unchanged_run_neighbors,
};
use octopus_run_record::RunRecord;
use octopus_tool_handlers::ToolResultArtifact;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::run_affordances::PeerMessage;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputDelivery {
    pub schema_version: u32,
    pub source_movement_seq: u64,
    pub source_artifact: ArtifactRef,
    pub message: PeerMessage,
}

impl InputDelivery {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != 1 {
            return Err("unsupported input delivery schema".into());
        }
        self.message.validate()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntakeEdit {
    pub before: String,
    pub after_seq: u64,
    pub edit: CheckedRunEdit,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ordered: Option<OwnerWriterEdit>,
}

impl IntakeEdit {
    pub fn matches_call(
        &self,
        ctx: &InterfaceLiftContext,
        config: &InputIntakeConfig,
        row: &Movement,
    ) -> Result<bool, String> {
        let captured = read_evidence_artifact(
            ctx,
            row.artifact.as_ref().ok_or("intake call has no artifact")?,
        )?;
        if captured.manifest.label != config.writer.call_label {
            return Ok(false);
        }
        let value: Value =
            serde_json::from_slice(&captured.payload_bytes).map_err(|e| e.to_string())?;
        if let Some(ordered) = &self.ordered { return Ok(ordered.matches_call(&value)); }
        Ok(value
            == json!({"tool":config.writer.tool,"correlation_id":self.edit.correlation_id,
            "arguments":self.edit.arguments()?}))
    }

    pub fn validate(
        &self,
        config: &InputIntakeConfig,
        path: &Path,
        source_seq: u64,
        source_artifact: &ArtifactRef,
        max_payload_bytes: u64,
    ) -> Result<(), String> {
        let expected = prepare_input_edit(
            config,
            path.to_path_buf(),
            self.before.as_bytes(),
            source_seq,
            source_artifact,
            self.after_seq,
            self.edit.json_format,
        )?;
        if serde_json::to_value(&self.edit).map_err(|e| e.to_string())?
            != serde_json::to_value(expected.edit).map_err(|e| e.to_string())?
        {
            return Err("retained intake edit differs from its exact source intent".into());
        }
        if let Some(ordered) = &self.ordered { ordered.validate(self.before.as_bytes(), &self.edit, &config.writer, max_payload_bytes)?; }
        let record = RunRecord::from_bytes(self.before.as_bytes()).map_err(|e|e.to_string())?;
        if self.ordered.is_some() != owner_writer_binding(&record,&config.writer.owner)?.is_some() { return Err("intake edit changed its declared writer mode".into()); }
        Ok(())
    }

    pub fn materialize(&self, root: &Path, config: &InputIntakeConfig) -> Result<InterfaceArtifactFile,String> {
        match &self.ordered { Some(ordered) => ordered.materialize(root), None => self.edit.materialize(root,&config.profile) }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingInput {
    pub source: Movement,
    pub delivery: InputDelivery,
    pub edit: Option<IntakeEdit>,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntakeState {
    pub completed_through: Option<u64>,
    pub pending: Option<PendingInput>,
}

pub fn reprepare_input(
    config: &InputIntakeConfig,
    pending: &PendingInput,
    current: &[u8],
    after_seq: u64,
    chain: &[CheckedRunTransition],
    max_payload_bytes: u64,
) -> Result<IntakeEdit, String> {
    let previous = pending
        .edit
        .as_ref()
        .ok_or("intake retry has no retained edit")?;
    previous.validate(config, &previous.edit.path, pending.source.seq.0, &pending.delivery.source_artifact, max_payload_bytes)?;
    let mut expected = previous.before.as_bytes();
    if chain.is_empty() {
        return Err("intake retry has no proven independent change".into());
    }
    for transition in chain {
        if transition.before != expected || transition.writer.owner == config.writer.owner {
            return Err("intake retry is not a complete independent provider chain".into());
        }
        octopus_run_authoring::validate_independent_provider_transition(
            &RunRecord::from_bytes(expected).map_err(|e|e.to_string())?,&config.provider_interface,&config.provider_profile,transition)?;
        expected = &transition.after;
    }
    if expected != current || current == previous.before.as_bytes() {
        return Err("intake retry differs from its proven current snapshot".into());
    }
    let original = RunRecord::from_bytes(previous.before.as_bytes()).map_err(|e| e.to_string())?;
    let actual = RunRecord::from_bytes(current).map_err(|e| e.to_string())?;
    let owner = &config.writer.owner;
    if original.interface_state.get(owner) != actual.interface_state.get(owner) {
        return Err("intake retry changed its own retained state".into());
    }
    let owned: BTreeSet<octopus_runtime::CheckpointId> =
        serde_json::from_value(original.interface_state[owner]["owned_checkpoint_ids"].clone())
            .map_err(|e| e.to_string())?;
    for id in owned {
        if original.checkpoint(&id) != actual.checkpoint(&id)
            || original.active_checkpoints.contains(&id) != actual.active_checkpoints.contains(&id)
        {
            return Err("intake retry changed its own pending checkpoint".into());
        }
    }
    prepare_input(
        config,
        previous.edit.path.clone(),
        current,
        pending.source.seq.0,
        &pending.delivery.source_artifact,
        after_seq,
        max_payload_bytes,
    )
}

fn read_evidence_artifact(
    ctx: &InterfaceLiftContext,
    reference: &ArtifactRef,
) -> Result<BoundedInterfaceArtifactSnapshot, String> {
    let limits = ctx.required_host_io_limits()?;
    let captured = InterfaceArtifactFile::new(ctx.blobs_dir.join(reference.content.to_hex()))
        .map_err(|e| e.to_string())?
        .with_payload_cache_root(&ctx.blobs_dir)
        .read_bounded(limits.max_manifest_bytes, limits.max_payload_bytes)
        .map_err(|e| e.to_string())?;
    if ContentHash::of_bytes(&captured.manifest_bytes) != reference.content {
        return Err("intake evidence manifest hash mismatch".into());
    }
    Ok(captured)
}

fn validate_attempt_edge(
    ctx: &InterfaceLiftContext,
    config: &InputIntakeConfig,
    original: &RunRecord,
    row: &Movement,
    result: bool,
) -> Result<(), String> {
    let writer = &config.writer;
    let (from, to, label) = if result {
        (&writer.door, &writer.owner, &writer.result_label)
    } else {
        (&writer.source, &writer.door, &writer.call_label)
    };
    let candidates = original
        .checkpoints
        .iter()
        .filter(|cp| {
            original.active_checkpoints.contains(&cp.id)
                && cp.inbound_interface() == Some(from)
                && cp.outbound_interfaces().collect::<Vec<_>>() == [to]
        })
        .collect::<Vec<_>>();
    let [checkpoint] = candidates.as_slice() else {
        return Err("intake attempt has no unique original writer checkpoint".into());
    };
    let metadata = row
        .invocation_metadata
        .as_ref()
        .and_then(|m| m.invocation())
        .ok_or("intake attempt has no invocation metadata")?;
    let runtime = metadata
        .runtime
        .as_ref()
        .ok_or("intake attempt has no runtime coordinate")?;
    let hash = runtime
        .run_record_hash
        .as_deref()
        .and_then(ContentHash::from_hex)
        .ok_or("intake attempt has no pinned snapshot")?;
    let bytes = octopus_interfaces::read_artifact_carrier_file_bounded(
        &ctx.blobs_dir.join(hash.to_hex()),
        ctx.required_host_io_limits()?.max_payload_bytes,
    )
    .map_err(|e| e.to_string())?;
    if ContentHash::of_bytes(&bytes) != hash {
        return Err("intake attempt snapshot failed integrity".into());
    }
    let snapshot = RunRecord::from_bytes(&bytes).map_err(|e| e.to_string())?;
    let binding = CheckpointBindingPolicySnapshot {
        inbound: checkpoint
            .interface_binding
            .clone()
            .ok_or("intake attempt has no binding")?,
        outbound: checkpoint.outbound_interface_bindings.clone(),
    };
    if row.kind != MovementKind::InterfaceOutbound
        || row.from != Endpoint::Interface(from.clone())
        || row.to != Endpoint::Interface(to.clone())
        || row.trace != original.trace
        || snapshot.trace != original.trace
        || snapshot.id != original.id
        || snapshot.ee != runtime.ee
        || snapshot.worker != original.worker
        || snapshot.checkpoint(&checkpoint.id) != Some(*checkpoint)
        || !snapshot.active_checkpoints.contains(&checkpoint.id)
        || runtime.run != original.id
        || runtime.worker != original.worker
        || runtime.interface != *to
        || runtime.checkpoint != checkpoint.id
        || runtime.epoch != checkpoint.epoch.0
        || metadata.checkpoint_binding_policy.as_ref() != Some(&binding)
        || metadata.outbound_interface_bindings != [to.clone()]
        || metadata.required_verified_artifacts != checkpoint.required_verified_artifacts
        || checkpoint
            .required_verified_artifacts
            .iter()
            .any(|required| {
                !metadata
                    .grant
                    .mark_outcomes
                    .contains(&required.mark.to_string())
            })
        || metadata.verification_failure.is_some()
        || !metadata
            .checkpoint_admission
            .as_ref()
            .is_some_and(|admission| {
                admission.complete
                    && admission.artifact_label == *label
                    && admission.required_artifact_labels
                        == checkpoint
                            .required_verified_artifacts
                            .iter()
                            .map(|required| required.artifact_label.clone())
                            .collect::<Vec<_>>()
            })
        || metadata.artifact_binding_refs.len() != checkpoint.artifact_bindings.len()
        || !metadata
            .artifact_binding_refs
            .iter()
            .zip(&checkpoint.artifact_bindings)
            .all(|(reference, bound)| {
                reference.id == bound.id
                    && metadata
                        .grant
                        .artifact_access
                        .get(bound.id.as_str())
                        .is_some_and(|access| access.read)
            })
        || (!result
            && (!metadata.grant.tools.contains(&writer.tool)
                || metadata.grant.hard_blocked_tools.contains(&writer.tool)
                || !metadata.grant.interfaces.contains(&writer.door.to_string())))
    {
        return Err("intake attempt differs from its exact admitted writer boundary".into());
    }
    Ok(())
}

pub fn checked_input_result(
    ctx: &InterfaceLiftContext,
    config: &InputIntakeConfig,
    pending: &IntakeEdit,
    rows: &[Movement],
) -> Result<Option<ToolResultArtifact>, String> {
    let mut found = None;
    for row in rows.iter().filter(|row| {
        row.seq.0 > pending.after_seq
            && row.kind == MovementKind::InterfaceOutbound
            && row.from == Endpoint::Interface(config.writer.door.clone())
            && row.to == Endpoint::Interface(config.writer.owner.clone())
    }) {
        let metadata = row
            .invocation_metadata
            .as_ref()
            .and_then(|m| m.invocation())
            .ok_or("intake writer result has no metadata")?;
        if metadata
            .runtime
            .as_ref()
            .is_none_or(|r| r.run.as_str() != config.run)
        {
            continue;
        }
        let captured = read_evidence_artifact(
            ctx,
            row.artifact
                .as_ref()
                .ok_or("intake result has no artifact")?,
        )?;
        if captured.manifest.label != config.writer.result_label {
            continue;
        }
        let result: ToolResultArtifact =
            serde_json::from_slice(&captured.payload_bytes).map_err(|e| e.to_string())?;
        if result.tool != config.writer.tool
            || result.correlation_id.as_deref() != Some(&pending.edit.correlation_id)
        {
            continue;
        }
        result.validate()?;
        if found.is_some() {
            return Err("intake writer has ambiguous duplicate results".into());
        }
        if !result.ok {
            if result.body.is_some()
                || result
                    .error
                    .as_ref()
                    .is_none_or(|error| error.details.is_some())
            {
                return Err("intake rejection is not an ordinary failed tool result".into());
            }
            let original =
                RunRecord::from_bytes(pending.before.as_bytes()).map_err(|e| e.to_string())?;
            validate_attempt_edge(ctx, config, &original, row, true)?;
            let mut call = None;
            for candidate in rows.iter().filter(|candidate| {
                candidate.seq.0 > pending.after_seq
                    && candidate.seq < row.seq
                    && candidate.kind == MovementKind::InterfaceOutbound
                    && candidate.from == Endpoint::Interface(config.writer.source.clone())
                    && candidate.to == Endpoint::Interface(config.writer.door.clone())
            }) {
                if !pending.matches_call(ctx, config, candidate)? {
                    continue;
                }
                validate_attempt_edge(ctx, config, &original, candidate, false)?;
                if call.replace(candidate).is_some() {
                    return Err("intake rejection has ambiguous exact calls".into());
                }
            }
            if call.is_none() {
                return Err("intake rejection has no exact admitted call".into());
            }
        }
        found = Some(result);
    }
    Ok(found)
}

pub fn admitted_input(
    rows: &[Movement],
    config: &InputIntakeConfig,
    hash: &ContentHash,
    after_seq: u64,
) -> Result<Option<u64>, String> {
    let rows = rows
        .iter()
        .filter(|row| {
            row.kind == MovementKind::InterfaceOutbound
                && row.seq.0 > after_seq
                && row.from == Endpoint::Interface(config.writer.owner.clone())
                && row.to == Endpoint::Interface(config.provider_interface.clone())
                && row
                    .artifact
                    .as_ref()
                    .is_some_and(|artifact| artifact.content == *hash)
        })
        .collect::<Vec<_>>();
    if rows.len() > 1 {
        return Err("input was admitted more than once across checkpoint generations".into());
    }
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    let metadata = row
        .invocation_metadata
        .as_ref()
        .and_then(|m| m.invocation())
        .ok_or("input admission has no metadata")?;
    let runtime = metadata
        .runtime
        .as_ref()
        .ok_or("input admission has no receiver run")?;
    if runtime.run.as_str() != config.run
        || runtime.interface != config.provider_interface
        || metadata.verification_failure.is_some()
        || !metadata
            .checkpoint_admission
            .as_ref()
            .is_some_and(|a| a.complete && a.artifact_label == config.input_label)
        || metadata
            .checkpoint_binding_policy
            .as_ref()
            .is_none_or(|b| b.inbound.interface != config.writer.owner)
    {
        return Err("input admission differs from its exact receiver declaration".into());
    }
    Ok(Some(row.seq.0))
}

pub fn prepare_input(
    config: &InputIntakeConfig,
    path: PathBuf,
    current: &[u8],
    source_seq: u64,
    source_artifact: &ArtifactRef,
    after_seq: u64,
    max_payload_bytes: u64,
) -> Result<IntakeEdit, String> {
    let mut pending = prepare_input_edit(config,path,current,source_seq,source_artifact,after_seq,Some(JsonFormat::Compact))?;
    pending.ordered = OwnerWriterEdit::prepare(current,&pending.edit,&config.writer,max_payload_bytes)?;
    Ok(pending)
}

fn prepare_input_edit(config: &InputIntakeConfig, path: PathBuf, current: &[u8], source_seq: u64, source_artifact: &ArtifactRef, after_seq: u64, json_format: Option<JsonFormat>) -> Result<IntakeEdit,String> {
    config.validate(&config.writer.owner)?;
    let original = RunRecord::from_bytes(current).map_err(|e| e.to_string())?;
    if original.id.as_str() != config.run {
        return Err("input intake run differs from its declaration".into());
    }
    let owner = config.writer.owner.as_str();
    let mut next: Value = serde_json::from_slice(current).map_err(|e| e.to_string())?;
    if !config.matches_declaration(&next["interface_state"][owner]["input_intake"])? {
        return Err("input intake descriptor differs from the run's authored declaration".into());
    }
    let before_owned: BTreeSet<String> =
        serde_json::from_value(next["interface_state"][owner]["owned_checkpoint_ids"].clone())
            .map_err(|e| e.to_string())?;
    let detached = owner_writer_binding(&original,&config.writer.owner)?.is_some();
    if before_owned.len() != if detached { 1 } else { 3 } {
        return Err("input intake must own exactly input/call/result checkpoints".into());
    }
    let generation = format!(
        "{source_seq}_{}",
        &ContentHash::of_bytes(current).to_hex()[..24]
    );
    let (mut rows, mut ids) = initial_checkpoints(config, &next["trace"], &generation)?;
    let source = octopus_run_authoring::input_artifacts::InputArtifactBinding {
        artifact: source_artifact.clone(), source_movement_seq: source_seq,
    };
    let mut sources = octopus_run_authoring::input_artifacts::selections(&original, &config.writer.owner)?;
    if !sources.iter().any(|prior| prior.artifact == source.artifact) { sources.push(source); }
    let context = provider_context_bindings(&original, &config.provider_interface)?;
    rows[0]["artifact_bindings"] = json!(sources.iter().try_fold(context, |context, source| source.read_bindings(context))?);
    next["interface_state"][owner]["input_artifact_bindings"] = json!(sources);
    let frontier = if detached { rows.truncate(1); ids.truncate(1); vec![] } else { vec![ids[1].clone(),ids[2].clone()] };
    next["checkpoints"]
        .as_array_mut()
        .ok_or("input intake run has no checkpoints")?
        .retain(|cp| {
            !cp["id"]
                .as_str()
                .is_some_and(|id| before_owned.contains(id))
        });
    next["active_checkpoints"]
        .as_array_mut()
        .ok_or("input intake run has no active checkpoints")?
        .retain(|cp| !cp.as_str().is_some_and(|id| before_owned.contains(id)));
    next["checkpoints"]
        .as_array_mut()
        .ok_or("input intake run has no checkpoints")?
        .extend(rows);
    next["active_checkpoints"]
        .as_array_mut()
        .ok_or("input intake run has no active checkpoints")?
        .extend(ids.iter().map(|id| json!(id)));
    next["interface_state"][owner]["owned_checkpoint_ids"] = json!(ids);
    next["interface_state"][owner]["frontier_checkpoint_ids"] = json!(frontier);
    let edit = CheckedRunEdit::prepare_with_format(path, current, next, format!("input:{owner}:{generation}"), json_format)?;
    validate_unchanged_run_neighbors(
        current,
        edit.content.as_bytes(),
        &config.writer.owner,
        &before_owned,
        &ids.into_iter().collect(),
        false,
    )?;
    Ok(IntakeEdit {
        before: String::from_utf8(current.to_vec()).map_err(|e| e.to_string())?,
        after_seq,
        edit,
        ordered: None,
    })
}

pub fn load_state(path: &Path, maximum: u64) -> Result<IntakeState, String> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata)
            if !metadata.is_file()
                || metadata.file_type().is_symlink()
                || metadata.len() > maximum =>
        {
            return Err("intake state is not a bounded regular private file".into());
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(IntakeState::default());
        }
        Err(error) => return Err(error.to_string()),
    }
    serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())
}

pub fn save_state(path: &Path, state: &IntakeState, maximum: u64) -> Result<(), String> {
    let bytes = serde_json::to_vec(state).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > maximum {
        return Err("intake state exceeds its declared private file bound".into());
    }
    let temporary = path.with_extension("pending");
    if std::fs::symlink_metadata(&temporary)
        .is_ok_and(|metadata| metadata.file_type().is_symlink() || !metadata.is_file())
    {
        return Err("intake temporary state is not a regular private file".into());
    }
    let mut options = std::fs::OpenOptions::new();
    options.create(true).write(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary).map_err(|e| e.to_string())?;
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())?;
    std::fs::rename(&temporary, path).map_err(|e| e.to_string())?;
    std::fs::File::open(path.parent().ok_or("intake state has no parent")?)
        .and_then(|file| file.sync_all())
        .map_err(|e| e.to_string())
}
