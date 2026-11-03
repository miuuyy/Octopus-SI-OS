struct LateCallNativeTurn {
    driver: Arc<CodexAppServerInterfaceDriver>,
    late: tokio::sync::oneshot::Sender<()>,
    server: tokio::task::JoinHandle<()>,
}

async fn late_call_native_turn(root: &Path, descriptor: &InterfaceDescriptor) -> LateCallNativeTurn {
    use octopus_appserver_proto::JSONRPCMessage;
    let socket_path = root.join("late-call-native.sock");
    let mut listener = crate::uds::UnixListener::bind(&socket_path).await.unwrap();
    let (late, late_rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        let mut socket = tokio_tungstenite::accept_async(listener.accept().await.unwrap()).await.unwrap();
        let JSONRPCMessage::Request(initialize) = next_test_rpc(&mut socket).await else { panic!("initialize") };
        send_test_result(&mut socket, initialize.id, json!({"userAgent":"codex/test","codexHome":"/tmp"})).await;
        let call = |id: &str, call: &str| json!({"id":id,"method":"item/tool/call","params":{"threadId":"thread-a",
            "turnId":"turn-a","callId":call,"namespace":"formed_interfaces","tool":"coder-dexec","arguments":{"command":call}}});
        socket.send(tokio_tungstenite::tungstenite::Message::Text(call("request-a", "call-a").to_string().into())).await.unwrap();
        late_rx.await.unwrap();
        socket.send(tokio_tungstenite::tungstenite::Message::Text(call("request-b", "call-b").to_string().into())).await.unwrap();
        while let Some(Ok(frame)) = socket.next().await {
            let tokio_tungstenite::tungstenite::Message::Text(frame) = frame else { continue };
            match serde_json::from_str::<JSONRPCMessage>(&frame).unwrap() {
                JSONRPCMessage::Notification(notification) => assert_eq!(notification.method, "initialized"),
                other => panic!("a late parallel call must not answer or restart the native turn: {other:?}"),
            }
        }
    });
    let client = crate::RemoteAppServerClient::connect(crate::RemoteAppServerConnectArgs {
        endpoint: crate::RemoteAppServerEndpoint::UnixSocket {
            socket_path: octopus_util_abspath::AbsolutePathBuf::from_absolute_path(&socket_path).unwrap(),
        },
        client_name: "octopus-test".into(), client_version: "0".into(), experimental_api: false,
        mcp_server_openai_form_elicitation: false, opt_out_notification_methods: vec![], channel_capacity: 16,
    }).await.unwrap();
    let driver = Arc::new(CodexAppServerInterfaceDriver::new_with_config(
        crate::AppServerClient::Remote(client), descriptor.interface_driver_config.as_ref(),
    ));
    LateCallNativeTurn { driver, late, server }
}

#[allow(clippy::too_many_arguments)]
async fn late_call_first_drain(
    root: &Path,
    descriptor: &InterfaceDescriptor,
    record: &octopus_run_record::RunRecord,
    input: &InterfaceInvocationEnvelope,
    source_seq: u64,
    control: &Arc<CodexTurnControlBroker>,
    driver: &CodexAppServerInterfaceDriver,
    tool: &str,
    stream: Arc<ProviderOutputStream>,
    terminal: tokio::sync::oneshot::Sender<bool>,
) {
    use crate::handler::{TransportStateScope, TurnContext, TurnDrainOutcome, TurnSink};
    use crate::live_stream::LiveStreamRules;
    let labels = crate::handler::codex_context_output_labels(descriptor).unwrap();
    let projections = crate::handler::codex_projection_output_labels(descriptor).unwrap();
    let rules = LiveStreamRules::from_private_config_or_disabled(
        &descriptor.interface_driver_config.as_ref().unwrap().private_config,
    ).unwrap();
    let output_root = root.join("var/provider/late-call-outputs");
    let streams = root.join("late-call-streams");
    let sink = TurnSink {
        emit: &stream, output_root: &output_root, live_stream_root: &streams, provider_interface: &descriptor.id,
        run: record.id.as_str(), source_movement_seq: source_seq, attempt: 1, context_output_labels: &labels,
        client_projection_output_labels: &projections, live_stream_rules: &rules,
        live_stream: std::sync::Mutex::new(None), continuity: stream.continuity.clone(),
    };
    let mut state = continuity(record.interface_state[&descriptor.id]["context_generation"].as_u64().unwrap(), None);
    state.provider_interface = descriptor.id.clone();
    state.run = record.id.to_string();
    state.turn_input = Some(input.artifact.clone());
    sink.set_continuity(state);
    let scope = TransportStateScope::Run(record.id.to_string());
    let barrier = ProviderHandoffBarrier::new();
    let effect_gate = Arc::new(tokio::sync::Mutex::new(()));
    let interrupted = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let names = BTreeMap::from([("coder-dexec".to_string(), tool.to_string())]);
    let operations = BTreeMap::new();
    let outcome = driver.drain_turn(TurnContext {
        run_invocation: None, scope: &scope, sink: &sink, control, interface_operations: &operations,
        tool_names: &names, source_artifact: &input.artifact, handoff_barrier: &barrier,
        effect_gate: &effect_gate, turn_interrupted: &interrupted, lifecycle: None, lifecycle_prompt: None,
    }, "thread-a", "turn-a").await.unwrap();
    assert!(matches!(outcome, TurnDrainOutcome::AwaitingResults));
    drop(sink);
    drop(stream);
    terminal.send(true).unwrap();
}
