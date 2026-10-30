use std::collections::BTreeMap;

use octopus_worker_env::{
    AbsPath, EnvContext, WorkerEnvCaps, WorkerEnvKindRegistry, WorkerEnvironmentKindId,
};

fn workspace_wek_json() -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "worker_environment_kind_id": "filesystem_workspace",
        "substrate_family": "process_exec",
        "door_driver_refs": ["system_workspace_env"],
        "tool_process_bins": ["target/debug/octopus-workspace-fs-tool"],
        "worker_consent": "explicit_ceiling",
        "supported_executor_kinds": ["exec"],
        "capabilities": ["filesystem", "process_exec"],
        "boundary_defaults": {
            "read_root_refs": [
                { "path": "workspace_root", "access": ["read"] },
                { "path": "host_filesystem_root", "access": ["read"] }
            ],
            "write_root_refs": [
                { "path": "workspace_root", "access": ["read", "write"] },
                { "path": "host_filesystem_root", "access": ["read", "write"] }
            ],
            "protected_read_only_subpaths": [],
            "network_ceiling": "denied"
        },
        "lifecycle": { "requires_release": false }
    }))
    .unwrap()
}

#[cfg(unix)]
#[tokio::test]
async fn direct_workspace_room_supports_non_git_roots_and_refuses_symlink_escape() {
    use std::os::unix::fs::symlink;

    let workspace = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let secondary = tempfile::tempdir().unwrap();
    let secondary_root = secondary.path().canonicalize().unwrap();
    let workspace_root = workspace.path().canonicalize().unwrap();
    let outside_root = outside.path().canonicalize().unwrap();
    std::fs::write(outside_root.join("secret.txt"), b"private\n").unwrap();
    symlink(&outside_root, workspace_root.join("escape")).unwrap();

    let mut registry = WorkerEnvKindRegistry::default();
    registry.upsert(&workspace_wek_json()).unwrap();
    let kind_id = WorkerEnvironmentKindId::from("filesystem_workspace");
    let (spec, descriptor_hash) = registry.get(&kind_id).unwrap();
    let context = EnvContext {
        additional_root_slots: BTreeMap::from([("workspace_root".into(), vec![AbsPath::new(&secondary_root).unwrap()])]),
        root_slots: BTreeMap::from([(
            "workspace_root".to_string(),
            AbsPath::new(&workspace_root).unwrap(),
        )]),
        env: BTreeMap::new(),
    };
    let caps = WorkerEnvCaps::form(&spec, &context.provided_root_bindings(), None);
    let environment = super::direct_workspace_driver()
        .open(
            &spec,
            descriptor_hash,
            caps,
            context,
            octopus_worker_env::WorkerEnvironmentId::derive(b"direct-workspace-contract"),
        )
        .await
        .expect("a canonical non-Git directory opens as its own worker environment");
    assert!(!workspace_root.join(".git").exists());
    assert!(
        environment
            .sandbox()
            .writable_roots()
            .iter()
            .any(|root| root.as_path() == workspace_root)
    );

    let fs = environment.fs();
    let secondary_file = AbsPath::new(&secondary_root.join("created.txt")).unwrap();
    fs.write(&secondary_file, b"second folder").await.unwrap();
    assert_eq!(fs.read(&secondary_file).await.unwrap(), b"second folder");
    let output = crate::run_sandboxed_in_room(
        environment.sandbox(), octopus_descriptors::ProcessSandboxMode::Enforced,
        &AbsPath::new(&workspace_root).unwrap(),
        vec!["/bin/sh".into(), "-c".into(), "pwd -P; cat \"$1\"".into(), "multi-root".into(), secondary_file.as_str().into()],
        std::time::Duration::from_secs(5), 4096, &std::sync::atomic::AtomicBool::new(false),
    ).unwrap();
    assert_eq!(output.exit_code, 0, "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(String::from_utf8(output.stdout).unwrap(), format!("{}\nsecond folder", workspace_root.display()));

    let created = AbsPath::new(&workspace_root.join("nested/created.txt")).unwrap();
    let created_receipt = fs.write(&created, b"workspace\n").await.unwrap();
    assert_eq!(created_receipt.previous_content.unwrap(), None);
    let edited_receipt = fs.write(&created, b"workspace\n").await.unwrap();
    assert_eq!(
        edited_receipt.previous_content.unwrap(),
        Some(b"workspace\n".to_vec())
    );
    std::fs::write(created.as_path(), vec![b'x'; 1024 * 1024 + 1]).unwrap();
    assert!(fs.write(&created, b"workspace\n").await.unwrap().previous_content.is_err());
    assert_eq!(fs.read(&created).await.unwrap(), b"workspace\n");

    let escaped = AbsPath::new(&workspace_root.join("escape/secret.txt")).unwrap();
    assert!(fs.read(&escaped).await.is_err());
    let patch = "*** Begin Patch\n*** Add File: escape/created.txt\n+escaped\n*** End Patch\n";
    assert!(fs.apply_patch(patch).await.is_err());
    assert!(!outside_root.join("created.txt").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn direct_workspace_room_admits_an_explicit_additional_filesystem_root() {
    use std::os::unix::fs::symlink;

    let host = tempfile::tempdir().unwrap();
    let host_root = host.path().canonicalize().unwrap();
    let workspace_root = host_root.join("workspace");
    let outside_root = host_root.join("outside");
    std::fs::create_dir_all(&workspace_root).unwrap();
    std::fs::create_dir_all(&outside_root).unwrap();
    std::fs::write(outside_root.join("secret.txt"), b"granted\n").unwrap();
    symlink(&outside_root, workspace_root.join("linked-outside")).unwrap();

    let mut registry = WorkerEnvKindRegistry::default();
    registry.upsert(&workspace_wek_json()).unwrap();
    let kind_id = WorkerEnvironmentKindId::from("filesystem_workspace");
    let (spec, descriptor_hash) = registry.get(&kind_id).unwrap();
    let context = EnvContext {
        additional_root_slots: Default::default(),
        root_slots: BTreeMap::from([
            (
                "workspace_root".to_string(),
                AbsPath::new(&workspace_root).unwrap(),
            ),
            (
                "host_filesystem_root".to_string(),
                AbsPath::new(&host_root).unwrap(),
            ),
        ]),
        env: BTreeMap::new(),
    };
    let caps = WorkerEnvCaps::form(&spec, &context.provided_root_bindings(), None);
    let environment = super::direct_workspace_driver()
        .open(
            &spec,
            descriptor_hash,
            caps,
            context,
            octopus_worker_env::WorkerEnvironmentId::derive(b"direct-workspace-expanded-contract"),
        )
        .await
        .expect("an explicitly granted additional root forms one direct workspace room");
    let fs = environment.fs();
    let outside_file = AbsPath::new(&outside_root.join("created.txt")).unwrap();
    fs.write(&outside_file, b"outside but granted\n")
        .await
        .unwrap();
    assert_eq!(
        fs.read(&outside_file).await.unwrap(),
        b"outside but granted\n"
    );
    let linked_file = AbsPath::new(&workspace_root.join("linked-outside/secret.txt")).unwrap();
    assert_eq!(fs.read(&linked_file).await.unwrap(), b"granted\n");
}
