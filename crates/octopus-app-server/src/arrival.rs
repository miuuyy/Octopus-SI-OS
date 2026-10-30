use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;

use octopus_descriptors::{CheckpointId, InterfaceId};
use octopus_interfaces::{
    DeliveryVerdict, InterfaceArtifactFile, InterfaceArtifactFileError, InterfaceArtifactManifest,
    RefusedReason,
};
use octopus_ledger::{
    ArtifactRef, BlobStore, ContentHash, Endpoint, Lamport, Movement, MovementDraft, MovementKind,
    MovementLog, MovementMetadata, TraceId,
};
use octopus_runtime::{
    InterfaceArrivalTarget, InterfaceOutboundArtifactTransfer, RunId, RuntimeError,
};
use octopus_run_record::GrantEpoch;

mod retained;

#[derive(Clone, Debug)]
pub struct InterfaceArrivalRecord {
    pub run: RunId,
    pub checkpoint: CheckpointId,
    pub outbound_bindings: Vec<InterfaceId>,
    pub epoch: GrantEpoch,
    pub arrival_movement: Movement,
    pub admitted_mark_outcome_movements: Vec<Movement>,
    pub outbound_transfer_movements: Vec<Movement>,
    pub outbound_refusal_movements: Vec<Movement>,
    pub artifact_root: PathBuf,
    pub outbound_transfers: Vec<InterfaceOutboundArtifactTransfer>,
    pub gated: bool,
}

#[derive(Clone, Debug)]
pub struct PinnedInterfaceArtifactPayload {
    pub artifact: ArtifactRef,
    pub artifact_label: String,
    pub payload_bytes: Vec<u8>,
    pub artifact_root: PathBuf,
}

pub(crate) struct InterfaceArrivalDispatch {
    pub records: Vec<InterfaceArrivalRecord>,
}

pub async fn recover_runtime_relay_arrivals(
    runtime: &octopus_runtime::Runtime,
    log: &mut (dyn MovementLog + Send),
    blobs: &mut dyn BlobStore,
    dedup: std::sync::Arc<std::sync::Mutex<octopus_ledger_index::ArrivalDedupIndex>>,
    movements: &[Movement],
) -> Result<Vec<InterfaceArrivalRecord>, ArrivalError> {
    let seeds: Vec<_> = movements.iter().filter(|movement| {
        movement.kind == MovementKind::InterfaceOutbound
            && matches!(&movement.to, Endpoint::Interface(interface) if runtime.is_runtime_relay_interface(interface))
    }).cloned().collect();
    let Some(first) = seeds.first() else {
        return Ok(Vec::new());
    };
    let Endpoint::Interface(interface) = &first.to;
    InterfaceArtifactArrivalFromInterface::new(
        runtime,
        log,
        blobs,
        dedup,
        first.trace.clone(),
        interface.clone(),
    )
    .drive_relay_chain(seeds)
    .await
}

fn pin_run_record_snapshot(
    blobs: &mut dyn BlobStore,
    run: &RunId,
    snapshot: &octopus_run_record::RunRecordSnapshot,
) -> Result<(), ArrivalError> {
    let actual = ContentHash::of_bytes(snapshot.bytes.as_ref());
    if actual != snapshot.hash {
        return Err(ArrivalError::Malformed(format!(
            "run '{}' resolved snapshot hash mismatch: expected {}, found {}",
            run.as_str(),
            snapshot.hash.to_hex(),
            actual.to_hex()
        )));
    }
    let pinned = blobs.put(snapshot.bytes.as_ref());
    if pinned != snapshot.hash {
        return Err(ArrivalError::Malformed(format!(
            "run '{}' snapshot blob pin mismatch: expected {}, stored {}",
            run.as_str(),
            snapshot.hash.to_hex(),
            pinned.to_hex()
        )));
    }
    Ok(())
}

pub fn pin_interface_artifact_payload_from_interface(
    blobs: &mut dyn BlobStore,
    interface: &InterfaceId,
    artifact_file: &InterfaceArtifactFile,
) -> Result<PinnedInterfaceArtifactPayload, ArrivalError> {
    if interface.as_str().trim().is_empty() {
        return Err(ArrivalError::Malformed(
            "interface artifact arrival has empty interface id".into(),
        ));
    }
    let loaded = artifact_file.load_into_blob(blobs)?;
    let artifact_root = artifact_file
        .manifest_path()
        .parent()
        .ok_or_else(|| {
            ArrivalError::Malformed(format!(
                "interface artifact manifest path has no parent: {}",
                artifact_file.manifest_path().display()
            ))
        })?
        .to_path_buf();
    Ok(PinnedInterfaceArtifactPayload {
        artifact: loaded.artifact,
        artifact_label: loaded.manifest.label,
        payload_bytes: loaded.payload_bytes,
        artifact_root,
    })
}

pub fn pin_interface_artifact_payload_from_reference(
    blobs: &mut dyn BlobStore,
    interface: &InterfaceId,
    artifact: &ArtifactRef,
) -> Result<PinnedInterfaceArtifactPayload, ArrivalError> {
    if interface.as_str().trim().is_empty() {
        return Err(ArrivalError::Malformed(
            "interface artifact reference has empty interface id".into(),
        ));
    }
    let manifest_bytes = blobs.get(&artifact.content).ok_or_else(|| {
        ArrivalError::Malformed(format!(
            "artifact reference '{}' names absent manifest blob '{}'",
            artifact.id.as_str(),
            artifact.content.to_hex()
        ))
    })?;
    let observed_manifest_hash = ContentHash::of_bytes(&manifest_bytes);
    if observed_manifest_hash != artifact.content {
        return Err(ArrivalError::Malformed(format!(
            "artifact reference '{}' manifest hash mismatch: expected {}, observed {}",
            artifact.id.as_str(),
            artifact.content.to_hex(),
            observed_manifest_hash.to_hex()
        )));
    }
    let manifest: InterfaceArtifactManifest =
        serde_json::from_slice(&manifest_bytes).map_err(|error| {
            ArrivalError::Malformed(format!(
                "artifact reference '{}' blob '{}' is not an interface artifact manifest: {error}",
                artifact.id.as_str(),
                artifact.content.to_hex()
            ))
        })?;
    if manifest.label.trim().is_empty() || manifest.label.trim() != manifest.label {
        return Err(ArrivalError::Malformed(format!(
            "artifact reference '{}' manifest has an invalid label",
            artifact.id.as_str()
        )));
    }
    if !manifest.payload_path.is_absolute() {
        return Err(ArrivalError::Malformed(format!(
            "artifact reference '{}' manifest payload path is not absolute",
            artifact.id.as_str()
        )));
    }
    let expected_id = format!("{}:{}", manifest.label, artifact.content.to_hex());
    if artifact.id.as_str() != expected_id {
        return Err(ArrivalError::Malformed(format!(
            "artifact reference '{}' does not match manifest identity '{}'",
            artifact.id.as_str(),
            expected_id
        )));
    }
    let payload_hash = manifest
        .payload_content_hash
        .as_deref()
        .and_then(ContentHash::from_hex)
        .ok_or_else(|| {
            ArrivalError::Malformed(format!(
                "artifact reference '{}' manifest has no valid payload content hash",
                artifact.id.as_str()
            ))
        })?;
    let payload_bytes = blobs.get(&payload_hash).ok_or_else(|| {
        ArrivalError::Malformed(format!(
            "artifact reference '{}' names absent payload blob '{}'",
            artifact.id.as_str(),
            payload_hash.to_hex()
        ))
    })?;
    let observed_payload_hash = ContentHash::of_bytes(&payload_bytes);
    if observed_payload_hash != payload_hash {
        return Err(ArrivalError::Malformed(format!(
            "artifact reference '{}' payload hash mismatch: expected {}, observed {}",
            artifact.id.as_str(),
            payload_hash.to_hex(),
            observed_payload_hash.to_hex()
        )));
    }
    let artifact_root = manifest
        .payload_path
        .parent()
        .map(PathBuf::from)
        .ok_or_else(|| {
            ArrivalError::Malformed(format!(
                "artifact reference '{}' manifest payload path has no parent",
                artifact.id.as_str()
            ))
        })?;
    Ok(PinnedInterfaceArtifactPayload {
        artifact: artifact.clone(),
        artifact_label: manifest.label,
        payload_bytes,
        artifact_root,
    })
}

#[derive(Debug, thiserror::Error)]
pub enum ArrivalError {
    #[error("interface artifact hand-in is malformed: {0}")]
    Malformed(String),
    #[error("interface artifact checkpoint already fired: {0}")]
    CheckpointSpent(String),
    #[error("interface artifact request denied at the gate: {0}")]
    Gate(#[from] RuntimeError),
    #[error("interface artifact verification failed on the edge: {0}")]
    VerificationFailed(octopus_marks::VerifiedArtifactError),
}

impl ArrivalError {
    pub fn verdict(&self) -> DeliveryVerdict {
        match self {
            ArrivalError::Malformed(message) => DeliveryVerdict::refused(
                RefusedReason::Malformed,
                serde_json::json!({ "message": message }),
            ),
            ArrivalError::CheckpointSpent(message) => DeliveryVerdict::refused(
                RefusedReason::CheckpointSpent,
                serde_json::json!({ "message": message }),
            ),
            ArrivalError::Gate(error) => {
                DeliveryVerdict::refused(error.refused_reason(), error.refusal_detail())
            }
            ArrivalError::VerificationFailed(error) => DeliveryVerdict::refused(
                RefusedReason::VerificationFailed,
                serde_json::to_value(error.failure_metadata())
                    .expect("verification failure metadata serializes"),
            ),
        }
    }
}

impl From<octopus_interface_projection::InterfaceArrivalPayloadError> for ArrivalError {
    fn from(error: octopus_interface_projection::InterfaceArrivalPayloadError) -> Self {
        Self::Malformed(error.to_string())
    }
}

impl From<InterfaceArtifactFileError> for ArrivalError {
    fn from(error: InterfaceArtifactFileError) -> Self {
        Self::Malformed(error.to_string())
    }
}

fn admitted_invocation_metadata(
    observation: &octopus_runtime::Observation,
) -> octopus_interfaces::InvocationMetadata {
    observation.invocation_metadata.clone()
}

fn ensure_checkpoint_not_already_admitted(
    dedup: &std::sync::Mutex<octopus_ledger_index::ArrivalDedupIndex>,
    log: &(dyn MovementLog + Send),
    run: &RunId,
    checkpoint: &CheckpointId,
    epoch: u64,
    artifact: &ArtifactRef,
    artifact_label: &str,
) -> Result<(), ArrivalError> {
    let mut index = dedup
        .lock()
        .map_err(|_| ArrivalError::Malformed("arrival dedup index mutex poisoned".into()))?;
    index
        .catch_up(log)
        .map_err(|e| ArrivalError::Malformed(e.to_string()))?;
    let admitted = index
        .checkpoint_admitted(run, checkpoint, epoch)
        .map_err(|e| ArrivalError::Malformed(e.to_string()))?;
    if admitted {
        return Err(ArrivalError::CheckpointSpent(format!(
            "run '{}' checkpoint '{}' already closed its required verified artifact set in the durable \
             movement log; interface JSON must author a new checkpoint generation before another artifact can enter",
            run.as_str(),
            checkpoint.as_str()
        )));
    }
    let routed = index
        .route_artifact_admitted(run, checkpoint, epoch, &artifact.id)
        .map_err(|e| ArrivalError::Malformed(e.to_string()))?;
    if routed {
        return Err(ArrivalError::CheckpointSpent(format!(
            "run '{}' checkpoint '{}' already admitted artifact '{}' in the durable movement log; \
             every arrival on a checkpoint is a distinct artifact",
            run.as_str(),
            checkpoint.as_str(),
            artifact.id.as_str()
        )));
    }
    let requirement_admitted = index
        .requirement_admitted(run, checkpoint, epoch, artifact_label)
        .map_err(|e| ArrivalError::Malformed(e.to_string()))?;
    if requirement_admitted {
        return Err(ArrivalError::CheckpointSpent(format!(
            "run '{}' checkpoint '{}' already admitted required artifact label '{}'; a requirement edge cannot be replayed",
            run.as_str(),
            checkpoint.as_str(),
            artifact_label
        )));
    }
    Ok(())
}

fn record_runtime_outbound_artifact_transfers(
    log: &mut (dyn MovementLog + Send),
    trace: &TraceId,
    transfers: &[octopus_runtime::InterfaceOutboundArtifactTransfer],
    causal_floor: Lamport,
) -> Vec<Movement> {
    let mut movements = Vec::with_capacity(transfers.len());
    for transfer in transfers {
        movements.push(log.append(MovementDraft {
            trace: trace.clone(),
            kind: MovementKind::InterfaceOutbound,
            from: Endpoint::Interface(transfer.from_interface.clone()),
            to: Endpoint::Interface(transfer.to_interface.clone()),
            artifact: Some(transfer.artifact.clone()),
            invocation_metadata: Some(transfer.invocation_metadata.clone().into()),
            causal_floor: Some(causal_floor),
        }));
    }
    movements
}

fn record_runtime_outbound_artifact_refusals(
    log: &mut (dyn MovementLog + Send),
    trace: &TraceId,
    refusals: &[octopus_runtime::InterfaceOutboundArtifactRefusal],
    causal_floor: Lamport,
) -> Result<Vec<Movement>, ArrivalError> {
    let mut movements = Vec::with_capacity(refusals.len());
    for refusal in refusals {
        let verdict = outbound_policy_refusal(
            &refusal.run,
            &refusal.checkpoint,
            &refusal.to_interface,
            &refusal.invocation_metadata,
        )?;
        movements.push(log.append(MovementDraft {
            trace: trace.clone(),
            kind: MovementKind::DeliveryRefused,
            from: Endpoint::Interface(refusal.from_interface.clone()),
            to: Endpoint::Interface(refusal.to_interface.clone()),
            artifact: Some(refusal.artifact.clone()),
            invocation_metadata: Some(verdict.into()),
            causal_floor: Some(causal_floor),
        }));
    }
    Ok(movements)
}

fn outbound_policy_refusal(
    run: &RunId,
    checkpoint: &CheckpointId,
    interface: &InterfaceId,
    metadata: &octopus_interfaces::InvocationMetadata,
) -> Result<DeliveryVerdict, ArrivalError> {
    let runtime = metadata.runtime.as_ref().ok_or_else(|| {
        ArrivalError::Malformed(format!(
            "outbound refusal for run '{}' checkpoint '{}' has no runtime provenance",
            run.as_str(),
            checkpoint.as_str()
        ))
    })?;
    if &runtime.run != run || &runtime.checkpoint != checkpoint || &runtime.interface != interface {
        return Err(ArrivalError::Malformed(format!(
            "outbound refusal for run '{}' checkpoint '{}' has mismatched runtime provenance",
            run.as_str(),
            checkpoint.as_str()
        )));
    }
    Ok(DeliveryVerdict::refused(
        RefusedReason::Policy,
        serde_json::json!({
            "code": "policy.interface_binding_denied",
            "run": run,
            "checkpoint": checkpoint,
            "epoch": runtime.epoch,
            "run_record_hash": runtime.run_record_hash,
            "interface": interface,
        }),
    ))
}

fn record_runtime_admitted_mark_outcomes(
    log: &mut (dyn MovementLog + Send),
    trace: &TraceId,
    marks: &[octopus_marks::Mark],
    from: &Endpoint,
    to: &Endpoint,
    causal_floor: Lamport,
    arrival_seq: octopus_ledger::Seq,
) -> Vec<Movement> {
    let mut movements = Vec::with_capacity(marks.len());
    for mark in marks {
        let mut mark = mark.clone();
        mark.arrival_seq = Some(arrival_seq.0);
        movements.push(log.append(MovementDraft {
            trace: trace.clone(),
            kind: MovementKind::MarkOutcomeAdmitted(mark.name.clone()),
            from: from.clone(),
            to: to.clone(),
            artifact: mark.evidence.clone(),
            invocation_metadata: Some(mark.into()),
            causal_floor: Some(causal_floor),
        }));
    }
    movements
}

#[derive(Clone)]
struct DurableArrivalForRecovery {
    movement: Movement,
    metadata: octopus_interfaces::InvocationMetadata,
    artifact_label: String,
    artifact: ArtifactRef,
    from_interface: InterfaceId,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct DurableCheckpointRecoveryKey {
    run: RunId,
    checkpoint: CheckpointId,
    epoch: u64,
    route_artifact: Option<octopus_descriptors::ArtifactId>,
}

struct DurableCheckpointRecoveryGroup {
    required_labels: Vec<String>,
    outbound_bindings: Vec<octopus_runtime::AuthoredInterfaceBinding>,
    arrivals: BTreeMap<String, DurableArrivalForRecovery>,
    complete: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum DurableTailMovementKind {
    Outbound,
    Refusal,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct DurableTailMovementLookupKey {
    kind: DurableTailMovementKind,
    trace: TraceId,
    artifact_id: octopus_descriptors::ArtifactId,
    artifact_content: ContentHash,
    from_interface: InterfaceId,
    to_interface: InterfaceId,
}

fn durable_tail_movement_lookup_key(
    kind: DurableTailMovementKind,
    trace: &TraceId,
    artifact: &ArtifactRef,
    from_interface: &InterfaceId,
    to_interface: &InterfaceId,
) -> DurableTailMovementLookupKey {
    DurableTailMovementLookupKey {
        kind,
        trace: trace.clone(),
        artifact_id: artifact.id.clone(),
        artifact_content: artifact.content,
        from_interface: from_interface.clone(),
        to_interface: to_interface.clone(),
    }
}

fn existing_durable_tail_movement(
    movement: &Movement,
) -> Option<(DurableTailMovementLookupKey, &MovementMetadata)> {
    let metadata = movement.invocation_metadata.as_ref()?;
    let kind = match movement.kind {
        MovementKind::InterfaceOutbound if metadata.invocation().is_some() => {
            DurableTailMovementKind::Outbound
        }
        MovementKind::DeliveryRefused if metadata.delivery_verdict().is_some() => {
            DurableTailMovementKind::Refusal
        }
        _ => return None,
    };
    let (Endpoint::Interface(from_interface), Endpoint::Interface(to_interface)) =
        (&movement.from, &movement.to);
    let artifact = movement.artifact.as_ref()?;
    Some((
        durable_tail_movement_lookup_key(
            kind,
            &movement.trace,
            artifact,
            from_interface,
            to_interface,
        ),
        metadata,
    ))
}

struct DurableTailMovementIndex<'a> {
    existing: HashMap<DurableTailMovementLookupKey, Vec<&'a MovementMetadata>>,
    recovered: HashMap<DurableTailMovementLookupKey, Vec<MovementMetadata>>,
}

impl<'a> DurableTailMovementIndex<'a> {
    fn from_movements(movements: &'a [Movement]) -> Self {
        let mut existing: HashMap<DurableTailMovementLookupKey, Vec<&MovementMetadata>> =
            HashMap::new();
        for movement in movements {
            let Some((key, metadata)) = existing_durable_tail_movement(movement) else {
                continue;
            };
            existing.entry(key).or_default().push(metadata);
        }
        Self {
            existing,
            recovered: HashMap::new(),
        }
    }

    fn contains_exact(
        &self,
        key: &DurableTailMovementLookupKey,
        metadata: &MovementMetadata,
    ) -> bool {
        self.existing
            .get(key)
            .into_iter()
            .flat_map(|candidates| candidates.iter().copied())
            .chain(
                self.recovered
                    .get(key)
                    .into_iter()
                    .flat_map(|candidates| candidates.iter()),
            )
            .any(|candidate| candidate == metadata)
    }

    fn record_if_absent(
        &mut self,
        key: DurableTailMovementLookupKey,
        metadata: MovementMetadata,
    ) -> Option<MovementMetadata> {
        if self.contains_exact(&key, &metadata) {
            return None;
        }
        self.recovered
            .entry(key)
            .or_default()
            .push(metadata.clone());
        Some(metadata)
    }
}

pub fn recover_durable_checkpoint_outbound_tail(
    log: &mut (dyn MovementLog + Send),
    movements: &[Movement],
) -> Result<Vec<Movement>, ArrivalError> {
    let mark_proofs = octopus_ledger::InterfaceArrivalMarkProofIndex::from_movements(movements)
        .map_err(|error| ArrivalError::Malformed(error.to_string()))?;
    let mut existing_tail = DurableTailMovementIndex::from_movements(movements);
    let mut groups: BTreeMap<DurableCheckpointRecoveryKey, DurableCheckpointRecoveryGroup> =
        BTreeMap::new();
    for movement in movements {
        if movement.kind != MovementKind::InterfaceArrival {
            continue;
        }
        let Some(run) = movement.run() else {
            return Err(ArrivalError::Malformed(
                "durable arrival has no run provenance".into(),
            ));
        };
        let Some(metadata) = movement
            .invocation_metadata
            .as_ref()
            .and_then(|metadata| metadata.invocation())
        else {
            continue;
        };
        if metadata.verification_failure.is_some() {
            continue;
        }
        let Some(runtime) = metadata.runtime.as_ref() else {
            continue;
        };
        let Some(admission) = metadata.checkpoint_admission.as_ref() else {
            continue;
        };
        let Endpoint::Interface(from_interface) = &movement.from;
        let Some(artifact) = movement.artifact.as_ref() else {
            continue;
        };
        if movement.to != Endpoint::Interface(runtime.worker.clone())
            || &runtime.interface != from_interface
        {
            continue;
        }
        let required: BTreeSet<&str> = admission
            .required_artifact_labels
            .iter()
            .map(String::as_str)
            .collect();
        let declared: BTreeSet<&str> = metadata
            .required_verified_artifacts
            .iter()
            .map(|required| required.artifact_label.as_str())
            .collect();
        if required.len() != admission.required_artifact_labels.len()
            || declared.len() != metadata.required_verified_artifacts.len()
            || required != declared
        {
            return Err(ArrivalError::Malformed(format!(
                "durable run '{}' checkpoint '{}' has inconsistent required verified artifact metadata",
                run.as_str(),
                runtime.checkpoint.as_str()
            )));
        }
        let mark_backed = required.is_empty() || mark_proofs.proof_lamport(movement).is_some();
        if !mark_backed {
            continue;
        }
        let outbound_bindings = metadata
            .checkpoint_binding_policy
            .as_ref()
            .map(|policy| policy.outbound.clone())
            .ok_or_else(|| {
                ArrivalError::Malformed(format!(
                    "durable run '{}' checkpoint '{}' has no checkpoint binding policy snapshot",
                    run.as_str(),
                    runtime.checkpoint.as_str()
                ))
            })?;
        let outbound_ids: Vec<InterfaceId> = outbound_bindings
            .iter()
            .map(|binding| binding.interface.clone())
            .collect();
        if metadata.outbound_interface_bindings != outbound_ids {
            return Err(ArrivalError::Malformed(format!(
                "durable run '{}' checkpoint '{}' has inconsistent outbound binding metadata",
                run.as_str(),
                runtime.checkpoint.as_str()
            )));
        }
        let key = DurableCheckpointRecoveryKey {
            run: run.clone(),
            checkpoint: runtime.checkpoint.clone(),
            epoch: runtime.epoch,
            route_artifact: required.is_empty().then(|| artifact.id.clone()),
        };
        let group = groups
            .entry(key)
            .or_insert_with(|| DurableCheckpointRecoveryGroup {
                required_labels: admission.required_artifact_labels.clone(),
                outbound_bindings: outbound_bindings.clone(),
                arrivals: BTreeMap::new(),
                complete: false,
            });
        if group.required_labels != admission.required_artifact_labels
            || group.outbound_bindings != outbound_bindings
        {
            return Err(ArrivalError::Malformed(format!(
                "durable run '{}' checkpoint '{}' combines different checkpoint snapshots",
                run.as_str(),
                runtime.checkpoint.as_str()
            )));
        }
        let recovered_arrival = DurableArrivalForRecovery {
            movement: movement.clone(),
            metadata: metadata.clone(),
            artifact_label: admission.artifact_label.clone(),
            artifact: artifact.clone(),
            from_interface: from_interface.clone(),
        };
        let prior = group
            .arrivals
            .insert(admission.artifact_label.clone(), recovered_arrival);
        if let Some(prior) = prior {
            if prior.artifact != *artifact {
                return Err(ArrivalError::Malformed(format!(
                    "durable run '{}' checkpoint '{}' repeats admitted artifact label '{}' with different evidence",
                    run.as_str(),
                    runtime.checkpoint.as_str(),
                    admission.artifact_label
                )));
            }
        }
        group.complete |= admission.complete;
    }

    let mut recovered = Vec::new();
    for (key, group) in groups {
        if !group.complete {
            continue;
        }
        let ordered: Vec<&DurableArrivalForRecovery> = if group.required_labels.is_empty() {
            if group.arrivals.len() != 1 {
                return Err(ArrivalError::Malformed(format!(
                    "durable run '{}' checkpoint '{}' epoch {} has {} unverified complete arrivals",
                    key.run.as_str(),
                    key.checkpoint.as_str(),
                    key.epoch,
                    group.arrivals.len()
                )));
            }
            group.arrivals.values().collect()
        } else {
            let mut ordered = Vec::with_capacity(group.required_labels.len());
            for label in &group.required_labels {
                ordered.push(group.arrivals.get(label).ok_or_else(|| {
                    ArrivalError::Malformed(format!(
                        "durable run '{}' checkpoint '{}' epoch {} is complete without mark-backed artifact label '{}'",
                        key.run.as_str(),
                        key.checkpoint.as_str(),
                        key.epoch,
                        label
                    ))
                })?);
            }
            ordered
        };
        let mut proof_floor = ordered
            .iter()
            .map(|arrival| arrival.movement.lamport)
            .max()
            .ok_or_else(|| {
                ArrivalError::Malformed(format!(
                    "durable run '{}' checkpoint '{}' has no completion proof floor",
                    key.run.as_str(),
                    key.checkpoint.as_str()
                ))
            })?;
        if !group.required_labels.is_empty() {
            for arrival in &ordered {
                let mark_floor = mark_proofs
                    .proof_lamport(&arrival.movement)
                    .ok_or_else(|| {
                        ArrivalError::Malformed(format!(
                            "durable run '{}' checkpoint '{}' epoch {} lost its exact mark proof",
                            key.run.as_str(),
                            key.checkpoint.as_str(),
                            key.epoch
                        ))
                    })?;
                proof_floor = proof_floor.max(mark_floor);
            }
        }
        let mut seen = BTreeSet::new();
        for arrival in ordered {
            for outbound in &group.outbound_bindings {
                if !seen.insert((arrival.artifact.id.clone(), outbound.interface.clone())) {
                    continue;
                }
                let mut output_metadata = arrival.metadata.clone();
                let runtime = output_metadata.runtime.as_mut().ok_or_else(|| {
                    ArrivalError::Malformed(format!(
                        "durable run '{}' checkpoint '{}' epoch {} lost runtime provenance",
                        key.run.as_str(),
                        key.checkpoint.as_str(),
                        key.epoch
                    ))
                })?;
                runtime.interface = outbound.interface.clone();
                if arrival
                    .metadata
                    .grant
                    .interfaces
                    .contains(&outbound.interface.as_str().to_string())
                    && outbound.admits(&arrival.artifact_label)
                {
                    let movement_key = durable_tail_movement_lookup_key(
                        DurableTailMovementKind::Outbound,
                        &arrival.movement.trace,
                        &arrival.artifact,
                        &arrival.from_interface,
                        &outbound.interface,
                    );
                    if let Some(output_metadata) =
                        existing_tail.record_if_absent(movement_key, output_metadata.into())
                    {
                        recovered.push(log.append(MovementDraft {
                            trace: arrival.movement.trace.clone(),
                            kind: MovementKind::InterfaceOutbound,
                            from: Endpoint::Interface(arrival.from_interface.clone()),
                            to: Endpoint::Interface(outbound.interface.clone()),
                            artifact: Some(arrival.artifact.clone()),
                            invocation_metadata: Some(output_metadata),
                            causal_floor: Some(proof_floor),
                        }));
                    }
                } else {
                    let refusal = outbound_policy_refusal(
                        &key.run,
                        &key.checkpoint,
                        &outbound.interface,
                        &output_metadata,
                    )?;
                    let movement_key = durable_tail_movement_lookup_key(
                        DurableTailMovementKind::Refusal,
                        &arrival.movement.trace,
                        &arrival.artifact,
                        &arrival.from_interface,
                        &outbound.interface,
                    );
                    if let Some(refusal) =
                        existing_tail.record_if_absent(movement_key, refusal.into())
                    {
                        recovered.push(log.append(MovementDraft {
                            trace: arrival.movement.trace.clone(),
                            kind: MovementKind::DeliveryRefused,
                            from: Endpoint::Interface(arrival.from_interface.clone()),
                            to: Endpoint::Interface(outbound.interface.clone()),
                            artifact: Some(arrival.artifact.clone()),
                            invocation_metadata: Some(refusal),
                            causal_floor: Some(proof_floor),
                        }));
                    }
                }
            }
        }
    }
    Ok(recovered)
}

struct PendingArrivalEdge {
    interface: InterfaceId,
    artifact: ArtifactRef,
    artifact_label: String,
    artifact_root: PathBuf,
    target: InterfaceArrivalTarget,
    source: Option<Movement>,
    pending: Option<Movement>,
}

pub struct InterfaceArtifactArrivalFromInterface<'a> {
    runtime: &'a octopus_runtime::Runtime,
    log: &'a mut (dyn MovementLog + Send),
    blobs: &'a mut dyn BlobStore,
    dedup: std::sync::Arc<std::sync::Mutex<octopus_ledger_index::ArrivalDedupIndex>>,
    trace: TraceId,
    interface: InterfaceId,
}

impl<'a> InterfaceArtifactArrivalFromInterface<'a> {
    pub fn new(
        runtime: &'a octopus_runtime::Runtime,
        log: &'a mut (dyn MovementLog + Send),
        blobs: &'a mut dyn BlobStore,
        dedup: std::sync::Arc<std::sync::Mutex<octopus_ledger_index::ArrivalDedupIndex>>,
        trace: TraceId,
        interface: InterfaceId,
    ) -> Self {
        Self {
            runtime,
            log,
            blobs,
            dedup,
            trace,
            interface,
        }
    }

    fn record_delivery_refusal(
        &mut self,
        error: ArrivalError,
        from: Endpoint,
        to: Endpoint,
        artifact: Option<ArtifactRef>,
    ) -> ArrivalError {
        let verdict = error.verdict();
        self.log.append(MovementDraft {
            trace: self.trace.clone(),
            kind: MovementKind::DeliveryRefused,
            from,
            to,
            artifact,
            invocation_metadata: Some(verdict.into()),
            causal_floor: None,
        });
        error
    }

    fn target_runtime_metadata(
        target: &InterfaceArrivalTarget,
        interface: &InterfaceId,
    ) -> octopus_ledger::RuntimeInvocationMetadata {
        octopus_ledger::RuntimeInvocationMetadata {
            run: target.run.clone(),
            checkpoint: target.checkpoint.clone(),
            ee: target.ee.clone(),
            worker: target.worker.clone(),
            interface: interface.clone(),
            epoch: target.epoch.0,
            run_record_hash: Some(target.run_record.hash.to_hex()),
        }
    }

    fn record_target_refusal(
        &mut self,
        error: ArrivalError,
        arriving_interface: &InterfaceId,
        artifact: &ArtifactRef,
        target: &InterfaceArrivalTarget,
        source: Option<&Movement>,
    ) -> ArrivalError {
        if source.is_some_and(|source| {
            matches!(
                self.relay_arrival_state(source, target),
                Ok(octopus_ledger_index::RelayArrivalState::Finished)
            )
        }) {
            return error;
        }
        self.log.append(MovementDraft {
            trace: target.checkpoint_trace.clone(),
            kind: MovementKind::DeliveryRefused,
            from: Endpoint::Interface(arriving_interface.clone()),
            to: Endpoint::Interface(target.worker.clone()),
            artifact: Some(artifact.clone()),
            invocation_metadata: Some(MovementMetadata::Delivery(
                octopus_ledger::DeliveryMetadata {
                    delivery_verdict: error.verdict(),
                    runtime: Some(Self::target_runtime_metadata(target, arriving_interface)),
                    source_movement_seq: source.map(|movement| movement.seq.0),
                    delivery_scope_seq: target.delivery_scope_seq,
                },
            )),
            causal_floor: source.map(|movement| movement.lamport),
        });
        error
    }

    fn relay_arrival_state(
        &self,
        source: &Movement,
        target: &InterfaceArrivalTarget,
    ) -> Result<octopus_ledger_index::RelayArrivalState, ArrivalError> {
        let mut index = self
            .dedup
            .lock()
            .map_err(|_| ArrivalError::Malformed("arrival dedup index mutex poisoned".into()))?;
        index
            .catch_up(self.log)
            .map_err(|error| ArrivalError::Malformed(error.to_string()))?;
        index
            .relay_arrival_state(
                source.seq.0,
                &target.run,
                &target.checkpoint,
                target.epoch.0,
                &target.run_record.hash.to_hex(),
            )
            .map_err(|error| ArrivalError::Malformed(error.to_string()))
    }

    fn prepare_target_edge(
        &mut self,
        arriving_interface: &InterfaceId,
        artifact: &ArtifactRef,
        artifact_label: &str,
        target: &InterfaceArrivalTarget,
        source: Option<&Movement>,
    ) -> Result<(), ArrivalError> {
        if let Err(error) = ensure_checkpoint_not_already_admitted(
            &self.dedup,
            self.log,
            &target.run,
            &target.checkpoint,
            target.epoch.0,
            artifact,
            artifact_label,
        ) {
            return Err(if matches!(error, ArrivalError::CheckpointSpent(_)) {
                self.record_target_refusal(error, arriving_interface, artifact, target, source)
            } else { error });
        }
        pin_run_record_snapshot(self.blobs, &target.run, &target.run_record)?;
        Ok(())
    }

    fn pending_required_arrival(
        &self,
        target: &InterfaceArrivalTarget,
        artifact_label: &str,
    ) -> Result<Option<Movement>, ArrivalError> {
        let seq = {
            let mut index = self
                .dedup
                .lock()
                .map_err(|_| ArrivalError::Malformed("arrival dedup index mutex poisoned".into()))?;
            index
                .catch_up(self.log)
                .map_err(|error| ArrivalError::Malformed(error.to_string()))?;
            index
                .pending_requirement_seq(
                    &target.run,
                    &target.checkpoint,
                    target.epoch.0,
                    artifact_label,
                )
                .map_err(|error| ArrivalError::Malformed(error.to_string()))?
        };
        seq.map(|seq| {
            self.log.read_at(seq).ok_or_else(|| {
                ArrivalError::Malformed(format!(
                    "pending required arrival {} disappeared from the ledger",
                    seq.0
                ))
            })
        })
        .transpose()
    }

    fn finish_target_edge(
        &mut self,
        edge: PendingArrivalEdge,
        result: Result<octopus_runtime::PreparedInterfaceCheckpointAdmission, RuntimeError>,
    ) -> Result<InterfaceArrivalRecord, ArrivalError> {
        let PendingArrivalEdge {
            interface,
            artifact,
            artifact_label: _,
            artifact_root,
            target,
            source,
            pending,
        } = edge;
        let arriving_interface = &interface;
        let artifact = &artifact;
        let source = source.as_ref();
        let target_trace = target.checkpoint_trace.clone();
        let prepared = match result {
            Ok(observation) => observation,
            Err(RuntimeError::VerifiedArtifactRejected {
                error,
                invocation_metadata,
            }) => {
                if pending.is_some() {
                    return Err(self.record_target_refusal(
                        ArrivalError::VerificationFailed(error),
                        arriving_interface,
                        artifact,
                        &target,
                        source,
                    ));
                }
                let failure_metadata =
                    (*invocation_metadata).with_verification_failure(error.failure_metadata());
                let arrival_movement = self.log.append(MovementDraft {
                    trace: target_trace.clone(),
                    kind: MovementKind::InterfaceArrival,
                    from: Endpoint::Interface(arriving_interface.clone()),
                    to: Endpoint::Interface(target.worker.clone()),
                    artifact: Some(artifact.clone()),
                    invocation_metadata: Some(failure_metadata.clone().into()),
                    causal_floor: source.map(|movement| movement.lamport),
                });
                self.log.append(MovementDraft {
                    trace: target_trace.clone(),
                    kind: MovementKind::VerificationFailed,
                    from: Endpoint::Interface(arriving_interface.clone()),
                    to: Endpoint::Interface(target.worker.clone()),
                    artifact: Some(artifact.clone()),
                    invocation_metadata: Some(failure_metadata.into()),
                    causal_floor: Some(arrival_movement.lamport),
                });
                return Err(ArrivalError::VerificationFailed(error));
            }
            Err(other) => {
                return Err(self.record_target_refusal(
                    ArrivalError::Gate(other),
                    arriving_interface,
                    artifact,
                    &target,
                    source,
                ));
            }
        };
        if let Err(error) = prepared.validate_commit() {
            return Err(self.record_target_refusal(ArrivalError::Gate(error), arriving_interface, artifact, &target, source));
        }
        let arrival_metadata = admitted_invocation_metadata(prepared.observation());
        let draft = MovementDraft {
            trace: target_trace.clone(), kind: MovementKind::InterfaceArrival,
            from: Endpoint::Interface(arriving_interface.clone()),
            to: Endpoint::Interface(target.worker.clone()), artifact: Some(artifact.clone()),
            invocation_metadata: Some(arrival_metadata.clone().into()),
            causal_floor: pending.as_ref().map(|movement| movement.lamport)
                .or_else(|| source.map(|movement| movement.lamport)),
        };
        let arrival_movement = if let Some(pending) = pending {
            if pending.trace != target_trace
                || pending.artifact.as_ref() != Some(artifact)
                || pending.kind != MovementKind::InterfaceArrival
                || pending.from != draft.from || pending.to != draft.to
            {
                return Err(self.record_target_refusal(ArrivalError::Malformed("pending admission changed its exact arrival identity".into()), arriving_interface, artifact, &target, source));
            }
            let prior = pending.invocation_metadata.as_ref().and_then(MovementMetadata::invocation);
            if prior == Some(&arrival_metadata) { pending }
            else if arrival_metadata.replaces_arrival_seq == Some(pending.seq.0)
                && prior.is_some_and(|prior| arrival_metadata.retains_required_admission_contract(prior))
            { self.log.append(draft) }
            else {
                return Err(self.record_target_refusal(ArrivalError::Malformed("pending admission has no checked replacement proof".into()), arriving_interface, artifact, &target, source));
            }
        } else {
            self.log.append(draft)
        };
        let admitted_mark_outcome_movements = record_runtime_admitted_mark_outcomes(
            self.log,
            &target_trace,
            &prepared.observation().admitted_mark_outcomes,
            &arrival_movement.from,
            &arrival_movement.to,
            arrival_movement.lamport,
            arrival_movement.seq,
        );
        let completion_floor = admitted_mark_outcome_movements
            .iter()
            .map(|movement| movement.lamport)
            .max()
            .unwrap_or(arrival_movement.lamport);
        let outbound_transfer_movements = record_runtime_outbound_artifact_transfers(
            self.log,
            &target_trace,
            &prepared.observation().outbound_artifact_transfers,
            completion_floor,
        );
        let outbound_refusal_movements = record_runtime_outbound_artifact_refusals(
            self.log,
            &target_trace,
            &prepared.observation().refused_outbound_artifact_transfers,
            completion_floor,
        )?;
        let observation = prepared.commit();
        Ok(InterfaceArrivalRecord {
            run: target.run,
            checkpoint: target.checkpoint,
            outbound_bindings: target
                .outbound_bindings
                .into_iter()
                .map(|binding| binding.interface)
                .collect(),
            epoch: target.epoch,
            arrival_movement,
            admitted_mark_outcome_movements,
            outbound_transfer_movements,
            outbound_refusal_movements,
            artifact_root: artifact_root.clone(),
            outbound_transfers: observation.outbound_artifact_transfers.clone(),
            gated: true,
        })
    }

    pub(crate) async fn arrive_artifact_for_run(
        &mut self,
        artifact_file: &InterfaceArtifactFile,
    ) -> Result<InterfaceArrivalDispatch, ArrivalError> {
        let pinned = pin_interface_artifact_payload_from_interface(
            self.blobs,
            &self.interface,
            artifact_file,
        )
        .map_err(|error| {
            self.record_delivery_refusal(
                error,
                Endpoint::Interface(self.interface.clone()),
                Endpoint::Interface(self.interface.clone()),
                None,
            )
        })?;
        self.trace = TraceId(format!(
            "handin:{}:{}",
            self.interface.as_str(),
            pinned.artifact.content.to_hex()
        ));
        let targets = match self
            .runtime
            .resolve_interface_arrival_targets(&self.interface, &pinned.artifact_label)
        {
            Ok(targets) => targets,
            Err(error) => {
                return Err(self.record_delivery_refusal(
                    ArrivalError::Gate(error),
                    Endpoint::Interface(self.interface.clone()),
                    Endpoint::Interface(self.interface.clone()),
                    Some(pinned.artifact.clone()),
                ));
            }
        };

        let edges = targets
            .into_iter()
            .map(|target| PendingArrivalEdge {
                interface: self.interface.clone(),
                artifact: pinned.artifact.clone(),
                artifact_label: pinned.artifact_label.clone(),
                artifact_root: pinned.artifact_root.clone(),
                target,
                source: None,
                pending: None,
            })
            .collect();
        let (records, refusal) = self.admit_edges_concurrently(edges).await?;
        if records.is_empty() {
            return Err(refusal.unwrap_or_else(|| {
                ArrivalError::Malformed("arrival resolved zero targets".into())
            }));
        }
        Ok(InterfaceArrivalDispatch { records })
    }

    async fn admit_edges_concurrently(
        &mut self,
        mut edges: Vec<PendingArrivalEdge>,
    ) -> Result<(Vec<InterfaceArrivalRecord>, Option<ArrivalError>), ArrivalError> {
        let mut inflight = tokio::task::JoinSet::new();
        let mut records = Vec::new();
        let mut first_refusal = None;
        let mut seen = BTreeSet::new();
        loop {
            for mut edge in edges.drain(..) {
                if let Err(error) = self.prepare_target_edge(
                    &edge.interface,
                    &edge.artifact,
                    &edge.artifact_label,
                    &edge.target,
                    edge.source.as_ref(),
                ) {
                    if !matches!(error, ArrivalError::CheckpointSpent(_)) { return Err(error); }
                    first_refusal.get_or_insert(error);
                    continue;
                }
                if edge.pending.is_none() {
                    match self.pending_required_arrival(&edge.target, &edge.artifact_label) {
                        Ok(pending) => edge.pending = pending,
                        Err(error) => return Err(error),
                    }
                }
                let runtime = self.runtime.clone();
                inflight.spawn(async move {
                    let result = runtime
                        .admit_resolved_interface_checkpoint(
                            &edge.target,
                            edge.artifact.clone(),
                            edge.artifact_label.clone(),
                            edge.pending.as_ref(),
                        )
                        .await;
                    (edge, result)
                });
            }
            let Some(completed) = inflight.join_next().await else {
                break;
            };
            let (edge, result) = completed.map_err(|error| {
                ArrivalError::Malformed(format!("artifact admission task failed: {error}"))
            })?;
            match self.finish_target_edge(edge, result) {
                Ok(record) => {
                    for source in &record.outbound_transfer_movements {
                        if seen.insert(source.seq) {
                            match self.relay_edges(source) {
                                Ok(next) => edges.extend(next),
                                Err(error) => {
                                    self.record_delivery_refusal(
                                        error,
                                        source.to.clone(),
                                        source.to.clone(),
                                        source.artifact.clone(),
                                    );
                                }
                            }
                        }
                    }
                    records.push(record);
                }
                Err(error) => {
                    first_refusal.get_or_insert(error);
                }
            }
        }
        Ok((records, first_refusal))
    }

    async fn drive_relay_chain(
        &mut self,
        seeds: Vec<Movement>,
    ) -> Result<Vec<InterfaceArrivalRecord>, ArrivalError> {
        let mut edges = Vec::new();
        for source in seeds {
            match self.relay_edges(&source) {
                Ok(next) => edges.extend(next),
                Err(error) => {
                    self.record_delivery_refusal(
                        error,
                        source.to.clone(),
                        source.to.clone(),
                        source.artifact.clone(),
                    );
                }
            }
        }
        self.admit_edges_concurrently(edges)
            .await
            .map(|(records, _)| records)
    }

    fn relay_edges(&mut self, source: &Movement) -> Result<Vec<PendingArrivalEdge>, ArrivalError> {
        let Endpoint::Interface(to_interface) = &source.to;
        if !self.runtime.is_runtime_relay_interface(to_interface) {
            return Ok(Vec::new());
        }
        let artifact = source
            .artifact
            .as_ref()
            .ok_or_else(|| ArrivalError::Malformed("relay source has no artifact".into()))?;
        let runtime = source
            .invocation_metadata
            .as_ref()
            .and_then(MovementMetadata::invocation)
            .and_then(|metadata| metadata.runtime.as_ref())
            .ok_or_else(|| {
                ArrivalError::Malformed(format!(
                    "relay source {} has no exact runtime provenance",
                    source.seq.0
                ))
            })?;
        if source.kind != MovementKind::InterfaceOutbound
            || runtime.interface != *to_interface
            || !matches!(&source.from, Endpoint::Interface(_))
        {
            return Err(ArrivalError::Malformed(format!(
                "relay source {} has inconsistent outbound provenance",
                source.seq.0
            )));
        }
        let label = match pin_interface_artifact_payload_from_reference(self.blobs, to_interface, artifact) {
            Ok(pinned) => pinned.artifact_label,
            Err(error) => {
                self.record_delivery_refusal(
                    error,
                    Endpoint::Interface(to_interface.clone()),
                    Endpoint::Interface(to_interface.clone()),
                    Some(artifact.clone()),
                );
                return Ok(Vec::new());
            }
        };
        let targets = self
            .runtime
            .resolve_runtime_relay_cross_run_arrival_targets(
                to_interface,
                artifact,
                &label,
                octopus_runtime::SystemInterfaceEmitter {
                    run: &runtime.run,
                    checkpoint: &runtime.checkpoint,
                    epoch: GrantEpoch(runtime.epoch),
                },
            )?;
        let mut records = Vec::with_capacity(targets.len());
        for target in targets {
            let state = match self.relay_arrival_state(source, &target) {
                Ok(state) => state,
                Err(error) => {
                    self.record_target_refusal(
                        error,
                        to_interface,
                        artifact,
                        &target,
                        Some(source),
                    );
                    continue;
                }
            };
            let pending = match state {
                octopus_ledger_index::RelayArrivalState::Finished => continue,
                octopus_ledger_index::RelayArrivalState::Pending(movement) => {
                    let full = self.log.read_at(movement.seq);
                    Some(full.ok_or_else(|| {
                        ArrivalError::Malformed(
                            "pending relay arrival disappeared from the ledger".into(),
                        )
                    })?)
                }
                octopus_ledger_index::RelayArrivalState::Unseen => None,
            };
            let pinned = match pin_interface_artifact_payload_from_reference(
                self.blobs,
                to_interface,
                artifact,
            ) {
                Ok(pinned) => pinned,
                Err(error) => {
                    self.record_target_refusal(
                        error,
                        to_interface,
                        artifact,
                        &target,
                        Some(source),
                    );
                    continue;
                }
            };
            records.push(PendingArrivalEdge {
                interface: to_interface.clone(),
                artifact: artifact.clone(),
                artifact_label: pinned.artifact_label,
                artifact_root: pinned.artifact_root,
                target: target.with_source_movement_seq(source.seq.0),
                source: Some(source.clone()),
                pending,
            });
        }
        Ok(records)
    }

    pub async fn emit_artifact_to_interface(
        &mut self,
        artifact_file: &InterfaceArtifactFile,
        emitter_run: &RunId,
        emitter_checkpoint: &CheckpointId,
        emitter_epoch: GrantEpoch,
        source_lamport: Lamport,
        to_interface: &InterfaceId,
    ) -> Result<SystemInterfaceEmissionOutcome, ArrivalError> {
        let pinned = pin_interface_artifact_payload_from_interface(
            self.blobs,
            &self.interface,
            artifact_file,
        )
        .map_err(|error| {
            self.record_delivery_refusal(
                error,
                Endpoint::Interface(self.interface.clone()),
                Endpoint::Interface(to_interface.clone()),
                None,
            )
        })?;
        self.emit_pinned_artifact_to_interface(
            pinned,
            emitter_run,
            emitter_checkpoint,
            emitter_epoch,
            source_lamport,
            to_interface,
        )
        .await
    }

    pub async fn emit_artifact_reference_to_interface(
        &mut self,
        artifact: &ArtifactRef,
        emitter_run: &RunId,
        emitter_checkpoint: &CheckpointId,
        emitter_epoch: GrantEpoch,
        source_lamport: Lamport,
        to_interface: &InterfaceId,
    ) -> Result<SystemInterfaceEmissionOutcome, ArrivalError> {
        let pinned =
            pin_interface_artifact_payload_from_reference(self.blobs, &self.interface, artifact)
                .map_err(|error| {
                    self.record_delivery_refusal(
                        error,
                        Endpoint::Interface(self.interface.clone()),
                        Endpoint::Interface(to_interface.clone()),
                        Some(artifact.clone()),
                    )
                })?;
        self.emit_pinned_artifact_to_interface(
            pinned,
            emitter_run,
            emitter_checkpoint,
            emitter_epoch,
            source_lamport,
            to_interface,
        )
        .await
    }

    async fn emit_pinned_artifact_to_interface(
        &mut self,
        pinned: PinnedInterfaceArtifactPayload,
        emitter_run: &RunId,
        emitter_checkpoint: &CheckpointId,
        emitter_epoch: GrantEpoch,
        source_lamport: Lamport,
        to_interface: &InterfaceId,
    ) -> Result<SystemInterfaceEmissionOutcome, ArrivalError> {
        let prior_admitted_seq = (|| -> Result<Option<u64>, ArrivalError> {
            let mut dedup = self.dedup.lock().map_err(|_| {
                ArrivalError::Malformed("arrival dedup index mutex poisoned".into())
            })?;
            dedup
                .catch_up(self.log)
                .map_err(|error| ArrivalError::Malformed(error.to_string()))?;
            Ok(dedup.admitted_emission_seq(
                emitter_run,
                emitter_checkpoint,
                emitter_epoch.0,
                to_interface,
                &pinned.artifact.id,
            ))
        })()
        .map_err(|error| {
            self.record_delivery_refusal(
                error,
                Endpoint::Interface(self.interface.clone()),
                Endpoint::Interface(to_interface.clone()),
                Some(pinned.artifact.clone()),
            )
        })?;
        if let Some(seq) = prior_admitted_seq {
            let source = self.log.read_at(octopus_ledger::Seq(seq));
            let source = source.ok_or_else(|| {
                ArrivalError::Malformed(format!(
                    "admitted emission seq {seq} disappeared from the ledger"
                ))
            })?;
            if source.artifact.as_ref() != Some(&pinned.artifact)
                || source.from != Endpoint::Interface(self.interface.clone())
                || source.to != Endpoint::Interface(to_interface.clone())
            {
                return Err(self.record_delivery_refusal(
                    ArrivalError::Malformed(format!(
                        "admitted emission seq {seq} does not match the retried exact artifact edge"
                    )),
                    Endpoint::Interface(self.interface.clone()),
                    Endpoint::Interface(to_interface.clone()),
                    Some(pinned.artifact.clone()),
                ));
            }
            let delivered = self.drive_relay_chain(vec![source]).await?;
            return Ok(SystemInterfaceEmissionOutcome {
                outbound_movement: None,
                prior_admitted_seq: Some(seq),
                delivered,
            });
        }
        let authority = match self.runtime.resolve_system_interface_emission_grant(
            octopus_runtime::SystemInterfaceEmitter {
                run: emitter_run,
                checkpoint: emitter_checkpoint,
                epoch: emitter_epoch,
            },
            &self.interface,
            to_interface,
            &pinned.artifact_label,
        ) {
            Ok(authority) => authority,
            Err(error) => {
                return Err(self.record_delivery_refusal(
                    ArrivalError::Gate(error),
                    Endpoint::Interface(self.interface.clone()),
                    Endpoint::Interface(to_interface.clone()),
                    Some(pinned.artifact.clone()),
                ));
            }
        };
        let run_record_hash = authority.run_record.hash.to_hex();
        if let Err(error) = pin_run_record_snapshot(self.blobs, emitter_run, &authority.run_record)
        {
            return Err(self.record_delivery_refusal(
                error,
                Endpoint::Interface(self.interface.clone()),
                Endpoint::Interface(to_interface.clone()),
                Some(pinned.artifact.clone()),
            ));
        }
        let emission_metadata =
            octopus_interfaces::InvocationMetadata::new(authority.metadata_projection)
                .with_runtime(octopus_interfaces::RuntimeInvocationMetadata {
                    run: emitter_run.clone(),
                    ee: authority.ee.clone(),
                    worker: authority.worker.clone(),
                    checkpoint: emitter_checkpoint.clone(),
                    interface: to_interface.clone(),
                    epoch: authority.epoch.0,
                    run_record_hash: Some(run_record_hash),
                })
                .with_checkpoint_binding_policy(authority.checkpoint_binding_policy);
        let outbound_movement = self.log.append(MovementDraft {
            trace: self.trace.clone(),
            kind: MovementKind::InterfaceOutbound,
            from: Endpoint::Interface(self.interface.clone()),
            to: Endpoint::Interface(to_interface.clone()),
            artifact: Some(pinned.artifact.clone()),
            invocation_metadata: Some(emission_metadata.into()),
            causal_floor: Some(source_lamport),
        });
        let delivered = self
            .drive_relay_chain(vec![outbound_movement.clone()])
            .await?;
        Ok(SystemInterfaceEmissionOutcome {
            outbound_movement: Some(outbound_movement),
            prior_admitted_seq: None,
            delivered,
        })
    }
}

#[derive(Clone, Debug)]
pub struct SystemInterfaceEmissionOutcome {
    pub outbound_movement: Option<Movement>,
    pub prior_admitted_seq: Option<u64>,
    pub delivered: Vec<InterfaceArrivalRecord>,
}

impl SystemInterfaceEmissionOutcome {
    pub fn admitted_seq(&self) -> u64 {
        self.outbound_movement
            .as_ref()
            .map(|movement| movement.seq.0)
            .or(self.prior_admitted_seq)
            .expect("an admitted interface emission has a movement or a prior admitted seq")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use octopus_descriptors::{ArtifactId, EeId, MarkId, VerifierId};
    use octopus_interfaces::{
        CheckpointAdmissionMetadata, CheckpointBindingPolicySnapshot, GrantProjection,
        InvocationMetadata, RuntimeInvocationMetadata,
    };
    use octopus_ledger::{InMemoryLog, Mark, MarkClass, RequiredVerifiedArtifact, Seq};

    #[test]
    fn durable_tail_index_requires_exact_metadata_within_the_small_identity() {
        let trace = TraceId("trace-tail-index".to_string());
        let artifact = ArtifactRef {
            id: ArtifactId::new("result:tail-index"),
            content: ContentHash::of_bytes(b"tail-index"),
        };
        let from_interface = InterfaceId::new("world_source");
        let to_interface = InterfaceId::new("world_target");
        let existing_metadata = InvocationMetadata::new(GrantProjection::default());
        let mut log = InMemoryLog::new();
        let movement = log.append(MovementDraft {
            trace: trace.clone(),
            kind: MovementKind::InterfaceOutbound,
            from: Endpoint::Interface(from_interface.clone()),
            to: Endpoint::Interface(to_interface.clone()),
            artifact: Some(artifact.clone()),
            invocation_metadata: Some(existing_metadata.clone().into()),
            causal_floor: None,
        });
        let movements = vec![movement];
        let mut index = DurableTailMovementIndex::from_movements(&movements);
        let key = durable_tail_movement_lookup_key(
            DurableTailMovementKind::Outbound,
            &trace,
            &artifact,
            &from_interface,
            &to_interface,
        );

        assert!(
            index
                .record_if_absent(key.clone(), existing_metadata.into())
                .is_none(),
            "an exact typed metadata match suppresses duplicate recovery"
        );

        let mut changed_metadata = InvocationMetadata::new(GrantProjection::default());
        changed_metadata.outbound_interface_bindings = vec![InterfaceId::new("world_other")];
        let changed_metadata: MovementMetadata = changed_metadata.into();
        assert!(
            index
                .record_if_absent(key.clone(), changed_metadata.clone())
                .is_some(),
            "the small identity alone cannot suppress different metadata"
        );
        assert!(
            index.record_if_absent(key, changed_metadata).is_none(),
            "metadata recovered in this pass is indexed before the next candidate"
        );
    }

    #[test]
    fn replay_recovers_only_the_exact_marked_checkpoint_generation() {
        let run = RunId::new("run_replay");
        let checkpoint = octopus_descriptors::CheckpointId::new("checkpoint_replay");
        let arriving_interface = InterfaceId::new("world_source");
        let outbound_interface = InterfaceId::new("world_target");
        let refused_interface = InterfaceId::new("world_refused");
        let artifact = ArtifactRef {
            id: ArtifactId::new("result:abc"),
            content: ContentHash::of_bytes(b"result"),
        };
        let required = RequiredVerifiedArtifact {
            mark: MarkId::new("result.ok"),
            artifact_label: "result".to_string(),
            verifier: VerifierId::new("result_verifier"),
            params: serde_json::json!({}),
        };
        let metadata = InvocationMetadata::new(GrantProjection {
            interfaces: vec![
                outbound_interface.as_str().into(),
                refused_interface.as_str().into(),
            ],
            ..GrantProjection::default()
        })
        .with_runtime(RuntimeInvocationMetadata {
            run: run.clone(),
            ee: EeId::new("ee_replay"),
            worker: InterfaceId::new("system_worker_replay"),
            checkpoint: checkpoint.clone(),
            interface: arriving_interface.clone(),
            epoch: 0,
            run_record_hash: Some(ContentHash::of_bytes(b"run record").to_hex()),
        })
        .with_outbound_interface_bindings(vec![
            outbound_interface.clone(),
            refused_interface.clone(),
        ])
        .with_checkpoint_binding_policy(CheckpointBindingPolicySnapshot {
            inbound: octopus_runtime::AuthoredInterfaceBinding::all(arriving_interface.clone()),
            outbound: vec![
                octopus_runtime::AuthoredInterfaceBinding::all(outbound_interface.clone()),
                octopus_runtime::AuthoredInterfaceBinding {
                    interface: refused_interface,
                    config: octopus_runtime::InterfaceBindingConfig {
                        artifact_policy: octopus_runtime::BindingArtifactPolicy::Allow {
                            names: vec!["other".to_string()],
                            namespaces: Vec::new(),
                        },
                    },
                },
            ],
        })
        .with_required_verified_artifacts(vec![required.clone()])
        .with_checkpoint_admission(CheckpointAdmissionMetadata {
            artifact_label: "result".to_string(),
            required_artifact_labels: vec!["result".to_string()],
            complete: true,
        });
        let mut log = InMemoryLog::new();
        let arrival = log.append(MovementDraft {
            trace: TraceId("trace-replay".to_string()),
            kind: MovementKind::InterfaceArrival,
            from: Endpoint::Interface(arriving_interface.clone()),
            to: Endpoint::Interface(InterfaceId::new("system_worker_replay")),
            artifact: Some(artifact.clone()),
            invocation_metadata: Some(metadata.clone().into()),
            causal_floor: None,
        });

        let before_mark = log.read_from(Seq(0));
        assert!(
            recover_durable_checkpoint_outbound_tail(&mut log, &before_mark)
                .unwrap()
                .is_empty()
        );

        let mark = Mark {
            name: required.mark.clone(),
            subject: checkpoint.clone(),
            run: run.clone(),
            run_epoch: 0,
            class: MarkClass::Success,
            evidence: Some(artifact.clone()),
            arrival_seq: None,
            verifier: required.verifier.clone(),
            verifier_package: Some(octopus_ledger::VerifierPackagePin {
                descriptor_content: ContentHash::of_bytes(b"descriptor"),
                script_content: ContentHash::of_bytes(b"script"),
            }),
            details: serde_json::json!({"passed": true}),
        };
        let admitted_mark = log.append(MovementDraft {
            trace: arrival.trace.clone(),
            kind: MovementKind::MarkOutcomeAdmitted(mark.name.clone()),
            from: Endpoint::Interface(arriving_interface.clone()),
            to: Endpoint::Interface(InterfaceId::new("system_worker_replay")),
            artifact: Some(artifact.clone()),
            invocation_metadata: Some(mark.clone().into()),
            causal_floor: Some(arrival.lamport),
        });
        let after_mark = log.read_from(Seq(0));
        let recovered = recover_durable_checkpoint_outbound_tail(&mut log, &after_mark).unwrap();
        assert_eq!(recovered.len(), 2);
        assert!(
            recovered
                .iter()
                .any(|movement| movement.kind == MovementKind::InterfaceOutbound)
        );
        assert!(
            recovered
                .iter()
                .any(|movement| movement.kind == MovementKind::DeliveryRefused)
        );
        assert!(
            recovered
                .iter()
                .all(|movement| movement.artifact.as_ref() == Some(&artifact))
        );
        assert!(
            recovered
                .iter()
                .all(|movement| movement.lamport > admitted_mark.lamport)
        );

        let complete = log.read_from(Seq(0));
        assert!(
            recover_durable_checkpoint_outbound_tail(&mut log, &complete)
                .unwrap()
                .is_empty()
        );
        assert_eq!(log.len(), 4);

        let next_artifact = ArtifactRef {
            id: ArtifactId::new("result:def"),
            content: ContentHash::of_bytes(b"next result"),
        };
        let mut next_metadata = metadata;
        let next_runtime = next_metadata.runtime.as_mut().unwrap();
        next_runtime.epoch = 1;
        next_runtime.run_record_hash = Some(ContentHash::of_bytes(b"next run record").to_hex());
        let next_arrival = log.append(MovementDraft {
            trace: TraceId("trace-replay-next".to_string()),
            kind: MovementKind::InterfaceArrival,
            from: Endpoint::Interface(arriving_interface.clone()),
            to: Endpoint::Interface(InterfaceId::new("system_worker_replay")),
            artifact: Some(next_artifact.clone()),
            invocation_metadata: Some(next_metadata.clone().into()),
            causal_floor: None,
        });
        let mut next_mark = mark;
        next_mark.run_epoch = 1;
        next_mark.evidence = Some(next_artifact.clone());
        log.append(MovementDraft {
            trace: next_arrival.trace.clone(),
            kind: MovementKind::MarkOutcomeAdmitted(next_mark.name.clone()),
            from: Endpoint::Interface(arriving_interface.clone()),
            to: Endpoint::Interface(InterfaceId::new("system_worker_replay")),
            artifact: Some(next_artifact.clone()),
            invocation_metadata: Some(next_mark.clone().into()),
            causal_floor: Some(next_arrival.lamport),
        });
        let next_generation = log.read_from(Seq(0));
        let next_recovered =
            recover_durable_checkpoint_outbound_tail(&mut log, &next_generation).unwrap();
        assert_eq!(next_recovered.len(), 2);
        assert!(
            next_recovered
                .iter()
                .all(|movement| movement.artifact.as_ref() == Some(&next_artifact))
        );
        assert!(next_recovered.iter().any(|movement| {
            matches!(
                movement
                    .invocation_metadata
                    .as_ref()
                    .and_then(|metadata| metadata.delivery_verdict()),
                Some(DeliveryVerdict::Refused { detail, .. }) if detail["epoch"] == 1
            )
        }));
        assert_eq!(log.len(), 8);

        let changed_snapshot_artifact = ArtifactRef {
            id: ArtifactId::new("result:ghi"),
            content: ContentHash::of_bytes(b"changed snapshot result"),
        };
        let mut changed_snapshot_metadata = next_metadata;
        changed_snapshot_metadata.runtime.as_mut().unwrap().epoch = 2;
        changed_snapshot_metadata
            .runtime
            .as_mut()
            .unwrap()
            .run_record_hash = Some(ContentHash::of_bytes(b"changed snapshot").to_hex());
        let changed_snapshot_arrival = log.append(MovementDraft {
            trace: TraceId("trace-replay-changed-snapshot".to_string()),
            kind: MovementKind::InterfaceArrival,
            from: Endpoint::Interface(arriving_interface.clone()),
            to: Endpoint::Interface(InterfaceId::new("system_worker_replay")),
            artifact: Some(changed_snapshot_artifact.clone()),
            invocation_metadata: Some(changed_snapshot_metadata.clone().into()),
            causal_floor: None,
        });
        let mut changed_snapshot_mark = next_mark;
        changed_snapshot_mark.run_epoch = 2;
        changed_snapshot_mark.evidence = Some(changed_snapshot_artifact.clone());
        log.append(MovementDraft {
            trace: changed_snapshot_arrival.trace.clone(),
            kind: MovementKind::MarkOutcomeAdmitted(changed_snapshot_mark.name.clone()),
            from: Endpoint::Interface(arriving_interface.clone()),
            to: Endpoint::Interface(InterfaceId::new("system_worker_replay")),
            artifact: Some(changed_snapshot_artifact.clone()),
            invocation_metadata: Some(changed_snapshot_mark.clone().into()),
            causal_floor: Some(changed_snapshot_arrival.lamport),
        });
        let changed_snapshot = log.read_from(Seq(0));
        let changed_snapshot_recovered =
            recover_durable_checkpoint_outbound_tail(&mut log, &changed_snapshot).unwrap();
        assert_eq!(changed_snapshot_recovered.len(), 2);
        assert!(
            changed_snapshot_recovered
                .iter()
                .all(|movement| { movement.artifact.as_ref() == Some(&changed_snapshot_artifact) })
        );
        assert_eq!(log.len(), 12);

        let mut ambiguous_log = InMemoryLog::new();
        let ambiguous_trace = TraceId("trace-replay-ambiguous".to_string());
        let first_ambiguous = ambiguous_log.append(MovementDraft {
            trace: ambiguous_trace.clone(),
            kind: MovementKind::InterfaceArrival,
            from: Endpoint::Interface(InterfaceId::new("world_source")),
            to: Endpoint::Interface(InterfaceId::new("system_worker_replay")),
            artifact: Some(changed_snapshot_artifact.clone()),
            invocation_metadata: Some(changed_snapshot_metadata.clone().into()),
            causal_floor: None,
        });
        let mut other_snapshot_metadata = changed_snapshot_metadata;
        other_snapshot_metadata
            .runtime
            .as_mut()
            .unwrap()
            .run_record_hash = Some(ContentHash::of_bytes(b"ambiguous snapshot").to_hex());
        ambiguous_log.append(MovementDraft {
            trace: ambiguous_trace.clone(),
            kind: MovementKind::InterfaceArrival,
            from: Endpoint::Interface(InterfaceId::new("world_source")),
            to: Endpoint::Interface(InterfaceId::new("system_worker_replay")),
            artifact: Some(changed_snapshot_artifact.clone()),
            invocation_metadata: Some(other_snapshot_metadata.into()),
            causal_floor: None,
        });
        ambiguous_log.append(MovementDraft {
            trace: ambiguous_trace,
            kind: MovementKind::MarkOutcomeAdmitted(changed_snapshot_mark.name.clone()),
            from: Endpoint::Interface(arriving_interface.clone()),
            to: Endpoint::Interface(InterfaceId::new("system_worker_replay")),
            artifact: Some(changed_snapshot_artifact),
            invocation_metadata: Some(changed_snapshot_mark.into()),
            causal_floor: Some(first_ambiguous.lamport),
        });
        let ambiguous = ambiguous_log.read_from(Seq(0));
        let error = recover_durable_checkpoint_outbound_tail(&mut ambiguous_log, &ambiguous)
            .expect_err("one mark cannot prove two pending arrival snapshots");
        assert!(error.to_string().contains("exact mark proof is ambiguous"));

        for failed in [[true, true], [true, false], [false, true]] {
            let mut rows = ambiguous.clone();
            for (row, rejected) in rows.iter_mut().zip(failed) {
                if rejected {
                    let Some(MovementMetadata::Invocation(metadata)) = &mut row.invocation_metadata
                        else { panic!("arrival must carry invocation metadata") };
                    metadata.verification_failure = Some(serde_json::from_value(
                        serde_json::json!({"error": "verifier rejected the artifact"}),
                    ).unwrap());
                }
            }
            let proofs = octopus_ledger::InterfaceArrivalMarkProofIndex::from_movements(&rows)
                .expect("rejected arrivals cannot compete for mark proof");
            for (row, rejected) in rows.iter().zip(failed) {
                assert_eq!(proofs.proof_lamport(row).is_some(), !rejected);
                assert_eq!(octopus_ledger::mark_outcome_proves_interface_arrival(row, &rows[2]), !rejected);
            }
            if failed == [true, true] {
                let mut probe = InMemoryLog::new();
                assert!(recover_durable_checkpoint_outbound_tail(&mut probe, &rows).unwrap().is_empty());
                assert_eq!(probe.len(), 0);
                let Some(MovementMetadata::Invocation(metadata)) = &mut rows[1].invocation_metadata
                    else { unreachable!() };
                metadata.replaces_arrival_seq = Some(ambiguous[0].seq.0);
                assert!(matches!(octopus_ledger::InterfaceArrivalMarkProofIndex::from_movements(&rows),
                    Err(octopus_ledger::LedgerError::InvalidInterfaceArrivalReplacement { .. })));
            }
        }
    }
}
