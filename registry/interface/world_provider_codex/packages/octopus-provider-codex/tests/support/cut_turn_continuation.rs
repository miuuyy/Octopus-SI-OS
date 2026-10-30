#[tokio::test]
async fn a_turn_cut_by_process_death_continues_from_its_late_result() {
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
    let turns = serde_json::json!({"data":[{
        "id":"turn-1","items":[],"itemsView":"notLoaded","status":"interrupted","error":null,
    }],"nextCursor":null,"backwardsCursor":null}).to_string();
    let requests = temp.path().join("app-server-requests.jsonl");
    let script = temp.path().join("app-server.sh");
    std::fs::write(&script, r#"#!/bin/sh
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$1"
  id=$(printf '%s\n' "$line" | sed -n 's/^{"id":\([^,]*\),.*/\1/p')
  case "$line" in
    *'"method":"initialize"'*) printf '{"id":%s,"result":{"userAgent":"codex/test","codexHome":"/tmp"}}\n' "$id" ;;
    *'"method":"thread/read"'*) printf '{"id":%s,"result":%s}\n' "$id" "$2" ;;
    *'"method":"thread/turns/list"'*) printf '{"id":%s,"result":%s}\n' "$id" "$3" ;;
    *'"method":"thread/items/list"'*) printf '{"id":%s,"result":{"data":[],"nextCursor":null,"backwardsCursor":null}}\n' "$id" ;;
    *'"method":"thread/resume"'*) printf '{"id":%s,"result":%s}\n' "$id" "$4" ;;
    *'"method":"hooks/list"'*) printf '{"id":%s,"result":{"data":[]}}\n' "$id" ;;
    *'"method":"turn/start"'*)
      printf '{"id":%s,"result":{"turn":{"id":"turn-continued","items":[],"itemsView":"notLoaded","status":"inProgress","error":null}}}\n' "$id"
      printf '%s\n' '{"id":"server-tool-2","method":"item/tool/call","params":{"threadId":"thr-exact","turnId":"turn-continued","callId":"call-2","namespace":"formed_interfaces","tool":"coder-dexec","arguments":{"command":"echo $$"}}}' ;;
    '{"id":'*) printf '{"id":%s,"error":{"code":-32601,"message":"unexpected request"}}\n' "$id" ;;
  esac
done
"#).unwrap();
    let spawned = crate::SpawnAppServerClient::connect(crate::spawn::SpawnConfig {
        bin: "/bin/sh".into(),
        args: vec![script.display().to_string(), requests.display().to_string(), read, turns, resume],
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
    let mut pending = continuity("world_provider_codex", "run-a", "thr-exact", 7, Some(CodexPendingContinuity::DynamicTool {
        native_operation: None, turn_id: "turn-1".into(), call_id: "call-1".into(), tool: "coder.exec".into(),
    }));
    pending.static_context_fingerprint = sha256_hex(&static_bytes);
    pending.thread_shape_fingerprint = thread_shape_fingerprint(
        Some("gpt-5.5"), "role", &native_affordances, &pending.static_context_fingerprint,
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
    metadata.grant.role_artifacts = vec!["role-instruction".into()];
    let read_access = serde_json::json!({"read":true,"change":false,"create":false,"delete":false});
    metadata.grant.artifact_access.insert("role-instruction".into(), serde_json::from_value(read_access.clone()).unwrap());
    let runtime = metadata.runtime.clone().unwrap();
    let mut admitted = serde_json::to_value(octopus_run_record::RunRecord {
        id: runtime.run.clone(), ee: runtime.ee.clone(), worker: runtime.worker.clone(),
        trace: octopus_runtime::TraceId("trace:cut-turn".into()), status: serde_json::json!("running"),
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
        "id":"checkpoint-a","run":"run-a","trace":"trace:cut-turn","epoch":1,
        "interface_binding":octopus_runtime::AuthoredInterfaceBinding::all(InterfaceId::new("system_coder")),
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
        "operator-task", "operator.message", serde_json::json!({"text": "restart the runtime and report its new pid"}), 1,
    )).unwrap();
    let carrier = bound("pending-tool-call", serde_json::json!({
        "label": "interface.artifact.output", CODEX_CONTINUITY_FIELD: pending,
        "session_context": {"schema_version": 3, "provider_interface": "world_provider_codex", "artifacts": [earlier.clone()]},
    }));
    let snapshot = ProviderInvocationSessionSnapshot { carrier: carrier.artifact.clone(), artifacts: Arc::new(vec![earlier]) };
    let result_bytes = serde_json::to_vec(&serde_json::json!({
        "label": "coder.exec.result", "tool": "coder.exec", "correlation_id": "call-1", "ok": true,
        "body": {"output": "restarted · pid 4242 · phase ready\nSURVIVED\n", "exit_code": 0},
    })).unwrap();
    metadata.runtime.as_mut().unwrap().run_record_hash = Some(octopus_ledger::ContentHash::of_bytes(&admitted_record).to_hex());
    metadata.artifact_binding_refs = vec![carrier.artifact.clone(), role.artifact.clone()];
    let envelope = InterfaceInvocationEnvelope {
        artifact: ArtifactRef {
            id: octopus_descriptors::ArtifactId::new("cut-call-result"),
            content: octopus_ledger::ContentHash::of_bytes(&result_bytes),
        },
        artifact_file: InterfaceArtifactFile::write(temp.path().join("input"), "coder.exec.result", &result_bytes).unwrap(),
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
        let mut labels = Vec::new();
        let mut call = None;
        while let Some(output) = output_rx.recv().await {
            match output {
                ProviderStreamEmission::Turn(emission) | ProviderStreamEmission::Progress(emission) | ProviderStreamEmission::TerminalGroup { terminal: emission, .. } => {
                    let label = emission.artifact.read_manifest().unwrap().label;
                    if label == TOOL_CALL_LABEL {
                        call = Some(serde_json::from_slice::<serde_json::Value>(&emission.artifact.read_payload_bytes().unwrap()).unwrap());
                    }
                    labels.push(label);
                }
                ProviderStreamEmission::TransportDispatch { admitted, .. } => admitted.send(Ok(())).unwrap(),
                ProviderStreamEmission::InterruptedResult { .. } => panic!("a turn cut by process death must continue, not close"),
                _ => panic!("unexpected output family for a cut turn continuation"),
            }
        }
        (labels, call)
    };
    let (invoked, (labels, call)) = tokio::join!(invocation, collect);
    invoked.unwrap();
    assert!(!labels.iter().any(|label| label == TURN_INTERRUPTED_RESULT_LABEL), "{labels:?}");
    let requests = std::fs::read_to_string(&requests).unwrap();
    assert!(!requests.contains("thread/inject_items"), "a cut turn has no live call to inject into");
    let start: serde_json::Value = serde_json::from_str(
        requests.lines().find(|line| line.contains(r#""method":"turn/start""#)).expect("the provider starts the continuation itself"),
    ).unwrap();
    assert_eq!(start["params"]["threadId"], "thr-exact");
    assert_eq!(start["params"]["input"], serde_json::json!([]));
    assert_eq!(start["params"]["turnTrigger"], TURN_TRIGGER_RESUME_INTERRUPTED_TASK);
    let context = start["params"]["additionalContext"]["octopus_bound_context"]["value"].as_str().unwrap();
    assert!(context.contains("coder.exec") && context.contains("SURVIVED") && context.contains("pid 4242"), "{context}");
    let call = call.expect("the continued turn reaches its next call");
    assert_eq!(call["correlation_id"], "call-2");
    let continued: CodexContinuityState = serde_json::from_value(call[CODEX_CONTINUITY_FIELD].clone()).unwrap();
    assert_eq!(continued.thread_id, "thr-exact");
    assert!(continued.interrupted_pending.is_none() && continued.native_terminal.is_none());
    assert!(matches!(continued.pending, Some(CodexPendingContinuity::DynamicTool { ref call_id, .. }) if call_id == "call-2"));
    driver.shutdown().await.unwrap();
}
