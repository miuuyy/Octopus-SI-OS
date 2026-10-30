async fn retained_admission_native_turn(
    root: &Path,
    descriptor: &InterfaceDescriptor,
    input: &InterfaceInvocationEnvelope,
    record: &octopus_run_record::RunRecord,
    source_seq: u64,
    control: &Arc<CodexTurnControlBroker>,
    stream: Arc<ProviderOutputStream>,
    terminal: tokio::sync::oneshot::Sender<bool>,
) {
    use crate::handler::{
        NativeRunInvocation, TransportStateScope, TurnContext, TurnDrainOutcome, TurnSink,
    };
    use crate::live_stream::LiveStreamRules;
    use octopus_appserver_proto::JSONRPCMessage;
    let socket_path = root.join("pending-native.sock");
    let mut listener = crate::uds::UnixListener::bind(&socket_path).await.unwrap();
    let (native_completed, completed) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let mut socket = tokio_tungstenite::accept_async(listener.accept().await.unwrap())
            .await
            .unwrap();
        let JSONRPCMessage::Request(initialize) = next_test_rpc(&mut socket).await else {
            panic!("initialize")
        };
        assert_eq!(initialize.method, "initialize");
        send_test_result(
            &mut socket,
            initialize.id,
            json!({"userAgent":"codex/test","codexHome":"/tmp"}),
        )
        .await;
        let turn =
            json!({"id":"turn-a","items":[],"itemsView":"full","status":"inProgress","error":null});
        for message in [
            json!({"method":"turn/started","params":{"threadId":"thread-a","turn":turn}}),
            json!({"method":"thread/tokenUsage/updated","params":{"threadId":"thread-a","turnId":"turn-a","tokenUsage":{
                "total":{"totalTokens":100,"inputTokens":80,"cachedInputTokens":0,"outputTokens":20,"reasoningOutputTokens":0},
                "last":{"totalTokens":100,"inputTokens":80,"cachedInputTokens":0,"outputTokens":20,"reasoningOutputTokens":0},
                "modelContextWindow":32000
            }}}),
            json!({"id":"pending-native-request","method":"item/tool/call","params":{
                "threadId":"thread-a","turnId":"turn-a","callId":"pending-native-call",
                "namespace":"formed_interfaces","tool":"list_runs","arguments":{}
            }}),
        ] {
            socket
                .send(tokio_tungstenite::tungstenite::Message::Text(
                    message.to_string().into(),
                ))
                .await
                .unwrap();
        }
        let mut responses = 0;
        while let Some(Ok(frame)) = socket.next().await {
            let tokio_tungstenite::tungstenite::Message::Text(frame) = frame else {
                continue;
            };
            match serde_json::from_str::<JSONRPCMessage>(&frame).unwrap() {
                JSONRPCMessage::Notification(notification) => {
                    assert_eq!(notification.method, "initialized")
                }
                JSONRPCMessage::Response(response) => {
                    assert_eq!(
                        response.id,
                        octopus_appserver_proto::RequestId::String("pending-native-request".into())
                    );
                    responses += 1;
                    assert_eq!(responses, 1, "native call must have exactly one output");
                    assert_eq!(response.result["success"], true);
                    let answer = json!({"type":"agentMessage","id":"native-answer","text":"Completed after admission","phase":"final_answer"});
                    socket.send(tokio_tungstenite::tungstenite::Message::Text(json!({
                        "method":"item/completed","params":{"threadId":"thread-a","turnId":"turn-a","item":answer,"completedAtMs":20}
                    }).to_string().into())).await.unwrap();
                    let mut done = turn.clone();
                    done["status"] = json!("completed");
                    done["items"] = json!([answer]);
                    socket.send(tokio_tungstenite::tungstenite::Message::Text(json!({
                        "method":"turn/completed","params":{"threadId":"thread-a","turn":done}
                    }).to_string().into())).await.unwrap();
                }
                _ => panic!("pending admission must not interrupt or restart the native turn"),
            }
        }
        assert!(
            completed.await.is_ok(),
            "native transport shut down before the original turn completed"
        );
        assert_eq!(responses, 1, "native custom tool output is missing");
    });
    let client = crate::RemoteAppServerClient::connect(crate::RemoteAppServerConnectArgs {
        endpoint: crate::RemoteAppServerEndpoint::UnixSocket {
            socket_path: octopus_util_abspath::AbsolutePathBuf::from_absolute_path(&socket_path)
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
    let driver = CodexAppServerInterfaceDriver::new_with_config(
        crate::AppServerClient::Remote(client),
        descriptor.interface_driver_config.as_ref(),
    );
    let catalog_bytes = serde_json::to_vec(&json!({
        "id":"planning-catalog","label":"provider.run.affordances","operations":[{
            "name":"list_runs","description":"Read current run coordinates",
            "input_schema":{"type":"object","additionalProperties":false},"operation":"list_runs"
        }]
    }))
    .unwrap();
    let catalog = octopus_interfaces::ArtifactBindingPayload::new(
        ArtifactRef {
            id: "planning-catalog".into(),
            content: ContentHash::of_bytes(&catalog_bytes),
        },
        catalog_bytes,
    );
    let mut planning_input = input.clone();
    let metadata = planning_input.metadata.as_mut().unwrap();
    metadata.grant.artifact_access.insert(
        "planning-catalog".into(),
        octopus_runtime::ArtifactAccess::read_only(),
    );
    metadata
        .artifact_binding_refs
        .push(catalog.artifact.clone());
    planning_input
        .artifact_binding_payloads
        .push(catalog.clone());
    let affordances = crate::run_affordances::RunAffordances::from_invocation(
        &planning_input,
        &json!({
            "run_affordances":{"catalog_artifact":catalog.artifact}
        }),
    )
    .unwrap()
    .unwrap();
    let labels = crate::handler::codex_context_output_labels(descriptor).unwrap();
    let projections = crate::handler::codex_projection_output_labels(descriptor).unwrap();
    let rules = LiveStreamRules::from_private_config_or_disabled(&serde_json::Map::new()).unwrap();
    let output_root = root.join("var/provider/native-outputs");
    let streams = root.join("native-streams");
    let sink = TurnSink {
        emit: &stream,
        output_root: &output_root,
        live_stream_root: &streams,
        provider_interface: &descriptor.id,
        run: record.id.as_str(),
        source_movement_seq: source_seq,
        attempt: 1,
        context_output_labels: &labels,
        client_projection_output_labels: &projections,
        live_stream_rules: &rules,
        live_stream: std::sync::Mutex::new(None),
        continuity: stream.continuity.clone(),
    };
    let mut state = continuity(
        record.interface_state[&descriptor.id]["context_generation"]
            .as_u64()
            .unwrap(),
        None,
    );
    state.provider_interface = descriptor.id.clone();
    state.run = record.id.to_string();
    state.turn_input = Some(input.artifact.clone());
    sink.set_continuity(state);
    stream.continuity.lock().unwrap().pending_input_context = true;
    let scope = TransportStateScope::Run(record.id.to_string());
    let barrier = ProviderHandoffBarrier::new();
    let effect_gate = Arc::new(tokio::sync::Mutex::new(()));
    let interrupted = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let names = BTreeMap::from([("list_runs".into(), "list_runs".into())]);
    let operations = BTreeMap::new();
    let outcome = driver
        .drain_turn(
            TurnContext {
                run_invocation: Some(NativeRunInvocation {
                    affordances: &affordances,
                    metadata: planning_input.metadata.as_ref().unwrap(),
                    admitted_record: record,
                    current_record: record,
                }),
                scope: &scope,
                sink: &sink,
                control,
                interface_operations: &operations,
                tool_names: &names,
                source_artifact: &input.artifact,
                handoff_barrier: &barrier,
                effect_gate: &effect_gate,
                turn_interrupted: &interrupted,
                lifecycle: None,
                lifecycle_prompt: None,
            },
            "thread-a",
            "turn-a",
        )
        .await
        .unwrap();
    assert!(matches!(outcome, TurnDrainOutcome::Complete));
    assert!(!interrupted.load(std::sync::atomic::Ordering::Acquire));
    native_completed.send(()).unwrap();
    drop(sink);
    drop(stream);
    terminal.send(true).unwrap();
    driver.shutdown().await.unwrap();
    server.await.unwrap();
}
