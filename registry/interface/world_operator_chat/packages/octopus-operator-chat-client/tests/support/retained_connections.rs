use super::*;
use serde_json::{Value, json};

#[test]
fn client_continuation_retains_exact_peer_but_removes_selected_capability() {
    let dir = tempfile::tempdir().unwrap();
    let registry = dir.path().join("registry");
    tests::copy_planner_descriptors(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../../../registry"), &registry);
    let ee = dir.path().join("ee");
    std::fs::create_dir(&ee).unwrap();
    tests::boot_operator_space_ee_instances(&ee);
    let write = |path: PathBuf, value: &Value| {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
    };
    let apply = |plan: &OperatorChatPlan| {
        for edit in plan.interface_writes.iter().chain(&plan.run_writes) {
            if let Some(expected) = &edit.expected_before_hash {
                assert_eq!(expected, &format!("{:x}", Sha256::digest(std::fs::read(&edit.path).unwrap())));
            }
            std::fs::create_dir_all(edit.path.parent().unwrap()).unwrap();
            std::fs::write(&edit.path, &edit.content).unwrap();
        }
    };
    let provider_path = registry.join("interface/world_provider_ollama/interface.json");
    let mut provider: Value = serde_json::from_slice(&std::fs::read(&provider_path).unwrap()).unwrap();
    provider["interface_driver_config"]["private_config"].as_object_mut().unwrap().remove("input_intake");
    write(provider_path, &provider);
    let mut request: OperatorChatPlanRequest = serde_json::from_value(json!({
        "registry_root":registry,"ee_instances_root":ee,"run_records_root":dir.path().join("runs"),"interface_private_state_root":dir.path().join("private"),"write_hash_algorithm":"sha256",
        "chat_id":"retained_peer","provider_interface_id":"world_provider_ollama",
        "ee_id":"ee_operator_space","worker_interface_id":"system_worker_coder_max",
        "client_surface_role":"task","cycle":"first",
        "selected_worker_environment_kinds":["process_session"],"conversation":{"ledger_offset":0}
    })).unwrap();
    let first = plan_operator_chat_topology(&request).unwrap();
    apply(&first);
    let mut prior: Value = serde_json::from_str(&first.run_record_content).unwrap();
    let peer = "system_worker_exact_retained_peer";
    let peer_source = octopus_ledger::ArtifactRef { id:"peer-source".into(), content:octopus_ledger::ContentHash::of_bytes(b"source") };
    write(registry.join("artifact/peer-role/artifact.json"), &json!({"label":"context.reference","sources":[peer_source]}));
    let peer_template = "system_worker_peer_template";
    write(registry.join(format!("interface/{peer_template}/interface.json")), &json!({"id":peer_template,"interface_type":"system","role":"worker","direction":"bidirectional","source_names":[peer_template],"interface_policy":{"variables":{"channels":{"reduce":"intersection","default":["work","other"]}}}}));
    let mut peer_policy: Value = serde_json::from_slice(&std::fs::read(registry.join("interface/system_worker_coder_max/policy.json")).unwrap()).unwrap();
    peer_policy["id"] = json!(peer_template);
    peer_policy["role_artifacts"] = json!(["peer-role"]);
    write(registry.join(format!("interface/{peer_template}/policy.json")), &peer_policy);
    let record_hash = blake3::hash(first.run_record_content.as_bytes()).to_hex().to_string();
    let native_request: RunCompositionRequest = serde_json::from_value(json!({
        "roots":{"registry_root":registry,"ee_instances_root":ee,"run_records_root":dir.path().join("runs"),"interface_private_state_root":dir.path().join("private")},
        "metadata":{"runtime":{"run":first.run_id,"ee":"ee_operator_space","worker":first.worker_interface_id,"interface":first.model_interface_id,"checkpoint":"cp_operator_input_first","epoch":0,"run_record_hash":record_hash}},
        "admitted_parent_record":prior,"identity_seed":"peer-create","ee_id":"ee_operator_space","worker_template_interface_id":peer_template,
        "provider_interface_id":"world_provider_ollama","model":first.model,"reasoning_effort":first.reasoning_effort,
        "authoring":{"source_interface_id":"system_interface_authoring","room_interface_id":"system_room","tool_id":"file.write","tool_call_label":"tool.call","tool_call_mark":"tool.call.allowed","tool_call_verifier":"tool_call_allowed","result_timeout_ms":30000},
        "communication":{"receive_interface_id":peer,"return_interface_ids":[first.worker_interface_id],"client_interface_id":null},
        "selected_worker_environment_kinds":[],"selected_tools":[],"door_host_slots":{},"context_artifact_bindings":[],"payload_artifact_bindings":[],
        "seed":{"label":"provider.peer.message","payload":{"text":"fixture task"}}
    })).unwrap();
    let native = plan_run_composition(&native_request).unwrap();
    assert_eq!(native.required_binding_updates[0].expected_record_hash.as_deref(), Some(record_hash.as_str()));
    assert_eq!(native.required_binding_updates[0].required_artifact_bindings, vec![ArtifactBindingSeed{id:"peer-role".into()},ArtifactBindingSeed{id:"peer-source".into()}]);
    for edit in native.descriptor_writes.iter().chain(native.required_policy_updates.iter().map(|update|&update.write)).chain(std::iter::once(&native.run_write)) {
        if let Some(expected) = &edit.expected_before_hash { assert_eq!(expected, &blake3::hash(&std::fs::read(&edit.path).unwrap()).to_hex().to_string()); }
        std::fs::create_dir_all(edit.path.parent().unwrap()).unwrap();
        std::fs::write(&edit.path, &edit.content).unwrap();
    }
    let worker_path = registry.join(format!("interface/{}/policy.json",first.worker_interface_id));
    let mut policy: Value = serde_json::from_slice(&std::fs::read(&worker_path).unwrap()).unwrap();
    assert!(policy["ceiling"]["interfaces"].as_array().unwrap().contains(&json!(peer)));
    policy["ceiling"]["interfaces"].as_array_mut().unwrap().push(json!("system_worker_unbound_stale"));
    policy["ceiling"]["interface_policy"][peer] = json!({"channels":["work"]});
    write(worker_path.clone(), &policy);
    let incoming = operator_chat_input_binding_names(&first.model_interface_id, &["provider.peer.message".into()]).unwrap();
    let outgoing = operator_chat_input_binding_names(peer, &["provider.peer.message".into()]).unwrap();
    let template = json!({"name":"outgoing_peer","role":"provider_interface_output","interface_binding":incoming,"outbound_interface_bindings":[outgoing],"include_context":false,"static_artifact_bindings":["peer-role","peer-source"],"required_verified_artifacts":[]});
    prior["interface_state"][&first.model_interface_id]["run_authoring_binding"]["checkpoints"].as_array_mut().unwrap().push(template.clone());
    prior["interface_state"][&first.model_interface_id]["frontier_checkpoint_ids"].as_array_mut().unwrap().push(json!("peer_output"));
    let mut peer_artifacts = prior["interface_state"][&first.model_interface_id]["run_authoring_binding"]["writer_run"]["initial_record"]["checkpoints"][0]["artifact_bindings"].as_array().unwrap().clone();
    peer_artifacts.extend([json!({"id":"peer-role"}),json!({"id":"peer-source"})]);
    prior["checkpoints"].as_array_mut().unwrap().push(json!({"id":"peer_output","run":first.run_id,"trace":"peer-trace","epoch":0,"interface_binding":incoming,"outbound_interface_bindings":[outgoing],"artifact_bindings":peer_artifacts,"required_verified_artifacts":[]}));
    prior["active_checkpoints"].as_array_mut().unwrap().push(json!("peer_output"));
    let mut neighbor = prior["interface_state"][&first.model_interface_id]["run_authoring_binding"]["writer_run"]["initial_record"]["checkpoints"][0].clone();
    neighbor["run"] = json!(first.run_id);
    neighbor["id"] = json!("independent_neighbor");
    neighbor["interface_binding"] = json!(AuthoredInterfaceBinding::all("system_worker_flash".into()));
    neighbor["outbound_interface_bindings"] = json!([]);
    neighbor["required_verified_artifacts"] = json!([{"mark":"tool.call.allowed","artifact_label":"neighbor.evidence","verifier":"artifact_exists","params":{"opaque":"unchanged"}}]);
    neighbor["artifact_bindings"].as_array_mut().unwrap().push(json!({"id":"peer-role"}));
    prior["checkpoints"].as_array_mut().unwrap().push(neighbor.clone());
    prior["active_checkpoints"].as_array_mut().unwrap().push(json!("independent_neighbor"));
    request.existing_run_record = Some(prior.clone());
    request.selected_worker_environment_kinds.clear();
    request.cycle = "second".into();
    let next = plan_operator_chat_topology(&request).expect("normal operator continuation retains the exact native peer");
    let next_run: Value = serde_json::from_str(&next.run_record_content).unwrap();
    assert!(next_run["interface_state"][&first.model_interface_id]["run_authoring_binding"]["checkpoints"].as_array().unwrap().contains(&template));
    let route = next_run["checkpoints"].as_array().unwrap().iter().find(|cp| cp["id"] == "cp_retained_outgoing_peer_second").unwrap();
    assert_eq!(route["interface_binding"], json!(incoming));
    assert_eq!(route["outbound_interface_bindings"], json!([outgoing]));
    assert!(!outgoing.admits("unselected.message"));
    assert!(next_run["checkpoints"].as_array().unwrap().contains(&neighbor));
    assert!(route["artifact_bindings"].as_array().unwrap().iter().any(|r|r["id"]=="peer-source"));
    let input = next_run["checkpoints"].as_array().unwrap().iter().find(|cp|cp["id"]=="cp_operator_input_second").unwrap();
    assert!(["peer-role","peer-source"].iter().all(|id|input["artifact_bindings"].as_array().unwrap().iter().any(|r|r["id"]==*id)));
    assert!(next.required_interface_ids.iter().any(|id|id==peer));
    assert!(next.capability_doors.keys().all(|door| !first.capability_doors.contains_key(door) || door=="system_room"));
    assert!(!next.removals.is_empty());
    assert!(next.removals.iter().all(|r| !r.path.ends_with(peer)));
    let next_policy: Value = serde_json::from_str(&next.interface_writes.iter().find(|w|w.path==worker_path).unwrap().content).unwrap();
    assert_eq!(next_policy["ceiling"]["interface_policy"][peer],json!({"channels":["work"]}));
    assert!(!next_policy["ceiling"]["interfaces"].as_array().unwrap().contains(&json!("system_worker_unbound_stale")));
    apply(&next);
    let chain = resolve_live_ee_policy_chain(&ee, &EeId::new("ee_operator_space")).unwrap();
    let formed = form_capability_topology(&registry, &CapabilityFormationRequest::from_live_chain(next.worker_interface_id.clone(), chain)).unwrap();
    assert!(formed.worker_relays.contains_key(&InterfaceId::new(peer)));
    assert_eq!(formed.interface_policies[&InterfaceId::new(peer)].context["channels"], json!(["work"]));
    request.existing_run_record = Some(next_run.clone());
    request.cycle = "third".into();
    assert!(plan_operator_chat_topology(&request).is_ok(), "retention is durable across another client turn");
    request.existing_run_record = Some(prior.clone());
    request.existing_run_record.as_mut().unwrap()["interface_state"][&first.model_interface_id]["frontier_checkpoint_ids"].as_array_mut().unwrap().retain(|id|id!="peer_output");
    assert!(plan_operator_chat_topology(&request).unwrap_err().contains("outside its provider frontier"));
    request.existing_run_record = Some(prior.clone());
    request.existing_run_record.as_mut().unwrap()["checkpoints"].as_array_mut().unwrap().iter_mut().find(|cp|cp["id"]=="peer_output").unwrap()["artifact_bindings"] = json!([{"id":"peer-role"}]);
    assert!(plan_operator_chat_topology(&request).unwrap_err().contains("lacks readable static artifact"));
    request.existing_run_record = Some(prior);
    policy["ceiling"]["interfaces"].as_array_mut().unwrap().retain(|id|id!=peer);
    write(worker_path.clone(), &policy);
    assert!(plan_operator_chat_topology(&request).unwrap_err().contains("not an existing exact System ceiling entry"));
    write(worker_path.clone(), &next_policy);
    request.existing_run_record = Some(next_run);
    request.external_inputs = vec![ExternalInputSeed { checkpoint_id:"client_peer_input".into(),interface_id:peer.into(),artifact_names:vec![],required_verified_artifacts:vec![] }];
    let client_connected = plan_operator_chat_topology(&request).unwrap();
    apply(&client_connected);
    request.existing_run_record = Some(serde_json::from_str(&client_connected.run_record_content).unwrap());
    request.cycle = "fourth".into();
    assert!(plan_operator_chat_topology(&request).unwrap().run_record_content.contains("outgoing_peer"), "unchanged client connections preserve native output");
    request.external_inputs[0].required_verified_artifacts = serde_json::from_value(json!([{"mark":"tool.call.allowed","artifact_label":"peer.checked","verifier":"artifact_exists","params":{}}])).unwrap();
    let changed = plan_operator_chat_topology(&request).unwrap();
    assert!(!changed.run_record_content.contains("outgoing_peer"));
    assert!(changed.run_record_content.contains("peer.checked"));
    request.external_inputs.clear();
    let removed = plan_operator_chat_topology(&request).expect("explicit client removal also removes the associated native output");
    assert!(!removed.required_interface_ids.iter().any(|id|id==peer));
    assert!(!removed.run_record_content.contains("outgoing_peer"));

}

#[test]
fn connected_application_operations_wait_per_call_on_the_shared_interface() {
    let dir = tempfile::tempdir().unwrap();
    let registry = dir.path().join("registry");
    tests::copy_planner_descriptors(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../../../registry"), &registry);
    let ee = dir.path().join("ee");
    std::fs::create_dir(&ee).unwrap();
    tests::boot_operator_space_ee_instances(&ee);
    let write = |path: PathBuf, value: &Value| {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
    };
    let provider_path = registry.join("interface/world_provider_ollama/interface.json");
    let mut provider: Value = serde_json::from_slice(&std::fs::read(&provider_path).unwrap()).unwrap();
    provider["interface_driver_config"]["private_config"].as_object_mut().unwrap().remove("input_intake");
    write(provider_path, &provider);
    let mut request: OperatorChatPlanRequest = serde_json::from_value(json!({
        "registry_root":registry,"ee_instances_root":ee,"run_records_root":dir.path().join("runs"),"interface_private_state_root":dir.path().join("private"),"write_hash_algorithm":"sha256",
        "chat_id":"application_operations","provider_interface_id":"world_provider_ollama",
        "ee_id":"ee_operator_space","worker_interface_id":"system_worker_coder_max",
        "client_surface_role":"task","cycle":"first","conversation":{"ledger_offset":0},
        "external_inputs":[{"checkpoint_id":"cp_app_world_gmail_input_1","interface_id":"world_gmail"}]
    })).unwrap();
    assert!(plan_operator_chat_topology(&request).unwrap_err().contains("reconnect the application"));
    let catalog_id = "artifact-operation-catalog-world_gmail";
    let operation = |name: &str, output: &str| json!({"name":name,"description":name,"input_schema":{"type":"object"},
        "request_label":"mcp.operation.request","output_label":output});
    let mut catalog = json!({"id":catalog_id,"label":"mcp.operation_catalog","interface":"world_gmail","request_label":"mcp.operation.request",
        "operations":[operation("gmail.status","mcp.result"),operation("gmail.send_email","mcp.tool.result")]});
    write(registry.join("artifact").join(catalog_id).join("artifact.json"), &catalog);
    let plan = plan_operator_chat_topology(&request).unwrap();
    let run: Value = serde_json::from_str(&plan.run_record_content).unwrap();
    let state = &run["interface_state"][&plan.model_interface_id];
    let active = |run: &Value| run["checkpoints"].as_array().unwrap().iter()
        .filter(|cp| run["active_checkpoints"].as_array().unwrap().contains(&cp["id"])).cloned().collect::<Vec<_>>();
    assert!(!active(&run).iter().any(|cp| cp["interface_binding"]["interface"] == "world_gmail"), "an application without task inputs has no standing route");
    let request_binding = |interface: &str| json!(operator_chat_input_binding_names(interface, &["mcp.operation.request".into()]).unwrap());
    let route = active(&run).into_iter().find(|cp| cp["outbound_interface_bindings"] == json!([request_binding("world_gmail")])).unwrap();
    assert_eq!(route["interface_binding"], request_binding(&plan.model_interface_id));
    assert!(state["frontier_checkpoint_ids"].as_array().unwrap().contains(&route["id"]));
    let template = state["run_authoring_binding"]["checkpoints"].as_array().unwrap().iter()
        .find(|cp| cp["role"] == "provider_interface_output").unwrap();
    assert_eq!(template["result_receiving"]["mcp.tool.result"], json!({"mark":"tool.result.corresponds","artifact_label":"mcp.tool.result","verifier":"tool_result_correspondence","params":{}}));
    assert_eq!(template["result_receiving"].as_object().unwrap().keys().collect::<Vec<_>>(), ["mcp.result", "mcp.tool.result"]);
    let input = run["checkpoints"].as_array().unwrap().iter().find(|cp| cp["id"] == "cp_operator_input_first").unwrap();
    assert!(input["artifact_bindings"].as_array().unwrap().iter().any(|binding| binding["id"] == catalog_id));

    catalog["task_input_labels"] = json!(["gmail.event"]);
    write(registry.join("artifact").join(catalog_id).join("artifact.json"), &catalog);
    let evented: Value = serde_json::from_str(&plan_operator_chat_topology(&request).unwrap().run_record_content).unwrap();
    let standing = active(&evented).into_iter().find(|cp| cp["interface_binding"]["interface"] == "world_gmail").unwrap();
    assert_eq!(standing["interface_binding"], json!(operator_chat_input_binding_names("world_gmail", &["gmail.event".into()]).unwrap()));
    catalog.as_object_mut().unwrap().remove("task_input_labels");
    write(registry.join("artifact").join(catalog_id).join("artifact.json"), &catalog);

    for edit in plan.interface_writes.iter().chain(&plan.run_writes) {
        std::fs::create_dir_all(edit.path.parent().unwrap()).unwrap();
        std::fs::write(&edit.path, &edit.content).unwrap();
    }
    request.existing_run_record = Some(run);
    request.cycle = "second".into();
    let next: Value = serde_json::from_str(&plan_operator_chat_topology(&request).expect("a continuation recomposes the operation route").run_record_content).unwrap();
    assert_eq!(active(&next).iter().filter(|cp| cp["outbound_interface_bindings"] == json!([request_binding("world_gmail")])).count(), 1);
    request.external_inputs.clear();
    let removed = plan_operator_chat_topology(&request).expect("disconnecting removes the operation route").run_record_content;
    assert!(!removed.contains("world_gmail"));
}
