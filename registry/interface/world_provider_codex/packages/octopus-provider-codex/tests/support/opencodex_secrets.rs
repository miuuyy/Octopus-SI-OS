use super::*;

#[test]
fn opencodex_configuration_selects_the_bridge_at_the_toml_root() {
    let root = tempfile::tempdir().unwrap();
    let descriptor: InterfaceDescriptor = serde_json::from_str(include_str!(
        "../../../../../world_provider_openrouter/interface.json"
    ))
    .unwrap();
    let declared = descriptor_config(&descriptor).unwrap().unwrap();
    let home = build_curated_opencodex_home(
        root.path(),
        "http://127.0.0.1:12345/v1",
        declared.model_catalog.as_ref(),
    )
    .unwrap();
    let encoded = std::fs::read_to_string(home.join("config.toml")).unwrap();
    let config: toml::Value = toml::from_str(&encoded).unwrap();
    assert_eq!(config["model_provider"].as_str(), Some("octopus_opencodex"));
    assert!(config.get("model_context_window").is_none());
    assert!(config.get("model_auto_compact_token_limit").is_none());
    assert!(
        !config["agents"]
            .as_table()
            .unwrap()
            .contains_key("model_provider")
    );
    let provider = &config["model_providers"]["octopus_opencodex"];
    assert_eq!(
        provider["base_url"].as_str(),
        Some("http://127.0.0.1:12345/v1")
    );
    assert_eq!(provider["requires_openai_auth"].as_bool(), Some(false));
    assert_eq!(provider["request_max_retries"].as_integer(), Some(4));
    assert_eq!(provider["stream_max_retries"].as_integer(), Some(5));
    assert_eq!(config["features"]["shell_tool"].as_bool(), Some(false));
    let catalog: ModelsResponse = serde_json::from_slice(
        &std::fs::read(config["model_catalog_json"].as_str().unwrap()).unwrap(),
    )
    .unwrap();
    assert_eq!(catalog, declared.model_catalog.unwrap());
    let sonnet = catalog
        .models
        .iter()
        .find(|model| model.slug == "anthropic/claude-sonnet-5")
        .unwrap();
    assert_eq!(sonnet.context_window, Some(400000));
    assert_eq!(sonnet.max_context_window, Some(1000000));
    assert!(!sonnet.used_fallback_model_metadata);
}

#[test]
fn opencodex_reads_the_interface_local_store_for_owner_and_task_interfaces() {
    let root = tempfile::tempdir().unwrap();
    let owner = root.path().join("world_provider_openrouter");
    let scope = SecretScope::named("world_provider_openrouter").unwrap();
    let name = SecretName::new("OPENROUTER_API_KEY").unwrap();
    let store = SecretsManager::new_with_keyring_store(
        owner.clone(),
        SecretsBackendKind::Local,
        Arc::new(LocalFileKeyringStore::new(owner.join("secrets/keyring"))),
    );
    store
        .set(&scope, &name, "test-interface-private-key")
        .unwrap();
    let mut descriptor: InterfaceDescriptor = serde_json::from_str(include_str!(
        "../../../../../world_provider_openrouter/interface.json"
    ))
    .unwrap();
    let config = descriptor_config(&descriptor).unwrap().unwrap();
    let mut transport = descriptor
        .interface_driver_config
        .as_ref()
        .unwrap()
        .transport
        .clone()
        .unwrap();
    transport.auth_env_key = None;
    transport.auth_secret_ref =
        Some("secret://world_provider_openrouter/OPENROUTER_API_KEY".into());
    let (arrivals, _) = tokio::sync::mpsc::channel(1);
    let mut ctx = InterfaceLiftContext::unprivileged(
        descriptor.id.clone(),
        owner.clone(),
        None,
        owner.clone(),
        root.path().join("movements.jsonl"),
        root.path().join("blobs"),
        root.path().join("ledger-index.sqlite"),
        BTreeMap::from([("activation_secrets_home".into(), owner.clone())]),
        arrivals,
        Default::default(),
    );
    assert_eq!(
        resolve_api_key(&ctx, &descriptor, &transport, &config, &PrivateValues { owner: None })
            .unwrap()
            .as_deref(),
        Some("test-interface-private-key")
    );
    descriptor.id = octopus_descriptors::InterfaceId::new("world_provider_openrouter_task");
    ctx.root_slots.clear();
    ctx.root_slots
        .insert(PROVIDER_SECRET_OWNER_ROOT_SLOT.into(), owner.clone());
    assert_eq!(
        resolve_api_key(&ctx, &descriptor, &transport, &config, &PrivateValues { owner: None })
            .unwrap()
            .as_deref(),
        Some("test-interface-private-key")
    );
    let mut wrong_scope = transport.clone();
    wrong_scope.auth_secret_ref = Some("secret://world_provider_other/OPENROUTER_API_KEY".into());
    assert!(
        resolve_api_key(&ctx, &descriptor, &wrong_scope, &config, &PrivateValues { owner: None })
            .unwrap_err()
            .contains("scope must equal")
    );
    store.delete(&scope, &name).unwrap();
    assert!(
        resolve_api_key(&ctx, &descriptor, &transport, &config, &PrivateValues { owner: None })
            .unwrap_err()
            .contains("credential is missing")
    );
}

#[test]
fn opencodex_task_copy_reads_setup_values_from_its_credential_owner() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let owner = root.path().join("world_provider_custom");
    std::fs::create_dir(&owner).unwrap();
    std::fs::set_permissions(&owner, std::fs::Permissions::from_mode(0o700)).unwrap();
    let document = serde_json::json!({
        "schema_version": 1,
        "interface": "world_provider_custom",
        "values": {
            "OCTOPUS_PROVIDER_CUSTOM_API_KEY": "test-custom-key",
            "OCTOPUS_PROVIDER_CUSTOM_BASE_URL": "https://example.test/v1",
            "OCTOPUS_PROVIDER_CUSTOM_MODEL": "vendor/model-x"
        }
    });
    let environment = owner.join("environment.json");
    std::fs::write(&environment, serde_json::to_vec(&document).unwrap()).unwrap();
    std::fs::set_permissions(&environment, std::fs::Permissions::from_mode(0o600)).unwrap();
    let mut descriptor: InterfaceDescriptor = serde_json::from_str(include_str!(
        "../../../../../world_provider_custom/interface.json"
    ))
    .unwrap();
    descriptor.id = octopus_descriptors::InterfaceId::new("world_provider_custom_task");
    let config = descriptor_config(&descriptor).unwrap().unwrap();
    let transport = descriptor
        .interface_driver_config
        .as_ref()
        .unwrap()
        .transport
        .clone()
        .unwrap();
    let (arrivals, _) = tokio::sync::mpsc::channel(1);
    let task_dir = root.path().join("world_provider_custom_task");
    let ctx = InterfaceLiftContext::unprivileged(
        descriptor.id.clone(),
        task_dir.clone(),
        None,
        task_dir,
        root.path().join("movements.jsonl"),
        root.path().join("blobs"),
        root.path().join("ledger-index.sqlite"),
        BTreeMap::from([(PROVIDER_SECRET_OWNER_ROOT_SLOT.into(), owner.clone())]),
        arrivals,
        Default::default(),
    );
    let values = PrivateValues::resolve(&ctx, &descriptor, &transport, &config).unwrap();
    assert_eq!(
        resolve_model_aliases(&config, &["custom-model".to_string()], &values).unwrap(),
        BTreeMap::from([("custom-model".to_string(), "vendor/model-x".to_string())])
    );
    assert_eq!(
        resolve_base_url(&transport, config.allow_private_network, &values).unwrap(),
        "https://example.test/v1"
    );
    assert_eq!(
        resolve_api_key(&ctx, &descriptor, &transport, &config, &values)
            .unwrap()
            .as_deref(),
        Some("test-custom-key")
    );
    std::fs::set_permissions(&environment, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(PrivateValues::resolve(&ctx, &descriptor, &transport, &config).is_err());
}

#[test]
fn opencodex_claude_cli_keeps_its_sign_in_inside_the_cli() {
    let descriptor: InterfaceDescriptor = serde_json::from_str(include_str!(
        "../../../../../world_provider_claude_cli/interface.json"
    ))
    .unwrap();
    let declared = descriptor_config(&descriptor).unwrap().unwrap();
    assert_eq!(
        serde_json::to_value(&declared.adapter).unwrap(),
        "claude-cli"
    );
    assert_eq!(declared.bridge_env_names, ["HOME", "USER", "PATH", "LANG"]);
    let transport = descriptor
        .interface_driver_config
        .as_ref()
        .unwrap()
        .transport
        .as_ref()
        .unwrap();
    assert!(transport.auth_env_key.is_none() && transport.auth_secret_ref.is_none());
}

#[test]
fn opencodex_account_roots_stay_in_the_selected_home() {
    let root = tempfile::tempdir().unwrap();
    let mut descriptor: InterfaceDescriptor = serde_json::from_str(include_str!(
        "../../../../../world_provider_antigravity/interface.json"
    ))
    .unwrap();
    let config = descriptor_config(&descriptor).unwrap().unwrap();
    let owner = root
        .path()
        .join("interfaces")
        .join(&config.credential_owner_interface);
    let directory = owner.join("opencodex-account");
    std::fs::create_dir_all(&directory).unwrap();
    let (arrivals, _) = tokio::sync::mpsc::channel(1);
    let mut ctx = InterfaceLiftContext::unprivileged(
        descriptor.id.clone(),
        owner.clone(),
        None,
        owner.clone(),
        root.path().join("movements"),
        root.path().join("blobs"),
        root.path().join("index"),
        BTreeMap::new(),
        arrivals,
        Default::default(),
    );
    assert_eq!(
        resolve_account(&ctx, &descriptor, &config)
            .unwrap()
            .unwrap()
            .directory,
        directory.canonicalize().unwrap()
    );
    descriptor.id = octopus_descriptors::InterfaceId::new("account_task");
    ctx.interface = descriptor.id.clone();
    ctx.var_dir = root.path().join("interfaces/account_task");
    ctx.root_slots.clear();
    assert!(
        resolve_account(&ctx, &descriptor, &config)
            .err()
            .unwrap()
            .contains("unbound root slot")
    );
    ctx.root_slots
        .insert(PROVIDER_ACCOUNT_ROOT_SLOT.into(), directory.clone());
    assert_eq!(
        resolve_account(&ctx, &descriptor, &config)
            .unwrap()
            .unwrap()
            .directory,
        directory.canonicalize().unwrap()
    );
    let other = root
        .path()
        .join("other/interfaces")
        .join(&config.credential_owner_interface)
        .join("opencodex-account");
    std::fs::create_dir_all(&other).unwrap();
    ctx.root_slots
        .insert(PROVIDER_ACCOUNT_ROOT_SLOT.into(), other);
    assert!(
        resolve_account(&ctx, &descriptor, &config)
            .err()
            .unwrap()
            .contains("selected Home")
    );
    ctx.root_slots
        .insert(PROVIDER_ACCOUNT_ROOT_SLOT.into(), directory.clone());
    std::fs::remove_dir(&directory).unwrap();
    assert!(
        resolve_account(&ctx, &descriptor, &config)
            .err()
            .unwrap()
            .contains("connect the provider first")
    );
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&owner, &directory).unwrap();
        assert!(
            resolve_account(&ctx, &descriptor, &config)
                .err()
                .unwrap()
                .contains("regular owned directory")
        );
    }
    let driver = descriptor.interface_driver_config.as_mut().unwrap();
    driver.transport.as_mut().unwrap().auth_env_key =
        Some("OCTOPUS_MACHINE_CODEX_AUTH_FILE".into());
    assert!(
        descriptor_config(&descriptor)
            .err()
            .unwrap()
            .contains("only from its owned account store")
    );
}

#[test]
fn opencodex_web_search_names_the_backend_that_runs_it() {
    let mut descriptor: InterfaceDescriptor = serde_json::from_str(include_str!(
        "../../../../../world_provider_antigravity/interface.json"
    ))
    .unwrap();
    let search = descriptor_config(&descriptor).unwrap().unwrap().web_search.unwrap();
    assert_eq!((search.backend.as_str(), search.model.as_str()), ("gemini", "gemini-4-hydrogen"));
    descriptor
        .interface_driver_config
        .as_mut()
        .unwrap()
        .private_config
        .get_mut(OPENCODEX_CONFIG_KEY)
        .unwrap()
        .as_object_mut()
        .unwrap()
        .remove("web_search");
    assert!(descriptor_config(&descriptor)
        .unwrap_err()
        .contains("names no OpenCodex search backend"));
}
