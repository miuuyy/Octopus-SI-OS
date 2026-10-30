#[tokio::test]
async fn changed_native_tools_continue_from_the_exact_completed_effect() {
    use octopus_appserver_proto::{
        ApprovalsReviewer, AskForApproval, SandboxPolicy, SessionSource, Thread, ThreadStatus,
    };
    for (interface_result, native_effect, quarantined_successor, late_successor) in [
        (false, false, false, false), (true, false, false, false), (false, true, false, false),
        (false, false, true, false), (false, false, false, true),
    ] {
        let effect_tool = if native_effect { "artifact.send" } else { "file.write" };
        let temp = tempfile::tempdir().unwrap();
        let socket_path = temp.path().join("refresh.sock");
        let mut listener = crate::uds::UnixListener::bind(&socket_path).await.unwrap();
        let cwd = octopus_util_abspath::AbsolutePathBuf::from_absolute_path(temp.path()).unwrap();
        let source_thread = Thread {
        id:"old-thread".into(), session_id:"old-thread".into(), forked_from_id:None, parent_thread_id:None,
        preview:String::new(), ephemeral:false, model_provider:"openai".into(), created_at:1, updated_at:1,
        recency_at:Some(1), status:ThreadStatus::Idle, path:None, cwd:cwd.clone(), cli_version:"test".into(),
        source:SessionSource::Exec, thread_source:None, agent_nickname:None, agent_role:None, git_info:None, name:None,
        turns:vec![serde_json::from_value(serde_json::json!({"id":"old-turn","items":[],"itemsView":"full","status":"inProgress","error":null})).unwrap()],
    };
        let server = tokio::spawn(async move {
            use futures::{SinkExt, StreamExt};
            let stream = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let mut retired = false;
            let mut methods = Vec::new();
            while let Some(Ok(frame)) = socket.next().await {
                let tokio_tungstenite::tungstenite::Message::Text(frame) = frame else { continue; };
                let octopus_appserver_proto::JSONRPCMessage::Request(request) =
                    serde_json::from_str(&frame).unwrap() else {
                    continue;
                };
                methods.push(request.method.clone());
                let response = match request.method.as_str() {
                    "initialize" => serde_json::json!({"userAgent":"codex/test","codexHome":cwd}),
                    "thread/read" => {
                        assert_eq!(request.params.as_ref().unwrap(), &serde_json::json!({"threadId":"old-thread"}));
                        let mut thread = source_thread.clone();
                        thread.turns.clear();
                        serde_json::json!({"thread":thread})
                    }
                    "thread/turns/list" => {
                        let params = request.params.as_ref().unwrap();
                        let cursor = params["cursor"].as_str();
                        assert_eq!(params, &serde_json::json!({
                            "threadId":"old-thread", "cursor":cursor, "limit":100,
                            "sortDirection":"desc", "itemsView":"notLoaded",
                        }));
                        let mut turn = source_thread.turns[0].clone();
                        turn.items.clear();
                        turn.items_view = TurnItemsView::NotLoaded;
                        let next = match cursor {
                            None => {
                                turn.id = "newer-turn".into();
                                Some("older")
                            }
                            Some("older") => {
                                if retired { turn.status = TurnStatus::Interrupted; }
                                None
                            }
                            other => panic!("unexpected turn cursor: {other:?}"),
                        };
                        serde_json::json!({"data":[turn],"nextCursor":next,"backwardsCursor":null})
                    }
                    "thread/items/list" => {
                        assert!(retired, "active turn metadata must not hydrate terminal items");
                        assert_eq!(request.params.as_ref().unwrap(), &serde_json::json!({
                            "threadId":"old-thread", "turnId":"old-turn", "cursor":null,
                            "limit":100, "sortDirection":"asc",
                        }));
                        serde_json::json!({"data":[],"nextCursor":null,"backwardsCursor":null})
                    }
                    "turn/interrupt" => {
                        retired = true;
                        send_test_result(&mut socket, request.id, serde_json::json!({})).await;
                        if quarantined_successor {
                            socket.send(tokio_tungstenite::tungstenite::Message::Text(serde_json::json!({
                                "id":"old-successor-request","method":"item/tool/call","params":{
                                    "threadId":"old-thread","turnId":"old-turn","callId":"old-successor",
                                    "namespace":"formed_interfaces","tool":"new-dcall","arguments":{}}
                            }).to_string().into())).await.unwrap();
                        }
                        let mut turn = source_thread.turns[0].clone();
                        turn.status = TurnStatus::Interrupted;
                        socket.send(tokio_tungstenite::tungstenite::Message::Text(serde_json::json!({
                        "method":"turn/completed","params":{"threadId":"old-thread","turn":turn}
                    }).to_string().into())).await.unwrap();
                        continue;
                    }
                    "thread/start" => {
                        let params = request.params.as_ref().unwrap();
                        assert!(params["dynamicTools"].to_string().contains("new-dcall"));
                        assert_eq!(params["developerInstructions"], "updated role");
                        let mut thread = source_thread.clone();
                        thread.id = "new-thread".into();
                        thread.session_id = thread.id.clone();
                        thread.turns.clear();
                        serde_json::to_value(ThreadStartResponse {
                            thread,
                            model: "gpt-5.5".into(),
                            model_provider: "openai".into(),
                            service_tier: None,
                            cwd: cwd.clone(),
                            runtime_workspace_roots: vec![],
                            instruction_sources: vec![],
                            approval_policy: AskForApproval::OnFailure,
                            approvals_reviewer: ApprovalsReviewer::User,
                            sandbox: SandboxPolicy::DangerFullAccess,
                            active_permission_profile: None,
                            reasoning_effort: None,
                            multi_agent_mode: None,
                        })
                        .unwrap()
                    }
                    "turn/start" => {
                        let params = request.params.as_ref().unwrap();
                        assert_eq!(params["threadId"], "new-thread");
                        assert_eq!(params["input"], serde_json::json!([]));
                        let context = params["additionalContext"].to_string();
                        assert!(context.contains("effect-performed-once"));
                        assert!(context.contains("original operator task"));
                        assert!(context.contains("freshly admitted grant"));
                        let mut turn = source_thread.turns[0].clone();
                        turn.id = "new-turn".into();
                        send_test_result(&mut socket, request.id, serde_json::json!({"turn":turn}))
                            .await;
                        if late_successor {
                            socket.send(tokio_tungstenite::tungstenite::Message::Text(serde_json::json!({
                                "id":"late-retired-request","method":"item/tool/call","params":{
                                    "threadId":"old-thread","turnId":"old-turn","callId":"late-retired-call",
                                    "namespace":"formed_interfaces","tool":"new-dcall","arguments":{}}
                            }).to_string().into())).await.unwrap();
                            let response = socket.next().await.unwrap().unwrap();
                            let response: serde_json::Value = serde_json::from_str(response.to_text().unwrap()).unwrap();
                            assert_eq!(response["id"], "late-retired-request");
                            assert_eq!(response["error"]["data"]["external_effect_dispatched"], false);
                        }
                        socket.send(tokio_tungstenite::tungstenite::Message::Text(serde_json::json!({
                        "id":"new-call-request","method":"item/tool/call","params":{
                            "threadId":"new-thread","turnId":"new-turn","callId":"new-call-id",
                            "namespace":"formed_interfaces","tool":"new-dcall","arguments":{}}
                    }).to_string().into())).await.unwrap();
                        while socket.next().await.is_some() {}
                        return methods;
                    }
                    other => panic!("unexpected provider operation: {other}"),
                };
                send_test_result(&mut socket, request.id, response).await;
            }
            methods
        });
        let client = crate::RemoteAppServerClient::connect(crate::RemoteAppServerConnectArgs {
            endpoint: crate::RemoteAppServerEndpoint::UnixSocket {
                socket_path: octopus_util_abspath::AbsolutePathBuf::from_absolute_path(
                    &socket_path,
                )
                .unwrap(),
            },
            client_name: "octopus-test".into(),
            client_version: "0".into(),
            experimental_api: false,
            mcp_server_openai_form_elicitation: false,
            opt_out_notification_methods: vec![],
            channel_capacity: 16,
        })
        .await
        .unwrap();
        let config = InterfaceDriverConfig { private_config:serde_json::json!({
        "private_thread_cold_start":"resume_compatible", "native_tool_namespace":{"name":"formed_interfaces","description":"tools"},
        "model_result_projection":{"max_tokens":10000}
    }).as_object().unwrap().clone(), ..InterfaceDriverConfig::default() };
        let driver = CodexAppServerInterfaceDriver::new_with_config(
            AppServerClient::Remote(client),
            Some(&config),
        );
        let scope = TransportStateScope::Run("run-a".into());
        let pending = if interface_result {
            CodexPendingContinuity::InterfaceOperation {
                turn_id: "old-turn".into(),
                call_id: "write-call".into(),
                operation: "file.write".into(),
                interface: "system_files".into(),
                output_label: "file.write.result".into(),
            }
        } else {
            CodexPendingContinuity::DynamicTool {
                native_operation: if native_effect { Some(serde_json::json!(NativePending {
                    call_id:"native-create-call".into(), operation_name:"create_run".into(), source_run:"run-a".into(),
                    effect:crate::run_affordances::NativeEffect { tool:effect_tool.into(), correlation_id:"write-call".into(), arguments:serde_json::json!({}) },
                    state:crate::run_affordances::NativePhase::AppliedSeeding {
                        created:serde_json::json!({"run_id":"run-b"}), files:vec![],
                        write_receipt:serde_json::json!({"receipt":"effect-performed-once"}), validation:serde_json::json!({"ok":true}),
                    },
                })) } else { None },
                turn_id: "old-turn".into(),
                call_id: "write-call".into(),
                tool: effect_tool.into(),
            }
        };
        let original = continuity(
            "world_provider_codex",
            "run-a",
            "old-thread",
            7,
            Some(pending),
        );
        let effect_gate = Arc::new(tokio::sync::Mutex::new(()));
        let interrupted = Arc::new(std::sync::atomic::AtomicBool::new(false));
        driver.transport_state.lock().await.insert(
            scope.clone(),
            TransportState {
                context_generation: 7,
                hooks_catalog_emitted: true,
                thread_id: Some("old-thread".into()),
                thread_shape_fingerprint: Some(original.thread_shape_fingerprint.clone()),
                static_context_fingerprint: Some(original.static_context_fingerprint.clone()),
                rollout_path: None,
            },
        );
        driver
            .record_drain_outcome(
                &scope,
                TurnDrainOutcome::Suspended(PendingServerRequest {
                    request_id: RequestId::String("write-request".into()),
                    thread_id: "old-thread".into(),
                    turn_id: "old-turn".into(),
                    key: "tool:write-call".into(),
                    kind: if interface_result {
                        PendingServerRequestKind::InterfaceOperation {
                            call_id: "write-call".into(),
                            operation: "file.write".into(),
                            interface: "system_files".into(),
                            output_label: "file.write.result".into(),
                        }
                    } else {
                        PendingServerRequestKind::DynamicTool {
                            call_id: "write-call".into(),
                            tool: effect_tool.into(),
                        }
                    },
                    effect_gate: effect_gate.clone(),
                    turn_interrupted: interrupted.clone(),
                }),
            )
            .await
            .unwrap();
        let (emit, mut rx) = ProviderOutputStream::channel();
        let labels = codex_context_labels();
        let projected = codex_client_projection_labels();
        let rules = live_stream_rules();
        let streams = temp.path().join("streams");
        let interface = InterfaceId::new("world_provider_codex");
        let sink = TurnSink {
            emit: &emit,
            output_root: temp.path(),
            live_stream_root: &streams,
            provider_interface: &interface,
            run: "run-a",
            source_movement_seq: 20,
            attempt: 1,
            context_output_labels: &labels,
            client_projection_output_labels: &projected,
            live_stream_rules: &rules,
            live_stream: std::sync::Mutex::new(None),
            continuity: emit.continuity.clone(),
        };
        sink.set_continuity(original.clone());
        sink.continuity.lock().unwrap().pending_input_context = true;
        let source_artifact = ArtifactRef {
            id: "exact-write-result".into(),
            content: octopus_ledger::ContentHash::of_bytes(b"receipt"),
        };
        let control = CodexTurnControlBroker::new(Some(20));
        let barrier = ProviderHandoffBarrier::new();
        let operations = BTreeMap::new();
        let names = BTreeMap::from([("new-dcall".into(), "new.call".into())]);
        let context = TurnContext {
            run_invocation: None,
            scope: &scope,
            sink: &sink,
            control: &control,
            interface_operations: &operations,
            tool_names: &names,
            source_artifact: &source_artifact,
            handoff_barrier: &barrier,
            effect_gate: &effect_gate,
            turn_interrupted: &interrupted,
            lifecycle: None,
            lifecycle_prompt: None,
        };
        let tools = namespace_native_affordances(
            vec![DynamicToolSpec::Function(DynamicToolFunctionSpec {
                name: "new-dcall".into(),
                description: "new capability".into(),
                input_schema: serde_json::json!({"type":"object"}),
                defer_loading: false,
            })],
            &NativeToolNamespace {
                name: "formed_interfaces".into(),
                description: "tools".into(),
            },
        )
        .unwrap();
        let static_hash = "c".repeat(64);
        let current = || CurrentNativeContext {
            model: "gpt-5.5",
            instructions: "updated role",
            tools: &tools,
            static_fingerprint: &static_hash,
            conversation: Some("original operator task"),
            obligations: Some("freshly admitted grant"),
            session_available: true,
            reasoning_effort: None,
            reasoning_summary: ReasoningSummary::None,
            recovered_dispatches: &[],
        };
        let result = |call: &str| {
            if interface_result {
                ProviderInput::InterfaceOperationResult(InterfaceOperationResultArtifact {
                    label: "file.write.result".into(),
                    operation: "file.write".into(),
                    correlation_id: call.into(),
                    ok: true,
                    value: serde_json::json!({"label":"file.write.result","operation":"file.write","correlation_id":call,"ok":true,"result":{"receipt":"effect-performed-once"}}),
                })
            } else {
                ProviderInput::ToolResult(serde_json::from_value(serde_json::json!({
            "label":format!("{effect_tool}.result"),"tool":effect_tool,"correlation_id":call,"ok":true,
            "body":if native_effect { serde_json::json!({"admitted":{"seq":21}}) } else { serde_json::json!({"receipt":"effect-performed-once"}) }
        })).unwrap())
            }
        };
        assert!(
            driver
                .refresh_native_context(
                    context,
                    &result("wrong-call"),
                    Some(&original),
                    7,
                    current()
                )
                .await
                .is_err()
        );
        let admitted_result = result("write-call");
        let receive = async {
            driver
                .refresh_native_context(context, &admitted_result, Some(&original), 7, current())
                .await
        };
        let collect = async {
            let mut phases = Vec::new();
            let mut prepared: Option<CodexContinuityState> = None;
            loop {
                let emission = rx.recv().await.unwrap();
                match emission {
                    ProviderStreamEmission::ContextBoundary {
                        emissions,
                        admitted,
                    } => {
                        let value: serde_json::Value = serde_json::from_slice(
                            &emissions
                                .last()
                                .unwrap()
                                .artifact
                                .read_payload_bytes()
                                .unwrap(),
                        )
                        .unwrap();
                        let marker = parse_codex_continuity(
                            &value[CODEX_CONTINUITY_FIELD],
                            &interface,
                            "run-a",
                        )
                        .unwrap();
                        let phase = value["context_refresh_phase"].as_str().unwrap().to_string();
                        assert_eq!(
                            codex_continuity_successor_is_settled(Some(&original), Some(&marker)).unwrap(),
                            late_successor && phase == "retired_request_rejected",
                        );
                        if phase == "retired_request_rejected" {
                            assert_eq!(value["rejected_native_request"]["params"]["threadId"], "old-thread");
                            assert_eq!(value["rejection"]["data"]["external_effect_dispatched"], false);
                        }
                        if phase == "prepared" {
                            assert_eq!(serde_json::to_value(marker.context_refresh.as_ref().unwrap()).unwrap()["completed_output"]["call_id"],
                                if native_effect { "native-create-call" } else { "write-call" });
                            prepared = Some(marker.clone());
                        }
                        if phase == "retired" {
                            assert_eq!(emissions.len(), 2);
                            let previous = prepared.as_ref().unwrap();
                            let stream_manifest_path = emissions[0].artifact.manifest_path();
                            let original_manifest = std::fs::read(stream_manifest_path).unwrap();
                            for missing_manifest_hash in [false, true] {
                                if missing_manifest_hash {
                                    let mut unfinished = emissions[0].artifact.read_manifest().unwrap();
                                    unfinished.payload_content_hash = None;
                                    std::fs::write(stream_manifest_path, serde_json::to_vec_pretty(&unfinished).unwrap()).unwrap();
                                }
                                sink.set_continuity(previous.clone());
                                let (restored, exact) = sink.recover_context_retirement(previous.context_refresh.as_ref().unwrap()).unwrap().unwrap();
                                assert_eq!(restored, marker);
                                assert_eq!(std::fs::read(stream_manifest_path).unwrap(), original_manifest);
                                for (before, after) in emissions.iter().zip(exact.iter()) {
                                    assert_eq!(before.artifact.read_payload_bytes().unwrap(), after.artifact.read_payload_bytes().unwrap());
                                }
                            }
                            sink.set_continuity(marker.clone());
                        }
                        phases.push(phase);
                        admitted.send(Ok(())).unwrap();
                    }
                    ProviderStreamEmission::TransportDispatch { admitted, .. } => {
                        admitted.send(Ok(())).unwrap();
                    }
                    ProviderStreamEmission::Turn(emission) | ProviderStreamEmission::Progress(emission) => {
                        let value: serde_json::Value = serde_json::from_slice(
                            &emission.artifact.read_payload_bytes().unwrap(),
                        )
                        .unwrap();
                        if value.get("tool") == Some(&serde_json::json!("new.call")) {
                            assert_eq!(value["correlation_id"], "new-call-id");
                            let state = parse_codex_continuity(
                                &value[CODEX_CONTINUITY_FIELD],
                                &interface,
                                "run-a",
                            )
                            .unwrap();
                            assert!(
                                !codex_continuity_successor_is_settled(
                                    Some(&original),
                                    Some(&state)
                                )
                                .unwrap()
                            );
                            let mut settled = state.clone();
                            settled.pending = None;
                            assert!(
                                codex_continuity_successor_is_settled(
                                    Some(&original),
                                    Some(&settled)
                                )
                                .unwrap()
                            );
                            let mut forged = settled.clone();
                            forged.context_refresh = None;
                            assert!(
                                codex_continuity_successor_is_settled(
                                    Some(&original),
                                    Some(&forged)
                                )
                                .is_err()
                            );
                            assert!(
                                reconcile_current_session_continuity(
                                    Some(original.clone()),
                                    Some(state)
                                )
                                .is_ok()
                            );
                            return phases;
                        }
                    }
                    _ => panic!("refresh must not emit an interrupted task or terminal reply"),
                }
            }
        };
        let (finished, phases) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            tokio::join!(receive, collect)
        })
        .await
        .unwrap();
        assert!(finished.expect("context refresh"));
        let mut expected = vec!["prepared", "retired"];
        if quarantined_successor { expected.push("retired_request_rejected"); }
        expected.push("ready");
        if late_successor { expected.push("retired_request_rejected"); }
        assert_eq!(phases, expected);
        assert!(
            !driver
                .pending_server_requests
                .lock()
                .await
                .contains_key(&(scope.clone(), "tool:write-call".into()))
        );
        assert!(
            driver
                .pending_request(&scope, "tool:new-call-id")
                .await
                .unwrap()
                .is_some()
        );
        for (thread, turn) in [("other-thread", "old-turn"), ("old-thread", "other-turn")] {
            let request: ServerRequest = serde_json::from_value(serde_json::json!({
                "id":"unproven-request","method":"item/tool/call","params":{
                    "threadId":thread,"turnId":turn,"callId":"unproven-call",
                    "namespace":"formed_interfaces","tool":"new-dcall","arguments":{}}
            })).unwrap();
            assert!(!driver.reject_retired_context_request(&sink, &*driver.client.lock().await, &request).await.unwrap());
        }
        driver.shutdown().await.unwrap();
        assert_eq!(
            server.await.unwrap(),
            [
                "initialize",
                "thread/read",
                "thread/turns/list",
                "thread/turns/list",
                "thread/read",
                "thread/turns/list",
                "thread/turns/list",
                "turn/interrupt",
                "thread/read",
                "thread/turns/list",
                "thread/turns/list",
                "thread/items/list",
                "thread/start",
                "turn/start"
            ]
        );
    }
}

fn interrupted_native_history_fixture(root: &Path) -> (PathBuf, CodexContinuityState, CodexContinuityState, Vec<SessionContextArtifact>) {
    let path = root.join("native-history.jsonl");
    let rows = [
        serde_json::json!({"type":"session_meta","payload":{"id":"old-thread"}}),
        serde_json::json!({"type":"turn_context","payload":{"turn_id":"old-turn"}}),
        serde_json::json!({"type":"response_item","payload":{"type":"custom_tool_call","call_id":"outer-exec","name":"exec","input":"const result = await tools.interval_union({intervals:[]}); text(result); text(await tools.artifact_list({}));"}}),
    ];
    std::fs::write(&path, rows.iter().map(|row| format!("{row}\n")).collect::<String>()).unwrap();
    let original = continuity("world_provider_codex", "run-a", "old-thread", 7, Some(CodexPendingContinuity::DynamicTool {
        native_operation: None, turn_id:"old-turn".into(), call_id:"inner-effect".into(), tool:"interval.union".into(),
    }));
    let result: SessionContextArtifact = serde_json::from_value(session_artifact("result", "interval.union.result", serde_json::json!({
        "label":"interval.union.result", "tool":"interval.union", "correlation_id":"inner-effect", "ok":true,
        "body":{"total_length":0,"merged_intervals":[]},
    }), 11)).unwrap();
    let mut closed = original.clone(); closed.pending = None; closed.context_generation = 8;
    closed.native_terminal = Some(NativeTerminalEvidence { thread_id:"old-thread".into(), turn:serde_json::json!({
        "id":"old-turn", "items":[], "itemsView":"full", "status":"interrupted", "error":null,
    }) });
    let mut closure = CodexTurnOutput::interrupted_result_closed("tool_result", "inner-effect".into(), "old-thread".into(), "old-turn".into(), true, &result.source_artifact);
    closed.native_terminal.as_ref().unwrap().apply_disposition(&mut closure).unwrap();
    closure.value["label"] = serde_json::json!(TURN_INTERRUPTED_RESULT_LABEL);
    closure.value["native_terminal_source_continuity"] = serde_json::json!(original);
    closure.value["result_origin"] = serde_json::json!("native_turn_completion");
    closure.value["terminal_evidence_source"] = serde_json::json!("thread/read");
    closure.value["native_terminal_input_context"] = serde_json::json!(false);
    let mut retained_state = closed.clone(); retained_state.context_generation = 7;
    let retained = closed.native_terminal.as_ref().unwrap().outputs(&[]).unwrap().into_iter().map(|mut output| {
        output.value["label"] = serde_json::json!(output.label);
        output.value[CODEX_CONTINUITY_FIELD] = serde_json::json!(retained_state);
        output.value
    }).collect::<Vec<_>>();
    closure.value["retained_native_outputs"] = serde_json::json!(retained);
    closure.value[CODEX_CONTINUITY_FIELD] = serde_json::json!(closed);
    let closure = serde_json::from_value(session_artifact("closure", TURN_INTERRUPTED_RESULT_LABEL, closure.value, 12)).unwrap();
    (path, original, closed, vec![result, closure])
}

#[tokio::test]
async fn settled_native_history_reseeds_only_after_exact_result_proof() {
    use octopus_appserver_proto::{ApprovalsReviewer, AskForApproval, SandboxPolicy, SessionSource, Thread, ThreadStatus};
    use futures::StreamExt;
    let temp = tempfile::tempdir().unwrap();
    let (path, original, mut settled, session) = interrupted_native_history_fixture(temp.path());
    let bytes = std::fs::read(&path).unwrap();
    let labels = codex_context_labels();
    let prove = |state: &CodexContinuityState, entries: &[SessionContextArtifact]| context_refresh::prove_native_history_retirement(&path, state, entries, &labels);
    assert!(prove(&settled, &session).unwrap().is_some());
    assert!(prove(&original, &session).unwrap().is_none());
    assert!(prove(&settled, &session[..1]).is_err());
    assert!(prove(&settled, &session[1..]).is_err());
    for (index, field, value) in [(0,"correlation_id",serde_json::json!("different-effect")), (0,"ok",serde_json::json!(false)), (1,"turn_id",serde_json::json!("other-turn"))] {
        let mut wrong = session.clone(); wrong[index].payload[field] = value;
        assert!(prove(&settled, &wrong).is_err());
    }
    let mut unadmitted = session.clone(); unadmitted[0].source_movement_seq = None;
    assert!(prove(&settled, &unadmitted).is_err());
    let mut rows=String::from_utf8(bytes.clone()).unwrap().lines().map(|line|serde_json::from_str::<serde_json::Value>(line).unwrap()).collect::<Vec<_>>();
    rows[2]["payload"]=serde_json::json!({"type":"function_call","call_id":"inner-effect","name":"interval_union","arguments":"{\"intervals\":[]}"});
    std::fs::write(&path,rows.iter().map(|row|format!("{row}\n")).collect::<String>()).unwrap();
    assert!(prove(&settled,&session).unwrap().is_some());
    rows[2]["payload"]["call_id"]=serde_json::json!("unrelated-function");
    std::fs::write(&path,rows.iter().map(|row|format!("{row}\n")).collect::<String>()).unwrap();
    assert!(prove(&settled,&session).unwrap_err().to_string().contains("exact pending correlation"));
    let mut rows=String::from_utf8(bytes).unwrap().lines().map(|line|serde_json::from_str::<serde_json::Value>(line).unwrap()).collect::<Vec<_>>();
    rows.push(serde_json::json!({"type":"response_item","payload":{"type":"custom_tool_call_output","call_id":"outer-exec","output":"Script running with cell ID 1\nWall time 4.1 seconds\nOutput:\n"}}));
    rows.push(serde_json::json!({"type":"response_item","payload":{"type":"function_call","call_id":"outer-wait","name":"wait","arguments":"{\"cell_id\":\"1\",\"max_tokens\":12000,\"yield_time_ms\":20000}"}}));
    let write_rows = |rows: &[serde_json::Value]| std::fs::write(&path,rows.iter().map(|row|format!("{row}\n")).collect::<String>()).unwrap();
    write_rows(&rows);
    let proof = prove(&settled,&session).unwrap().unwrap();
    let serialized = serde_json::to_value(&proof).unwrap();
    assert_eq!(serialized["calls"][0]["interrupted_cell_wait"]["exec"], rows[2]["payload"]);
    for (index, field, value) in [
        (4,"arguments",serde_json::json!("{\"cell_id\":\"other\"}")),
        (4,"arguments",serde_json::json!("{\"cell_id\":\"1\",\"terminate\":true}")),
        (4,"arguments",serde_json::json!("{\"cell_id\":\"1\",\"other\":true}")),
        (4,"namespace",serde_json::json!("other")), (4,"name",serde_json::json!("other")),
        (3,"call_id",serde_json::json!("other")), (2,"name",serde_json::json!("other")),
    ] {
        let mut wrong=rows.clone(); wrong[index]["payload"][field]=value; write_rows(&wrong);
        assert!(prove(&settled,&session).is_err(),"{index}/{field}");
    }
    let mut late=rows.clone(); late.swap(3,4); write_rows(&late);
    assert!(prove(&settled,&session).is_err());
    let mut foreign=rows.clone(); foreign.insert(4,serde_json::json!({"type":"turn_context","payload":{"turn_id":"other-turn"}})); write_rows(&foreign);
    assert!(prove(&settled,&session).is_err());
    let mut more=rows.clone(); let mut exec=rows[2].clone(); exec["payload"]["call_id"]=serde_json::json!("second-exec");
    let mut yielded=rows[3].clone(); yielded["payload"]["call_id"]=serde_json::json!("second-exec");
    yielded["payload"]["output"]=serde_json::json!("Script running with cell ID 2\nWall time 1.0 seconds\nOutput:\n");
    more.splice(4..4,[exec,yielded]); write_rows(&more); assert!(prove(&settled,&session).is_err());
    let mut prior=rows.clone(); let mut wait=rows[4].clone(); wait["payload"]["call_id"]=serde_json::json!("prior-wait");
    let yielded=serde_json::json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"prior-wait","output":"Script running with cell ID 1\nWall time 1.0 seconds\nOutput:\n"}});
    prior.splice(4..4,[wait,yielded]); write_rows(&prior); assert!(prove(&settled,&session).unwrap().is_some());
    prior[5]["payload"]["output"]=serde_json::json!("Script completed\nWall time 1.0 seconds\nOutput:\n");
    write_rows(&prior); assert!(prove(&settled,&session).is_err());
    prior[5]["payload"]["output"]=serde_json::json!("unproven completion");
    write_rows(&prior); assert!(prove(&settled,&session).is_err());
    prior[4]["payload"]["arguments"]=serde_json::json!("{\"cell_id\":\"2\"}");
    prior[5]["payload"]["output"]=serde_json::json!("Script running with cell ID 2\nWall time 1.0 seconds\nOutput:\n");
    write_rows(&prior); assert!(prove(&settled,&session).is_err());
    let mut tampered=serialized; tampered["calls"][0]["interrupted_cell_wait"]["yielded"]["call_id"]=serde_json::json!("other");
    assert!(serde_json::from_value::<NativeHistoryProof>(tampered).unwrap().validate(&settled).is_err());
    write_rows(&rows); let bytes=std::fs::read(&path).unwrap();
    let socket_path = temp.path().join("retirement.sock");
    let mut listener = crate::uds::UnixListener::bind(&socket_path).await.unwrap();
    let cwd = octopus_util_abspath::AbsolutePathBuf::from_absolute_path(temp.path()).unwrap();
    let server = tokio::spawn(async move {
        let mut socket = tokio_tungstenite::accept_async(listener.accept().await.unwrap()).await.unwrap();
        let mut methods = Vec::new();
        while let Some(Ok(frame)) = socket.next().await {
            let tokio_tungstenite::tungstenite::Message::Text(frame) = frame else { continue; };
            let JSONRPCMessage::Request(request) = serde_json::from_str::<JSONRPCMessage>(&frame).unwrap() else { continue; };
            methods.push(request.method.clone());
            let thread = Thread { id:"replacement".into(), session_id:"replacement".into(), forked_from_id:None, parent_thread_id:None,
                preview:String::new(), ephemeral:false, model_provider:"openai".into(), created_at:1, updated_at:1, recency_at:Some(1), status:ThreadStatus::Idle, path:None,
                cwd:cwd.clone(), cli_version:"test".into(), source:SessionSource::Exec, thread_source:None, agent_nickname:None, agent_role:None, git_info:None, name:None, turns:vec![] };
            let response = match request.method.as_str() {
                "initialize" => serde_json::json!({"userAgent":"codex/test","codexHome":cwd}),
                "thread/start" => {
                    assert_eq!(request.params.as_ref().unwrap()["developerInstructions"], "bound role");
                    serde_json::to_value(ThreadStartResponse { thread, model:"gpt-5.5".into(), model_provider:"openai".into(), service_tier:None, cwd:cwd.clone(),
                        runtime_workspace_roots:vec![],instruction_sources:vec![],approval_policy:AskForApproval::OnFailure,approvals_reviewer:ApprovalsReviewer::User,
                        sandbox:SandboxPolicy::DangerFullAccess,active_permission_profile:None,reasoning_effort:None,multi_agent_mode:None }).unwrap()
                }
                "thread/resume" => {
                    assert_eq!(request.params.as_ref().unwrap()["threadId"], "replacement");
                    serde_json::to_value(ThreadResumeResponse { thread, model:"gpt-5.5".into(), model_provider:"openai".into(), service_tier:None, cwd:cwd.clone(),
                        runtime_workspace_roots:vec![],instruction_sources:vec![],approval_policy:AskForApproval::OnFailure,approvals_reviewer:ApprovalsReviewer::User,
                        sandbox:SandboxPolicy::DangerFullAccess,active_permission_profile:None,reasoning_effort:None,multi_agent_mode:None,initial_turns_page:None }).unwrap()
                }
                "thread/read" => serde_json::json!({"thread":thread}),
                other => panic!("retirement must not fabricate output or dispatch effects: {other}"),
            };
            send_test_result(&mut socket, request.id, response).await;
        }
        methods
    });
    let client = crate::RemoteAppServerClient::connect(crate::RemoteAppServerConnectArgs {
        endpoint:crate::RemoteAppServerEndpoint::UnixSocket { socket_path:octopus_util_abspath::AbsolutePathBuf::from_absolute_path(&socket_path).unwrap() },
        client_name:"octopus-test".into(),client_version:"0".into(),experimental_api:false,mcp_server_openai_form_elicitation:false,opt_out_notification_methods:vec![],channel_capacity:16,
    }).await.unwrap();
    let mut driver = CodexAppServerInterfaceDriver::new(AppServerClient::Remote(client));
    let scope = TransportStateScope::Run("run-a".into());
    let acquisition = |source, retired_history, session_context_available| ThreadAcquisition { recovered:Some(source), retired_history, require_exact_resume:false,
        context_generation:9,input_changes_context:true,model:Some("gpt-5.5".into()),bound_instructions:"bound role",native_affordances:&[],static_context_fingerprint:&settled.static_context_fingerprint,session_context_available };
    assert!(driver.acquire_transport_thread(&scope, acquisition(&settled,Some(&proof),true)).await.is_err());
    driver.interface_private_config.insert("private_thread_cold_start".into(), serde_json::json!("resume_compatible"));
    assert!(driver.acquire_transport_thread(&scope, acquisition(&original,Some(&proof),true)).await.is_err());
    assert!(driver.acquire_transport_thread(&scope, acquisition(&settled,Some(&proof),false)).await.is_err());
    let (replacement, cold, _) = driver.acquire_transport_thread(&scope, acquisition(&settled,Some(&proof),true)).await.unwrap();
    assert!(cold); assert_eq!(replacement,"replacement");
    let (emit, mut rx) = ProviderOutputStream::channel();
    let projected = codex_client_projection_labels(); let rules = live_stream_rules(); let streams = temp.path().join("streams");
    let interface = InterfaceId::new("world_provider_codex");
    let sink = TurnSink { emit:&emit,output_root:temp.path(),live_stream_root:&streams,provider_interface:&interface,run:"run-a",source_movement_seq:20,attempt:1,
        context_output_labels:&labels,client_projection_output_labels:&projected,live_stream_rules:&rules,live_stream:std::sync::Mutex::new(None),continuity:emit.continuity.clone() };
    let mut healthy = settled.clone(); healthy.native_terminal = None;
    assert!(driver.settled_native_history_proof(&scope,"old-thread",Some(&healthy),&[],&sink).await.unwrap().is_none());
    assert!(driver.settled_native_history_proof(&scope,"old-thread",Some(&healthy),&session,&sink).await.is_err());
    settled.native_history_retirement = Some(NativeHistoryRetirement { source_thread:settled.thread_id.clone(),source_generation:8,replacement_thread:replacement.clone(),
        source_artifact:session[0].source_artifact.clone(),source_movement_seq:20,proof });
    settled.thread_id = replacement;
    settled.thread_shape_fingerprint = thread_shape_fingerprint(Some("gpt-5.5"),"bound role",&[],&settled.static_context_fingerprint).unwrap();
    settled.native_terminal = None; sink.set_continuity(settled);
    let admit = async { sink.admit_native_history_retirement().await.unwrap(); };
    let collect = async {
        let ProviderStreamEmission::ContextBoundary { emissions, admitted } = rx.recv().await.unwrap() else { panic!("ordinary context boundary") };
        assert_eq!(emissions.len(),1);
        let value: serde_json::Value = serde_json::from_slice(&emissions[0].artifact.read_payload_bytes().unwrap()).unwrap();
        assert_eq!(value["label"], INTERFACE_CONTEXT_UPDATED_LABEL);
        let state = parse_codex_continuity(&value[CODEX_CONTINUITY_FIELD],&interface,"run-a").unwrap();
        assert_eq!(state.context_generation,9); assert!(state.pending.is_none());
        assert!(codex_continuity_successor_is_settled(Some(&original),Some(&state)).unwrap());
        let mut other = original.clone(); other.pending = Some(CodexPendingContinuity::DynamicTool { native_operation:None,turn_id:"old-turn".into(),call_id:"different-effect".into(),tool:"interval.union".into() });
        assert!(reconcile_current_session_continuity(Some(other),Some(state.clone())).is_err());
        admitted.send(Ok(())).unwrap(); state
    };
    let (_, restored) = tokio::join!(admit, collect);
    driver.transport_state.lock().await.clear();
    let acquired = driver.acquire_transport_thread(&scope,ThreadAcquisition { recovered:Some(&restored),retired_history:None,require_exact_resume:false,context_generation:10,input_changes_context:true,
        model:Some("gpt-5.5".into()),bound_instructions:"bound role",native_affordances:&[],static_context_fingerprint:&restored.static_context_fingerprint,session_context_available:true }).await.unwrap();
    assert_eq!(acquired.0,"replacement"); assert!(!acquired.1);
    assert_eq!(std::fs::read(&path).unwrap(),bytes);
    driver.shutdown().await.unwrap();
    assert_eq!(server.await.unwrap(),["initialize","thread/start","thread/resume","thread/read"]);
}

#[test]
#[ignore = "requires explicitly selected read-only native history and Home evidence"]
fn selected_home_retains_native_history_retirement_evidence() {
    let home = PathBuf::from(std::env::var("OCTOPUS_REPLAY_HOME").unwrap());
    let provider = InterfaceId::new(std::env::var("OCTOPUS_REPLAY_PROVIDER").unwrap());
    let run = octopus_descriptors::RunId::new(std::env::var("OCTOPUS_REPLAY_RUN").unwrap());
    let rollout = PathBuf::from(std::env::var("OCTOPUS_REPLAY_ROLLOUT").unwrap());
    assert!(rollout.starts_with(home.join("interfaces").join(provider.as_str())));
    let path = home.join("state/runs").join(format!("{run}.json"));
    let before = std::fs::read(&path).unwrap();
    let native_before = std::fs::read(&rollout).unwrap();
    let ledger_before = std::fs::read(home.join("ledger/movements.jsonl")).unwrap();
    let record = octopus_run_record::RunRecord::from_bytes(&before).unwrap();
    let descriptor: InterfaceDescriptor = serde_json::from_slice(&std::fs::read(home.join("registry/interface").join(provider.as_str()).join("interface.json")).unwrap()).unwrap();
    let life: serde_json::Value = serde_json::from_slice(&std::fs::read(home.join("registry/interface").join(provider.as_str()).join("life.json")).unwrap()).unwrap();
    let limits = serde_json::from_value(life["processes"][0]["io_limits"].clone()).unwrap();
    let config = &descriptor.interface_driver_config.as_ref().unwrap().private_config;
    let labels = private_config_string_set(config,"context_output_labels").unwrap();
    let snapshots = private_config_string_set(config,"session_snapshot_labels").unwrap();
    let mut session = Vec::new(); let mut current = None;
    for binding in octopus_run_authoring::provider_context_bindings(&record,&provider).unwrap() {
        let Some((_, hash)) = binding.id.as_str().rsplit_once(':') else { continue; };
        let Some(hash) = octopus_ledger::ContentHash::from_hex(hash) else { continue; };
        let reference = ArtifactRef { id:binding.id,content:hash };
        let manifest_bytes = std::fs::read(home.join("ledger/blobs").join(reference.content.to_hex())).unwrap();
        assert_eq!(octopus_ledger::ContentHash::of_bytes(&manifest_bytes), reference.content);
        let manifest: InterfaceArtifactManifest = serde_json::from_slice(&manifest_bytes).unwrap();
        let hash = manifest.payload_content_hash.unwrap();
        let bytes = std::fs::read(home.join("ledger/blobs").join(&hash)).unwrap();
        assert_eq!(octopus_ledger::ContentHash::of_bytes(&bytes).to_hex(),hash);
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        if let Some(raw) = value.get(CODEX_CONTINUITY_FIELD) {
            let state = parse_codex_continuity(raw,&provider,run.as_str()).unwrap();
            if current.as_ref().is_none_or(|prior: &CodexContinuityState| state.context_generation > prior.context_generation) { current=Some(state); }
        }
        if value.get(SESSION_CONTEXT_FIELD).is_some() {
            session = octopus_run_authoring::resolve_session_snapshot_chain(&home.join("ledger/blobs"),limits,&reference,&bytes,None,&snapshots,&provider).unwrap();
        }
    }
    let current = current.unwrap();
    assert_eq!(current.context_generation,record.interface_state[&provider]["context_generation"].as_u64().unwrap());
    assert!(codex_continuity_waits_for_result(&current));
    assert!(context_refresh::prove_native_history_retirement(&rollout,&current,&session,&labels).unwrap().is_none());
    let closure = session.iter().find(|entry| entry.source_movement_seq == Some(6592)).expect("exact closure retained in selected context");
    let closed = parse_codex_continuity(&closure.payload[CODEX_CONTINUITY_FIELD],&provider,run.as_str()).unwrap();
    assert!(context_refresh::prove_native_history_retirement(&rollout,&closed,&session,&labels).unwrap_err().to_string().contains("unfinished native function call"));
    let mut prefix = Vec::new(); let mut old_turn_seen = false;
    for line in native_before.split_inclusive(|byte| *byte==b'\n') {
        let row:serde_json::Value=serde_json::from_slice(line).unwrap();
        let turn = if row["type"]=="turn_context" || (row["type"]=="event_msg" && matches!(row["payload"]["type"].as_str(),Some("task_started"|"turn_started"))) { row["payload"]["turn_id"].as_str() } else { None };
        if let Some(turn)=turn {
            if old_turn_seen && turn != closure.payload["turn_id"].as_str().unwrap() { break; }
            old_turn_seen |= turn == closure.payload["turn_id"].as_str().unwrap();
        }
        prefix.extend_from_slice(line);
    }
    assert!(old_turn_seen && prefix.len() < native_before.len());
    let scratch=tempfile::tempdir().unwrap(); let prior_history=scratch.path().join("exact-interrupted-prefix.jsonl");
    std::fs::write(&prior_history,&prefix).unwrap();
    let proof = context_refresh::prove_native_history_retirement(&prior_history,&closed,&session,&labels).unwrap().unwrap();
    let proof = serde_json::to_value(proof).unwrap();
    assert_eq!(proof["calls"].as_array().unwrap().len(),1);
    assert_eq!(proof["calls"][0]["call"]["call_id"],"call_2bd265b9a61a4504bc1b9e67dff9e4e3");
    for (field, ref_field) in [("result_movement_seq","result_artifact"),("closure_movement_seq","closure_artifact")] {
        let seq = proof["calls"][0][field].as_u64().unwrap();
        let row: serde_json::Value = serde_json::from_slice(ledger_before.split(|byte| *byte==b'\n').find(|line| serde_json::from_slice::<serde_json::Value>(line).ok().is_some_and(|row| row["seq"]==seq)).unwrap()).unwrap();
        assert_eq!(row["artifact"],proof["calls"][0][ref_field]);
    }
    assert_eq!(std::fs::read(path).unwrap(),before);
    assert_eq!(std::fs::read(rollout).unwrap(),native_before);
    assert_eq!(std::fs::read(home.join("ledger/movements.jsonl")).unwrap(),ledger_before);
    eprintln!("Selected context {} retains exact result6566/closure6592; original native prefix proves its orphan. Full current history also has an unfinished function call; current pending prevents retirement",current.context_generation);
}
