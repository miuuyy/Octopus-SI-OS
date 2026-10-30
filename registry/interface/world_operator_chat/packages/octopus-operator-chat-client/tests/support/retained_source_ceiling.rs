use super::*;

const RELAY: &str = "system_worker_memory_source_test";
const WORKER: &str = "system_worker_operator_home_test";

fn identity() -> PlannedIdentity {
    PlannedIdentity {
        voice: None,
        native_communication: None,
        chat_id: "home_test".to_string(),
        run_id: "run_operator_chat_home_test".to_string(),
        ee_id: "ee_existence".to_string(),
        chat_interface_id: "world_operator_chat_home_test".to_string(),
        model_interface_id: "world_provider_home_test".to_string(),
        worker_interface_id: WORKER.to_string(),
        authoring_source_id: "system_interface_authoring_home_test".to_string(),
        authoring_environment_id: "system_room_authoring_home_test".to_string(),
        code_mode_owners: BTreeMap::new(),
        surface_role: "home".to_string(),
        provider_choice_interface: "world_provider_test".to_string(),
        worker_choice_interface: "system_worker_operator_home".to_string(),
        capability_doors: BTreeMap::new(),
        capability_tools: BTreeMap::new(),
        capability_tool_sources: BTreeMap::new(),
        authoring_tool_id: "file.write__authoring_home_test".to_string(),
    }
}

fn registry(relay_in_ceiling: bool) -> (tempfile::TempDir, Value) {
    let dir = tempfile::tempdir().unwrap();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../system_worker_memory_observer");
    let mut relay: Value = serde_json::from_slice(&std::fs::read(source.join("interface.json")).unwrap()).unwrap();
    relay["id"] = serde_json::json!(RELAY);
    let mut policy: Value = serde_json::from_slice(&std::fs::read(source.join("policy.json")).unwrap()).unwrap();
    policy["id"] = serde_json::json!(WORKER);
    policy["ceiling"]["interfaces"] = if relay_in_ceiling {
        serde_json::json!(["world_provider_home_test", RELAY])
    } else {
        serde_json::json!(["world_provider_home_test"])
    };
    for (owner, file, value) in [(RELAY, "interface.json", &relay), (WORKER, "policy.json", &policy)] {
        let folder = dir.path().join("interface").join(owner);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join(file), serde_json::to_vec_pretty(value).unwrap()).unwrap();
    }
    (dir, policy)
}

#[test]
fn an_unchanged_ceiling_that_already_holds_the_relay_is_retained_without_a_write() {
    let (dir, _) = registry(true);
    let mut writes: Vec<PlannedClientWrite> = Vec::new();
    retain_source_ceiling(dir.path(), &identity(), RELAY, &mut writes).unwrap();
    assert!(writes.is_empty(), "an unchanged policy must not gain a planned write");
}

#[test]
fn a_relay_outside_the_existing_ceiling_fails_closed() {
    let (dir, _) = registry(false);
    let mut writes: Vec<PlannedClientWrite> = Vec::new();
    let error = retain_source_ceiling(dir.path(), &identity(), RELAY, &mut writes).unwrap_err();
    assert!(error.contains("outside its existing worker ceiling"), "{error}");
}

#[test]
fn a_planned_ceiling_write_still_receives_the_relay() {
    let (dir, mut policy) = registry(true);
    policy["ceiling"]["interfaces"] = serde_json::json!(["world_provider_home_test"]);
    let mut writes = vec![PlannedClientWrite {
        path: dir.path().join("interface").join(WORKER).join("policy.json"),
        content: serde_json::to_string(&policy).unwrap(),
        tag: format!("policy:{WORKER}"),
        expected_before_hash: None,
    }];
    retain_source_ceiling(dir.path(), &identity(), RELAY, &mut writes).unwrap();
    let planned: Value = serde_json::from_str(&writes[0].content).unwrap();
    assert!(planned["ceiling"]["interfaces"].as_array().unwrap().contains(&serde_json::json!(RELAY)));
}
