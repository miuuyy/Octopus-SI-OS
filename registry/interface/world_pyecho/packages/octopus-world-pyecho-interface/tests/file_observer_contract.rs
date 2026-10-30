use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use octopus_descriptors::{InterfaceId, ProcessAuthoritySpec};
use octopus_interface_transport::process_authority::ProcessAuthoritySpecExecution;
use octopus_interface_transport::spawn_life_config_of;
use octopus_run_record::RunRecord;

#[test]
fn file_observer_starter_uses_the_existing_life_and_checkpoint_contracts() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../..");
    let registry = root.join("registry");
    let interface = InterfaceId::new("world_file_observer");
    let life = spawn_life_config_of(&registry, &interface)
        .expect("observer life parses")
        .expect("observer declares a life");
    assert_eq!(life.processes.len(), 1);
    let example: serde_json::Value = serde_json::from_slice(
        &std::fs::read(registry.join("interface/world_file_observer/examples/two_file_wait.json"))
            .expect("starter reads"),
    )
    .expect("starter is JSON");
    let run = RunRecord::from_bytes(
        &serde_json::to_vec(&example["runs/run_file_observer_example.json"]).unwrap(),
    )
    .expect("starter run satisfies runtime schema and checkpoint validation");
    assert_eq!(run.active_checkpoints.len(), 1);
    let checkpoint = &run.checkpoints[0];
    assert_eq!(checkpoint.required_verified_artifacts.len(), 2);
    assert_eq!(
        checkpoint.inbound_interface().unwrap().as_str(),
        "world_file_observer_example"
    );
    let ceiling =
        &example["registry/interface/system_worker_file_observer_example/policy.json"]["ceiling"];
    assert_eq!(ceiling["tools"], serde_json::json!([]));
    assert_eq!(ceiling["wek"], serde_json::json!([]));
    for requirement in &checkpoint.required_verified_artifacts {
        let value = serde_json::to_value(requirement).unwrap();
        let mark = value["mark"].as_str().unwrap();
        assert!(ceiling["mark_outcomes"]
            .as_array()
            .unwrap()
            .contains(&value["mark"]));
        assert_eq!(
            example[format!("registry/mark/{mark}/mark.json")]["id"],
            mark
        );
        assert_eq!(value["verifier"], "artifact_exists");
    }
    assert_eq!(
        example["registry/interface/world_file_observer_example/life.json"]["processes"][0]
            ["authority"]["read_paths"],
        example["registry/interface/world_file_observer_example/observer.json"]["read_roots"]
    );
}

#[test]
fn explicit_observer_directory_resolves_before_selected_file_exists() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../..");
    let fixture = tempfile::tempdir().unwrap();
    let scope = fixture.path().canonicalize().unwrap();
    let selected = scope.join("not-created.txt");
    assert!(!selected.exists());
    let output = Command::new("python3")
        .args([
            "-B",
            "-c",
            r#"
import json, sys
sys.path.insert(0, sys.argv[1])
from authoring import observer_package
package = observer_package("world_file_test", [
    {"id": "input", "path": sys.argv[3], "label": "file.input.observed", "max_bytes": 128}
], read_roots=[sys.argv[2]])
print(json.dumps(package))
"#,
        ])
        .arg(repository.join("registry/interface/world_file_observer"))
        .arg(&scope)
        .arg(&selected)
        .output()
        .expect("package authoring helper executes");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let package: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let authority: ProcessAuthoritySpec = serde_json::from_value(
        package["registry/interface/world_file_test/life.json"]["processes"][0]["authority"]
            .clone(),
    )
    .unwrap();
    assert_eq!(
        authority.read_paths,
        [scope.to_string_lossy().into_owned()].into()
    );
    assert!(authority.write_paths.is_empty());
    assert!(authority.write_root_slots.is_empty());
    assert!(authority.write_env_paths.is_empty());
    let environment = BTreeMap::from([("PATH".to_string(), std::env::var_os("PATH").unwrap())]);
    authority.resolve(&repository, &BTreeMap::new(), &environment, [], [], [], [])
        .expect("real process authority resolves the existing explicit directory despite absent selected file");
    assert!(
        !selected.exists(),
        "resolution must not create the selected file"
    );
    let mut rejected = authority;
    rejected.read_paths = [selected.to_string_lossy().into_owned()].into();
    assert!(rejected.resolve(&repository, &BTreeMap::new(), &environment, [], [], [], []).is_err(),
        "an absent exact file remains invalid read_paths authority; no compatibility path was added");
}
