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
        &mut writes,
    )
    .unwrap();
    let life: SpawnLifeConfig = serde_json::from_str(&writes[0].content).unwrap();
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
            .chain(&process.authority.write_env_paths)
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
