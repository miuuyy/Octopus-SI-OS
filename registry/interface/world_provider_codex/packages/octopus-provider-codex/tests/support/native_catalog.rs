#[test]
fn current_run_catalogs_match_the_typed_authoring_request() {
    use octopus_operator_chat_client::run_composition::current_run::{
        AuthoredCurrentRun, CurrentRunRequest, CurrentRunSelection,
    };
    let before = serde_json::json!({
        "id":"run-a","ee":"ee-a","worker":"worker-a","trace":"trace-a",
        "status":"running","checkpoints":[],"active_checkpoints":[],
    });
    let requests = [
        CurrentRunRequest { change: None,
            publications:vec![],
            amendment_key:"attach".into(), authored:None,
            selection:Some(CurrentRunSelection {
                selected_tools:vec![], selected_worker_environment_kinds:vec![],
                door_host_slots:BTreeMap::new(),
                external_inputs:vec![octopus_operator_chat_client::ExternalInputSeed {
                    checkpoint_id:"cp_scheduled".into(), interface_id:"world_trigger".into(),
                    artifact_names:vec!["cron.current_time.0123456789abcdef01234567".into()],
                    required_verified_artifacts:vec![],
                }],
                external_relays:vec![],
                model_outputs:vec![], context_artifact_bindings:vec![], payload_artifact_bindings:vec![],
            }),
        },
        CurrentRunRequest { change: None,
            publications:vec![],
            amendment_key:"attach".into(), selection:None,
            authored:Some(AuthoredCurrentRun {
                expected_before_hash:octopus_ledger::ContentHash::of_bytes(
                    &serde_json::to_vec(&before).unwrap(),
                ).to_hex(),
                patch:serde_json::from_value(serde_json::json!([{"op":"test","path":"/id","value":before["id"]}])).unwrap(), writes:vec![],
            }),
        },
    ];
    for owner in ["world_provider_codex", "world_provider_chatgpt"] {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../..").join(owner).join("interface.json");
        let descriptor:serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let profile = &descriptor["interface_driver_config"]["private_config"]["run_affordance_profile"];
        assert_eq!(profile["file_read_tool"], "file.read");
        for operation in ["preview_current_run", "amend_current_run"] {
            let entry = profile["catalog"]["operations"].as_array().unwrap().iter()
                .find(|entry| entry["operation"] == operation).unwrap();
            let validator = octopus_tool_registry::ToolArgumentValidator::compile(&entry["input_schema"]).unwrap();
            for request in &requests {
                let value = serde_json::to_value(request).unwrap();
                validator.validate(&value).unwrap();
                serde_json::from_value::<CurrentRunRequest>(value.clone()).unwrap();
                let mode = if request.selection.is_some() { "selection" } else { "authored" };
                for field in value[mode].as_object().unwrap().keys() {
                    let mut incomplete = value.clone();
                    incomplete[mode].as_object_mut().unwrap().remove(field);
                    assert!(validator.validate(&incomplete).is_err(), "{owner} {operation} permits missing {mode}.{field}");
                    assert!(serde_json::from_value::<CurrentRunRequest>(incomplete).is_err());
                }
                let mut ambiguous = value;
                let other = if mode == "selection" { "authored" } else { "selection" };
                ambiguous[other] = serde_json::json!({});
                assert!(validator.validate(&ambiguous).is_err());
            }
            let change = serde_json::json!({"amendment_key":"attach","publications":[],
                "change":{"remove":{"selected_tools":[{"tool":"file.read","interface":"system_room","worker_environment_kind":"system_room"}]}}});
            validator.validate(&change).unwrap();
            assert!(serde_json::from_value::<CurrentRunRequest>(change.clone()).unwrap().change.is_some());
            let mut ambiguous = change;
            ambiguous["selection"] = serde_json::to_value(&requests[0]).unwrap()["selection"].clone();
            assert!(validator.validate(&ambiguous).is_err(), "{owner} {operation} permits a change beside a selection");
        }
    }
}

#[test]
fn create_run_starts_a_task_from_title_and_text_alone() {
    for owner in ["world_provider_codex", "world_provider_chatgpt"] {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..").join(owner).join("interface.json");
        let descriptor:serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let operations = descriptor["interface_driver_config"]["private_config"]["run_affordance_profile"]["catalog"]["operations"].as_array().unwrap();
        let entry = operations.iter().find(|entry| entry["operation"] == "create_run").unwrap();
        let validator = octopus_tool_registry::ToolArgumentValidator::compile(&entry["input_schema"]).unwrap();
        let request = serde_json::json!({"title":"Weather widget","text":"Refresh the Kyiv weather widget every hour."});
        validator.validate(&request).unwrap();
        let typed: crate::run_affordances::CreateRunRequest = serde_json::from_value(request).unwrap();
        assert!(typed.creation_key.is_none() && typed.authored.is_none());
        assert!(validator.validate(&serde_json::json!({"title":"No assignment"})).is_err(), "{owner} create_run admits a task without an assignment");
    }
}
