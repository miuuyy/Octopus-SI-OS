#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use octopus_app_server::{
    admit_interface_artifact_arrival, build_embedded_spine, SharedBlobHandle, SharedBlobs,
    SpineRootConfig,
};
use octopus_descriptors::InterfaceId;
use octopus_interface_transport::{
    spawn_life_config_of, spawn_process_life, InterfaceActivationArrival, InterfaceLiftContext,
};
use octopus_interfaces::InterfaceArtifactFile;
use octopus_ledger::{Endpoint, MovementKind};
use octopus_ledger_store::{DurableBlobs, DurableLog};
use octopus_runtime::RunId;
use tokio_util::sync::CancellationToken;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../..")
}

fn repo_registry_root() -> PathBuf {
    repo_root().join("registry")
}

fn write_pyecho_run_record(runs_dir: &Path, run_id: &str) {
    std::fs::create_dir_all(runs_dir).expect("runs dir creates");
    std::fs::write(
        runs_dir.join(format!("{run_id}.json")),
        serde_json::to_vec_pretty(&serde_json::json!({
            "id": run_id,
            "ee": "ee_existence",
            "worker": "system_worker_pyecho",
            "trace": format!("trace:{run_id}"),
            "status": "running",
            "interface_state": {},
            "checkpoints": [{
                "id": "cp_pyecho_in",
                "run": run_id,
                "trace": format!("trace:{run_id}:cp_pyecho_in"),
                "epoch": 0,
                "interface_binding": octopus_runtime::AuthoredInterfaceBinding::all(octopus_descriptors::InterfaceId::new("world_operator_chat")),
                "outbound_interface_bindings": [octopus_runtime::AuthoredInterfaceBinding::all(octopus_descriptors::InterfaceId::new("world_pyecho"))],
                "artifact_bindings": []
            }],
            "active_checkpoints": ["cp_pyecho_in"]
        }))
        .expect("run record encodes"),
    )
    .expect("run record writes");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pure_python_interface_folder_lifts_and_round_trips_an_artifact_through_the_real_spine() {
    let dir = tempfile::tempdir().expect("temp root");
    let durable_root = dir.path().join("durable");
    std::fs::create_dir_all(durable_root.join("blobs")).expect("durable blobs dir");
    let registry_root = repo_registry_root();

    let blobs: SharedBlobs = Arc::new(std::sync::Mutex::new(
        DurableBlobs::open(durable_root.join("blobs")).expect("durable blob store opens"),
    ));
    let interface_artifact_root = durable_root.join("handler-interface-artifacts");
    let embedded = build_embedded_spine(SpineRootConfig {
        registry_root: registry_root.clone(),
        run: RunId::new("run_pyecho_probe"),
        blobs: blobs.clone(),
        interface_artifact_root: interface_artifact_root.clone(),
        run_records_root: interface_artifact_root.join("runs"),
        ee_instances_root: interface_artifact_root.join("ee_instances"),
    })
    .await
    .expect("the spine boots over the registry carrying the python interface folder");

    write_pyecho_run_record(&interface_artifact_root.join("runs"), "run_pyecho_probe");

    let mut log =
        DurableLog::open(durable_root.join("movements.jsonl")).expect("durable log opens");
    let mut arrival_blobs = SharedBlobHandle::new(blobs.clone());
    let artifact_root = dir.path().join("interface-artifacts");
    let input = InterfaceArtifactFile::write(
        &artifact_root,
        "operator.message",
        br#"{"text":"hello octopus"}"#,
    )
    .and_then(|artifact| artifact.capture_bounded(1024 * 1024, 64 * 1024 * 1024))
    .expect("terminal input artifact captures before runtime hand-in");

    let admitted = admit_interface_artifact_arrival(
        &embedded,
        &mut log,
        &mut arrival_blobs,
        &input,
        &InterfaceId::new("world_operator_chat"),
    )
    .await
    .expect("the input artifact routes through the run to the python interface");
    assert!(
        admitted.admission.first_outbound_artifact().is_some(),
        "the run records an outbound transfer to the python interface"
    );

    let outbound_to_pyecho = octopus_ledger::MovementLog::read_from(&log, octopus_ledger::Seq(0))
        .into_iter()
        .find(|m| {
            m.kind == MovementKind::InterfaceOutbound
                && matches!(&m.to, Endpoint::Interface(i) if i.as_str() == "world_pyecho")
        })
        .expect("an InterfaceOutbound movement is addressed to the python interface folder");
    drop(log);

    let interface = InterfaceId::new("world_pyecho");
    let config = spawn_life_config_of(&registry_root, &interface)
        .expect("the life.json parses")
        .expect("the python interface folder declares a spawn life");

    std::env::set_current_dir(repo_root()).expect("cwd to repo root for the relative spawn bin");
    let (tx, mut rx) = tokio::sync::mpsc::channel::<InterfaceActivationArrival>(4);
    let cancel = CancellationToken::new();
    let ctx = InterfaceLiftContext::unprivileged(
        interface.clone(),
        registry_root.join("interface").join(interface.as_str()),
        None,
        dir.path().join("home/interfaces").join(interface.as_str()),
        durable_root.join("movements.jsonl"),
        durable_root.join("blobs"),
        durable_root.join("ledger-index.sqlite"),
        std::collections::BTreeMap::from([("repo_root".to_string(), repo_root())]),
        tx,
        cancel.child_token(),
    );
    let life = tokio::spawn(spawn_process_life(ctx, config));

    let reply = tokio::time::timeout(Duration::from_secs(20), rx.recv())
        .await
        .expect("the lifted python process must emit within the timeout")
        .expect("the arrival intake is open");
    cancel.cancel();

    assert_eq!(reply.interface.as_str(), "world_pyecho");
    let manifest = reply
        .artifact
        .read_manifest()
        .expect("reply manifest reads");
    assert_eq!(manifest.label, "pyecho.uppercased");
    let payload = reply
        .artifact
        .read_payload_bytes()
        .expect("reply payload reads");
    let reply_json: serde_json::Value =
        serde_json::from_slice(&payload).expect("the python reply is JSON");
    assert_eq!(
        reply_json["text"], "HELLO OCTOPUS",
        "the python transformer uppercased the routed input payload"
    );
    assert_eq!(reply_json["by"], "world_pyecho");

    assert!(outbound_to_pyecho.artifact.is_some());

    let _ = tokio::time::timeout(Duration::from_secs(10), life).await;
}
