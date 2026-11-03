use std::path::PathBuf;
use std::sync::Arc;

use octopus_app_server::{
    SharedBlobHandle, SharedBlobs, SpineRootConfig, admit_interface_artifact_arrival,
};
use octopus_descriptors::InterfaceId;
use octopus_ledger::{InMemoryBlobs, InMemoryLog, LiveMovementLog, MovementLog};
use octopus_runtime::RunId;

mod support;

fn registry_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../registry")
}

#[tokio::test]
async fn embedded_spine_drives_a_live_turn_through_the_shared_root() {
    let blobs: SharedBlobs = Arc::new(std::sync::Mutex::new(InMemoryBlobs::new()));
    let interface_artifact_root = std::env::temp_dir().join(format!(
        "octopus-app-server-test-artifacts-{}-{}",
        std::process::id(),
        line!()
    ));

    let embedded = support::build_test_spine(SpineRootConfig {
        registry_root: registry_root(),
        run: RunId::new("run_embedded_tui"),
        blobs: blobs.clone(),
        interface_artifact_root: interface_artifact_root.clone(),
        run_records_root: interface_artifact_root.join("runs"),
        ee_instances_root: interface_artifact_root.join("ee_instances"),
    })
    .await
    .expect("the embedded product spine must boot from the shared composition root");
    support::write_terminal_run_record(
        &embedded.interface_artifact_root,
        &embedded.runtime,
        &embedded.run,
    );

    let mut log = LiveMovementLog::new(InMemoryLog::new());

    let prompt = serde_json::json!({"prompt": "hello from the TUI",
    });

    let mut blob_handle = SharedBlobHandle::new(blobs.clone());
    let submit = support::materialized_interface_arrival_submit_artifact(
        &std::env::temp_dir().join(format!(
            "octopus-app-server-test-arrivals-{}-{}",
            std::process::id(),
            line!()
        )),
        &blobs,
        &prompt,
    );
    let run = embedded.run.clone();
    let outcome = admit_interface_artifact_arrival(
        &embedded,
        &mut log,
        &mut blob_handle,
        &submit,
        &InterfaceId::new("world_operator_chat"),
    )
    .await
    .expect("the embedded chat submit must drive arrival→run through the gate");

    assert_eq!(
        outcome.admission.records.first().map(|record| &record.run),
        Some(&run),
        "the reply is scoped to the embedded run (continuity = the run)"
    );

    assert!(
        outcome.admission.first_outbound_artifact().is_some(),
        "runtime transfer facts must be recorded on the arrival records"
    );
    assert!(
        log.read_from(octopus_ledger::Seq(0)).len() >= 2,
        "the live turn must have appended its arrival + outbound movements to the embedded ledger"
    );
}

#[tokio::test]
async fn embedded_spine_admits_artifact_movement_without_mounted_interface_transport() {
    let blobs: SharedBlobs = Arc::new(std::sync::Mutex::new(InMemoryBlobs::new()));
    let interface_artifact_root = std::env::temp_dir().join(format!(
        "octopus-app-server-test-artifacts-{}-{}",
        std::process::id(),
        line!()
    ));
    let embedded = support::build_test_spine(SpineRootConfig {
        registry_root: registry_root(),
        run: RunId::new("run_embedded_tui_no_interface_transport"),
        blobs: blobs.clone(),
        interface_artifact_root: interface_artifact_root.clone(),
        run_records_root: interface_artifact_root.join("runs"),
        ee_instances_root: interface_artifact_root.join("ee_instances"),
    })
    .await
    .expect("the spine must boot without mounting interface transport code");
    support::write_terminal_run_record(
        &embedded.interface_artifact_root,
        &embedded.runtime,
        &embedded.run,
    );

    let mut log = LiveMovementLog::new(InMemoryLog::new());
    let submit = support::materialized_interface_arrival_submit_artifact(
        &std::env::temp_dir().join(format!(
            "octopus-app-server-test-arrivals-{}-{}",
            std::process::id(),
            line!()
        )),
        &blobs,
        &serde_json::json!({"prompt":"hi"}),
    );
    let mut blob_handle = SharedBlobHandle::new(blobs.clone());
    let outcome = admit_interface_artifact_arrival(
        &embedded,
        &mut log,
        &mut blob_handle,
        &submit,
        &InterfaceId::new("world_operator_chat"),
    )
    .await
    .expect("the act flows through runtime admission as artifact movement");

    assert_eq!(
        outcome.admission.records.first().map(|record| &record.run),
        Some(&embedded.run)
    );
    assert!(
        outcome.admission.first_outbound_artifact().is_some(),
        "runtime admission succeeds or fails on artifact movement and grant, then records outbound transfers"
    );
}

#[tokio::test]
async fn failed_verifier_gate_records_durable_movements_and_no_mark() {
    use octopus_app_server::ArrivalError;
    use octopus_ledger::MovementKind;

    let blobs: SharedBlobs = Arc::new(std::sync::Mutex::new(InMemoryBlobs::new()));
    let interface_artifact_root = std::env::temp_dir().join(format!(
        "octopus-app-server-test-artifacts-{}-{}",
        std::process::id(),
        line!()
    ));
    let embedded = support::build_test_spine(SpineRootConfig {
        registry_root: registry_root(),
        run: RunId::new("run_embedded_failed_verification"),
        blobs: blobs.clone(),
        interface_artifact_root: interface_artifact_root.clone(),
        run_records_root: interface_artifact_root.join("runs"),
        ee_instances_root: interface_artifact_root.join("ee_instances"),
    })
    .await
    .expect("the spine must boot for the failed-verification leg");
    support::write_terminal_run_record_with_required_verified_artifacts(
        &embedded.interface_artifact_root,
        &embedded.runtime,
        &embedded.run,
        serde_json::json!([{
            "mark": "ocr.text.contains",
            "artifact_label": "operator.input",
            "verifier": "ocr_text_contains",
            "params": {"expected_text": "NEVER_ON_ANY_SCREEN_c3f9"}
        }]),
    );

    let mut log = LiveMovementLog::new(InMemoryLog::new());
    let submit = support::materialized_interface_arrival_submit_artifact(
        &std::env::temp_dir().join(format!(
            "octopus-app-server-test-arrivals-{}-{}",
            std::process::id(),
            line!()
        )),
        &blobs,
        &serde_json::json!({"prompt":"hello from the TUI"}),
    );
    let mut blob_handle = SharedBlobHandle::new(blobs.clone());
    let error = admit_interface_artifact_arrival(
        &embedded,
        &mut log,
        &mut blob_handle,
        &submit,
        &InterfaceId::new("world_operator_chat"),
    )
    .await
    .expect_err("a failed verifier gate returns the verification error to the interface");
    assert!(
        matches!(
            error,
            octopus_app_server::InterfaceArtifactArrivalAdmissionError::Arrival(
                ArrivalError::VerificationFailed(_)
            )
        ),
        "the error is the typed verification failure, got: {error}"
    );

    let movements = log.read_from(octopus_ledger::Seq(0));
    let arrival = movements
        .iter()
        .find(|m| m.kind == MovementKind::InterfaceArrival)
        .expect("the artifact reached the checkpoint before its verifier rejected the edge");
    assert!(
        arrival
            .invocation_metadata
            .as_ref()
            .and_then(octopus_ledger::MovementMetadata::invocation)
            .is_some_and(|metadata| metadata.verification_failure.is_some()),
        "the arrival records that it reached but did not satisfy the checkpoint"
    );
    let failed = movements
        .iter()
        .find(|m| m.kind == MovementKind::VerificationFailed)
        .expect("the failed verification is a durable movement, not a silent drop");
    let metadata = failed
        .invocation_metadata
        .as_ref()
        .and_then(octopus_ledger::MovementMetadata::invocation)
        .expect("the failure movement carries the typed invocation envelope");
    let runtime = metadata
        .runtime
        .as_ref()
        .expect("failed edge retains runtime coordinates");
    assert_eq!(&runtime.run, &embedded.run);
    assert_eq!(runtime.checkpoint.as_str(), "ckpt-0");
    assert!(runtime.run_record_hash.is_some());
    assert_eq!(
        arrival.from,
        octopus_ledger::Endpoint::Interface(runtime.interface.clone())
    );
    assert_eq!(
        arrival.to,
        octopus_ledger::Endpoint::Interface(runtime.worker.clone())
    );
    assert_eq!((&failed.from, &failed.to), (&arrival.from, &arrival.to));
    assert!(!metadata.grant.formation_pin.inputs.is_empty());
    let details = metadata
        .verification_failure
        .as_ref()
        .expect("failed edge retains verifier evidence beside its policy envelope");
    assert_eq!(
        details.mark.as_ref().map(|mark| mark.as_str()),
        Some("ocr.text.contains"),
        "the failure names the requirement whose tick did not appear"
    );
    assert!(
        !movements
            .iter()
            .any(|m| matches!(m.kind, MovementKind::MarkOutcomeAdmitted(_))),
        "no mark exists for a failed check — a mark is the success tick only"
    );
}

#[tokio::test]
async fn a_failed_verification_never_poisons_the_pair_for_retry() {
    use octopus_app_server::ArrivalError;
    use octopus_ledger::MovementKind;

    let blobs: SharedBlobs = Arc::new(std::sync::Mutex::new(InMemoryBlobs::new()));
    let interface_artifact_root = std::env::temp_dir().join(format!(
        "octopus-app-server-test-artifacts-{}-{}",
        std::process::id(),
        line!()
    ));
    let embedded = support::build_test_spine(SpineRootConfig {
        registry_root: registry_root(),
        run: RunId::new("run_embedded_failed_then_retry"),
        blobs: blobs.clone(),
        interface_artifact_root: interface_artifact_root.clone(),
        run_records_root: interface_artifact_root.join("runs"),
        ee_instances_root: interface_artifact_root.join("ee_instances"),
    })
    .await
    .expect("the spine must boot for the failed-then-retry leg");
    support::write_terminal_run_record_with_required_verified_artifacts(
        &embedded.interface_artifact_root,
        &embedded.runtime,
        &embedded.run,
        serde_json::json!([{
            "mark": "ocr.text.contains",
            "artifact_label": "operator.input",
            "verifier": "ocr_text_contains",
            "params": {"expected_text": "the retry token 9b1"}
        }]),
    );

    let mut log = LiveMovementLog::new(InMemoryLog::new());
    let mut blob_handle = SharedBlobHandle::new(blobs.clone());

    let failing = support::materialized_interface_arrival_submit_artifact(
        &std::env::temp_dir().join(format!(
            "octopus-app-server-test-arrivals-{}-{}",
            std::process::id(),
            line!()
        )),
        &blobs,
        &serde_json::json!({"prompt":"hello from the TUI"}),
    );
    let error = admit_interface_artifact_arrival(
        &embedded,
        &mut log,
        &mut blob_handle,
        &failing,
        &InterfaceId::new("world_operator_chat"),
    )
    .await
    .expect_err("the first artifact fails its verifier");
    assert!(
        matches!(
            error,
            octopus_app_server::InterfaceArtifactArrivalAdmissionError::Arrival(
                ArrivalError::VerificationFailed(_)
            )
        ),
        "the first hand-in fails as the typed verification failure, got: {error}"
    );

    let passing = support::materialized_interface_arrival_submit_artifact(
        &std::env::temp_dir().join(format!(
            "octopus-app-server-test-arrivals-{}-{}",
            std::process::id(),
            line!()
        )),
        &blobs,
        &serde_json::json!({"prompt":"the retry token 9b1"}),
    );
    admit_interface_artifact_arrival(
        &embedded,
        &mut log,
        &mut blob_handle,
        &passing,
        &InterfaceId::new("world_operator_chat"),
    )
    .await
    .expect(
        "the checkpoint stayed OPEN after the failed verification — a satisfying retry on the \
         same (run, binding) pair must admit",
    );

    let movements = log.read_from(octopus_ledger::Seq(0));
    assert!(
        movements.iter().any(|m| matches!(
            &m.kind,
            MovementKind::MarkOutcomeAdmitted(mark) if mark.as_str() == "ocr.text.contains"
        )),
        "the retry closes the requirement with the mark tick"
    );
}

#[tokio::test]
async fn passing_verifier_gate_records_the_mark_tick() {
    use octopus_ledger::MovementKind;

    let blobs: SharedBlobs = Arc::new(std::sync::Mutex::new(InMemoryBlobs::new()));
    let interface_artifact_root = std::env::temp_dir().join(format!(
        "octopus-app-server-test-artifacts-{}-{}",
        std::process::id(),
        line!()
    ));
    let embedded = support::build_test_spine(SpineRootConfig {
        registry_root: registry_root(),
        run: RunId::new("run_embedded_passing_verification"),
        blobs: blobs.clone(),
        interface_artifact_root: interface_artifact_root.clone(),
        run_records_root: interface_artifact_root.join("runs"),
        ee_instances_root: interface_artifact_root.join("ee_instances"),
    })
    .await
    .expect("the spine must boot for the passing-verification leg");
    support::write_terminal_run_record_with_required_verified_artifacts(
        &embedded.interface_artifact_root,
        &embedded.runtime,
        &embedded.run,
        serde_json::json!([{
            "mark": "ocr.text.contains",
            "artifact_label": "operator.input",
            "verifier": "ocr_text_contains",
            "params": {"expected_text": "hello from the TUI"}
        }]),
    );

    let mut log = LiveMovementLog::new(InMemoryLog::new());
    let submit = support::materialized_interface_arrival_submit_artifact(
        &std::env::temp_dir().join(format!(
            "octopus-app-server-test-arrivals-{}-{}",
            std::process::id(),
            line!()
        )),
        &blobs,
        &serde_json::json!({"prompt":"hello from the TUI"}),
    );
    let mut blob_handle = SharedBlobHandle::new(blobs.clone());
    admit_interface_artifact_arrival(
        &embedded,
        &mut log,
        &mut blob_handle,
        &submit,
        &InterfaceId::new("world_operator_chat"),
    )
    .await
    .expect("an artifact satisfying the requirement admits through the gate");

    let movements = log.read_from(octopus_ledger::Seq(0));
    assert!(
        movements.iter().any(|m| matches!(
            &m.kind,
            MovementKind::MarkOutcomeAdmitted(mark) if mark.as_str() == "ocr.text.contains"
        )),
        "the passing gate records the mark tick in the ledger"
    );
    assert!(
        !movements
            .iter()
            .any(|m| m.kind == MovementKind::VerificationFailed),
        "nothing failed on this edge"
    );
    let arrival = movements
        .iter()
        .find(|movement| movement.kind == MovementKind::InterfaceArrival)
        .expect("the admitted edge records its arrival");
    let mark = movements
        .iter()
        .find(|movement| matches!(movement.kind, MovementKind::MarkOutcomeAdmitted(_)))
        .expect("the admitted edge records its mark");
    let outbound = movements
        .iter()
        .find(|movement| movement.kind == MovementKind::InterfaceOutbound)
        .expect("the completed edge records its outbound");
    let runtime = arrival
        .runtime()
        .expect("arrival carries typed run provenance");
    assert_eq!(
        arrival.from,
        octopus_ledger::Endpoint::Interface(runtime.interface.clone())
    );
    assert_eq!(
        arrival.to,
        octopus_ledger::Endpoint::Interface(runtime.worker.clone())
    );
    assert_eq!((&mark.from, &mark.to), (&arrival.from, &arrival.to));
    assert_eq!(mark.run(), arrival.run());
    assert!(arrival.seq < mark.seq && mark.seq < outbound.seq);
    assert!(arrival.lamport < mark.lamport && mark.lamport < outbound.lamport);

    let resumed = support::build_test_spine(SpineRootConfig {
        registry_root: registry_root(),
        run: embedded.run.clone(),
        blobs: blobs.clone(),
        interface_artifact_root: interface_artifact_root.clone(),
        run_records_root: interface_artifact_root.join("runs"),
        ee_instances_root: interface_artifact_root.join("ee_instances"),
    }).await.unwrap();
    let mut interrupted = LiveMovementLog::new(InMemoryLog::new());
    interrupted.append(octopus_ledger::MovementDraft {
        trace: arrival.trace.clone(), kind: arrival.kind.clone(),
        from: arrival.from.clone(), to: arrival.to.clone(), artifact: arrival.artifact.clone(),
        invocation_metadata: arrival.invocation_metadata.clone(), causal_floor: None,
    });
    resumed.runtime.reconstruct_spent_checkpoints_from_log(&interrupted).unwrap();
    admit_interface_artifact_arrival(&resumed, &mut interrupted, &mut blob_handle,
        &submit, &InterfaceId::new("world_operator_chat")).await.unwrap();
    let recovered = interrupted.read_from(octopus_ledger::Seq(0));
    assert_eq!(recovered.iter().filter(|row| row.kind == MovementKind::InterfaceArrival).count(), 1);
    resumed.runtime.reconstruct_spent_checkpoints_from_movements(&recovered).unwrap();
    assert!(admit_interface_artifact_arrival(&resumed, &mut interrupted, &mut blob_handle,
        &submit, &InterfaceId::new("world_operator_chat")).await.is_err());
}
