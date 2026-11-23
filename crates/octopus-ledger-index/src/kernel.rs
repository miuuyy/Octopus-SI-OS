use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ops::ControlFlow;

use octopus_descriptors::{ArtifactId, CheckpointId, InterfaceId, RunId};
use octopus_ledger::{
    ArtifactRef, Endpoint, InterfaceArrivalMarkProofKey, Movement, MovementKind, MovementLog,
    MovementMetadata, RuntimeInvocationMetadata, Seq, admitted_mark_outcome_proof_key,
    interface_arrival_mark_proof_key, mark_outcome_proves_interface_arrival,
};

mod fanout;
pub use fanout::RetainedArrivalState;

#[derive(Debug, thiserror::Error)]
pub enum LedgerIndexError {
    #[error(
        "movement log ({log_len} rows) is behind the index watermark ({watermark}); \
         this index does not belong to this ledger"
    )]
    LogBehindIndex { watermark: u64, log_len: u64 },
    #[error("movement log returned seq {got} where {expected} was expected (non-contiguous tail)")]
    NonContiguousTail { expected: u64, got: u64 },
    #[error("interface-arrival movement for run '{run}' has no metadata.runtime.checkpoint")]
    MalformedArrivalMetadata { run: String },
    #[error(
        "relay arrival for run '{run}' on interface '{interface}' has no exact source movement provenance"
    )]
    UnattributedRelayArrival { run: String, interface: String },
    #[error(
        "relay source {source_seq} has an unfinished arrival against an older run record for '{run}'"
    )]
    StaleRelayArrival { source_seq: u64, run: String },
    #[error("retained arrival scope is invalid: {0}")]
    InvalidArrivalScope(String),
}

#[derive(Debug, Default)]
pub struct ArrivalDedupIndex {
    watermark: u64,
    arrival_artifacts: BTreeSet<ArtifactId>,
    admitted_emissions: BTreeMap<(CheckpointIdentity, InterfaceId, ArtifactId), u64>,
    admitted_checkpoints: BTreeSet<CheckpointIdentity>,
    admitted_route_artifacts: BTreeSet<(CheckpointIdentity, ArtifactId)>,
    admitted_requirements: BTreeSet<(CheckpointIdentity, String)>,
    pending_required_arrivals: HashMap<InterfaceArrivalMarkProofKey, Movement>,
    pending_requirement_sequences: BTreeMap<(CheckpointIdentity, String), Seq>,
    completion_requirements: BTreeMap<CheckpointIdentity, BTreeSet<String>>,
    malformed_runs: BTreeSet<RunId>,
    malformed_arrival: bool,
    relay_sources: BTreeMap<u64, (InterfaceId, ArtifactRef)>,
    relay_source_sequences: BTreeMap<InterfaceId, BTreeSet<u64>>,
    relay_receipts:
        BTreeMap<(u64, RunId), BTreeMap<CheckpointIdentity, (Option<String>, RelayArrivalState)>>,
    unattributed_arrivals: BTreeMap<(RunId, InterfaceId, ArtifactId), Seq>,
    fanout: fanout::FanoutIndex,
}

#[derive(Clone, Debug)]
pub enum RelayArrivalState {
    Unseen,
    Pending(Movement),
    Finished,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct CheckpointIdentity {
    run: RunId,
    checkpoint: String,
    epoch: u64,
}

impl CheckpointIdentity {
    fn new(run: &RunId, checkpoint: &CheckpointId, epoch: u64) -> Self {
        Self {
            run: run.clone(),
            checkpoint: checkpoint.as_str().to_string(),
            epoch,
        }
    }
}

/// Read-only retained-delivery projection for one emitting interface.
/// The reader validates its complete ledger cursor; this projection receives
/// only that interface's real movements, in their original sequence order.
/// It exposes no kernel admission decisions.
pub struct InterfaceArrivalProjection {
    interface: InterfaceId,
    last_sequence: Option<u64>,
    index: ArrivalDedupIndex,
}

impl InterfaceArrivalProjection {
    pub fn new(interface: InterfaceId) -> Self {
        Self { interface, last_sequence: None, index: ArrivalDedupIndex::new() }
    }

    pub fn observe(&mut self, movement: &Movement) -> Result<(), LedgerIndexError> {
        if movement.from != Endpoint::Interface(self.interface.clone())
            || self.last_sequence.is_some_and(|last| movement.seq.0 <= last)
        {
            return Err(LedgerIndexError::InvalidArrivalScope(
                "interface projection received a different source or unordered movement".into(),
            ));
        }
        self.index.note(movement);
        self.last_sequence = Some(movement.seq.0);
        Ok(())
    }

    pub fn retained_arrival_scope(&self, identity: &str) -> Result<Option<Movement>, LedgerIndexError> {
        self.index.retained_arrival_scope(&self.interface, identity)
    }

    pub fn retained_arrival_edges(&self, scope_seq: u64) -> Result<Vec<(RuntimeInvocationMetadata, RetainedArrivalState)>, LedgerIndexError> {
        self.index.retained_arrival_edges(scope_seq)
    }
}

impl ArrivalDedupIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn watermark(&self) -> u64 {
        self.watermark
    }

    pub fn catch_up(&mut self, log: &(dyn MovementLog + Send)) -> Result<(), LedgerIndexError> {
        let log_len = log.len() as u64;
        if log_len < self.watermark {
            return Err(LedgerIndexError::LogBehindIndex {
                watermark: self.watermark,
                log_len,
            });
        }
        if log_len == self.watermark {
            return Ok(());
        }
        let from = Seq(self.watermark);
        let mut failure = None;
        log.scan_from(from, &mut |movement| {
            if movement.seq.0 != self.watermark {
                failure = Some(LedgerIndexError::NonContiguousTail {
                    expected: self.watermark,
                    got: movement.seq.0,
                });
                return ControlFlow::Break(());
            }
            self.note(&movement);
            self.watermark += 1;
            ControlFlow::Continue(())
        });
        match failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    pub fn catch_up_movements(
        &mut self,
        movements: impl IntoIterator<Item = Movement>,
    ) -> Result<(), LedgerIndexError> {
        for movement in movements {
            self.catch_up_movement(&movement)?;
        }
        Ok(())
    }

    pub fn catch_up_movement_slice(
        &mut self,
        movements: &[Movement],
    ) -> Result<(), LedgerIndexError> {
        for movement in movements {
            self.catch_up_movement(movement)?;
        }
        Ok(())
    }

    fn catch_up_movement(&mut self, movement: &Movement) -> Result<(), LedgerIndexError> {
        if movement.seq.0 != self.watermark {
            return Err(LedgerIndexError::NonContiguousTail {
                expected: self.watermark,
                got: movement.seq.0,
            });
        }
        self.note(movement);
        self.watermark += 1;
        Ok(())
    }

    fn note(&mut self, movement: &Movement) {
        if matches!(movement.invocation_metadata, Some(MovementMetadata::ArrivalFanout(_))) {
            self.fanout.note_scope(movement);
            return;
        }
        if movement.kind == MovementKind::DeliveryRefused {
            if let Some(MovementMetadata::Delivery(metadata)) =
                movement.invocation_metadata.as_ref()
            {
                if let Some(runtime) = metadata.runtime.as_ref() {
                    self.note_relay_receipt(
                        movement,
                        runtime,
                        metadata.source_movement_seq,
                        RelayArrivalState::Finished,
                    );
                }
            }
            return;
        }
        if movement.kind == MovementKind::InterfaceOutbound {
            let (Endpoint::Interface(_), Endpoint::Interface(interface), Some(artifact)) =
                (&movement.from, &movement.to, movement.artifact.as_ref())
            else {
                return;
            };
            let Some(runtime) = movement
                .invocation_metadata
                .as_ref()
                .and_then(|metadata| metadata.invocation())
                .and_then(|metadata| metadata.runtime.as_ref())
            else {
                return;
            };
            if &runtime.interface != interface {
                return;
            }
            let generation =
                CheckpointIdentity::new(&runtime.run, &runtime.checkpoint, runtime.epoch);
            self.relay_sources
                .insert(movement.seq.0, (interface.clone(), artifact.clone()));
            self.relay_source_sequences
                .entry(interface.clone())
                .or_default()
                .insert(movement.seq.0);
            self.admitted_emissions
                .entry((generation, interface.clone(), artifact.id.clone()))
                .or_insert(movement.seq.0);
            return;
        }
        if matches!(movement.kind, MovementKind::MarkOutcomeAdmitted(_)) {
            if let Some(key) = admitted_mark_outcome_proof_key(movement) {
                let proven = self
                    .pending_required_arrivals
                    .get(&key)
                    .is_some_and(|arrival| {
                        mark_outcome_proves_interface_arrival(arrival, movement)
                    });
                if proven {
                    if let Some(arrival) = self.pending_required_arrivals.remove(&key) {
                        self.commit_required_arrival(&arrival);
                    }
                }
            }
            return;
        }
        if movement.kind != MovementKind::InterfaceArrival {
            return;
        }
        let Some(run) = movement.run() else {
            self.malformed_arrival = true;
            return;
        };
        let metadata = movement
            .invocation_metadata
            .as_ref()
            .and_then(|metadata| metadata.invocation());
        let Some(metadata) = metadata else {
            self.malformed_runs.insert(run.clone());
            return;
        };
        let Some(runtime) = metadata.runtime.as_ref() else {
            self.malformed_runs.insert(run.clone());
            return;
        };
        let source_matches = matches!(
            &movement.from,
            Endpoint::Interface(interface) if interface == &runtime.interface
        );
        if movement.to != Endpoint::Interface(runtime.worker.clone()) || !source_matches {
            self.malformed_runs.insert(run.clone());
            return;
        }
        let Some(artifact) = movement.artifact.as_ref() else {
            self.malformed_runs.insert(run.clone());
            return;
        };
        if metadata.source_movement_seq.is_none() {
            self.unattributed_arrivals.insert(
                (run.clone(), runtime.interface.clone(), artifact.id.clone()),
                movement.seq,
            );
        }
        if metadata.verification_failure.is_some() {
            self.note_relay_receipt(
                movement,
                runtime,
                metadata.source_movement_seq,
                RelayArrivalState::Finished,
            );
            self.arrival_artifacts.insert(artifact.id.clone());
            return;
        }
        let generation = CheckpointIdentity::new(run, &runtime.checkpoint, runtime.epoch);
        match metadata.checkpoint_admission.as_ref() {
            Some(admission) => {
                let required: BTreeSet<&str> = admission
                    .required_artifact_labels
                    .iter()
                    .map(String::as_str)
                    .collect();
                if admission.artifact_label.trim().is_empty()
                    || required.len() != admission.required_artifact_labels.len()
                    || admission
                        .required_artifact_labels
                        .iter()
                        .any(|label| label.trim().is_empty())
                    || (!required.is_empty()
                        && !required.contains(admission.artifact_label.as_str()))
                    || (required.is_empty() && !admission.complete)
                {
                    self.malformed_runs.insert(run.clone());
                    return;
                }
                if !required.is_empty() {
                    let declared: BTreeSet<&str> = metadata
                        .required_verified_artifacts
                        .iter()
                        .map(|required| required.artifact_label.as_str())
                        .collect();
                    if declared.len() != metadata.required_verified_artifacts.len()
                        || declared != required
                    {
                        self.malformed_runs.insert(run.clone());
                        return;
                    }
                    let Some(proof_key) = interface_arrival_mark_proof_key(movement) else {
                        self.malformed_runs.insert(run.clone());
                        return;
                    };
                    let requirement = (generation, admission.artifact_label.clone());
                    let previous = self.pending_required_arrivals.get(&proof_key);
                    let previous_sequence = self.pending_requirement_sequences.get(&requirement);
                    let valid = match metadata.replaces_arrival_seq {
                        Some(sequence) => previous_sequence == Some(&Seq(sequence))
                            && previous.is_some_and(|prior| octopus_ledger::replaces_pending_interface_arrival(prior, movement)),
                        None => previous.is_none() && previous_sequence.is_none(),
                    };
                    if !valid {
                        self.malformed_runs.insert(run.clone());
                        return;
                    }
                    self.note_relay_receipt(
                        movement,
                        runtime,
                        metadata.source_movement_seq,
                        RelayArrivalState::Pending(movement.clone()),
                    );
                    self.pending_requirement_sequences.insert(requirement, movement.seq);
                    self.pending_required_arrivals.insert(proof_key, movement.clone());
                    return;
                }
                if metadata.replaces_arrival_seq.is_some() {
                    self.malformed_runs.insert(run.clone());
                    return;
                }
                self.note_relay_receipt(
                    movement,
                    runtime,
                    metadata.source_movement_seq,
                    RelayArrivalState::Finished,
                );
                self.arrival_artifacts.insert(artifact.id.clone());
                if !self
                    .admitted_route_artifacts
                    .insert((generation, artifact.id.clone()))
                {
                    self.malformed_runs.insert(run.clone());
                }
            }
            None => {
                self.note_relay_receipt(
                    movement,
                    runtime,
                    metadata.source_movement_seq,
                    RelayArrivalState::Finished,
                );
                self.arrival_artifacts.insert(artifact.id.clone());
                self.admitted_checkpoints.insert(generation);
            }
        }
    }

    fn commit_required_arrival(&mut self, movement: &Movement) {
        let Some(run) = movement.run() else {
            self.malformed_arrival = true;
            return;
        };
        let Some(metadata) = movement
            .invocation_metadata
            .as_ref()
            .and_then(|metadata| metadata.invocation())
        else {
            self.malformed_runs.insert(run.clone());
            return;
        };
        let Some(runtime) = metadata.runtime.as_ref() else {
            self.malformed_runs.insert(run.clone());
            return;
        };
        let Some(admission) = metadata.checkpoint_admission.as_ref() else {
            self.malformed_runs.insert(run.clone());
            return;
        };
        let Some(artifact) = movement.artifact.as_ref() else {
            self.malformed_runs.insert(run.clone());
            return;
        };
        let generation = CheckpointIdentity::new(run, &runtime.checkpoint, runtime.epoch);
        self.pending_requirement_sequences
            .remove(&(generation.clone(), admission.artifact_label.clone()));
        self.note_relay_receipt(
            movement,
            runtime,
            metadata.source_movement_seq,
            RelayArrivalState::Finished,
        );
        self.arrival_artifacts.insert(artifact.id.clone());
        self.admitted_requirements
            .insert((generation.clone(), admission.artifact_label.clone()));
        if admission.complete {
            self.completion_requirements.insert(
                generation.clone(),
                admission.required_artifact_labels.iter().cloned().collect(),
            );
        }
        let complete = self
            .completion_requirements
            .get(&generation)
            .is_some_and(|required| {
                required.iter().all(|label| {
                    self.admitted_requirements
                        .contains(&(generation.clone(), label.clone()))
                })
            });
        if complete {
            self.admitted_checkpoints.insert(generation);
        }
    }

    fn note_relay_receipt(
        &mut self,
        movement: &Movement,
        runtime: &RuntimeInvocationMetadata,
        source_movement_seq: Option<u64>,
        state: RelayArrivalState,
    ) {
        self.fanout.note_edge(movement, runtime, &state);
        let Some(source_seq) = source_movement_seq else {
            return;
        };
        let valid = self
            .relay_sources
            .get(&source_seq)
            .is_some_and(|(interface, artifact)| {
                source_seq < movement.seq.0
                    && interface == &runtime.interface
                    && movement.artifact.as_ref() == Some(artifact)
                    && movement.from == Endpoint::Interface(interface.clone())
                    && movement.to == Endpoint::Interface(runtime.worker.clone())
                    && runtime.run_record_hash.is_some()
            });
        if !valid {
            self.malformed_runs.insert(runtime.run.clone());
            return;
        }
        let generation = CheckpointIdentity::new(&runtime.run, &runtime.checkpoint, runtime.epoch);
        self.relay_receipts
            .entry((source_seq, runtime.run.clone()))
            .or_default()
            .insert(generation, (runtime.run_record_hash.clone(), state));
    }

    pub fn retained_arrival_scope(&self, interface: &InterfaceId, identity: &str) -> Result<Option<Movement>, LedgerIndexError> {
        let scope = self.fanout.scope(interface, identity)?;
        let targets = scope.as_ref().and_then(|movement| match &movement.invocation_metadata {
            Some(MovementMetadata::ArrivalFanout(metadata)) => Some(&metadata.arrival_fanout.targets),
            _ => None,
        });
        self.ensure_retained_arrival_metadata(targets.into_iter().flatten().map(|target| &target.run))?;
        Ok(scope)
    }

    pub fn retained_arrival_edges(&self, scope_seq: u64) -> Result<Vec<(RuntimeInvocationMetadata, RetainedArrivalState)>, LedgerIndexError> {
        let edges = self.fanout.edges(scope_seq)?;
        self.ensure_retained_arrival_metadata(edges.iter().map(|(target, _)| &target.run))?;
        Ok(edges)
    }

    fn ensure_retained_arrival_metadata<'a>(&self, runs: impl Iterator<Item = &'a RunId>) -> Result<(), LedgerIndexError> {
        if self.malformed_arrival {
            return Err(LedgerIndexError::InvalidArrivalScope("arrival evidence has no run provenance".into()));
        }
        for run in runs {
            if self.malformed_runs.contains(run) {
                return Err(LedgerIndexError::MalformedArrivalMetadata { run: run.to_string() });
            }
        }
        Ok(())
    }

    pub fn relay_source_sequences(&self, interfaces: &BTreeSet<InterfaceId>) -> Vec<u64> {
        interfaces
            .iter()
            .filter_map(|id| self.relay_source_sequences.get(id))
            .flat_map(|sequences| sequences.iter().copied())
            .collect()
    }

    pub fn relay_arrival_state(
        &self,
        source_seq: u64,
        run: &RunId,
        checkpoint: &CheckpointId,
        epoch: u64,
        run_record_hash: &str,
    ) -> Result<RelayArrivalState, LedgerIndexError> {
        if self.malformed_arrival || self.malformed_runs.contains(run) {
            return Err(LedgerIndexError::MalformedArrivalMetadata {
                run: run.to_string(),
            });
        }
        let generation = CheckpointIdentity::new(run, checkpoint, epoch);
        if let Some(receipts) = self.relay_receipts.get(&(source_seq, run.clone())) {
            if let Some((_, state)) = receipts.get(&generation) {
                return Ok(state.clone());
            }
            if !receipts
                .values()
                .any(|(hash, _)| hash.as_deref() == Some(run_record_hash))
            {
                if receipts
                    .values()
                    .any(|(_, state)| matches!(state, RelayArrivalState::Pending(_)))
                {
                    return Err(LedgerIndexError::StaleRelayArrival {
                        source_seq,
                        run: run.to_string(),
                    });
                }
                return Ok(RelayArrivalState::Finished);
            }
        }
        if let Some((interface, artifact)) = self.relay_sources.get(&source_seq) {
            if self
                .unattributed_arrivals
                .get(&(run.clone(), interface.clone(), artifact.id.clone()))
                .is_some_and(|arrival_seq| arrival_seq.0 > source_seq)
            {
                return Err(LedgerIndexError::UnattributedRelayArrival {
                    run: run.to_string(),
                    interface: interface.to_string(),
                });
            }
        }
        Ok(RelayArrivalState::Unseen)
    }

    pub fn checkpoint_admitted(
        &self,
        run: &RunId,
        checkpoint: &CheckpointId,
        epoch: u64,
    ) -> Result<bool, LedgerIndexError> {
        if self.malformed_arrival || self.malformed_runs.contains(run) {
            return Err(LedgerIndexError::MalformedArrivalMetadata {
                run: run.as_str().to_string(),
            });
        }
        Ok(self
            .admitted_checkpoints
            .contains(&CheckpointIdentity::new(run, checkpoint, epoch)))
    }

    pub fn route_artifact_admitted(
        &self,
        run: &RunId,
        checkpoint: &CheckpointId,
        epoch: u64,
        artifact: &ArtifactId,
    ) -> Result<bool, LedgerIndexError> {
        if self.malformed_arrival || self.malformed_runs.contains(run) {
            return Err(LedgerIndexError::MalformedArrivalMetadata {
                run: run.as_str().to_string(),
            });
        }
        Ok(self.admitted_route_artifacts.contains(&(
            CheckpointIdentity::new(run, checkpoint, epoch),
            artifact.clone(),
        )))
    }

    pub fn arrival_evidence_duplicate(&self, ids: &[&ArtifactId]) -> Option<ArtifactId> {
        ids.iter()
            .find(|id| self.arrival_artifacts.contains(**id))
            .map(|id| (*id).clone())
    }

    pub fn requirement_admitted(
        &self,
        run: &RunId,
        checkpoint: &CheckpointId,
        epoch: u64,
        artifact_label: &str,
    ) -> Result<bool, LedgerIndexError> {
        if self.malformed_arrival || self.malformed_runs.contains(run) {
            return Err(LedgerIndexError::MalformedArrivalMetadata {
                run: run.as_str().to_string(),
            });
        }
        Ok(self.admitted_requirements.contains(&(
            CheckpointIdentity::new(run, checkpoint, epoch),
            artifact_label.to_string(),
        )))
    }

    pub fn pending_requirement_seq(
        &self,
        run: &RunId,
        checkpoint: &CheckpointId,
        epoch: u64,
        artifact_label: &str,
    ) -> Result<Option<Seq>, LedgerIndexError> {
        if self.malformed_arrival || self.malformed_runs.contains(run) {
            return Err(LedgerIndexError::MalformedArrivalMetadata {
                run: run.as_str().to_string(),
            });
        }
        Ok(self
            .pending_requirement_sequences
            .get(&(
                CheckpointIdentity::new(run, checkpoint, epoch),
                artifact_label.to_string(),
            ))
            .copied())
    }

    pub fn admitted_emission_seq(
        &self,
        run: &RunId,
        checkpoint: &CheckpointId,
        epoch: u64,
        interface: &InterfaceId,
        artifact: &ArtifactId,
    ) -> Option<u64> {
        self.admitted_emissions
            .get(&(
                CheckpointIdentity::new(run, checkpoint, epoch),
                interface.clone(),
                artifact.clone(),
            ))
            .copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use octopus_descriptors::{EeId, InterfaceId, MarkId, VerifierId};
    use octopus_interfaces::{
        CheckpointAdmissionMetadata, DeliveryVerdict, GrantProjection, InvocationMetadata,
        RuntimeInvocationMetadata, VerificationFailureMetadata,
    };
    use octopus_ledger::{
        ArtifactRef, ContentHash, InMemoryLog, Mark, MarkClass, MovementDraft,
        RequiredVerifiedArtifact, TraceId, VerifierPackagePin,
    };

    fn arrival_draft(
        run: &str,
        interface: &str,
        checkpoint: Option<&str>,
        artifact_label: &str,
    ) -> MovementDraft {
        let bytes = artifact_label.as_bytes();
        MovementDraft {
            trace: TraceId("t".into()),
            kind: MovementKind::InterfaceArrival,
            from: Endpoint::Interface(InterfaceId::new(interface)),
            to: Endpoint::Interface(InterfaceId::new("system_worker_test")),
            artifact: Some(ArtifactRef {
                id: ArtifactId::new(format!(
                    "{artifact_label}:{}",
                    ContentHash::of_bytes(bytes).to_hex()
                )),
                content: ContentHash::of_bytes(bytes),
            }),
            invocation_metadata: checkpoint.map(|checkpoint| {
                InvocationMetadata::new(GrantProjection::default())
                    .with_runtime(RuntimeInvocationMetadata {
                        run: RunId::new(run),
                        ee: EeId::new("ee_test"),
                        worker: InterfaceId::new("system_worker_test"),
                        checkpoint: CheckpointId::new(checkpoint),
                        interface: InterfaceId::new(interface),
                        epoch: 0,
                        run_record_hash: None,
                    })
                    .into()
            }),
            causal_floor: None,
        }
    }

    #[test]
    fn unattributed_arrival_only_ambiguates_sources_already_written() {
        let mut legacy = arrival_draft("receiver", "system_relay", Some("old"), "reply");
        let Some(MovementMetadata::Invocation(metadata)) = legacy.invocation_metadata.as_mut()
        else {
            unreachable!()
        };
        metadata.runtime.as_mut().unwrap().run_record_hash = Some("old-record".into());
        let mut source = legacy.clone();
        source.kind = MovementKind::InterfaceOutbound;
        source.from = Endpoint::Interface(InterfaceId::new("system_source"));
        source.to = Endpoint::Interface(InterfaceId::new("system_relay"));
        let Some(MovementMetadata::Invocation(metadata)) = source.invocation_metadata.as_mut()
        else {
            unreachable!()
        };
        metadata.runtime.as_mut().unwrap().run = RunId::new("emitter");
        let mut log = octopus_ledger::InMemoryLog::new();
        let before = log.append(source.clone());
        let old_arrival = log.append(legacy);
        let Some(MovementMetadata::Invocation(metadata)) = source.invocation_metadata.as_mut()
        else {
            unreachable!()
        };
        metadata.runtime.as_mut().unwrap().epoch = 1;
        let after = log.append(source);
        assert!(before.seq < old_arrival.seq && old_arrival.seq < after.seq);
        let mut index = ArrivalDedupIndex::new();
        index.catch_up(&log).unwrap();
        let run = RunId::new("receiver");
        let checkpoint = CheckpointId::new("new");
        assert!(matches!(
            index.relay_arrival_state(before.seq.0, &run, &checkpoint, 1, "new-record"),
            Err(LedgerIndexError::UnattributedRelayArrival { .. })
        ));
        assert!(matches!(
            index
                .relay_arrival_state(after.seq.0, &run, &checkpoint, 1, "new-record")
                .unwrap(),
            RelayArrivalState::Unseen
        ));
        let mut arrival = arrival_draft("receiver", "system_relay", Some("new"), "reply");
        let Some(MovementMetadata::Invocation(metadata)) = arrival.invocation_metadata.as_mut()
        else {
            unreachable!()
        };
        metadata.runtime.as_mut().unwrap().run_record_hash = Some("new-record".into());
        metadata.runtime.as_mut().unwrap().epoch = 1;
        metadata.source_movement_seq = Some(after.seq.0);
        metadata.checkpoint_admission = Some(octopus_ledger::CheckpointAdmissionMetadata {
            artifact_label: "reply".into(),
            required_artifact_labels: vec![],
            complete: true,
        });
        let arrival = log.append(arrival);
        index.catch_up(&log).unwrap();
        assert!(matches!(
            index
                .relay_arrival_state(after.seq.0, &run, &checkpoint, 1, "new-record")
                .unwrap(),
            RelayArrivalState::Finished
        ));
        assert!(!index.checkpoint_admitted(&run, &checkpoint, 1).unwrap());
        assert!(index
            .route_artifact_admitted(&run, &checkpoint, 1, &arrival.artifact.as_ref().unwrap().id)
            .unwrap());
        assert!(matches!(
            index.relay_arrival_state(before.seq.0, &run, &checkpoint, 1, "new-record"),
            Err(LedgerIndexError::UnattributedRelayArrival { .. })
        ));
    }

    fn mark_draft(arrival: &Movement, mark: &str, verifier: &str) -> MovementDraft {
        let runtime = arrival
            .invocation_metadata
            .as_ref()
            .and_then(|metadata| metadata.invocation())
            .and_then(|metadata| metadata.runtime.as_ref())
            .unwrap();
        let evidence = arrival.artifact.clone().unwrap();
        let mark = Mark {
            name: MarkId::new(mark),
            subject: runtime.checkpoint.clone(),
            run: runtime.run.clone(),
            run_epoch: runtime.epoch,
            class: MarkClass::Success,
            evidence: Some(evidence.clone()),
            arrival_seq: None,
            verifier: VerifierId::new(verifier),
            verifier_package: Some(VerifierPackagePin {
                descriptor_content: ContentHash::of_bytes(b"descriptor"),
                script_content: ContentHash::of_bytes(b"script"),
            }),
            details: serde_json::json!({}),
        };
        MovementDraft {
            trace: arrival.trace.clone(),
            kind: MovementKind::MarkOutcomeAdmitted(mark.name.clone()),
            from: arrival.from.clone(),
            to: arrival.to.clone(),
            artifact: Some(evidence),
            invocation_metadata: Some(mark.into()),
            causal_floor: Some(arrival.lamport),
        }
    }

    fn set_generation(draft: &mut MovementDraft, epoch: u64, run_record_hash: Option<&str>) {
        let Some(octopus_ledger::MovementMetadata::Invocation(metadata)) =
            draft.invocation_metadata.as_mut()
        else {
            panic!("arrival draft carries invocation metadata");
        };
        let runtime = metadata.runtime.as_mut().unwrap();
        runtime.epoch = epoch;
        runtime.run_record_hash = run_record_hash.map(str::to_string);
    }

    #[test]
    fn admitted_emission_requires_interface_provenance_and_typed_coordinates() {
        let mut log = InMemoryLog::new();
        let artifact = ArtifactRef {
            id: ArtifactId::new("out.one:abc"),
            content: ContentHash::of_bytes(b"out.one"),
        };
        log.append(MovementDraft {
            trace: TraceId("t".into()),
            kind: MovementKind::InterfaceOutbound,
            from: Endpoint::Interface(InterfaceId::new("system_room_exact")),
            to: Endpoint::Interface(InterfaceId::new("world_provider_exact")),
            artifact: Some(artifact.clone()),
            invocation_metadata: Some(
                InvocationMetadata::new(GrantProjection::default())
                    .with_runtime(RuntimeInvocationMetadata {
                        run: RunId::new("run_a"),
                        ee: EeId::new("ee_test"),
                        worker: InterfaceId::new("system_worker_test"),
                        checkpoint: CheckpointId::new("c1"),
                        interface: InterfaceId::new("world_provider_exact"),
                        epoch: 0,
                        run_record_hash: None,
                    })
                    .into(),
            ),
            causal_floor: None,
        });
        log.append(MovementDraft {
            trace: TraceId("t".into()),
            kind: MovementKind::InterfaceOutbound,
            from: Endpoint::Interface(InterfaceId::new("system_worker_test")),
            to: Endpoint::Interface(InterfaceId::new("world_provider_exact")),
            artifact: Some(artifact.clone()),
            invocation_metadata: Some(DeliveryVerdict::Admitted { seq: 1 }.into()),
            causal_floor: None,
        });

        let mut index = ArrivalDedupIndex::new();
        index.catch_up(&log).unwrap();
        assert_eq!(
            index.admitted_emission_seq(
                &RunId::new("run_a"),
                &CheckpointId::new("c1"),
                0,
                &InterfaceId::new("world_provider_exact"),
                &artifact.id,
            ),
            Some(0)
        );
        assert_eq!(
            index.admitted_emission_seq(
                &RunId::new("run_a"),
                &CheckpointId::new("legacy"),
                0,
                &InterfaceId::new("world_provider_exact"),
                &artifact.id,
            ),
            None
        );
    }

    #[test]
    fn catch_up_is_incremental_and_answers_match_a_direct_scan() {
        let mut log = InMemoryLog::new();
        log.append(arrival_draft("run_a", "world_x", Some("c1"), "in.one"));
        log.append(MovementDraft {
            trace: TraceId("t".into()),
            kind: MovementKind::ArtifactMoved,
            from: Endpoint::Interface(InterfaceId::new("system_worker_test")),
            to: Endpoint::Interface(InterfaceId::new("system_worker_test")),
            artifact: None,
            invocation_metadata: None,
            causal_floor: None,
        });

        let mut index = ArrivalDedupIndex::new();
        index.catch_up(&log).unwrap();
        assert_eq!(index.watermark(), 2);
        assert!(
            index
                .checkpoint_admitted(&RunId::new("run_a"), &CheckpointId::new("c1"), 0)
                .unwrap()
        );
        assert!(
            !index
                .checkpoint_admitted(&RunId::new("run_a"), &CheckpointId::new("c2"), 0)
                .unwrap()
        );

        log.append(arrival_draft("run_a", "world_x", Some("c2"), "in.two"));
        index.catch_up(&log).unwrap();
        assert_eq!(index.watermark(), 3);
        assert!(
            index
                .checkpoint_admitted(&RunId::new("run_a"), &CheckpointId::new("c2"), 0)
                .unwrap()
        );

        let seen = ArtifactId::new(format!(
            "in.one:{}",
            ContentHash::of_bytes(b"in.one").to_hex()
        ));
        let unseen = ArtifactId::new("never:00");
        assert_eq!(
            index.arrival_evidence_duplicate(&[&unseen, &seen]),
            Some(seen)
        );
        assert_eq!(index.arrival_evidence_duplicate(&[&unseen]), None);
    }

    #[test]
    fn checkpoint_dedup_is_scoped_to_the_exact_authored_generation() {
        let mut log = InMemoryLog::new();
        log.append(arrival_draft("run_a", "world_x", Some("c1"), "first"));
        let mut next_epoch = arrival_draft("run_a", "world_x", Some("c1"), "second");
        set_generation(&mut next_epoch, 1, Some("snapshot-a"));
        log.append(next_epoch);
        let mut next_snapshot = arrival_draft("run_a", "world_x", Some("c1"), "third");
        set_generation(&mut next_snapshot, 1, Some("snapshot-b"));
        log.append(next_snapshot);

        let mut index = ArrivalDedupIndex::new();
        index.catch_up(&log).unwrap();
        let run = RunId::new("run_a");
        let checkpoint = CheckpointId::new("c1");
        assert!(index.checkpoint_admitted(&run, &checkpoint, 0).unwrap());
        assert!(index.checkpoint_admitted(&run, &checkpoint, 1).unwrap());
        assert!(index.checkpoint_admitted(&run, &checkpoint, 1).unwrap());
        assert!(!index.checkpoint_admitted(&run, &checkpoint, 2).unwrap());
    }

    #[test]
    fn an_unattributed_malformed_arrival_fails_closed() {
        let mut log = InMemoryLog::new();
        log.append(arrival_draft("run_a", "world_x", None, "in.bad"));
        log.append(arrival_draft("run_b", "world_y", Some("c1"), "in.good"));

        let mut index = ArrivalDedupIndex::new();
        index.catch_up(&log).unwrap();
        assert!(matches!(
            index.checkpoint_admitted(&RunId::new("run_a"), &CheckpointId::new("c1"), 0),
            Err(LedgerIndexError::MalformedArrivalMetadata { .. })
        ));
        assert!(
            index
                .checkpoint_admitted(&RunId::new("run_b"), &CheckpointId::new("c1"), 0)
                .is_err()
        );
    }

    #[test]
    fn a_failed_verifier_arrival_is_evidence_but_not_checkpoint_admission() {
        let mut log = InMemoryLog::new();
        let metadata = InvocationMetadata::new(GrantProjection::default())
            .with_runtime(RuntimeInvocationMetadata {
                run: RunId::new("run_a"),
                ee: EeId::new("ee_test"),
                worker: InterfaceId::new("system_worker_test"),
                checkpoint: CheckpointId::new("c1"),
                interface: InterfaceId::new("world_x"),
                epoch: 0,
                run_record_hash: Some("run-record-hash".to_string()),
            })
            .with_verification_failure(VerificationFailureMetadata {
                error: "verifier_rejected".to_string(),
                verifier: Some(VerifierId::new("test.verifier")),
                mark: Some(MarkId::new("test.mark")),
                artifact_label: None,
                run: Some(RunId::new("run_a")),
                checkpoint: Some(CheckpointId::new("c1")),
                verifier_package: None,
                details: None,
            });
        let artifact = ArtifactRef {
            id: ArtifactId::new("in.one:x"),
            content: ContentHash::of_bytes(b"in.one"),
        };
        log.append(MovementDraft {
            trace: TraceId("t".into()),
            kind: MovementKind::InterfaceArrival,
            from: Endpoint::Interface(InterfaceId::new("world_x")),
            to: Endpoint::Interface(InterfaceId::new("system_worker_test")),
            artifact: Some(artifact.clone()),
            invocation_metadata: Some(metadata.clone().into()),
            causal_floor: None,
        });
        log.append(MovementDraft {
            trace: TraceId("t".into()),
            kind: MovementKind::VerificationFailed,
            from: Endpoint::Interface(InterfaceId::new("world_x")),
            to: Endpoint::Interface(InterfaceId::new("system_worker_test")),
            artifact: Some(artifact.clone()),
            invocation_metadata: Some(metadata.into()),
            causal_floor: None,
        });

        let mut index = ArrivalDedupIndex::new();
        index.catch_up(&log).unwrap();
        assert!(
            !index
                .checkpoint_admitted(&RunId::new("run_a"), &CheckpointId::new("c1"), 0)
                .unwrap()
        );
        assert_eq!(
            index.arrival_evidence_duplicate(&[&artifact.id]),
            Some(artifact.id.clone())
        );

        log.append(arrival_draft("run_a", "world_x", Some("c1"), "in.one"));
        index.catch_up(&log).unwrap();
        assert!(
            index
                .checkpoint_admitted(&RunId::new("run_a"), &CheckpointId::new("c1"), 0)
                .unwrap()
        );
    }

    #[test]
    fn checkpoint_is_admitted_only_after_its_exact_requirement_set_is_complete() {
        let required = vec!["in.one".to_string(), "in.two".to_string()];
        let required_verified_artifacts = vec![
            RequiredVerifiedArtifact {
                mark: MarkId::new("in.one.ok"),
                artifact_label: "in.one".to_string(),
                verifier: VerifierId::new("in.one.verifier"),
                params: serde_json::json!({}),
            },
            RequiredVerifiedArtifact {
                mark: MarkId::new("in.two.ok"),
                artifact_label: "in.two".to_string(),
                verifier: VerifierId::new("in.two.verifier"),
                params: serde_json::json!({}),
            },
        ];
        let mut first = arrival_draft("run_a", "world_x", Some("c1"), "in.one");
        let first_metadata = InvocationMetadata::new(GrantProjection::default())
            .with_runtime(RuntimeInvocationMetadata {
                run: RunId::new("run_a"),
                ee: EeId::new("ee_test"),
                worker: InterfaceId::new("system_worker_test"),
                checkpoint: CheckpointId::new("c1"),
                interface: InterfaceId::new("world_x"),
                epoch: 0,
                run_record_hash: None,
            })
            .with_checkpoint_admission(CheckpointAdmissionMetadata {
                artifact_label: "in.one".to_string(),
                required_artifact_labels: required.clone(),
                complete: false,
            })
            .with_required_verified_artifacts(required_verified_artifacts.clone());
        first.invocation_metadata = Some(first_metadata.into());

        let mut second = arrival_draft("run_a", "world_x", Some("c1"), "in.two");
        let second_metadata = InvocationMetadata::new(GrantProjection::default())
            .with_runtime(RuntimeInvocationMetadata {
                run: RunId::new("run_a"),
                ee: EeId::new("ee_test"),
                worker: InterfaceId::new("system_worker_test"),
                checkpoint: CheckpointId::new("c1"),
                interface: InterfaceId::new("world_x"),
                epoch: 0,
                run_record_hash: None,
            })
            .with_checkpoint_admission(CheckpointAdmissionMetadata {
                artifact_label: "in.two".to_string(),
                required_artifact_labels: required,
                complete: true,
            })
            .with_required_verified_artifacts(required_verified_artifacts);
        second.invocation_metadata = Some(second_metadata.into());

        let mut log = InMemoryLog::new();
        let first = log.append(first);
        let mut index = ArrivalDedupIndex::new();
        index.catch_up(&log).unwrap();
        assert!(
            !index
                .requirement_admitted(&RunId::new("run_a"), &CheckpointId::new("c1"), 0, "in.one",)
                .unwrap()
        );
        assert!(
            !index
                .checkpoint_admitted(&RunId::new("run_a"), &CheckpointId::new("c1"), 0)
                .unwrap()
        );
        log.append(mark_draft(&first, "in.one.ok", "in.one.verifier"));
        index.catch_up(&log).unwrap();
        assert!(
            index
                .requirement_admitted(&RunId::new("run_a"), &CheckpointId::new("c1"), 0, "in.one",)
                .unwrap()
        );

        let second = log.append(second);
        index.catch_up(&log).unwrap();
        assert!(
            !index
                .requirement_admitted(&RunId::new("run_a"), &CheckpointId::new("c1"), 0, "in.two",)
                .unwrap()
        );
        assert!(
            !index
                .checkpoint_admitted(&RunId::new("run_a"), &CheckpointId::new("c1"), 0)
                .unwrap()
        );
        log.append(mark_draft(&second, "in.two.ok", "in.two.verifier"));
        index.catch_up(&log).unwrap();
        assert!(
            index
                .requirement_admitted(&RunId::new("run_a"), &CheckpointId::new("c1"), 0, "in.two",)
                .unwrap()
        );
        assert!(
            index
                .checkpoint_admitted(&RunId::new("run_a"), &CheckpointId::new("c1"), 0)
                .unwrap()
        );
    }

    #[test]
    fn a_shrunk_log_is_refused_not_papered_over() {
        let mut log = InMemoryLog::new();
        log.append(arrival_draft("run_a", "world_x", Some("c1"), "in.one"));
        let mut index = ArrivalDedupIndex::new();
        index.catch_up(&log).unwrap();

        let empty = InMemoryLog::new();
        assert!(matches!(
            index.catch_up(&empty),
            Err(LedgerIndexError::LogBehindIndex { .. })
        ));
    }
}
