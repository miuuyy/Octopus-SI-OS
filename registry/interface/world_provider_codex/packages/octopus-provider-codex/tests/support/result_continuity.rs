#[test]
fn forwarded_result_preserves_its_exact_native_wait() {
    let temp = tempfile::tempdir().unwrap();
    let mut input = model_policy_input(serde_json::json!(["gpt-5.5"]));
    let runtime = input.metadata.as_ref().unwrap().runtime.as_ref().unwrap().clone();
    let payload = serde_json::json!({"label":"read.result","tool":"read","correlation_id":"call-a","ok":true,"body":{"text":"ok"}});
    let bytes = serde_json::to_vec(&payload).unwrap();
    let original = InterfaceArtifactFile::write(temp.path().join("original"), "read.result", &bytes).unwrap();
    let source = original.pin_manifest_into_blob(&mut octopus_ledger::InMemoryBlobs::new()).unwrap().artifact;
    input.artifact_file = InterfaceArtifactFile::write(temp.path().join("forwarded"), "read.result", &bytes).unwrap();
    input.artifact = input.artifact_file.pin_manifest_into_blob(&mut octopus_ledger::InMemoryBlobs::new()).unwrap().artifact;
    assert_ne!(input.artifact, source);
    input.artifact_binding_payloads = vec![ArtifactBindingPayload::new(source.clone(), bytes)];
    input.metadata.as_mut().unwrap().artifact_binding_refs = vec![source.clone()];
    input.metadata.as_mut().unwrap().grant.artifact_access.insert(source.id.to_string(), octopus_runtime::ArtifactAccess::read_only());
    let requirement = serde_json::json!({"mark":"corresponds","verifier":"tool_result_correspondence","artifact_label":"read.result",
        "params":{"call_artifact":source,"tool":"read","correlation_id":"call-a","params":{},"invocation":{
            "source":{"artifact":source,"source_movement_seq":40},"artifact":input.artifact,"manifest_relative_path":"invocation/result.json"}}});
    let binding = serde_json::json!({"interface":"result-door","config":{"artifact_policy":{"mode":"all"}}});
    let record = octopus_run_record::RunRecord::from_bytes(&serde_json::to_vec(&serde_json::json!({
        "id":runtime.run,"ee":runtime.ee,"worker":runtime.worker,"trace":"trace","status":"running",
        "active_checkpoints":[runtime.checkpoint],"checkpoints":[{"id":runtime.checkpoint,"run":runtime.run,"trace":"trace","epoch":1,
            "interface_binding":binding,"outbound_interface_bindings":[],"artifact_bindings":[{"id":source.id}],"required_verified_artifacts":[requirement]}],
        "interface_state":{runtime.interface.as_str():{"run_authoring_binding":{"schema_version":2,"run":runtime.run,"worker":runtime.worker,
            "provider_interface":runtime.interface,"authoring_source_interface":"writer","checkpoints":[{"name":"result","role":"result_input",
                "interface_binding":binding,"outbound_interface_bindings":[],"include_context":true,"required_verified_artifacts":[requirement]}]}}}
    })).unwrap()).unwrap();
    let pending = continuity(runtime.interface.as_str(), runtime.run.as_str(), "thread-a", 5, Some(CodexPendingContinuity::DynamicTool {
        native_operation:None,turn_id:"turn-a".into(),call_id:"call-a".into(),tool:"read".into(),
    }));
    let mut result: SessionContextArtifact = serde_json::from_value(session_artifact("original-result", "read.result", payload.clone(), 40)).unwrap();
    result.source_artifact = source;
    let call = serde_json::from_value(session_artifact("call", "tool.call", serde_json::json!({"label":"tool.call",CODEX_CONTINUITY_FIELD:pending}), 30)).unwrap();
    let session = vec![call, result];
    let sources = continuity_input_sources(&record, &input).unwrap();
    let recovered = recover_codex_continuity_from_current_session(&session, &sources, &runtime.interface, runtime.run.as_str(), &codex_context_labels()).unwrap().unwrap();
    assert_eq!(recovered, pending);
    let call = bound("exact-call", serde_json::json!({"label":"tool.call", "tool":"read", "correlation_id":"call-a", "arguments":{}, CODEX_CONTINUITY_FIELD:pending}));
    input.artifact_binding_payloads.push(call.clone());
    input.metadata.as_mut().unwrap().artifact_binding_refs.push(call.artifact.clone());
    input.metadata.as_mut().unwrap().grant.artifact_access.insert(call.artifact.id.to_string(), octopus_runtime::ArtifactAccess::read_only());
    let mut value = serde_json::to_value(&record).unwrap();
    value["checkpoints"][0]["artifact_bindings"].as_array_mut().unwrap().push(serde_json::json!({"id":call.artifact.id}));
    value["checkpoints"][0]["required_verified_artifacts"][0]["params"]["call_artifact"] = serde_json::to_value(&call.artifact).unwrap();
    let template = &mut value["interface_state"][runtime.interface.as_str()]["run_authoring_binding"]["checkpoints"][0];
    template["static_artifact_bindings"] = serde_json::json!([call.artifact.id]);
    template["required_verified_artifacts"][0]["params"]["call_artifact"] = serde_json::to_value(&call.artifact).unwrap();
    let pinned = serde_json::to_vec(&value).unwrap();
    let record = octopus_run_record::RunRecord::from_bytes(&pinned).unwrap();
    let context = octopus_run_authoring::context_artifact_payloads(&record, input.metadata.as_ref().unwrap(), &input.artifact_binding_payloads).unwrap();
    assert!(context.iter().all(|payload| payload.artifact != call.artifact));
    let prior = continuity(runtime.interface.as_str(), runtime.run.as_str(), "thread-a", 4, None);
    let prior = serde_json::from_value(session_artifact("prior-commentary", "turn.agent_message", serde_json::json!({"label":"turn.agent_message",CODEX_CONTINUITY_FIELD:prior}), 25)).unwrap();
    let session_without_call = vec![prior, session[1].clone()];
    let descriptor: InterfaceDescriptor = serde_json::from_slice(&std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../interface.json")).unwrap()).unwrap();
    let mut descriptor = descriptor;
    descriptor.id = runtime.interface.clone();
    assert_eq!(recover_provider_result_continuity(&descriptor, &input, None, &session_without_call, &runtime.run, &pinned).unwrap(), Some(pending.clone()));
    let previous_pending = continuity(runtime.interface.as_str(), runtime.run.as_str(), "thread-a", 4, Some(CodexPendingContinuity::DynamicTool {
        native_operation: None, turn_id: "turn-a".into(), call_id: "call-previous".into(), tool: "read".into(),
    }));
    let previous_call = serde_json::from_value(session_artifact("previous-call", "tool.call",
        serde_json::json!({"label":"tool.call", CODEX_CONTINUITY_FIELD:previous_pending}), 25)).unwrap();
    let previous_result = serde_json::from_value(session_artifact("previous-result", "read.result",
        serde_json::json!({"label":"read.result","tool":"read","correlation_id":"call-previous","ok":true,"body":{"text":"earlier"}}), 26)).unwrap();
    let projected_settlement = vec![previous_call, previous_result, session[1].clone()];
    assert_eq!(recover_provider_result_continuity(&descriptor, &input, None, &projected_settlement,
        &runtime.run, &pinned).unwrap(), Some(pending.clone()));
    let settled = continuity(runtime.interface.as_str(), runtime.run.as_str(), "thread-a", 5, None);
    let mut recorded_settlement = projected_settlement.clone();
    recorded_settlement.push(serde_json::from_value(session_artifact("recorded-settlement", "turn.agent_message",
        serde_json::json!({"label":"turn.agent_message", CODEX_CONTINUITY_FIELD:settled}), 35)).unwrap());
    assert!(recover_provider_result_continuity(&descriptor, &input, None, &recorded_settlement,
        &runtime.run, &pinned).unwrap().is_none());
    let mut foreign = session_without_call.clone();
    foreign[0].payload[CODEX_CONTINUITY_FIELD]["run"] = serde_json::json!("other-run");
    assert!(recover_provider_result_continuity(&descriptor, &input, None, &foreign, &runtime.run, &pinned).is_err());
    let wrong = serde_json::json!({"label":"read.result","tool":"read","correlation_id":"call-a","ok":true,"body":{"text":"changed"}});
    input.artifact_binding_payloads[0] = ArtifactBindingPayload::new(sources[1].clone(), serde_json::to_vec(&wrong).unwrap());
    assert!(continuity_input_sources(&record, &input).is_err());
    input.artifact_binding_payloads[0] = ArtifactBindingPayload::new(sources[1].clone(), serde_json::to_vec(&payload).unwrap());
    input.metadata.as_mut().unwrap().grant.artifact_access.clear();
    assert!(continuity_input_sources(&record, &input).is_err());
}

#[test]
fn successive_tool_results_use_admitted_session_continuity() {
    let temp = tempfile::tempdir().unwrap();
    let descriptor: InterfaceDescriptor = serde_json::from_slice(&std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../interface.json"),
    ).unwrap()).unwrap();
    let mut input = model_policy_input(serde_json::json!(["gpt-5.5"]));
    let runtime = input.metadata.as_ref().unwrap().runtime.as_ref().unwrap().clone();
    let pending = |call: &str, tool: &str| CodexPendingContinuity::DynamicTool {
        native_operation: None, turn_id: "turn-a".into(), call_id: call.into(), tool: tool.into(),
    };
    let first = continuity(runtime.interface.as_str(), runtime.run.as_str(), "thread-a", 3,
        Some(pending("call-list", "artifact.list")));
    let second = continuity(runtime.interface.as_str(), runtime.run.as_str(), "thread-a", 5,
        Some(pending("call-read", "artifact.read")));
    let first_payload = serde_json::json!({"label":"tool.call", CODEX_CONTINUITY_FIELD:first});
    let first_bound = bound("first-call", first_payload.clone());
    input.metadata.as_mut().unwrap().artifact_binding_refs = vec![first_bound.artifact.clone()];
    input.artifact_binding_payloads = vec![first_bound];
    let result_payload = serde_json::json!({"label":"artifact.read.result", "tool":"artifact.read",
        "correlation_id":"call-read", "ok":true, "body":{"text":"current document"}});
    input.artifact_file = InterfaceArtifactFile::write(temp.path(), "artifact.read.result",
        &serde_json::to_vec(&result_payload).unwrap()).unwrap();
    input.artifact = input.artifact_file.pin_manifest_into_blob(
        &mut octopus_ledger::InMemoryBlobs::new()).unwrap().artifact;
    let record = serde_json::to_vec(&serde_json::json!({
        "id":runtime.run, "ee":runtime.ee, "worker":runtime.worker, "trace":"trace:successive-tools",
        "status":"running", "checkpoints":[], "active_checkpoints":[],
    })).unwrap();
    let session = vec![
        session_artifact("first-call", "tool.call", first_payload, 10),
        session_artifact("first-result", "artifact.list.result", serde_json::json!({
            "label":"artifact.list.result", "tool":"artifact.list", "correlation_id":"call-list",
            "ok":true,"body":{"artifacts":[]}}), 20),
        session_artifact("second-call", "tool.call", serde_json::json!({
            "label":"tool.call", CODEX_CONTINUITY_FIELD:second}), 30),
    ].into_iter().map(|entry| serde_json::from_value::<SessionContextArtifact>(entry).unwrap())
        .collect::<Vec<_>>();
    let recovered = recover_provider_result_continuity(&descriptor, &input, None, &session,
        &runtime.run, &record).unwrap().unwrap();
    assert_eq!(recovered, second);
    assert!(recover_provider_result_continuity(&descriptor, &input, None, &[],
        &runtime.run, &record).is_err());
    let mut wrong_thread = session.clone();
    wrong_thread[2].payload[CODEX_CONTINUITY_FIELD]["thread_id"] = serde_json::json!("unrelated-thread");
    assert!(recover_provider_result_continuity(&descriptor, &input, None, &wrong_thread,
        &runtime.run, &record).is_err());
    let mut wrong_call = session.clone();
    wrong_call[2].payload[CODEX_CONTINUITY_FIELD]["pending"]["call_id"] = serde_json::json!("unrelated-call");
    assert!(recover_provider_result_continuity(&descriptor, &input, None, &wrong_call,
        &runtime.run, &record).is_err());

    // The prior result has already produced a new call. Only its admitted handoff
    // may finish that old source; correlation mismatches alone never authorize it.
    let mut old_input = input.clone();
    old_input.artifact_file = InterfaceArtifactFile::write(temp.path(), "artifact.list.result",
        &serde_json::to_vec(&serde_json::json!({"label":"artifact.list.result", "tool":"artifact.list",
            "correlation_id":"call-list", "ok":true, "body":{"artifacts":[]}})).unwrap()).unwrap();
    old_input.artifact = old_input.artifact_file.pin_manifest_into_blob(
        &mut octopus_ledger::InMemoryBlobs::new()).unwrap().artifact;
    let mut handed = session;
    handed[2].payload["tool"] = serde_json::json!("artifact.read");
    handed[2].payload["correlation_id"] = serde_json::json!("call-read");
    handed[2].payload["arguments"] = serde_json::json!({"path":"document"});
    let outcomes = vec![octopus_run_authoring::ProviderHandoffOutcome::Admitted {
        seq:30, artifact:handed[2].source_artifact.clone(),
    }];
    let mut current: serde_json::Value = serde_json::from_slice(&record).unwrap();
    current["interface_state"] = serde_json::json!({runtime.interface.as_str():{"context_generation":5}});
    let current = serde_json::to_vec(&current).unwrap();
    assert!(recover_provider_result_continuity(&descriptor, &old_input, None, &handed,
        &runtime.run, &record).is_err());
    let receipt = recovered_tool_handoff_receipt(&descriptor, &old_input, 20, &current,
        &handed, &outcomes, temp.path()).unwrap().unwrap();
    let value: serde_json::Value = serde_json::from_slice(&receipt.artifact.read_payload_bytes().unwrap()).unwrap();
    assert_eq!(value["label"], INTERFACE_CONTEXT_UPDATED_LABEL);
    assert_eq!(value["effect_artifact"], serde_json::to_value(&handed[2].source_artifact).unwrap());
    assert_eq!(value["source_artifact"], serde_json::to_value(&old_input.artifact).unwrap());
    assert_eq!(value[CODEX_CONTINUITY_FIELD]["context_generation"], 6);
    assert_eq!(value[CODEX_CONTINUITY_FIELD]["pending"], handed[2].payload[CODEX_CONTINUITY_FIELD]["pending"]);
    assert!(recovered_tool_handoff_receipt(&descriptor, &old_input, 20, &current,
        &handed, &[], temp.path()).unwrap().is_none());
    assert!(recovered_tool_handoff_receipt(&descriptor, &old_input, 30, &current,
        &handed, &outcomes, temp.path()).is_err());
    let mut forged = handed.clone();
    forged[2].payload["correlation_id"] = serde_json::json!("different-call");
    assert!(recovered_tool_handoff_receipt(&descriptor, &old_input, 20, &current,
        &forged, &outcomes, temp.path()).is_err());
    let mut forged = handed;
    forged[2].source_movement_seq = Some(31);
    assert!(recovered_tool_handoff_receipt(&descriptor, &old_input, 20, &current,
        &forged, &outcomes, temp.path()).is_err());

    // Two routes can deliver one immutable result. The second receipt must keep
    // the successor wait intact and retain the completed input's exact identity.
    let mut duplicate_context = forged;
    duplicate_context[2].source_movement_seq = Some(30);
    duplicate_context[1].source_artifact = old_input.artifact.clone();
    duplicate_context[1].payload = serde_json::from_slice(&old_input.read_artifact_payload().unwrap()).unwrap();
    let receipt = duplicate_tool_result_receipt(&descriptor, &old_input, 25, 20,
        &current, &duplicate_context, temp.path()).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&receipt.artifact.read_payload_bytes().unwrap()).unwrap();
    assert_eq!(value["code"], "duplicate_tool_result");
    assert_eq!(value["label"], TURN_INPUT_ACKNOWLEDGED_LABEL);
    assert!(!codex_context_labels().contains(TURN_INPUT_ACKNOWLEDGED_LABEL));
    assert_eq!(value["completed_source_movement_seq"], 20);
    assert!(value.get(CODEX_CONTINUITY_FIELD).is_none());
    assert_eq!(value["context_generation"], 5);
    assert!(duplicate_tool_result_receipt(&descriptor, &old_input, 20, 20,
        &current, &duplicate_context, temp.path()).is_err());
    let mut changed = duplicate_context.clone();
    changed[1].payload["body"] = serde_json::json!({"artifacts":["forged"]});
    assert!(duplicate_tool_result_receipt(&descriptor, &old_input, 25, 20,
        &current, &changed, temp.path()).is_err());
    let mut without_prior_context = duplicate_context.clone();
    without_prior_context.remove(1);
    duplicate_tool_result_receipt(&descriptor, &old_input, 25, 20,
        &current, &without_prior_context, temp.path()).unwrap();
    let mut changed = duplicate_context.clone();
    changed[1].source_movement_seq = Some(26);
    assert!(duplicate_tool_result_receipt(&descriptor, &old_input, 25, 20,
        &current, &changed, temp.path()).is_err());
    let mut interrupted = duplicate_context.clone();
    let pending = interrupted[2].payload[CODEX_CONTINUITY_FIELD]["pending"].take();
    interrupted[2].payload[CODEX_CONTINUITY_FIELD]["interrupted_pending"] = pending;
    let receipt = duplicate_tool_result_receipt(&descriptor, &old_input, 25, 20,
        &current, &interrupted, temp.path()).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&receipt.artifact.read_payload_bytes().unwrap()).unwrap();
    assert_eq!(value["context_generation"], 5);
    assert!(value.get(CODEX_CONTINUITY_FIELD).is_none());
    duplicate_context[2].payload[CODEX_CONTINUITY_FIELD]["pending"]["call_id"] = serde_json::json!("call-list");
    assert!(duplicate_tool_result_receipt(&descriptor, &old_input, 25, 20,
        &current, &duplicate_context, temp.path()).is_err());
}
