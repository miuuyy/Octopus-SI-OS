use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use octopus_app_server::{
    SharedBlobHandle, SharedBlobs, SpineRootConfig, admit_interface_artifact_arrival,
    admit_system_interface_reference_emission,
};
use octopus_descriptors::{CheckpointId, EeId, InterfaceId, RunId};
use octopus_interfaces::{GrantProjection, InvocationMetadata, RuntimeInvocationMetadata};
use octopus_ledger::{
    Endpoint, InMemoryBlobs, InMemoryLog, LiveMovementLog, MovementDraft, MovementKind,
    MovementLog, Seq, TraceId,
};
use octopus_runtime::Runtime;

mod support;

fn registry_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../registry")
}

const WORKER_INTERFACE: &str = "system_worker_planner";
const UNRELATED_INTERFACE: &str = "system_worker_other";

fn write_run_record(root: &Path, runtime: &Runtime, record: &serde_json::Value) {
    let runs_dir = root.join("runs");
    runtime.set_run_records_dir(runs_dir.clone());
    std::fs::create_dir_all(&runs_dir).expect("runs dir");
    let run = record
        .get("id")
        .and_then(serde_json::Value::as_str)
        .expect("run record fixture declares an id");
    let bytes = serde_json::to_vec_pretty(record).expect("run record fixture encodes");
    std::fs::write(runs_dir.join(format!("{run}.json")), bytes).expect("run record fixture writes");
}

fn build_success_payload() -> serde_json::Value {
    serde_json::json!({
        "label": "coder.build.result",
        "tool": "coder.build",
        "ok": true,
        "body": {
            "result": "command_finished",
            "output": {"exit_code": 0, "stdout": [], "stderr": []}
        }
    })
}

fn captured_boundary_artifact(
    root: impl AsRef<Path>,
    label: &str,
    payload: &[u8],
) -> octopus_app_server::InterfaceArtifactFile {
    octopus_app_server::InterfaceArtifactFile::write(root, label, payload)
        .and_then(|artifact| artifact.capture_bounded(1024 * 1024, 64 * 1024 * 1024))
        .expect("boundary artifact captures before runtime hand-in")
}

#[tokio::test]
async fn worker_interface_delivers_executor_artifact_cross_run_to_the_planner_thread() {
    let blobs: SharedBlobs = Arc::new(std::sync::Mutex::new(InMemoryBlobs::new()));
    let artifact_root = std::env::temp_dir().join(format!(
        "octopus-worker-interface-cross-run-{}-{}",
        std::process::id(),
        line!()
    ));
    let embedded = support::build_test_spine(SpineRootConfig {
        registry_root: registry_root(),
        run: RunId::new("run_executor_b"),
        blobs: blobs.clone(),
        interface_artifact_root: artifact_root.clone(),
        run_records_root: artifact_root.join("runs"),
        ee_instances_root: artifact_root.join("ee_instances"),
    })
    .await
    .expect("the embedded product spine boots for the worker-interface cross-run seam");

    write_run_record(
        &embedded.interface_artifact_root,
        &embedded.runtime,
        &serde_json::json!({
            "id": "run_executor_b",
            "ee": "ee_existence",
            "worker": "system_worker_coder_max",
            "trace": "trace:run_executor_b",
            "status": "running",
            "interface_state": {},
            "checkpoints": [{
                "id": "cB",
                "run": "run_executor_b",
                "trace": "trace:run_executor_b:cB",
                "epoch": 0,
                "interface_binding": support::all_binding("system_context"),
                "outbound_interface_bindings": [support::all_binding(WORKER_INTERFACE)],
                "artifact_bindings": support::worker_role_artifact_bindings(&[
                    "system_worker_coder_max",
                    WORKER_INTERFACE,
                ]),
            }],
            "active_checkpoints": ["cB"],
        }),
    );

    // Planner run A: a watching checkpoint on the SAME worker-interface folder in a
    // different run, carrying the required verified artifact whose verifier is the wake.
    write_run_record(
        &embedded.interface_artifact_root,
        &embedded.runtime,
        &serde_json::json!({
            "id": "run_planner_a",
            "ee": "ee_existence",
            "worker": "system_worker_coder_max",
            "trace": "trace:run_planner_a",
            "status": "running",
            "interface_state": {},
            "checkpoints": [{
                "id": "cA",
                "run": "run_planner_a",
                "trace": "trace:run_planner_a:cA",
                "epoch": 0,
                "interface_binding": support::all_binding(WORKER_INTERFACE),
                "outbound_interface_bindings": [],
                "artifact_bindings": support::worker_role_artifact_bindings(&[
                    "system_worker_coder_max",
                    WORKER_INTERFACE,
                ]),
                "required_verified_artifacts": [{
                    "mark": "coder.build.ok",
                    "artifact_label": "coder.build.result",
                    "verifier": "build_success",
                    "params": {}
                }],
            }],
            "active_checkpoints": ["cA"],
        }),
    );

    write_run_record(
        &embedded.interface_artifact_root,
        &embedded.runtime,
        &serde_json::json!({
            "id": "run_unrelated_c",
            "ee": "ee_existence",
            "worker": "system_worker_coder",
            "trace": "trace:run_unrelated_c",
            "status": "running",
            "interface_state": {},
            "checkpoints": [{
                "id": "cC",
                "run": "run_unrelated_c",
                "trace": "trace:run_unrelated_c:cC",
                "epoch": 0,
                "interface_binding": support::all_binding(UNRELATED_INTERFACE),
                "outbound_interface_bindings": [],
                "artifact_bindings": support::worker_role_artifact_bindings(&[
                    "system_worker_coder",
                ]),
                "required_verified_artifacts": [{
                    "mark": "coder.build.ok",
                    "artifact_label": "coder.build.result",
                    "verifier": "build_success",
                    "params": {}
                }],
            }],
            "active_checkpoints": ["cC"],
        }),
    );

    let planner_path = artifact_root.join("runs/run_planner_a.json");
    let mut planner: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&planner_path).unwrap()).unwrap();
    planner["checkpoints"][0]["outbound_interface_bindings"] =
        serde_json::json!([support::all_binding("world_terminal")]);
    let mut sibling = planner["checkpoints"][0].clone();
    sibling["id"] = serde_json::json!("cA2");
    sibling["trace"] = serde_json::json!("trace:run_planner_a:cA2");
    planner["checkpoints"].as_array_mut().unwrap().push(sibling);
    planner["active_checkpoints"] = serde_json::json!(["cA", "cA2"]);
    write_run_record(&artifact_root, &embedded.runtime, &planner);

    let mut log = LiveMovementLog::new(InMemoryLog::new());

    let arrivals_root = artifact_root.join("executor-arrival");
    let payload = serde_json::to_vec(&build_success_payload()).expect("payload encodes");
    let submit = captured_boundary_artifact(&arrivals_root, "coder.build.result", &payload);

    let mut blob_handle = SharedBlobHandle::new(blobs.clone());
    let outcome = admit_interface_artifact_arrival(
        &embedded,
        &mut log,
        &mut blob_handle,
        &submit,
        &InterfaceId::new("system_context"),
    )
    .await
    .expect("the executor closes its checkpoint and hands its result to the worker interface");

    let runs: Vec<String> = outcome
        .admission
        .records
        .iter()
        .map(|record| record.run.as_str().to_string())
        .collect();
    assert!(
        runs.contains(&"run_executor_b".to_string()),
        "the arrival first admits the executor's own checkpoint: {runs:?}"
    );
    assert!(
        runs.contains(&"run_planner_a".to_string()),
        "the executor's outbound artifact reaches the planner run cross-run: {runs:?}"
    );
    assert!(
        !runs.contains(&"run_unrelated_c".to_string()),
        "a different interface folder never receives the artifact: {runs:?}"
    );

    let movements = log.read_from(Seq(0));

    let outbound_from_b = movements.iter().any(|m| {
        matches!(m.kind, MovementKind::InterfaceOutbound)
            && matches!(&m.from, octopus_ledger::Endpoint::Interface(interface) if interface.as_str() == "system_context")
            && matches!(&m.to, octopus_ledger::Endpoint::Interface(interface) if interface.as_str() == WORKER_INTERFACE)
    });
    assert!(
        outbound_from_b,
        "an InterfaceOutbound movement carries run B provenance from the arriving interface toward the worker-interface folder"
    );

    let arrival_on_a = movements.iter().any(|m| {
        matches!(m.kind, MovementKind::InterfaceArrival)
            && m.run().is_some_and(|run| run.as_str() == "run_planner_a")
            && m.to == Endpoint::Interface(InterfaceId::new("system_worker_coder_max"))
            && matches!(&m.from, octopus_ledger::Endpoint::Interface(interface) if interface.as_str() == WORKER_INTERFACE)
    });
    assert!(
        arrival_on_a,
        "the cross-run arrival lands on run A along the worker-interface folder"
    );

    let mark_on_a = movements.iter().any(|m| {
        matches!(&m.kind, MovementKind::MarkOutcomeAdmitted(_))
            && m.run().is_some_and(|run| run.as_str() == "run_planner_a")
            && m.from == Endpoint::Interface(InterfaceId::new(WORKER_INTERFACE))
            && m.to == Endpoint::Interface(InterfaceId::new("system_worker_coder_max"))
    });
    assert!(
        mark_on_a,
        "the verifier on run A's watching edge admits the mark — the planner wakes"
    );

    let arrival_on_c = movements.iter().any(|m| {
        matches!(
            m.kind,
            MovementKind::InterfaceArrival | MovementKind::MarkOutcomeAdmitted(_)
        ) && m.run().is_some_and(|run| run.as_str() == "run_unrelated_c")
    });
    assert!(
        !arrival_on_c,
        "the artifact never crosses into the unrelated interface folder"
    );

    let arrivals_on_b = movements
        .iter()
        .filter(|m| {
            matches!(m.kind, MovementKind::InterfaceArrival)
                && m.run().is_some_and(|run| run.as_str() == "run_executor_b")
        })
        .count();
    assert_eq!(
        arrivals_on_b, 1,
        "the self-loop guard admits only the executor's original arrival — the artifact it hands out never returns to its own emitting checkpoint"
    );
    let original_rows = log.read_from(Seq(0));
    let source = original_rows
        .iter()
        .find(|movement| {
            movement.kind == MovementKind::InterfaceOutbound
                && movement.to == Endpoint::Interface(InterfaceId::new(WORKER_INTERFACE))
        })
        .unwrap()
        .clone();
    let receiver_arrivals: Vec<_> = original_rows
        .iter()
        .filter(|movement| {
            movement.kind == MovementKind::InterfaceArrival
                && movement
                    .invocation_metadata
                    .as_ref()
                    .and_then(|metadata| metadata.invocation())
                    .is_some_and(|metadata| metadata.source_movement_seq == Some(source.seq.0))
        })
        .collect();
    assert_eq!(receiver_arrivals.len(), 2);
    let cuts = [
        source.seq.0 + 1,
        receiver_arrivals[0].seq.0 + 1,
        receiver_arrivals[0].seq.0 + 2,
        receiver_arrivals[1].seq.0,
        original_rows.len() as u64,
    ];
    for cut in cuts {
        let mut replay = LiveMovementLog::new(InMemoryLog::new());
        for movement in original_rows.iter().take(cut as usize) {
            replay.append(MovementDraft {
                trace: movement.trace.clone(),
                kind: movement.kind.clone(),
                from: movement.from.clone(),
                to: movement.to.clone(),
                artifact: movement.artifact.clone(),
                invocation_metadata: movement.invocation_metadata.clone(),
                causal_floor: Some(octopus_ledger::Lamport(movement.lamport.0 - 1)),
            });
        }
        let resumed = support::build_test_spine(SpineRootConfig {
            registry_root: registry_root(),
            run: RunId::new("run_executor_b"),
            blobs: blobs.clone(),
            interface_artifact_root: artifact_root.clone(),
            run_records_root: artifact_root.join("runs"),
            ee_instances_root: artifact_root.join("ee_instances"),
        })
        .await
        .unwrap();
        let mut durable = replay.read_from(Seq(0));
        resumed
            .runtime
            .reconstruct_spent_checkpoints_from_movements(&durable)
            .unwrap();
        let recovered_tail =
            octopus_app_server::recover_durable_checkpoint_outbound_tail(&mut replay, &durable)
                .unwrap();
        durable.extend(recovered_tail);
        octopus_app_server::arrival::recover_runtime_relay_arrivals(
            &resumed.runtime,
            &mut replay,
            &mut blob_handle,
            resumed.arrival_dedup.clone(),
            &durable,
        )
        .await
        .unwrap();
        let recovered_rows = replay.read_from(Seq(0));
        let mut admitted_edges: Vec<_> = recovered_rows
            .iter()
            .filter_map(|movement| {
                if movement.kind != MovementKind::InterfaceArrival {
                    return None;
                }
                let metadata = movement.invocation_metadata.as_ref()?.invocation()?;
                let runtime = metadata.runtime.as_ref()?;
                (runtime.run.as_str() == "run_planner_a")
                    .then_some((runtime.checkpoint.as_str(), metadata.source_movement_seq))
            })
            .collect();
        admitted_edges.sort_unstable();
        assert_eq!(
            admitted_edges,
            [("cA", Some(source.seq.0)), ("cA2", Some(source.seq.0))],
            "crash cut {cut}"
        );
        assert_eq!(
            recovered_rows
                .iter()
                .filter(|movement| matches!(movement.kind, MovementKind::MarkOutcomeAdmitted(_)))
                .count(),
            2,
            "cut {cut}; verifier refusals: {:?}",
            recovered_rows.iter().filter_map(|row| row.invocation_metadata.as_ref()?.invocation()?.verification_failure.as_ref()).collect::<Vec<_>>()
        );
        assert_eq!(
            recovered_rows
                .iter()
                .filter(|movement| movement.kind == MovementKind::InterfaceOutbound
                    && movement.to == Endpoint::Interface(InterfaceId::new("world_terminal")))
                .count(),
            2,
            "cut {cut}"
        );
        octopus_app_server::arrival::recover_runtime_relay_arrivals(
            &resumed.runtime,
            &mut replay,
            &mut blob_handle,
            resumed.arrival_dedup.clone(),
            &recovered_rows,
        )
        .await
        .unwrap();
        assert_eq!(
            replay.len(),
            recovered_rows.len(),
            "recovery is idempotent at cut {cut}"
        );
        let mut handin = octopus_app_server::InterfaceArtifactArrivalFromInterface::new(
            &resumed.runtime,
            &mut replay,
            &mut blob_handle,
            resumed.arrival_dedup.clone(),
            source.trace.clone(),
            InterfaceId::new("system_context"),
        );
        let retried = handin
            .emit_artifact_reference_to_interface(
                source.artifact.as_ref().unwrap(),
                &RunId::new("run_executor_b"),
                &CheckpointId::new("cB"),
                octopus_run_record::GrantEpoch(0),
                source.lamport,
                &InterfaceId::new(WORKER_INTERFACE),
            )
            .await
            .unwrap();
        assert_eq!(retried.prior_admitted_seq, Some(source.seq.0));
        assert!(retried.delivered.is_empty());
        assert_eq!(replay.len(), recovered_rows.len());

        planner["checkpoints"][0]["epoch"] = serde_json::json!(1);
        planner["checkpoints"][1]["epoch"] = serde_json::json!(1);
        write_run_record(&artifact_root, &resumed.runtime, &planner);
        octopus_app_server::arrival::recover_runtime_relay_arrivals(
            &resumed.runtime,
            &mut replay,
            &mut blob_handle,
            resumed.arrival_dedup.clone(),
            &recovered_rows,
        )
        .await
        .unwrap();
        assert_eq!(
            replay.len(),
            recovered_rows.len(),
            "consumed source must not enter a re-armed frontier"
        );
        planner["checkpoints"][0]["epoch"] = serde_json::json!(0);
        planner["checkpoints"][1]["epoch"] = serde_json::json!(0);
        write_run_record(&artifact_root, &resumed.runtime, &planner);
    }
    planner["checkpoints"][0]["epoch"] = serde_json::json!(1);
    planner["checkpoints"][1]["epoch"] = serde_json::json!(1);
    write_run_record(&artifact_root, &embedded.runtime, &planner);
    let emitter_path = artifact_root.join("runs/run_executor_b.json");
    let mut emitter: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&emitter_path).unwrap()).unwrap();
    emitter["checkpoints"][0]["epoch"] = serde_json::json!(1);
    write_run_record(&artifact_root, &embedded.runtime, &emitter);
    embedded.reload_registry_snapshot_on_artifact_arrival().unwrap();
    let fresh = octopus_app_server::InterfaceArtifactArrivalFromInterface::new(
        &embedded.runtime,
        &mut log,
        &mut blob_handle,
        embedded.arrival_dedup.clone(),
        source.trace.clone(),
        InterfaceId::new("system_context"),
    )
    .emit_artifact_reference_to_interface(
        source.artifact.as_ref().unwrap(),
        &RunId::new("run_executor_b"),
        &CheckpointId::new("cB"),
        octopus_run_record::GrantEpoch(1),
        source.lamport,
        &InterfaceId::new(WORKER_INTERFACE),
    )
    .await
    .unwrap();
    assert!(fresh.outbound_movement.is_some());
    assert_eq!(
        fresh.delivered.len(),
        2,
        "a fresh emission must admit re-armed receiver edges: {:?}",
        log.read_from(Seq(0)).into_iter().filter(|row| row.kind == MovementKind::DeliveryRefused).collect::<Vec<_>>()
    );
    assert_ne!(fresh.admitted_seq(), source.seq.0);
}

#[tokio::test]
async fn interface_addressed_reference_emission_preserves_artifact_identity() {
    let blobs: SharedBlobs = Arc::new(std::sync::Mutex::new(InMemoryBlobs::new()));
    let artifact_root = std::env::temp_dir().join(format!(
        "octopus-reference-emission-{}-{}",
        std::process::id(),
        line!()
    ));
    let embedded = support::build_test_spine(SpineRootConfig {
        registry_root: registry_root(),
        run: RunId::new("run_reference_emitter"),
        blobs: blobs.clone(),
        interface_artifact_root: artifact_root.clone(),
        run_records_root: artifact_root.join("runs"),
        ee_instances_root: artifact_root.join("ee_instances"),
    })
    .await
    .expect("the embedded spine boots for reference emission");

    let payload = br#"{"fact":"same artifact"}"#;
    let source = octopus_app_server::InterfaceArtifactFile::write(
        artifact_root.join("source"),
        "memory.fact",
        payload,
    )
    .expect("source artifact materializes");
    let original = {
        let mut blobs = blobs.lock().expect("blob store lock");
        source
            .load_into_blob(&mut *blobs)
            .expect("source artifact pins")
            .artifact
    };

    write_run_record(
        &embedded.interface_artifact_root,
        &embedded.runtime,
        &serde_json::json!({
            "id": "run_reference_emitter",
            "ee": "ee_existence",
            "worker": "system_worker_coder_max",
            "trace": "trace:run_reference_emitter",
            "status": "running",
            "interface_state": {},
            "checkpoints": [{
                "id": "c_emit",
                "run": "run_reference_emitter",
                "trace": "trace:run_reference_emitter:c_emit",
                "epoch": 0,
                "interface_binding": support::all_binding("system_context"),
                "outbound_interface_bindings": [support::all_binding(WORKER_INTERFACE)],
                "artifact_bindings": support::worker_role_artifact_bindings(&[
                    "system_worker_coder_max",
                    WORKER_INTERFACE,
                ])
                .into_iter()
                .chain([serde_json::json!({"id": original.id})])
                .collect::<Vec<_>>(),
                "required_verified_artifacts": []
            }],
            "active_checkpoints": ["c_emit"]
        }),
    );

    let mut log = LiveMovementLog::new(InMemoryLog::new());
    let source_metadata = InvocationMetadata::new(GrantProjection::default()).with_runtime(
        RuntimeInvocationMetadata {
            run: RunId::new("run_reference_emitter"),
            ee: EeId::new("ee_existence"),
            worker: InterfaceId::new("system_worker_coder_max"),
            checkpoint: CheckpointId::new("c_emit"),
            interface: InterfaceId::new("system_context"),
            epoch: 0,
            run_record_hash: None,
        },
    );
    let source_movement = log.append(MovementDraft {
        trace: TraceId("reference-emission".into()),
        kind: MovementKind::InterfaceOutbound,
        from: Endpoint::Interface(InterfaceId::new(WORKER_INTERFACE)),
        to: Endpoint::Interface(InterfaceId::new("system_context")),
        artifact: Some(original.clone()),
        invocation_metadata: Some(source_metadata.clone().into()),
        causal_floor: None,
    });
    let mut blob_handle = SharedBlobHandle::new(blobs);
    let outcome = admit_system_interface_reference_emission(
        &embedded,
        &mut log,
        &mut blob_handle,
        &original,
        source_movement.seq.0,
        &InterfaceId::new("system_context"),
        &InterfaceId::new(WORKER_INTERFACE),
    )
    .await
    .expect("bound artifact reference emits");

    assert_eq!(
        outcome
            .outbound_movement
            .as_ref()
            .and_then(|movement| movement.artifact.as_ref()),
        Some(&original)
    );
    let movements = log.read_from(Seq(0));
    assert!(movements.iter().any(|movement| {
        movement.kind == MovementKind::InterfaceOutbound
            && movement.from == Endpoint::Interface(InterfaceId::new("system_context"))
            && movement.to == Endpoint::Interface(InterfaceId::new(WORKER_INTERFACE))
            && movement.artifact.as_ref() == Some(&original)
    }));

    let mismatched_source = log.append(MovementDraft {
        trace: TraceId("mismatched-reference-emission".into()),
        kind: MovementKind::InterfaceOutbound,
        from: Endpoint::Interface(InterfaceId::new(WORKER_INTERFACE)),
        to: Endpoint::Interface(InterfaceId::new("world_terminal")),
        artifact: Some(original.clone()),
        invocation_metadata: Some(source_metadata.into()),
        causal_floor: None,
    });
    let mismatched_refusal = admit_system_interface_reference_emission(
        &embedded,
        &mut log,
        &mut blob_handle,
        &original,
        mismatched_source.seq.0,
        &InterfaceId::new("system_context"),
        &InterfaceId::new(WORKER_INTERFACE),
    )
    .await
    .expect_err("another interface's movement cannot borrow exact interface authority");
    assert!(mismatched_refusal.to_string().contains(
        "targets interface 'world_terminal', not host-attested interface 'system_context'"
    ));

    let refused = admit_system_interface_reference_emission(
        &embedded,
        &mut log,
        &mut blob_handle,
        &original,
        9_999,
        &InterfaceId::new("system_context"),
        &InterfaceId::new(WORKER_INTERFACE),
    )
    .await
    .expect_err("a child cannot invent source movement coordinates");
    assert!(refused.to_string().contains("does not exist"));
    assert!(log.read_from(Seq(0)).iter().any(|movement| {
        movement.kind == MovementKind::DeliveryRefused
            && movement.from == Endpoint::Interface(InterfaceId::new("system_context"))
            && movement.to == Endpoint::Interface(InterfaceId::new(WORKER_INTERFACE))
            && movement.artifact.as_ref() == Some(&original)
    }));

    let run_path = artifact_root.join("runs/run_reference_emitter.json");
    let mut rearmed: serde_json::Value = serde_json::from_slice(&std::fs::read(run_path).unwrap()).unwrap();
    rearmed["checkpoints"][0]["epoch"] = serde_json::json!(1);
    write_run_record(&artifact_root, &embedded.runtime, &rearmed);
    let repeated = admit_system_interface_reference_emission(
        &embedded, &mut log, &mut blob_handle, &original, source_movement.seq.0,
        &InterfaceId::new("system_context"), &InterfaceId::new(WORKER_INTERFACE),
    ).await.unwrap();
    assert_eq!(repeated.prior_admitted_seq, Some(outcome.admitted_seq()));
    let unaccepted = captured_boundary_artifact(artifact_root.join("unaccepted"), "memory.fact", br#"{"fact":"new"}"#);
    let refused = octopus_app_server::admit_system_interface_emission(
        &embedded, &mut log, &mut blob_handle, &unaccepted, source_movement.seq.0,
        &InterfaceId::new("system_context"), &InterfaceId::new(WORKER_INTERFACE),
    ).await.unwrap_err();
    assert!(refused.to_string().contains("emission belongs to epoch 0"));
}

/// Per-edge verdicts (notes2 2026-07-08, VERIFIERS.md §5b): every edge of the fan-out gets
/// ITS OWN outcome. A watcher whose verifier rejects the artifact records its failure on
/// that route only — the sibling watcher still admits, and the hand-in stays admitted.
#[tokio::test]
async fn a_failing_edge_records_its_own_verdict_and_never_poisons_the_sibling_edges() {
    let blobs: SharedBlobs = Arc::new(std::sync::Mutex::new(InMemoryBlobs::new()));
    let artifact_root = std::env::temp_dir().join(format!(
        "octopus-per-edge-verdicts-{}-{}",
        std::process::id(),
        line!()
    ));
    let embedded = support::build_test_spine(SpineRootConfig {
        registry_root: registry_root(),
        run: RunId::new("run_executor_edges"),
        blobs: blobs.clone(),
        interface_artifact_root: artifact_root.clone(),
        run_records_root: artifact_root.join("runs"),
        ee_instances_root: artifact_root.join("ee_instances"),
    })
    .await
    .expect("the embedded product spine boots for the per-edge verdict seam");

    write_run_record(
        &embedded.interface_artifact_root,
        &embedded.runtime,
        &serde_json::json!({
            "id": "run_executor_edges",
            "ee": "ee_existence",
            "worker": "system_worker_coder_max",
            "trace": "trace:run_executor_edges",
            "status": "running",
            "interface_state": {},
            "checkpoints": [{
                "id": "cE",
                "run": "run_executor_edges",
                "trace": "trace:run_executor_edges:cE",
                "epoch": 0,
                "interface_binding": support::all_binding("system_context"),
                "outbound_interface_bindings": [support::all_binding(WORKER_INTERFACE)],
                "artifact_bindings": support::worker_role_artifact_bindings(&[
                    "system_worker_coder_max",
                    WORKER_INTERFACE,
                ]),
            }],
            "active_checkpoints": ["cE"],
        }),
    );

    // Watcher P: the blank tick — the artifact arrived and is readable — passes.
    write_run_record(
        &embedded.interface_artifact_root,
        &embedded.runtime,
        &serde_json::json!({
            "id": "run_watcher_pass",
            "ee": "ee_existence",
            "worker": "system_worker_coder_max",
            "trace": "trace:run_watcher_pass",
            "status": "running",
            "interface_state": {},
            "checkpoints": [{
                "id": "cP",
                "run": "run_watcher_pass",
                "trace": "trace:run_watcher_pass:cP",
                "epoch": 0,
                "interface_binding": support::all_binding(WORKER_INTERFACE),
                "outbound_interface_bindings": [],
                "artifact_bindings": support::worker_role_artifact_bindings(&[
                    "system_worker_coder_max",
                    WORKER_INTERFACE,
                ]),
                "required_verified_artifacts": [{
                    "mark": "coder.build.ok",
                    "artifact_label": "coder.build.result",
                    "verifier": "artifact_exists",
                    "params": {}
                }],
            }],
            "active_checkpoints": ["cP"],
        }),
    );

    // Watcher F: demands a PASSING build — the exit_code=1 evidence must fail its edge.
    write_run_record(
        &embedded.interface_artifact_root,
        &embedded.runtime,
        &serde_json::json!({
            "id": "run_watcher_fail",
            "ee": "ee_existence",
            "worker": "system_worker_coder_max",
            "trace": "trace:run_watcher_fail",
            "status": "running",
            "interface_state": {},
            "checkpoints": [{
                "id": "cF",
                "run": "run_watcher_fail",
                "trace": "trace:run_watcher_fail:cF",
                "epoch": 0,
                "interface_binding": support::all_binding(WORKER_INTERFACE),
                "outbound_interface_bindings": [],
                "artifact_bindings": support::worker_role_artifact_bindings(&[
                    "system_worker_coder_max",
                    WORKER_INTERFACE,
                ]),
                "required_verified_artifacts": [{
                    "mark": "coder.build.ok",
                    "artifact_label": "coder.build.result",
                    "verifier": "build_success",
                    "params": {}
                }],
            }, {
                "id": "cMissingRoles", "run": "run_watcher_fail",
                "trace": "trace:run_watcher_fail:cMissingRoles", "epoch": 0,
                "interface_binding": support::all_binding(WORKER_INTERFACE),
                "outbound_interface_bindings": [], "artifact_bindings": [],
            }],
            "active_checkpoints": ["cF", "cMissingRoles"],
        }),
    );

    let mut log = LiveMovementLog::new(InMemoryLog::new());
    let payload = serde_json::to_vec(&serde_json::json!({
        "label": "coder.build.result",
        "tool": "coder.build",
        "ok": true,
        "body": {
            "result": "command_finished",
            "output": {"exit_code": 1, "stdout": [], "stderr": []}
        }
    }))
    .expect("payload encodes");
    let submit = captured_boundary_artifact(
        &artifact_root.join("executor-arrival"),
        "coder.build.result",
        &payload,
    );

    let mut blob_handle = SharedBlobHandle::new(blobs.clone());
    let outcome = admit_interface_artifact_arrival(
        &embedded,
        &mut log,
        &mut blob_handle,
        &submit,
        &InterfaceId::new("system_context"),
    )
    .await
    .expect("one refused edge never turns the whole hand-in into a refusal");

    let runs: Vec<String> = outcome
        .admission
        .records
        .iter()
        .map(|record| record.run.as_str().to_string())
        .collect();
    assert!(runs.contains(&"run_executor_edges".to_string()));
    assert!(
        runs.contains(&"run_watcher_pass".to_string()),
        "the passing watcher's edge admits regardless of the sibling: {runs:?}"
    );
    assert!(
        !runs.contains(&"run_watcher_fail".to_string()),
        "the failing watcher's edge is not an admitted record: {runs:?}"
    );

    let movements = log.read_from(Seq(0));
    let formation_refusals: Vec<_> = movements
        .iter()
        .filter_map(|movement| {
            let octopus_ledger::MovementMetadata::Delivery(metadata) =
                movement.invocation_metadata.as_ref()?
            else {
                return None;
            };
            let runtime = metadata.runtime.as_ref()?;
            if runtime.checkpoint.as_str() != "cMissingRoles" {
                return None;
            }
            assert_eq!(runtime.run.as_str(), "run_watcher_fail");
            assert_eq!(runtime.interface.as_str(), WORKER_INTERFACE);
            assert!(runtime.run_record_hash.is_some());
            assert!(metadata.source_movement_seq.is_some());
            let octopus_interfaces::DeliveryVerdict::Refused { reason, detail } =
                &metadata.delivery_verdict
            else {
                panic!("formation refuses");
            };
            assert_eq!(*reason, octopus_interfaces::RefusedReason::Policy);
            assert!(detail.to_string().contains("role"));
            Some(movement)
        })
        .collect();
    assert_eq!(formation_refusals.len(), 1);
    let failed_edge_movement = movements.iter().any(|m| {
        matches!(m.kind, MovementKind::VerificationFailed)
            && m.run()
                .is_some_and(|run| run.as_str() == "run_watcher_fail")
            && m.from == Endpoint::Interface(InterfaceId::new(WORKER_INTERFACE))
            && m.to == Endpoint::Interface(InterfaceId::new("system_worker_coder_max"))
    });
    assert!(
        failed_edge_movement,
        "the refused edge carries its own durable VerificationFailed movement"
    );
    let pass_mark = movements.iter().any(|m| {
        matches!(&m.kind, MovementKind::MarkOutcomeAdmitted(_))
            && m.run()
                .is_some_and(|run| run.as_str() == "run_watcher_pass")
            && m.from == Endpoint::Interface(InterfaceId::new(WORKER_INTERFACE))
            && m.to == Endpoint::Interface(InterfaceId::new("system_worker_coder_max"))
    });
    assert!(
        pass_mark,
        "the passing watcher's edge still records its admitted tick"
    );
}

/// A System interface with a live DRIVER (`system_coder_env` holds
/// `interface_driver_ref`) is an executable reader, not a driverless relay:
/// its outbound movement is recorded for the door's OWN ledger-tail process to
/// consume (arch.md §Interface liveness). The runtime must NOT re-inject that
/// outbound as a cross-run arrival — if it did, the model's tool-call would
/// spend the door's own result checkpoint before the real result returns. The
/// driverless run→run seam above proves the OTHER half of the same distinction.
#[tokio::test]
async fn a_driver_backed_door_outbound_is_not_relayed_into_its_result_checkpoint() {
    let blobs: SharedBlobs = Arc::new(std::sync::Mutex::new(InMemoryBlobs::new()));
    let artifact_root = std::env::temp_dir().join(format!(
        "octopus-driver-door-no-relay-{}-{}",
        std::process::id(),
        line!()
    ));
    let embedded = support::build_test_spine(SpineRootConfig {
        registry_root: registry_root(),
        run: RunId::new("run_door_turn"),
        blobs: blobs.clone(),
        interface_artifact_root: artifact_root.clone(),
        run_records_root: artifact_root.join("runs"),
        ee_instances_root: artifact_root.join("ee_instances"),
    })
    .await
    .expect("the embedded product spine boots for the executable-reader seam");

    // One run wearing the coder pin: the model turn emits a tool-call OUT toward
    // the coder-env door (cp_tool), and a separate result checkpoint waits for
    // the door's real result on that same folder (cp_result).
    write_run_record(
        &embedded.interface_artifact_root,
        &embedded.runtime,
        &serde_json::json!({
            "id": "run_door_turn",
            "ee": "ee_existence",
            "worker": "system_worker_coder_max",
            "trace": "trace:run_door_turn",
            "status": "running",
            "interface_state": {},
            "checkpoints": [
                {
                    "id": "cp_tool",
                    "run": "run_door_turn",
                    "trace": "trace:run_door_turn:cp_tool",
                    "epoch": 0,
                    "interface_binding": support::all_binding("world_terminal"),
                    "outbound_interface_bindings": [support::all_binding("system_coder_env")],
                    "artifact_bindings": support::worker_role_artifact_bindings(&[
                        "system_worker_coder_max",
                    ]),
                },
                {
                    "id": "cp_result",
                    "run": "run_door_turn",
                    "trace": "trace:run_door_turn:cp_result",
                    "epoch": 0,
                    "interface_binding": support::all_binding("system_coder_env"),
                    "outbound_interface_bindings": [support::all_binding("world_terminal")],
                    "artifact_bindings": support::worker_role_artifact_bindings(&[
                        "system_worker_coder_max",
                    ]),
                }
            ],
            "active_checkpoints": ["cp_tool", "cp_result"],
        }),
    );

    let mut log = LiveMovementLog::new(InMemoryLog::new());
    let payload =
        serde_json::to_vec(&serde_json::json!({ "tool": "coder.apply_patch" })).expect("encodes");
    let submit = captured_boundary_artifact(
        &artifact_root.join("tool-call-arrival"),
        "tool.call",
        &payload,
    );

    let mut blob_handle = SharedBlobHandle::new(blobs.clone());
    let outcome = admit_interface_artifact_arrival(
        &embedded,
        &mut log,
        &mut blob_handle,
        &submit,
        &InterfaceId::new("world_terminal"),
    )
    .await
    .expect("the terminal tool-call admits its own checkpoint");

    let admitted_checkpoints: Vec<String> = outcome
        .admission
        .records
        .iter()
        .map(|record| record.checkpoint.as_str().to_string())
        .collect();
    assert!(
        admitted_checkpoints.contains(&"cp_tool".to_string()),
        "the tool-call admits its own emitting checkpoint: {admitted_checkpoints:?}"
    );
    assert!(
        !admitted_checkpoints.contains(&"cp_result".to_string()),
        "the driver-backed door is an executable reader: its result checkpoint is NOT spent by the outbound tool-call: {admitted_checkpoints:?}"
    );

    let movements = log.read_from(Seq(0));
    let outbound_to_door = movements.iter().any(|m| {
        matches!(m.kind, MovementKind::InterfaceOutbound)
            && matches!(&m.to, octopus_ledger::Endpoint::Interface(i) if i.as_str() == "system_coder_env")
    });
    assert!(
        outbound_to_door,
        "the tool-call's outbound movement toward the door IS recorded for the door's own ledger-tail driver to consume"
    );
    let arrival_from_door = movements.iter().any(|m| {
        matches!(m.kind, MovementKind::InterfaceArrival)
            && matches!(&m.from, octopus_ledger::Endpoint::Interface(i) if i.as_str() == "system_coder_env")
    });
    assert!(
        !arrival_from_door,
        "the runtime never plays the door's role: no cross-run arrival is injected FROM the driver-backed door"
    );
}

/// Artifact identity is content addressing; occurrence is the checkpoint edge.
/// After a re-arm the SAME boundary artifact (verbatim repeat) must admit the
/// freshly armed checkpoint and record a SECOND movement — there is no global
/// (interface, artifact) gate that fake-admits it. Idempotency stays exactly at
/// the concrete checkpoint edge: a repeat into a still-open spent checkpoint
/// refuses `checkpoint_spent`.
#[tokio::test]
async fn the_same_boundary_artifact_admits_each_freshly_armed_checkpoint() {
    let blobs: SharedBlobs = Arc::new(std::sync::Mutex::new(InMemoryBlobs::new()));
    let artifact_root = std::env::temp_dir().join(format!(
        "octopus-repeat-artifact-two-movements-{}-{}",
        std::process::id(),
        line!()
    ));
    let embedded = support::build_test_spine(SpineRootConfig {
        registry_root: registry_root(),
        run: RunId::new("run_repeat_turn"),
        blobs: blobs.clone(),
        interface_artifact_root: artifact_root.clone(),
        run_records_root: artifact_root.join("runs"),
        ee_instances_root: artifact_root.join("ee_instances"),
    })
    .await
    .expect("the embedded product spine boots for the repeat-artifact seam");

    let arm = |checkpoint: &str| {
        serde_json::json!({
            "id": "run_repeat_turn",
            "ee": "ee_existence",
            "worker": "system_worker_coder_max",
            "trace": "trace:run_repeat_turn",
            "status": "running",
            "interface_state": {},
            "checkpoints": [{
                "id": checkpoint,
                "run": "run_repeat_turn",
                "trace": format!("trace:run_repeat_turn:{checkpoint}"),
                "epoch": 0,
                "interface_binding": support::all_binding("world_terminal"),
                "outbound_interface_bindings": [],
                "artifact_bindings": support::worker_role_artifact_bindings(&[
                    "system_worker_coder_max",
                ]),
            }],
            "active_checkpoints": [checkpoint],
        })
    };

    let mut log = LiveMovementLog::new(InMemoryLog::new());
    let payload = serde_json::to_vec(&serde_json::json!({ "prompt": "the exact same message" }))
        .expect("encodes");
    let submit = captured_boundary_artifact(
        &artifact_root.join("repeat-arrival"),
        "operator.input",
        &payload,
    );
    let mut blob_handle = SharedBlobHandle::new(blobs.clone());

    write_run_record(
        &embedded.interface_artifact_root,
        &embedded.runtime,
        &arm("cp_turn_1"),
    );
    let first = admit_interface_artifact_arrival(
        &embedded,
        &mut log,
        &mut blob_handle,
        &submit,
        &InterfaceId::new("world_terminal"),
    )
    .await
    .expect("the first arrival admits the first armed checkpoint");
    let artifact_id = first.admission.records[0]
        .arrival_movement
        .artifact
        .as_ref()
        .expect("the arrival movement carries its boundary artifact")
        .id
        .clone();

    // The interface re-arms its run JSON: a NEW checkpoint on the same folder.
    write_run_record(
        &embedded.interface_artifact_root,
        &embedded.runtime,
        &arm("cp_turn_2"),
    );
    admit_interface_artifact_arrival(
        &embedded,
        &mut log,
        &mut blob_handle,
        &submit,
        &InterfaceId::new("world_terminal"),
    )
    .await
    .expect(
        "the SAME artifact admits the freshly armed checkpoint — no global gate fake-admits it",
    );

    let arrivals: Vec<_> = log
        .read_from(Seq(0))
        .into_iter()
        .filter(|m| {
            matches!(m.kind, MovementKind::InterfaceArrival)
                && m.run().is_some_and(|run| run.as_str() == "run_repeat_turn")
                && m.artifact.as_ref().map(|a| &a.id) == Some(&artifact_id)
        })
        .collect();
    assert_eq!(
        arrivals.len(),
        2,
        "the verbatim repeat creates a SECOND movement for the same content-addressed artifact"
    );

    let mut edited = arm("cp_turn_2");
    let mut neighbor = edited["checkpoints"][0].clone();
    neighbor["id"] = serde_json::json!("cp_neighbor");
    neighbor["interface_binding"] = serde_json::json!(support::all_binding("system_context"));
    edited["checkpoints"].as_array_mut().unwrap().push(neighbor);
    edited["active_checkpoints"] = serde_json::json!(["cp_turn_2", "cp_neighbor"]);
    write_run_record(&artifact_root, &embedded.runtime, &edited);

    let spent = admit_interface_artifact_arrival(
        &embedded,
        &mut log,
        &mut blob_handle,
        &submit,
        &InterfaceId::new("world_terminal"),
    )
    .await
    .expect_err("editing a neighboring checkpoint must not re-arm the spent edge");
    assert!(
        matches!(
            spent,
            octopus_app_server::InterfaceArtifactArrivalAdmissionError::Arrival(
                octopus_app_server::ArrivalError::CheckpointSpent(_)
            )
        ),
        "the concrete checkpoint edge is the idempotency boundary, got: {spent}"
    );
}

#[tokio::test]
async fn retained_fanout_recovers_only_original_unfinished_edges_after_restart() {
    use octopus_app_server::admit_retained_interface_artifact_arrival as deliver;
    use octopus_file_storage::publication::WriteGuard;
    use octopus_ledger::{ArtifactDelivery, DeliveryVerdict};
    let root = tempfile::tempdir().unwrap();
    let blobs: SharedBlobs = Arc::new(std::sync::Mutex::new(InMemoryBlobs::new()));
    let config = || SpineRootConfig {
        registry_root: registry_root(), run: RunId::new("first"), blobs: blobs.clone(),
        interface_artifact_root: root.path().into(), run_records_root: root.path().join("runs"),
        ee_instances_root: root.path().join("ee_instances"),
    };
    let embedded = support::build_test_spine(config()).await.unwrap();
    let record = |id: &str, checkpoint: &str, verified: bool| serde_json::json!({
        "id":id, "ee":"ee_existence", "worker":"system_worker_coder_max", "trace":format!("trace:{id}"),
        "status":"running", "interface_state":{}, "active_checkpoints":[checkpoint], "checkpoints":[{
            "id":checkpoint, "run":id, "trace":format!("trace:{id}:{checkpoint}"), "epoch":0,
            "interface_binding":support::all_binding("world_terminal"), "outbound_interface_bindings":[],
            "artifact_bindings":support::worker_role_artifact_bindings(&["system_worker_coder_max"]),
            "required_verified_artifacts":if verified { serde_json::json!([{
                "mark":"coder.build.ok", "artifact_label":"coder.build.result", "verifier":"build_success", "params":{}
            }, {
                "mark":"coder.build.ok", "artifact_label":"second.result", "verifier":"artifact_exists", "params":{}
            }]) } else { serde_json::json!([]) }
        }]
    });
    write_run_record(root.path(), &embedded.runtime, &record("first", "first_0", false));
    write_run_record(root.path(), &embedded.runtime, &record("second", "second_0", true));
    let artifact = captured_boundary_artifact(root.path().join("output"), "coder.build.result", &serde_json::to_vec(&build_success_payload()).unwrap());
    let interface = InterfaceId::new("world_terminal");
    let mut log = LiveMovementLog::new(InMemoryLog::new());
    let mut blob_handle = SharedBlobHandle::new(blobs.clone());
    let busy = WriteGuard::acquire(&root.path().join("runs/second.json"), false).unwrap();
    let first = deliver(&embedded, &mut log, &mut blob_handle, &artifact, &interface, "completion:1").await.unwrap();
    assert!(matches!(first, ArtifactDelivery::Pending { .. }));
    assert_eq!(log.read_from(Seq(0)).iter().filter(|row| row.kind == MovementKind::InterfaceArrival).count(), 1);
    drop(busy);
    let ArtifactDelivery::Pending { scope_seq } = first else { unreachable!() };
    let source = log.read_at(Seq(scope_seq)).unwrap().artifact.unwrap();
    let mut target = embedded.runtime.resolve_interface_arrival_targets(&interface, "coder.build.result").unwrap()
        .into_iter().find(|target| target.run.as_str() == "second").unwrap();
    target.delivery_scope_seq = Some(scope_seq);
    let prepared = embedded.runtime.admit_resolved_interface_checkpoint(
        &target, source.clone(), "coder.build.result".into(), None,
    ).await.unwrap();
    let prefix = log.append(MovementDraft {
        trace:target.checkpoint_trace.clone(), kind:MovementKind::InterfaceArrival,
        from:Endpoint::Interface(interface.clone()), to:Endpoint::Interface(target.worker.clone()),
        artifact:Some(source), invocation_metadata:Some(prepared.observation().invocation_metadata.clone().into()),
        causal_floor:None,
    });
    drop(prepared);
    let other = captured_boundary_artifact(root.path().join("other"), "second.result", b"independent requirement");
    deliver(&embedded, &mut log, &mut blob_handle, &other, &interface, "completion:2").await.unwrap();
    write_run_record(root.path(), &embedded.runtime, &record("first", "first_1", false));
    write_run_record(root.path(), &embedded.runtime, &record("later", "later_0", false));
    let mut second = record("second", "second_0", true);
    second["interface_state"] = serde_json::json!({"world_terminal":{"draft":"later independent context"}});
    write_run_record(root.path(), &embedded.runtime, &second);
    drop(embedded);
    let cold = support::build_test_spine(config()).await.unwrap();
    cold.runtime.reconstruct_spent_checkpoints_from_log(&log).unwrap();
    let file = root.path().join("runs/second.json");
    let bytes = std::fs::read(&file).unwrap();
    std::fs::remove_file(&file).unwrap();
    std::fs::create_dir(&file).unwrap();
    assert_eq!(deliver(&cold, &mut log, &mut blob_handle, &artifact, &interface, "completion:1").await.unwrap(),
        ArtifactDelivery::Pending { scope_seq });
    std::fs::remove_dir(&file).unwrap();
    std::fs::write(&file, bytes).unwrap();
    let completed = deliver(&cold, &mut log, &mut blob_handle, &artifact, &interface, "completion:1").await.unwrap();
    assert!(matches!(completed, ArtifactDelivery::Complete { verdict: DeliveryVerdict::Admitted { .. } }));
    let rows = log.read_from(Seq(0));
    let arrivals: Vec<_> = rows.iter().filter(|row| row.kind == MovementKind::InterfaceArrival).collect();
    assert_eq!(arrivals.len(), 5);
    let standing = arrivals.iter().filter(|row| row.runtime().unwrap().checkpoint.as_str() == "first_0")
        .map(|row| row.artifact.as_ref().unwrap().id.clone()).collect::<std::collections::BTreeSet<_>>();
    assert_eq!(standing.len(), 2, "a checkpoint without requirements takes each distinct artifact once");
    assert!(arrivals.iter().any(|row| row.runtime().unwrap().checkpoint.as_str() == "second_0"));
    assert!(!arrivals.iter().any(|row| matches!(row.runtime().unwrap().checkpoint.as_str(), "first_1" | "later_0")));
    assert_eq!(rows.iter().filter(|row| matches!(row.kind, MovementKind::MarkOutcomeAdmitted(_))).count(), 2);
    let replacement = arrivals.iter().find(|row| row.invocation_metadata.as_ref().unwrap().invocation().unwrap()
        .replaces_arrival_seq == Some(prefix.seq.0)).unwrap();
    let proofs = octopus_ledger::InterfaceArrivalMarkProofIndex::from_movements(&rows).unwrap();
    assert!(proofs.proof_lamport(&prefix).is_none());
    assert!(proofs.proof_lamport(replacement).is_some());
    let mark = rows.iter().find(|row| row.invocation_metadata.as_ref().and_then(|metadata| metadata.mark_outcome())
        .is_some_and(|mark| mark.arrival_seq == Some(replacement.seq.0))).unwrap();
    assert!(!octopus_ledger::mark_outcome_proves_interface_arrival(&prefix, mark));
    let mut invalid = rows.clone();
    if let Some(octopus_ledger::MovementMetadata::Invocation(metadata)) = &mut invalid[replacement.seq.0 as usize].invocation_metadata {
        metadata.replaces_arrival_seq = Some(prefix.seq.0 + 1);
    }
    assert!(octopus_ledger::InterfaceArrivalMarkProofIndex::from_movements(&invalid).is_err());
    let saved_index = std::mem::take(&mut *cold.arrival_dedup.lock().unwrap());
    for warm in [false, true] {
        let mut corrupt_log = LiveMovementLog::new(InMemoryLog::new());
        let mut index = octopus_ledger_index::ArrivalDedupIndex::default();
        for row in &invalid {
            if warm && row.seq == replacement.seq { index.catch_up(&corrupt_log).unwrap(); }
            corrupt_log.append(MovementDraft {
                trace: row.trace.clone(), kind: row.kind.clone(), from: row.from.clone(), to: row.to.clone(),
                artifact: row.artifact.clone(), invocation_metadata: row.invocation_metadata.clone(),
                causal_floor: Some(octopus_ledger::Lamport(row.lamport.0 - 1)),
            });
        }
        index.catch_up(&corrupt_log).unwrap();
        assert!(index.retained_arrival_scope(&interface, "completion:1").is_err());
        assert!(index.retained_arrival_edges(scope_seq).is_err());
        *cold.arrival_dedup.lock().unwrap() = index;
        let before = corrupt_log.len();
        assert!(deliver(&cold, &mut corrupt_log, &mut blob_handle, &artifact, &interface, "completion:1").await.is_err());
        assert_eq!(corrupt_log.len(), before, "index corruption cannot become a terminal recipient refusal");
    }
    *cold.arrival_dedup.lock().unwrap() = saved_index;
    cold.runtime.reconstruct_spent_checkpoints_from_log(&log).unwrap();
    let before = log.len();
    assert_eq!(deliver(&cold, &mut log, &mut blob_handle, &artifact, &interface, "completion:1").await.unwrap(), completed);
    assert_eq!(log.len(), before);
    let changed = captured_boundary_artifact(root.path().join("changed"), "coder.build.result", b"different output");
    assert!(deliver(&cold, &mut log, &mut blob_handle, &changed, &interface, "completion:1").await.is_err());
    deliver(&cold, &mut log, &mut blob_handle, &artifact, &interface, "new deliberate submission").await.unwrap();
    assert!(log.read_from(Seq(before as u64)).iter().any(|row| row.kind == MovementKind::InterfaceArrival
        && row.runtime().unwrap().checkpoint.as_str() == "first_1"));
}
