use super::*;
use octopus_ledger::{ArrivalFanoutMetadata, ArrivalFanoutScope, ArtifactDelivery};
use octopus_ledger_index::RetainedArrivalState;

impl InterfaceArtifactArrivalFromInterface<'_> {
    fn retained_scope(&mut self, identity: &str, pinned: &PinnedInterfaceArtifactPayload) -> Result<Movement, ArrivalError> {
        if identity.trim().is_empty() || identity.len() > 256 {
            return Err(ArrivalError::Malformed("retained arrival identity must contain 1..256 bytes".into()));
        }
        let existing = {
            let mut index = self.dedup.lock().map_err(|_| ArrivalError::Malformed("arrival index is poisoned".into()))?;
            index.catch_up(self.log).map_err(|error| ArrivalError::Malformed(error.to_string()))?;
            index.retained_arrival_scope(&self.interface, identity).map_err(|error| ArrivalError::Malformed(error.to_string()))?
        };
        let scope = match existing {
            Some(scope) => scope,
            None => {
                let targets = match self.runtime.resolve_interface_arrival_targets(&self.interface, &pinned.artifact_label) {
                    Ok(targets) => targets,
                    Err(RuntimeError::DescriptorDrift { reason: RefusedReason::NoBinding | RefusedReason::Policy, .. }) => Vec::new(),
                    Err(error) => return Err(error.into()),
                };
                for target in &targets { pin_run_record_snapshot(self.blobs, &target.run, &target.run_record)?; }
                let mut index = self.dedup.lock().map_err(|_| ArrivalError::Malformed("arrival index is poisoned".into()))?;
                index.catch_up(self.log).map_err(|error| ArrivalError::Malformed(error.to_string()))?;
                match index.retained_arrival_scope(&self.interface, identity).map_err(|error| ArrivalError::Malformed(error.to_string()))? {
                    Some(scope) => scope,
                    None => self.log.append(MovementDraft {
                        trace: self.trace.clone(), kind: MovementKind::ArtifactMoved,
                        from: Endpoint::Interface(self.interface.clone()), to: Endpoint::Interface(self.interface.clone()),
                        artifact: Some(pinned.artifact.clone()),
                        invocation_metadata: Some(MovementMetadata::ArrivalFanout(ArrivalFanoutMetadata {
                            arrival_fanout: ArrivalFanoutScope {
                                identity: identity.into(),
                                targets: targets.iter().map(|target| Self::target_runtime_metadata(target, &self.interface)).collect(),
                            },
                        })), causal_floor: None,
                    }),
                }
            }
        };
        if scope.artifact.as_ref() != Some(&pinned.artifact) {
            return Err(ArrivalError::Malformed("retained arrival identity was reused for a different artifact".into()));
        }
        Ok(scope)
    }

    fn retained_edges(&self, scope_seq: u64) -> Result<Vec<(octopus_ledger::RuntimeInvocationMetadata, RetainedArrivalState)>, ArrivalError> {
        let mut index = self.dedup.lock().map_err(|_| ArrivalError::Malformed("arrival index is poisoned".into()))?;
        index.catch_up(self.log).map_err(|error| ArrivalError::Malformed(error.to_string()))?;
        index.retained_arrival_edges(scope_seq).map_err(|error| ArrivalError::Malformed(error.to_string()))
    }

    pub(crate) async fn arrive_retained_artifact(&mut self, artifact: &InterfaceArtifactFile, identity: &str) -> Result<ArtifactDelivery, ArrivalError> {
        let pinned = pin_interface_artifact_payload_from_interface(self.blobs, &self.interface, artifact)?;
        let scope = self.retained_scope(identity, &pinned)?;
        let mut edges = Vec::new();
        for (coordinate, state) in self.retained_edges(scope.seq.0)? {
            let pending = match state {
                RetainedArrivalState::Finished(_) => continue,
                RetainedArrivalState::Pending(movement) => Some(movement),
                RetainedArrivalState::Unseen => None,
            };
            let hash = coordinate.run_record_hash.as_deref().and_then(ContentHash::from_hex)
                .ok_or_else(|| ArrivalError::Malformed("retained arrival has no snapshot identity".into()))?;
            let bytes = self.blobs.get(&hash).ok_or_else(|| ArrivalError::Malformed("retained arrival snapshot is unreadable".into()))?;
            let original = InterfaceArrivalTarget::from_retained_snapshot(&coordinate, octopus_run_record::RunRecordSnapshot {
                hash, bytes: bytes.into(),
            }, scope.seq.0).map_err(ArrivalError::Malformed)?;
            let target = match self.runtime.refresh_retained_interface_arrival_target(&original) {
                Ok(target) => target,
                Err(error) => {
                    self.record_target_refusal(error.into(), &self.interface.clone(), &pinned.artifact, &original, None);
                    continue;
                }
            };
            edges.push(PendingArrivalEdge {
                interface: self.interface.clone(), artifact: pinned.artifact.clone(), artifact_label: pinned.artifact_label.clone(),
                artifact_root: pinned.artifact_root.clone(), target, source: None, pending,
            });
        }
        self.admit_edges_concurrently(edges).await?;
        let mut admitted = None;
        let mut refused = None;
        for (_, state) in self.retained_edges(scope.seq.0)? {
            match state {
                RetainedArrivalState::Finished(verdict @ DeliveryVerdict::Admitted { .. }) => { admitted.get_or_insert(verdict); }
                RetainedArrivalState::Finished(verdict) => { refused.get_or_insert(verdict); }
                _ => return Ok(ArtifactDelivery::Pending { scope_seq: scope.seq.0 }),
            }
        }
        Ok(ArtifactDelivery::Complete { verdict: admitted.or(refused).unwrap_or_else(|| DeliveryVerdict::refused(
            RefusedReason::NoBinding, serde_json::json!({"message":"the original arrival scope contains no active checkpoint", "scope_seq":scope.seq.0}),
        )) })
    }
}
