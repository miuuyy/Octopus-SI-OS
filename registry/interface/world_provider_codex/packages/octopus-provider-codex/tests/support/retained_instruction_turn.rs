#[tokio::test]
async fn a_declared_instruction_arrival_opens_its_turn_as_one_retained_tool_output() {
    use octopus_appserver_proto::{ApprovalsReviewer, AskForApproval, SandboxPolicy, SessionSource, Thread, ThreadStatus};
    let temp = tempfile::tempdir().unwrap();
    let cwd = octopus_util_abspath::AbsolutePathBuf::from_absolute_path(temp.path()).unwrap();
    let thread = Thread {
        id: "thr-exact".into(), session_id: "thr-exact".into(), forked_from_id: None, parent_thread_id: None,
        preview: String::new(), ephemeral: false, model_provider: "openai".into(), created_at: 1, updated_at: 1,
        recency_at: Some(1), status: ThreadStatus::Idle, path: None, cwd: cwd.clone(), cli_version: "test".into(),
        source: SessionSource::Exec, thread_source: None, agent_nickname: None, agent_role: None, git_info: None,
        name: None, turns: Vec::new(),
    };
    let read = serde_json::to_string(&ThreadReadResponse { thread: thread.clone() }).unwrap();
    let resume = serde_json::to_string(&ThreadResumeResponse {
        thread, model: "gpt-5.5".into(), model_provider: "openai".into(), service_tier: None, cwd,
        runtime_workspace_roots: Vec::new(), instruction_sources: Vec::new(), approval_policy: AskForApproval::OnFailure,
        approvals_reviewer: ApprovalsReviewer::User, sandbox: SandboxPolicy::DangerFullAccess,
        active_permission_profile: None, reasoning_effort: None, multi_agent_mode: None, initial_turns_page: None,
    }).unwrap();
    let message = serde_json::json!({"action":"followup","control_id":"peer-1","text":"Report what the source run left open."});
    let message_bytes = serde_json::to_vec(&message).unwrap();
    let message_ref = ArtifactRef {
        id: octopus_descriptors::ArtifactId::new("peer-instruction"),
        content: octopus_ledger::ContentHash::of_bytes(&message_bytes),
    };
    let instruction = peer_input_text(&serde_json::from_value(message.clone()).unwrap(), &message_ref).unwrap();
    let item = serde_json::json!({
        "type":"functionCallOutput","id":"instruction-1","name":"octopus_instruction",
        "namespace":"formed_interfaces","output":instruction,
    });
    let answer = serde_json::json!({"type":"agentMessage","id":"answer-1","text":"Nothing is open.","phase":"final_answer"});
    let done = serde_json::json!({
        "id":"turn-2","items":[item, answer],"itemsView":"full","status":"completed","error":null,
    });
    let requests = temp.path().join("app-server-requests.jsonl");
    let script = temp.path().join("app-server.sh");
    std::fs::write(&script, r#"#!/bin/sh
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$1"
  id=$(printf '%s\n' "$line" | sed -n 's/^{"id":\([^,]*\),.*/\1/p')
  case "$line" in
    *'"method":"initialize"'*) printf '{"id":%s,"result":{"userAgent":"codex/test","codexHome":"/tmp"}}\n' "$id" ;;
    *'"method":"thread/read"'*) printf '{"id":%s,"result":%s}\n' "$id" "$2" ;;
    *'"method":"thread/turns/list"'*) printf '{"id":%s,"result":{"data":[],"nextCursor":null,"backwardsCursor":null}}\n' "$id" ;;
    *'"method":"thread/items/list"'*) printf '{"id":%s,"result":{"data":[],"nextCursor":null,"backwardsCursor":null}}\n' "$id" ;;
    *'"method":"thread/resume"'*) printf '{"id":%s,"result":%s}\n' "$id" "$3" ;;
    *'"method":"hooks/list"'*) printf '{"id":%s,"result":{"data":[]}}\n' "$id" ;;
    *'"method":"turn/start"'*'"input":[]'*'"toolOutput":{'*)
      printf '{"id":%s,"result":{"turn":{"id":"turn-2","items":[],"itemsView":"notLoaded","status":"inProgress","error":null}}}\n' "$id"
      printf '{"method":"item/started","params":{"threadId":"thr-exact","turnId":"turn-2","startedAtMs":10,"item":%s}}\n' "$4"
      printf '{"method":"item/completed","params":{"threadId":"thr-exact","turnId":"turn-2","completedAtMs":11,"item":%s}}\n' "$4"
      printf '%s\n' '{"method":"thread/tokenUsage/updated","params":{"threadId":"thr-exact","turnId":"turn-2","tokenUsage":{"total":{"totalTokens":100,"inputTokens":80,"cachedInputTokens":0,"outputTokens":20,"reasoningOutputTokens":0},"last":{"totalTokens":100,"inputTokens":80,"cachedInputTokens":0,"outputTokens":20,"reasoningOutputTokens":0},"modelContextWindow":32000}}}'
      printf '%s\n' '{"method":"item/completed","params":{"threadId":"thr-exact","turnId":"turn-2","completedAtMs":20,"item":{"type":"agentMessage","id":"answer-1","text":"Nothing is open.","phase":"final_answer"}}}'
      printf '{"method":"turn/completed","params":{"threadId":"thr-exact","turn":%s}}\n' "$5" ;;
    '{"id":'*) printf '{"id":%s,"error":{"code":-32601,"message":"unexpected request"}}\n' "$id" ;;
  esac
done
"#).unwrap();
    let spawned = crate::SpawnAppServerClient::connect(crate::spawn::SpawnConfig {
        bin: "/bin/sh".into(),
        args: vec![script.display().to_string(), requests.display().to_string(), read, resume, item.to_string(), done.to_string()],
        cwd: Some(temp.path().to_path_buf()), codex_home: None,
        initialize_params: serde_json::from_value(serde_json::json!({
            "clientInfo": {"name": "octopus-test", "version": "0"}, "capabilities": {},
        })).unwrap(),
        compatibility: None, channel_capacity: 16,
    }).await.unwrap();
    let driver_config = InterfaceDriverConfig {
        private_config: serde_json::json!({
            "model": "gpt-5.5", "reasoning_summary": "none", "web_search": "live",
            "instruction_artifacts": [], "worker_instruction_artifacts": ["role-instruction"],
            "context_output_labels": codex_context_labels(), "session_snapshot_labels": codex_snapshot_labels(),
            "client_projection_output_labels": codex_client_projection_labels(), "live_stream": live_stream_config_value(),
            "native_tool_namespace": {"name": "formed_interfaces", "description": "Formed interface tools."},
            "model_result_projection": {"max_tokens": 10000}, "private_thread_cold_start": "resume_exact",
            "retained_tool_output": {"name": "octopus_instruction", "arrival_labels": [PEER_MESSAGE_LABEL]},
        }).as_object().unwrap().clone(),
        ..InterfaceDriverConfig::default()
    };
    let driver = CodexAppServerInterfaceDriver::new_with_config(AppServerClient::Spawn(spawned), Some(&driver_config));
    let role = bound("role-instruction", serde_json::json!({"label":"context.reference","text":"role"}));
    let mut static_bytes = Vec::new();
    append_static_context_source(&mut static_bytes, &role).unwrap();
    static_bytes.extend_from_slice(br#"{"coder-dexec":"coder.exec"}"#);
    let native_affordances = namespace_native_affordances(vec![DynamicToolSpec::Function(DynamicToolFunctionSpec {
        name: "coder-dexec".into(), description: "execute".into(),
        input_schema: serde_json::json!({"type":"object"}), defer_loading: false,
    })], &NativeToolNamespace { name: "formed_interfaces".into(), description: "Formed interface tools.".into() }).unwrap();
    let mut settled = continuity("world_provider_codex", "run-a", "thr-exact", 7, None);
    settled.static_context_fingerprint = sha256_hex(&static_bytes);
    settled.thread_shape_fingerprint = thread_shape_fingerprint(
        Some("gpt-5.5"), "role", &native_affordances, &settled.static_context_fingerprint,
    ).unwrap();
    let mut grant = octopus_interfaces::GrantProjection::default();
    grant.tools.push("coder.exec".into());
    grant.interface_affordance_specs.push(octopus_ledger::InterfaceToolAffordance {
        name: "coder.exec".into(), description: "execute".into(), args_schema: serde_json::json!({"type":"object"}),
    });
    grant.interface_policy.insert("world_provider_codex".into(), BTreeMap::from([
        ("models".to_string(), serde_json::json!(["gpt-5.5"])),
        ("web_search_modes".to_string(), serde_json::json!(["live"])),
    ]));
    let mut metadata = octopus_interfaces::InvocationMetadata::new(grant).with_runtime(RuntimeInvocationMetadata {
        run: octopus_interfaces::RunId::new("run-a"), ee: octopus_descriptors::EeId::new("ee-a"),
        worker: InterfaceId::new("system_worker_test"), checkpoint: octopus_descriptors::CheckpointId::new("checkpoint-a"),
        interface: InterfaceId::new("world_provider_codex"), epoch: 1, run_record_hash: None,
    });
    metadata.checkpoint_binding_policy = Some(serde_json::from_value(serde_json::json!({
        "inbound":{"interface":"system_worker_test","config":{"artifact_policy":{"mode":"all"}}},"outbound":[],
    })).unwrap());
    metadata.grant.role_artifacts = vec!["role-instruction".into()];
    let read_access = serde_json::json!({"read":true,"change":false,"create":false,"delete":false});
    metadata.grant.artifact_access.insert("role-instruction".into(), serde_json::from_value(read_access).unwrap());
    let runtime = metadata.runtime.clone().unwrap();
    let mut admitted = serde_json::to_value(octopus_run_record::RunRecord {
        id: runtime.run.clone(), ee: runtime.ee.clone(), worker: runtime.worker.clone(),
        trace: octopus_runtime::TraceId("trace:retained-instruction".into()), status: serde_json::json!("running"),
        interface_state: BTreeMap::new(), checkpoints: Vec::new(), active_checkpoints: Vec::new(),
    }).unwrap();
    admitted["interface_state"]["world_provider_codex"] = serde_json::json!({
        "context_generation": 6,
        "run_authoring_binding": {
            "schema_version":2,"run":"run-a","worker":"system_worker_test","provider_interface":"world_provider_codex",
            "authoring_source_interface":"world_provider_codex","payload_artifact_bindings":[],"checkpoints":[],
        },
    });
    admitted["checkpoints"] = serde_json::json!([{
        "id":"checkpoint-a","run":"run-a","trace":"trace:retained-instruction","epoch":1,
        "interface_binding":octopus_runtime::AuthoredInterfaceBinding::all(InterfaceId::new("system_worker_test")),
        "outbound_interface_bindings":[octopus_runtime::AuthoredInterfaceBinding::all(runtime.interface.clone())],
        "artifact_bindings":[],"required_verified_artifacts":[],
    }]);
    admitted["active_checkpoints"] = serde_json::json!(["checkpoint-a"]);
    let admitted_record = serde_json::to_vec(&admitted).unwrap();
    octopus_run_record::RunRecord::from_bytes(&admitted_record).unwrap();
    let mut current = admitted;
    current["interface_state"]["world_provider_codex"]["context_generation"] = serde_json::json!(7);
    current["checkpoints"] = serde_json::json!([]);
    current["active_checkpoints"] = serde_json::json!([]);
    let current_record = serde_json::to_vec(&current).unwrap();
    let earlier: SessionContextArtifact = serde_json::from_value(session_artifact(
        "operator-task", "operator.message", serde_json::json!({"text": "start the source run"}), 1,
    )).unwrap();
    let carrier = bound("settled-answer", serde_json::json!({
        "label": "interface.artifact.output", CODEX_CONTINUITY_FIELD: settled,
        "session_context": {"schema_version": 3, "provider_interface": "world_provider_codex", "artifacts": [earlier.clone()]},
    }));
    let snapshot = ProviderInvocationSessionSnapshot { carrier: carrier.artifact.clone(), artifacts: Arc::new(vec![earlier]) };
    metadata.runtime.as_mut().unwrap().run_record_hash = Some(octopus_ledger::ContentHash::of_bytes(&admitted_record).to_hex());
    metadata.artifact_binding_refs = vec![carrier.artifact.clone(), role.artifact.clone()];
    let envelope = InterfaceInvocationEnvelope {
        artifact: message_ref.clone(),
        artifact_file: InterfaceArtifactFile::write(temp.path().join("input"), PEER_MESSAGE_LABEL, &message_bytes).unwrap(),
        artifact_binding_payloads: vec![carrier, role],
        metadata: Some(metadata),
    };
    let (output, mut output_rx) = ProviderOutputStream::channel();
    let control = CodexTurnControlBroker::new(Some(1));
    let (output_root, live_stream_root) = (temp.path().join("output"), temp.path().join("streams"));
    let invocation = driver.receive_artifact_and_stream_outputs(CodexProviderInvocation {
        callable_tool_names: envelope.granted_tool_names().unwrap().into_iter().collect(),
        input: envelope, emit: output, resolved_session_snapshot: Some(snapshot),
        current_session_context: Arc::new(Vec::new()),
        output_root: &output_root, live_stream_root: &live_stream_root,
        source_movement_seq: 1, attempt: 1, admitted_run_record: &admitted_record, current_run_record: &current_record,
        deferred_context_generations: 0, input_changes_context: true, interrupted_after_artifact: None,
        recovered_handoff_artifacts: Arc::new(Vec::new()), durable_interrupted_continuity: None,
        reconciled_result_continuity: None, control: &control, handoff_barrier: ProviderHandoffBarrier::new(),
    });
    let collect = async {
        let mut emitted = Vec::new();
        while let Some(output) = output_rx.recv().await {
            match output {
                ProviderStreamEmission::Turn(emission) | ProviderStreamEmission::Progress(emission) | ProviderStreamEmission::TerminalGroup { terminal: emission, .. } => {
                    emitted.push((
                        emission.artifact.read_manifest().unwrap().label,
                        serde_json::from_slice::<serde_json::Value>(&emission.artifact.read_payload_bytes().unwrap()).unwrap(),
                    ));
                }
                ProviderStreamEmission::TransportDispatch { admitted, .. } => admitted.send(Ok(())).unwrap(),
                _ => panic!("unexpected output family for an instruction turn"),
            }
        }
        emitted
    };
    let (invoked, emitted) = tokio::join!(invocation, collect);
    invoked.unwrap();
    let requests = std::fs::read_to_string(&requests).unwrap();
    let start: serde_json::Value = serde_json::from_str(
        requests.lines().find(|line| line.contains(r#""method":"turn/start""#)).expect("the instruction starts a native turn"),
    ).unwrap();
    assert_eq!(start["params"]["input"], serde_json::json!([]));
    assert_eq!(start["params"]["toolOutput"], serde_json::json!({
        "name":"octopus_instruction","namespace":"formed_interfaces","output":instruction,"retain":true,
    }));
    let labels = emitted.iter().map(|(label, _)| label.as_str()).collect::<Vec<_>>();
    assert!(!labels.contains(&TURN_ERROR_LABEL), "{labels:?}");
    assert!(emitted.iter().all(|(_, payload)| payload.get("item_id") != Some(&serde_json::json!("instruction-1"))),
        "the recorded instruction is turn input, not model activity: {labels:?}");
    let (_, completed) = emitted.iter().find(|(label, _)| label == TURN_COMPLETED_LABEL)
        .expect("the completed native turn is projected");
    assert_eq!(completed["turn"]["items"][0], item);
    let (_, answer) = emitted.iter().find(|(label, _)| label == INTERFACE_ARTIFACT_OUTPUT_LABEL)
        .expect("the instruction turn settles with its final answer");
    assert_eq!(answer["text"], "Nothing is open.");
    driver.shutdown().await.unwrap();
}
