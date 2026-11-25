#![cfg(any(target_os = "macos", target_os = "linux"))]

use octopus_descriptors::{ProcessAuthoritySpec, ProcessSandboxMode};
use octopus_verifiers::process_authority::ProcessAuthoritySpecExecution;
use std::{collections::BTreeMap, path::Path};

#[test]
fn descriptor_mode_controls_os_enforcement_without_relaxing_validation() {
    let root = tempfile::tempdir().unwrap();
    let profiles = tempfile::tempdir().unwrap();
    let mkdir = if Path::new("/bin/mkdir").exists() {
        "/bin/mkdir"
    } else {
        "/usr/bin/mkdir"
    };
    #[cfg(target_os = "macos")]
    let authority_available = true;
    #[cfg(target_os = "linux")]
    let authority_available =
        Path::new(octopus_verifiers::process_authority::linux::CONTROL_SOCKET).exists();

    for mode in [None, Some("enforced"), Some("disabled")] {
        let mut json = serde_json::to_value(ProcessAuthoritySpec::default()).unwrap();
        json.as_object_mut().unwrap().remove("sandbox_mode");
        if let Some(mode) = mode {
            json["sandbox_mode"] = mode.into();
        }
        let spec: ProcessAuthoritySpec = serde_json::from_value(json).unwrap();
        let disabled = spec.sandbox_mode == ProcessSandboxMode::Disabled;
        let target = root.path().join(mode.unwrap_or("default"));
        let resolved = spec
            .resolve(
                root.path(),
                &BTreeMap::new(),
                &BTreeMap::new(),
                [],
                [],
                [],
                [],
            )
            .unwrap();
        let prepared = resolved.authority.prepare(
            mkdir,
            &[target.clone().into_os_string()],
            root.path(),
            None,
            profiles.path(),
        );
        if !disabled && !authority_available {
            let err = prepared.expect_err("enforced mode must fail closed without control socket");
            assert!(
                err.contains("Linux process authority is unavailable"),
                "unexpected error for {mode:?}: {err}"
            );
        } else {
            let process = prepared.unwrap_or_else(|err| panic!("{mode:?} prepare failed: {err}"));
            #[cfg(target_os = "macos")]
            assert_eq!(process.program == process.executable, disabled);
            let result = process
                .command()
                .env_clear()
                .envs(&process.environment)
                .current_dir(root.path())
                .output()
                .unwrap();
            assert_eq!(result.status.success(), disabled, "{mode:?}: {result:?}");
            assert_eq!(target.exists(), disabled, "{mode:?}");
        }
        let invalid = ProcessAuthoritySpec {
            read_root_slots: ["missing".into()].into(),
            ..spec
        };
        assert!(
            invalid
                .resolve(
                    root.path(),
                    &BTreeMap::new(),
                    &BTreeMap::new(),
                    [],
                    [],
                    [],
                    []
                )
                .is_err()
        );
    }
    for invalid in [
        serde_json::json!(null),
        serde_json::json!(false),
        serde_json::json!("none"),
    ] {
        let mut json = serde_json::to_value(ProcessAuthoritySpec::default()).unwrap();
        json["sandbox_mode"] = invalid;
        assert!(serde_json::from_value::<ProcessAuthoritySpec>(json).is_err());
    }
}
