#[tokio::test]
async fn two_outstanding_dynamic_calls_resolve_in_reverse_order() {
    use futures::{SinkExt, StreamExt};
    use octopus_appserver_proto::JSONRPCMessage;
    use serde_json::json;
    use std::time::Duration;
    use tokio_tungstenite::tungstenite::Message;

    let root = tempfile::tempdir().unwrap();
    let socket_path = root.path().join("parallel.sock");
    let mut listener = crate::uds::UnixListener::bind(&socket_path).await.unwrap();
    let server = tokio::spawn(async move {
        let mut socket = tokio_tungstenite::accept_async(listener.accept().await.unwrap())
            .await
            .unwrap();
        let JSONRPCMessage::Request(initialize) = next_test_rpc(&mut socket).await else {
            panic!("initialize")
        };
        send_test_result(&mut socket, initialize.id, json!({"userAgent":"codex/test","codexHome":"/tmp"})).await;
        let turn = json!({"id":"turn-a","items":[],"itemsView":"full","status":"inProgress","error":null});
        for message in [
            json!({"method":"turn/started","params":{"threadId":"thread-a","turn":turn}}),
            json!({"id":"request-a","method":"item/tool/call","params":{"threadId":"thread-a","turnId":"turn-a",
                "callId":"call-a","namespace":"formed_interfaces","tool":"coder-dexec","arguments":{"command":"a"}}}),
            json!({"id":"request-b","method":"item/tool/call","params":{"threadId":"thread-a","turnId":"turn-a",
                "callId":"call-b","namespace":"formed_interfaces","tool":"coder-dexec","arguments":{"command":"b"}}}),
        ] {
            socket.send(Message::Text(message.to_string().into())).await.unwrap();
        }
        let mut answered = Vec::new();
        while let Some(Ok(frame)) = socket.next().await {
            let Message::Text(frame) = frame else { continue; };
            match serde_json::from_str::<JSONRPCMessage>(&frame).unwrap() {
                JSONRPCMessage::Notification(notification) => assert_eq!(notification.method, "initialized"),
                JSONRPCMessage::Request(request) => {
                    let result = match request.method.as_str() {
                        "thread/read" => json!({"thread":{
                            "id":"thread-a","sessionId":"thread-a","forkedFromId":null,"parentThreadId":null,
                            "preview":"","ephemeral":false,"modelProvider":"fixture","createdAt":1,"updatedAt":1,
                            "recencyAt":1,"status":{"type":"idle"},"path":null,"cwd":"/tmp","cliVersion":"test",
                            "source":"exec","threadSource":null,"agentNickname":null,"agentRole":null,
                            "gitInfo":null,"name":null,"turns":[],
                        }}),
                        "thread/turns/list" => json!({"data":[turn],"nextCursor":null,"backwardsCursor":null}),
                        method => panic!("parallel calls issued an unexpected provider request '{method}'"),
                    };
                    send_test_result(&mut socket, request.id, result).await;
                }
                JSONRPCMessage::Response(response) => {
                    answered.push((response.id, response.result));
                    if answered.len() == 2 {
                        let answer = json!({"type":"agentMessage","id":"answer","text":"both","phase":"final_answer"});
                        let mut done = turn.clone();
                        done["status"] = json!("completed");
                        done["items"] = json!([answer]);
                        for message in [
                            json!({"method":"thread/tokenUsage/updated","params":{"threadId":"thread-a","turnId":"turn-a","tokenUsage":{
                                "total":{"totalTokens":100,"inputTokens":80,"cachedInputTokens":0,"outputTokens":20,"reasoningOutputTokens":0},
                                "last":{"totalTokens":100,"inputTokens":80,"cachedInputTokens":0,"outputTokens":20,"reasoningOutputTokens":0},
                                "modelContextWindow":32000
                            }}}),
                            json!({"method":"item/completed","params":{"threadId":"thread-a","turnId":"turn-a","item":answer,"completedAtMs":20}}),
                            json!({"method":"turn/completed","params":{"threadId":"thread-a","turn":done}}),
                        ] {
                            socket.send(Message::Text(message.to_string().into())).await.unwrap();
                        }
                    }
                }
                JSONRPCMessage::Error(error) => panic!("provider answered with an error: {error:?}"),
            }
        }
        answered
    });
    let client = crate::RemoteAppServerClient::connect(crate::RemoteAppServerConnectArgs {
        endpoint: crate::RemoteAppServerEndpoint::UnixSocket {
            socket_path: octopus_util_abspath::AbsolutePathBuf::from_absolute_path(&socket_path).unwrap(),
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
    let config = InterfaceDriverConfig {
        private_config: json!({
            "native_tool_namespace":{"name":"formed_interfaces","description":"tools"},
            "model_result_projection":{"max_tokens":4096},
        })
        .as_object()
        .unwrap()
        .clone(),
        ..InterfaceDriverConfig::default()
    };
    let driver = CodexAppServerInterfaceDriver::new_with_config(AppServerClient::Remote(client), Some(&config));
    let scope = TransportStateScope::Run("run-a".into());
    let initial = continuity("world_provider_codex", "run-a", "thread-a", 3, None);
    driver.transport_state.lock().await.insert(scope.clone(), TransportState {
        context_generation: 3,
        hooks_catalog_emitted: true,
        thread_id: Some("thread-a".into()),
        thread_shape_fingerprint: Some(initial.thread_shape_fingerprint.clone()),
        static_context_fingerprint: Some(initial.static_context_fingerprint.clone()),
        rollout_path: None,
    });
    let labels = codex_context_labels();
    let projected = codex_client_projection_labels();
    let rules = LiveStreamRules::from_private_config_or_disabled(&serde_json::Map::new()).unwrap();
    let streams = root.path().join("streams");
    let provider = InterfaceId::new("world_provider_codex");
    let control = CodexTurnControlBroker::new(Some(7));
    let barrier = ProviderHandoffBarrier::new();
    let gate = Arc::new(tokio::sync::Mutex::new(()));
    let interrupted = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let names = BTreeMap::from([("coder-dexec".to_string(), "coder.exec".to_string())]);
    let operations = BTreeMap::new();
    let source = bound("turn-input", json!({"text":"look up a and b"}));
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut ready = Vec::new();
        while ready.len() < 3 {
            ready.extend(driver.client.lock().await.take_ready_events());
            tokio::task::yield_now().await;
        }
        driver.client.lock().await.restore_ready_events(ready);
    })
    .await
    .unwrap();
    let calls_of = |state: &CodexContinuityState| state.outstanding().map(|pending| match pending {
        CodexPendingContinuity::DynamicTool { call_id, .. } => call_id.clone(),
        _ => panic!("parallel calls hold a non-tool wait"),
    }).collect::<Vec<_>>();
    let mut session = Vec::<SessionContextArtifact>::new();
    let mut seq = 7;
    let mut durable = initial;
    let mut emitted = Vec::new();
    for (index, answer) in [None, Some(("call-b", "b")), Some(("call-a", "a"))].into_iter().enumerate() {
        let (emit, mut outputs) = ProviderOutputStream::channel();
        emit.continuity.lock().unwrap().continuity = Some(durable.clone());
        let sink = TurnSink {
            emit: &emit, output_root: root.path(), live_stream_root: &streams, provider_interface: &provider,
            run: "run-a", source_movement_seq: seq, attempt: 1, context_output_labels: &labels,
            client_projection_output_labels: &projected, live_stream_rules: &rules,
            live_stream: std::sync::Mutex::new(None), continuity: emit.continuity.clone(),
        };
        let context = TurnContext {
            run_invocation: None, scope: &scope, sink: &sink, control: &control, interface_operations: &operations,
            tool_names: &names, source_artifact: &source.artifact, handoff_barrier: &barrier, effect_gate: &gate,
            turn_interrupted: &interrupted, lifecycle: None, lifecycle_prompt: None,
        };
        let settled = match answer {
            None => matches!(
                tokio::time::timeout(Duration::from_secs(5), driver.drain_turn(context, "thread-a", "turn-a")).await.unwrap().unwrap(),
                TurnDrainOutcome::AwaitingResults
            ),
            Some((call, output)) => {
                let result = ToolResultArtifact::success("coder.exec", Some(call.to_string()), json!({"output":output}));
                let payload = serde_json::to_value(&result).unwrap();
                let reference = octopus_interfaces::ArtifactRef {
                    id: octopus_descriptors::ArtifactId::new(format!("result-{call}")),
                    content: octopus_ledger::ContentHash::of_bytes(&serde_json::to_vec(&payload).unwrap()),
                };
                let recovered = recover_codex_continuity_from_current_session(
                    &session, std::slice::from_ref(&reference), &provider, "run-a", &labels,
                ).unwrap().unwrap();
                assert_eq!(calls_of(&recovered), if index == 1 { vec!["call-a", "call-b"] } else { vec!["call-a"] });
                emit.continuity.lock().unwrap().continuity = Some(recovered.clone());
                let settled = tokio::time::timeout(Duration::from_secs(5), driver.resume_tool_result(
                    context, result.clone(), Some(&recovered), recovered.context_generation, false,
                )).await.unwrap().unwrap();
                session.push(SessionContextArtifact {
                    label: result.label.clone(), payload, source_artifact: reference, source_movement_seq: Some(seq),
                });
                seq += 1;
                settled
            }
        };
        assert!(settled);
        durable = sink.continuity_snapshot().unwrap();
        drop(sink);
        drop(emit);
        while let Ok(output) = outputs.try_recv() {
            let (ProviderStreamEmission::Turn(emission) | ProviderStreamEmission::Progress(emission)) = output else {
                panic!("parallel calls emitted an unexpected provider output family")
            };
            let label = emission.artifact.read_manifest().unwrap().label;
            let value: serde_json::Value = serde_json::from_slice(&emission.artifact.read_payload_bytes().unwrap()).unwrap();
            if labels.contains(&label) {
                session.push(SessionContextArtifact {
                    label: label.clone(),
                    payload: value.clone(),
                    source_artifact: octopus_interfaces::ArtifactRef {
                        id: octopus_descriptors::ArtifactId::new(format!("emitted-{seq}")),
                        content: octopus_ledger::ContentHash::of_bytes(&serde_json::to_vec(&value).unwrap()),
                    },
                    source_movement_seq: Some(seq),
                });
                seq += 1;
            }
            emitted.push((index, label, value));
        }
    }
    let calls = emitted.iter().filter(|(index, label, _)| *index == 0 && label == TOOL_CALL_LABEL).collect::<Vec<_>>();
    assert_eq!(calls.len(), 2, "both buffered parallel calls are admitted before either result arrives");
    for ((_, _, call), expected) in calls.iter().zip([vec!["call-a"], vec!["call-a", "call-b"]]) {
        let state: CodexContinuityState = serde_json::from_value(call[CODEX_CONTINUITY_FIELD].clone()).unwrap();
        assert_eq!(calls_of(&state), expected);
    }
    let (_, _, answer) = emitted.iter()
        .find(|(index, label, value)| *index == 2 && label == INTERFACE_ARTIFACT_OUTPUT_LABEL && value["text"] == "both")
        .expect("the native turn answers once both results returned");
    let settled: CodexContinuityState = serde_json::from_value(answer[CODEX_CONTINUITY_FIELD].clone()).unwrap();
    assert!(calls_of(&settled).is_empty());
    driver.shutdown().await.unwrap();
    let answered = server.await.unwrap();
    assert_eq!(
        answered.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>(),
        [RequestId::String("request-b".into()), RequestId::String("request-a".into())]
    );
    for ((_, result), output) in answered.iter().zip(["b", "a"]) {
        let response: DynamicToolCallResponse = serde_json::from_value(result.clone()).unwrap();
        assert!(response.success);
        assert!(serde_json::to_string(&response.content_items).unwrap().contains(output));
    }
}
