use super::*;
use octopus_ledger::{ContentHash, DeliveryVerdict, RefusedReason};

#[derive(Clone, Debug)]
pub enum RetainedArrivalState {
    Unseen,
    Pending(Movement),
    Finished(DeliveryVerdict),
}

#[derive(Debug, Default)]
pub(super) struct FanoutIndex {
    identities: BTreeMap<(InterfaceId, String), u64>,
    scopes: BTreeMap<u64, Scope>,
    invalid: BTreeSet<u64>,
}

#[derive(Debug)]
struct Scope {
    movement: Movement,
    edges: BTreeMap<CheckpointIdentity, (RuntimeInvocationMetadata, RetainedArrivalState)>,
}

impl FanoutIndex {
    pub(super) fn note_scope(&mut self, movement: &Movement) {
        let Some(MovementMetadata::ArrivalFanout(metadata)) = movement.invocation_metadata.as_ref() else { return; };
        let Endpoint::Interface(interface) = &movement.from;
        let scope = &metadata.arrival_fanout;
        let key = (interface.clone(), scope.identity.clone());
        if let Some(previous) = self.identities.insert(key, movement.seq.0) {
            self.invalid.insert(previous);
            self.invalid.insert(movement.seq.0);
        }
        let mut edges = BTreeMap::new();
        let mut valid = movement.kind == MovementKind::ArtifactMoved
            && movement.from == movement.to && movement.artifact.is_some()
            && !scope.identity.trim().is_empty() && scope.identity.len() <= 256;
        for target in &scope.targets {
            valid &= target.interface == *interface
                && !target.run.as_str().trim().is_empty()
                && !target.checkpoint.as_str().trim().is_empty()
                && !target.ee.as_str().trim().is_empty()
                && !target.worker.as_str().trim().is_empty()
                && target.run_record_hash.as_deref().and_then(ContentHash::from_hex).is_some();
            let identity = CheckpointIdentity::new(&target.run, &target.checkpoint, target.epoch);
            valid &= edges.insert(identity, (target.clone(), RetainedArrivalState::Unseen)).is_none();
        }
        if !valid { self.invalid.insert(movement.seq.0); }
        self.scopes.insert(movement.seq.0, Scope { movement: movement.clone(), edges });
    }

    pub(super) fn note_edge(&mut self, movement: &Movement, runtime: &RuntimeInvocationMetadata, state: &RelayArrivalState) {
        let (scope_seq, refused) = match movement.invocation_metadata.as_ref() {
            Some(MovementMetadata::Invocation(metadata)) => (metadata.delivery_scope_seq, None),
            Some(MovementMetadata::Delivery(metadata)) => (metadata.delivery_scope_seq, Some(&metadata.delivery_verdict)),
            _ => return,
        };
        let Some(scope_seq) = scope_seq else { return; };
        let identity = CheckpointIdentity::new(&runtime.run, &runtime.checkpoint, runtime.epoch);
        let Some(scope) = self.scopes.get_mut(&scope_seq) else {
            self.invalid.insert(scope_seq);
            return;
        };
        let Some((original, current)) = scope.edges.get_mut(&identity) else {
            self.invalid.insert(scope_seq);
            return;
        };
        if scope_seq >= movement.seq.0 || movement.from != scope.movement.from
            || movement.to != Endpoint::Interface(runtime.worker.clone())
            || movement.artifact != scope.movement.artifact
            || original.interface != runtime.interface
            || runtime.run_record_hash.as_deref().and_then(ContentHash::from_hex).is_none()
        {
            self.invalid.insert(scope_seq);
            return;
        }
        if matches!(current, RetainedArrivalState::Finished(_)) { return; }
        if let Some(verdict) = refused {
            if !matches!(verdict, DeliveryVerdict::Refused { .. }) {
                self.invalid.insert(scope_seq);
            } else if !verdict.awaits_current_source() {
                *current = RetainedArrivalState::Finished(verdict.clone());
            }
            return;
        }
        *current = match state {
            RelayArrivalState::Unseen => RetainedArrivalState::Unseen,
            RelayArrivalState::Pending(arrival) => RetainedArrivalState::Pending(arrival.clone()),
            RelayArrivalState::Finished => {
                let failure = movement.invocation_metadata.as_ref().and_then(MovementMetadata::invocation)
                    .and_then(|metadata| metadata.verification_failure.as_ref());
                let verdict = match failure {
                    Some(failure) => DeliveryVerdict::refused(RefusedReason::VerificationFailed,
                        serde_json::to_value(failure).expect("typed verification failure serializes")),
                    None => DeliveryVerdict::Admitted { seq: movement.seq.0 },
                };
                RetainedArrivalState::Finished(verdict)
            }
        };
    }

    pub(super) fn scope(&self, interface: &InterfaceId, identity: &str) -> Result<Option<Movement>, LedgerIndexError> {
        self.identities.get(&(interface.clone(), identity.into())).map(|seq| {
            self.checked_scope(*seq).map(|scope| scope.movement.clone())
        }).transpose()
    }

    pub(super) fn edges(&self, sequence: u64) -> Result<Vec<(RuntimeInvocationMetadata, RetainedArrivalState)>, LedgerIndexError> {
        Ok(self.checked_scope(sequence)?.edges.values().cloned().collect())
    }

    fn checked_scope(&self, sequence: u64) -> Result<&Scope, LedgerIndexError> {
        if self.invalid.contains(&sequence) {
            return Err(LedgerIndexError::InvalidArrivalScope(format!("movement {sequence} has inconsistent identity or edge evidence")));
        }
        self.scopes.get(&sequence).ok_or_else(|| LedgerIndexError::InvalidArrivalScope(format!("movement {sequence} is missing")))
    }
}
