use octopus_operator_chat_client::{SelectedToolSeed, run_composition::*};
use octopus_run_record::RunRecord;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

pub fn write(root: &Path, relative: &str, value: Value) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
}

pub fn client_fixture(root: &Path) -> RunCompositionRequest {
    let mut request = fixture(root);
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../../..");
    let declared: Value = serde_json::from_slice(
        &std::fs::read(source.join("registry/interface/world_provider_codex/interface.json"))
            .unwrap(),
    )
    .unwrap();
    let provider_file = "registry/interface/world_provider_fixture/interface.json";
    let mut provider: Value =
        serde_json::from_slice(&std::fs::read(root.join(provider_file)).unwrap()).unwrap();
    for field in ["context_output_labels", "session_snapshot_labels"] {
        provider["interface_driver_config"]["private_config"][field] =
            declared["interface_driver_config"]["private_config"][field].clone();
    }
    write(root, provider_file, provider);
    for name in ["interface.json", "life.json", "client.json"] {
        let mut value: Value = serde_json::from_slice(
            &std::fs::read(
                source
                    .join("registry/interface/world_operator_chat")
                    .join(name),
            )
            .unwrap(),
        )
        .unwrap();
        if name == "client.json" {
            value["run"]["context_artifact_bindings"] = json!([]);
            value["run"].as_object_mut().unwrap().remove("maintenance_observer");
        }
        write(
            root,
            &format!("registry/interface/world_operator_chat/{name}"),
            value,
        );
    }
    for (file, pointer) in [
        (
            "registry/interface/system_worker_template/policy.json",
            "/ceiling/interfaces",
        ),
        (
            "registry/ee_template/existence/ee_template.json",
            "/allowed_interfaces",
        ),
        (
            "ee/ee_existence.json",
            "/policy_narrowing/allowed_interfaces",
        ),
    ] {
        let mut value: Value =
            serde_json::from_slice(&std::fs::read(root.join(file)).unwrap()).unwrap();
        value
            .pointer_mut(pointer)
            .unwrap()
            .as_array_mut()
            .unwrap()
            .push(json!("world_operator_chat"));
        write(root, file, value);
    }
    request.communication.client_template_interface_id = Some("world_operator_chat".into());
    request.communication.client_title = Some("Context check".into());
    request
}

pub fn fixture(root: &Path) -> RunCompositionRequest {
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../../..");
    for relative in [
        "interface/system_room/interface.json",
        "interface/system_room/life.json",
        "interface/system_interface_authoring/interface.json",
        "interface/system_interface_authoring/life.json",
        "worker_environment_kind/system_room/worker_environment_kind.json",
        "tool/file.write/tool.json",
        "tool/file.read/tool.json",
        "tool/artifact.read/tool.json",
        "tool/artifact.send/tool.json",
        "mark/tool.call.allowed/mark.json",
        "mark/tool.result.corresponds/mark.json",
        "verifier/tool_result_correspondence/verifier.json",
        "verifier/tool_call_allowed/verifier.json",
    ] {
        let value =
            serde_json::from_slice(&std::fs::read(source.join("registry").join(relative)).unwrap())
                .unwrap();
        write(&root.join("registry"), relative, value);
    }
    std::fs::copy(
        source.join("registry/verifier/tool_call_allowed/verify.py"),
        root.join("registry/verifier/tool_call_allowed/verify.py"),
    )
    .unwrap();
    std::fs::copy(source.join("registry/verifier/tool_result_correspondence/verify.py"),
        root.join("registry/verifier/tool_result_correspondence/verify.py")).unwrap();
    write(
        root,
        "registry/interface/world_provider_fixture/interface.json",
        json!({
            "id":"world_provider_fixture", "interface_type":"world", "role":"provider", "direction":"bidirectional",
            "source_names":["fixture"], "interface_driver_ref":"fixture",
            "interface_policy":{"variables":{"models":{"reduce":"intersection","default":["fixture-a","fixture-b"]}}},
            "interface_driver_config":{"private_config":{"model":"fixture-a", "reasoning_effort":"xhigh",
                "model_catalog":[{"model":"fixture-a","supported_reasoning_efforts":["xhigh"]},{"model":"fixture-b"}],
                "run_authoring_profile":{"file_write_tool":"file.write"},"model_hidden_tools":["file.write"]}}
        }),
    );
    let life: Value = serde_json::from_slice(
        &std::fs::read(source.join("registry/interface/system_interface_authoring/life.json"))
            .unwrap(),
    )
    .unwrap();
    write(
        root,
        "registry/interface/world_provider_fixture/life.json",
        life,
    );
    for id in [
        "system_worker_template",
        "system_worker_parent",
        "system_worker_unselected",
    ] {
        write(
            root,
            &format!("registry/interface/{id}/interface.json"),
            json!({"id":id,"interface_type":"system","role":"worker","direction":"bidirectional","source_names":[id]}),
        );
        write(
            root,
            &format!("registry/interface/{id}/policy.json"),
            json!({"id":id,
            "ceiling":{"interfaces":["system_worker_template","system_room","system_interface_authoring","world_provider_fixture","system_worker_parent","system_worker_unselected"],
                "tools":["file.write","artifact.send"],"wek":["system_room"],"mark_outcomes":["tool.call.allowed","tool.result.corresponds"],
                "hard_blocked_tools":[],"interface_policy":{"world_provider_fixture":{"models":["fixture-a"]}}},
            "role_artifacts":[format!("role-{id}")]}),
        );
        write(
            root,
            &format!("registry/artifact/role-{id}/artifact.json"),
            json!({"id":format!("role-{id}"),"label":"context.reference","text":format!("Role for {id}")}),
        );
    }
    write(
        root,
        "registry/ee_template/existence/ee_template.json",
        json!({"id":"existence","goal_class":"existence",
        "allowed_interfaces":["system_worker_template","system_worker_parent","system_worker_unselected","system_room","system_interface_authoring","world_provider_fixture"],
        "allowed_tools":["file.write","artifact.send"],"bootstrap_root":true}),
    );
    write(
        root,
        "ee/ee_existence.json",
        json!({"ee_id":"ee_existence","template_name":"existence","parent_ee_id":null,"goal":"existence",
        "policy_narrowing":{"allowed_interfaces":["system_worker_template","system_worker_parent","system_worker_unselected","system_room","system_interface_authoring","world_provider_fixture"],"allowed_tools":["file.write","artifact.send"]}}),
    );
    let parent = RunRecord::from_bytes(&serde_json::to_vec(&json!({"id":"caller","ee":"ee_existence","worker":"system_worker_parent","trace":"caller-trace","status":"running",
        "interface_state":{"world_provider_fixture":{"private_history":"MUST NOT COPY"}},
        "checkpoints":[{"id":"current","run":"caller","trace":"caller-trace","epoch":0,"interface_binding":octopus_runtime::AuthoredInterfaceBinding::all("world_provider_fixture".into()),"artifact_bindings":[{"id":"private-transcript"}]}],"active_checkpoints":["current"]})).unwrap()).unwrap();
    RunCompositionRequest {
        roots: CompositionRoots { registry_root:root.join("registry"), ee_instances_root:root.join("ee"), run_records_root:root.join("runs"), interface_private_state_root:root.join("private") },
        metadata: serde_json::from_value(json!({"runtime":{"run":"caller","ee":"ee_existence","worker":"system_worker_parent","checkpoint":"current","interface":"world_provider_fixture","epoch":0}})).unwrap(),
        admitted_parent_record: parent, identity_seed:"call/first".into(), ee_id:"ee_existence".into(),
        worker_template_interface_id:"system_worker_template".into(), provider_interface_id:"world_provider_fixture".into(),
        model:"fixture-a".into(), reasoning_effort:"xhigh".into(), service_tier:None,
        authoring: CompositionAuthoring { source_interface_id:"system_interface_authoring".into(),room_interface_id:"system_room".into(),tool_id:"file.write".into(),tool_call_label:"tool.call".into(),tool_call_mark:"tool.call.allowed".into(),tool_call_verifier:"tool_call_allowed".into(),result_timeout_ms:30000 },
        communication: CompositionCommunication { client_template_interface_id:None, client_title:None, receive_interface_id:"system_worker_private_first".into(),return_interface_ids:vec!["system_worker_parent".into()],client_interface_id:None,output_selection:None },
        selected_worker_environment_kinds:vec![], selected_tools:vec![SelectedToolSeed { tool:"file.write".into(),interface:"system_room".into(),worker_environment_kind:"system_room".into() }, SelectedToolSeed { tool:"artifact.send".into(),interface:"system_room".into(),worker_environment_kind:"system_room".into() }],
        door_host_slots:BTreeMap::new(),external_inputs:vec![],external_relays:vec![],model_outputs:vec![],publications:vec![],context_artifact_bindings:vec![],payload_artifact_bindings:vec![],
        seed:CompositionSeed { label:"native.work".into(),payload:json!({"text":"Do the explicit task"}) },
    }
}
