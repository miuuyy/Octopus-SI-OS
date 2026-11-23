use super::*;

#[test]
fn opencodex_account_composition_grants_only_the_connected_owner_directory() {
    let root = tempfile::tempdir().unwrap();
    let interface_root =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../../registry/interface");
    let source = "world_provider_antigravity";
    let provider: InterfaceDescriptor = serde_json::from_slice(
        &std::fs::read(interface_root.join(source).join("interface.json")).unwrap(),
    )
    .unwrap();
    let original = provider.interface_driver_config.unwrap();
    let private_root = root.path().join("interfaces");
    let directory = private_root.join(source).join("opencodex-account");
    std::fs::create_dir_all(&private_root).unwrap();
    let configure = |config: &mut octopus_interfaces::InterfaceDriverConfig| {
        configure_provider_owner_roots(config, source, "account_task", Some(&private_root))
    };
    assert!(configure(&mut original.clone())
        .err()
        .unwrap()
        .contains("requires a connected account"));
    assert!(!directory.exists());
    std::fs::create_dir_all(&directory).unwrap();
    let mut config = original.clone();
    configure_provider_private_auth_store(&mut config, source).unwrap();
    let slots = configure(&mut config).unwrap();
    assert_eq!(slots, [("provider_account_root", true)]);
    assert_eq!(
        config.host_slots["provider_account_root"].path,
        directory.canonicalize().unwrap()
    );
    assert!(config.host_slots["provider_account_root"]
        .additional_paths
        .is_empty());
    assert!(!config.host_slots.contains_key("provider_secret_owner_root"));
    let mut writes = Vec::new();
    plan_interface_life_with_root_slot_access(
        &interface_root,
        source,
        "account_task",
        &slots,
        None,
        &mut writes,
    )
    .unwrap();
    let life: SpawnLifeConfig = serde_json::from_str(&writes[0].content).unwrap();
    assert!(life.private_environment_root_slot.is_none());
    for process in life.processes {
        assert!(process
            .authority
            .read_root_slots
            .contains("provider_account_root"));
        assert!(process
            .authority
            .write_root_slots
            .contains("provider_account_root"));
        assert!(!process
            .inherit_env
            .iter()
            .any(|name| name.starts_with("OCTOPUS_MACHINE_CODEX_AUTH")));
        assert!(!process
            .authority
            .read_env_paths
            .iter()
            .chain(&process.authority.optional_read_env_paths)
            .chain(&process.authority.write_env_paths)
            .chain(&process.authority.optional_write_env_paths)
            .any(|name| name.starts_with("OCTOPUS_MACHINE_CODEX_AUTH")));
    }
    config
        .host_slots
        .get_mut("provider_account_root")
        .unwrap()
        .additional_paths
        .push(root.path().to_path_buf());
    assert!(configure(&mut config).err().unwrap().contains("conflicts"));
    let mut config = original.clone();
    config.residual_ambient.as_mut().unwrap()["private_auth_store"] =
        serde_json::json!({"auth_file_env":"OCTOPUS_MACHINE_CODEX_AUTH_FILE"});
    assert!(configure_provider_private_auth_store(&mut config, source)
        .err()
        .unwrap()
        .contains("cannot import"));
    let mut config = original;
    config.private_config["opencodex"]["credential_owner_interface"] =
        serde_json::json!("another_owner");
    assert!(configure(&mut config).err().unwrap().contains("must own"));
}

#[test]
fn opencodex_vertex_composition_grants_secret_owner_root_and_private_env_slot() {
    let root = tempfile::tempdir().unwrap();
    let interface_root =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../../registry/interface");
    let source = "world_provider_vertex";
    let provider: InterfaceDescriptor = serde_json::from_slice(
        &std::fs::read(interface_root.join(source).join("interface.json")).unwrap(),
    )
    .unwrap();
    let mut config = provider.interface_driver_config.unwrap();
    let private_root = root.path().join("interfaces");
    let slots = configure_provider_owner_roots(
        &mut config,
        source,
        "world_provider_vertex_task",
        Some(&private_root),
    )
    .unwrap();
    assert_eq!(slots, [("provider_secret_owner_root", false)]);
    assert_eq!(
        config.host_slots["provider_secret_owner_root"].path,
        private_root.join(source)
    );
    assert!(config.host_slots["provider_secret_owner_root"]
        .additional_paths
        .is_empty());

    let mut writes = Vec::new();
    plan_interface_life_with_root_slot_access(
        &interface_root,
        source,
        "world_provider_vertex_task",
        &slots,
        Some("provider_secret_owner_root"),
        &mut writes,
    )
    .unwrap();
    let life: SpawnLifeConfig = serde_json::from_str(&writes[0].content).unwrap();
    assert_eq!(
        life.private_environment_root_slot.as_deref(),
        Some("provider_secret_owner_root")
    );
    assert!(!life.processes.is_empty());
    for process in life.processes {
        assert!(process
            .authority
            .read_root_slots
            .contains("provider_secret_owner_root"));
        assert!(!process
            .authority
            .write_root_slots
            .contains("provider_secret_owner_root"));
        assert!(process
            .authority
            .read_env_paths
            .contains("OCTOPUS_PROVIDER_VERTEX_CREDENTIALS_FILE"));
    }
}

#[test]
fn world_provider_realtime_life_resolves_with_and_without_codex_auth_env() {
    use octopus_verifiers::process_authority::ProcessAuthoritySpecExecution;
    use std::collections::BTreeMap;
    use std::ffi::OsString;

    let _dir = tempfile::tempdir().unwrap();
    let root = _dir.path().canonicalize().unwrap();
    let root_slots: BTreeMap<String, PathBuf> = [("run_records_root", "runs"), ("interface_hand_in_root", "hand_in"), ("app_server_control_root", "control")]
        .into_iter().map(|(slot, dir)| { let p = root.join(dir); std::fs::create_dir_all(&p).unwrap(); (slot.to_string(), p) }).collect();
    let life_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../../registry/interface/world_provider_realtime/life.json");
    let life: SpawnLifeConfig = serde_json::from_slice(&std::fs::read(&life_path).unwrap()).unwrap();
    assert!(!life.processes.is_empty());

    let (auth_file, auth_lock) = (root.join(".codex/auth.json"), root.join(".codex/auth.lock"));
    let present_env = BTreeMap::from([
        ("OCTOPUS_MACHINE_CODEX_AUTH_FILE".to_string(), OsString::from(&auth_file)),
        ("OCTOPUS_MACHINE_CODEX_AUTH_LOCK_FILE".to_string(), OsString::from(&auth_lock)),
    ]);
    let absent_env = BTreeMap::new();
    let empty = || std::iter::empty::<PathBuf>();
    for process in &life.processes {
        let absent = process.authority.resolve(&root, &root_slots, &absent_env, empty(), empty(), empty(), empty()).unwrap();
        assert!(!absent.authority.writable().contains(&auth_file) && !absent.authority.writable().contains(&auth_lock));
        let present = process.authority.resolve(&root, &root_slots, &present_env, empty(), empty(), empty(), empty()).unwrap();
        assert!(present.authority.readable().contains(&auth_file) && present.authority.writable().contains(&auth_file) && present.authority.writable().contains(&auth_lock));
    }
}
