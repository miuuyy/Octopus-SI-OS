use std::path::PathBuf;
use std::sync::Arc;

use octopus_app_server::{
    SharedBlobHandle, SharedBlobs, SpineRootConfig, admit_interface_artifact_arrival,
};
use octopus_descriptors::{EeId, InterfaceId};
use octopus_ledger::{InMemoryBlobs, InMemoryLog, LiveMovementLog};
use octopus_policy::WorkerInterfacePolicy;
use octopus_runtime::RunId;

mod support;

fn registry_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../registry")
}

fn terminal_worker_policy() -> WorkerInterfacePolicy {
    serde_json::from_slice(
        &std::fs::read(registry_root().join("interface/system_worker_coder_max/policy.json"))
            .expect("the terminal worker policy is on disk"),
    )
    .expect("the terminal worker policy parses")
}

fn write_terminal_run_record_with_context_bindings(
    root: &std::path::Path,
    runtime: &octopus_runtime::Runtime,
    run: &RunId,
    outbound_interface: &str,
    role_artifacts: &[octopus_descriptors::ArtifactId],
) {
    let runs_dir = root.join("runs");
    runtime.set_run_records_dir(runs_dir.clone());
    std::fs::create_dir_all(&runs_dir).unwrap();
    let artifact_bindings = role_artifacts
        .iter()
        .map(|id| serde_json::json!({"id": id}))
        .collect::<Vec<_>>();
    let record = serde_json::json!({
        "id": run,
        "ee": EeId::new("ee_operator_interaction"),
        "worker": InterfaceId::new("system_worker_coder_max"),
        "trace": format!("trace:{}", run.as_str()),
        "status": "running",
        "interface_state": {},
        "checkpoints": [{
            "id": "cp_boot_terminal_input",
            "run": run,
            "trace": format!("trace:{}:cp_boot_terminal_input", run.as_str()),
            "epoch": 0,
            "interface_binding": support::all_binding("world_operator_chat"),
            "outbound_interface_bindings": [support::all_binding(outbound_interface)],
            "artifact_bindings": artifact_bindings,
        }],
        "active_checkpoints": ["cp_boot_terminal_input"],
    });
    std::fs::write(
        runs_dir.join(format!("{}.json", run.as_str())),
        serde_json::to_vec_pretty(&record).unwrap(),
    )
    .unwrap();
}

#[tokio::test]
async fn terminal_turn_receives_nonempty_role_and_goal_context() {
    let blobs: SharedBlobs = Arc::new(std::sync::Mutex::new(InMemoryBlobs::new()));
    let interface_artifact_root = std::env::temp_dir().join(format!(
        "octopus-live-context-test-{}-{}",
        std::process::id(),
        line!()
    ));
    let embedded = support::build_test_spine(SpineRootConfig {
        registry_root: registry_root(),
        run: RunId::new("run_terminal_live_context"),
        blobs: blobs.clone(),
        interface_artifact_root: interface_artifact_root.clone(),
        run_records_root: interface_artifact_root.join("runs"),
        ee_instances_root: interface_artifact_root.join("ee_instances"),
    })
    .await
    .expect("the embedded spine boots");

    let worker_policy = terminal_worker_policy();
    let role_artifacts = &worker_policy.role_artifacts;
    let pins_id = |id: &str| -> bool {
        role_artifacts
            .iter()
            .any(|artifact| artifact.as_str() == id)
    };
    assert!(
        pins_id("artifact-worker-role-template"),
        "the worker pin declares the role artifact"
    );
    assert!(
        pins_id("artifact-grounding-system-architecture"),
        "the worker pin declares the grounding artifact"
    );

    write_terminal_run_record_with_context_bindings(
        &embedded.interface_artifact_root,
        &embedded.runtime,
        &embedded.run,
        "system_context",
        role_artifacts,
    );

    let mut log = LiveMovementLog::new(InMemoryLog::new());
    let mut blob_handle = SharedBlobHandle::new(blobs.clone());
    let submit = support::materialized_interface_arrival_submit_artifact(
        &std::env::temp_dir().join(format!(
            "octopus-live-context-arrivals-{}-{}",
            std::process::id(),
            line!()
        )),
        &blobs,
        &serde_json::json!({"prompt": "hello from the terminal"}),
    );
    let outcome = admit_interface_artifact_arrival(
        &embedded,
        &mut log,
        &mut blob_handle,
        &submit,
        &InterfaceId::new("world_operator_chat"),
    )
    .await
    .expect("the terminal submit drives arrival→run through the gate");

    let transfer = outcome
        .admission
        .records
        .iter()
        .flat_map(|record| record.outbound_transfers.iter())
        .find(|t| t.to_interface.as_str() == "system_context")
        .expect("the terminal turn fans out along the authored interface route");

    let goal = transfer
        .invocation_metadata
        .goal()
        .expect("the model-facing grant projection carries our EE goal");
    assert!(
        !goal.trim().is_empty() && goal.contains("operator conversation"),
        "the goal is our ee_operator_interaction goal_class, got: {goal}"
    );

    assert_eq!(
        transfer.artifact_bindings.len(),
        role_artifacts.len(),
        "every worker role artifact rides the authored route on the outbound transfer"
    );
    let store = blobs.lock().unwrap();
    for binding in &transfer.artifact_bindings {
        let bytes = store
            .get(&binding.content)
            .unwrap_or_else(|| panic!("binding {} resolves to a seeded blob", binding.id.as_str()));
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).expect("the seeded context payload is JSON");
        assert_eq!(
            value.get("label").and_then(serde_json::Value::as_str),
            Some("context.reference"),
            "a seeded context binding is a role artifact (Seam 3 folds it into the model's base instructions)"
        );
        let text = value
            .get("text")
            .and_then(serde_json::Value::as_str)
            .expect("the role artifact carries its text");
        assert!(!text.trim().is_empty(), "the role text is non-empty");
        let declared: serde_json::Value = serde_json::from_slice(
            &std::fs::read(registry_root().join("artifact").join(binding.id.as_str()).join("artifact.json")).unwrap(),
        ).unwrap();
        assert_eq!(value["text"], declared["text"]);
    }
}
