use super::*;
use crate::state::Call;
use futures_util::{SinkExt, StreamExt};
use octopus_descriptors::{CheckpointId, EeId, RunId};
use octopus_interface_transport::{
    InterfaceActivationArrival, InterfaceHostIoLimits, InterfaceLiftContext,
};
use octopus_interfaces::{
    DeliveryVerdict, GrantProjection, InterfaceDescriptor, InterfaceInvocationEnvelope,
    InvocationMetadata, RuntimeInvocationMetadata,
};
use octopus_ledger::{
    CheckpointBindingPolicySnapshot, Endpoint, Lamport, Movement, MovementKind, Seq, TraceId,
};
use octopus_run_authoring::ProviderHandoffSession;
use std::{collections::BTreeSet, io::Write, path::PathBuf, time::Duration};
use tokio::{net::UnixListener, sync::mpsc};
use tokio_tungstenite::{tungstenite::Message, WebSocketStream};
use tokio_util::sync::CancellationToken;

struct Fixture {
    root: std::sync::Arc<tempfile::TempDir>,
    ctx: InterfaceLiftContext,
    descriptor: InterfaceDescriptor,
    input: InterfaceInvocationEnvelope,
    pinned: Vec<u8>,
    run_path: PathBuf,
    listener: UnixListener,
    arrivals: mpsc::Receiver<InterfaceActivationArrival>,
    source: String,
    door: String,
    source_seq: u64,
}
fn binding(id: &str, label: Option<&str>) -> AuthoredInterfaceBinding {
    let mut value =
        serde_json::to_value(AuthoredInterfaceBinding::all(InterfaceId::new(id))).unwrap();
    if let Some(label) = label {
        value["config"]["artifact_policy"] =
            json!({"mode":"allow","names":[label],"namespaces":[]});
    }
    serde_json::from_value(value).unwrap()
}
fn pin(file: &InterfaceArtifactFile, blobs: &Path) -> ArtifactRef {
    let manifest = file.read_manifest().unwrap();
    let identity = artifact_identity(file, &manifest.label).unwrap();
    std::fs::write(
        blobs.join(identity.content.to_hex()),
        std::fs::read(file.manifest_path()).unwrap(),
    )
    .unwrap();
    std::fs::write(
        blobs.join(manifest.payload_content_hash.unwrap()),
        file.read_payload_bytes().unwrap(),
    )
    .unwrap();
    identity
}
fn metadata(checkpoint: &Value, bytes: &[u8], interface: &str) -> InvocationMetadata {
    let record: Value = serde_json::from_slice(bytes).unwrap();
    InvocationMetadata::new(GrantProjection {
        tools: vec!["file.write".into()],
        worker_environment_kinds: vec!["system_room".into()],
        interfaces: ["voice", "source", "door", "control", "home", "client", "peer", "peer_source", "peer_door", "peer_control", "peer_client"]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        ee_root_slot_allowance: Some(vec!["run_records_root".into()]),
        ..Default::default()
    })
    .with_runtime(RuntimeInvocationMetadata {
        run: RunId::new(record["id"].as_str().unwrap()),
        ee: EeId::new("ee"),
        worker: InterfaceId::new("worker"),
        checkpoint: CheckpointId::new(checkpoint["id"].as_str().unwrap()),
        interface: InterfaceId::new(interface),
        epoch: checkpoint["epoch"].as_u64().unwrap(),
        run_record_hash: Some(ContentHash::of_bytes(bytes).to_hex()),
    })
    .with_outbound_interface_bindings(
        serde_json::from_value::<Vec<AuthoredInterfaceBinding>>(
            checkpoint["outbound_interface_bindings"].clone(),
        )
        .unwrap()
        .into_iter()
        .map(|b| b.interface)
        .collect(),
    )
    .with_checkpoint_binding_policy(CheckpointBindingPolicySnapshot {
        inbound: serde_json::from_value(checkpoint["interface_binding"].clone()).unwrap(),
        outbound: serde_json::from_value(checkpoint["outbound_interface_bindings"].clone())
            .unwrap(),
    })
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir_in("/tmp").unwrap();
        let base = root.path().canonicalize().unwrap();
        let blobs = base.join("blobs");
        let runs = base.join("runs");
        let control = base.join("control");
        for path in [&blobs, &runs, &control] {
            std::fs::create_dir_all(path).unwrap();
        }
        let ledger = base.join("movements.jsonl");
        std::fs::write(&ledger, b"").unwrap();
        let mut descriptor: Value =
            serde_json::from_str(include_str!("../../../../interface.json")).unwrap();
        descriptor["id"] = json!("voice");
        let descriptor: InterfaceDescriptor = serde_json::from_value(descriptor).unwrap();
        let mut templates = [
            ("input", "stable", "control", "voice", None),
            ("output", "provider_artifact_output", "voice", "client", Some("realtime.transcript")),
            ("delegation", "provider_interface_output", "voice", "home", Some("realtime.delegation")),
            ("write", "authoring_call", "source", "door", Some("tool.call")),
            ("result", "authoring_result", "door", "voice", Some("file.write.result")),
        ].into_iter().map(|(name,role,from,to,label)| json!({"name":name,"role":role,
            "interface_binding":binding(from,label),"outbound_interface_bindings":[binding(to,label)],
            "include_context":role == "stable","required_verified_artifacts":[]})).collect::<Vec<_>>();
        templates[1]["interface_binding"]["config"]["artifact_policy"]["names"] =
            json!(["realtime.transcript", "realtime.session.state", "realtime.session.answer"]);
        templates[1]["outbound_interface_bindings"][0]["config"]["artifact_policy"]["names"] =
            json!(["realtime.transcript", "realtime.session.state", "realtime.session.answer"]);
        templates.push(json!({"name":"peer_input","role":"owner_input","include_context":false,
            "owner_authoring":{"owner":"peer","source":"peer","door":"peer_door","tool":"file.write","call_label":"tool.call","result_label":"file.write.result"},
            "interface_binding":binding("peer",Some("realtime.session.update")),"outbound_interface_bindings":[binding("voice",Some("realtime.session.update"))],"required_verified_artifacts":[]}));
        let checkpoints = templates.iter().map(|t|json!({"id":t["name"],"run":"run","trace":"trace:run","epoch":0,
            "interface_binding":t["interface_binding"],"outbound_interface_bindings":t["outbound_interface_bindings"],
            "artifact_bindings":[],"required_verified_artifacts":[]})).collect::<Vec<_>>();
        let run = json!({"id":"run","ee":"ee","worker":"worker","trace":"trace:run","status":"running",
            "interface_state":{"voice":{"context_generation":0,"authoring_generation":0,
                "frontier_checkpoint_ids":["output","delegation","write","result"],
                "run_authoring_binding":{"schema_version":2,"run":"run","worker":"worker","provider_interface":"voice",
                    "authoring_source_interface":"source","checkpoints":templates}}},
            "checkpoints":checkpoints,"active_checkpoints":["input","output","delegation","write","result","peer_input"]});
        let mut pinned = serde_json::to_vec_pretty(&run).unwrap();
        pinned.push(b'\n');
        let run_path = runs.join("run.json");
        std::fs::write(&run_path, &pinned).unwrap();
        std::fs::write(blobs.join(ContentHash::of_bytes(&pinned).to_hex()), &pinned).unwrap();
        let file =
            InterfaceArtifactFile::write(base.join("voice/input"), "realtime.session.offer", b"{}")
                .unwrap();
        let input = InterfaceInvocationEnvelope {
            artifact: pin(&file, &blobs),
            artifact_file: file,
            artifact_binding_payloads: vec![],
            metadata: Some(metadata(&checkpoints[0], &pinned, "voice")),
        };
        let (sender, arrivals) = mpsc::channel(4);
        let ctx = InterfaceLiftContext::unprivileged(
            InterfaceId::new("voice"),
            base.join("registry/voice"),
            Some(descriptor.clone()),
            base.join("voice"),
            ledger,
            blobs,
            base.join("index.sqlite"),
            BTreeMap::from([
                ("run_records_root".into(), runs),
                ("interface_hand_in_root".into(), base.join("hand-in")),
                ("app_server_control_root".into(), control.clone()),
            ]),
            sender,
            CancellationToken::new(),
        )
        .with_host_io_limits(InterfaceHostIoLimits {
            max_control_frame_bytes: 1_048_576,
            max_stderr_bytes: 262_144,
            max_manifest_bytes: 1_048_576,
            max_payload_bytes: 268_435_456,
        })
        .unwrap();
        let socket = octopus_app_server_interface::interface_control_socket_path_from_root(
            &control,
            &InterfaceId::new("source"),
        );
        std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
        let listener = UnixListener::bind(socket).unwrap();
        let fixture = Self {
            root: std::sync::Arc::new(root),
            ctx,
            descriptor,
            input,
            pinned,
            run_path,
            listener,
            arrivals,
            source: "source".into(),
            door: "door".into(),
            source_seq: 0,
        };
        fixture.append(
            MovementKind::InterfaceOutbound,
            "control",
            "voice",
            fixture.input.artifact.clone(),
            fixture.input.metadata.clone(),
        );
        fixture
    }
    fn handoff(&self) -> ProviderOutputHandoff {
        ProviderHandoffSession::new(&self.ctx)
            .unwrap()
            .from_invocation(
                &self.ctx,
                &self.descriptor,
                &RunId::new("run"),
                &self.input,
                self.source_seq,
                &self.pinned,
            )
            .unwrap()
    }
    fn append(
        &self,
        kind: MovementKind,
        from: &str,
        to: &str,
        artifact: ArtifactRef,
        metadata: Option<InvocationMetadata>,
    ) -> u64 {
        let seq = std::fs::read_to_string(&self.ctx.movements_path)
            .unwrap()
            .lines()
            .count() as u64;
        let row = Movement {
            seq: Seq(seq),
            lamport: Lamport(seq + 1),
            recorded_at_unix_ms: seq + 1,
            trace: TraceId("trace:run".into()),
            kind,
            from: Endpoint::Interface(InterfaceId::new(from)),
            to: Endpoint::Interface(InterfaceId::new(to)),
            artifact: Some(artifact),
            invocation_metadata: metadata.map(Into::into),
        };
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&self.ctx.movements_path)
            .unwrap();
        writeln!(file, "{}", serde_json::to_string(&row).unwrap()).unwrap();
        file.sync_all().unwrap();
        seq
    }
    async fn accept_write(
        &self,
    ) -> (
        WebSocketStream<tokio::net::UnixStream>,
        Value,
        InterfaceArtifactFile,
    ) {
        let (socket, _) = self.listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(socket).await.unwrap();
        let manifest: Value =
            serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        let bytes = std::fs::read(manifest["payload_path"].as_str().unwrap()).unwrap();
        let call: Value = serde_json::from_slice(&bytes).unwrap();
        let manifests =
            std::fs::read_dir(self.ctx.root_slots["interface_hand_in_root"].join(&self.source))
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
                .filter(|path| {
                    serde_json::from_slice::<Value>(&std::fs::read(path).unwrap()).unwrap()
                        == manifest
                })
                .collect::<Vec<_>>();
        assert_eq!(
            manifests.len(),
            1,
            "exact private authoring attempt must already exist"
        );
        let file = InterfaceArtifactFile::new(manifests[0].clone()).unwrap();
        assert_eq!(
            serde_json::to_value(file.read_manifest().unwrap()).unwrap(),
            manifest
        );
        (ws, call, file)
    }
    fn write(&self, call: &Value, file: &InterfaceArtifactFile) -> u64 {
        let before = std::fs::read(&self.run_path).unwrap();
        assert_eq!(
            ContentHash::of_bytes(&before).to_hex(),
            call["arguments"]["expected_before_hash"]
        );
        let before_record: Value = serde_json::from_slice(&before).unwrap();
        let checkpoint = before_record["checkpoints"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["interface_binding"]["interface"] == self.source)
            .unwrap();
        let call_ref = pin(file, &self.ctx.blobs_dir);
        let call_seq = self.append(
            MovementKind::InterfaceArrival,
            &self.source,
            "worker",
            call_ref.clone(),
            Some(metadata(checkpoint, &before, &self.source)),
        );
        self.append(
            MovementKind::InterfaceOutbound,
            &self.source,
            &self.door,
            call_ref,
            Some(metadata(checkpoint, &before, &self.door)),
        );
        let invocation = InterfaceInvocationEnvelope {
            artifact: pin(file, &self.ctx.blobs_dir),
            artifact_file: file.clone(),
            artifact_binding_payloads: vec![],
            metadata: Some(metadata(checkpoint, &before, &self.door)),
        };
        let body = octopus_system_room_env::execute_file_write_result(
            &invocation, &call["arguments"], &["system_room".into()],
            &BTreeMap::from([("run_records_root".into(), self.ctx.root_slots["run_records_root"].clone())]),
            &self.ctx.var_dir.join("effects"),
        self.ctx.host_io_limits.as_ref().unwrap().max_payload_bytes,
        ).unwrap();
        assert_eq!(body["writes"][0]["after_hash"], call["arguments"]["expected_after_hash"]);
        let after = std::fs::read(&self.run_path).unwrap();
        std::fs::write(
            self.ctx
                .blobs_dir
                .join(ContentHash::of_bytes(&after).to_hex()),
            &after,
        )
        .unwrap();
        call_seq
    }
    fn write_result(&self, call: &Value) {
        let after = std::fs::read(&self.run_path).unwrap();
        let value: Value = serde_json::from_slice(&after).unwrap();
        let result=InterfaceArtifactFile::write(self.ctx.var_dir.join("results"),"file.write.result",&serde_json::to_vec(&json!({
            "label":"file.write.result","tool":"file.write","ok":true,"correlation_id":call["correlation_id"],
            "body":{"result":"files_written","writes":[{"path":self.run_path,"before_hash":call["arguments"]["expected_before_hash"],
                "after_hash":call["arguments"]["expected_after_hash"],"bytes":after.len()}]}})).unwrap()).unwrap();
        let checkpoint = value["checkpoints"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["interface_binding"]["interface"] == self.door)
            .unwrap();
        let mut result_metadata = metadata(checkpoint, &after, self.ctx.interface.as_str());
        result_metadata.checkpoint_admission = Some(octopus_ledger::CheckpointAdmissionMetadata {
            artifact_label: "file.write.result".into(),
            required_artifact_labels: vec![],
            complete: true,
        });
        let mut arrival_metadata = result_metadata.clone();
        arrival_metadata.runtime.as_mut().unwrap().interface = InterfaceId::new(&self.door);
        self.append(
            MovementKind::InterfaceArrival,
            &self.door,
            "worker",
            pin(&result, &self.ctx.blobs_dir),
            Some(arrival_metadata),
        );
        self.append(
            MovementKind::InterfaceOutbound,
            &self.door,
            self.ctx.interface.as_str(),
            pin(&result, &self.ctx.blobs_dir),
            Some(result_metadata),
        );
    }
    async fn install(
        &self,
        mut ws: WebSocketStream<tokio::net::UnixStream>,
        call: Value,
        file: InterfaceArtifactFile,
    ) {
        let call_seq = self.write(&call, &file);
        self.write_result(&call);
        let _ = ws
            .send(Message::Text(
                serde_json::to_string(&DeliveryVerdict::Admitted { seq: call_seq })
                    .unwrap()
                    .into(),
            ))
            .await;
    }
    async fn admit(&mut self) -> ArtifactRef {
        let output = self.arrivals.recv().await.unwrap();
        let identity = pin(&output.artifact, &self.ctx.blobs_dir);
        let bytes = std::fs::read(&self.run_path).unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        let checkpoint =
            &value["interface_state"][self.ctx.interface.as_str()]["pending_handoff"]["items"][0]["checkpoint"];
        assert_eq!(
            value["interface_state"][self.ctx.interface.as_str()]["pending_handoff"]["items"][0]["artifact_id"],
            json!(identity.id)
        );
        let mut admitted = metadata(checkpoint, &bytes, self.ctx.interface.as_str());
        if let Some(retained) = &output.retained {
            let scope = std::fs::read_to_string(&self.ctx.movements_path).unwrap().lines()
                .map(|line| serde_json::from_str::<Movement>(line).unwrap())
                .find(|row| matches!(&row.invocation_metadata,
                    Some(octopus_ledger::MovementMetadata::ArrivalFanout(scope))
                        if scope.arrival_fanout.identity == retained.id));
            let scope_seq = match scope {
                Some(scope) => { assert_eq!(scope.artifact.as_ref(), Some(&identity)); scope.seq.0 }
                None => {
                    let seq = std::fs::read_to_string(&self.ctx.movements_path).unwrap().lines().count() as u64;
                    let scope = Movement {
                        seq: Seq(seq), lamport: Lamport(seq + 1), recorded_at_unix_ms: seq + 1,
                        trace: TraceId("trace:run".into()), kind: MovementKind::ArtifactMoved,
                        from: Endpoint::Interface(self.ctx.interface.clone()),
                        to: Endpoint::Interface(self.ctx.interface.clone()), artifact: Some(identity.clone()),
                        invocation_metadata: Some(octopus_ledger::MovementMetadata::ArrivalFanout(octopus_ledger::ArrivalFanoutMetadata {
                            arrival_fanout: octopus_ledger::ArrivalFanoutScope {
                                identity: retained.id.clone(), targets: vec![admitted.runtime.clone().unwrap()],
                            },
                        })),
                    };
                    let mut file = std::fs::OpenOptions::new().append(true).open(&self.ctx.movements_path).unwrap();
                    writeln!(file, "{}", serde_json::to_string(&scope).unwrap()).unwrap(); file.sync_all().unwrap();
                    seq
                }
            };
            admitted.delivery_scope_seq = Some(scope_seq);
        }
        let seq = self.append(
            MovementKind::InterfaceArrival,
            self.ctx.interface.as_str(),
            "worker",
            identity.clone(),
            Some(admitted),
        );
        let verdict = DeliveryVerdict::Admitted { seq };
        if let Some(retained) = output.retained {
            retained.reply.send(Ok(octopus_ledger::ArtifactDelivery::Complete { verdict })).unwrap();
        } else {
            output.verdict_tx.unwrap().send(verdict).unwrap();
        }
        identity
    }
    fn store(&self) -> Store {
        let mut store = Store::open(&self.ctx.var_dir, 1_048_576).unwrap();
        advance_source(&mut store, "run", 0).unwrap();
        store.state.calls.insert(
            "call".into(),
            Call {
                correlation_id: "call".into(),
                offer_digest: "offer".into(),
                call_id: Some("rtc_call".into()),
                source_seq: 0,
                delegation_interface: "home".into(),
                status: "attached".into(),
                auth_owner: None,
                answer: None,
                transcript: vec![],
                provider_events: BTreeSet::new(),
                delegations: BTreeSet::new(),
                updates: BTreeMap::new(),
                stops: BTreeMap::new(),
                client_context: None,
                session_configuration: None,
            },
        );
        store
    }
}

#[path = "realtime_conflict.rs"]
mod conflict;
fn queued(store: &mut Store) {
    queue(store,"call","realtime.delegation",json!({"delegation_id":format!("d{}",store.state.next_emission_seq),"input_text":"same words"}),true,true).unwrap();
    store.save().unwrap();
}

#[tokio::test]
async fn bound_instruction_text_survives_reference_changes_and_cold_recovery() {
    for (label, payload, valid) in [
        ("instruction.text", b"Previously referenced instructions".as_slice(), true),
        ("instruction.text", br#"{"text":"Still instruction text, not conversation history"}"#.as_slice(), true),
        ("instruction.text", b" ".as_slice(), false),
        ("instruction.text", b"\xff".as_slice(), false),
        ("realtime.session.context", b"Invalid session snapshot".as_slice(), false),
    ] {
        let mut fixture = Fixture::new();
        let file = InterfaceArtifactFile::write(fixture.ctx.var_dir.join("instructions"), label, payload).unwrap();
        let reference = pin(&file, &fixture.ctx.blobs_dir);
        let mut record: Value = serde_json::from_slice(&fixture.pinned).unwrap();
        record["checkpoints"][0]["artifact_bindings"] = json!([{"id":reference.id}]);
        fixture.pinned = serde_json::to_vec_pretty(&record).unwrap();
        std::fs::write(&fixture.run_path, &fixture.pinned).unwrap();
        std::fs::write(fixture.ctx.blobs_dir.join(ContentHash::of_bytes(&fixture.pinned).to_hex()), &fixture.pinned).unwrap();
        let mut admitted = metadata(&record["checkpoints"][0], &fixture.pinned, "voice")
            .with_artifact_binding_refs(vec![reference.clone()]);
        admitted.grant.artifact_access.insert(reference.id.to_string(), octopus_policy::ArtifactAccess::read_only());
        fixture.input.metadata = Some(admitted);
        fixture.input.artifact_binding_payloads = vec![octopus_interfaces::ArtifactBindingPayload::new(reference.clone(), payload.to_vec())];
        std::fs::write(&fixture.ctx.movements_path, b"").unwrap();
        fixture.append(MovementKind::InterfaceOutbound, "control", "voice", fixture.input.artifact.clone(), fixture.input.metadata.clone());
        let session = ProviderHandoffSession::new(&fixture.ctx).unwrap();
        let warm = session.from_invocation(&fixture.ctx, &fixture.descriptor, &RunId::new("run"), &fixture.input, 0, &fixture.pinned);
        assert_eq!(warm.is_ok(), valid, "{label}: {:?}", warm.err().map(|e| e.to_string()));
        let cold = crate::life::recover_handoffs(&fixture.ctx, &fixture.descriptor,
            &ProviderHandoffSession::new(&fixture.ctx).unwrap(), BTreeSet::from([0])).await;
        assert_eq!(cold.is_ok(), valid, "cold {label}");
        if let Ok(handoffs) = cold {
            assert!(handoffs[&0].current_session_context_artifacts().await.iter()
                .all(|entry| entry.source_artifact != reference));
        }
    }
}

#[tokio::test]
async fn actual_handoff_recovers_persisted_second_observation_before_installation() {
    for ok in [false, true] {
    tokio::time::timeout(Duration::from_secs(10),async {
        let mut fixture=Fixture::new();
        let mut store=fixture.store(); let handoffs=BTreeMap::from([(0,fixture.handoff())]);
        let root=fixture.ctx.var_dir.clone(); let provider=fixture.ctx.interface.clone();
        if ok {
            queue(&mut store,"call","realtime.session.answer",json!({"ok":true,"answer_sdp":"v=0\r\n"}),false,false).unwrap();
        } else {
            queue_refusal(&mut store,0,"realtime.session.answer",Some(&json!({"correlation_id":"call"})),"HTTP 400").unwrap();
        }
        let deliver=async { let (ws,call,file)=fixture.accept_write().await; fixture.install(ws,call,file).await; Ok::<_,String>(fixture.admit().await) };
        let (_,first)=tokio::try_join!(flush(&root,&provider,&mut store,&handoffs),deliver).unwrap();
        let first_record=std::fs::read(&fixture.run_path).unwrap();
        queued(&mut store);
        let (ws,persisted,_file)={
            let attempt=flush(&root,&provider,&mut store,&handoffs); tokio::pin!(attempt);
            tokio::select! { receipt=fixture.accept_write()=>receipt, result=&mut attempt=>panic!("second handoff finished before seam: {result:?}") }
        };
        drop(ws); drop(handoffs); drop(store);
        assert_eq!(std::fs::read(&fixture.run_path).unwrap(),first_record);
        let mut restored=Store::open(&root,1_048_576).unwrap(); assert_eq!(restored.state.pending.len(),1);
        let handoffs=crate::life::recover_handoffs(&fixture.ctx,&fixture.descriptor,&ProviderHandoffSession::new(&fixture.ctx).unwrap(),BTreeSet::from([0])).await.unwrap();
        let deliver=async { let (ws,call,file)=fixture.accept_write().await; assert_eq!(call,persisted); fixture.install(ws,call,file).await; Ok::<_,String>(fixture.admit().await) };
        let (_,second)=tokio::try_join!(flush(&root,&provider,&mut restored,&handoffs),deliver).unwrap();
        assert_ne!(first,second); assert!(restored.state.pending.is_empty());
        flush(&root,&provider,&mut restored,&handoffs).await.unwrap();
        let rows=std::fs::read_to_string(&fixture.ctx.movements_path).unwrap().lines().map(|line|serde_json::from_str::<Movement>(line).unwrap()).collect::<Vec<_>>();
        for identity in [first,second] { assert_eq!(rows.iter().filter(|row|row.kind==MovementKind::InterfaceArrival && row.artifact.as_ref()==Some(&identity)).count(),1); }
        let record:Value=serde_json::from_slice(&std::fs::read(&fixture.run_path).unwrap()).unwrap();
        assert_eq!(record["interface_state"]["voice"]["authoring_generation"],2);
        assert_eq!(record["interface_state"]["voice"]["pending_handoff"]["terminal"],false);
        assert!(fixture.arrivals.try_recv().is_err());
        assert!(fixture.root.path().exists());
        let previous_bytes=std::fs::read(&fixture.run_path).unwrap();
        let previous:Value=serde_json::from_slice(&previous_bytes).unwrap();
        let input_checkpoint=previous["checkpoints"].as_array().unwrap().iter().find(|c|c["interface_binding"]["interface"]=="control").unwrap();
        assert_ne!(input_checkpoint["id"], "input", "nonterminal answer must rearm Stable control");
        assert!(previous["active_checkpoints"].as_array().unwrap().contains(&input_checkpoint["id"]));
        let peer=previous["checkpoints"].as_array().unwrap().iter().find(|c|c["id"]=="peer_input").unwrap().clone();
        for _ in 0..=octopus_ledger_index::AUTHORITATIVE_LEDGER_SCAN_MOVEMENT_BUDGET {
            fixture.append(MovementKind::InterfaceOutbound,"foreign","other",fixture.input.artifact.clone(),None);
        }
        let next_file=InterfaceArtifactFile::write(root.join("input"),"realtime.session.update",br#"{"text":"task result"}"#).unwrap();
        let next_input=InterfaceInvocationEnvelope{artifact:pin(&next_file,&fixture.ctx.blobs_dir),artifact_file:next_file,artifact_binding_payloads:vec![],metadata:Some(metadata(input_checkpoint,&previous_bytes,"voice"))};
        let next_seq=fixture.append(MovementKind::InterfaceOutbound,"control","voice",next_input.artifact.clone(),next_input.metadata.clone());
        let log=octopus_ledger_store::DurableLog::open(&fixture.ctx.movements_path).unwrap();
        let _index=octopus_ledger_index::LedgerSqliteIndex::open_healthy(&fixture.ctx.ledger_index_path,&log).unwrap();
        let mut reader=octopus_world_pyecho_life_runtime::LedgerTailReader::open(&fixture.ctx).unwrap();
        let first_batch=reader.read_new().unwrap(); assert_eq!(first_batch.len(),1); assert_eq!(first_batch[0].movement.seq.0,0);
        reader.commit_through(&first_batch[0]).unwrap();
        let cursor=std::fs::read(root.join("ledger-cursor.json")).unwrap();
        advance_source(&mut restored,"run",next_seq).unwrap();
        assert!(restored.state.pending[0].terminal);
        assert_eq!(restored.state.calls["call"].source_seq,next_seq);
        let deliver=async { let (ws,call,file)=fixture.accept_write().await; fixture.install(ws,call,file).await; Ok::<_,String>(fixture.admit().await) };
        tokio::try_join!(flush(&root,&provider,&mut restored,&handoffs),deliver).unwrap();
        let handoffs=crate::life::recover_handoffs(&fixture.ctx,&fixture.descriptor,&ProviderHandoffSession::new(&fixture.ctx).unwrap(),BTreeSet::from([next_seq])).await.unwrap();
        assert_eq!(std::fs::read(root.join("ledger-cursor.json")).unwrap(),cursor);
        queued(&mut restored);
        let (ws,persisted,_file)={
            let attempt=flush(&root,&provider,&mut restored,&handoffs); tokio::pin!(attempt);
            tokio::select! { receipt=fixture.accept_write()=>receipt, result=&mut attempt=>panic!("next source finished before seam: {result:?}") }
        };
        drop(ws); drop(handoffs); drop(restored);
        let mut restored=Store::open(&root,1_048_576).unwrap();
        let handoffs=crate::life::recover_handoffs(&fixture.ctx,&fixture.descriptor,&ProviderHandoffSession::new(&fixture.ctx).unwrap(),BTreeSet::from([next_seq])).await.unwrap();
        let deliver=async { let (ws,call,file)=fixture.accept_write().await; assert_eq!(call,persisted); fixture.install(ws,call,file).await; Ok::<_,String>(fixture.admit().await) };
        tokio::try_join!(flush(&root,&provider,&mut restored,&handoffs),deliver).unwrap();
        assert!(restored.state.pending.is_empty());
        let record:Value=serde_json::from_slice(&std::fs::read(&fixture.run_path).unwrap()).unwrap();
        assert_eq!(record["interface_state"]["voice"]["authoring_generation"],4);
        assert_eq!(record["interface_state"]["voice"]["pending_handoff"]["source_movement_seq"],next_seq);
        assert_eq!(record["interface_state"]["voice"]["pending_handoff"]["terminal"],false);
        assert!(record["checkpoints"].as_array().unwrap().contains(&peer));
        assert!(!root.join("ledger-cursor.call-origins.json").exists());
        assert!(crate::life::recover_handoffs(&fixture.ctx,&fixture.descriptor,&ProviderHandoffSession::new(&fixture.ctx).unwrap(),BTreeSet::from([u64::MAX])).await.err().unwrap().contains("missing checked source movements"));
    }).await.expect("real handoff recovery exceeded deadline");
    }
}

#[tokio::test]
async fn actual_handoff_reconciles_installed_write_and_admission_before_local_save() {
    for admitted in [false, true] {
        tokio::time::timeout(Duration::from_secs(10),async {
            let mut fixture=Fixture::new(); let mut store=fixture.store();
            let root=fixture.ctx.var_dir.clone(); let provider=fixture.ctx.interface.clone();
            let handoffs=BTreeMap::from([(0,fixture.handoff())]);
            queued(&mut store);
            let deliver=async { let (ws,call,file)=fixture.accept_write().await; fixture.install(ws,call,file).await; Ok::<_,String>(fixture.admit().await) };
            let (_,first)=tokio::try_join!(flush(&root,&provider,&mut store,&handoffs),deliver).unwrap();
            queued(&mut store);
            let persisted={
                let attempt=flush(&root,&provider,&mut store,&handoffs); tokio::pin!(attempt);
                let (ws,call,file)=tokio::select! { receipt=fixture.accept_write()=>receipt, result=&mut attempt=>panic!("handoff ended before write: {result:?}") };
                if admitted {
                    fixture.install(ws,call.clone(),file).await;
                    tokio::select! { biased; _=fixture.admit()=>{}, result=&mut attempt=>panic!("handoff ended before local-save seam: {result:?}") }
                } else {
                    fixture.write(&call,&file); drop(ws);
                }
                call
            };
            let second=store.state.pending[0].delivery.as_ref().unwrap().artifact.clone();
            drop(handoffs); drop(store);
            if !admitted {
                let session=ProviderHandoffSession::new(&fixture.ctx).unwrap();
                assert!(session.from_invocation(&fixture.ctx,&fixture.descriptor,&RunId::new("run"),&fixture.input,0,&fixture.pinned).is_err(),"installed bytes alone are not file-write evidence");
                fixture.write_result(&persisted);
            }
            let handoffs=BTreeMap::from([(0,fixture.handoff())]);
            let mut restored=Store::open(&root,1_048_576).unwrap();
            if admitted {
                flush(&root,&provider,&mut restored,&handoffs).await.unwrap();
            } else {
                tokio::try_join!(flush(&root,&provider,&mut restored,&handoffs),async {fixture.admit().await;Ok::<_,String>(())}).unwrap();
            }
            assert!(restored.state.pending.is_empty());
            let rows=std::fs::read_to_string(&fixture.ctx.movements_path).unwrap().lines().map(|line|serde_json::from_str::<Movement>(line).unwrap()).collect::<Vec<_>>();
            for identity in [first,second] { assert_eq!(rows.iter().filter(|row|row.kind==MovementKind::InterfaceArrival && row.artifact.as_ref()==Some(&identity)).count(),1); }
            assert_eq!(rows.iter().filter(|row|row.kind==MovementKind::InterfaceArrival && row.from==Endpoint::Interface(InterfaceId::new("source"))).count(),2);
            let record:Value=serde_json::from_slice(&std::fs::read(&fixture.run_path).unwrap()).unwrap();
            assert_eq!(record["interface_state"]["voice"]["authoring_generation"],2);
            assert!(fixture.arrivals.try_recv().is_err());
            assert!(tokio::time::timeout(Duration::from_millis(20),fixture.listener.accept()).await.is_err());
        }).await.expect("real handoff recovery exceeded deadline");
    }
}
