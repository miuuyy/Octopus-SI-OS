use std::{fs, path::Path, sync::Arc};

use octopus_ledger::{GrantProjection, RefusedReason, VerifierPackagePin};
use octopus_run_record::GrantEpoch;
use octopus_runtime::*;
use serde_json::json;

const INTERFACES: [&str; 2] = ["world_test", "world_client"];

struct FixtureGrant(CapSet<InterfaceId>);

impl RunGrantResolver for FixtureGrant {
    fn resolve_run_grant(&self, query: RunGrantQuery<'_>) -> Result<RunGrantResolution, String> {
        let grant = form_policy(
            ContextGrant {
                interfaces: CapSet::from_allowed(INTERFACES.map(InterfaceId::new)),
                mark_outcomes: CapSet::from_allowed(["first.ok", "second.ok"].map(MarkId::new)),
                ..Default::default()
            },
            &[PolicyLayer {
                interfaces: Some(self.0.clone()),
                ..Default::default()
            }],
            &[PolicyLayer {
                interfaces: Some(CapSet::from_allowed(
                    query.authored_interfaces.iter().cloned(),
                )),
                ..Default::default()
            }],
            FormationPin::default(),
        );
        let projection = GrantProjection::from_formed(&grant);
        RunGrantResolution::new(grant, projection)
    }
}

struct FixtureVerifier;

impl VerifiedArtifactAdmission for FixtureVerifier {
    fn prepare_required_verified_artifact(
        &self,
        required: &RequiredVerifiedArtifact,
        input: VerificationInput,
    ) -> Result<Box<dyn PreparedVerification>, VerifiedArtifactError> {
        assert_eq!(required.artifact_label, input.artifact_label);
        Ok(Box::new(ReadyVerification::passed(
            Mark::passed_with_class(
                required.mark.clone(),
                input.checkpoint,
                input.run,
                input.epoch,
                MarkClass::Success,
                input.artifact,
                required.verifier.clone(),
                VerifierPackagePin {
                    descriptor_content: ContentHash::of_bytes(b"fixture verifier descriptor"),
                    script_content: ContentHash::of_bytes(b"fixture verifier script"),
                },
                json!({"passed": true}),
            ),
        )))
    }
}

fn runtime(dir: &Path, allowed_interfaces: &[&str]) -> Runtime {
    let catalog = RuntimeInterfaceCatalog::from_classes(INTERFACES.map(|name| {
        (
            InterfaceId::new(name),
            InterfaceClass::new(
                InterfaceType::World,
                InterfaceRuntimeDisposition::ExternalReader,
            )
            .unwrap(),
        )
    }));
    let runtime = Runtime::open(RuntimeServices {
        interface_catalog: Arc::new(catalog),
    })
    .unwrap();
    runtime.set_run_records_dir(dir.to_path_buf());
    runtime.set_run_grant_resolver(Arc::new(FixtureGrant(CapSet::from_allowed(
        allowed_interfaces
            .iter()
            .map(|name| InterfaceId::new(*name)),
    ))));
    runtime.set_verified_artifact_admission(Arc::new(FixtureVerifier));
    runtime
}

fn arrival(label: &str) -> InterfaceArtifactAdmission {
    InterfaceArtifactAdmission {
        run: RunId::new("r1"),
        checkpoint: Some(CheckpointId::new("c1")),
        epoch: GrantEpoch(0),
        interface: InterfaceId::new("world_test"),
        artifact: ArtifactRef {
            id: ArtifactId::new(label),
            content: ContentHash::of_bytes(label.as_bytes()),
        },
        artifact_label: label.into(),
        run_record_hash: None,
    }
}

fn proof_rows(observation: &Observation, label: &str, seq: u64) -> [Movement; 2] {
    let runtime = observation.invocation_metadata.runtime.as_ref().unwrap();
    let mark = &observation.admitted_mark_outcomes[0];
    let arrival = Movement {
        seq: Seq(seq),
        lamport: Lamport(seq + 1),
        recorded_at_unix_ms: seq + 1,
        trace: TraceId("checkpoint-coordinate-replay".into()),
        kind: MovementKind::InterfaceArrival,
        from: Endpoint::Interface(runtime.interface.clone()),
        to: Endpoint::Interface(runtime.worker.clone()),
        artifact: Some(arrival(label).artifact),
        invocation_metadata: Some(observation.invocation_metadata.clone().into()),
    };
    let mark = Movement {
        seq: Seq(seq + 1),
        lamport: Lamport(seq + 2),
        recorded_at_unix_ms: seq + 2,
        kind: MovementKind::MarkOutcomeAdmitted(mark.name.clone()),
        invocation_metadata: Some(mark.clone().into()),
        ..arrival.clone()
    };
    [arrival, mark]
}

#[tokio::test]
async fn record_only_edit_preserves_partial_checkpoint_live_and_on_cold_replay() {
    let dir = std::env::temp_dir().join(format!(
        "octopus-checkpoint-coordinate-replay-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&dir).unwrap();
    let record_path = dir.join("r1.json");
    let mut record = json!({
        "id": "r1",
        "ee": "ee1",
        "worker": "system_worker_flash",
        "trace": "trace:r1",
        "status": "running",
        "interface_state": {},
        "checkpoints": [{
            "id": "c1",
            "run": "r1",
            "trace": "trace:r1:c1",
            "epoch": 0,
            "interface_binding": AuthoredInterfaceBinding::all(InterfaceId::new("world_test")),
            "outbound_interface_bindings": [
                AuthoredInterfaceBinding::all(InterfaceId::new("world_client"))
            ],
            "artifact_bindings": [],
            "required_verified_artifacts": [
                {"mark": "first.ok", "artifact_label": "first", "verifier": "fixture", "params": {}},
                {"mark": "second.ok", "artifact_label": "second", "verifier": "fixture", "params": {}}
            ]
        }],
        "active_checkpoints": ["c1"]
    });
    fs::write(&record_path, serde_json::to_vec(&record).unwrap()).unwrap();
    let live = runtime(&dir, &INTERFACES);
    let first = live
        .admit_bound_interface_checkpoint(arrival("first"))
        .await
        .unwrap();
    assert_eq!(first.admitted_mark_outcomes.len(), 1);
    assert!(
        !first
            .invocation_metadata
            .checkpoint_admission
            .as_ref()
            .unwrap()
            .complete
    );
    assert!(first.outbound_artifact_transfers.is_empty());
    let mut rows = proof_rows(&first, "first", 0).to_vec();
    let partial_replay = runtime(&dir, &INTERFACES);
    partial_replay
        .reconstruct_spent_checkpoints_from_movements(&rows)
        .unwrap();

    record["checkpoints"][0]["required_verified_artifacts"][1]["params"] =
        json!({"expected": "changed contract"});
    fs::write(&record_path, serde_json::to_vec(&record).unwrap()).unwrap();
    for candidate in [&live, &partial_replay] {
        let error = candidate
            .admit_bound_interface_checkpoint(arrival("second"))
            .await
            .unwrap_err();
        assert_eq!(error.refused_reason(), RefusedReason::StaleEpoch);
    }
    record["checkpoints"][0]["required_verified_artifacts"][1]["params"] = json!({});
    record["interface_state"] = json!({"world_test": {"presentation": "changed independently"}});
    fs::write(&record_path, serde_json::to_vec(&record).unwrap()).unwrap();

    let revoked = runtime(&dir, &["world_test"]);
    revoked
        .reconstruct_spent_checkpoints_from_movements(&rows)
        .unwrap();
    let error = revoked
        .admit_bound_interface_checkpoint(arrival("second"))
        .await
        .unwrap_err();
    assert_eq!(error.refused_reason(), RefusedReason::StaleEpoch);

    let second = live
        .admit_bound_interface_checkpoint(arrival("second"))
        .await
        .unwrap();
    assert_eq!(second.admitted_mark_outcomes.len(), 1);
    assert!(
        second
            .invocation_metadata
            .checkpoint_admission
            .as_ref()
            .unwrap()
            .complete
    );
    let first_runtime = first.invocation_metadata.runtime.as_ref().unwrap();
    let second_runtime = second.invocation_metadata.runtime.as_ref().unwrap();
    let first_snapshot = first_runtime.run_record_hash.as_ref().unwrap();
    let second_snapshot = second_runtime.run_record_hash.as_ref().unwrap();
    assert_ne!(first_snapshot, second_snapshot);
    assert_ne!(first_runtime, second_runtime);
    assert_eq!(second.outbound_artifact_transfers.len(), 2);
    for (transfer, (label, snapshot)) in second
        .outbound_artifact_transfers
        .iter()
        .zip([("first", first_snapshot), ("second", second_snapshot)])
    {
        assert_eq!(transfer.artifact.id, ArtifactId::new(label));
        assert_eq!(transfer.to_interface, InterfaceId::new("world_client"));
        let metadata = transfer.invocation_metadata.runtime.as_ref().unwrap();
        assert_eq!(metadata.run_record_hash.as_ref(), Some(snapshot));
    }

    rows.extend(proof_rows(&second, "second", 2));
    let history_path = dir.join("checkpoint-movements.json");
    fs::write(&history_path, serde_json::to_vec(&rows).unwrap()).unwrap();
    drop((live, partial_replay, revoked, first, second, rows));
    let restored_rows: Vec<Movement> =
        serde_json::from_slice(&fs::read(&history_path).unwrap()).unwrap();
    let cold = runtime(&dir, &INTERFACES);
    cold.reconstruct_spent_checkpoints_from_movements(&restored_rows)
        .unwrap();
    for label in ["first", "second"] {
        let error = cold
            .admit_bound_interface_checkpoint(arrival(label))
            .await
            .unwrap_err();
        assert_eq!(error.refused_reason(), RefusedReason::CheckpointSpent);
    }
    fs::remove_dir_all(dir).unwrap();
}
