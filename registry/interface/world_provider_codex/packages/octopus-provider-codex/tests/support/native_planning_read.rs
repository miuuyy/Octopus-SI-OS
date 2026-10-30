#[tokio::test]
async fn native_planning_reads_the_acknowledged_tip_after_progress_without_replaying_effects() {
    use futures::{SinkExt, StreamExt};
    use octopus_appserver_proto::JSONRPCMessage;
    use octopus_run_record::RunRecord;
    use serde_json::json;
    use std::time::Duration;

    for mode in ["fresh", "missing", "malformed", "wrong_identity", "dropped"] {
        let root = tempfile::tempdir().unwrap();
        let socket_path = root.path().join("planning.sock");
        let mut listener = crate::uds::UnixListener::bind(&socket_path).await.unwrap();
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
            let turn = json!({"id":"turn-a","items":[],"itemsView":"full","status":"inProgress","error":null});
            for message in [
                json!({"method":"turn/started","params":{"threadId":"thread-a","turn":turn}}),
                json!({"method":"turn/moderationMetadata","params":{"threadId":"thread-a","turnId":"other-turn","metadata":{"ignored":true}}}),
                json!({"method":"turn/moderationMetadata","params":{"threadId":"other-thread","turnId":"turn-a","metadata":{"ignored":true}}}),
                json!({"method":"turn/moderationMetadata","params":{"threadId":"thread-a","turnId":"turn-a","metadata":{"review":{"state":"complete"},"scores":[0,0.5],"opaque":null}}}),
                json!({"id":"planning-request","method":"item/tool/call","params":{
                    "threadId":"thread-a","turnId":"turn-a","callId":"planning-call",
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
            let mut responses = Vec::new();
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
                            mode, "fresh",
                            "failed checked read must not answer or replay a native call"
                        );
                        assert_eq!(response.id, RequestId::String("planning-request".into()));
                        assert!(responses.is_empty(), "native call answered more than once");
                        responses.push(response.result);
                        let mut terminal = turn.clone();
                        terminal["status"] = json!("interrupted");
                        socket.send(tokio_tungstenite::tungstenite::Message::Text(json!({
                            "method":"turn/completed","params":{"threadId":"thread-a","turn":terminal}
                        }).to_string().into())).await.unwrap();
                    }
                    _ => panic!("planning read issued another provider request or effect"),
                }
            }
            responses
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
        let config = InterfaceDriverConfig {
            private_config:
                json!({"native_tool_namespace":{"name":"formed_interfaces","description":"tools"}})
                    .as_object()
                    .unwrap()
                    .clone(),
            ..InterfaceDriverConfig::default()
        };
        let driver = CodexAppServerInterfaceDriver::new_with_config(
            AppServerClient::Remote(client),
            Some(&config),
        );
        let admitted_bytes = serde_json::to_vec(&json!({
            "id":"run-a","ee":"ee-a","worker":"system_worker_test","trace":"trace-a","status":"idle",
            "checkpoints":[{"id":"source","run":"run-a","trace":"trace-a","epoch":4,
                "interface_binding":octopus_runtime::AuthoredInterfaceBinding::all("world_provider_codex".into()),
                "artifact_bindings":[]}],"active_checkpoints":["source"]
        })).unwrap();
        let admitted = RunRecord::from_bytes(&admitted_bytes).unwrap();
        let mut tip = admitted.clone();
        tip.status = json!("running");
        tip.checkpoints[0].id = "after-progress".into();
        tip.checkpoints[0].epoch.0 += 1;
        tip.active_checkpoints = vec!["after-progress".into()];
        let catalog = bound(
            "planning-catalog",
            json!({
                "id":"planning-catalog","label":"provider.run.affordances","operations":[{
                    "name":"list_runs","description":"Read current run coordinates",
                    "input_schema":{"type":"object","additionalProperties":false},"operation":"list_runs"
                }]
            }),
        );
        let mut input = metadata_input(json!([]));
        input.metadata = Some(serde_json::from_value(json!({
            "source_movement_seq":7,
            "runtime":{"run":"run-a","ee":"ee-a","worker":"system_worker_test","checkpoint":"source",
                "interface":"world_provider_codex","epoch":4,
                "run_record_hash":octopus_ledger::ContentHash::of_bytes(&admitted_bytes).to_hex()},
            "grant":{"artifact_access":{"planning-catalog":{"read":true}}},
            "artifact_binding_refs":[catalog.artifact]
        })).unwrap());
        input.artifact_binding_payloads = vec![catalog.clone()];
        let affordances = RunAffordances::from_invocation(
            &input,
            &json!({
                "run_affordances":{"catalog_artifact":catalog.artifact}
            }),
        )
        .unwrap()
        .unwrap();
        let metadata = input.metadata.as_ref().unwrap();
        let source_metadata = serde_json::to_value(metadata).unwrap();
        let source = bound(
            "completed-effect",
            json!({"label":"file.write.result","tool":"file.write",
            "correlation_id":"previous-call","ok":true,"body":{"receipt":"effect-performed-once"}}),
        );
        let (emit, mut outputs) = ProviderOutputStream::channel();
        let labels = codex_context_labels();
        let projected = codex_client_projection_labels();
        let rules =
            LiveStreamRules::from_private_config_or_disabled(&serde_json::Map::new()).unwrap();
        let streams = root.path().join("streams");
        let provider = InterfaceId::new("world_provider_codex");
        let sink = TurnSink {
            emit: &emit,
            output_root: root.path(),
            live_stream_root: &streams,
            provider_interface: &provider,
            run: "run-a",
            source_movement_seq: 7,
            attempt: 1,
            context_output_labels: &labels,
            client_projection_output_labels: &projected,
            live_stream_rules: &rules,
            live_stream: std::sync::Mutex::new(None),
            continuity: emit.continuity.clone(),
        };
        let scope = TransportStateScope::Run("run-a".into());
        let control = CodexTurnControlBroker::new(Some(7));
        let barrier = ProviderHandoffBarrier::new();
        let gate = Arc::new(tokio::sync::Mutex::new(()));
        let interrupted = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let names = BTreeMap::from([("list_runs".into(), "list_runs".into())]);
        let operations = BTreeMap::new();
        let context = TurnContext {
            run_invocation: Some(NativeRunInvocation {
                affordances: &affordances,
                metadata,
                admitted_record: &admitted,
                current_record: &admitted,
            }),
            scope: &scope,
            sink: &sink,
            control: &control,
            interface_operations: &operations,
            tool_names: &names,
            source_artifact: &source.artifact,
            handoff_barrier: &barrier,
            effect_gate: &gate,
            turn_interrupted: &interrupted,
            lifecycle: None,
            lifecycle_prompt: None,
        };
        let mut observed = Vec::new();
        let mut reads = 0;
        let outcome = tokio::time::timeout(Duration::from_secs(5), async {
            let drain = driver.drain_turn(context, "thread-a", "turn-a");
            tokio::pin!(drain);
            loop {
                tokio::select! {
                    result = &mut drain => break result,
                    event = outputs.recv() => match event.unwrap() {
                        ProviderStreamEmission::Turn(emission) | ProviderStreamEmission::Progress(emission) => {
                            assert!(matches!(emission.route, ProviderEmissionRoute::ClientProjection), "planning replayed a tool effect");
                            observed.push(emission.artifact);
                        }
                        ProviderStreamEmission::CurrentRunRecord { reply, .. } => {
                            reads += 1;
                            assert_eq!(reads, 1, "the first native call requests one checked snapshot");
                            assert!(observed.iter().any(|artifact| artifact.read_manifest().unwrap().label == TURN_STARTED_LABEL));
                            assert!(observed.iter().any(|artifact| artifact.read_manifest().unwrap().label == TURN_DYNAMIC_TOOL_CALL_LABEL));
                            assert!(tokio::time::timeout(Duration::from_millis(10), &mut drain).await.is_err(), "planner ran before its acknowledged read");
                            assert!(outputs.try_recv().is_err());
                            match mode {
                                "missing" => reply.send(Err("checked run snapshot is missing".into())).unwrap(),
                                "malformed" => reply.send(Ok(b"{".to_vec())).unwrap(),
                                "wrong_identity" => {
                                    let mut other = tip.clone(); other.worker = "system_worker_other".into();
                                    reply.send(Ok(serde_json::to_vec(&other).unwrap())).unwrap();
                                }
                                "dropped" => drop(reply),
                                _ => reply.send(Ok(serde_json::to_vec(&tip).unwrap())).unwrap(),
                            }
                        }
                        _ => panic!("checked read emitted a write, transport dispatch or binding mutation"),
                    }
                }
            }
        }).await.expect("checked planning boundary stalled");
        while let Ok(event) = outputs.try_recv() {
            let (ProviderStreamEmission::Turn(emission) | ProviderStreamEmission::Progress(emission)) = event else {
                panic!("late side effect")
            };
            assert!(matches!(
                emission.route,
                ProviderEmissionRoute::ClientProjection
            ));
            observed.push(emission.artifact);
        }
        assert_eq!(
            reads, 1,
            "native planning bypassed the checked snapshot boundary"
        );
        if mode == "fresh" {
            assert!(matches!(outcome, Ok(TurnDrainOutcome::Complete)));
        } else {
            let error = outcome
                .err()
                .expect("invalid checked read must fail closed")
                .to_string();
            if mode == "missing" {
                assert!(error.contains("checked run snapshot is missing"));
            }
            if mode == "wrong_identity" {
                assert!(error.contains("changed run identity"));
            }
            if mode == "dropped" {
                assert!(error.contains("dropped the checked run read"));
            }
        }
        assert_eq!(serde_json::to_value(metadata).unwrap(), source_metadata);
        assert_eq!(admitted, RunRecord::from_bytes(&admitted_bytes).unwrap());
        assert!(!driver.has_pending_request(&scope).await);
        driver.shutdown().await.unwrap();
        let responses = server.await.unwrap();
        assert_eq!(responses.len(), usize::from(mode == "fresh"));
        if mode == "fresh" {
            let response: DynamicToolCallResponse =
                serde_json::from_value(responses[0].clone()).unwrap();
            assert!(response.success);
            let encoded = serde_json::to_value(response.content_items).unwrap();
            let result: serde_json::Value =
                serde_json::from_str(encoded[0]["text"].as_str().unwrap()).unwrap();
            assert_eq!(result["source_run"]["recorded_status"], "running");
            assert_eq!(
                result["source_run"]["run_record_hash"],
                source_metadata["runtime"]["run_record_hash"]
            );
            assert_eq!(result["source_movement_seq"], 7);
            assert_eq!(result["receive_interface_id"], "system_worker_test");
        }
    }
}
