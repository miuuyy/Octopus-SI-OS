use std::path::{Component, Path, PathBuf};

use octopus_descriptors::{ArtifactId, InterfaceId};
use octopus_interface_transport::SpawnLifeConfig;
use octopus_interfaces::{InterfaceArtifactFile, InterfaceArtifactManifest};
use octopus_ledger::{ArtifactRef, ContentHash, Endpoint, MovementKind};
use octopus_run_authoring::{
    CheckedRunEdit, CheckedRunEvidence, CheckedRunEvidenceReader, CheckedRunInstall,
    CheckedRunWriter, ConsentEvidenceRef, FileCheckedRunReader, OwnerWriterEdit,
    ProviderInputRevision, RunAuthoringProfile, checked_declared_run_advance,
    checked_declared_run_install, payload_artifact_bindings, prepare_provider_input_payloads,
    prepare_provider_input_rearm, provider_context_bindings,
    provider_input_payloads_ready, provider_input_rearm_ready,
    validate_unchanged_admitted_checkpoint,
};
use octopus_run_record::RunRecord;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const PROPOSAL_LABEL: &str = "operator.run_revision.proposal";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RevisionOperation {
    OwnerInputs { desired_inputs: Vec<Value> },
    Rearm,
    Payloads { selected: Vec<ArtifactRef> },
}

/// Ordinary exact-record authoring may append future checkpoints while keeping
/// accepted contracts and provider continuity as data. Replacing
/// input generations or retiring active waits belongs to the existing typed
/// provider input revision/rearm path with its admitted writer evidence.
pub fn validate_checkpoint_extension(before: &[u8], after: &[u8]) -> Result<(), String> {
    let before = RunRecord::from_bytes(before).map_err(|error| error.to_string())?;
    let after = RunRecord::from_bytes(after).map_err(|error| error.to_string())?;
    if before.id != after.id
        || before.ee != after.ee
        || before.worker != after.worker
        || before.trace != after.trace
        || before.status != after.status
        || before.interface_state != after.interface_state
        || !after.checkpoints.starts_with(&before.checkpoints)
        || before
            .active_checkpoints
            .iter()
            .any(|checkpoint| !after.active_checkpoints.contains(checkpoint))
    {
        return Err("exact run revision must preserve identity, provider state, existing checkpoints and active waits; generation replacement requires the typed input revision owner".into());
    }
    if after
        .active_checkpoints
        .iter()
        .any(|id| !before.active_checkpoints.contains(id) && before.checkpoint(id).is_some())
    {
        return Err(
            "exact run revision cannot reactivate a historical checkpoint; author a fresh id/epoch"
                .into(),
        );
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunRevisionRequest {
    pub registry_root: PathBuf,
    pub run_path: PathBuf,
    pub ledger_root: PathBuf,
    pub artifact_root: PathBuf,
    pub interface_hand_in_root: PathBuf,
    pub provider: InterfaceId,
    pub source: InterfaceId,
    pub key: String,
    pub operation: RevisionOperation,
    pub expected_before_content: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunRevisionVerification {
    pub registry_root: PathBuf,
    pub run_path: PathBuf,
    pub ledger_root: PathBuf,
    pub artifact_root: PathBuf,
    pub interface_hand_in_root: PathBuf,
    pub provider: InterfaceId,
    pub source: InterfaceId,
    pub key: String,
    pub selected_payloads: Option<Vec<ArtifactRef>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputPayloadInspection {
    pub run_record_content: String,
    pub provider: InterfaceId,
}

pub fn inspect_input_payloads(request: InputPayloadInspection) -> Result<Value, String> {
    Ok(json!({"selected_payloads": selected_payloads(
        request.run_record_content.as_bytes(), &request.provider,
    )?}))
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Proposal {
    schema_version: u32,
    key: String,
    source: InterfaceId,
    before: String,
    after_seq: u64,
    operation: RevisionOperation,
    writer: CheckedRunWriter,
    edit: CheckedRunEdit,
    ordered: Option<OwnerWriterEdit>,
    call_path: PathBuf,
}

struct Scope {
    writer: CheckedRunWriter,
    profile: RunAuthoringProfile,
    limits: octopus_interface_transport::InterfaceHostIoLimits,
    root: PathBuf,
    call_root: PathBuf,
    proposal_path: PathBuf,
    movements: PathBuf,
    index: PathBuf,
    blobs: PathBuf,
}

impl Scope {
    fn open(request: &RunRevisionVerification) -> Result<Self, String> {
        for path in [
            &request.registry_root,
            &request.run_path,
            &request.ledger_root,
            &request.artifact_root,
            &request.interface_hand_in_root,
        ] {
            normal_path(path)?;
        }
        crate::safe_segment(request.provider.as_str(), "revision provider")?;
        crate::safe_segment(request.source.as_str(), "revision source")?;
        if request.key.is_empty()
            || request.key.len() > 192
            || !request
                .key
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphanumeric())
            || !request
                .key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        {
            return Err("run revision key must be an exact file segment".into());
        }
        let value: Value = serde_json::from_slice(&read_regular(&request.run_path, 268_435_456)?)
            .map_err(|error| error.to_string())?;
        let record =
            RunRecord::from_bytes(&serde_json::to_vec(&value).map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())?;
        if request.run_path.file_name().and_then(|v| v.to_str())
            != Some(&format!("{}.json", record.id))
            || value["interface_state"][request.source.as_str()]["provider_runtime_interface"]
                .as_str()
                != Some(request.provider.as_str())
        {
            return Err("run revision lost its exact client, provider or Run path".into());
        }
        let descriptor: Value = serde_json::from_slice(&read_regular(
            &request
                .registry_root
                .join("interface")
                .join(request.provider.as_str())
                .join("interface.json"),
            1_048_576,
        )?)
        .map_err(|error| error.to_string())?;
        if descriptor["id"].as_str() != Some(request.provider.as_str()) {
            return Err("run revision provider descriptor changed identity".into());
        }
        let profile: RunAuthoringProfile = serde_json::from_value(
            descriptor
                .pointer("/interface_driver_config/private_config/run_authoring_profile")
                .cloned()
                .ok_or("run revision requires the declared provider write profile")?,
        )
        .map_err(|error| error.to_string())?;
        let topology =
            &value["interface_state"][request.provider.as_str()]["run_authoring_binding"];
        let source = topology["authoring_source_interface"]
            .as_str()
            .ok_or("run revision writer source absent")?;
        let rows = topology["checkpoints"]
            .as_array()
            .ok_or("run revision topology absent")?;
        let returns = rows
            .iter()
            .filter(|row| row["role"] == "authoring_result")
            .collect::<Vec<_>>();
        let [returned] = returns.as_slice() else {
            return Err("run revision requires one declared writer return".into());
        };
        let door = returned["interface_binding"]["interface"]
            .as_str()
            .ok_or("run revision writer door absent")?;
        crate::safe_segment(source, "revision writer source")?;
        crate::safe_segment(door, "revision writer door")?;
        let writer = CheckedRunWriter {
            owner: request.provider.clone(),
            source: InterfaceId::new(source),
            door: InterfaceId::new(door),
            tool: profile.file_write_tool.clone(),
            call_label: profile.file_write_label.clone(),
            result_label: profile.file_write_result_label.clone(),
        };
        let life: SpawnLifeConfig = serde_json::from_slice(&read_regular(
            &request
                .registry_root
                .join("interface")
                .join(source)
                .join("life.json"),
            1_048_576,
        )?)
        .map_err(|error| error.to_string())?;
        let [process] = life.processes.as_slice() else {
            return Err("run revision source requires one declared hand-in process".into());
        };
        process.io_limits.validate()?;
        let root = request.artifact_root.join("input-revisions");
        let call_root = request.interface_hand_in_root.join(source).join("input-revisions");
        Ok(Self {
            writer,
            profile,
            limits: process.io_limits,
            proposal_path: root.join(format!("{}.artifact.json", request.key)),
            root,
            call_root,
            movements: request.ledger_root.join("movements.jsonl"),
            index: request.ledger_root.join("ledger-index.sqlite"),
            blobs: request.ledger_root.join("blobs"),
        })
    }

    fn evidence(&self) -> CheckedRunEvidence<'_> {
        CheckedRunEvidence {
            movements_path: &self.movements,
            ledger_index_path: &self.index,
            blobs_dir: &self.blobs,
            max_control_frame_bytes: self.limits.max_control_frame_bytes,
            max_manifest_bytes: self.limits.max_manifest_bytes,
            max_payload_bytes: self.limits.max_payload_bytes,
        }
    }

    fn load(&self, request: &RunRevisionVerification) -> Result<Proposal, String> {
        let (_, payload, _) = self.capture(&self.root, &self.proposal_path, PROPOSAL_LABEL)?;
        let proposal: Proposal =
            serde_json::from_slice(&payload).map_err(|error| error.to_string())?;
        if proposal.schema_version != 1
            || proposal.key != request.key
            || proposal.source != request.source
            || proposal.writer != self.writer
            || proposal.edit.path != request.run_path
        {
            return Err("run revision proposal changed its exact owner or target".into());
        }
        let edit = match &proposal.operation {
            RevisionOperation::OwnerInputs { desired_inputs } => {
                match self.evidence().prepare_provider_input_revision(
                    request.run_path.clone(),
                    proposal.before.as_bytes(),
                    &proposal.writer,
                    desired_inputs.clone(),
                    proposal.after_seq.checked_add(1).ok_or("run revision evidence boundary overflow")?,
                )? {
                    ProviderInputRevision::Prepared { edit } => edit,
                    _ => {
                        return Err(
                            "saved input revision is no longer an exact prepared edit".into()
                        );
                    }
                }
            }
            RevisionOperation::Rearm => prepare_provider_input_rearm(
                request.run_path.clone(),
                proposal.before.as_bytes(),
                &proposal.writer,
                proposal.source.clone(),
            )?,
            RevisionOperation::Payloads { selected } => {
                if request.selected_payloads.as_ref() != Some(selected) {
                    return Err(
                        "run revision retry changed its exact selected payload artifacts".into(),
                    );
                }
                prepare_provider_input_payloads(
                    request.run_path.clone(),
                    proposal.before.as_bytes(),
                    &proposal.writer,
                    proposal.source.clone(),
                    selected.clone(),
                )?
            }
        };
        if serde_json::to_value(edit).map_err(|e| e.to_string())?
            != serde_json::to_value(&proposal.edit).map_err(|e| e.to_string())?
        {
            return Err("run revision proposal differs from its exact typed transition".into());
        }
        let before: Value = serde_json::from_str(&proposal.before).map_err(|e| e.to_string())?;
        if let (RevisionOperation::Rearm, Some(selected)) =
            (&proposal.operation, &request.selected_payloads)
        {
            let retained = selected_payloads(proposal.before.as_bytes(), &proposal.writer.owner)?;
            if *selected != retained {
                return Err("run rearm retry changed its retained payload selection".into());
            }
        }
        if proposal.ordered.is_some()
            != before["interface_state"][proposal.writer.owner.as_str()]["run_authoring_binding"]
                .get("writer_run")
                .is_some_and(|value| !value.is_null())
        {
            return Err(
                "run revision call changed its declared embedded or separate writer".into(),
            );
        }
        let (_, call, _) = self.capture_call(&proposal.call_path)?;
        let call: Value = serde_json::from_slice(&call).map_err(|e| e.to_string())?;
        if let Some(ordered) = &proposal.ordered {
            ordered.validate(
                proposal.before.as_bytes(),
                &proposal.edit,
                &proposal.writer,
                self.limits.max_payload_bytes,
            )?;
            if !ordered.matches_call(&call) {
                return Err("saved ordered run call differs from its prospective artifact".into());
            }
        } else if call
            != json!({"tool":proposal.writer.tool,"correlation_id":proposal.edit.correlation_id,"arguments":proposal.edit.arguments()?})
        {
            return Err("saved embedded run call differs from its prospective artifact".into());
        }
        Ok(proposal)
    }

    fn capture_call(&self, path: &Path) -> Result<(InterfaceArtifactManifest, Vec<u8>, ArtifactRef), String> {
        normal_path(path)?;
        let parent = std::fs::canonicalize(path.parent().ok_or("revision call has no parent")?)
            .map_err(|e| e.to_string())?;
        let proposal_root = std::fs::canonicalize(&self.root).map_err(|e| e.to_string())?;
        let root = if parent == proposal_root { &self.root } else { &self.call_root };
        self.capture(root, path, &self.writer.call_label)
    }

    fn capture(
        &self,
        root: &Path,
        path: &Path,
        label: &str,
    ) -> Result<(InterfaceArtifactManifest, Vec<u8>, ArtifactRef), String> {
        normal_path(path)?;
        let root = std::fs::canonicalize(root).map_err(|e| e.to_string())?;
        if std::fs::canonicalize(path.parent().ok_or("revision artifact has no parent")?)
            .map_err(|e| e.to_string())?
            != root
        {
            return Err("run revision artifact is outside its exact owner root".into());
        }
        let bytes = read_regular(path, self.limits.max_manifest_bytes)?;
        let manifest: InterfaceArtifactManifest =
            serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        if manifest.label != label
            || manifest
                .payload_content_hash
                .as_ref()
                .and_then(|hash| ContentHash::from_hex(hash))
                .is_none_or(|hash| {
                    manifest.payload_content_hash.as_deref() != Some(hash.to_hex().as_str())
                })
        {
            return Err(
                "run revision carrier has no exact label and canonical integrity pin".into(),
            );
        }
        normal_path(&manifest.payload_path)?;
        if manifest.payload_path.parent() != Some(root.as_path()) {
            return Err("run revision payload is outside its exact owner root".into());
        }
        let payload = read_regular(&manifest.payload_path, self.limits.max_payload_bytes)?;
        InterfaceArtifactFile::from_captured_snapshot(path, bytes.clone(), payload.clone())
            .map_err(|e| e.to_string())?;
        let hash = ContentHash::of_bytes(&bytes);
        Ok((
            manifest,
            payload,
            ArtifactRef {
                id: ArtifactId::new(format!("{label}:{}", hash.to_hex())),
                content: hash,
            },
        ))
    }
}

pub fn prepare_run_revision(request: RunRevisionRequest) -> Result<Value, String> {
    let verify = RunRevisionVerification {
        registry_root: request.registry_root,
        run_path: request.run_path,
        ledger_root: request.ledger_root,
        artifact_root: request.artifact_root,
        interface_hand_in_root: request.interface_hand_in_root,
        provider: request.provider,
        source: request.source,
        key: request.key,
        selected_payloads: match &request.operation {
            RevisionOperation::Payloads { selected } => Some(selected.clone()),
            _ => None,
        },
    };
    let scope = Scope::open(&verify)?;
    if scope
        .proposal_path
        .try_exists()
        .map_err(|e| e.to_string())?
    {
        let proposal = scope.load(&verify)?;
        if proposal.operation != request.operation {
            return Err("run revision retry changed its original requested operation".into());
        }
        return Ok(json!({"status":"prepared"}));
    }
    let before = read_regular(&verify.run_path, scope.limits.max_payload_bytes)?;
    if before != request.expected_before_content.as_bytes() {
        return Err("run revision preimage changed before preparation".into());
    }
    let evidence = scope.evidence();
    let record = RunRecord::from_bytes(&before).map_err(|e| e.to_string())?;
    let prior = record
        .interface_state
        .get(&scope.writer.owner)
        .and_then(|state| state.get("pending_handoff"))
        .and_then(|pending| pending.get("source_movement_seq"))
        .map(|value| {
            value
                .as_u64()
                .ok_or("run revision pending source is malformed")
        })
        .transpose()?
        .unwrap_or(0);
    let mut reader = FileCheckedRunReader::open(&evidence, Some(prior))?;
    let mut after_seq = prior;
    while let Some(row) = reader.movement(reader.next_sequence())? {
        after_seq = row.seq.0;
    }
    let edit = match &request.operation {
        RevisionOperation::OwnerInputs { desired_inputs } => {
            if !owner_input_ready(&evidence, &verify.run_path, &before, &scope.writer, &verify.source)? {
                return Ok(json!({"status":"deferred","checkpoints":record.active_checkpoints}));
            }
            match evidence.prepare_provider_input_revision(
                verify.run_path.clone(),
                &before,
                &scope.writer,
                desired_inputs.clone(),
                after_seq.checked_add(1).ok_or("run revision evidence boundary overflow")?,
            )? {
                ProviderInputRevision::Prepared { edit } => edit,
                ProviderInputRevision::Unchanged => return Ok(json!({"status":"unchanged"})),
                ProviderInputRevision::Deferred { checkpoints } => {
                    return Ok(json!({"status":"deferred","checkpoints":checkpoints}));
                }
            }
        }
        RevisionOperation::Rearm => {
            let record = RunRecord::from_bytes(&before).map_err(|e| e.to_string())?;
            if !provider_input_rearm_ready(&evidence, &record, &scope.writer.owner)? {
                return Ok(json!({"status":"deferred","checkpoints":record.active_checkpoints}));
            }
            prepare_provider_input_rearm(
                verify.run_path.clone(),
                &before,
                &scope.writer,
                verify.source.clone(),
            )?
        }
        RevisionOperation::Payloads { selected } => {
            if !provider_input_payloads_ready(
                &evidence,
                &verify.run_path,
                &before,
                &scope.writer,
                &verify.source,
            )? {
                return Ok(json!({"status":"deferred","checkpoints":record.active_checkpoints}));
            }
            prepare_provider_input_payloads(
                verify.run_path.clone(),
                &before,
                &scope.writer,
                verify.source.clone(),
                selected.clone(),
            )?
        }
    };
    let ordered = OwnerWriterEdit::prepare(
        &before,
        &edit,
        &scope.writer,
        scope.limits.max_payload_bytes,
    )?;
    let call = match &ordered {
        Some(ordered) => ordered.materialize(&scope.call_root)?,
        None => edit.materialize(&scope.call_root, &scope.profile)?,
    };
    let manifest = call
        .read_manifest_bounded(scope.limits.max_manifest_bytes)
        .map_err(|e| e.to_string())?;
    let call = InterfaceArtifactFile::store_manifest_bounded(
        &scope.call_root,
        &serde_json::to_vec(&manifest.manifest).map_err(|e| e.to_string())?,
        scope.limits.max_manifest_bytes,
        scope.limits.max_payload_bytes,
    )
    .map_err(|e| e.to_string())?;
    octopus_interface_transport::capture_spawned_artifact(
        call.manifest_path(), std::slice::from_ref(&scope.call_root), scope.limits,
    )?;
    let proposal = Proposal {
        schema_version: 1,
        key: verify.key.clone(),
        source: verify.source.clone(),
        before: String::from_utf8(before.clone()).map_err(|e| e.to_string())?,
        after_seq,
        operation: request.operation,
        writer: scope.writer.clone(),
        edit,
        ordered,
        call_path: call.manifest_path().to_path_buf(),
    };
    if read_regular(&verify.run_path, scope.limits.max_payload_bytes)? != before {
        return Err("run revision changed while preparing its call".into());
    }
    let retained = InterfaceArtifactFile::write(
        &scope.root,
        PROPOSAL_LABEL,
        &serde_json::to_vec(&proposal).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    match std::fs::hard_link(retained.manifest_path(), &scope.proposal_path) {
        Ok(()) => std::fs::File::open(&scope.root)
            .and_then(|file| file.sync_all())
            .map_err(|e| e.to_string())?,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.to_string()),
    }
    let saved = scope.load(&verify)?;
    if saved.operation != proposal.operation {
        return Err("concurrent run revision changed the requested definition".into());
    }
    Ok(json!({"status":"prepared"}))
}

pub fn verify_run_revision(request: RunRevisionVerification) -> Result<Value, String> {
    let scope = Scope::open(&request)?;
    let proposal = scope.load(&request)?;
    let (manifest, _, artifact) =
        scope.capture_call(&proposal.call_path)?;
    let evidence = scope.evidence();
    let mut reader = FileCheckedRunReader::open(&evidence, Some(proposal.after_seq))?;
    let mut call = None;
    let mut result = None;
    let mut arrived = false;
    let mut refused = false;
    while let Some(row) = reader.movement(reader.next_sequence())? {
        if row.seq.0 <= proposal.after_seq {
            continue;
        }
        if row.artifact.as_ref() == Some(&artifact) {
            arrived |= row.kind == MovementKind::InterfaceArrival;
            refused |= matches!(
                row.kind,
                MovementKind::DeliveryRefused | MovementKind::VerificationFailed
            );
        }
        if row.kind != MovementKind::InterfaceOutbound {
            continue;
        }
        let is_call = row.from == Endpoint::Interface(scope.writer.source.clone())
            && row.to == Endpoint::Interface(scope.writer.door.clone());
        let is_result = row.from == Endpoint::Interface(scope.writer.door.clone())
            && row.to == Endpoint::Interface(scope.writer.owner.clone());
        if !is_call && !is_result {
            continue;
        }
        let reference = row
            .artifact
            .as_ref()
            .ok_or("run revision writer movement has no artifact")?;
        let (header, payload) = reader.artifact(reference)?;
        let expected_label = if is_call {
            &scope.writer.call_label
        } else {
            &scope.writer.result_label
        };
        if header.label.as_str() != expected_label.as_str() {
            continue;
        }
        let value: Value = serde_json::from_slice(&payload).map_err(|e| e.to_string())?;
        if value["correlation_id"].as_str() != Some(proposal.edit.correlation_id.as_str()) {
            continue;
        }
        if value["tool"].as_str() != Some(scope.writer.tool.as_str()) {
            return Err("run revision correlation changed its declared tool".into());
        }
        let proof = ConsentEvidenceRef {
            artifact: reference.clone(),
            movement_seq: row.seq.0,
        };
        if is_call {
            if *reference != artifact || call.replace(proof).is_some() {
                return Err(
                    "run revision call is ambiguous or differs from its retained artifact".into(),
                );
            }
        } else if result.replace(proof).is_some() {
            return Err("run revision result is ambiguous".into());
        }
    }
    let before_read = read_regular(&request.run_path, scope.limits.max_payload_bytes)?;
    if let (Some(call), Some(result)) = (&call, &result) {
        let install = CheckedRunInstall {
            call: call.clone(),
            result: result.clone(),
        };
        let transition = checked_declared_run_install(
            &evidence,
            &request.run_path,
            proposal.before.as_bytes(),
            &install,
            vec![scope.writer.clone()],
        )?;
        if transition.before != proposal.before.as_bytes()
            || transition.after != proposal.edit.content.as_bytes()
            || transition.writer != scope.writer
        {
            return Err("admitted run revision differs from its exact prospective edit".into());
        }
        checked_declared_run_advance(
            &evidence,
            &request.run_path,
            &transition.after,
            &before_read,
            install.result.movement_seq,
            vec![scope.writer.clone()],
        )?;
        if !matches!(proposal.operation, RevisionOperation::OwnerInputs { .. }) {
            let prospective =
                RunRecord::from_bytes(&transition.after).map_err(|e| e.to_string())?;
            let topology: crate::BoundRunAuthoringTopology = serde_json::from_value(
                prospective
                    .interface_state
                    .get(&proposal.writer.owner)
                    .and_then(|state| state.get("run_authoring_binding"))
                    .cloned()
                    .ok_or("checked revision has no declared provider topology")?,
            )
            .map_err(|e| e.to_string())?;
            let templates = topology
                .checkpoints
                .iter()
                .filter(|template| {
                    template.role == crate::BoundCheckpointRole::Stable
                        && template.include_context
                        && template.interface_binding.interface == proposal.source
                })
                .collect::<Vec<_>>();
            let [template] = templates.as_slice() else {
                return Err("checked revision has no unique declared stable client input".into());
            };
            let inputs = prospective
                .checkpoints
                .iter()
                .filter(|checkpoint| {
                    prospective.active_checkpoints.contains(&checkpoint.id)
                        && checkpoint.interface_binding.as_ref()
                            == Some(&template.interface_binding)
                        && checkpoint.outbound_interface_bindings
                            == template.outbound_interface_bindings
                        && checkpoint.required_verified_artifacts
                            == template.required_verified_artifacts
                })
                .collect::<Vec<_>>();
            let [input] = inputs.as_slice() else {
                return Err("checked revision has no unique exact client input generation".into());
            };
            validate_unchanged_admitted_checkpoint(&transition.after, &before_read, &input.id)?;
            let expected = match &proposal.operation {
                RevisionOperation::Payloads { selected } => selected.clone(),
                _ => selected_payloads(proposal.before.as_bytes(), &proposal.writer.owner)?,
            };
            if selected_payloads(&before_read, &proposal.writer.owner)? != expected {
                return Err("checked revision input changed its selected payload artifacts".into());
            }
        }
        if !input_ready(&proposal, &evidence, &before_read)? {
            return Err("run revision input is no longer ready for its next message".into());
        }
        if read_regular(&request.run_path, scope.limits.max_payload_bytes)? != before_read {
            return Err("run changed during checked revision verification".into());
        }
        return Ok(
            json!({"status":"installed","path":request.run_path,"before_hash":proposal.edit.before_hash,
            "after_hash":proposal.edit.after_hash,"install":install}),
        );
    }
    if call.is_none() && (result.is_some() || refused) {
        return Err("run revision has no admitted matching writer call".into());
    }
    if call.is_some() || arrived {
        return Ok(json!({"status":"awaiting_result"}));
    }
    if before_read != proposal.before.as_bytes() {
        return Err("run revision preimage changed without its checked installation".into());
    }
    if let RevisionOperation::OwnerInputs { desired_inputs } = &proposal.operation {
        let ProviderInputRevision::Prepared { edit } = evidence.prepare_provider_input_revision(
            request.run_path.clone(), &before_read, &proposal.writer, desired_inputs.clone(), reader.next_sequence(),
        )? else { return Err("saved owner input correction is no longer prepared".into()); };
        if serde_json::to_value(&edit).map_err(|error| error.to_string())?
            != serde_json::to_value(&proposal.edit).map_err(|error| error.to_string())?
        { return Err("saved owner input correction changed before its writer effect".into()); }
    }
    if !input_ready(&proposal, &evidence, &before_read)? {
        return Ok(json!({"status":"deferred"}));
    }
    if proposal.ordered.is_some() {
        let (_, bytes, _) = scope.capture_call(&proposal.call_path)?;
        let call: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        let path = Path::new(
            call.pointer("/arguments/writes/0/path")
                .and_then(Value::as_str)
                .ok_or("ordered writer path absent")?,
        );
        normal_path(path)?;
        let expected = call
            .pointer("/arguments/writes/0/expected_before_hash")
            .and_then(Value::as_str)
            .ok_or("ordered writer preimage absent")?;
        if ContentHash::of_bytes(&read_regular(path, scope.limits.max_payload_bytes)?).to_hex()
            != expected
        {
            return Err("run revision writer advanced before its saved call was admitted".into());
        }
    }
    if !scope.call_root.exists() || std::fs::canonicalize(proposal.call_path.parent().ok_or("revision call has no parent")?)
        .map_err(|e| e.to_string())? != std::fs::canonicalize(&scope.call_root).map_err(|e| e.to_string())?
    {
        return Err("retained revision call is outside its source-owned output root; explicit reprepare requires proof that the original call had no admitted effect".into());
    }
    octopus_interface_transport::capture_spawned_artifact(
        &proposal.call_path, std::slice::from_ref(&scope.call_root), scope.limits,
    )?;
    Ok(json!({"status":"ready","source_interface":scope.writer.source,"manifest":manifest}))
}

fn normal_path(path: &Path) -> Result<(), String> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, Component::RootDir | Component::Normal(_)))
    {
        return Err("run revision requires normal absolute paths".into());
    }
    Ok(())
}

fn input_ready(
    proposal: &Proposal,
    evidence: &CheckedRunEvidence<'_>,
    bytes: &[u8],
) -> Result<bool, String> {
    let record = RunRecord::from_bytes(bytes).map_err(|e| e.to_string())?;
    match proposal.operation {
        RevisionOperation::OwnerInputs { .. } => {
            owner_input_ready(evidence, &proposal.edit.path, bytes, &proposal.writer, &proposal.source)
        }
        RevisionOperation::Rearm => {
            provider_input_rearm_ready(evidence, &record, &proposal.writer.owner)
        }
        RevisionOperation::Payloads { .. } => provider_input_payloads_ready(
            evidence,
            &proposal.edit.path,
            bytes,
            &proposal.writer,
            &proposal.source,
        ),
    }
}

fn owner_input_ready(
    evidence: &CheckedRunEvidence<'_>,
    path: &Path,
    current: &[u8],
    writer: &CheckedRunWriter,
    source: &InterfaceId,
) -> Result<bool, String> {
    let record = RunRecord::from_bytes(current).map_err(|error|error.to_string())?;
    let state = record
        .interface_state
        .get(&writer.owner)
        .ok_or("provider state absent")?;
    if state.get("pending_handoff").is_none() {
        provider_input_payloads_ready(evidence, path, current, writer, source)
    } else {
        Ok(true)
    }
}

fn selected_payloads(bytes: &[u8], provider: &InterfaceId) -> Result<Vec<ArtifactRef>, String> {
    let record = RunRecord::from_bytes(bytes).map_err(|e| e.to_string())?;
    let context = provider_context_bindings(&record, provider)?;
    Ok(payload_artifact_bindings(&record, provider)?
        .into_iter()
        .filter(|reference| {
            context
                .iter()
                .any(|binding| binding.id == reference.id && binding.access.read)
        })
        .collect())
}

fn read_regular(path: &Path, maximum: u64) -> Result<Vec<u8>, String> {
    let metadata = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.len() > maximum {
        return Err("run revision file is not a bounded regular file".into());
    }
    octopus_interfaces::read_artifact_carrier_file_bounded(path, maximum).map_err(|e| e.to_string())
}
