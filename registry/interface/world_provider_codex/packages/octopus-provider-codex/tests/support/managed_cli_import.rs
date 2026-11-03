use super::*;

#[test]
fn managed_cli_import_resolves_the_declared_owner_and_checks_exact_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("interface");
    let owner = root.join("world_provider_codex/packages/bin");
    std::fs::create_dir_all(&owner).unwrap();
    std::fs::create_dir_all(root.join("world_provider_chatgpt/packages/bin")).unwrap();
    let binary = owner.join("codex");
    let bytes = b"managed provider executable";
    let license = b"upstream license fixture";
    std::fs::write(&binary, bytes).unwrap();
    std::fs::write(owner.join("LICENSE"), license).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let helper = owner.join("codex-code-mode-host");
    std::fs::copy(&binary, &helper).unwrap();
    let receipt = ManagedCliReceipt {
        schema_version: 2,
        version: "0.166.0".into(),
        release_url: "https://github.com/openai/codex/releases/tag/rust-v0.166.0".into(),
        archive_sha256: "a".repeat(64),
        executable: "codex".into(),
        executable_sha256: format!("{:x}", Sha256::digest(bytes)),
        executable_bytes: bytes.len() as u64,
        schema_baseline: "rust-v0.166.0".into(),
        schemas: std::collections::BTreeMap::from([(
            "TurnSteerParams.json".into(),
            "b".repeat(64),
        )]),
        legal: std::collections::BTreeMap::from([(
            "LICENSE".into(),
            format!("{:x}", Sha256::digest(license)),
        )]),
        companions: std::collections::BTreeMap::from([(
            "codex-code-mode-host".into(),
            ManagedCliCompanion {
                archive_sha256: "c".repeat(64),
                executable: "codex-code-mode-host".into(),
                executable_sha256: format!("{:x}", Sha256::digest(bytes)),
                executable_bytes: bytes.len() as u64,
                signing: None,
            },
        )]),
        signing: None,
    };
    std::fs::write(
        owner.join(MANAGED_CLI_RECEIPT),
        serde_json::to_vec(&receipt).unwrap(),
    )
    .unwrap();
    let import = ManagedCliImport {
        executable: "interface://world_provider_codex/packages/bin/codex".into(),
    };
    assert_eq!(
        import.resolve_in(&root).unwrap(),
        std::fs::canonicalize(&binary).unwrap()
    );
    for reference in [
        "owner://packages/bin/codex",
        "interface://world_provider_codex/../outside/codex",
        "interface://world_provider_chatgpt/packages/bin/codex",
    ] {
        assert!(ManagedCliImport {
            executable: reference.into()
        }
        .resolve_in(&root)
        .is_err());
    }
    std::fs::remove_file(&helper).unwrap();
    assert!(import.resolve_in(&root).is_err());
    std::fs::copy(&binary, &helper).unwrap();
    std::fs::write(&helper, b"corrupted companion").unwrap();
    assert!(import.resolve_in(&root).is_err());
    std::fs::copy(&binary, &helper).unwrap();
    std::fs::write(&binary, b"corrupted provider executable").unwrap();
    assert!(import.resolve_in(&root).is_err());
}
