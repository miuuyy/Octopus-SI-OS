    use super::*;
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/native_terminal_handoff.rs"));
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/turn_deferral.rs"));
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/declared_input.rs"));

    use futures::{SinkExt, StreamExt};
    use octopus_interface_transport::InterfaceHostIoLimits;
    use octopus_interfaces::{
        GrantProjection, InterfaceArtifactFile, InvocationMetadata, RuntimeInvocationMetadata,
    };
    use octopus_ledger::{Lamport, Movement, Seq, TraceId};
    use serde_json::json;

    fn continuity(
        context_generation: u64,
        pending_field: Option<&str>,
    ) -> crate::handler::CodexContinuityState {
        let mut value = json!({
            "schema_version": 1,
            "provider_interface": "world_provider_codex",
            "run": "run-a",
            "thread_id": "thread-a",
            "thread_shape_fingerprint": "a".repeat(64),
            "context_generation": context_generation,
            "static_context_fingerprint": "b".repeat(64)
        });
        if let Some(pending_field) = pending_field {
            value[pending_field] = json!({
                "kind": "dynamic_tool",
                "turn_id": "turn-a",
                "call_id": "call-a",
                "tool": "coder.exec"
            });
        }
        serde_json::from_value(value).unwrap()
    }

    fn descriptor() -> InterfaceDescriptor {
        serde_json::from_str(include_str!("../../../../interface.json")).unwrap()
    }

    #[test]
    fn current_descriptor_read_detects_selection_drift_after_lift() {
        let temp = tempfile::tempdir().unwrap();
        let lifted = descriptor();
        let ctx = context(temp.path(), &lifted);
        let mut current = lifted.clone();
        current
            .interface_driver_config
            .as_mut()
            .unwrap()
            .private_config
            .insert("model".to_string(), json!("gpt-5.6-terra"));
        let interface_dir = temp.path().join("registry/interface/world_provider_codex");
        std::fs::create_dir_all(&interface_dir).unwrap();
        std::fs::write(
            interface_dir.join("interface.json"),
            serde_json::to_vec_pretty(&current).unwrap(),
        )
        .unwrap();

        assert_eq!(load_own_descriptor(&ctx).unwrap(), lifted);
        assert_eq!(load_current_own_descriptor(&ctx).unwrap(), current);
    }

    fn bound(id: &str, payload: serde_json::Value) -> octopus_interfaces::ArtifactBindingPayload {
        let bytes = serde_json::to_vec(&payload).unwrap();
        octopus_interfaces::ArtifactBindingPayload::new(
            ArtifactRef {
                id: ArtifactId::new(id),
                content: ContentHash::of_bytes(&bytes),
            },
            bytes,
        )
    }

    fn pinned_test_run_record() -> Vec<u8> {
        let inbound = octopus_runtime::AuthoredInterfaceBinding::all(InterfaceId::new("world_operator"));
        let outbound = octopus_runtime::AuthoredInterfaceBinding::all(InterfaceId::new("world_provider_codex"));
        let bytes = serde_json::to_vec(&json!({"id":"run-a","ee":"ee-a","worker":"system_worker_test",
            "trace":"trace-a","status":"running","checkpoints":[{"id":"checkpoint-a","run":"run-a",
                "trace":"trace-a","epoch":1,"interface_binding":inbound,"outbound_interface_bindings":[outbound]}],
            "active_checkpoints":["checkpoint-a"]})).unwrap();
        octopus_run_record::RunRecord::from_bytes(&bytes).unwrap();
        bytes
    }

    fn runtime_metadata() -> InvocationMetadata {
        let mut grant = GrantProjection::default();
        grant.interface_policy.insert("world_provider_codex".into(), BTreeMap::from([
            ("web_search_modes".into(), json!(["live"])),
        ]));
        InvocationMetadata::new(grant).with_runtime(
            RuntimeInvocationMetadata {
                run: RunId::new("run-a"),
                ee: octopus_descriptors::EeId::new("ee-a"),
                worker: octopus_descriptors::InterfaceId::new("system_worker_test"),
                checkpoint: octopus_descriptors::CheckpointId::new("checkpoint-a"),
                interface: octopus_descriptors::InterfaceId::new("world_provider_codex"),
                epoch: 1,
                run_record_hash: Some(ContentHash::of_bytes(&pinned_test_run_record()).to_hex()),
            },
        )
    }

    async fn next_test_rpc(
        socket: &mut tokio_tungstenite::WebSocketStream<crate::uds::UnixStream>,
    ) -> octopus_appserver_proto::JSONRPCMessage {
        let message = socket.next().await.unwrap().unwrap();
        serde_json::from_str(&message.into_text().unwrap()).unwrap()
    }

    async fn send_test_result(
        socket: &mut tokio_tungstenite::WebSocketStream<crate::uds::UnixStream>,
        id: octopus_appserver_proto::RequestId,
        result: serde_json::Value,
    ) {
        let response = octopus_appserver_proto::JSONRPCMessage::Response(
            octopus_appserver_proto::JSONRPCResponse { id, result },
        );
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                serde_json::to_string(&response).unwrap().into(),
            ))
            .await
            .unwrap();
    }

    async fn send_test_notification(
        socket: &mut tokio_tungstenite::WebSocketStream<crate::uds::UnixStream>,
        notification: octopus_appserver_proto::ServerNotification,
    ) {
        let message: octopus_appserver_proto::JSONRPCMessage =
            serde_json::from_value(serde_json::to_value(notification).unwrap()).unwrap();
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                serde_json::to_string(&message).unwrap().into(),
            ))
            .await
            .unwrap();
    }

    async fn interrupted_drain_driver(
        root: &Path,
        descriptor: &InterfaceDescriptor,
    ) -> (
        Arc<CodexAppServerInterfaceDriver>,
        tokio::sync::oneshot::Sender<()>,
        tokio::sync::mpsc::UnboundedReceiver<String>,
        tokio::task::JoinHandle<()>,
    ) {
        let socket_path = root.join("interrupted-app-server.sock");
        let mut listener = crate::uds::UnixListener::bind(&socket_path).await.unwrap();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let (method_tx, method_rx) = tokio::sync::mpsc::unbounded_channel();
        let server = tokio::spawn(async move {
            let stream = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let initialize = next_test_rpc(&mut socket).await;
            let octopus_appserver_proto::JSONRPCMessage::Request(initialize) = initialize else {
                panic!("expected initialize request")
            };
            assert_eq!(initialize.method, "initialize");
            send_test_result(
                &mut socket,
                initialize.id,
                json!({"userAgent": "codex/test", "codexHome": "/tmp"}),
            )
            .await;
            assert!(matches!(
                next_test_rpc(&mut socket).await,
                octopus_appserver_proto::JSONRPCMessage::Notification(_)
            ));
            let mut release_rx = release_rx;
            loop {
                tokio::select! {
                    release = &mut release_rx => {
                        release.unwrap();
                        send_test_notification(
                            &mut socket,
                            octopus_appserver_proto::ServerNotification::Warning(
                                octopus_appserver_proto::WarningNotification {
                                    thread_id: Some("thread-a".to_string()),
                                    message: "partial cancelled-turn activity".to_string(),
                                },
                            ),
                        )
                        .await;
                        send_test_notification(
                            &mut socket,
                            octopus_appserver_proto::ServerNotification::Error(
                                octopus_appserver_proto::ErrorNotification {
                                    error: octopus_appserver_proto::TurnError {
                                        message: "cancelled turn transport failed".to_string(),
                                        codex_error_info: None,
                                        additional_details: None,
                                    },
                                    will_retry: false,
                                    thread_id: "thread-a".to_string(),
                                    turn_id: "turn-a".to_string(),
                                },
                            ),
                        )
                        .await;
                        socket.close(None).await.unwrap();
                        break;
                    }
                    message = socket.next() => {
                        let Some(message) = message else {
                            break;
                        };
                        let message = message.unwrap();
                        let message: octopus_appserver_proto::JSONRPCMessage =
                            serde_json::from_str(&message.into_text().unwrap()).unwrap();
                        if let octopus_appserver_proto::JSONRPCMessage::Request(request) = message {
                            method_tx.send(request.method.clone()).unwrap();
                            let result = match request.method.as_str() {
                                "thread/read" => {
                                    assert_eq!(serde_json::from_value::<octopus_appserver_proto::ThreadReadParams>(request.params.clone().unwrap()).unwrap().include_turns,false);
                                    json!({"thread":octopus_appserver_proto::Thread {
                                        id:"thread-a".into(),session_id:"thread-a".into(),forked_from_id:None,parent_thread_id:None,
                                        preview:String::new(),ephemeral:false,model_provider:"fixture".into(),created_at:1,updated_at:1,recency_at:Some(1),
                                        status:octopus_appserver_proto::ThreadStatus::Idle,path:None,cwd:octopus_util_abspath::AbsolutePathBuf::from_absolute_path("/tmp").unwrap(),
                                        cli_version:"test".into(),source:octopus_appserver_proto::SessionSource::Exec,thread_source:None,
                                        agent_nickname:None,agent_role:None,git_info:None,name:None,turns:vec![],
                                    }})
                                }
                                "thread/turns/list" => json!({"data":[{"id":"turn-a","items":[],"itemsView":"notLoaded","status":"interrupted","error":null}],"nextCursor":null}),
                                "thread/items/list" => json!({"data":[],"nextCursor":null}),
                                _ => json!({"turnId":"turn-a"}),
                            };
                            send_test_result(&mut socket,request.id,result).await;
                        }
                    }
                }
            }
        });
        let client = crate::RemoteAppServerClient::connect(crate::RemoteAppServerConnectArgs {
            endpoint: crate::RemoteAppServerEndpoint::UnixSocket {
                socket_path: octopus_util_abspath::AbsolutePathBuf::from_absolute_path(
                    &socket_path,
                )
                .unwrap(),
            },
            client_name: "octopus-test".to_string(),
            client_version: "0".to_string(),
            experimental_api: false,
            mcp_server_openai_form_elicitation: false,
            opt_out_notification_methods: Vec::new(),
            channel_capacity: 8,
        })
        .await
        .unwrap();
        let driver = Arc::new(CodexAppServerInterfaceDriver::new_with_config(
            crate::AppServerClient::Remote(client),
            descriptor.interface_driver_config.as_ref(),
        ));
        (driver, release_tx, method_rx, server)
    }

    fn context(root: &Path, descriptor: &InterfaceDescriptor) -> InterfaceLiftContext {
        let (arrivals, _) = tokio::sync::mpsc::channel(1);
        let record = pinned_test_run_record();
        let blobs = root.join("ledger/blobs");
        std::fs::create_dir_all(&blobs).unwrap();
        std::fs::write(blobs.join(ContentHash::of_bytes(&record).to_hex()), record).unwrap();
        InterfaceLiftContext::unprivileged(
            descriptor.id.clone(),
            root.join("registry/interface/world_provider_codex"),
            Some(descriptor.clone()),
            root.join("var/provider"),
            root.join("ledger/movements.jsonl"),
            root.join("ledger/blobs"),
            root.join("ledger/ledger-index.sqlite"),
            BTreeMap::new(),
            arrivals,
            Default::default(),
        )
        .with_host_io_limits(InterfaceHostIoLimits {
            max_control_frame_bytes: 1_048_576,
            max_stderr_bytes: 262_144,
            max_manifest_bytes: 1_048_576,
            max_payload_bytes: 1_048_576,
        })
        .unwrap()
    }

    #[test]
    fn resume_requires_the_current_admitted_stopping_receipt() {
        let root = tempfile::tempdir().unwrap();
        let descriptor = descriptor();
        let ctx = context(root.path(), &descriptor);
        let run = RunId::new("run-a");
        let result = CodexTurnControlResult { control_id: "stop-a".into(), action: "stop".into(), acknowledged: true,
            thread_id: Some("thread-a".into()), turn_id: Some("turn-a".into()), detail: json!({"interrupted":true}), continuity: Some(continuity(5, None)) };
        persist_control_journal(&ctx, 0, &run, "stop-a", "stop", TurnControlJournalPhase::Delivered { result: result.clone() }).unwrap();
        let receipt = materialize_turn_control_result(&ctx.interface_var_dir().join("emitted"), 0, result.clone()).unwrap();
        let source = movement(0, MovementKind::InterfaceOutbound, Endpoint::Interface(InterfaceId::new("world_operator")),
            Endpoint::Interface(descriptor.id.clone()), ArtifactRef { id: ArtifactId::new("stop"), content: ContentHash::of_bytes(b"stop") });
        let admitted = movement(1, MovementKind::InterfaceArrival, Endpoint::Interface(descriptor.id.clone()),
            Endpoint::Interface(InterfaceId::new("system_worker_test")), cache_artifact(&ctx, &receipt));
        write_movements(&ctx.movements_path, &[source.clone(), admitted]);
        assert_eq!(resume_control_continuity(&ctx, &run, 2, 5).unwrap(), continuity(5, None));
        assert!(resume_control_continuity(&ctx, &run, 2, 6).is_err());
        assert!(resume_control_continuity(&ctx, &run, 1, 5).is_err());
        assert!(resume_control_continuity(&ctx, &RunId::new("another-run"), 2, 5).is_err());
        write_movements(&ctx.movements_path, &[source]);
        assert!(resume_control_continuity(&ctx, &run, 2, 5).is_err());
    }

    fn movement(
        seq: u64,
        kind: MovementKind,
        from: Endpoint,
        to: Endpoint,
        artifact: ArtifactRef,
    ) -> Movement {
        let invocation_metadata = matches!(
            &kind,
            MovementKind::InterfaceArrival | MovementKind::InterfaceOutbound
        )
        .then(|| runtime_metadata().into());
        Movement {
            seq: Seq(seq),
            lamport: Lamport(seq + 1),
            recorded_at_unix_ms: seq + 1,
            trace: TraceId(format!("trace-{seq}")),
            kind,
            from,
            to,
            artifact: Some(artifact),
            invocation_metadata,
        }
    }

    fn write_movements(path: &Path, movements: &[Movement]) {
        let mut bytes = Vec::new();
        for movement in movements {
            serde_json::to_writer(&mut bytes, movement).unwrap();
            bytes.push(b'\n');
        }
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
        sync_ledger_index(path);
    }

    fn append_movement(path: &Path, movement: &Movement) {
        let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        serde_json::to_writer(&mut file, movement).unwrap();
        std::io::Write::write_all(&mut file, b"\n").unwrap();
        drop(file);
        sync_ledger_index(path);
    }

    fn sync_ledger_index(path: &Path) {
        let log = octopus_ledger_store::DurableLog::open(path).unwrap();
        octopus_ledger_index::LedgerSqliteIndex::open_healthy(
            &path.parent().unwrap().join("ledger-index.sqlite"),
            &log,
        )
        .unwrap();
    }

    fn artifact(root: &Path, label: &str, value: serde_json::Value) -> InterfaceArtifactFile {
        InterfaceArtifactFile::write(root, label, &serde_json::to_vec(&value).unwrap()).unwrap()
    }

    fn cache_artifact(ctx: &InterfaceLiftContext, artifact: &InterfaceArtifactFile) -> ArtifactRef {
        let limits = ctx.required_host_io_limits().unwrap();
        let captured = artifact
            .read_bounded(limits.max_manifest_bytes, limits.max_payload_bytes)
            .unwrap();
        let artifact_ref = materialized_artifact_ref(artifact).unwrap();
        let manifest: octopus_interfaces::InterfaceArtifactManifest =
            serde_json::from_slice(&captured.manifest_bytes).unwrap();
        std::fs::create_dir_all(&ctx.blobs_dir).unwrap();
        std::fs::write(
            ctx.blobs_dir.join(artifact_ref.content.to_hex()),
            captured.manifest_bytes,
        )
        .unwrap();
        std::fs::write(
            ctx.blobs_dir
                .join(manifest.payload_content_hash.as_deref().unwrap()),
            captured.payload_bytes,
        )
        .unwrap();
        artifact_ref
    }

    fn assert_deferred_receipt_round_trip(
        ctx: &InterfaceLiftContext,
        descriptor: &InterfaceDescriptor,
        turn: &QueuedTurn,
    ) {
        let path = deferred_provider_turn_path(ctx, turn.seq);
        let bytes = std::fs::read(&path).unwrap();
        let expected: DeferredProviderTurnJournal = serde_json::from_slice(&bytes).unwrap();
        let mut old: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        old["schema_version"] = json!(2);
        let old = serde_json::to_vec(&old).unwrap();
        std::fs::write(&path, &old).unwrap();
        assert!(load_deferred_provider_turns(ctx, descriptor).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), old);
        std::fs::write(&path, &bytes).unwrap();
        let captured = turn.envelope.artifact_file.read_bounded(1_048_576, 1_048_576).unwrap();
        let payload = ctx.blobs_dir.join(&expected.payload_content_hash);
        let original = std::fs::read(&payload).unwrap();
        std::fs::write(&payload, b"corrupt canonical input").unwrap();
        assert!(load_deferred_provider_turns(ctx, descriptor).is_err());
        std::fs::write(&payload, &original).unwrap();
        std::fs::remove_file(turn.envelope.artifact_file.manifest_path()).unwrap();
        std::fs::remove_file(&captured.manifest.payload_path).unwrap();
        let loaded = load_deferred_provider_turns(ctx, descriptor).unwrap();
        assert_eq!(loaded[&turn.seq].turn.envelope.metadata, turn.envelope.metadata);
        assert_eq!(loaded[&turn.seq].turn.envelope.read_artifact_payload_bounded(1_048_576).unwrap(), original);
        for (actual, original) in loaded[&turn.seq].turn.envelope.artifact_binding_payloads.iter()
            .zip(&turn.envelope.artifact_binding_payloads)
        { assert_eq!(actual.artifact, original.artifact); assert_eq!(actual.read_bytes().unwrap().as_ref(), original.read_bytes().unwrap().as_ref()); }
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        std::fs::write(turn.envelope.artifact_file.manifest_path(), &captured.manifest_bytes).unwrap();
        std::fs::write(&captured.manifest.payload_path, &captured.payload_bytes).unwrap();
    }

    #[tokio::test]
    async fn control_reader_progresses_other_runs_past_unready_targets() {
        for (ready_count, with_starting) in [(1_u64, false), (10, true), (12, true)] {
            let temp = tempfile::tempdir().unwrap();
            let descriptor = descriptor();
            let ctx = context(temp.path(), &descriptor);
            let controls = Arc::new(tokio::sync::Mutex::new(BTreeMap::new()));
            let starting = CodexTurnControlBroker::new(None);
            if with_starting {
                starting.publish_recovered_interrupted_pending(&continuity(8, Some("interrupted_pending"))).unwrap();
                controls
                    .lock()
                    .await
                    .insert("starting".into(), starting.clone());
            }
            let source_seq = |index: u64| index + u64::from(with_starting);
            let source_count = ready_count + u64::from(with_starting) + 1;
            let mut movements = Vec::new();
            let waiting = artifact(temp.path(), "operator.message", json!({"prompt": "queued"}));
            let mut waiting = movement(
                0,
                MovementKind::InterfaceOutbound,
                Endpoint::Interface(InterfaceId::new("operator")),
                Endpoint::Interface(ctx.interface.clone()),
                cache_artifact(&ctx, &waiting),
            );
            let mut metadata = runtime_metadata();
            metadata.runtime.as_mut().unwrap().run = RunId::new(if with_starting { "starting" } else { "queued" });
            waiting.invocation_metadata = Some(metadata.into());
            movements.push(waiting);
            let mut admissions = Vec::new();
            for index in u64::from(!with_starting)..=ready_count {
                let run = if index == 0 {
                    "starting".to_string()
                } else {
                    format!("ready-{index}")
                };
                if index != 0 {
                    let broker = CodexTurnControlBroker::new(None);
                    broker.finish();
                    controls.lock().await.insert(run.clone(), broker);
                }
                let seq = source_seq(index);
                let id = format!("stop-{index}");
                let input = artifact(
                    &temp.path().join(&id),
                    "operator.turn.control",
                    json!({"action": "stop", "control_id": id}),
                );
                let mut source = movement(
                    seq,
                    MovementKind::InterfaceOutbound,
                    Endpoint::Interface(InterfaceId::new("operator")),
                    Endpoint::Interface(ctx.interface.clone()),
                    cache_artifact(&ctx, &input),
                );
                let mut metadata = runtime_metadata();
                metadata.runtime.as_mut().unwrap().run = RunId::new(&run);
                source.invocation_metadata = Some(metadata.clone().into());
                movements.push(source);
                let result = terminally_satisfied_control(&id, "stop").result;
                persist_control_journal(
                    &ctx,
                    seq,
                    &RunId::new(&run),
                    &id,
                    "stop",
                    TurnControlJournalPhase::Result {
                        result: result.clone(),
                    },
                )
                .unwrap();
                let output = materialize_turn_control_result(
                    &ctx.interface_var_dir().join("emitted"),
                    seq,
                    result,
                )
                .unwrap();
                let mut admission = movement(
                    source_count + index - u64::from(!with_starting),
                    MovementKind::InterfaceArrival,
                    Endpoint::Interface(ctx.interface.clone()),
                    Endpoint::Interface(InterfaceId::new("system_worker_test")),
                    cache_artifact(&ctx, &output),
                );
                admission.invocation_metadata = Some(metadata.into());
                admissions.push(admission);
            }
            movements.extend(admissions);
            write_movements(&ctx.movements_path, &movements);
            let (progress, progress_rx) = tokio::sync::watch::channel(MainReaderProgress {
                next_seq: 0,
                revision: 0,
            });
            let (fences, fence_rx) = tokio::sync::mpsc::unbounded_channel();
            let gates = Arc::new(tokio::sync::Mutex::new(BTreeMap::new()));
            let task = tokio::spawn(control_reader_life(
                ctx.clone(),
                descriptor.clone(),
                ProviderHandoffSession::new(&ctx).unwrap(),
                controls.clone(),
                gates.clone(),
                InterruptedEffectArbiter::new(ctx.clone()),
                progress_rx,
                fence_rx,
            ));
            let began = std::time::Instant::now();
            let delivered = tokio::time::timeout(std::time::Duration::from_millis(500), async {
                loop {
                    let count = (1..=ready_count)
                        .filter(|index| {
                            matches!(
                                load_control_journal(
                                    &ctx,
                                    source_seq(*index),
                                    &RunId::new(format!("ready-{index}")),
                                    &format!("stop-{index}"),
                                    "stop"
                                )
                                .unwrap()
                                .unwrap()
                                .phase,
                                TurnControlJournalPhase::Delivered { .. }
                            )
                        })
                        .count();
                    if count == ready_count as usize {
                        break count;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await;
            eprintln!(
                "control reader: ready={ready_count} elapsed_us={} completed={delivered:?}",
                began.elapsed().as_micros()
            );
            let committed = LedgerTailReader::open_named(&ctx, "turn-control")
                .unwrap()
                .next_seq();
            assert_eq!(
                committed, 0,
                "unsettled first movement must remain replayable"
            );
            let watermark = tokio::time::timeout(
                std::time::Duration::from_millis(500),
                fence_control_reader(&fences, &RunId::new("ready-1"), Some(0)),
            )
            .await
            .unwrap()
            .unwrap();
            assert!(watermark >= source_count);
            ctx.cancel.cancel();
            task.await.unwrap().unwrap();
            assert!(
                gates
                    .lock()
                    .await
                    .values()
                    .all(|gate| gate.strong_count() == 0)
            );
            assert_eq!(delivered.unwrap(), ready_count as usize);
            assert_eq!(
                LedgerTailReader::open_named(&ctx, "turn-control")
                    .unwrap()
                    .next_seq(),
                0
            );
            if with_starting {
                assert!(matches!(
                    load_control_journal(&ctx, 1, &RunId::new("starting"), "stop-0", "stop")
                        .unwrap()
                        .unwrap()
                        .phase,
                    TurnControlJournalPhase::Result { .. }
                ));
            }
            publish_main_reader_progress(&progress, 1);
            if with_starting { controls.lock().await.remove("starting"); }
            let mut restarted = ctx.clone();
            restarted.cancel = Default::default();
            let (fences, fence_rx) = tokio::sync::mpsc::unbounded_channel();
            let task = tokio::spawn(control_reader_life(
                restarted.clone(),
                descriptor,
                ProviderHandoffSession::new(&restarted).unwrap(),
                controls.clone(),
                gates,
                InterruptedEffectArbiter::new(restarted.clone()),
                progress.subscribe(),
                fence_rx,
            ));
            if with_starting {
                retire_controls_before_deferred_turn(&controls, &progress, &fences, &RunId::new("starting")).await.unwrap();
                assert!(matches!(load_control_journal(&ctx, 1, &RunId::new("starting"), "stop-0", "stop")
                    .unwrap().unwrap().phase, TurnControlJournalPhase::Delivered { .. }));
            }
            let runs = (1..=ready_count)
                .map(|index| RunId::new(format!("ready-{index}")))
                .chain(with_starting.then(|| RunId::new("starting")))
                .collect::<Vec<_>>();
            tokio::time::timeout(
                std::time::Duration::from_secs(2),
                futures::future::try_join_all(
                    runs.iter()
                        .map(|run| fence_control_reader(&fences, run, None)),
                ),
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(
                LedgerTailReader::open_named(&restarted, "turn-control")
                    .unwrap()
                    .next_seq(),
                source_count
            );
            assert_eq!(
                std::fs::read(&ctx.movements_path)
                    .unwrap()
                    .iter()
                    .filter(|byte| **byte == b'\n')
                    .count(),
                movements.len()
            );
            let cursor = restarted.interface_var_dir().join("ledger-cursor.turn-control.json");
            let durable = (std::fs::read(&cursor).unwrap(), std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(&cursor).unwrap()));
            let mut foreign = movements.last().unwrap().clone();
            foreign.seq = Seq(movements.len() as u64);
            foreign.lamport = Lamport(movements.len() as u64 + 1);
            append_movement(&ctx.movements_path, &foreign);
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                while fence_control_reader(&fences, &runs[0], None).await.unwrap() <= movements.len() as u64 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert_eq!(
                (std::fs::read(&cursor).unwrap(), std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(&cursor).unwrap())),
                durable,
                "settled foreign ledger progress stays volatile"
            );
            restarted.cancel.cancel();
            task.await.unwrap().unwrap();
        }
    }

    #[tokio::test]
    async fn recovered_dispatch_scan_uses_the_full_authoritative_source_suffix() {
        let temp = tempfile::tempdir().unwrap();
        let descriptor = descriptor();
        let ctx = context(temp.path(), &descriptor);
        let run = RunId::new("run-a");
        let source = artifact(temp.path(), "coder.exec.result", json!({"ok": true}));
        let source_ref = cache_artifact(&ctx, &source);
        let dispatch = artifact(
            temp.path(),
            PROVIDER_TRANSPORT_DISPATCH_LABEL,
            json!({
                "label": PROVIDER_TRANSPORT_DISPATCH_LABEL,
                "provider_interface": "world_provider_codex",
                "source_movement_seq": 0,
                "attempt": 1,
                "method": "turn/start",
                "transport": "process",
                "endpoint": {"scheme": "process", "host": "local", "port": null, "path": "/turn/start"},
                "request": {"bytes": 8, "blake3": "a".repeat(64)},
                "credential_handle_fingerprint": null
            }),
        );
        let dispatch_ref = cache_artifact(&ctx, &dispatch);
        let later_source = artifact(temp.path(), "operator.message", json!({"text": "later"}));
        let later_source_ref = cache_artifact(&ctx, &later_source);
        let later_dispatch = artifact(
            temp.path(),
            PROVIDER_TRANSPORT_DISPATCH_LABEL,
            json!({
                "label": PROVIDER_TRANSPORT_DISPATCH_LABEL,
                "provider_interface": "world_provider_codex",
                "source_movement_seq": 2,
                "attempt": 1,
                "method": "turn/start",
                "transport": "process",
                "endpoint": {"scheme": "process", "host": "local", "port": null, "path": "/turn/start"},
                "request": {"bytes": 8, "blake3": "b".repeat(64)},
                "credential_handle_fingerprint": null
            }),
        );
        let later_dispatch_ref = cache_artifact(&ctx, &later_dispatch);
        write_movements(
            &ctx.movements_path,
            &[
                movement(
                    0,
                    MovementKind::InterfaceOutbound,
                    Endpoint::Interface(octopus_descriptors::InterfaceId::new("system_tool")),
                    Endpoint::Interface(ctx.interface.clone()),
                    source_ref,
                ),
                movement(
                    1,
                    MovementKind::InterfaceArrival,
                    Endpoint::Interface(ctx.interface.clone()),
                    Endpoint::Interface(InterfaceId::new("system_worker_test")),
                    dispatch_ref.clone(),
                ),
                movement(
                    2,
                    MovementKind::InterfaceOutbound,
                    Endpoint::Interface(octopus_descriptors::InterfaceId::new("system_operator")),
                    Endpoint::Interface(ctx.interface.clone()),
                    later_source_ref,
                ),
                movement(
                    3,
                    MovementKind::InterfaceArrival,
                    Endpoint::Interface(ctx.interface.clone()),
                    Endpoint::Interface(InterfaceId::new("system_worker_test")),
                    later_dispatch_ref,
                ),
            ],
        );
        let recovered = recovered_codex_handoff_artifacts(&ctx, &run, 0, &[])
            .await
            .unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].movement_seq, 1);
        assert_eq!(recovered[0].artifact, dispatch_ref);
    }

    fn session_entry(
        label: &str,
        payload: serde_json::Value,
        source_artifact: ArtifactRef,
        source_movement_seq: u64,
    ) -> serde_json::Value {
        json!({
            "label": label,
            "payload": payload,
            "source_artifact": source_artifact,
            "source_movement_seq": source_movement_seq,
        })
    }

    #[tokio::test]
    async fn operator_message_is_queued_during_interrupted_draining_before_steer_dispatch() {
        let pinned_run_record = pinned_test_run_record();
        for result_before_ack in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path();
            let run = RunId::new("run-a");
            let descriptor = descriptor();
            let interrupted = continuity(8, Some("interrupted_pending"));
            let pending = continuity(7, Some("pending"));
            let mut ack_result = CodexTurnControlResult {
                control_id: "control-a".to_string(),
                action: "stop".to_string(),
                acknowledged: true,
                thread_id: Some("thread-a".to_string()),
                turn_id: Some("turn-a".to_string()),
                detail: json!({"interrupted": true, "exact_result_pending": true}),
                continuity: Some(interrupted.clone()),
            };
            let ack = materialize_turn_control_result(
                &root.join("var/provider/emitted"),
                0,
                ack_result.clone(),
            )
            .unwrap();
            let ack_ref = materialized_artifact_ref(&ack).unwrap();
            let stop = artifact(
                &root.join("stop"),
                "operator.turn.control",
                json!({"action": "stop", "control_id": "control-a"}),
            );
            let stop_ref = materialized_artifact_ref(&stop).unwrap();
            let message = artifact(
                &root.join("message"),
                "operator.message",
                json!({"prompt": "continue"}),
            );
            let message_ref = materialized_artifact_ref(&message).unwrap();
            let result = artifact(
                &root.join("result"),
                "coder.exec.result",
                json!({
                    "label": "coder.exec.result",
                    "tool": "coder.exec",
                    "correlation_id": "call-a",
                    "ok": true,
                    "body": {"exit_code": 0}
                }),
            );
            let result_ref = materialized_artifact_ref(&result).unwrap();
            let result_envelope = InterfaceInvocationEnvelope {
                artifact: result_ref.clone(),
                artifact_file: result,
                artifact_binding_payloads: vec![bound(
                    "result-before-ack",
                    json!({
                        "label": "tool.call",
                        "codex_continuity": pending,
                    }),
                )],
                metadata: Some(runtime_metadata()),
            };
            let stop_envelope = InterfaceInvocationEnvelope {
                artifact: stop_ref.clone(),
                artifact_file: stop,
                artifact_binding_payloads: Vec::new(),
                metadata: Some(runtime_metadata()),
            };
            let message_envelope = InterfaceInvocationEnvelope {
                artifact: message_ref.clone(),
                artifact_file: message,
                artifact_binding_payloads: Vec::new(),
                metadata: Some(runtime_metadata()),
            };
            let result_seq = if result_before_ack { 1 } else { 2 };
            let ack_seq = if result_before_ack { 2 } else { 1 };
            let mut movements = vec![
                movement(
                    0,
                    MovementKind::InterfaceOutbound,
                    Endpoint::Interface(octopus_descriptors::InterfaceId::new("world_operator")),
                    Endpoint::Interface(descriptor.id.clone()),
                    stop_ref,
                ),
                movement(
                    result_seq,
                    MovementKind::InterfaceOutbound,
                    Endpoint::Interface(octopus_descriptors::InterfaceId::new("system_coder")),
                    Endpoint::Interface(descriptor.id.clone()),
                    result_ref.clone(),
                ),
                movement(
                    ack_seq,
                    MovementKind::InterfaceArrival,
                    Endpoint::Interface(descriptor.id.clone()),
                    Endpoint::Interface(InterfaceId::new("system_worker_test")),
                    ack_ref,
                ),
                movement(
                    3,
                    MovementKind::InterfaceOutbound,
                    Endpoint::Interface(octopus_descriptors::InterfaceId::new("world_operator")),
                    Endpoint::Interface(descriptor.id.clone()),
                    message_ref,
                ),
            ];
            movements.sort_by_key(|movement| movement.seq.0);
            let movements_path = root.join("ledger/movements.jsonl");
            write_movements(&movements_path, &movements);
            let ctx = context(root, &descriptor);
            let handoff = ProviderHandoffSession::new(&ctx).unwrap();
            let arbiter = InterruptedEffectArbiter::new(ctx.clone());
            hand_control_result(
                &ctx,
                &descriptor,
                &handoff,
                &arbiter,
                &run,
                ControlResultDelivery {
                    envelope: &stop_envelope,
                    source_movement_seq: 0,
                    output: None,
                    handoff_barrier: None,
                    effect_guard: None,
                    result: &mut ack_result,
                    recovery: true,
                    defer_message_receipt: false,
                },
            )
            .await
            .unwrap();
            let after_ack_restart = InterruptedEffectArbiter::new(ctx.clone());
            let recovered = after_ack_restart
                .durable_continuity(&run)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(recovered, interrupted);
            assert!(
                after_ack_restart
                    .begin_result(&run, result_seq, &result_envelope)
                    .await
                    .unwrap()
            );
            let resolving_restart = InterruptedEffectArbiter::new(ctx.clone());
            assert_eq!(
                resolving_restart.durable_continuity(&run).await.unwrap(),
                Some(interrupted.clone())
            );
            let broker = CodexTurnControlBroker::new(None);
            broker
                .publish_recovered_interrupted_pending(&interrupted)
                .unwrap();
            let (driver, release, mut methods, server) =
                interrupted_drain_driver(root, &descriptor).await;
            driver
                .seed_interrupted_dynamic_tool_request_for_test(&interrupted)
                .await
                .unwrap();
            let (output, mut output_rx) = ProviderOutputStream::channel();
            let first_output_root = ctx.interface_var_dir().join("emitted");
            let live_stream_root = ctx.interface_var_dir().join("streams");
            let receive = driver.receive_artifact_and_stream_outputs(
                crate::handler::CodexProviderInvocation {
                    callable_tool_names: result_envelope.granted_tool_names().unwrap().into_iter().collect(),
                    input: result_envelope.clone(),
                    emit: output,
                    resolved_session_snapshot: None,
                    current_session_context: Arc::new(Vec::new()),
                    output_root: &first_output_root,
                    live_stream_root: &live_stream_root,
                    source_movement_seq: result_seq,
                    attempt: 1,
                    admitted_run_record: &pinned_run_record,
                    current_run_record: &pinned_run_record,
                    deferred_context_generations: 0,
                    input_changes_context: true,
                    interrupted_after_artifact: None,
                    recovered_handoff_artifacts: Arc::new(Vec::new()),
                    durable_interrupted_continuity: Some(interrupted.clone()),
                    reconciled_result_continuity: None,
                    control: &broker,
                    handoff_barrier: ProviderHandoffBarrier::new(),
                },
            );
            let control_gates = Arc::new(tokio::sync::Mutex::new(BTreeMap::new()));
            let race = async {
                tokio::time::timeout(std::time::Duration::from_secs(1), async {
                    while !broker.is_interrupted_draining() {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .unwrap();
                let control = decode_admitted_turn_control(&ctx, &descriptor, &message_envelope)
                    .unwrap()
                    .expect("operator message is normally a steer candidate");
                let disposition = handle_control_source(
                    &ctx,
                    &descriptor,
                    &handoff,
                    &resolving_restart,
                    &run,
                    ControlSource {
                        envelope: &message_envelope,
                        source_movement_seq: 3,
                        broker: &broker,
                        control_gates: &control_gates,
                        control,
                        terminal_control_fenced: true,
                        independent_reader: false,
                    },
                )
                .await
                .unwrap();
                assert_eq!(disposition, ControlSourceDisposition::DeferredTurn);
                assert!(!control_journal_exists(&ctx, 3).unwrap());
                assert!(methods.try_recv().is_err());
                release.send(()).unwrap();
            };
            let (receive, ()) = tokio::join!(receive, race);
            receive.unwrap();
            server.await.unwrap();
            assert!(methods.try_recv().is_err());
            let ProviderStreamEmission::InterruptedResult {
                activity: None,
                stream: Some(stream),
                closure,
                ..
            } = output_rx.recv().await.unwrap()
            else {
                panic!("expected one atomic interrupted-result emission")
            };
            assert_eq!(
                stream.artifact.read_manifest().unwrap().label,
                "turn.stream"
            );
            let stream_rows = String::from_utf8(stream.artifact.read_payload_bytes().unwrap())
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
                .collect::<Vec<_>>();
            let mut activity_payload = stream_rows
                .iter()
                .find(|row| row["type"] == "event" && row["label"] == "turn.dynamic_tool_call")
                .unwrap()["payload"]
                .clone();
            assert_eq!(activity_payload["returned_to_model"], false);
            activity_payload.as_object_mut().unwrap().insert(
                "codex_continuity".to_string(),
                serde_json::to_value(&interrupted).unwrap(),
            );
            let legacy_activity = octopus_interfaces::InterfaceArtifactFile::write(
                root.join("legacy-interrupted-activity"),
                "turn.dynamic_tool_call",
                &serde_json::to_vec(&activity_payload).unwrap(),
            )
            .unwrap();
            assert_eq!(
                closure.artifact.read_manifest().unwrap().label,
                "turn.interrupted.result"
            );
            let first_closure_bytes = closure.artifact.read_payload_bytes().unwrap();
            let closure_payload: serde_json::Value =
                serde_json::from_slice(&first_closure_bytes).unwrap();
            assert_eq!(closure_payload["returned_to_model"], false);
            assert!(closure_payload.get("cancelled_turn_transport").is_none());
            let closed = &closure_payload["codex_continuity"];
            assert_eq!(closed["context_generation"], 10);
            assert!(closed["pending"].is_null() && closed["interrupted_pending"].is_null());
            let mut context: Vec<octopus_run_authoring::SessionContextArtifact> = vec![serde_json::from_value(session_entry(
                TURN_INTERRUPTED_RESULT_LABEL, closure_payload.clone(),
                materialized_artifact_ref(&closure.artifact).unwrap(), 5,
            )).unwrap()];
            let settled = serde_json::to_value(terminal_output_continuity(&descriptor, &message_envelope, &context, &run,
                br#"{"id":"run-a","interface_state":{"world_provider_codex":{"context_generation":10}}}"#,
                0,
            ).unwrap().unwrap()).unwrap();
            assert_eq!(settled["thread_id"], "thread-a");
            assert_eq!(settled["context_generation"], 11);
            context[0].payload["codex_continuity"]["context_generation"] = json!(11);
            assert!(terminal_output_continuity(&descriptor, &message_envelope, &context, &run,
                br#"{"id":"run-a","interface_state":{"world_provider_codex":{"context_generation":10}}}"#,
                0,
            ).unwrap_err().to_string().contains("terminal Codex continuity is ahead of its authored context"));
            let settled = terminal_output_continuity(&descriptor, &message_envelope, &context, &run,
                br#"{"id":"run-a","interface_state":{"world_provider_codex":{"context_generation":8}}}"#,
                3,
            ).unwrap().unwrap();
            assert_eq!(settled.context_generation, 12);
            assert!(terminal_output_continuity(&descriptor, &message_envelope, &context, &run,
                br#"{"id":"run-a","interface_state":{"world_provider_codex":{"context_generation":8}}}"#,
                2,
            ).is_err());
            assert!(output_rx.recv().await.is_none());
            let activity_ref = materialized_artifact_ref(&legacy_activity).unwrap();
            let closure_ref = materialized_artifact_ref(&closure.artifact).unwrap();
            let mut replay = QueuedTurn { envelope: result_envelope.clone(), seq: result_seq };
            let (recovered, recovered_ref) = recover_sealed_interrupted_result(
                &ctx, &descriptor, &replay, &interrupted,
            ).unwrap().unwrap();
            assert_eq!(recovered_ref, closure_ref);
            assert_eq!(recovered.len(), 2);
            assert_eq!(recovered[0].artifact.read_payload_bytes().unwrap(), stream.artifact.read_payload_bytes().unwrap());
            assert_eq!(recovered[1].artifact.read_payload_bytes().unwrap(), first_closure_bytes);
            replay.seq += 1;
            assert!(recover_sealed_interrupted_result(&ctx, &descriptor, &replay, &interrupted).is_err());
            let failed_context_session = ProviderHandoffSession::new(&ctx).unwrap();
            let replay_control = CodexTurnControlBroker::new(Some(result_seq));
            assert!(recover_interrupted_result_without_context(
                &ctx, &descriptor, &failed_context_session, &resolving_restart, &replay_control, &run, &replay,
            ).await.is_err());
            assert!(!recover_interrupted_result_without_context(
                &ctx, &descriptor, &failed_context_session, &resolving_restart, &replay_control, &run,
                &QueuedTurn { envelope: message_envelope.clone(), seq: result_seq + 1 },
            ).await.unwrap());
            replay.seq = result_seq;
            replay.envelope.artifact.content = ContentHash::of_bytes(b"another result");
            assert!(recover_sealed_interrupted_result(&ctx, &descriptor, &replay, &interrupted).is_err());
            resolving_restart
                .begin_closing(&run, result_seq, closure_ref.clone())
                .await
                .unwrap();
            let closing_restart = InterruptedEffectArbiter::new(ctx.clone());
            assert_eq!(
                closing_restart.durable_continuity(&run).await.unwrap(),
                Some(interrupted.clone())
            );
            append_movement(
                &movements_path,
                &movement(
                    4,
                    MovementKind::InterfaceArrival,
                    Endpoint::Interface(descriptor.id.clone()),
                    Endpoint::Interface(InterfaceId::new("system_worker_test")),
                    activity_ref.clone(),
                ),
            );
            assert_eq!(
                find_artifact_admission(&ctx, &run, result_seq, &activity_ref)
                    .unwrap()
                    .movement_seq,
                Some(4)
            );
            let recovery_broker = CodexTurnControlBroker::new(None);
            recovery_broker
                .publish_recovered_interrupted_pending(&interrupted)
                .unwrap();
            let (recovery_output, mut recovery_rx) = ProviderOutputStream::channel();
            driver
                .receive_artifact_and_stream_outputs(crate::handler::CodexProviderInvocation {
                    callable_tool_names: result_envelope.granted_tool_names().unwrap().into_iter().collect(),
                    input: result_envelope,
                    emit: recovery_output,
                    resolved_session_snapshot: None,
                    current_session_context: Arc::new(Vec::new()),
                    output_root: &first_output_root,
                    live_stream_root: &live_stream_root,
                    source_movement_seq: result_seq,
                    attempt: 2,
                    admitted_run_record: &pinned_run_record,
                    current_run_record: &pinned_run_record,
                    deferred_context_generations: 0,
                    input_changes_context: true,
                    interrupted_after_artifact: Some("turn.dynamic_tool_call".to_string()),
                    recovered_handoff_artifacts: Arc::new(Vec::new()),
                    durable_interrupted_continuity: Some(interrupted.clone()),
                    reconciled_result_continuity: None,
                    control: &recovery_broker,
                    handoff_barrier: ProviderHandoffBarrier::new(),
                })
                .await
                .unwrap();
            let ProviderStreamEmission::InterruptedResult {
                activity: None,
                stream: Some(recovered_stream),
                closure: recovered_closure,
                ..
            } = recovery_rx.recv().await.unwrap()
            else {
                panic!("activity-only recovery must emit only the exact closure")
            };
            assert_eq!(
                recovered_closure.artifact.read_payload_bytes().unwrap(),
                first_closure_bytes
            );
            assert_eq!(
                recovered_stream.artifact.read_payload_bytes().unwrap(),
                stream.artifact.read_payload_bytes().unwrap()
            );
            assert!(recovery_rx.recv().await.is_none());
            let recovered_closure_ref =
                materialized_artifact_ref(&recovered_closure.artifact).unwrap();
            assert_eq!(recovered_closure_ref, closure_ref);
            closing_restart
                .begin_closing(&run, result_seq, recovered_closure_ref.clone())
                .await
                .unwrap();
            append_movement(
                &movements_path,
                &movement(
                    5,
                    MovementKind::InterfaceArrival,
                    Endpoint::Interface(descriptor.id.clone()),
                    Endpoint::Interface(InterfaceId::new("system_worker_test")),
                    recovered_closure_ref.clone(),
                ),
            );
            let reconciled_restart = InterruptedEffectArbiter::new(ctx.clone());
            assert!(
                reconciled_restart
                    .durable_continuity(&run)
                    .await
                    .unwrap()
                    .is_some()
            );
            reconciled_restart
                .recover_completed_source(
                    &run,
                    result_seq,
                    &[ProviderHandoffOutcome::Admitted {
                        seq: 5,
                        artifact: recovered_closure_ref.clone(),
                    }],
                )
                .await
                .unwrap();
            assert!(
                reconciled_restart
                    .durable_continuity(&run)
                    .await
                    .unwrap()
                    .is_none()
            );
            assert_eq!(
                find_artifact_admission(&ctx, &run, result_seq, &recovered_closure_ref)
                    .unwrap()
                    .movement_seq,
                Some(5)
            );
            assert!(
                InterruptedEffectArbiter::new(ctx)
                    .durable_continuity(&run)
                    .await
                    .unwrap()
                    .is_none()
            );
            broker.finish();
            recovery_broker.finish();
        }
    }

    #[tokio::test]
    async fn result_and_ack_orders_reconcile_once_across_each_restart_boundary() {
        for result_before_ack in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path();
            let run = RunId::new("run-a");
            let descriptor = descriptor();
            let pending = continuity(7, Some("pending"));
            let interrupted = continuity(8, Some("interrupted_pending"));
            let closed = continuity(10, None);
            let stop = artifact(
                &root.join("stop"),
                "operator.turn.control",
                json!({"action": "stop", "control_id": "control-a"}),
            );
            let stop_ref = materialized_artifact_ref(&stop).unwrap();
            let result = artifact(
                &root.join("result"),
                "coder.exec.result",
                json!({
                    "label": "coder.exec.result",
                    "tool": "coder.exec",
                    "correlation_id": "call-a",
                    "ok": true,
                    "body": {"exit_code": 0}
                }),
            );
            let result_ref = materialized_artifact_ref(&result).unwrap();
            let mut ack_result = CodexTurnControlResult {
                control_id: "control-a".to_string(),
                action: "stop".to_string(),
                acknowledged: true,
                thread_id: Some("thread-a".to_string()),
                turn_id: Some("turn-a".to_string()),
                detail: json!({"interrupted": true, "exact_result_pending": true}),
                continuity: Some(interrupted.clone()),
            };
            let ack = materialize_turn_control_result(
                &root.join("var/provider/emitted"),
                0,
                ack_result.clone(),
            )
            .unwrap();
            let ack_ref = materialized_artifact_ref(&ack).unwrap();
            let result_seq = if result_before_ack { 1 } else { 2 };
            let ack_seq = if result_before_ack { 2 } else { 1 };
            let mut ordered = vec![movement(
                0,
                MovementKind::InterfaceOutbound,
                Endpoint::Interface(octopus_descriptors::InterfaceId::new("world_operator")),
                Endpoint::Interface(descriptor.id.clone()),
                stop_ref,
            )];
            ordered.push(movement(
                result_seq,
                MovementKind::InterfaceOutbound,
                Endpoint::Interface(octopus_descriptors::InterfaceId::new("system_coder")),
                Endpoint::Interface(descriptor.id.clone()),
                result_ref.clone(),
            ));
            ordered.push(movement(
                ack_seq,
                MovementKind::InterfaceArrival,
                Endpoint::Interface(descriptor.id.clone()),
                Endpoint::Interface(InterfaceId::new("system_worker_test")),
                ack_ref.clone(),
            ));
            ordered.sort_by_key(|movement| movement.seq.0);
            let movements_path = root.join("ledger/movements.jsonl");
            write_movements(&movements_path, &ordered);
            let ctx = context(root, &descriptor);
            let arbiter = InterruptedEffectArbiter::new(ctx.clone());
            let stop_envelope = InterfaceInvocationEnvelope {
                artifact: materialized_artifact_ref(&stop).unwrap(),
                artifact_file: stop,
                artifact_binding_payloads: Vec::new(),
                metadata: None,
            };
            let handoff = ProviderHandoffSession::new(&ctx).unwrap();
            hand_control_result(
                &ctx,
                &descriptor,
                &handoff,
                &arbiter,
                &run,
                ControlResultDelivery {
                    envelope: &stop_envelope,
                    source_movement_seq: 0,
                    output: None,
                    handoff_barrier: None,
                    effect_guard: None,
                    result: &mut ack_result,
                    recovery: true,
                    defer_message_receipt: false,
                },
            )
            .await
            .unwrap();

            let after_ack_restart = InterruptedEffectArbiter::new(ctx.clone());
            let durable = after_ack_restart
                .durable_continuity(&run)
                .await
                .unwrap()
                .unwrap();
            let (reconciled, override_used) =
                crate::handler::reconcile_durable_interrupted_continuity(
                    Some(pending),
                    Some(durable.clone()),
                )
                .unwrap();
            assert!(override_used);
            assert_eq!(reconciled, Some(interrupted.clone()));
            let result_envelope = InterfaceInvocationEnvelope {
                artifact: result_ref.clone(),
                artifact_file: result,
                artifact_binding_payloads: Vec::new(),
                metadata: None,
            };
            assert!(
                after_ack_restart
                    .begin_result(&run, result_seq, &result_envelope)
                    .await
                    .unwrap()
            );
            let after_result_claim_restart = InterruptedEffectArbiter::new(ctx.clone());
            assert!(
                after_result_claim_restart
                    .durable_continuity(&run)
                    .await
                    .unwrap()
                    .is_some()
            );
            let resolving_journal: InterruptedEffectJournal = serde_json::from_slice(
                &std::fs::read(interrupted_effect_journal_path(&ctx, &run)).unwrap(),
            )
            .unwrap();
            assert!(matches!(
                resolving_journal.phase,
                InterruptedEffectPhase::Resolving {
                    result_source_movement_seq: current_source,
                    ref source_artifact,
                } if current_source == result_seq && source_artifact == &result_ref
            ));
            let closure = artifact(
                &root.join("closure"),
                "turn.interrupted.result",
                json!({
                    "label": "turn.interrupted.result",
                    "result_kind": "tool_result",
                    "correlation_id": "call-a",
                    "thread_id": "thread-a",
                    "turn_id": "turn-a",
                    "ok": true,
                    "turn_interrupted": true,
                    "returned_to_model": false,
                    "result_disposition": "durably_admitted_after_interrupt",
                    "source_artifact": result_ref.clone(),
                    "codex_continuity": closed
                }),
            );
            let closure_payload: serde_json::Value =
                serde_json::from_slice(&closure.read_payload_bytes().unwrap()).unwrap();
            assert_eq!(closure_payload["returned_to_model"], false);
            let closure_ref = interrupted_result_closure_ref(&closure, &durable, &result_ref, &codex_context_output_labels(&descriptor).unwrap())
                .unwrap()
                .unwrap();
            after_result_claim_restart
                .begin_closing(&run, result_seq, closure_ref.clone())
                .await
                .unwrap();

            let before_closure_restart = InterruptedEffectArbiter::new(ctx.clone());
            assert!(
                before_closure_restart
                    .durable_continuity(&run)
                    .await
                    .unwrap()
                    .is_some()
            );
            let admitted_closure_ref = ArtifactRef {
                id: ArtifactId::new("turn.interrupted.result:post-handoff"),
                content: ContentHash::of_bytes(b"post-handoff-session-snapshot"),
            };
            append_movement(
                &movements_path,
                &movement(
                    3,
                    MovementKind::InterfaceArrival,
                    Endpoint::Interface(descriptor.id.clone()),
                    Endpoint::Interface(InterfaceId::new("system_worker_test")),
                    admitted_closure_ref.clone(),
                ),
            );

            let after_closure_restart = InterruptedEffectArbiter::new(ctx.clone());
            assert!(
                after_closure_restart
                    .durable_continuity(&run)
                    .await
                    .unwrap()
                    .is_some()
            );
            after_closure_restart
                .recover_completed_source(
                    &run,
                    result_seq,
                    &[ProviderHandoffOutcome::Admitted {
                        seq: 3,
                        artifact: admitted_closure_ref.clone(),
                    }],
                )
                .await
                .unwrap();
            assert!(
                after_closure_restart
                    .durable_continuity(&run)
                    .await
                    .unwrap()
                    .is_none()
            );
            let journal: InterruptedEffectJournal = serde_json::from_slice(
                &std::fs::read(interrupted_effect_journal_path(&ctx, &run)).unwrap(),
            )
            .unwrap();
            assert!(matches!(
                journal.phase,
                InterruptedEffectPhase::Reconciled {
                    result_source_movement_seq,
                    closure: DurableArtifactAdmission {
                        ref artifact,
                        movement_seq: 3,
                    }
                } if result_source_movement_seq == result_seq && artifact == &admitted_closure_ref
            ));
            assert_eq!(
                find_artifact_admission(&ctx, &run, result_seq, &admitted_closure_ref)
                    .unwrap()
                    .movement_seq,
                Some(3)
            );
            let settled_restart = InterruptedEffectArbiter::new(ctx.clone());
            assert!(
                !settled_restart
                    .begin_result(&run, result_seq + 1, &result_envelope)
                    .await
                    .unwrap()
            );
            assert!(
                settled_restart
                    .begin_result(&run, result_seq, &result_envelope)
                    .await
                    .unwrap()
            );
            assert!(
                InterruptedEffectArbiter::new(ctx)
                    .durable_continuity(&run)
                    .await
                    .unwrap()
                    .is_none()
            );
        }
    }

    #[tokio::test]
    async fn admission_scan_waits_for_a_complete_concurrent_jsonl_tail() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let run = RunId::new("run-a");
        let descriptor = descriptor();
        let ctx = context(root, &descriptor);
        let expected = ArtifactRef {
            id: ArtifactId::new("turn.control.acknowledged:exact"),
            content: ContentHash::of_bytes(b"exact-ack"),
        };
        let source = movement(
            0,
            MovementKind::InterfaceOutbound,
            Endpoint::Interface(octopus_descriptors::InterfaceId::new("world_operator")),
            Endpoint::Interface(descriptor.id.clone()),
            ArtifactRef {
                id: ArtifactId::new("operator.turn.control:source"),
                content: ContentHash::of_bytes(b"source"),
            },
        );
        let admitted = movement(
            1,
            MovementKind::InterfaceArrival,
            Endpoint::Interface(descriptor.id.clone()),
            Endpoint::Interface(InterfaceId::new("system_worker_test")),
            expected.clone(),
        );
        let trailing = movement(
            2,
            MovementKind::InterfaceOutbound,
            Endpoint::Interface(octopus_descriptors::InterfaceId::new("world_operator")),
            Endpoint::Interface(descriptor.id.clone()),
            ArtifactRef {
                id: ArtifactId::new("operator.message:partial"),
                content: ContentHash::of_bytes(b"partial"),
            },
        );
        let path = root.join("ledger/movements.jsonl");
        write_movements(&path, &[source.clone(), admitted.clone()]);
        let trailing_bytes = serde_json::to_vec(&trailing).unwrap();
        let split = trailing_bytes.len() / 2;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        std::io::Write::write_all(&mut file, &trailing_bytes[..split]).unwrap();
        std::io::Write::flush(&mut file).unwrap();
        let scan = find_artifact_admission(&ctx, &run, 0, &expected).unwrap();
        assert_eq!(scan.movement_seq, Some(1));
        assert!(!scan.complete_boundary);

        let admitted_bytes = serde_json::to_vec(&admitted).unwrap();
        let split = admitted_bytes.len() / 2;
        write_movements(&path, std::slice::from_ref(&source));
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        std::io::Write::write_all(&mut file, &admitted_bytes[..split]).unwrap();
        std::io::Write::flush(&mut file).unwrap();
        drop(file);
        let wait = await_artifact_admission_scan(&ctx, &run, 0, &expected);
        let complete = async {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            std::io::Write::write_all(&mut file, &admitted_bytes[split..]).unwrap();
            std::io::Write::write_all(&mut file, b"\n").unwrap();
            file.sync_all().unwrap();
            drop(file);
            sync_ledger_index(&path);
        };
        let (scan, ()) = tokio::join!(wait, complete);
        let scan = scan.unwrap();
        assert_eq!(scan.movement_seq, Some(1));
        assert!(scan.complete_boundary);

        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        std::io::Write::write_all(&mut file, b"{malformed}\n").unwrap();
        file.sync_all().unwrap();
        let error = find_artifact_admission(&ctx, &run, 0, &expected).unwrap_err();
        assert!(error.contains("malformed row 2"), "{error}");

        let mut failed = serde_json::to_value(&admitted).unwrap();
        failed["invocation_metadata"]["verification_failure"] = json!({"error":"no_requirement_for_label"});
        let failed: Movement = serde_json::from_value(failed).unwrap();
        let mut later = admitted.clone(); later.seq.0 = 2; later.lamport.0 = 3;
        write_movements(&path, &[source.clone(), failed.clone(), later.clone()]);
        assert_eq!(find_artifact_admission(&ctx, &run, 0, &expected).unwrap().movement_seq, Some(2));
        assert!(validate_artifact_admission(&ctx, &run, 0, &DurableArtifactAdmission {
            artifact: expected.clone(), movement_seq: 1,
        }).is_err());
        write_movements(&path, &[source.clone(), failed]);
        assert_eq!(find_artifact_admission(&ctx, &run, 0, &expected).unwrap().movement_seq, None);
        write_movements(&path, &[source, admitted, later]);
        assert!(find_artifact_admission(&ctx, &run, 0, &expected).unwrap_err().contains("duplicate provider admissions"));
    }

    #[tokio::test]
    async fn terminal_carrierless_release_survives_later_carrier_before_journal_delete() {
        for with_settled_continuity in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path();
            let descriptor = descriptor();
            let ctx = context(root, &descriptor);
            let run = RunId::new("run-a");
            let file_bytes = [0xff, 0, 0x81, b'A'].repeat(32 * 1024);
            let file = InterfaceArtifactFile::write(root.join("binary"), "operator.file.binary", &file_bytes).unwrap();
            let file_ref = cache_artifact(&ctx, &file);
            let attachment = json!({"name":"résumé.zip","label":"operator.file.binary","mime_type":"application/zip",
                "size":file_bytes.len(),"payload_path":file.read_manifest().unwrap().payload_path,
                "payload_content_hash":format!("{:x}", <sha2::Sha256 as sha2::Digest>::digest(&file_bytes)),"last_modified":0});
            let message = artifact(
                &root.join("message"),
                "operator.message",
                json!({"text": "next task", "message_id": "message-a", "attachments":[attachment]}),
            );
            let message_ref = cache_artifact(&ctx, &message);
            let mut artifact_binding_payloads = with_settled_continuity
                .then(|| {
                    bound(
                        "settled-context",
                        json!({
                            "label": "interface.artifact.output",
                            "codex_continuity": continuity(1, None),
                        }),
                    )
                })
                .into_iter()
                .collect::<Vec<_>>();
            for bound in &artifact_binding_payloads {
                std::fs::write(ctx.blobs_dir.join(bound.artifact.content.to_hex()), bound.read_bytes().unwrap().as_ref()).unwrap();
            }
            artifact_binding_payloads.push(octopus_interfaces::ArtifactBindingPayload::new(file_ref.clone(), file_bytes.to_vec()));
            let mut pinned: serde_json::Value = serde_json::from_slice(&pinned_test_run_record()).unwrap();
            let checkpoint = &mut pinned["checkpoints"][0];
            checkpoint["artifact_bindings"] = json!(artifact_binding_payloads.iter().map(|bound| json!({"id":bound.artifact.id})).collect::<Vec<_>>());
            let input_binding = checkpoint["interface_binding"].clone();
            let outputs = checkpoint["outbound_interface_bindings"].clone();
            pinned["interface_state"] = json!({"world_provider_codex":{"run_authoring_binding":{"schema_version":2,
                "run":"run-a","worker":"system_worker_test","provider_interface":"world_provider_codex",
                "authoring_source_interface":"world_provider_codex","payload_artifact_bindings":[file_ref],
                "checkpoints":[{"name":"input","role":"stable","include_context":true,"interface_binding":input_binding,
                    "outbound_interface_bindings":outputs,"required_verified_artifacts":[]}]}}});
            let pinned = serde_json::to_vec(&pinned).unwrap();
            octopus_run_record::RunRecord::from_bytes(&pinned).unwrap();
            let pin = ContentHash::of_bytes(&pinned);
            std::fs::write(ctx.blobs_dir.join(pin.to_hex()), &pinned).unwrap();
            let mut metadata = runtime_metadata();
            metadata.runtime.as_mut().unwrap().run_record_hash = Some(pin.to_hex());
            metadata.grant.artifact_access.insert(file_ref.id.to_string(), Default::default());
            metadata.artifact_binding_refs = artifact_binding_payloads
                .iter()
                .map(|bound| bound.artifact.clone())
                .collect();
            let turn = QueuedTurn {
                envelope: InterfaceInvocationEnvelope {
                    artifact: message_ref.clone(),
                    artifact_file: message,
                    artifact_binding_payloads,
                    metadata: Some(metadata.clone()),
                },
                seq: 0,
            };
            let decode = |envelope: &InterfaceInvocationEnvelope| {
                let Some(CodexTurnControl::Steer { control_id, input, .. }) = decode_admitted_turn_control(&ctx, &descriptor, envelope).unwrap()
                    else { panic!("exact selected operator source must remain steering") };
                assert_eq!(control_id, "message-a");
                let octopus_appserver_proto::UserInput::Text { text, .. } = &input[0] else { panic!("missing selected-file metadata") };
                serde_json::from_str::<serde_json::Value>(text).unwrap()
            };
            let visible = decode(&turn.envelope);
            let chosen: ArtifactRef = serde_json::from_value(visible["selected_files"][0]["artifact_ref"].clone()).unwrap();
            assert_eq!(chosen, file_ref);
            assert_eq!(visible["selected_files"][0]["filename"], "résumé.zip");
            let mut revoked = turn.envelope.clone();
            revoked.metadata.as_mut().unwrap().grant.artifact_access.clear();
            assert!(decode_admitted_turn_control(&ctx, &descriptor, &revoked).is_err());
            assert!(decode_inflight_turn_control(&descriptor, &turn.envelope, Some(&pinned_test_run_record())).is_err());
            let mut cleared = turn.envelope.clone();
            cleared.metadata.as_mut().unwrap().runtime.as_mut().unwrap().run_record_hash =
                Some(ContentHash::of_bytes(&pinned_test_run_record()).to_hex());
            assert!(decode_admitted_turn_control(&ctx, &descriptor, &cleared).is_err());
            let mut source = movement(
                0,
                MovementKind::InterfaceOutbound,
                Endpoint::Interface(octopus_descriptors::InterfaceId::new("world_operator")),
                Endpoint::Interface(descriptor.id.clone()),
                message_ref.clone(),
            );
            source.invocation_metadata = Some(metadata.into());
            write_movements(&ctx.movements_path, &[source]);
            persist_control_journal(
                &ctx,
                0,
                &run,
                "message-a",
                "steer",
                TurnControlJournalPhase::DeferredTurn {
                    result: CodexTurnControlResult {
                        control_id: "message-a".to_string(),
                        action: "steer".to_string(),
                        acknowledged: false,
                        thread_id: None,
                        turn_id: None,
                        detail: json!({"error": "no active provider turn accepts this control artifact"}),
                        continuity: None,
                    },
                    cause: CodexDeferredTurnCause::TerminalTurnMoved,
                },
            )
            .unwrap();
            persist_deferred_provider_turn(&ctx, &run, &turn).unwrap();
            let deferred = DeferredProviderTurn {
                run: run.clone(),
                turn: turn.clone(),
                release: None,
            };
            let DeferredTurnReadiness::Ready(released) =
                prepare_deferred_provider_turn(&ctx, &descriptor, &run, &deferred, &ActiveTurnControls::default()).await.unwrap()
            else {
                panic!("terminal carrierless ownership must release the original envelope")
            };
            assert_eq!(released.envelope.artifact, message_ref);
            assert_deferred_receipt_round_trip(&ctx, &descriptor, &turn);
            assert_eq!(
                released.envelope.artifact_binding_payloads.len(),
                usize::from(with_settled_continuity) + 1
            );
            let completed = artifact(
                &root.join("completed-session"),
                "interface.artifact.output",
                json!({
                    "label": "interface.artifact.output",
                    "codex_continuity": continuity(2, None),
                    "session_context": {
                        "schema_version": 3,
                        "provider_interface": "world_provider_codex",
                        "artifacts": []
                    }
                }),
            );
            let completed_ref = cache_artifact(&ctx, &completed);
            append_movement(
                &ctx.movements_path,
                &movement(
                    1,
                    MovementKind::InterfaceArrival,
                    Endpoint::Interface(descriptor.id.clone()),
                    Endpoint::Interface(InterfaceId::new("system_worker_test")),
                    completed_ref.clone(),
                ),
            );
            let labels = codex_session_snapshot_labels(&descriptor).unwrap();
            let suffix = latest_provider_session_carrier(&ctx, &run, 0, &labels, None).unwrap();
            assert_eq!(
                suffix.latest.as_ref().map(|carrier| &carrier.artifact),
                Some(&completed_ref)
            );

            // Simulate restart after source completion but before journal deletion.
            let loaded = load_deferred_provider_turns(&ctx, &descriptor).unwrap();
            let recovered = loaded.get(&0).unwrap();
            assert_eq!(decode(&recovered.turn.envelope), visible);
            assert_eq!(ctx.pinned_run_record_bytes(&recovered.turn.envelope).unwrap(), pinned);
            let journal_path = deferred_provider_turn_path(&ctx, 0);
            let preserved = std::fs::read(&journal_path).unwrap();
            let actual_manifest = std::fs::read(ctx.blobs_dir.join(chosen.content.to_hex())).unwrap();
            for replacement in [vec![0xfe, 0, 0x81, b'A'], actual_manifest] {
                let mut changed: DeferredProviderTurnJournal = serde_json::from_slice(&preserved).unwrap();
                let bound = changed.artifact_bindings.iter_mut().find(|bound| bound.artifact == chosen).unwrap();
                bound.byte_count = replacement.len() as u64;
                bound.bytes_content = ContentHash::of_bytes(&replacement);
                std::fs::write(&journal_path, serde_json::to_vec(&changed).unwrap()).unwrap();
                assert!(load_deferred_provider_turns(&ctx, &descriptor).err().unwrap().contains("admitted manifest payload pin"));
            }
            std::fs::write(&journal_path, preserved).unwrap();
            assert!(matches!(
                recovered.release,
                Some(DeferredProviderTurnRelease::OriginalEnvelope {
                    observed_ledger_next_seq: 1,
                    ..
                })
            ));
            assert!(matches!(
                prepare_deferred_provider_turn(&ctx, &descriptor, &run, recovered, &ActiveTurnControls::default()).await.unwrap(),
                DeferredTurnReadiness::Ready(_)
            ));
            remove_deferred_provider_turn(&ctx, 0).unwrap();
            assert!(load_deferred_provider_turns(&ctx, &descriptor).unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn completed_turn_race_rebases_the_message_as_the_next_turn() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let descriptor = descriptor();
        let ctx = context(root, &descriptor);
        let run = RunId::new("run-a");
        let message = artifact(
            &root.join("message"),
            "operator.message",
            json!({"prompt": "next task", "message_id": "message-a"}),
        );
        let message_ref = cache_artifact(&ctx, &message);
        let turn = QueuedTurn {
            envelope: InterfaceInvocationEnvelope {
                artifact: message_ref.clone(),
                artifact_file: message,
                artifact_binding_payloads: Vec::new(),
                metadata: Some(runtime_metadata()),
            },
            seq: 0,
        };
        let completed = artifact(
            &root.join("completed-session"),
            "interface.artifact.output",
            json!({
                "label": "interface.artifact.output",
                "codex_continuity": continuity(1, None),
                "session_context": {
                    "schema_version": 3,
                    "provider_interface": "world_provider_codex",
                    "artifacts": [],
                }
            }),
        );
        let completed_ref = cache_artifact(&ctx, &completed);
        write_movements(
            &ctx.movements_path,
            &[
                movement(
                    0,
                    MovementKind::InterfaceOutbound,
                    Endpoint::Interface(octopus_descriptors::InterfaceId::new("world_operator")),
                    Endpoint::Interface(descriptor.id.clone()),
                    message_ref.clone(),
                ),
                movement(
                    1,
                    MovementKind::InterfaceArrival,
                    Endpoint::Interface(descriptor.id.clone()),
                    Endpoint::Interface(InterfaceId::new("system_worker_test")),
                    completed_ref.clone(),
                ),
            ],
        );

        assert!(operator_turn_requires_deferral(&ctx, &descriptor, &run, &turn, &[]).unwrap());
        persist_deferred_provider_turn(&ctx, &run, &turn).unwrap();
        assert_deferred_receipt_round_trip(&ctx, &descriptor, &turn);
        let deferred = DeferredProviderTurn {
            run: run.clone(),
            turn,
            release: None,
        };
        let DeferredTurnReadiness::Ready(rebased) =
            prepare_deferred_provider_turn(&ctx, &descriptor, &run, &deferred, &ActiveTurnControls::default()).await.unwrap()
        else {
            panic!("the completed-turn carrier must release the next ordinary turn")
        };
        assert_eq!(rebased.seq, 0);
        assert_eq!(rebased.envelope.artifact, message_ref);
        assert_eq!(rebased.envelope.artifact_binding_payloads.len(), 1);
        assert_eq!(
            rebased.envelope.artifact_binding_payloads[0].artifact,
            completed_ref
        );
        let journal: DeferredProviderTurnJournal =
            serde_json::from_slice(&std::fs::read(deferred_provider_turn_path(&ctx, 0)).unwrap())
                .unwrap();
        assert!(journal.release.as_ref().is_some_and(|release| matches!(
            release,
            DeferredProviderTurnRelease::SessionRebase {
                source_session_carrier: None,
                ..
            }
        )));
        assert_deferred_receipt_round_trip(&ctx, &descriptor, &deferred.turn);
    }

    #[tokio::test]
    async fn pending_result_releases_the_same_deferred_operator_message_once() {
        for (label, payload) in [
            ("operator.message", json!({"prompt": "next task", "message_id": "message-a"})),
            ("client.surface.document", json!({"schema": "octopus.client_surface", "id": "widget-a", "revision": 1})),
        ] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let descriptor = descriptor();
        let ctx = context(root, &descriptor);
        let run = RunId::new("run-a");
        let controls = ActiveTurnControls::default();
        let pending = continuity(7, Some("pending"));
        let original_content = json!({
            "label": "interface.artifact.output",
            "codex_continuity": pending,
        });
        let original = artifact(
            &root.join("original-session"),
            "interface.artifact.output",
            json!({
                "label": "interface.artifact.output",
                "codex_continuity": continuity(7, Some("pending")),
                "session_context": {
                    "schema_version": 3,
                    "provider_interface": "world_provider_codex",
                    "artifacts": [],
                }
            }),
        );
        let original_ref = cache_artifact(&ctx, &original);
        let result_payload = json!({
            "label": "coder.exec.result",
            "tool": "coder.exec",
            "correlation_id": "call-a",
            "ok": true,
            "body": {"exit_code": 0},
        });
        let result = artifact(
            &root.join("result"),
            "coder.exec.result",
            result_payload.clone(),
        );
        let result_ref = cache_artifact(&ctx, &result);
        let settled = artifact(
            &root.join("settled-session"),
            "interface.artifact.output",
            json!({
                "label": "interface.artifact.output",
                "codex_continuity": continuity(8, Some("pending")),
                "session_context": {
                    "schema_version": 3,
                    "provider_interface": "world_provider_codex",
                    "parent": original_ref.clone(),
                    "artifacts": [
                        session_entry(
                            "interface.artifact.output",
                            original_content,
                            original_ref.clone(),
                            0,
                        ),
                        session_entry(
                            "coder.exec.result",
                            result_payload,
                            result_ref.clone(),
                            1,
                        ),
                    ],
                }
            }),
        );
        let settled_ref = cache_artifact(&ctx, &settled);
        let message = artifact(
            &root.join("message"),
            label,
            payload.clone(),
        );
        let message_ref = cache_artifact(&ctx, &message);
        let mut metadata = runtime_metadata();
        metadata.artifact_binding_refs = vec![original_ref.clone(), result_ref.clone()];
        let turn = QueuedTurn {
            envelope: InterfaceInvocationEnvelope {
                artifact: message_ref.clone(),
                artifact_file: message,
                artifact_binding_payloads: vec![
                    octopus_interfaces::ArtifactBindingPayload::new(
                        original_ref.clone(),
                        original.read_payload_bytes().unwrap(),
                    ),
                    octopus_interfaces::ArtifactBindingPayload::new(
                        result_ref.clone(),
                        result.read_payload_bytes().unwrap(),
                    ),
                ],
                metadata: Some(metadata.clone()),
            },
            seq: 2,
        };
        let mut source = movement(
            2,
            MovementKind::InterfaceOutbound,
            Endpoint::Interface(octopus_descriptors::InterfaceId::new("world_operator")),
            Endpoint::Interface(descriptor.id.clone()),
            message_ref.clone(),
        );
        source.invocation_metadata = Some(metadata.into());
        write_movements(
            &ctx.movements_path,
            &[
                movement(
                    0,
                    MovementKind::InterfaceArrival,
                    Endpoint::Interface(descriptor.id.clone()),
                    Endpoint::Interface(InterfaceId::new("system_worker_test")),
                    original_ref.clone(),
                ),
                movement(
                    1,
                    MovementKind::InterfaceOutbound,
                    Endpoint::Interface(octopus_descriptors::InterfaceId::new("system_coder")),
                    Endpoint::Interface(descriptor.id.clone()),
                    result_ref,
                ),
                source,
            ],
        );

        assert!(is_provider_turn_input(&descriptor, &turn.envelope).unwrap());
        assert!(operator_turn_requires_deferral(&ctx, &descriptor, &run, &turn, &[]).unwrap());
        persist_deferred_provider_turn(&ctx, &run, &turn).unwrap();
        persist_deferred_provider_turn(&ctx, &run, &turn).unwrap();
        let deferred = DeferredProviderTurn {
            run: run.clone(),
            turn: turn.clone(),
            release: None,
        };
        assert!(matches!(
            prepare_deferred_provider_turn(&ctx, &descriptor, &run, &deferred, &controls).await.unwrap(),
            DeferredTurnReadiness::AwaitingSessionSettlement
        ));

        append_movement(
            &ctx.movements_path,
            &movement(
                3,
                MovementKind::InterfaceArrival,
                Endpoint::Interface(descriptor.id.clone()),
                Endpoint::Interface(InterfaceId::new("system_worker_test")),
                settled_ref.clone(),
            ),
        );
        let broker = CodexTurnControlBroker::new(Some(1));
        controls.lock().await.insert(run.to_string(), broker.clone());
        for interrupted in [false, true] {
            if interrupted { broker.publish_recovered_interrupted_pending(&continuity(8, Some("interrupted_pending"))).unwrap(); }
            assert!(matches!(prepare_deferred_provider_turn(&ctx, &descriptor, &run, &deferred, &controls).await.unwrap(),
                DeferredTurnReadiness::AwaitingSessionSettlement));
            assert!(load_deferred_provider_turns(&ctx, &descriptor).unwrap().get(&2).unwrap().release.is_none());
        }
        broker.finish();
        let DeferredTurnReadiness::Ready(rebased) =
            prepare_deferred_provider_turn(&ctx, &descriptor, &run, &deferred, &controls).await.unwrap()
        else {
            panic!("the exact pending result must release the queued message")
        };
        assert_eq!(rebased.seq, 2);
        assert_eq!(rebased.envelope.artifact_file.read_manifest().unwrap().label, label);
        assert_eq!(rebased.envelope.artifact_file.read_payload_bytes().unwrap(), turn.envelope.artifact_file.read_payload_bytes().unwrap());
        assert_eq!(rebased.envelope.artifact, message_ref);
        assert_eq!(rebased.envelope.artifact_binding_payloads.len(), 1);
        assert_eq!(
            rebased.envelope.artifact_binding_payloads[0].artifact,
            settled_ref
        );

        persist_deferred_provider_turn(&ctx, &run, &turn).unwrap();
        let loaded = load_deferred_provider_turns(&ctx, &descriptor).unwrap();
        assert_eq!(loaded.len(), 1);
        let recovered = loaded.get(&2).unwrap();
        assert!(recovered.release.is_some());
        assert!(matches!(
            prepare_deferred_provider_turn(&ctx, &descriptor, &run, recovered, &controls).await.unwrap(),
            DeferredTurnReadiness::Ready(_)
        ));
        remove_deferred_provider_turn(&ctx, 2).unwrap();
        assert!(load_deferred_provider_turns(&ctx, &descriptor).unwrap().is_empty());

        let ctx = context(&root.join("interrupted"), &descriptor);
        let interrupted = continuity(7, Some("interrupted_pending"));
        let closed = continuity(8, None);
        let partial = artifact(&root.join("interrupted-session"), "interface.artifact.output", json!({
            "label":"interface.artifact.output", "codex_continuity":interrupted,
            "session_context":{"schema_version":3,"provider_interface":"world_provider_codex","artifacts":[]},
        }));
        let partial_ref = cache_artifact(&ctx, &partial);
        let result_ref = cache_artifact(&ctx, &result);
        let closure_file = artifact(&root.join("interrupted-closure"), TURN_INTERRUPTED_RESULT_LABEL, json!({
            "label":TURN_INTERRUPTED_RESULT_LABEL,"codex_continuity":closed,
            "result_kind":"tool_result","correlation_id":"call-a","thread_id":"thread-a","turn_id":"turn-a",
            "ok":true,"turn_interrupted":true,"returned_to_model":false,
            "result_disposition":"durably_admitted_after_interrupt","source_artifact":result_ref,
        }));
        let closure = DurableArtifactAdmission { artifact:cache_artifact(&ctx, &closure_file), movement_seq:3 };
        let result_source = DurableArtifactAdmission { artifact:result_ref, movement_seq:2 };
        std::fs::remove_file(result.read_manifest().unwrap().payload_path).unwrap();
        std::fs::remove_file(closure_file.read_manifest().unwrap().payload_path).unwrap();
        assert!(result.read_payload_bytes().is_err());
        assert!(closure_file.read_payload_bytes().is_err());
        assert!(read_pinned_provider_artifact_payload(&ctx, &result_source.artifact, "interrupted result").is_ok());
        assert!(read_pinned_provider_artifact_payload(&ctx, &closure.artifact, "interrupted closure").is_ok());
        let mut original = turn.clone();
        original.envelope.artifact_binding_payloads = vec![octopus_interfaces::ArtifactBindingPayload::new(
            partial_ref.clone(), partial.read_payload_bytes().unwrap())];
        original.envelope.metadata.as_mut().unwrap().artifact_binding_refs = vec![partial_ref.clone()];
        let mut source = movement(1, MovementKind::InterfaceOutbound,
            Endpoint::Interface(InterfaceId::new("world_operator")), Endpoint::Interface(descriptor.id.clone()), message_ref);
        source.invocation_metadata = Some(original.envelope.metadata.clone().unwrap().into());
        let mut later_source = source.clone();
        later_source.seq = Seq(4);
        later_source.lamport = Lamport(5);
        write_movements(&ctx.movements_path, &[
            movement(0, MovementKind::InterfaceArrival, Endpoint::Interface(descriptor.id.clone()),
                Endpoint::Interface(InterfaceId::new("system_worker_test")), partial_ref.clone()), source,
            movement(2, MovementKind::InterfaceOutbound, Endpoint::Interface(InterfaceId::new("system_coder")),
                Endpoint::Interface(descriptor.id.clone()), result_source.artifact.clone()),
            movement(3, MovementKind::InterfaceArrival, Endpoint::Interface(descriptor.id.clone()),
                Endpoint::Interface(InterfaceId::new("system_worker_test")), closure.artifact.clone()), later_source,
        ]);
        let mut current = vec![octopus_run_authoring::SessionContextArtifact {
            label:"interface.artifact.output".into(), payload:json!({"label":"interface.artifact.output","codex_continuity":interrupted}),
            source_artifact:partial_ref, source_movement_seq:Some(0),
        }];
        for seq in [1, 4] {
            original.seq = seq;
            let pinned = ctx.pinned_run_record_bytes(&original.envelope).unwrap();
            let snapshot = bound_operator_session_snapshot(&ctx, &descriptor, &original).unwrap();
            assert_eq!(recover_provider_turn_continuity(&descriptor, &original.envelope,
                snapshot.as_ref().map(|(_, snapshot)| snapshot), &run, &pinned).unwrap(), Some(interrupted.clone()));
            let private = checked_interrupted_result_continuity(&ctx, &descriptor, &run, &original.envelope,
                &interrupted, &result_source, &closure, &current).await.unwrap();
            assert_eq!(private, closed);
            assert_eq!(original.envelope.artifact_binding_payloads.len(), 1);
            assert_eq!(current.len(), 1);
        }
        let later = continuity(10, None);
        current.push(octopus_run_authoring::SessionContextArtifact {
            label:"turn.agent_message".into(), payload:json!({"label":"turn.agent_message","codex_continuity":later}),
            source_artifact:ArtifactRef { id:ArtifactId::new("later-answer"), content:ContentHash::of_bytes(b"later") },
            source_movement_seq:Some(5),
        });
        assert_eq!(checked_interrupted_result_continuity(&ctx, &descriptor, &run, &original.envelope,
            &interrupted, &result_source, &closure, &current).await.unwrap(), later);
        assert!(reconcile_delivered_interrupted_continuity(Some(&interrupted), Some(continuity(10, Some("pending"))), Some(closed.clone())).is_err());
        for key in ["provider_interface", "run", "thread_id", "thread_shape_fingerprint", "static_context_fingerprint"] {
            let mut wrong = serde_json::to_value(&later).unwrap();
            wrong[key] = json!("different");
            assert!(reconcile_delivered_interrupted_continuity(Some(&interrupted),
                Some(serde_json::from_value(wrong).unwrap()), Some(closed.clone())).is_err());
        }
        let mut wrong = result_source.clone();
        wrong.artifact.content = ContentHash::of_bytes(b"different result");
        assert!(checked_interrupted_result_continuity(&ctx, &descriptor, &run, &original.envelope,
            &interrupted, &wrong, &closure, &current).await.is_err());
        let mut wrong = closure.clone();
        wrong.movement_seq = 4;
        assert!(checked_interrupted_result_continuity(&ctx, &descriptor, &run, &original.envelope,
            &interrupted, &result_source, &wrong, &current).await.is_err());
        }
    }

    #[tokio::test]
    async fn independent_message_receipt_waits_for_main_reader_after_output_closes() {
        let temp = tempfile::tempdir().unwrap();
        let descriptor = descriptor();
        let run = RunId::new("run-a");
        let file = artifact(&temp.path().join("message"), "provider.peer.message",
            json!({"action":"queue", "control_id":"queue-a", "text":"hello"}));
        let envelope = InterfaceInvocationEnvelope {
            artifact: materialized_artifact_ref(&file).unwrap(), artifact_file: file,
            artifact_binding_payloads: Vec::new(), metadata: Some(runtime_metadata()),
        };
        write_movements(&temp.path().join("ledger/movements.jsonl"), &[movement(
            0, MovementKind::InterfaceOutbound,
            Endpoint::Interface(octopus_descriptors::InterfaceId::new("system_peer")),
            Endpoint::Interface(descriptor.id.clone()), envelope.artifact.clone(),
        )]);
        let ctx = context(temp.path(), &descriptor);
        let handoff = ProviderHandoffSession::new(&ctx).unwrap();
        let arbiter = InterruptedEffectArbiter::new(ctx.clone());
        let broker = CodexTurnControlBroker::new(Some(0));
        broker.finish();
        let gates = ControlSourceGates::default();
        persist_control_journal(&ctx, 0, &run, "queue-a", "queue_message",
            TurnControlJournalPhase::Result { result: CodexTurnControlResult {
                control_id: "queue-a".into(), action: "queue_message".into(),
                acknowledged: true, thread_id: Some("thread-a".into()), turn_id: None,
                detail: json!({"queued":true}), continuity: None,
            }}).unwrap();
        let before = std::fs::read(&ctx.movements_path).unwrap();
        for _ in 0..2 {
            assert_eq!(handle_control_source(&ctx, &descriptor, &handoff, &arbiter, &run,
                ControlSource { envelope: &envelope, source_movement_seq: 0,
                    broker: &broker, control_gates: &gates,
                    control: CodexTurnControl::Inject { control_id: "queue-a".into(), items: vec![] },
                    terminal_control_fenced: true, independent_reader: true,
                }).await.unwrap(), ControlSourceDisposition::Handled);
            assert!(matches!(load_control_journal(&ctx, 0, &run, "queue-a", "queue_message")
                .unwrap().unwrap().phase, TurnControlJournalPhase::Result { .. }));
            assert_eq!(std::fs::read(&ctx.movements_path).unwrap(), before);
        }
    }

    #[test]
    fn terminal_broker_leaves_the_next_operator_message_for_the_main_reader() {
        let broker = CodexTurnControlBroker::new(Some(9));
        broker.finish();

        assert!(matches!(control_reader_ownership(Some(&broker), 10, 11, "operator.message"), ControlReaderOwnership::Main));
        assert!(matches!(control_reader_ownership(Some(&broker), 10, 12, "operator.turn.control"), ControlReaderOwnership::Independent));
    }
