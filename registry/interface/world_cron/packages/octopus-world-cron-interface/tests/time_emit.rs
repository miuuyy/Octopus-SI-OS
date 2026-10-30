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

const CRON_RUN: &str = "run_cron_existence";

fn private_life_stderr(var_dir: &Path) -> String {
    let Ok(entries) = std::fs::read_dir(var_dir) else {
        return "stderr unavailable: interface var directory is absent".to_string();
    };
    let mut paths = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    name.starts_with("interface-life.stderr.") && name.ends_with(".log")
                })
        })
        .collect::<Vec<_>>();
    paths.sort();
    if paths.is_empty() {
        return "stderr unavailable: life process emitted no diagnostics".to_string();
    }
    paths
        .into_iter()
        .map(|path| {
            std::fs::read_to_string(&path)
                .unwrap_or_else(|error| format!("{}: {error}", path.display()))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cron_interface_folder_lifts_and_emits_a_time_artifact_into_a_run() {
    let dir = tempfile::tempdir().expect("temp root");
    let durable_root = dir.path().join("durable");
    std::fs::create_dir_all(durable_root.join("blobs")).expect("durable blobs dir");
    let registry_root = repo_registry_root();
    let installed_registry_root = dir.path().join("installed/registry");
    let interface_dir = installed_registry_root.join("interface/world_cron");
    let timezone_database_root = dir.path().join("timezone-db");
    std::fs::create_dir_all(&interface_dir).expect("installed cron interface dir");
    std::fs::create_dir_all(timezone_database_root.join("Europe"))
        .expect("timezone fixture directory");
    std::fs::copy(
        "/usr/share/zoneinfo/Europe/Kyiv",
        timezone_database_root.join("Europe/Kyiv"),
    )
    .expect("timezone fixture copies");
    for file in [
        "interface.json",
        "life.json",
        "cron_life.py",
        "octopus_content_hash.py",
    ] {
        std::fs::copy(
            registry_root.join("interface/world_cron").join(file),
            interface_dir.join(file),
        )
        .expect("cron interface package file copies");
    }
    let mut activation: serde_json::Value = serde_json::from_slice(
        &std::fs::read(registry_root.join("interface/world_cron/activation.json"))
            .expect("cron activation reads"),
    )
    .expect("cron activation parses");
    activation["activations"][0]["enabled"] = serde_json::json!(true);
    activation["activations"][0]["config"]["schedule"]["timezone"] =
        serde_json::json!("Europe/Kyiv");
    std::fs::write(
        interface_dir.join("activation.json"),
        serde_json::to_vec_pretty(&activation).expect("enabled cron activation encodes"),
    )
    .expect("enabled cron activation writes");

    let blobs: SharedBlobs = Arc::new(std::sync::Mutex::new(
        DurableBlobs::open(durable_root.join("blobs")).expect("durable blob store opens"),
    ));
    let interface_artifact_root = durable_root.join("handler-interface-artifacts");
    let runs_dir = interface_artifact_root.join("runs");
    let ee_instances_dir = interface_artifact_root.join("ee_instances");
    std::fs::create_dir_all(&runs_dir).expect("runs dir creates");
    std::fs::create_dir_all(&ee_instances_dir).expect("EE instances dir creates");
    std::fs::write(
        ee_instances_dir.join("ee_existence.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "ee_id": "ee_existence",
            "template_name": "existence",
            "parent_ee_id": null,
            "goal": "Exist"
        }))
        .expect("bootstrap EE encodes"),
    )
    .expect("bootstrap EE writes");
    let embedded = build_embedded_spine(SpineRootConfig {
        registry_root: registry_root.clone(),
        run: RunId::new("run_cron_probe"),
        blobs: blobs.clone(),
        interface_artifact_root: interface_artifact_root.clone(),
        run_records_root: runs_dir.clone(),
        ee_instances_root: ee_instances_dir.clone(),
    })
    .await
    .expect("the spine boots over the registry carrying the python cron folder");

    let interface = InterfaceId::new("world_cron");
    let config = spawn_life_config_of(&installed_registry_root, &interface)
        .expect("the life.json parses")
        .expect("the python cron folder declares a spawn life");

    let run_template = activation["activations"][0]["config"]["run_template"].clone();
    std::fs::write(
        runs_dir.join(format!("{CRON_RUN}.json")),
        serde_json::to_vec_pretty(&run_template).expect("cron run template encodes"),
    )
    .expect("operator-authored cron run writes");

    std::env::set_current_dir(repo_root()).expect("cwd to repo root for the relative spawn bin");
    let (tx, mut rx) = tokio::sync::mpsc::channel::<InterfaceActivationArrival>(4);
    let cancel = CancellationToken::new();
    let ctx = InterfaceLiftContext::unprivileged(
        interface.clone(),
        interface_dir,
        None,
        dir.path().join("home/interfaces").join(interface.as_str()),
        durable_root.join("movements.jsonl"),
        durable_root.join("blobs"),
        durable_root.join("ledger-index.sqlite"),
        std::collections::BTreeMap::from([
            ("interface_life_install_root".to_string(), repo_root()),
            ("repo_root".to_string(), repo_root()),
            ("run_records_root".to_string(), runs_dir.clone()),
            ("ee_instances_root".to_string(), ee_instances_dir),
            (
                "registry_interface_root".to_string(),
                registry_root.join("interface"),
            ),
            (
                "registry_ee_template_root".to_string(),
                registry_root.join("ee_template"),
            ),
            ("registry_tool_root".to_string(), registry_root.join("tool")),
            (
                "registry_worker_environment_kind_root".to_string(),
                registry_root.join("worker_environment_kind"),
            ),
            ("timezone_database_root".to_string(), timezone_database_root),
            (
                "app_server_control_root".to_string(),
                dir.path().join("home/app-server-control"),
            ),
            (
                "interface_hand_in_root".to_string(),
                dir.path().join("home/interface-hand-in"),
            ),
        ]),
        tx,
        cancel.child_token(),
    );
    let mut life = tokio::spawn(spawn_process_life(ctx, config));

    let tick = tokio::time::timeout(Duration::from_secs(20), async {
        tokio::select! {
            arrival = rx.recv() => arrival.ok_or_else(|| {
                let stderr = private_life_stderr(
                    &dir.path().join("home/interfaces/world_cron"),
                );
                format!("the cron arrival intake closed: {stderr}")
            }),
            result = &mut life => Err(format!("the cron life exited before emitting: {result:?}")),
        }
    })
    .await
    .expect("the lifted python cron process must emit a tick within the timeout")
    .expect("the cron life remains available until its tick");
    cancel.cancel();

    assert_eq!(tick.interface.as_str(), "world_cron");
    let authored = octopus_run_record::RunRecord::from_bytes(
        &std::fs::read(runs_dir.join(format!("{CRON_RUN}.json")))
            .expect("the operator-authored cron run remains readable after the tick"),
    )
    .expect("the operator-authored run record parses through the canonical run contract");
    assert_eq!(authored.id, RunId::new(CRON_RUN));
    assert_eq!(authored.worker.as_str(), "system_worker_cron");
    let manifest = tick.artifact.read_manifest().expect("tick manifest reads");
    assert_eq!(manifest.label, "cron.current_time");
    let payload = tick
        .artifact
        .read_payload_bytes()
        .expect("tick payload reads");
    let tick_json: serde_json::Value =
        serde_json::from_slice(&payload).expect("the cron tick is JSON");
    assert_eq!(tick_json["source"], "cron");
    assert!(
        tick_json["now_unix_ms"].as_i64().is_some_and(|ms| ms > 0),
        "the tick carries a real current-time stamp: {tick_json}"
    );
    assert!(
        tick_json.get("now_iso").and_then(|v| v.as_str()).is_some(),
        "the tick carries an ISO current-time string: {tick_json}"
    );

    let mut log =
        DurableLog::open(durable_root.join("movements.jsonl")).expect("durable log opens");
    let mut arrival_blobs = SharedBlobHandle::new(blobs.clone());

    admit_interface_artifact_arrival(
        &embedded,
        &mut log,
        &mut arrival_blobs,
        &tick.artifact,
        &InterfaceId::new("world_cron"),
    )
    .await
    .expect("the cron time-artifact routes through the run to its inbox");

    let arrival_into_run = octopus_ledger::MovementLog::read_from(&log, octopus_ledger::Seq(0))
        .into_iter()
        .find(|m| {
            m.kind == MovementKind::InterfaceArrival
                && m.run().is_some_and(|run| run.as_str() == CRON_RUN)
                && m.runtime().is_some_and(|runtime| m.to == Endpoint::Interface(runtime.worker.clone()))
                && matches!(&m.from, Endpoint::Interface(i) if i.as_str() == "world_cron")
        })
        .expect("an InterfaceArrival movement carries the cron tick into the run's inbox");
    assert!(
        arrival_into_run.artifact.is_some(),
        "the arrival movement carries the time artifact"
    );

    let _ = tokio::time::timeout(Duration::from_secs(10), life).await;
}
