#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use octopus_descriptors::InterfaceId;
use octopus_interface_transport::{
    interfaces_mentioned_in_live_runs, spawn_life_config_of, spawn_process_life,
    InterfaceLiftContext,
};
use octopus_interfaces::InterfaceDescriptor;
use tokio_util::sync::CancellationToken;

fn repo_registry_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../registry")
}

fn seed_bootstrap_ee_root(root: &Path) {
    std::fs::create_dir_all(root).expect("test EE instance root creates");
    std::fs::write(
        root.join("ee_existence.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "ee_id": "ee_existence",
            "template_name": "existence",
            "parent_ee_id": null,
            "goal": "existence",
        }))
        .expect("test bootstrap EE encodes"),
    )
    .expect("test installation seeds its bootstrap EE root");
}

fn copy_provider_folder_with_new_id(temp_registry: &Path, id: &str) {
    let source = repo_registry_root().join("interface/world_provider_codex/interface.json");
    let bytes = std::fs::read(&source).expect("the real provider interface.json reads");
    let mut descriptor: serde_json::Value =
        serde_json::from_slice(&bytes).expect("the real provider interface.json parses");
    descriptor["id"] = serde_json::Value::String(id.to_string());

    let dir = temp_registry.join("interface").join(id);
    std::fs::create_dir_all(&dir).expect("copied interface dir creates");
    std::fs::write(
        dir.join("interface.json"),
        serde_json::to_vec_pretty(&descriptor).expect("copied descriptor encodes"),
    )
    .expect("copied interface.json writes");

    let life = serde_json::json!({
        "processes": [{
            "bin": "/bin/bash",
            "io_limits": {
                "max_control_frame_bytes": 1048576,
                "max_stderr_bytes": 262144,
                "max_manifest_bytes": 1048576,
                "max_payload_bytes": 268435456
            },
            "args": [
                "-c",
                "printf '%s\\n%s\\n' \"$OCTOPUS_INTERFACE_ID\" \"$OCTOPUS_INTERFACE_DIR\" \
                 > \"$OCTOPUS_VAR_DIR/probe.txt\"; printf '{\"kind\":\"ready\"}\\n'; \
                 while IFS= read -r _; do :; done"
            ],
            "read_handles": [],
            "authority": {
                "read_root_slots": [],
                "write_root_slots": [],
                "executable_root_slots": [],
                "read_paths": ["."],
                "write_paths": [],
                "executable_paths": ["/bin/bash"],
                "executable_commands": [],
                "read_env_paths": [],
                "write_env_paths": [],
                "executable_env_paths": [],
                "allow_network": false,
                "allow_local_ipc": false
            }
        }]
    });
    std::fs::write(
        dir.join("life.json"),
        serde_json::to_vec_pretty(&life).expect("life sidecar encodes"),
    )
    .expect("copied life.json writes");
}

fn write_run_record_mentioning(runs_dir: &Path, run_id: &str, interfaces: &[&str]) {
    std::fs::create_dir_all(runs_dir).expect("runs dir creates");
    let checkpoints: Vec<serde_json::Value> = interfaces
        .iter()
        .enumerate()
        .map(|(i, interface)| {
            serde_json::json!({
                "id": format!("c{i}"),
                "run": run_id,
                "trace": format!("trace:{run_id}:c{i}"),
                "epoch": 0,
                "interface_binding": octopus_runtime::AuthoredInterfaceBinding::all(octopus_descriptors::InterfaceId::new(*interface)),
                "outbound_interface_bindings": [],
            })
        })
        .collect();
    let active_checkpoints: Vec<String> = (0..interfaces.len())
        .map(|index| format!("c{index}"))
        .collect();
    let bytes = serde_json::to_vec(&serde_json::json!({
        "id": run_id,
        "ee": "ee_test",
        "worker": "system_worker_test",
        "trace": format!("trace:{run_id}"),
        "status": "running",
        "interface_state": {},
        "checkpoints": checkpoints,
        "active_checkpoints": active_checkpoints,
    }))
    .expect("run record encodes");
    std::fs::write(runs_dir.join(format!("{run_id}.json")), bytes).expect("run record writes");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_provider_interface_folder_replicates_by_copy_and_an_id_field_edit() {
    let dir = tempfile::tempdir().expect("temp root");
    let temp_registry = dir.path().join("registry");
    let octopus_home = dir.path().join("home");
    let durable_root = dir.path().join("durable");
    let blobs_dir = durable_root.join("blobs");
    std::fs::create_dir_all(&blobs_dir).expect("durable blob root creates");

    let copy_a = "world_provider_codex_copy_a";
    let copy_b = "world_provider_codex_copy_b";
    copy_provider_folder_with_new_id(&temp_registry, copy_a);
    copy_provider_folder_with_new_id(&temp_registry, copy_b);

    let runs_dir = durable_root.join("runs");
    write_run_record_mentioning(&runs_dir, "run_twins", &[copy_a, copy_b]);
    let mentioned =
        interfaces_mentioned_in_live_runs(&runs_dir).expect("live run closure resolves");
    assert!(mentioned.contains(&InterfaceId::new(copy_a)));
    assert!(mentioned.contains(&InterfaceId::new(copy_b)));

    let loaded = octopus_descriptors::loader::load_kind::<InterfaceDescriptor>(
        &temp_registry.join("interface"),
    )
    .expect("both id-edited copies of the real provider descriptor load fail-closed");
    assert_eq!(loaded.len(), 2, "two sibling descriptors, one shape");

    let (arrivals_tx, _arrivals_rx) = tokio::sync::mpsc::channel(4);
    let cancel = CancellationToken::new();
    let mut lifted = Vec::new();
    for id in [copy_a, copy_b] {
        let interface = InterfaceId::new(id);
        let config = spawn_life_config_of(&temp_registry, &interface)
            .expect("the copied life.json parses")
            .expect("the copied folder declares a spawn life");
        let (ctx, ready) = InterfaceLiftContext::unprivileged(
            interface.clone(),
            temp_registry.join("interface").join(interface.as_str()),
            None,
            octopus_home.join("interfaces").join(interface.as_str()),
            durable_root.join("movements.jsonl"),
            durable_root.join("blobs"),
            durable_root.join("ledger-index.sqlite"),
            std::collections::BTreeMap::from([
                ("repo_root".to_string(), dir.path().to_path_buf()),
                (
                    "interface_life_install_root".to_string(),
                    dir.path().to_path_buf(),
                ),
            ]),
            arrivals_tx.clone(),
            cancel.child_token(),
        )
        .with_readiness_channel();
        let mut handle = tokio::spawn(spawn_process_life(ctx, config));
        match tokio::time::timeout(Duration::from_secs(20), ready).await {
            Ok(Ok(Ok(()))) => {}
            Ok(Ok(Err(error))) => panic!("copied interface life failed readiness: {error}"),
            Ok(Err(_)) => {
                let result = tokio::time::timeout(Duration::from_secs(1), &mut handle).await;
                panic!("copied interface life exited before readiness: {result:?}");
            }
            Err(_) => {
                handle.abort();
                panic!("copied interface life did not report readiness within 20 seconds");
            }
        }
        lifted.push(handle);
    }

    for id in [copy_a, copy_b] {
        let probe = octopus_home.join("interfaces").join(id).join("probe.txt");
        let text = std::fs::read_to_string(&probe).unwrap_or_else(|error| {
            panic!(
                "ready copied interface did not persist its probe at '{}': {error}",
                probe.display()
            )
        });
        let mut lines = text.lines();
        assert_eq!(
            lines.next(),
            Some(id),
            "the lifted process receives ITS OWN interface id through OCTOPUS_INTERFACE_ID"
        );
        let dir_line = lines.next().unwrap_or_default();
        assert!(
            dir_line.ends_with(&format!("interface/{id}")),
            "OCTOPUS_INTERFACE_DIR points at the copied folder itself, got '{dir_line}'"
        );
    }

    cancel.cancel();
    for handle in lifted {
        let result = tokio::time::timeout(Duration::from_secs(20), handle)
            .await
            .expect("the cancelled life ends within the timeout")
            .expect("the life task does not panic");
        assert!(result.is_ok(), "a cancelled spawn life ends cleanly");
    }
}

#[test]
fn a_run_mentioned_interface_with_no_registry_folder_is_data_only_for_the_life_switch() {
    let dir = tempfile::tempdir().expect("temp root");
    let temp_registry = dir.path().join("registry");
    std::fs::create_dir_all(temp_registry.join("interface")).expect("registry root creates");
    let runs_dir = dir.path().join("runs");
    write_run_record_mentioning(&runs_dir, "run_orphan", &["world_provider_codex_deleted"]);
    let mentioned =
        interfaces_mentioned_in_live_runs(&runs_dir).expect("live run closure resolves");
    assert!(mentioned.contains(&InterfaceId::new("world_provider_codex_deleted")));

    let missing = InterfaceId::new("world_provider_codex_deleted");
    let spawn = spawn_life_config_of(&temp_registry, &missing)
        .expect("a missing life sidecar is a VALID state, not a failure");
    assert!(
        spawn.is_none(),
        "a missing folder has no spawn life — the switch's data-only `continue`, never a crash"
    );
}

fn copy_registry_descriptors_without(temp_registry: &Path, exclude: &[&str]) {
    for kind in [
        "interface",
        "ee_template",
        "worker_environment_kind",
        "tool",
        "mark",
        "artifact",
    ] {
        let kind_root = repo_registry_root().join(kind);
        for entry in std::fs::read_dir(&kind_root).expect("registry kind dir reads") {
            let entry = entry.expect("registry kind entry reads");
            if !entry.file_type().expect("entry file type").is_dir() {
                continue;
            }
            let id = entry.file_name().to_string_lossy().to_string();
            if kind == "interface" && exclude.contains(&id.as_str()) {
                continue;
            }
            let source = entry.path().join(format!("{kind}.json"));
            if !source.exists() {
                continue;
            }
            let target_dir = temp_registry.join(kind).join(&id);
            std::fs::create_dir_all(&target_dir).expect("copied descriptor dir creates");
            std::fs::copy(&source, target_dir.join(format!("{kind}.json")))
                .expect("descriptor json copies byte-identically");
            if kind == "interface" {
                let policy = entry.path().join("policy.json");
                if policy.exists() {
                    std::fs::copy(&policy, target_dir.join("policy.json"))
                        .expect("worker pin policy sidecar copies byte-identically");
                }
            }
        }
    }
}

fn write_terminal_to_provider_run_record(runs_dir: &Path, run_id: &str, checkpoint: &str) {
    std::fs::create_dir_all(runs_dir).expect("runs dir creates");
    std::fs::write(
        runs_dir.join(format!("{run_id}.json")),
        serde_json::to_vec_pretty(&serde_json::json!({
            "id": run_id,
            "ee": "ee_existence",
            "worker": "system_worker_coder_max",
            "trace": format!("trace:{run_id}"),
            "status": "running",
            "interface_state": {},
            "checkpoints": [{
                "id": checkpoint,
                "run": run_id,
                "trace": format!("trace:{run_id}:{checkpoint}"),
                "epoch": 0,
                "interface_binding": octopus_runtime::AuthoredInterfaceBinding::all(octopus_descriptors::InterfaceId::new("world_operator_chat")),
                "outbound_interface_bindings": [octopus_runtime::AuthoredInterfaceBinding::all(octopus_descriptors::InterfaceId::new("world_provider_codex"))],
                "artifact_bindings": [],
            }],
            "active_checkpoints": [checkpoint],
        }))
        .expect("run record encodes"),
    )
    .expect("run record writes");
}

#[tokio::test]
async fn the_spine_restarts_green_with_the_provider_folder_gone_and_refuses_its_route_at_send_time()
{
    use octopus_app_server::{
        admit_interface_artifact_arrival, build_embedded_spine, InterfaceArtifactFile,
        SharedBlobHandle, SharedBlobs, SpineRootConfig,
    };
    use octopus_ledger::{
        DeliveryVerdict, Endpoint, InMemoryBlobs, InMemoryLog, LiveMovementLog, MovementKind,
        RefusedReason,
    };
    use octopus_runtime::RunId;
    use std::sync::Arc;

    let dir = tempfile::tempdir().expect("temp root");
    let temp_registry = dir.path().join("registry");
    copy_registry_descriptors_without(&temp_registry, &["world_provider_codex"]);

    let blobs: SharedBlobs = Arc::new(std::sync::Mutex::new(InMemoryBlobs::new()));
    let interface_artifact_root = dir.path().join("handler-interface-artifacts");
    seed_bootstrap_ee_root(&interface_artifact_root.join("ee_instances"));
    let embedded = build_embedded_spine(SpineRootConfig {
        registry_root: temp_registry.clone(),
        run: RunId::new("run_daemon"),
        blobs: blobs.clone(),
        interface_artifact_root: interface_artifact_root.clone(),
        run_records_root: interface_artifact_root.join("runs"),
        ee_instances_root: interface_artifact_root.join("ee_instances"),
    })
    .await
    .expect("the spine BOOTS GREEN with the provider folder physically gone (a coma, not a death)");

    write_terminal_to_provider_run_record(
        &interface_artifact_root.join("runs"),
        "run_daemon",
        "cp_boot_terminal_input",
    );
    let mut log = LiveMovementLog::new(InMemoryLog::new());
    let mut arrival_blobs = SharedBlobHandle::new(blobs.clone());
    let artifact_root = dir.path().join("interface-artifacts");
    let input = InterfaceArtifactFile::write(
        &artifact_root,
        "operator.message",
        br#"{"text":"route me to the absent provider"}"#,
    )
    .and_then(|artifact| artifact.capture_bounded(1_048_576, 268_435_456))
    .expect("terminal input artifact writes and crosses the captured hand-in boundary");

    let refusal = admit_interface_artifact_arrival(
        &embedded,
        &mut log,
        &mut arrival_blobs,
        &input,
        &octopus_descriptors::InterfaceId::new("world_operator_chat"),
    )
    .await
    .expect_err("the route to the absent world must refuse at send time");
    let refusal_text = refusal.to_string();
    assert!(
        refusal_text.contains("world_provider_codex"),
        "the send-time refusal names the absent interface: {refusal_text}"
    );
    assert!(
        matches!(
            refusal.verdict(),
            octopus_interfaces::DeliveryVerdict::Refused {
                reason: octopus_interfaces::RefusedReason::UnknownInterface,
                ..
            }
        ),
        "the hand-in gets a synchronous typed verdict (a syscall return), refusing on the absent provider world: {:?}",
        refusal.verdict()
    );

    let refusal_rows = octopus_ledger::MovementLog::read_from(&log, octopus_ledger::Seq(0));
    let delivery_refusals: Vec<_> = refusal_rows
        .iter()
        .filter(|m| m.kind == MovementKind::DeliveryRefused)
        .collect();
    assert_eq!(
        delivery_refusals.len(),
        1,
        "a send-time refusal records EXACTLY ONE DeliveryRefused movement (a red edge in the topology), not a silent stderr warning"
    );
    let refused = delivery_refusals[0];
    let (refusal_reason, refusal_detail) = match refused
        .invocation_metadata
        .as_ref()
        .and_then(|metadata| metadata.delivery_verdict())
    {
        Some(DeliveryVerdict::Refused { reason, detail }) => (*reason, detail.to_string()),
        other => panic!("DeliveryRefused movement has no typed refused verdict: {other:?}"),
    };
    assert_eq!(
        refusal_reason,
        RefusedReason::UnknownInterface,
        "the DeliveryRefused verdict rides invocation_metadata, reason=unknown_interface: {refused:?}"
    );
    assert!(
        refusal_detail.contains("world_provider_codex"),
        "the red edge names the absent provider world in its detail: {refusal_detail}"
    );
    assert!(
        !refusal_rows.iter().any(|m| matches!(
            m.kind,
            MovementKind::InterfaceArrival | MovementKind::InterfaceOutbound
        )),
        "the refused hand-in leaves NO arrival/outbound movement — the checkpoint stays open, only the red refusal edge is drawn"
    );

    let restored_dir = temp_registry.join("interface/world_provider_codex");
    std::fs::create_dir_all(&restored_dir).expect("restored provider dir creates");
    std::fs::copy(
        repo_registry_root().join("interface/world_provider_codex/interface.json"),
        restored_dir.join("interface.json"),
    )
    .expect("the real provider descriptor restores byte-identically");

    let admitted = admit_interface_artifact_arrival(
        &embedded,
        &mut log,
        &mut arrival_blobs,
        &input,
        &octopus_descriptors::InterfaceId::new("world_operator_chat"),
    )
    .await
    .expect("after restore the SAME checkpoint admits — the limb woke up, nothing was rebooted");
    assert!(
        admitted.admission.first_outbound_artifact().is_some(),
        "the restored route records the outbound transfer to the provider"
    );
    assert!(
        octopus_ledger::MovementLog::read_from(&log, octopus_ledger::Seq(0)).iter().any(|m| {
            m.kind == MovementKind::InterfaceOutbound
                && matches!(&m.to, Endpoint::Interface(i) if i.as_str() == "world_provider_codex")
        }),
        "the outbound transfer movement is addressed to the provider interface folder"
    );
}

#[tokio::test]
async fn the_spine_boots_green_over_a_mid_authored_interface_folder() {
    use octopus_app_server::{build_embedded_spine, SharedBlobs, SpineRootConfig};
    use octopus_ledger::InMemoryBlobs;
    use octopus_runtime::RunId;
    use std::sync::Arc;

    let dir = tempfile::tempdir().expect("temp root");
    let temp_registry = dir.path().join("registry");
    copy_registry_descriptors_without(&temp_registry, &[]);
    let mid_authored = temp_registry.join("interface/world_being_authored");
    std::fs::create_dir_all(&mid_authored).expect("mid-authored interface dir creates");
    std::fs::write(mid_authored.join("notes.md"), b"descriptor json lands next")
        .expect("non-descriptor sibling file writes");

    let blobs: SharedBlobs = Arc::new(std::sync::Mutex::new(InMemoryBlobs::new()));
    let interface_artifact_root = dir.path().join("handler-interface-artifacts");
    seed_bootstrap_ee_root(&interface_artifact_root.join("ee_instances"));
    let embedded = build_embedded_spine(SpineRootConfig {
        registry_root: temp_registry,
        run: RunId::new("run_daemon"),
        blobs,
        interface_artifact_root: interface_artifact_root.clone(),
        run_records_root: interface_artifact_root.join("runs"),
        ee_instances_root: interface_artifact_root.join("ee_instances"),
    })
    .await
    .expect("the spine BOOTS GREEN while a sibling interface folder is mid-authored");
    assert!(
        !embedded
            .interfaces_loaded
            .iter()
            .any(|(_, desc)| desc.id.as_str() == "world_being_authored"),
        "the json-less dir is not an entry until its descriptor lands"
    );
}

#[tokio::test]
async fn the_spine_restarts_green_with_the_operator_chat_folder_gone() {
    use octopus_app_server::{build_embedded_spine, SharedBlobs, SpineRootConfig};
    use octopus_ledger::InMemoryBlobs;
    use octopus_runtime::RunId;
    use std::sync::Arc;

    let dir = tempfile::tempdir().expect("temp root");
    let temp_registry = dir.path().join("registry");
    copy_registry_descriptors_without(&temp_registry, &["world_operator_chat"]);

    let blobs: SharedBlobs = Arc::new(std::sync::Mutex::new(InMemoryBlobs::new()));
    let interface_artifact_root = dir.path().join("handler-interface-artifacts");
    seed_bootstrap_ee_root(&interface_artifact_root.join("ee_instances"));
    let embedded = build_embedded_spine(SpineRootConfig {
        registry_root: temp_registry,
        run: RunId::new("run_daemon"),
        blobs,
        interface_artifact_root: interface_artifact_root.clone(),
        run_records_root: interface_artifact_root.join("runs"),
        ee_instances_root: interface_artifact_root.join("ee_instances"),
    })
    .await
    .expect("the spine BOOTS GREEN with the operator chat folder physically gone");

    assert!(!embedded
        .interfaces_loaded
        .iter()
        .any(|(_, interface)| interface.id.as_str() == "world_operator_chat"));
}
