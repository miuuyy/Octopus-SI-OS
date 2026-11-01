use std::path::Path;

use octopus_interfaces::InterfaceDescriptor;
use octopus_provider_claude_native::{ClaudeNativeConfig, verify_claude_grant, verify_sdk_child};
use serde_json::{Value, json};

fn declared() -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../interface.json");
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn parse(value: &Value) -> Result<ClaudeNativeConfig, String> {
    let descriptor: InterfaceDescriptor = octopus_descriptors::parse_strict(&serde_json::to_vec(value).unwrap()).unwrap();
    ClaudeNativeConfig::from_descriptor(&descriptor)
}

#[test]
fn the_declared_descriptor_is_accepted_and_drift_fails_closed() {
    let descriptor = declared();
    let config = parse(&descriptor).unwrap();
    assert_eq!(config.sdk_child.claude_executable, "claude");
    for (pointer, value, expected) in [
        ("/interface_driver_config/private_config/model", json!("claude-unknown"), "outside interface_policy"),
        ("/interface_driver_config/private_config/reasoning_effort", json!("ultra"), "reasoning_effort"),
        ("/interface_driver_config/private_config/sdk_child/protocol", json!("octopus.claude_sdk_child/2"), "protocol"),
        ("/interface_driver_config/private_config/sdk_child/executable", json!("interface://world_provider_claude_native/packages/../../escape"), "normal path"),
        ("/interface_driver_config/private_config/sdk_child/executable", json!("owner://packages/octopus-claude-sdk-child/bin/child"), "interface:// import"),
        ("/interface_driver_config/private_config/instruction_artifacts", json!(["a", "a"]), "unique"),
        ("/interface_driver_config/private_config/sdk_child/claude_executable", json!("bin/claude"), "bare command"),
    ] {
        let mut drifted = descriptor.clone();
        *drifted.pointer_mut(pointer).unwrap() = value;
        let error = parse(&drifted).unwrap_err();
        assert!(error.contains(expected), "{pointer}: {error}");
    }
    let mut unknown = descriptor.clone();
    unknown["interface_driver_config"]["private_config"]["opencodex"] = json!({});
    assert!(parse(&unknown).unwrap_err().contains("unknown field"));
}

#[test]
fn the_sdk_child_must_match_its_build_receipt() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let executable = bin.join("octopus-claude-sdk-child");
    std::fs::write(&executable, b"child bytes").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let receipt = |sha: &str| {
        std::fs::write(
            dir.path().join("build-receipt.json"),
            serde_json::to_vec(&json!({"schema_version":1,"executable":"bin/octopus-claude-sdk-child","executable_sha256":sha,"executable_bytes":11,"inputs":{}})).unwrap(),
        )
        .unwrap();
    };
    receipt("8f4e9a0b0c6ec1f7a2c1d3c3f0b5f43a3c6a3e1f0d7e3e2b8a7d6b5c4a3f2e1d");
    assert!(verify_sdk_child(&executable).unwrap_err().contains("does not attest"));
    use sha2::Digest;
    let digest: String = sha2::Sha256::digest(b"child bytes").iter().map(|byte| format!("{byte:02x}")).collect();
    receipt(&digest);
    verify_sdk_child(&executable).unwrap();
}

#[test]
fn the_claude_executable_must_be_granted_by_its_own_life() {
    let owner = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    verify_claude_grant(&owner, "claude").unwrap();
    let configured = "/opt/claude-code/bin/claude";
    assert!(verify_claude_grant(&owner, configured).unwrap_err().contains("authority.executable_paths"));
    let dir = tempfile::tempdir().unwrap();
    let mut life: Value = serde_json::from_slice(&std::fs::read(owner.join("life.json")).unwrap()).unwrap();
    life["processes"][0]["authority"]["executable_paths"] = json!([configured]);
    std::fs::write(dir.path().join("life.json"), serde_json::to_vec(&life).unwrap()).unwrap();
    verify_claude_grant(dir.path(), configured).unwrap();
    assert!(verify_claude_grant(dir.path(), "claude-other").unwrap_err().contains("authority.executable_commands"));
}
