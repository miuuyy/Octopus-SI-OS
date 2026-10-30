use super::*;
include!("rejected_final.rs");

#[test]
#[ignore = "requires explicitly selected read-only completed-source installation evidence"]
fn selected_run_replays_completed_source_after_outgoing_withdrawal() {
    let home = PathBuf::from(std::env::var("OCTOPUS_REPLAY_HOME").unwrap());
    let provider = InterfaceId::new(std::env::var("OCTOPUS_REPLAY_PROVIDER").unwrap());
    let run = RunId::new(std::env::var("OCTOPUS_REPLAY_RUN").unwrap());
    let source: u64 = std::env::var("OCTOPUS_REPLAY_SOURCE").unwrap().parse().unwrap();
    let dir = home.join("registry/interface").join(provider.as_str());
    let descriptor: InterfaceDescriptor = serde_json::from_slice(&std::fs::read(dir.join("interface.json")).unwrap()).unwrap();
    let life: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join("life.json")).unwrap()).unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let (arrivals, receiver) = tokio::sync::mpsc::channel(1); drop(receiver);
    let ctx = InterfaceLiftContext::unprivileged(provider.clone(), dir, Some(descriptor.clone()), scratch.path().join("provider"),
        home.join("ledger/movements.jsonl"), home.join("ledger/blobs"), home.join("ledger/ledger-index.sqlite"),
        BTreeMap::from([("run_records_root".into(), home.join("state/runs")), ("interface_hand_in_root".into(), home.join("interface-hand-in"))]),
        arrivals, Default::default()).with_host_io_limits(serde_json::from_value(life["processes"][0]["io_limits"].clone()).unwrap()).unwrap();
    let path = home.join("state/runs").join(format!("{run}.json"));
    let current = std::fs::read(&path).unwrap();
    for directory in ["emitted", "handoff-delivery"] {
        let target = ctx.interface_var_dir().join(directory);
        std::fs::create_dir_all(&target).unwrap();
        for entry in std::fs::read_dir(home.join("interfaces").join(provider.as_str()).join(directory)).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_file() && (directory != "emitted" || entry.file_name().to_string_lossy().ends_with(".artifact.json")) {
                std::fs::copy(entry.path(), target.join(entry.file_name())).unwrap();
            }
        }
    }
    let mut replay = octopus_world_pyecho_life_runtime::LedgerReplayReader::open(&ctx).unwrap();
    let mut input = None;
    while let Some(batch) = replay.read_batch(256).unwrap() {
        if let Some(row) = batch.into_iter().find(|row| row.movement.seq.0 == source) {
            input = Some(replay.envelope(&row).unwrap()); break;
        }
    }
    let input = input.unwrap(); let pinned = ctx.pinned_run_record_bytes(&input).unwrap();
    for bytes in [&pinned, &current] {
        let record = RunRecord::from_bytes(bytes).unwrap();
        if let Some(pending) = persisted_pending_handoff(&record, &provider).unwrap() {
            for item in pending.with_standing_items().items {
                let target = ctx.interface_var_dir().join(&item.manifest_relative_path);
                std::fs::create_dir_all(target.parent().unwrap()).unwrap();
                std::fs::copy(home.join("interfaces").join(provider.as_str()).join(&item.manifest_relative_path), target).unwrap();
            }
        }
    }
    let handoff = ProviderHandoffSession::new(&ctx).unwrap().from_invocation(&ctx, &descriptor, &run, &input, source, &pinned).unwrap();
    let record = RunRecord::from_bytes(&current).unwrap();
    if let Some(pending) = persisted_pending_handoff(&record, &provider).unwrap() {
        let mut state = handoff.state.try_lock().unwrap().clone();
        handoff.restore_pending_context(&mut state, &record, &pending).unwrap();
    }
    assert_eq!(std::fs::read(path).unwrap(), current);
}

#[tokio::test]
async fn authoring_retains_exact_request_while_registry_changes() {
    let mut fixture = CompletedInputReplayFixture::new();
    let control = tempfile::tempdir_in("/tmp").unwrap();
    let handoff = &mut fixture.handoff;
    handoff.ctx.root_slots.insert("app_server_control_root".into(), control.path().into());
    let socket = octopus_app_server_interface::interface_control_socket_path_from_root(
        control.path(), &handoff.config.authoring_source_interface);
    std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
    let manifest = std::fs::read(fixture.input_file.manifest_path()).unwrap();
    let original_run = std::fs::read(&handoff.run_path).unwrap();
    let late = async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        assert_eq!(socket.next().await.unwrap().unwrap().into_text().unwrap().as_bytes(), manifest);
        socket.send(WsMessage::Text(serde_json::to_string(&DeliveryVerdict::Admitted { seq: 41 }).unwrap().into())).await.unwrap();
        listener
    };
    let (result, listener) = tokio::join!(handoff.send_authoring_manifest(&manifest), late);
    assert_eq!(result.unwrap(), DeliveryVerdict::Admitted { seq: 41 });
    for terminal in [DeliveryVerdict::Admitted { seq: 42 }, DeliveryVerdict::refused(
        octopus_ledger::RefusedReason::Policy, serde_json::json!({"message":"revoked"}))] {
        let serve = async {
            for verdict in [DeliveryVerdict::refused(octopus_ledger::RefusedReason::RegistryUnstable,
                serde_json::json!({"message":"registry changed before admission"})), terminal.clone()] {
                let (stream, _) = listener.accept().await.unwrap();
                let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
                assert_eq!(socket.next().await.unwrap().unwrap().into_text().unwrap().as_bytes(), manifest);
                assert_eq!(std::fs::read(&handoff.run_path).unwrap(), original_run);
                socket.send(WsMessage::Text(serde_json::to_string(&verdict).unwrap().into())).await.unwrap();
            }
        };
        let (result, ()) = tokio::join!(handoff.send_authoring_manifest(&manifest), serve);
        assert_eq!(result.unwrap(), terminal);
    }
    handoff.ctx.cancel.cancel();
    assert!(handoff.send_authoring_manifest(&manifest).await.unwrap_err().contains("cancelled"));
    assert!(tokio::time::timeout(Duration::from_millis(20), listener.accept()).await.is_err());
}

#[tokio::test]
async fn retained_pending_retries_exact_admission_with_declared_timeout_and_cancellation() {
    use serde_json::json;
    use octopus_ledger::{ArrivalFanoutMetadata, ArrivalFanoutScope, ArtifactDelivery, DeliveryMetadata, MovementMetadata, RefusedReason};
    for mode in ["admitted", "timeout", "cancelled", "transport", "wrong_scope", "incomplete", "policy"] {
        let mut fixture = CompletedInputReplayFixture::new();
        fixture.handoff.config.result_timeout_ms = if mode == "timeout" { 120 } else { 3000 };
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        fixture.handoff.ctx.arrivals = tx;
        let authored = fixture.output(false, false, false);
        let pending = authored.pending_handoff.clone();
        let item = &pending.items[0];
        let (file, reference) = load_pending_artifact(&fixture.handoff.var_dir, item).unwrap();
        let original_payload = file.read_payload_bytes().unwrap();
        let handoff = fixture.handoff.clone();
        let _ownership = handoff.dispatch_ownership().await.unwrap();
        let (done_tx, mut done_rx) = tokio::sync::oneshot::channel();
        let sending = async {
            let result = handoff.hand_retained_pending(&pending, item, file.clone()).await;
            let _ = done_tx.send(());
            result
        };
        let receiving = async {
            let mut attempts = 0;
            let mut retained_id = None;
            let mut original_scope = None;
            let mut edge_template = None;
            loop {
                let arrival = tokio::select! {
                    biased;
                    _ = &mut done_rx => break,
                    arrival = rx.recv() => arrival.unwrap(),
                };
                attempts += 1;
                assert_eq!(arrival.artifact.read_payload_bytes().unwrap(), original_payload);
                assert_eq!(ContentHash::of_bytes(&arrival.artifact.read_manifest_bounded(1 << 20).unwrap().manifest_bytes), reference.content);
                let retained = arrival.retained.unwrap();
                if let Some(id) = &retained_id { assert_eq!(&retained.id, id); }
                else { retained_id = Some(retained.id.clone()); }
                if attempts == 1 {
                    retained.reply.send(Ok(ArtifactDelivery::Complete {
                        verdict: DeliveryVerdict::refused(RefusedReason::RegistryUnstable, json!({"attempts":4})),
                    })).unwrap();
                    continue;
                }
                let mut edge = edge_template.get_or_insert_with(|| {
                    fixture.edge(authored.content.as_bytes(), &item.checkpoint.id, &arrival.artifact, "worker".into(), MovementKind::InterfaceArrival);
                    fixture.rows.pop().unwrap()
                }).clone();
                let runtime = edge.invocation_metadata.as_ref().unwrap().invocation().unwrap().runtime.clone();
                let scope_seq = *original_scope.get_or_insert_with(|| {
                    let mut scope = edge.clone();
                    scope.kind = MovementKind::ArtifactMoved;
                    scope.to = scope.from.clone();
                    scope.invocation_metadata = Some(MovementMetadata::ArrivalFanout(ArrivalFanoutMetadata {
                        arrival_fanout: ArrivalFanoutScope { identity: retained.id.clone(), targets: vec![runtime.clone().unwrap()] },
                    }));
                    let seq = scope.seq.0;
                    fixture.rows.push(scope);
                    seq
                });
                edge.seq.0 = fixture.rows.len() as u64; edge.lamport.0 = edge.seq.0;
                if attempts == 2 {
                    edge.kind = MovementKind::DeliveryRefused;
                    edge.invocation_metadata = Some(MovementMetadata::Delivery(DeliveryMetadata {
                        delivery_verdict: DeliveryVerdict::refused(RefusedReason::RegistryUnstable, json!({"message":"admission registry snapshot changed"})),
                        runtime, source_movement_seq: None, delivery_scope_seq: Some(scope_seq),
                    }));
                    fixture.rows.push(edge); fixture.save();
                    retained.reply.send(Ok(ArtifactDelivery::Pending { scope_seq })).unwrap();
                    std::fs::write(file.manifest_path(), b"carrier changed after first dispatch").unwrap();
                    continue;
                }
                let delivery = match mode {
                    "admitted" => {
                        let mut metadata = edge.invocation_metadata.as_ref().unwrap().invocation().unwrap().clone();
                        metadata.delivery_scope_seq = Some(scope_seq); edge.invocation_metadata = Some(metadata.into());
                        let seq = edge.seq.0; fixture.rows.push(edge.clone());
                        for requirement in &item.checkpoint.required_verified_artifacts {
                            let mark = octopus_interfaces::Mark::passed(requirement.mark.clone(), item.checkpoint.id.clone(), handoff.config.run.clone(),
                                item.checkpoint.epoch, reference.clone(), requirement.verifier.clone(), octopus_ledger::VerifierPackagePin {
                                    descriptor_content: ContentHash::of_bytes(b"descriptor"), script_content: ContentHash::of_bytes(b"script"),
                                }, json!({}));
                            edge.seq.0 = fixture.rows.len() as u64; edge.lamport.0 = edge.seq.0;
                            edge.kind = MovementKind::MarkOutcomeAdmitted(requirement.mark.clone());
                            edge.invocation_metadata = Some(mark.into()); fixture.rows.push(edge.clone());
                        }
                        fixture.save();
                        Ok(ArtifactDelivery::Complete { verdict: DeliveryVerdict::Admitted { seq } })
                    }
                    "policy" => {
                        let verdict = DeliveryVerdict::refused(RefusedReason::Policy, json!({"message":"revoked permission"}));
                        edge.kind = MovementKind::DeliveryRefused;
                        edge.invocation_metadata = Some(MovementMetadata::Delivery(DeliveryMetadata {
                            delivery_verdict: verdict.clone(), runtime, source_movement_seq: None, delivery_scope_seq: Some(scope_seq),
                        }));
                        fixture.rows.push(edge); fixture.save();
                        Ok(ArtifactDelivery::Complete { verdict })
                    }
                    "transport" => Err("fixture transport closed".into()),
                    "wrong_scope" => Ok(ArtifactDelivery::Pending { scope_seq: scope_seq + 1 }),
                    "incomplete" => Ok(ArtifactDelivery::Complete { verdict: DeliveryVerdict::Admitted { seq: edge.seq.0 } }),
                    _ => Ok(ArtifactDelivery::Pending { scope_seq }),
                };
                retained.reply.send(delivery).unwrap();
                if mode == "cancelled" { handoff.ctx.cancel.cancel(); }
            }
            attempts
        };
        let (result, attempts) = tokio::time::timeout(Duration::from_secs(5), async { tokio::join!(sending, receiving) }).await.unwrap();
        assert!(attempts >= 2, "{mode}: {result:?}");
        match mode {
            "admitted" => assert!(matches!(result.unwrap(), DeliveryVerdict::Admitted { .. })),
            "policy" => assert!(matches!(result.unwrap(), DeliveryVerdict::Refused { reason: RefusedReason::Policy, .. })),
            _ => assert!(result.unwrap_err().contains(match mode {
                "timeout" => "timed out after 120ms", "cancelled" => "cancelled",
                "transport" => "fixture transport closed", "wrong_scope" => "changed its original scope",
                "incomplete" => "without all original edge decisions", _ => unreachable!(),
            }), "{mode}"),
        }
        assert_eq!(fixture.rows.iter().filter(|row| row.kind == MovementKind::ArtifactMoved && row.artifact.as_ref() == Some(&reference)).count(), 1);
        assert_eq!(fixture.rows.iter().filter(|row| row.kind == MovementKind::InterfaceArrival && row.artifact.as_ref() == Some(&reference)).count(), usize::from(mode == "admitted"));
        assert_eq!(std::fs::read(&handoff.run_path).unwrap(), authored.content.as_bytes());
    }
}

#[tokio::test]
async fn pending_output_retains_original_fanout_across_restart_and_refuses_legacy_ambiguity() {
    use octopus_ledger::{ArrivalFanoutMetadata, ArrivalFanoutScope, ArtifactDelivery, DeliveryMetadata, MovementMetadata};
    let mut fixture = CompletedInputReplayFixture::new();
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    fixture.handoff.ctx.arrivals = tx;
    let before = std::fs::read(&fixture.handoff.run_path).unwrap();
    let file = InterfaceArtifactFile::write(fixture.handoff.var_dir.join("outputs"), "answer", br#"{"text":"saved output"}"#).unwrap();
    let identity = artifact_identity(&file, &fixture.handoff.var_dir).unwrap();
    let authored = fixture.handoff.author_run(&before, &[], &[PreparedHandoff {
        identity:identity.clone(), output_checkpoint:fixture.handoff.config.checkpoint_for_role(RunAuthoringCheckpointRole::ProviderArtifactOutput).unwrap().clone(),
        verified_inputs:Vec::new(),
    }], false, None, false, true).unwrap();
    fixture.install(&authored);
    let pending = authored.pending_handoff.clone();
    let item = pending.items[0].clone();
    let record = RunRecord::from_bytes(authored.content.as_bytes()).unwrap();
    let target = octopus_ledger::RuntimeInvocationMetadata {
        run:record.id.clone(), ee:record.ee.clone(), worker:record.worker.clone(), checkpoint:item.checkpoint.id.clone(),
        interface:fixture.handoff.ctx.interface.clone(), epoch:item.checkpoint.epoch, run_record_hash:Some(authored.after_hash.clone()),
    };
    let mut sibling = target.clone(); sibling.run = "sibling-run".into(); sibling.worker = "peer".into(); sibling.checkpoint = "sibling".into();
    let scope_seq = fixture.rows.len() as u64;
    let own_seq = scope_seq + 1;
    let handoff = fixture.handoff.clone();
    let sending = handoff.hand_pending_items(&pending, authored.content.as_bytes());
    let receive = async {
        let arrival = rx.recv().await.unwrap();
        assert_eq!(artifact_identity(&arrival.artifact, arrival.artifact.manifest_path().parent().unwrap()).unwrap().artifact, identity.artifact);
        let retained = arrival.retained.unwrap();
        let delivery_id = retained.id.clone();
        let mut scope = fixture.rows[0].clone(); scope.seq.0 = scope_seq; scope.lamport.0 = scope_seq;
        scope.kind = MovementKind::ArtifactMoved; scope.from = Endpoint::Interface("provider".into()); scope.to = scope.from.clone();
        scope.artifact = Some(identity.artifact.clone());
        scope.invocation_metadata = Some(MovementMetadata::ArrivalFanout(ArrivalFanoutMetadata {
            arrival_fanout:ArrivalFanoutScope {identity:delivery_id.clone(), targets:vec![target.clone(), sibling.clone()]},
        }));
        fixture.rows.push(scope);
        fixture.edge(authored.content.as_bytes(), &item.checkpoint.id, &file, "worker".into(), MovementKind::InterfaceArrival);
        let own = fixture.rows.last_mut().unwrap();
        let mut metadata = own.invocation_metadata.as_ref().unwrap().invocation().unwrap().clone();
        metadata.delivery_scope_seq = Some(scope_seq); own.invocation_metadata = Some(metadata.into()); fixture.save();
        retained.reply.send(Err("fixture transport closed after retaining scope".into())).unwrap();
        delivery_id
    };
    let (result, delivery_id) = tokio::join!(sending, receive);
    assert!(result.unwrap_err().contains("fixture transport closed"));
    assert_eq!(handoff.pending_handoff_evidence(&pending).unwrap(), vec![None]);
    assert_eq!(std::fs::read(&handoff.run_path).unwrap(), authored.content.as_bytes());
    let mut cold = handoff.clone(); cold.session = ProviderHandoffSession::new(&cold.ctx).unwrap();
    let retry = cold.hand_pending_items(&pending, authored.content.as_bytes());
    let complete = async {
        let arrival = rx.recv().await.unwrap(); let retained = arrival.retained.unwrap();
        assert_eq!(retained.id, delivery_id);
        assert_eq!(artifact_identity(&arrival.artifact, arrival.artifact.manifest_path().parent().unwrap()).unwrap().artifact, identity.artifact);
        let mut refused = fixture.rows[scope_seq as usize].clone(); refused.seq.0 = fixture.rows.len() as u64; refused.lamport.0 = refused.seq.0;
        refused.kind = MovementKind::DeliveryRefused; refused.to = Endpoint::Interface(sibling.worker.clone());
        refused.invocation_metadata = Some(MovementMetadata::Delivery(DeliveryMetadata {
            delivery_verdict:DeliveryVerdict::refused(octopus_ledger::RefusedReason::Policy, serde_json::json!({"message":"revoked peer permission"})),
            runtime:Some(sibling), source_movement_seq:None, delivery_scope_seq:Some(scope_seq),
        }));
        fixture.rows.push(refused); fixture.save();
        retained.reply.send(Ok(ArtifactDelivery::Complete {verdict:DeliveryVerdict::Admitted {seq:own_seq}})).unwrap();
    };
    let (result, ()) = tokio::join!(retry, complete);
    assert_eq!(result.unwrap(), vec![ProviderHandoffOutcome::Admitted {seq:own_seq, artifact:identity.artifact.clone()}]);
    assert_eq!(cold.pending_handoff_evidence(&pending).unwrap(), vec![Some(own_seq)]);
    assert_eq!(fixture.rows.iter().filter(|row| row.kind == MovementKind::InterfaceArrival && row.artifact.as_ref() == Some(&identity.artifact)).count(), 1);
    assert!(rx.try_recv().is_err());
    let receipt = std::fs::read_dir(cold.ctx.interface_var_dir().join("handoff-delivery")).unwrap().next().unwrap().unwrap().path();
    let saved = std::fs::read(&receipt).unwrap();
    let mut corrupt:serde_json::Value = serde_json::from_slice(&saved).unwrap(); corrupt["source_movement_seq"] = serde_json::json!(99);
    std::fs::write(&receipt, serde_json::to_vec(&corrupt).unwrap()).unwrap();
    assert!(cold.pending_handoff_evidence(&pending).unwrap_err().contains("exact pending contract"));
    std::fs::remove_file(&receipt).unwrap();
    assert!(cold.pending_handoff_evidence(&pending).unwrap_err().contains("saved dispatch identity"));

    let mut legacy = CompletedInputReplayFixture::new();
    let authored = legacy.output(false, false, false);
    let pending = authored.pending_handoff.clone();
    let ownership = legacy.handoff.dispatch_ownership().await.unwrap();
    legacy.handoff.record_pending_dispatch(&pending, &pending.items[0]).unwrap(); drop(ownership);
    assert!(legacy.handoff.hand_pending_items(&pending, authored.content.as_bytes()).await.unwrap_err().contains("dispatched without a retained identity"));
    assert!(!legacy.handoff.ctx.interface_var_dir().join("handoff-delivery").exists());
    assert_eq!(std::fs::read(&legacy.handoff.run_path).unwrap(), authored.content.as_bytes());
    let mut legacy_record:serde_json::Value = serde_json::from_str(&authored.content).unwrap();
    legacy_record["interface_state"]["provider"]["pending_handoff"].as_object_mut().unwrap().remove("dispatch_fenced");
    let bytes = serde_json::to_vec(&legacy_record).unwrap(); std::fs::write(&legacy.handoff.run_path, &bytes).unwrap();
    let record = RunRecord::from_bytes(&bytes).unwrap(); let pending = persisted_pending_handoff(&record, &"provider".into()).unwrap().unwrap();
    assert!(legacy.handoff.hand_pending_items(&pending, &bytes).await.unwrap_err().contains("durable unsubmitted proof"));
    assert!(!legacy.handoff.ctx.interface_var_dir().join("handoff-delivery").exists());
}

#[test]
fn context_publications_preserve_fanout_evidence_without_repeating_the_output() {
    use octopus_ledger::{ArrivalFanoutMetadata, ArrivalFanoutScope, DeliveryMetadata, MovementMetadata};
    for refused in [false, true] {
        let mut fixture = CompletedInputReplayFixture::new();
        let mut record = RunRecord::from_bytes(&std::fs::read(&fixture.handoff.run_path).unwrap()).unwrap();
        let mut sibling = record.checkpoint(&"client".into()).unwrap().clone(); sibling.id = "observed".into();
        if refused { sibling.required_verified_artifacts = record.checkpoint(&"tool".into()).unwrap().required_verified_artifacts.clone(); }
        record.active_checkpoints.push(sibling.id.clone()); record.checkpoints.push(sibling);
        let bytes = serde_json::to_vec(&record).unwrap();
        let file = InterfaceArtifactFile::write(fixture.handoff.var_dir.join("fanout"), "tool.call", br#"{"label":"tool.call"}"#).unwrap();
        let artifact = artifact_identity(&file, &fixture.handoff.var_dir).unwrap().artifact;
        let coordinates = ["client", "observed"].map(|checkpoint| octopus_ledger::RuntimeInvocationMetadata {
            run:record.id.clone(),ee:record.ee.clone(),worker:record.worker.clone(),interface:"provider".into(),
            checkpoint:checkpoint.into(),epoch:0,run_record_hash:Some(ContentHash::of_bytes(&bytes).to_hex()),
        });
        let scope = fixture.rows.len() as u64;
        let mut row = fixture.rows[0].clone(); row.seq.0=scope; row.lamport.0=scope; row.kind=MovementKind::ArtifactMoved;
        row.from=Endpoint::Interface("provider".into()); row.to=row.from.clone(); row.artifact=Some(artifact.clone());
        row.invocation_metadata=Some(MovementMetadata::ArrivalFanout(ArrivalFanoutMetadata { arrival_fanout:ArrivalFanoutScope {identity:"same-output".into(),targets:coordinates.to_vec()} }));
        fixture.rows.push(row);
        let mut arrivals = Vec::new();
        for coordinate in &coordinates {
            fixture.edge(&bytes, &coordinate.checkpoint, &file, "worker".into(), MovementKind::InterfaceArrival);
            let row = fixture.rows.last_mut().unwrap(); arrivals.push(row.seq.0);
            let mut metadata = row.invocation_metadata.as_ref().unwrap().invocation().unwrap().clone();
            metadata.delivery_scope_seq=Some(scope); row.invocation_metadata=Some(metadata.into());
        }
        if refused {
            let mut row=fixture.rows.last().unwrap().clone(); row.seq.0=fixture.rows.len() as u64;row.lamport.0=row.seq.0;row.kind=MovementKind::DeliveryRefused;
            row.invocation_metadata=Some(MovementMetadata::Delivery(DeliveryMetadata {runtime:Some(coordinates[1].clone()),source_movement_seq:None,delivery_scope_seq:Some(scope),
                delivery_verdict:DeliveryVerdict::refused(octopus_ledger::RefusedReason::VerificationFailed,serde_json::json!({"message":"other tool edge"}))}));
            fixture.rows.push(row);
        }
        fixture.save();
        let mut full = octopus_ledger_index::ArrivalDedupIndex::new();
        full.catch_up_movements(fixture.rows.clone()).unwrap();
        let mut selected = octopus_ledger_index::InterfaceArrivalProjection::new("provider".into());
        for row in &fixture.rows {
            if row.from == Endpoint::Interface("provider".into()) { selected.observe(row).unwrap(); }
        }
        assert_eq!(selected.retained_arrival_scope("same-output").unwrap(),
            full.retained_arrival_scope(&"provider".into(), "same-output").unwrap());
        assert_eq!(format!("{:?}", selected.retained_arrival_edges(scope).unwrap()),
            format!("{:?}", full.retained_arrival_edges(scope).unwrap()));
        let mut publications=ContextPublications::new();
        for seq in &arrivals { fixture.handoff.record_context_publication(&mut publications,&artifact,*seq).unwrap(); }
        assert_eq!(publications[&artifact_ref_key(&artifact)], arrivals.iter().copied().collect());
        assert!(fixture.handoff.record_context_publication(&mut publications,&artifact,0).is_err());
        assert!(!fixture.handoff.session.index().unwrap().retained_delivery.same_output_delivery(&"foreign".into(),&"worker".into(),&artifact,arrivals[0],arrivals[1]).unwrap());
        let entry=|seq| SessionContextArtifact {label:"tool.call".into(),payload:serde_json::json!({"label":"tool.call"}),source_artifact:artifact.clone(),source_movement_seq:Some(seq)};
        let existing=vec![entry(arrivals[0])];let mut recovered=vec![entry(arrivals[1])];
        assert!(reconcile_recovered_session_snapshot(&existing,&mut recovered,&publications,&artifact).unwrap());
        assert_eq!(session_artifacts_fingerprint(&existing).unwrap(),session_artifacts_fingerprint(&recovered).unwrap());
    }
}

#[test]
fn materialized_checkpoints_preserve_static_role_overlap_access() {
    let fixture = CompletedInputReplayFixture::new();
    let mut template = fixture.handoff.config.checkpoints[0].clone();
    template.static_artifact_bindings = vec![ArtifactId::new("role"), ArtifactId::new("declared-static")];
    for elevated in [false, true] {
        let mut role = ArtifactBinding { id: ArtifactId::new("role"), access: Default::default() };
        role.access.change = elevated; role.access.create = elevated; role.access.delete = elevated;
        let raw = serde_json::to_value(&role).unwrap();
        let value = materialize_checkpoint(&template, "context", &fixture.handoff.config.run, &[raw.clone()]);
        assert_eq!(value["artifact_bindings"], serde_json::json!([raw, {"id":"declared-static"}]));
        let checkpoint = serde_json::from_value(value).unwrap();
        assert!(checkpoint_role_bindings_match(&checkpoint, &[role], &template));
        let duplicates = materialize_checkpoint(&template, "context", &fixture.handoff.config.run, &[raw.clone(), raw]);
        assert_eq!(duplicates["artifact_bindings"].as_array().unwrap().len(), 3, "existing duplicate input must remain invalid, not be silently repaired");
    }
}

#[test]
fn failed_owner_input_static_revision_requires_checked_terminal_source() {
    for mode in ["valid", "ordinary_active", "missing_error", "wrong_error_source", "earlier_input", "later_input", "denied_read"] {
        let mut fixture = CompletedInputReplayFixture::new();
        let static_id = ArtifactId::new(format!("static:{}", ContentHash::of_bytes(b"declared context").to_hex()));
        let static_ids = vec![static_id.clone(), ArtifactId::new(format!("static:{}", ContentHash::of_bytes(b"second context").to_hex()))];
        let owner = CheckedRunWriter { owner:"room".into(), source:"room".into(), door:"owner-writer".into(),
            tool:"owner.write".into(), call_label:"tool.call".into(), result_label:"owner.write.result".into() };
        fixture.handoff.config.checkpoints[0].role = RunAuthoringCheckpointRole::OwnerInput;
        fixture.handoff.config.checkpoints[0].owner_authoring = Some(owner);
        fixture.handoff.config.checkpoints[1].static_artifact_bindings = static_ids.clone();
        fixture.handoff.bound_topology.checkpoints = fixture.handoff.config.checkpoints.clone();
        let mut value: serde_json::Value = serde_json::from_slice(&std::fs::read(&fixture.handoff.run_path).unwrap()).unwrap();
        value["interface_state"]["provider"][RUN_AUTHORING_BINDING] = serde_json::to_value(&fixture.handoff.bound_topology).unwrap();
        for row in value["checkpoints"].as_array_mut().unwrap().iter_mut().filter(|row| row["id"] == "input" || row["id"] == "alternate") {
            row["artifact_bindings"] = serde_json::json!([{"id":static_ids[0]}, {"id":static_ids[1]}]);
        }
        let initial = serde_json::to_vec(&value).unwrap();
        std::fs::write(&fixture.handoff.run_path, &initial).unwrap();
        fixture.rows.clear();
        let input_file = fixture.input_file.clone();
        fixture.edge(&initial, &"input".into(), &input_file, "provider".into(), MovementKind::InterfaceOutbound);
        if mode == "earlier_input" {
            fixture.edge(&initial, &"input".into(), &input_file, "provider".into(), MovementKind::InterfaceOutbound);
        }
        fixture.handoff.source_movement_seq = fixture.rows.len() as u64 - 1;
        for row in &mut fixture.rows {
            let mut metadata = row.invocation_metadata.as_ref().unwrap().invocation().unwrap().clone();
            if mode == "denied_read" {
                metadata.grant.artifact_access.clear();
            } else {
                for id in &static_ids { metadata.grant.artifact_access.insert(id.to_string(), octopus_policy::ArtifactAccess::read_only()); }
            }
            row.invocation_metadata = Some(metadata.into());
        }
        fixture.save();
        fixture.handoff.failed_source = mode != "ordinary_active";
        let source = fixture.handoff.source_movement_seq;
        let file = InterfaceArtifactFile::write(fixture.handoff.var_dir.join("error"), "turn.error",
            &serde_json::to_vec(&serde_json::json!({"label":"turn.error", "message":"admitted context invalid", "will_retry":false,
                "additional_details":{"source_movement_seq":source + u64::from(mode == "wrong_error_source")}})).unwrap()).unwrap();
        let authored = fixture.handoff.author_run_with_bindings(&initial, &static_ids, &[PreparedHandoff {
            identity:artifact_identity(&file, &fixture.handoff.var_dir).unwrap(),
            output_checkpoint:fixture.handoff.config.checkpoint_for_role(RunAuthoringCheckpointRole::ProviderArtifactOutput).unwrap().clone(),
            verified_inputs:Vec::new(),
        }], true, None, false, true, &[]).unwrap();
        fixture.install(&authored);
        if mode != "missing_error" {
            fixture.edge(authored.content.as_bytes(), &authored.pending_handoff.items[0].checkpoint.id,
                &file, "worker".into(), MovementKind::InterfaceArrival);
        }
        let before = std::fs::read(&fixture.handoff.run_path).unwrap();
        let record = RunRecord::from_bytes(&before).unwrap();
        if mode == "later_input" {
            let template = fixture.handoff.config.checkpoint_for_role(RunAuthoringCheckpointRole::Stable).unwrap();
            let cp = record.checkpoints.iter().find(|cp| record.active_checkpoints.contains(&cp.id)
                && checkpoint_record_matches_template(cp, template, &record.id)).unwrap().id.clone();
            fixture.edge(&before, &cp, &input_file, "provider".into(), MovementKind::InterfaceOutbound);
        }
        let writer = declared_provider_run_writer(&record, &"provider".into(), &RunAuthoringProfile {
            file_write_tool:"write".into(), file_write_label:"tool.call".into(), file_write_result_label:"write.result".into(), result_timeout_ms:1000,
        }).unwrap();
        let mut desired = fixture.handoff.config.checkpoints[0].clone();
        desired.static_artifact_bindings = vec![static_id];
        let desired = vec![serde_json::to_value(desired).unwrap()];
        assert!(matches!(prepare_provider_input_revision(fixture.handoff.run_path.clone(), &before, &writer, desired.clone()).unwrap(), ProviderInputRevision::Deferred { .. }));
        let evidence = CheckedRunEvidence { movements_path:&fixture.handoff.ctx.movements_path, ledger_index_path:&fixture.handoff.ctx.ledger_index_path,
            blobs_dir:&fixture.handoff.ctx.blobs_dir, max_control_frame_bytes:1<<20, max_manifest_bytes:1<<20, max_payload_bytes:1<<26 };
        let cutoff = fixture.rows.len() as u64;
        let prepared = evidence.prepare_provider_input_revision(fixture.handoff.run_path.clone(), &before, &writer, desired, cutoff);
        assert_eq!(std::fs::read(&fixture.handoff.run_path).unwrap(), before);
        if mode == "ordinary_active" {
            assert!(matches!(prepared.unwrap(), ProviderInputRevision::Deferred { .. })); continue;
        }
        if mode == "denied_read" {
            assert!(prepared.as_ref().err().unwrap().to_string().contains("static correction was not readable in the exact failed input"));
        }
        if mode != "valid" { assert!(prepared.is_err(), "{mode}: {prepared:?}"); continue; }
        let mut prepared = prepared.unwrap();
        let mut before = before;
        let mut cutoff = cutoff;
        let mut chain = Vec::new();
        for count in 1..=2 {
        let ProviderInputRevision::Prepared { edit } = prepared else { panic!("eligible correction deferred"); };
        let after = RunRecord::from_bytes(edit.content.as_bytes()).unwrap();
        assert_eq!(record.checkpoint(&"input".into()), after.checkpoint(&"input".into()));
        assert_eq!(persisted_pending_handoff(&record, &writer.owner).unwrap(), persisted_pending_handoff(&after, &writer.owner).unwrap());
        let revised = AuthoredRun { json_format:edit.json_format, content:edit.content.clone(), patch:edit.patch.clone(), before_hash:edit.before_hash,
            after_hash:edit.after_hash, correlation_id:edit.correlation_id,
            authoring_result_checkpoint:active_checkpoint_for_role(&after, &fixture.handoff.config, RunAuthoringCheckpointRole::AuthoringResult).unwrap().id.clone(),
            authoring_result_interface:authored.authoring_result_interface.clone(), pending_handoff:authored.pending_handoff.clone() };
        fixture.install(&revised);
        let evidence = CheckedRunEvidence { movements_path:&fixture.handoff.ctx.movements_path, ledger_index_path:&fixture.handoff.ctx.ledger_index_path,
            blobs_dir:&fixture.handoff.ctx.blobs_dir, max_control_frame_bytes:1<<20, max_manifest_bytes:1<<20, max_payload_bytes:1<<26 };
        let install = CheckedRunInstall {
            call:ConsentEvidenceRef {artifact:fixture.rows[cutoff as usize].artifact.clone().unwrap(), movement_seq:cutoff},
            result:ConsentEvidenceRef {artifact:fixture.rows[cutoff as usize + 1].artifact.clone().unwrap(), movement_seq:cutoff + 1},
        };
        chain.push(checked_declared_run_install(&evidence, &fixture.handoff.run_path, &before, &install, vec![writer.clone()]).unwrap());
        let (coordinate, source) = fixture.handoff.session.index().unwrap().exact_provider_source_outbound(
            &record.id, &record.worker, &writer.owner, source).unwrap();
        let hash = ContentHash::of_bytes(&initial);
        let checkpoint = record.checkpoint(&"input".into()).unwrap();
        assert!(input_revision::checked_failed_source_static_context(&evidence, &fixture.handoff.run_path, &[], &writer.owner, &source).unwrap().is_none());
        let proof = input_revision::checked_failed_source_static_context(&evidence, &fixture.handoff.run_path, &chain, &writer.owner, &source).unwrap().unwrap();
        let refs = proof.refs_for(&coordinate, &source, &hash, checkpoint).unwrap();
        assert_eq!(refs.iter().map(|reference| reference.id.clone()).collect::<Vec<_>>(), static_ids[..count]);
        let mut foreign = source.clone(); foreign.seq += 1;
        assert!(input_revision::checked_failed_source_static_context(&evidence, &fixture.handoff.run_path, &chain, &writer.owner, &foreign).unwrap().is_none());
        assert!(proof.refs_for(&coordinate, &foreign, &hash, checkpoint).is_err());
        foreign = source.clone(); foreign.artifact_binding_refs[0].content = ContentHash::of_bytes(b"other revision");
        assert!(proof.refs_for(&coordinate, &foreign, &hash, checkpoint).is_err());
        foreign = source.clone(); foreign.readable_artifacts.clear();
        assert!(proof.refs_for(&coordinate, &foreign, &hash, checkpoint).is_err());
        assert!(proof.refs_for(&coordinate, &source, &ContentHash::of_bytes(b"other snapshot"), checkpoint).is_err());
        let mut other = checkpoint.clone(); other.epoch.0 += 1;
        assert!(proof.refs_for(&coordinate, &source, &hash, &other).is_err());
        if count == 2 { break; }
        before = edit.content.into_bytes();
        cutoff = fixture.rows.len() as u64;
        let mut desired = fixture.handoff.config.checkpoints[0].clone(); desired.static_artifact_bindings = static_ids.clone();
        prepared = evidence.prepare_provider_input_revision(fixture.handoff.run_path.clone(), &before, &writer,
            vec![serde_json::to_value(desired).unwrap()], cutoff).unwrap();
        }
    }
}

struct CompletedInputReplayFixture {
    _root: tempfile::TempDir,
    handoff: ProviderOutputHandoff,
    rows: Vec<Movement>,
    input: ArtifactRef,
    input_file: InterfaceArtifactFile,
}

#[test]
fn failed_capture_closes_only_a_fenced_unsubmitted_final_with_its_exact_admitted_failure() {
    let mut fixture = CompletedInputReplayFixture::new();
    let before = std::fs::read(&fixture.handoff.run_path).unwrap();
    let file = InterfaceArtifactFile::write(fixture.handoff.var_dir.join("final"), "answer", br#"{"text":"final answer"}"#).unwrap();
    let identity = artifact_identity(&file, &fixture.handoff.var_dir).unwrap();
    let authored = fixture.handoff.author_run_with_bindings(&before, &[fixture.input.id.clone()], &[PreparedHandoff {
        identity, output_checkpoint: fixture.handoff.config.checkpoint_for_role(RunAuthoringCheckpointRole::ProviderArtifactOutput).unwrap().clone(),
        verified_inputs: Vec::new(),
    }], true, None, false, true, &[]).unwrap();
    fixture.install(&authored);
    let record = RunRecord::from_bytes(authored.content.as_bytes()).unwrap();
    let pending = &authored.pending_handoff;
    assert!(fixture.handoff.pending_source_unsubmitted(&record, pending).unwrap());
    assert!(!fixture.handoff.unsubmitted_source_failure_before(pending, fixture.rows.len() as u64).unwrap());
    let error = InterfaceArtifactFile::write(fixture.handoff.var_dir.join("failure"), "turn.error",
        &serde_json::to_vec(&serde_json::json!({"label":"turn.error","will_retry":false,
            "additional_details":{"source_movement_seq":pending.source_movement_seq}})).unwrap()).unwrap();
    let failure_seq = fixture.rows.len() as u64;
    fixture.edge(authored.content.as_bytes(), &pending.items[0].checkpoint.id, &error, "worker".into(), MovementKind::InterfaceArrival);
    assert!(!fixture.handoff.unsubmitted_source_failure_before(pending, failure_seq).unwrap());
    assert!(fixture.handoff.unsubmitted_source_failure_before(pending, failure_seq + 1).unwrap());
    let mut changed = pending.clone(); changed.source_movement_seq += 1;
    assert!(!fixture.handoff.unsubmitted_source_failure_before(&changed, failure_seq + 1).unwrap());
    let mut legacy = pending.clone(); legacy.dispatch_fenced = false;
    assert!(!fixture.handoff.pending_source_unsubmitted(&record, &legacy).unwrap());
    let marker = source_withdrawal::dispatch_path(&fixture.handoff.ctx, &record, &pending.items[0]);
    std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
    std::fs::write(&marker, b"{}").unwrap();
    assert!(!fixture.handoff.pending_source_unsubmitted(&record, pending).unwrap());
    assert!(!fixture.handoff.unsubmitted_source_failure_before(pending, failure_seq + 1).unwrap());
    std::fs::remove_file(marker).unwrap();
    fixture.edge(authored.content.as_bytes(), &pending.items[0].checkpoint.id, &file, "worker".into(), MovementKind::InterfaceArrival);
    assert!(!fixture.handoff.pending_source_unsubmitted(&record, pending).unwrap());
    assert!(!fixture.handoff.unsubmitted_source_failure_before(pending, fixture.rows.len() as u64).unwrap());

    let mut effect = CompletedInputReplayFixture::new();
    let call = effect.output(true, false, false);
    let record = RunRecord::from_bytes(call.content.as_bytes()).unwrap();
    assert!(!effect.handoff.pending_source_unsubmitted(&record, &call.pending_handoff).unwrap());
}

#[test]
fn historical_role_binding_is_static_only_with_its_admitted_grant() {
    let mut fixture = CompletedInputReplayFixture::new();
    let role_file = InterfaceArtifactFile::write(fixture.handoff.var_dir.join("role"), "context.role",
        br#"{"label":"context.role","text":"Reviewer instructions"}"#).unwrap();
    let role = artifact_identity(&role_file, &fixture.handoff.var_dir).unwrap().artifact;
    std::fs::write(fixture.handoff.ctx.blobs_dir.join(role.content.to_hex()), std::fs::read(role_file.manifest_path()).unwrap()).unwrap();
    std::fs::write(fixture.handoff.ctx.blobs_dir.join(role_file.read_manifest().unwrap().payload_content_hash.unwrap()), role_file.read_payload_bytes().unwrap()).unwrap();
    let mut value: serde_json::Value = serde_json::from_slice(&std::fs::read(&fixture.handoff.run_path).unwrap()).unwrap();
    value["checkpoints"].as_array_mut().unwrap().iter_mut().find(|cp| cp["id"] == "input").unwrap()["artifact_bindings"] =
        serde_json::json!([{ "id": role.id, "access": { "read": true, "create": false, "change": false, "delete": false } }]);
    let bytes = serde_json::to_vec(&value).unwrap();
    let record = RunRecord::from_bytes(&bytes).unwrap();
    fixture.rows.clear();
    fixture.edge(&bytes, &"input".into(), &fixture.input_file.clone(), "provider".into(), MovementKind::InterfaceOutbound);
    let mut metadata = fixture.rows[0].invocation_metadata.as_ref().unwrap().invocation().unwrap().clone();
    metadata.grant.role_artifacts = vec![role.id.to_string()];
    fixture.rows[0].invocation_metadata = Some(metadata.clone().into());
    fixture.save();
    let recover = |fixture: &CompletedInputReplayFixture| {
        recover_source_session_context_from_cache(&fixture.handoff.ctx.blobs_dir,
            fixture.handoff.ctx.required_host_io_limits().unwrap(),
            &ProviderHandoffSession::new(&fixture.handoff.ctx).unwrap(), &record, &fixture.handoff.config, 0,
            &fixture.handoff.context_output_labels, &fixture.handoff.session_snapshot_labels)
    };
    let (history, _, statics) = recover(&fixture).unwrap();
    assert_eq!(statics, vec![role.id.clone()]);
    assert_eq!(history.artifacts.len(), 1);
    assert_eq!(history.artifacts[0].source_artifact, fixture.input);
    metadata.grant.role_artifacts.clear();
    fixture.rows[0].invocation_metadata = Some(metadata.into());
    fixture.save();
    assert!(recover(&fixture).err().unwrap().contains("no eligible movement"));
}

#[test]
#[ignore = "requires explicitly selected read-only source context evidence"]
fn selected_home_source_context_preserves_admitted_roles_and_input() {
    let home = PathBuf::from(std::env::var("OCTOPUS_REPLAY_HOME").unwrap());
    let provider = InterfaceId::new(std::env::var("OCTOPUS_REPLAY_PROVIDER").unwrap());
    let run = RunId::new(std::env::var("OCTOPUS_REPLAY_RUN").unwrap());
    let dir = home.join("registry/interface").join(provider.as_str());
    let descriptor: InterfaceDescriptor = serde_json::from_slice(&std::fs::read(dir.join("interface.json")).unwrap()).unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let (sender, receiver) = tokio::sync::mpsc::channel(1); drop(receiver);
    let ctx = InterfaceLiftContext::unprivileged(provider.clone(), dir, Some(descriptor.clone()), scratch.path().join("var"),
        home.join("ledger/movements.jsonl"), home.join("ledger/blobs"), scratch.path().join("index.sqlite"),
        BTreeMap::new(), sender, Default::default()).with_host_io_limits(InterfaceHostIoLimits {
            max_control_frame_bytes:1_048_576, max_stderr_bytes:262_144,
            max_manifest_bytes:1_048_576, max_payload_bytes:268_435_456,
        }).unwrap();
    let path = home.join("state/runs").join(format!("{run}.json"));
    let current = std::fs::read(&path).unwrap();
    let record = RunRecord::from_bytes(&current).unwrap();
    let config = config_from_persisted_profile(&record, &provider, &run_authoring_profile(&descriptor).unwrap()).unwrap();
    let (outputs, snapshots) = provider_context_label_sets(&descriptor).unwrap();
    let session = ProviderHandoffSession::new(&ctx).unwrap();
    for source in std::env::var("OCTOPUS_REPLAY_SOURCE").unwrap().split(',').map(|value| value.parse::<u64>().unwrap()) {
        let (_, admitted) = session.index().unwrap().exact_provider_source_outbound(&run, &record.worker, &provider, source).unwrap();
        assert!(!admitted.role_artifacts.is_empty());
        let (history, _, statics) = recover_source_session_context_from_cache(&ctx.blobs_dir, ctx.required_host_io_limits().unwrap(),
            &session, &record, &config, source, &outputs, &snapshots).unwrap();
        assert!(admitted.role_artifacts.iter().all(|id| statics.contains(&ArtifactId::new(id))));
        assert!(history.artifacts.iter().all(|entry| !admitted.role_artifacts.contains(&entry.source_artifact.id.to_string())));
        assert_eq!(history.artifacts.last().unwrap().source_artifact, admitted.artifact);
        assert_eq!(history.artifacts.last().unwrap().source_movement_seq, Some(source));
        eprintln!("source {source}: admitted roles remain static; exact input and ledger provenance recovered");
    }
    assert_eq!(std::fs::read(path).unwrap(), current);
}

#[tokio::test]
#[ignore = "requires an explicitly selected Home; reads live evidence without delivering or writing run state"]
async fn selected_home_failed_capture_recovers_original_inputs_without_delivering() {
    let home = PathBuf::from(std::env::var("OCTOPUS_REPLAY_HOME").unwrap());
    let provider = InterfaceId::new(std::env::var("OCTOPUS_REPLAY_PROVIDER").unwrap());
    let run = RunId::new(std::env::var("OCTOPUS_REPLAY_RUN").unwrap());
    let sequence: u64 = std::env::var("OCTOPUS_REPLAY_SOURCE").unwrap().parse().unwrap();
    let failed: u64 = std::env::var("OCTOPUS_REPLAY_FAILED_SOURCE").unwrap().parse().unwrap();
    let directory = home.join("registry/interface").join(provider.as_str());
    let descriptor: InterfaceDescriptor = serde_json::from_slice(&std::fs::read(directory.join("interface.json")).unwrap()).unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let (sender, receiver) = tokio::sync::mpsc::channel(1); drop(receiver);
    let ctx = InterfaceLiftContext::unprivileged(provider.clone(), directory, Some(descriptor.clone()),
        home.join("interfaces").join(provider.as_str()), home.join("ledger/movements.jsonl"), home.join("ledger/blobs"),
        scratch.path().join("index.sqlite"), BTreeMap::from([
            ("run_records_root".into(), home.join("state/runs")),
            ("interface_hand_in_root".into(), home.join("interface-hand-in")),
        ]), sender, Default::default()).with_host_io_limits(InterfaceHostIoLimits {
            max_control_frame_bytes:1_048_576, max_stderr_bytes:262_144,
            max_manifest_bytes:1_048_576, max_payload_bytes:268_435_456,
        }).unwrap();
    let path = home.join("state/runs").join(format!("{run}.json"));
    let before = std::fs::read(&path).unwrap();
    let mut replay = octopus_world_pyecho_life_runtime::LedgerReplayReader::open(&ctx).unwrap();
    let mut input = None;
    while let Some(batch) = replay.read_batch(256).unwrap() {
        for entry in batch { if entry.movement.seq.0 == sequence { input = Some(replay.envelope(&entry).unwrap()); } }
        if input.is_some() { break; }
    }
    let input = input.expect("selected source is not an outbound of this provider");
    let session = ProviderHandoffSession::new(&ctx).unwrap();
    let pinned = ctx.pinned_run_record_bytes(&input).unwrap();
    let handoff = session.from_invocation_state(&ctx, &descriptor, &run, &input, sequence, &pinned).unwrap();
    let contexts = handoff.checked_failed_source_contexts(&before, &[], None).unwrap();
    assert!(contexts.contains_key(&failed), "lost the source whose final capture failed");
    let recovered = &contexts[&failed].artifacts;
    assert_eq!(recovered.iter().filter(|entry| entry.source_movement_seq == Some(failed)).count(), 1);
    let (_, source) = session.index().unwrap().exact_provider_source_outbound(&run, &handoff.config.worker, &provider, failed).unwrap();
    assert!(recovered.iter().any(|entry| entry.source_artifact == source.artifact));
    assert_eq!(std::fs::read(path).unwrap(), before);
    eprintln!("recovered exact source {failed} and {} admitted history artifacts; no outputs delivered", recovered.len());
}

impl CompletedInputReplayFixture {
    fn new() -> Self {
        Self::with_required_final(false)
    }

    fn with_required_final(required: bool) -> Self {
        use serde_json::json;
        let root = tempfile::tempdir().unwrap();
        let binding = |id: &str| json!(AuthoredInterfaceBinding::all(InterfaceId::new(id)));
        let row = |name: &str, role: &str, source: &str, target: &str, tool: Option<&str>| json!({
            "name":name,"role":role,"interface_binding":binding(source),"outbound_interface_bindings":[binding(target)],
            "include_context":role == "stable","required_verified_artifacts":tool.map(|tool| vec![json!({
                "mark":"tool.call.allowed","artifact_label":"tool.call","verifier":"tool_call_allowed","params":{"tools":[tool]}
            })]).unwrap_or_default()
        });
        let mut config: RunAuthoringConfig = serde_json::from_value(json!({
            "run":"run","worker":"worker","provider_interface":"provider","authoring_source_interface":"writer",
            "file_write_tool":"write","file_write_label":"tool.call","file_write_result_label":"write.result","result_timeout_ms":1000,
            "checkpoints":[row("input","stable","room","provider",None),row("alternate","stable","alternate_room","provider",None),
                row("control","independent_input","control","provider",None),
                row("client","provider_artifact_output","provider","client",None),
                row("tool","provider_tool_output","provider","room",Some("read")),
                row("writer_call","authoring_call","writer","writer_room",Some("write")),
                row("writer_result","authoring_result","writer_room","provider",None)]
        })).unwrap();
        if required {
            let mut requirement = config.checkpoints.iter().find(|cp| cp.name == "tool").unwrap().required_verified_artifacts[0].clone();
            requirement.artifact_label = "answer".into();
            config.checkpoints.iter_mut().find(|cp| cp.name == "client").unwrap().required_verified_artifacts = vec![requirement];
        }
        let topology = BoundRunAuthoringTopology { schema_version:2,run:config.run.clone(),worker:config.worker.clone(),
            provider_interface:config.provider_interface.clone(),authoring_source_interface:config.authoring_source_interface.clone(),
            payload_artifact_bindings:Vec::new(),writer_run:None,checkpoints:config.checkpoints.clone() };
        let initial = serde_json::to_vec(&json!({"id":"run","ee":"ee","worker":"worker","trace":"run","status":"running",
            "checkpoints":config.checkpoints.iter().map(|cp|materialize_checkpoint(cp,&cp.name,&config.run,&[])).collect::<Vec<_>>(),
            "active_checkpoints":config.checkpoints.iter().map(|cp|cp.name.clone()).collect::<Vec<_>>(),
            "interface_state":{"provider":{"context_generation":0,"authoring_generation":0,
                "frontier_checkpoint_ids":["client","tool","writer_call","writer_result"],"run_authoring_binding":topology}}
        })).unwrap();
        let (arrivals, _) = tokio::sync::mpsc::channel(1);
        let ctx = InterfaceLiftContext::unprivileged(config.provider_interface.clone(),root.path().join("interface"),None,
            root.path().join("var"),root.path().join("movements.jsonl"),root.path().join("blobs"),root.path().join("index.sqlite"),
            BTreeMap::new(),arrivals,Default::default()).with_host_io_limits(octopus_interface_transport::InterfaceHostIoLimits {
                max_control_frame_bytes:1_048_576,max_stderr_bytes:262_144,max_manifest_bytes:1_048_576,max_payload_bytes:16_777_216,
            }).unwrap();
        std::fs::create_dir_all(&ctx.var_dir).unwrap();
        std::fs::create_dir_all(&ctx.blobs_dir).unwrap();
        std::fs::write(&ctx.movements_path, b"").unwrap();
        let run_path = root.path().join("run.json");
        std::fs::write(&run_path,&initial).unwrap();
        let result_reader = LedgerTailReader::open_named(&ctx,"completed-input-test").unwrap();
        let handoff = ProviderOutputHandoff {
            invocation_metadata: InvocationMetadata::new(Default::default()),
            session:ProviderHandoffSession::new(&ctx).unwrap(),var_dir:ctx.var_dir.clone(),ctx,
            config,run_path,hand_in_root:root.path().join("hand-in"),source_movement_seq:0,source_carries_context:true,failed_source:false,
            checked_settlement_output: None,
            context_output_labels:BTreeSet::from(["tool.call".into()]),session_snapshot_labels:BTreeSet::new(),standing_candidate_labels:BTreeSet::new(),
            granted_tools:BTreeSet::from(["read".into(),"write".into()]),granted_interfaces:BTreeSet::new(),granted_mark_outcomes:BTreeSet::new(),
            bound_topology:topology,delivery_mode:HandoffDeliveryMode::Rearm,invocation_session_snapshot:None,
            result_reader:Arc::new(tokio::sync::Mutex::new(result_reader)),
            state:Arc::new(tokio::sync::Mutex::new(HandoffState {
            validated_withdrawal_output: None,
                context_artifacts:Vec::new(),static_context_artifacts:Vec::new(),session_context:ResolvedSessionContext {
                    provider_interface:"provider".into(),artifacts:Vec::new(),
                },session_snapshot_head:None,run_record_bytes:initial.clone(),run_record_evidence_seq:Some(0),
                consumed_input:true,consumed_context_free_independent_input:None,consumed_verified_input:false,deferred_context_generations:0,
            })),
        };
        let file = InterfaceArtifactFile::write(handoff.var_dir.join("input"),"tool.result",
            br#"{"label":"tool.result","tool":"read","correlation_id":"first-call","ok":true,"body":{"value":1}}"#).unwrap();
        let input = artifact_identity(&file,&handoff.var_dir).unwrap().artifact;
        let mut fixture = Self { _root:root,handoff,rows:Vec::new(),input,input_file:file.clone() };
        fixture.edge(&initial,&"input".into(),&file,"provider".into(),MovementKind::InterfaceOutbound);
        fixture.edge(&initial,&"alternate".into(),&file,"provider".into(),MovementKind::InterfaceOutbound);
        append_session_payload(&mut fixture.handoff.state.try_lock().unwrap().session_context,"tool.result",
            &file.read_payload_bytes().unwrap(),&fixture.input,Some(0),"prior exact input").unwrap();
        fixture
    }

    fn save(&self) {
        std::fs::write(&self.handoff.ctx.movements_path,self.rows.iter()
            .map(|row|format!("{}\n",serde_json::to_string(row).unwrap())).collect::<String>()).unwrap();
    }

    fn edge(&mut self, bytes: &[u8], checkpoint: &CheckpointId, file: &InterfaceArtifactFile, target: InterfaceId, kind: MovementKind) {
        use octopus_interfaces::{GrantProjection, RuntimeInvocationMetadata};
        let h = &self.handoff;
        let record = RunRecord::from_bytes(bytes).unwrap();
        let cp = record.checkpoint(checkpoint).unwrap();
        let hash = ContentHash::of_bytes(bytes).to_hex();
        std::fs::write(h.ctx.blobs_dir.join(&hash),bytes).unwrap();
        let artifact = artifact_identity(file,file.manifest_path().parent().unwrap()).unwrap().artifact;
        let manifest = file.read_manifest().unwrap();
        std::fs::write(h.ctx.blobs_dir.join(artifact.content.to_hex()),std::fs::read(file.manifest_path()).unwrap()).unwrap();
        std::fs::write(h.ctx.blobs_dir.join(manifest.payload_content_hash.unwrap()),file.read_payload_bytes().unwrap()).unwrap();
        let mut metadata = InvocationMetadata::new(GrantProjection {
            tools:h.granted_tools.iter().cloned().collect(),interfaces:vec![target.to_string()],
            artifact_access:cp.artifact_bindings.iter().map(|binding|(binding.id.to_string(),binding.access.clone())).collect(),
            mark_outcomes:h.granted_mark_outcomes.iter().cloned().collect(),..Default::default()
        }).with_runtime(RuntimeInvocationMetadata {
            run:record.id.clone(),ee:record.ee.clone(),worker:record.worker.clone(),checkpoint:checkpoint.clone(),
            interface:if kind == MovementKind::InterfaceArrival {h.ctx.interface.clone()} else {target.clone()},
            epoch:cp.epoch.0,run_record_hash:Some(hash),
        }).with_outbound_interface_bindings(cp.outbound_interface_bindings.iter().map(|b|b.interface.clone()).collect())
            .with_checkpoint_binding_policy(CheckpointBindingPolicySnapshot {inbound:cp.interface_binding.clone().unwrap(),outbound:cp.outbound_interface_bindings.clone()})
            .with_required_verified_artifacts(cp.required_verified_artifacts.clone())
            .with_checkpoint_admission(octopus_ledger::CheckpointAdmissionMetadata {artifact_label:manifest.label,
                required_artifact_labels:cp.required_verified_artifacts.iter().map(|r|r.artifact_label.clone()).collect(),complete:true});
        metadata.artifact_binding_refs = cp.artifact_bindings.iter().map(|b|ArtifactRef {id:b.id.clone(),
            content:ContentHash::from_hex(b.id.as_str().rsplit(':').next().unwrap()).unwrap()}).collect();
        let seq = self.rows.len() as u64;
        self.rows.push(Movement {seq:octopus_ledger::Seq(seq),lamport:octopus_ledger::Lamport(seq),recorded_at_unix_ms:seq,
            trace:cp.trace.clone(),kind,from:Endpoint::Interface(cp.interface_binding.as_ref().unwrap().interface.clone()),
            to:Endpoint::Interface(target),artifact:Some(artifact),invocation_metadata:Some(metadata.into())});
        self.save();
    }

    fn install(&mut self, authored: &AuthoredRun) {
        let before = std::fs::read(&self.handoff.run_path).unwrap();
        let record = RunRecord::from_bytes(&before).unwrap();
        let cp = active_checkpoint_for_role(&record,&self.handoff.config,RunAuthoringCheckpointRole::AuthoringCall).unwrap().id.clone();
        let call = self.handoff.materialize_file_write_call(authored).unwrap();
        self.edge(&before,&cp,&call,authored.authoring_result_interface.clone(),MovementKind::InterfaceOutbound);
        std::fs::write(&self.handoff.run_path,&authored.content).unwrap();
        let result = InterfaceArtifactFile::write(self.handoff.var_dir.join("results"),"write.result",
            &serde_json::to_vec(&serde_json::json!({"label":"write.result","tool":"write","correlation_id":authored.correlation_id,"ok":true,
                "body":{"result":"files_written","writes":[{"path":self.handoff.run_path,"before_hash":authored.before_hash,
                    "after_hash":authored.after_hash,"bytes":authored.content.len()}]}})).unwrap()).unwrap();
        self.edge(authored.content.as_bytes(),&authored.authoring_result_checkpoint,&result,"provider".into(),MovementKind::InterfaceOutbound);
    }

    fn output(&mut self, terminal: bool, admit: bool, mark: bool) -> AuthoredRun {
        let before = std::fs::read(&self.handoff.run_path).unwrap();
        let record = RunRecord::from_bytes(&before).unwrap();
        let file = InterfaceArtifactFile::write(self.handoff.var_dir.join("outputs"),"tool.call",
            &serde_json::to_vec(&serde_json::json!({"label":"tool.call","tool":"read","arguments":{},"correlation_id":format!("next-{}",self.rows.len())})).unwrap()).unwrap();
        let identity = artifact_identity(&file,&self.handoff.var_dir).unwrap();
        let mut context = current_context_artifacts(&record,&self.handoff.config).unwrap();
        append_artifact(&mut context,self.input.id.clone());
        append_artifact(&mut context,identity.artifact.id.clone());
        let authored = self.handoff.author_run_with_bindings(&before,&context,&[PreparedHandoff {
            identity,output_checkpoint:self.handoff.config.checkpoint_for_role(RunAuthoringCheckpointRole::ProviderToolOutput).unwrap().clone(),verified_inputs:Vec::new(),
        }],true,None,false,terminal,&[]).unwrap();
        self.install(&authored);
        if admit {
            self.edge(authored.content.as_bytes(),&authored.pending_handoff.items[0].checkpoint.id,&file,"worker".into(),MovementKind::InterfaceArrival);
            if mark { self.mark(&authored); }
        }
        authored
    }

    fn mark(&mut self, authored: &AuthoredRun) {
        let item = &authored.pending_handoff.items[0];
        let record = RunRecord::from_bytes(authored.content.as_bytes()).unwrap();
        let trace = record.checkpoint(&item.checkpoint.id).unwrap().trace.clone();
        let (_, artifact) = load_pending_artifact(&self.handoff.var_dir,item).unwrap();
        let requirement = &item.checkpoint.required_verified_artifacts[0];
        let seq = self.rows.len() as u64;
        let mark = octopus_interfaces::Mark::passed(requirement.mark.clone(),item.checkpoint.id.clone(),self.handoff.config.run.clone(),
            item.checkpoint.epoch,artifact.clone(),requirement.verifier.clone(),octopus_ledger::VerifierPackagePin {
                descriptor_content:ContentHash::of_bytes(b"descriptor"),script_content:ContentHash::of_bytes(b"script"),
            },serde_json::json!({}));
        self.rows.push(Movement {seq:octopus_ledger::Seq(seq),lamport:octopus_ledger::Lamport(seq),recorded_at_unix_ms:seq,
            trace,kind:MovementKind::MarkOutcomeAdmitted(requirement.mark.clone()),
            from:Endpoint::Interface("provider".into()),to:Endpoint::Interface("worker".into()),artifact:Some(artifact),invocation_metadata:Some(mark.into())});
        self.save();
    }

    fn publish(&mut self, authored: &AuthoredRun) {
        let publication = self.handoff.author_context_publication(authored.content.as_bytes(),&authored.pending_handoff).unwrap();
        self.install(&publication);
    }

    fn reload_definition(&mut self) -> RunRecord {
        let record = RunRecord::from_bytes(&std::fs::read(&self.handoff.run_path).unwrap()).unwrap();
        self.handoff.config = attempt_config(&record, &self.handoff.config).unwrap();
        self.handoff.bound_topology.checkpoints = self.handoff.config.checkpoints.clone();
        record
    }
}

#[tokio::test]
async fn a_snapshot_restores_checked_history_before_an_already_replayed_output() {
    let mut fixture = CompletedInputReplayFixture::new();
    fixture.handoff.context_output_labels.insert("reply".into());
    fixture.handoff.session_snapshot_labels.insert("reply".into());
    let call = fixture.output(false, true, true);
    fixture.publish(&call);
    fixture.reload_definition();
    let (file, reference) = load_pending_artifact(&fixture.handoff.var_dir, &call.pending_handoff.items[0]).unwrap();
    let sequence = fixture.handoff.pending_handoff_evidence(&call.pending_handoff).unwrap()[0].unwrap();
    let mut captured = fixture.handoff.state.try_lock().unwrap().session_context.clone();
    append_session_payload(&mut captured, "tool.call", &file.read_payload_bytes().unwrap(), &reference, Some(sequence), "admitted output").unwrap();
    let reply = InterfaceArtifactFile::write(fixture.handoff.var_dir.join("reply"), "reply", br#"{"text":"Done"}"#).unwrap();
    let snapshot = materialize_session_snapshot(&reply, &captured, None).unwrap();
    let identity = artifact_identity(&snapshot, &fixture.handoff.var_dir).unwrap();
    let source = identity.artifact.clone();
    let before = std::fs::read(&fixture.handoff.run_path).unwrap();
    let authored = fixture.handoff.author_run_with_bindings(&before, &[source.id.clone()], &[PreparedHandoff {
        identity, output_checkpoint: fixture.handoff.config.checkpoint_for_role(RunAuthoringCheckpointRole::ProviderArtifactOutput).unwrap().clone(), verified_inputs: Vec::new(),
    }], true, None, false, true, &[]).unwrap();
    fixture.install(&authored);
    fixture.edge(authored.content.as_bytes(), &authored.pending_handoff.items[0].checkpoint.id, &snapshot, "worker".into(), MovementKind::InterfaceArrival);
    if authored.pending_handoff.context_publication.is_some() { fixture.publish(&authored); }
    let record = fixture.reload_definition();
    let bytes = std::fs::read(&fixture.handoff.run_path).unwrap();
    let ledger = std::fs::read(&fixture.handoff.ctx.movements_path).unwrap();
    let pending = persisted_pending_handoff(&record, &fixture.handoff.config.provider_interface).unwrap().unwrap();
    let mut state = fixture.handoff.state.try_lock().unwrap().clone();
    state.run_record_bytes = bytes.clone();
    state.session_context.artifacts = vec![captured.artifacts[1].clone()];
    fixture.handoff.restore_pending_context(&mut state, &record, &pending).unwrap();
    assert_eq!(state.session_context.artifacts.iter().map(|entry| &entry.source_artifact).collect::<Vec<_>>(), vec![&fixture.input, &reference, &source]);
    assert_eq!(std::fs::read(&fixture.handoff.run_path).unwrap(), bytes);
    assert_eq!(std::fs::read(&fixture.handoff.ctx.movements_path).unwrap(), ledger);
}

#[tokio::test]
async fn outstanding_results_preserve_context_and_require_exact_consumption() {
    let mut fixture = CompletedInputReplayFixture::new();
    let h = &mut fixture.handoff;
    h.run_path = h.run_path.canonicalize().unwrap();
    h.ctx.root_slots.insert("run_records_root".into(), h.run_path.parent().unwrap().to_path_buf());
    h.granted_mark_outcomes.insert("tool.result.received".into());
    let catalogue_file = InterfaceArtifactFile::write(h.var_dir.join("catalogue"), "context.collection",
        br#"{"label":"context.collection","directory":"entries"}"#).unwrap();
    let catalogue = artifact_identity(&catalogue_file, &h.var_dir).unwrap().artifact;
    let manifest = catalogue_file.read_manifest().unwrap();
    std::fs::write(h.ctx.blobs_dir.join(catalogue.content.to_hex()), std::fs::read(catalogue_file.manifest_path()).unwrap()).unwrap();
    std::fs::write(h.ctx.blobs_dir.join(manifest.payload_content_hash.unwrap()), catalogue_file.read_payload_bytes().unwrap()).unwrap();
    for template in h.config.checkpoints.iter_mut().filter(|template|template.role == RunAuthoringCheckpointRole::Stable && template.include_context) {
        template.static_artifact_bindings.push(catalogue.id.clone());
    }
    h.state.try_lock().unwrap().static_context_artifacts.push(catalogue.id.clone());
    let door = vec![AuthoredInterfaceBinding::all("result_room".into())];
    {
        let tool = h.config.checkpoints.iter_mut().find(|cp| cp.role == RunAuthoringCheckpointRole::ProviderToolOutput).unwrap();
        tool.outbound_interface_bindings = door.clone();
        tool.result_receiving.insert("read".into(), serde_json::from_value(serde_json::json!({
            "mark":"tool.result.received","artifact_label":"read.result","verifier":"result_correspondence","params":{}
        })).unwrap());
    }
    h.bound_topology.checkpoints = h.config.checkpoints.clone();
    h.config.validate(&h.ctx.interface, &h.config.run).unwrap();
    let mut duplicate = h.config.clone();
    duplicate.checkpoints[0].interface_binding = AuthoredInterfaceBinding::all("result_room".into());
    assert!(duplicate.validate(&h.ctx.interface, &h.config.run).is_err());
    let mut seed: serde_json::Value = serde_json::from_slice(&std::fs::read(&h.run_path).unwrap()).unwrap();
    seed["interface_state"]["provider"][RUN_AUTHORING_BINDING] = serde_json::to_value(&h.bound_topology).unwrap();
    seed["checkpoints"].as_array_mut().unwrap().iter_mut().find(|cp| cp["id"] == "tool").unwrap()["outbound_interface_bindings"] =
        serde_json::to_value(door).unwrap();
    let mut binding = ArtifactBinding { id:fixture.input.id.clone(), access:Default::default() };
    binding.access.change = true;
    for cp in seed["checkpoints"].as_array_mut().unwrap().iter_mut().filter(|cp| cp["id"] == "input" || cp["id"] == "alternate") {
        cp["artifact_bindings"] = serde_json::json!([binding, ArtifactBinding {id:catalogue.id.clone(), access:Default::default()}]);
    }
    let initial = serde_json::to_vec(&seed).unwrap();
    std::fs::write(&h.run_path, &initial).unwrap();
    h.state.try_lock().unwrap().run_record_bytes = initial.clone();
    fixture.rows.clear();
    let input_file = fixture.input_file.clone();
    fixture.edge(&initial, &"input".into(), &input_file, "provider".into(), MovementKind::InterfaceOutbound);
    fixture.edge(&initial, &"alternate".into(), &input_file, "provider".into(), MovementKind::InterfaceOutbound);
    {
        let mut cold = fixture.handoff.clone();
        let mut detached = seed.clone();
        let writer_run = RunId::new("result_saved_writer");
        let writer = separate_writer_run(&mut detached, &cold.ctx.interface, &writer_run).unwrap();
        std::fs::write(cold.run_path.parent().unwrap().join(format!("{writer_run}.json")), JsonFormat::Compact.encode(&writer).unwrap()).unwrap();
        let detached = JsonFormat::Compact.encode(&detached).unwrap();
        std::fs::write(&cold.run_path, &detached).unwrap();
        let record = RunRecord::from_bytes(&detached).unwrap();
        cold.config = attempt_config(&record, &cold.config).unwrap();
        cold.bound_topology = serde_json::from_value(record.interface_state[&cold.ctx.interface][RUN_AUTHORING_BINDING].clone()).unwrap();
        cold.ctx.movements_path = cold.var_dir.join("result-saved.jsonl");
        cold.ctx.ledger_index_path = cold.var_dir.join("result-saved.sqlite");
        cold.hand_in_root = cold.var_dir.join("result-saved-calls");
        let mut source = fixture.rows[0].clone();
        let mut metadata = source.invocation_metadata.as_ref().unwrap().invocation().unwrap().clone();
        let hash = ContentHash::of_bytes(&detached);
        metadata.runtime.as_mut().unwrap().run_record_hash = Some(hash.to_hex());
        source.invocation_metadata = Some(metadata.clone().into());
        std::fs::write(cold.ctx.blobs_dir.join(hash.to_hex()), &detached).unwrap();
        std::fs::write(&cold.ctx.movements_path, format!("{}\n", serde_json::to_string(&source).unwrap())).unwrap();
        cold.invocation_metadata = metadata;
        cold.session = ProviderHandoffSession::new(&cold.ctx).unwrap();
        let call = InterfaceArtifactFile::write(cold.var_dir.join("detached-tool"), "tool.call",
            br#"{"tool":"read","arguments":{},"correlation_id":"detached-result"}"#).unwrap();
        let identity = artifact_identity(&call, &cold.var_dir).unwrap();
        let saved = cold.author_run_with_bindings(&detached, &[fixture.input.id.clone(), catalogue.id.clone(), identity.artifact.id.clone()], &[PreparedHandoff {
            identity, output_checkpoint:cold.config.checkpoint_for_role(RunAuthoringCheckpointRole::ProviderToolOutput).unwrap().clone(),
            verified_inputs:Vec::new(),
        }], true, None, false, false, &[]).unwrap();
        let edit = CheckedRunEdit { path:cold.run_path.clone(), json_format:saved.json_format, before_hash:saved.before_hash.clone(),
            after_hash:saved.after_hash.clone(), content:saved.content.clone(), patch:saved.patch.clone(),
            correlation_id:"provider-writer-run:run:1:1".into() };
        let writer = declared_provider_run_writer(&record, &cold.ctx.interface, &RunAuthoringProfile {
            file_write_tool:"write".into(), file_write_label:"tool.call".into(), file_write_result_label:"write.result".into(), result_timeout_ms:1000,
        }).unwrap();
        OwnerWriterEdit::prepare(&detached, &edit, &writer, 1<<26).unwrap().unwrap().materialize(&cold.hand_in_root).unwrap();
        let AuthoringAttemptRecovery::InFlight(recovered) = cold.recover_authoring_attempt(&record, &detached).unwrap() else { panic!("lost detached result receiver"); };
        assert_eq!(recovered.authored.pending_handoff, saved.pending_handoff);
        assert_eq!(recovered.authored.content, saved.content);
        assert_eq!(std::fs::read(&cold.run_path).unwrap(), detached);
        std::fs::write(&fixture.handoff.run_path, &initial).unwrap();
    }
    let first = fixture.output(false, true, true);
    let before = RunRecord::from_bytes(&initial).unwrap();
    let record = fixture.reload_definition();
    result_inputs::validate_saved(&fixture.handoff.ctx, &before, &record, &fixture.handoff.config).unwrap();
    let call_file = fixture.handoff.materialize_file_write_call(&first).unwrap();
    validate_authoring_attempt_source(AuthoringAttemptSourceValidation {
        ctx:&fixture.handoff.ctx, record:&before, record_bytes:&initial, config:&fixture.handoff.config,
        call:serde_json::from_slice(&call_file.read_payload_bytes().unwrap()).unwrap(),
        manifest_path:call_file.manifest_path().to_path_buf(),
    }).unwrap();
    let profile = RunAuthoringProfile { file_write_tool:"write".into(), file_write_label:"tool.call".into(),
        file_write_result_label:"write.result".into(), result_timeout_ms:1000 };
    let mut created = CheckedRunTransition { writer:declared_provider_run_writer(&before, &"provider".into(), &profile).unwrap(),
        before:initial.clone(), after:first.content.as_bytes().to_vec(), call:fixture.rows[2].clone(), result:fixture.rows[3].clone(),
        placement: false, };
    let mut metadata = created.call.invocation_metadata.as_ref().unwrap().invocation().unwrap().clone();
    metadata.grant.mark_outcomes.clear();
    created.call.invocation_metadata = Some(metadata.into());
    {
        let evidence = CheckedRunEvidence { movements_path:&fixture.handoff.ctx.movements_path, ledger_index_path:&fixture.handoff.ctx.ledger_index_path,
            blobs_dir:&fixture.handoff.ctx.blobs_dir, max_control_frame_bytes:1<<20, max_manifest_bytes:1<<20, max_payload_bytes:1<<26 };
        let mut reader = FileCheckedRunReader::open(&evidence, None).unwrap();
        result_inputs::validate_checked(&mut reader, &created).unwrap();
    }
    let receipt = fixture.handoff.config.checkpoints.iter().find(|cp| cp.role == RunAuthoringCheckpointRole::ResultInput).unwrap().clone();
    assert_eq!(receipt.static_artifact_bindings, [catalogue.id.clone(), first.pending_handoff.items[0].artifact_id.clone()]);
    let pinned = record.checkpoints.iter().find(|cp| checkpoint_record_matches_template(cp, &receipt, &record.id)).unwrap().clone();
    assert_eq!(pinned.artifact_bindings[0], binding);
    assert_eq!(pinned.artifact_bindings[1], ArtifactBinding {id:catalogue.id.clone(), access:Default::default()});
    assert_eq!(pinned.artifact_bindings[2].id, first.pending_handoff.items[0].artifact_id);
    assert!(!provider_context_bindings(&record, &"provider".into()).unwrap().iter()
        .any(|binding| binding.id == first.pending_handoff.items[0].artifact_id));
    fixture.publish(&first);
    let published = fixture.reload_definition();
    assert_eq!(published.checkpoint(&pinned.id), Some(&pinned));
    assert_eq!(provider_context_bindings(&published, &"provider".into()).unwrap()[0], binding);
    let alternate = fixture.handoff.config.checkpoints.iter().find(|cp| cp.name == "alternate").unwrap();
    let next_input = published.checkpoints.iter().find(|cp| checkpoint_record_matches_template(cp, alternate, &published.id)).unwrap();
    let published_bytes = std::fs::read(&fixture.handoff.run_path).unwrap();
    let writer = declared_provider_run_writer(&published, &"provider".into(), &profile).unwrap();
    let retirement = prepare_definition_edit(fixture.handoff.run_path.clone(), &published_bytes, &writer, &[pinned.id.clone()], None).unwrap();
    let retired = RunRecord::from_bytes(retirement.content.as_bytes()).unwrap();
    assert!(retired.checkpoint(&pinned.id).is_none());
    assert_eq!(provider_context_bindings(&retired, &"provider".into()).unwrap(), provider_context_bindings(&published, &"provider".into()).unwrap());
    assert_eq!(persisted_pending_handoff(&retired, &"provider".into()).unwrap(), persisted_pending_handoff(&published, &"provider".into()).unwrap());
    result_inputs::validate_saved(&fixture.handoff.ctx, &published, &retired, &fixture.handoff.config).unwrap();
    let future = provider_context_bindings(&published, &"provider".into()).unwrap();
    let revision = prepare_definition_edit(fixture.handoff.run_path.clone(), &published_bytes, &writer,
        &[pinned.id.clone()], Some(&future)).unwrap();
    let revised = RunRecord::from_bytes(revision.content.as_bytes()).unwrap();
    let revised_config = attempt_config(&revised, &fixture.handoff.config).unwrap();
    assert_eq!(revised_config.checkpoints.iter().find(|cp|cp.name == receipt.name).unwrap().static_artifact_bindings, [catalogue.id.clone()]);
    assert!(revised.checkpoint(&pinned.id).is_none());
    let receiver = revised.checkpoints.iter().find(|cp| checkpoint_record_matches_template(cp, &receipt, &revised.id)).unwrap();
    assert_eq!(receiver.artifact_bindings, future);
    assert_eq!(revised.checkpoint(&"control".into()), published.checkpoint(&"control".into()));
    result_inputs::validate_saved(&fixture.handoff.ctx, &published, &revised, &fixture.handoff.config).unwrap();
    let mut wrong_rights = future.clone();
    wrong_rights[0].access.delete = true;
    assert!(prepare_definition_edit(fixture.handoff.run_path.clone(), &published_bytes, &writer,
        &[pinned.id.clone()], Some(&wrong_rights)).is_err());
    assert_eq!(std::fs::read(&fixture.handoff.run_path).unwrap(), published_bytes);
    fixture.edge(&published_bytes, &next_input.id, &input_file, "provider".into(), MovementKind::InterfaceOutbound);
    fixture.handoff.source_movement_seq = fixture.rows.len() as u64 - 1;
    fixture.handoff.invocation_metadata = fixture.rows.last().unwrap().invocation_metadata.as_ref().unwrap().invocation().unwrap().clone();
    let second = fixture.output(false, true, true);
    let record = fixture.reload_definition();
    assert_eq!(record.checkpoint(&pinned.id), Some(&pinned));
    let receipts = fixture.handoff.config.checkpoints.iter().filter(|cp| cp.role == RunAuthoringCheckpointRole::ResultInput).collect::<Vec<_>>();
    assert_eq!(receipts.len(), 2);
    assert_eq!(receipts[0].interface(), receipts[1].interface());
    let received = receipts.iter().find(|cp| **cp != &receipt).unwrap();
    let received = (**received).clone();
    let receiving = record.checkpoints.iter().find(|cp| checkpoint_record_matches_template(cp, &received, &record.id)).unwrap().clone();
    assert!(!provider_context_bindings(&record, &"provider".into()).unwrap().iter()
        .any(|binding| binding.id == second.pending_handoff.items[0].artifact_id));
    let call = load_pending_artifact(&fixture.handoff.var_dir, &second.pending_handoff.items[0]).unwrap();
    let correlation = received.required_verified_artifacts[0].params["correlation_id"].as_str().unwrap();
    let result = InterfaceArtifactFile::write(fixture.handoff.var_dir.join("received"), "read.result",
        &serde_json::to_vec(&serde_json::json!({"label":"read.result","tool":"read","correlation_id":correlation,"ok":true,"body":{}})).unwrap()).unwrap();
    let envelope = InterfaceInvocationEnvelope { artifact:artifact_identity(&result, &fixture.handoff.var_dir).unwrap().artifact,
        artifact_file:result.clone(), artifact_binding_payloads:vec![octopus_interfaces::ArtifactBindingPayload::new(
            call.1, call.0.read_payload_bytes().unwrap())], metadata:None };
    result_inputs::validate_invocation(&envelope, &received).unwrap();
    assert!(result_inputs::validate_invocation(&envelope, &receipt).is_err());
    let mut wrong = envelope.clone();
    wrong.artifact_file = InterfaceArtifactFile::write(fixture.handoff.var_dir.join("wrong-result"), "read.result",
        br#"{"label":"read.result","tool":"read","correlation_id":"other-call","ok":true,"body":{}}"#).unwrap();
    assert!(result_inputs::validate_invocation(&wrong, &received).is_err());
    wrong.artifact_file = InterfaceArtifactFile::write(fixture.handoff.var_dir.join("failed-result"), "read.result",
        &serde_json::to_vec(&serde_json::json!({"label":"read.result","tool":"read","correlation_id":correlation,"ok":false,
            "error":{"code":"read_failed","message":"read failed"}})).unwrap()).unwrap();
    result_inputs::validate_invocation(&wrong, &received).unwrap();
    fixture.publish(&second);
    let published = fixture.reload_definition();
    let prior = std::fs::read(&fixture.handoff.run_path).unwrap();
    let input_b = InterfaceArtifactFile::write(fixture.handoff.var_dir.join("independent-b"), "operator.input",
        br#"{"text":"independent future B"}"#).unwrap();
    let alternate = fixture.handoff.config.checkpoints.iter().find(|cp| cp.name == "alternate").unwrap();
    let input_cp = published.checkpoints.iter().find(|cp| checkpoint_record_matches_template(cp, alternate, &published.id)).unwrap();
    fixture.edge(&prior, &input_cp.id, &input_b, "provider".into(), MovementKind::InterfaceOutbound);
    fixture.handoff.source_movement_seq = fixture.rows.len() as u64 - 1;
    fixture.handoff.invocation_metadata = fixture.rows.last().unwrap().invocation_metadata.as_ref().unwrap().invocation().unwrap().clone();
    let b = artifact_identity(&input_b, &fixture.handoff.var_dir).unwrap().artifact;
    let reply_b = InterfaceArtifactFile::write(fixture.handoff.var_dir.join("reply-b"), "reply", br#"{"text":"B received"}"#).unwrap();
    let mut future = current_context_artifacts(&published, &fixture.handoff.config).unwrap();
    append_artifact(&mut future, b.id.clone());
    let authored_b = fixture.handoff.author_run_with_bindings(&prior, &future, &[PreparedHandoff {
        identity:artifact_identity(&reply_b, &fixture.handoff.var_dir).unwrap(),
        output_checkpoint:fixture.handoff.config.checkpoint_for_role(RunAuthoringCheckpointRole::ProviderArtifactOutput).unwrap().clone(),
        verified_inputs:Vec::new(),
    }], true, None, false, true, &[]).unwrap();
    fixture.install(&authored_b);
    fixture.edge(authored_b.content.as_bytes(), &authored_b.pending_handoff.items[0].checkpoint.id, &reply_b,
        "worker".into(), MovementKind::InterfaceArrival);
    assert!(authored_b.pending_handoff.context_publication.is_none());
    let record = fixture.reload_definition();
    let current = std::fs::read(&fixture.handoff.run_path).unwrap();
    assert_eq!(record.checkpoint(&receiving.id), Some(&receiving));
    assert!(!receiving.artifact_bindings.iter().any(|binding| binding.id == b.id));
    assert!(provider_context_bindings(&record, &"provider".into()).unwrap().iter().any(|binding| binding.id == b.id));
    let source = fixture.rows.len() as u64;
    fixture.edge(&current, &receiving.id, &result, "provider".into(), MovementKind::InterfaceOutbound);
    fixture.handoff.source_movement_seq = source;
    fixture.handoff.invocation_metadata = fixture.rows.last().unwrap().invocation_metadata.as_ref().unwrap().invocation().unwrap().clone();
    {
        let payloads = fixture.handoff.invocation_metadata.artifact_binding_refs.iter().map(|reference| {
            let (_, payload) = read_pinned_artifact_from_cache(&fixture.handoff.ctx.blobs_dir, fixture.handoff.ctx.required_host_io_limits().unwrap(), reference).unwrap();
            octopus_interfaces::ArtifactBindingPayload::new(reference.clone(), payload)
        }).collect::<Vec<_>>();
        let projected = context_artifact_payloads(&record, &fixture.handoff.invocation_metadata, &payloads).unwrap();
        assert!(!projected.iter().any(|bound|bound.artifact.id == second.pending_handoff.items[0].artifact_id));
        let mut denied = fixture.handoff.invocation_metadata.clone();
        denied.grant.artifact_access.get_mut(second.pending_handoff.items[0].artifact_id.as_str()).unwrap().change = true;
        assert!(context_artifact_payloads(&record, &denied, &payloads).unwrap_err().contains("read-only call evidence"));
        let mut state = fixture.handoff.state.try_lock().unwrap().clone();
        append_session_payload(&mut state.session_context, "read.result", &result.read_payload_bytes().unwrap(),
            &envelope.artifact, Some(source), "exact result input").unwrap();
        let captured_provider = state.session_context.provider_interface.clone();
        let captured_artifacts = serde_json::to_value(&state.session_context.artifacts).unwrap();
        fixture.handoff.restore_checked_context(&mut state, &record, &[]).unwrap();
        assert_eq!(state.session_context.provider_interface, captured_provider);
        assert_eq!(serde_json::to_value(&state.session_context.artifacts).unwrap(), captured_artifacts);
        let mut expected = current_context_artifacts(&record, &fixture.handoff.config).unwrap();
        append_artifact(&mut expected, envelope.artifact.id.clone());
        assert_eq!(state.context_artifacts, expected);
        fixture.handoff.restore_pending_context(&mut state, &record, &second.pending_handoff).unwrap();
        assert_eq!(state.context_artifacts, expected);
        assert_eq!(state.session_context.provider_interface, captured_provider);
        assert_eq!(serde_json::to_value(&state.session_context.artifacts).unwrap(), captured_artifacts);
        assert!(result_inputs::validate_snapshot_context(&fixture.handoff.config, &state.context_artifacts, &[], &state.session_context).is_err());
        let future = state.context_artifacts.clone();
        assert!(result_inputs::validate_snapshot_context(&fixture.handoff.config, &future, &future, &state.session_context).is_ok());
    }
    let answer = InterfaceArtifactFile::write(fixture.handoff.var_dir.join("answer"), "reply", br#"{"text":"done"}"#).unwrap();
    let captured = fixture.handoff.state.try_lock().unwrap().session_context.clone();
    let fork = materialize_session_snapshot(&answer, &captured, None).unwrap();
    let fork_identity = artifact_identity(&fork, &fixture.handoff.var_dir).unwrap();
    let forked = fixture.handoff.author_run_with_bindings(&current, &[fork_identity.artifact.id.clone()], &[PreparedHandoff {
        identity:fork_identity,
        output_checkpoint:fixture.handoff.config.checkpoint_for_role(RunAuthoringCheckpointRole::ProviderArtifactOutput).unwrap().clone(),
        verified_inputs:Vec::new(),
    }], true, None, false, true, &[]).unwrap();
    assert!(result_inputs::validate_saved(&fixture.handoff.ctx, &record,
        &RunRecord::from_bytes(forked.content.as_bytes()).unwrap(), &fixture.handoff.config).is_err());
    let bridge = fixture.handoff.author_result_invocation(&current).unwrap();
    {
        let mut peer = record.clone();
        peer.interface_state.insert("peer".into(), BTreeMap::from([("input_seq".into(), source.into())]));
        peer.checkpoints.iter_mut().find(|cp| cp.id.as_str() == "control").unwrap().epoch.0 += 1;
        let peer_bytes = serde_json::to_vec(&peer).unwrap();
        let preimage = fixture.handoff.authored_run_preimage(&peer_bytes, &bridge).unwrap();
        assert_eq!(preimage, current);
        let rebased = authoring_retry::rebase_authored_run(&preimage, &peer_bytes, &bridge, &fixture.handoff.config, Some(JsonFormat::Compact)).unwrap();
        let after = RunRecord::from_bytes(rebased.content.as_bytes()).unwrap();
        assert_eq!(after.interface_state[&InterfaceId::new("peer")], peer.interface_state[&InterfaceId::new("peer")]);
        assert_eq!(after.checkpoint(&"control".into()), peer.checkpoint(&"control".into()));
        assert_eq!(result_inputs::definition_source(&peer, &after, &fixture.handoff.config).unwrap(), Some(source));
        result_inputs::validate_saved(&fixture.handoff.ctx, &peer, &after, &fixture.handoff.config).unwrap();
        let mut forged = bridge.clone();
        forged.content.push(' ');
        assert!(fixture.handoff.authored_run_preimage(&peer_bytes, &forged).is_err());
        peer.checkpoints.iter_mut().find(|cp| cp.id == receiving.id).unwrap().artifact_bindings[0].access.delete = true;
        assert!(authoring_retry::rebase_authored_run(&preimage, &serde_json::to_vec(&peer).unwrap(), &bridge, &fixture.handoff.config, Some(JsonFormat::Compact)).is_err());
    }
    let raw_handoff = fixture.handoff.clone();
    let bridged = RunRecord::from_bytes(bridge.content.as_bytes()).unwrap();
    assert!(bridged.checkpoint(&receiving.id).is_none());
    assert_eq!(bridged.checkpoint(&pinned.id), Some(&pinned));
    assert_eq!(bridged.checkpoint(&"control".into()), record.checkpoint(&"control".into()));
    assert_eq!(provider_context_bindings(&bridged, &"provider".into()).unwrap(), provider_context_bindings(&record, &"provider".into()).unwrap());
    assert_eq!(persisted_pending_handoff(&bridged, &"provider".into()).unwrap(), persisted_pending_handoff(&record, &"provider".into()).unwrap());
    fixture.handoff.materialize_file_write_call(&bridge).unwrap();
    let AuthoringAttemptRecovery::InFlight(saved) = fixture.handoff.recover_authoring_attempt(&record, &current).unwrap()
        else { panic!("lost exact result invocation attempt"); };
    assert_eq!(saved.provider_source_movement_seq, source);
    assert_eq!(saved.authored, bridge);
    assert_eq!(result_inputs::definition_source(&record, &bridged, &fixture.handoff.config).unwrap(), Some(source));
    {
        let mut cold = fixture.handoff.clone();
        let mut detached:serde_json::Value = serde_json::from_slice(&current).unwrap();
        let writer_run = RunId::new("result_bridge_writer");
        let writer = separate_writer_run(&mut detached, &cold.ctx.interface, &writer_run).unwrap();
        let cold_root = cold.var_dir.join("bridge-saved-target");
        std::fs::create_dir_all(&cold_root).unwrap();
        cold.run_path = cold_root.join("run.json");
        let detached = JsonFormat::Compact.encode(&detached).unwrap();
        std::fs::write(&cold.run_path, &detached).unwrap();
        std::fs::write(cold_root.join(format!("{writer_run}.json")), JsonFormat::Compact.encode(&writer).unwrap()).unwrap();
        let target = RunRecord::from_bytes(&detached).unwrap();
        cold.config = attempt_config(&target, &cold.config).unwrap();
        cold.bound_topology = serde_json::from_value(target.interface_state[&cold.ctx.interface][RUN_AUTHORING_BINDING].clone()).unwrap();
        cold.hand_in_root = cold.var_dir.join("bridge-saved-calls");
        cold.ctx.root_slots.insert("run_records_root".into(), cold_root);
        cold.ctx.ledger_index_path = cold.var_dir.join("bridge-saved.sqlite");
        cold.session = ProviderHandoffSession::new(&cold.ctx).unwrap();
        let ordered = cold.author_result_invocation(&detached).unwrap();
        let generation = correlation_generation(&target.id, &ordered.correlation_id).unwrap();
        let edit = CheckedRunEdit { path:cold.run_path.clone(), json_format:ordered.json_format,
            before_hash:ordered.before_hash.clone(), after_hash:ordered.after_hash.clone(), content:ordered.content.clone(),
            patch:ordered.patch.clone(), correlation_id:format!("provider-writer-run:run:{generation}:1") };
        let writer = declared_provider_run_writer(&target, &cold.ctx.interface, &profile).unwrap();
        OwnerWriterEdit::prepare(&detached, &edit, &writer, 1<<26).unwrap().unwrap().materialize(&cold.hand_in_root).unwrap();
        let AuthoringAttemptRecovery::InFlight(saved) = cold.recover_authoring_attempt(&target, &detached).unwrap()
            else { panic!("lost detached bridge source"); };
        assert_eq!(saved.provider_source_movement_seq, source);
        assert_eq!(saved.authored.content, ordered.content);
        assert_eq!(std::fs::read(&cold.run_path).unwrap(), detached);
    }
    let bridge_cutoff = fixture.rows.len();
    fixture.install(&bridge);
    let transition = CheckedRunTransition { writer:declared_provider_run_writer(&record, &"provider".into(), &profile).unwrap(),
        before:current.clone(), after:bridge.content.as_bytes().to_vec(), call:fixture.rows[bridge_cutoff].clone(), result:fixture.rows[bridge_cutoff+1].clone(),
        placement: false, };
    validate_checked_provider_transition(&transition).unwrap();
    {
        let evidence = CheckedRunEvidence { movements_path:&fixture.handoff.ctx.movements_path, ledger_index_path:&fixture.handoff.ctx.ledger_index_path,
            blobs_dir:&fixture.handoff.ctx.blobs_dir, max_control_frame_bytes:1<<20, max_manifest_bytes:1<<20, max_payload_bytes:1<<26 };
        result_inputs::validate_checked(&mut FileCheckedRunReader::open(&evidence, None).unwrap().with_provider_context(&fixture.handoff.ctx), &transition).unwrap();
    }
    let record = fixture.reload_definition();
    let next = fixture.handoff.config.checkpoints.iter().find(|template| template.role == RunAuthoringCheckpointRole::ResultInput
        && template.required_verified_artifacts[0].params.get("invocation").is_some()).unwrap().clone();
    let next_cp = record.checkpoints.iter().find(|cp| checkpoint_record_matches_template(cp, &next, &record.id)).unwrap().clone();
    let path = next.required_verified_artifacts[0].params["invocation"]["manifest_relative_path"].as_str().unwrap();
    let copied = InterfaceArtifactFile::new(fixture.handoff.var_dir.join(path)).unwrap();
    assert_eq!(copied.read_payload_bytes().unwrap(), result.read_payload_bytes().unwrap());
    let source = fixture.rows.len() as u64;
    fixture.edge(bridge.content.as_bytes(), &next_cp.id, &copied, "provider".into(), MovementKind::InterfaceOutbound);
    fixture.handoff.source_movement_seq = source;
    fixture.handoff.invocation_metadata = fixture.rows.last().unwrap().invocation_metadata.as_ref().unwrap().invocation().unwrap().clone();
    let bindings = fixture.handoff.invocation_metadata.artifact_binding_refs.iter().map(|reference| {
        let (_, payload) = read_pinned_artifact_from_cache(&fixture.handoff.ctx.blobs_dir, fixture.handoff.ctx.required_host_io_limits().unwrap(), reference).unwrap();
        octopus_interfaces::ArtifactBindingPayload::new(reference.clone(), payload)
    }).collect::<Vec<_>>();
    let envelope = InterfaceInvocationEnvelope { artifact:artifact_identity(&copied, &fixture.handoff.var_dir).unwrap().artifact,
        artifact_file:copied, artifact_binding_payloads:bindings, metadata:Some(fixture.handoff.invocation_metadata.clone()) };
    result_inputs::validate_invocation(&envelope, &next).unwrap();
    let bridged_recovery = raw_handoff.recover_source_with_outcomes().await.unwrap();
    assert_eq!(bridged_recovery, (ProviderSourceRecovery::Bridged { invocation_movement_seq:source },
        vec![ProviderHandoffOutcome::Admitted { seq:source, artifact:envelope.artifact.clone() }]));
    assert_eq!(fixture.handoff.recover_source().await.unwrap(), ProviderSourceRecovery::Process);
    let model = context_artifact_payloads(&record, &fixture.handoff.invocation_metadata, &envelope.artifact_binding_payloads).unwrap();
    assert_eq!(next.static_artifact_bindings, [catalogue.id.clone()]);
    assert!(model.iter().any(|bound|bound.artifact.id == second.pending_handoff.items[0].artifact_id));
    assert!(model.iter().any(|bound| bound.artifact == b));
    assert!(!model.iter().any(|bound| bound.artifact == wrong.artifact));
    assert_eq!(next_cp.artifact_bindings.iter().filter(|binding| binding.id == wrong.artifact.id).count(), 1);
    let mut wrong_envelope = envelope.clone();
    wrong_envelope.artifact = wrong.artifact;
    assert!(result_inputs::validate_invocation(&wrong_envelope, &next).is_err());
    let payload = envelope.artifact_file.read_manifest().unwrap().payload_path;
    std::fs::remove_file(envelope.artifact_file.manifest_path()).unwrap();
    std::fs::remove_file(payload).unwrap();
    let answer = InterfaceArtifactFile::write(fixture.handoff.var_dir.join("answer-without-fork"), "reply", br#"{"text":"done"}"#).unwrap();
    let mut context = provider_context_bindings(&record, &"provider".into()).unwrap().into_iter().map(|binding| binding.id).collect::<Vec<_>>();
    context.push(envelope.artifact.id.clone());
    let authored = fixture.handoff.author_run_with_bindings(bridge.content.as_bytes(), &context, &[PreparedHandoff {
        identity:artifact_identity(&answer, &fixture.handoff.var_dir).unwrap(),
        output_checkpoint:fixture.handoff.config.checkpoint_for_role(RunAuthoringCheckpointRole::ProviderArtifactOutput).unwrap().clone(),
        verified_inputs:Vec::new(),
    }], true, None, false, true, &[]).unwrap();
    let after = RunRecord::from_bytes(authored.content.as_bytes()).unwrap();
    result_inputs::validate_saved(&fixture.handoff.ctx, &record, &after, &fixture.handoff.config).unwrap();
    let tool = fixture.handoff.config.checkpoint_for_role(RunAuthoringCheckpointRole::ProviderToolOutput).unwrap();
    let retained = after.checkpoints.iter().filter(|row| after.active_checkpoints.contains(&row.id)
        && checkpoint_record_matches_template(row, tool, &after.id)).collect::<Vec<_>>();
    assert_eq!(retained.len(), 1);
    let mut lost_route = after.clone();
    lost_route.checkpoints.iter_mut().find(|row| row.id == retained[0].id).unwrap().required_verified_artifacts[0].params =
        serde_json::json!({"tools":["ungranted"]});
    assert!(validate_bound_run_authoring_topology(&lost_route, &fixture.handoff.config, None).is_err());
    assert_eq!(after.checkpoint(&pinned.id), Some(&pinned));
    assert!(after.checkpoint(&next_cp.id).is_none());
    let mut forged = after.clone();
    forged.interface_state.get_mut(&InterfaceId::new("provider")).unwrap().get_mut("pending_handoff").unwrap()["source_movement_seq"] = 0.into();
    assert!(result_inputs::validate_saved(&fixture.handoff.ctx, &record, &forged, &fixture.handoff.config).is_err());
    forged = record.clone();
    forged.checkpoints.iter_mut().find(|cp| cp.id == pinned.id).unwrap().artifact_bindings[0].access.delete = true;
    assert!(result_inputs::validate_transition(&record, &forged, &fixture.handoff.config).is_err());
    let cutoff = fixture.rows.len() as u64;
    fixture.install(&authored);
    let evidence = CheckedRunEvidence { movements_path:&fixture.handoff.ctx.movements_path, ledger_index_path:&fixture.handoff.ctx.ledger_index_path,
        blobs_dir:&fixture.handoff.ctx.blobs_dir, max_control_frame_bytes:1<<20, max_manifest_bytes:1<<20, max_payload_bytes:1<<26 };
    let writer = declared_provider_run_writer(&record, &"provider".into(), &RunAuthoringProfile {
        file_write_tool:"write".into(),file_write_label:"tool.call".into(),file_write_result_label:"write.result".into(),result_timeout_ms:1000,
    }).unwrap();
    let transition = checked_declared_run_install(&evidence, &fixture.handoff.run_path, bridge.content.as_bytes(), &CheckedRunInstall {
        call:ConsentEvidenceRef { artifact:fixture.rows[cutoff as usize].artifact.clone().unwrap(), movement_seq:cutoff },
        result:ConsentEvidenceRef { artifact:fixture.rows[cutoff as usize + 1].artifact.clone().unwrap(), movement_seq:cutoff + 1 },
    }, vec![writer]).unwrap();
    let mut reader = FileCheckedRunReader::open(&evidence, None).unwrap();
    result_inputs::validate_checked(&mut reader, &transition).unwrap();
    assert_eq!(raw_handoff.recover_source_with_outcomes().await.unwrap(), bridged_recovery);
}

#[tokio::test]
async fn completed_source_settlement_requires_exact_predecessor_and_replays_once() {
    for mode in ["valid", "wrong_predecessor", "nonterminal", "wrong_source", "effect", "wrong_generation", "unadmitted"] {
        let mut fixture = CompletedInputReplayFixture::new();
        let first = fixture.output(mode != "nonterminal", mode != "unadmitted", true);
        if mode != "unadmitted" { fixture.publish(&first); }
        let before = std::fs::read(&fixture.handoff.run_path).unwrap();
        let record = RunRecord::from_bytes(&before).unwrap();
        let mut predecessor = load_pending_artifact(&fixture.handoff.var_dir, &first.pending_handoff.items[0]).unwrap().1;
        if mode == "wrong_predecessor" { predecessor.content = ContentHash::of_bytes(b"different receipt"); }
        if mode == "wrong_source" { fixture.handoff.source_movement_seq = 1; }
        let file = InterfaceArtifactFile::write(fixture.handoff.var_dir.join("settlement"), if mode == "effect" { "tool.call" } else { "context.snapshot" },
            br#"{"label":"context.snapshot","receipt":"settled"}"#).unwrap();
        let identity = artifact_identity(&file, &fixture.handoff.var_dir).unwrap();
        let mut context = current_context_artifacts(&record, &fixture.handoff.config).unwrap();
        context.retain(|id| id != &first.pending_handoff.items[0].artifact_id);
        append_artifact(&mut context, identity.artifact.id.clone());
        let mut authored = fixture.handoff.author_run_with_bindings(&before, &context, &[PreparedHandoff {
            identity, output_checkpoint:fixture.handoff.config.checkpoint_for_role(if mode == "effect" {
                RunAuthoringCheckpointRole::ProviderToolOutput
            } else { RunAuthoringCheckpointRole::ProviderArtifactOutput }).unwrap().clone(), verified_inputs:Vec::new(),
        }], mode != "wrong_generation", None, false, true, &[]).unwrap();
        let result = fixture.handoff.attach_source_settlement(&before, &mut authored, &predecessor,
            optional_u64(&record.interface_state[&fixture.handoff.config.provider_interface], "context_generation").unwrap());
        if mode != "valid" { assert!(result.is_err(), "{mode}"); continue; }
        result.unwrap();
        let mut missing = RunRecord::from_bytes(authored.content.as_bytes()).unwrap();
        missing.interface_state.get_mut(&fixture.handoff.config.provider_interface).unwrap().get_mut("pending_handoff").unwrap()["settlement_of"]["content"] = serde_json::json!(ContentHash::of_bytes(b"wrong"));
        assert!(source_settlement::validate_transition(&record, &missing, &fixture.handoff.config).is_err());
        let mut widened = RunRecord::from_bytes(authored.content.as_bytes()).unwrap();
        let cp = widened.checkpoints.iter_mut().find(|cp|cp.inbound_interface()==Some(&InterfaceId::new("room"))).unwrap();
        cp.artifact_bindings[0].access.change = true;
        assert!(source_settlement::validate_transition(&record, &widened, &fixture.handoff.config).is_err());
        fixture.install(&authored);
        let closure = artifact_identity(&file, &fixture.handoff.var_dir).unwrap().artifact;
        fixture.handoff.state.lock().await.run_record_bytes = authored.content.as_bytes().to_vec();
        assert!(fixture.handoff.completed_source_settlement_predecessor(&closure).await.is_err());
        fixture.edge(authored.content.as_bytes(), &authored.pending_handoff.items[0].checkpoint.id, &file, "worker".into(), MovementKind::InterfaceArrival);
        assert!(fixture.handoff.completed_source_settlement_predecessor(&closure).await.is_err());
        let publication = fixture.handoff.author_context_publication(authored.content.as_bytes(), &authored.pending_handoff).unwrap();
        let installed = RunRecord::from_bytes(authored.content.as_bytes()).unwrap();
        let mut changed = RunRecord::from_bytes(publication.content.as_bytes()).unwrap();
        changed.checkpoints.iter_mut().find(|cp|cp.inbound_interface()==Some(&InterfaceId::new("control"))).unwrap().epoch.0 += 1;
        assert!(source_settlement::validate_transition(&installed, &changed, &fixture.handoff.config).is_err());
        let mut changed = RunRecord::from_bytes(publication.content.as_bytes()).unwrap();
        changed.interface_state.get_mut(&fixture.handoff.config.provider_interface).unwrap().get_mut("pending_handoff").unwrap()["source_without_context"] = serde_json::json!(true);
        assert!(source_settlement::validate_transition(&installed, &changed, &fixture.handoff.config).is_err());
        fixture.publish(&authored);
        fixture.handoff.source_movement_seq = 1;
        fixture.handoff.state.lock().await.run_record_bytes = std::fs::read(&fixture.handoff.run_path).unwrap();
        fixture.handoff.session = ProviderHandoffSession::new(&fixture.handoff.ctx).unwrap();
        assert_eq!(fixture.handoff.completed_input_artifact_source(&fixture.input).await.unwrap(), Some(0));
        let bytes = std::fs::read(&fixture.handoff.run_path).unwrap();
        let ledger = std::fs::read(&fixture.handoff.ctx.movements_path).unwrap();
        assert_eq!(fixture.handoff.completed_input_artifact_source(&fixture.input).await.unwrap(), Some(0));
        assert_eq!(std::fs::read(&fixture.handoff.run_path).unwrap(), bytes);
        assert_eq!(std::fs::read(&fixture.handoff.ctx.movements_path).unwrap(), ledger);
        fixture.handoff.source_movement_seq = 0;
        let proof = fixture.handoff.completed_source_settlement_predecessor(&closure).await.unwrap();
        assert_eq!(proof.0, before);
        assert!(matches!(proof.1.as_slice(), [ProviderHandoffOutcome::Admitted {artifact,..}] if artifact == &predecessor));
        let next = fixture.rows.len() as u64;
        fixture.edge(&bytes, &"control".into(), &fixture.input_file.clone(), "provider".into(), MovementKind::InterfaceOutbound);
        fixture.handoff.source_movement_seq = next;
        let later = fixture.output(true, true, true); fixture.publish(&later);
        fixture.handoff.source_movement_seq = 0;
        let bytes = std::fs::read(&fixture.handoff.run_path).unwrap();
        let ledger = std::fs::read(&fixture.handoff.ctx.movements_path).unwrap();
        fixture.handoff.state.lock().await.run_record_bytes = bytes.clone();
        assert_eq!(fixture.handoff.completed_source_settlement_predecessor(&closure).await.unwrap(), proof);
        let mut wrong = closure.clone(); wrong.content = ContentHash::of_bytes(b"different closure");
        assert!(fixture.handoff.completed_source_settlement_predecessor(&wrong).await.is_err());
        assert_eq!(std::fs::read(&fixture.handoff.run_path).unwrap(), bytes);
        assert_eq!(std::fs::read(&fixture.handoff.ctx.movements_path).unwrap(), ledger);
    }
}

#[test]
fn payload_readiness_proves_completed_newer_inputs() {
    for checkpoint in ["control", "alternate"] {
    for mode in ["completed", "unprocessed", "nonterminal", "unadmitted", "unpublished", "late_admission", "late_mark", "wrong_artifact", "extra_input", "changed_current"] {
        let mut fixture = CompletedInputReplayFixture::new();
        fixture.rows.pop(); fixture.save();
        let initial = std::fs::read(&fixture.handoff.run_path).unwrap();
        let control = InterfaceArtifactFile::write(fixture.handoff.var_dir.join("independent"), "arbitrary.independent.input", br#"{"value":1}"#).unwrap();
        fixture.edge(&initial,&CheckpointId::new(checkpoint),&control,"provider".into(),MovementKind::InterfaceOutbound);
        fixture.handoff.source_movement_seq = 1;
        let independent = if mode == "unprocessed" { None } else {
            let output = fixture.output(mode != "nonterminal", !["unadmitted","late_admission"].contains(&mode), mode != "late_mark");
            if mode != "unpublished" { fixture.publish(&output); }
            Some(output)
        };
        fixture.handoff.source_movement_seq = 0;
        let last = fixture.output(true,true,true); fixture.publish(&last);
        if mode == "late_admission" {
            let output = independent.as_ref().unwrap();
            let file = load_pending_artifact(&fixture.handoff.var_dir,&output.pending_handoff.items[0]).unwrap().0;
            fixture.edge(output.content.as_bytes(),&output.pending_handoff.items[0].checkpoint.id,&file,"worker".into(),MovementKind::InterfaceArrival);
            fixture.mark(output);
        }
        if mode == "late_mark" { fixture.mark(independent.as_ref().unwrap()); }
        if mode == "wrong_artifact" {
            let id = &independent.as_ref().unwrap().pending_handoff.items[0].artifact_id;
            fixture.rows.iter_mut().find(|row|row.kind==MovementKind::InterfaceArrival && row.artifact.as_ref().is_some_and(|reference|&reference.id==id)).unwrap().artifact = Some(fixture.input.clone());
            fixture.save();
        }
        let mut current = std::fs::read(&fixture.handoff.run_path).unwrap();
        if mode == "extra_input" {
            let record = RunRecord::from_bytes(&current).unwrap();
            let cp = record.checkpoints.iter().find(|cp|record.active_checkpoints.contains(&cp.id) && cp.inbound_interface()==Some(&InterfaceId::new("room"))).unwrap().id.clone();
            fixture.edge(&current,&cp,&fixture.input_file.clone(),"provider".into(),MovementKind::InterfaceOutbound);
        }
        if mode == "changed_current" {
            let mut value:serde_json::Value = serde_json::from_slice(&current).unwrap();
            value["interface_state"]["provider"]["context_generation"] = serde_json::json!(999);
            current = serde_json::to_vec(&value).unwrap();
        }
        let h = &fixture.handoff;
        let writer = CheckedRunWriter {owner:h.config.provider_interface.clone(),source:h.config.authoring_source_interface.clone(),
            door:h.config.checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringResult).unwrap().interface().clone(),
            tool:h.config.file_write_tool.clone(),call_label:h.config.file_write_label.clone(),result_label:h.config.file_write_result_label.clone()};
        let evidence = CheckedRunEvidence {movements_path:&h.ctx.movements_path,ledger_index_path:&h.ctx.ledger_index_path,
            blobs_dir:&h.ctx.blobs_dir,max_control_frame_bytes:1_048_576,max_manifest_bytes:1_048_576,max_payload_bytes:16_777_216};
        let cutoff = fixture.rows.len() as u64;
        let file = provider_input_payloads_ready(&evidence,&h.run_path,&current,&writer,&InterfaceId::new("room"));
        let mut reader = FileCheckedRunReader::open(&evidence,None).unwrap();
        let generic = input_revision::reader_payload_readiness(&mut reader,&h.run_path,&current,&writer,&InterfaceId::new("room"),cutoff);
        assert_eq!(file.as_ref().ok(),generic.as_ref().ok(),"{mode}: file={file:?}, generic={generic:?}");
        assert_eq!(file.as_ref().is_ok_and(|ready|*ready),mode=="completed","{checkpoint}/{mode}: {file:?}");
        let selection = InterfaceArtifactFile::write(h.var_dir.join("selection"),"context.reference",br#"{"value":"selected"}"#).unwrap();
        let selected = prepare_provider_input_payloads(h.run_path.clone(),&current,&writer,InterfaceId::new("room"),
            vec![artifact_identity(&selection,&h.var_dir).unwrap().artifact]).unwrap();
        let mut call = fixture.rows.last().unwrap().clone(); call.seq = octopus_ledger::Seq(cutoff);
        let transition = CheckedRunTransition {writer:writer.clone(),before:current.clone(),after:selected.content.into_bytes(),call:call.clone(),result:call,placement:false};
        let mut reader = FileCheckedRunReader::open(&evidence,None).unwrap();
        assert_eq!(input_revision::validate_rearm_evidence(&mut reader,&transition,&h.run_path).is_ok(),mode=="completed","historical {mode}");
        if mode == "completed" {
            let mut reader = FileCheckedRunReader::open(&evidence,None).unwrap();
            assert!(!input_revision::reader_payload_readiness(&mut reader,&h.run_path,&current,&writer,&InterfaceId::new("room"),1).unwrap());
        }
    }
    }
}

#[test]
#[ignore = "requires an explicitly selected stopped Home after exact source settlement"]
fn selected_home_payload_readiness_proves_consumed_independent_input() {
    let home = PathBuf::from(std::env::var("OCTOPUS_REPLAY_HOME").unwrap());
    let provider = InterfaceId::new(std::env::var("OCTOPUS_REPLAY_PROVIDER").unwrap());
    let run = RunId::new(std::env::var("OCTOPUS_REPLAY_RUN").unwrap());
    let source = InterfaceId::new(std::env::var("OCTOPUS_REPLAY_CLIENT").unwrap());
    let path = home.join("state/runs").join(format!("{run}.json"));
    let bytes = std::fs::read(&path).unwrap();let record = RunRecord::from_bytes(&bytes).unwrap();
    let descriptor:InterfaceDescriptor = serde_json::from_slice(&std::fs::read(home.join("registry/interface").join(provider.as_str()).join("interface.json")).unwrap()).unwrap();
    let writer = declared_provider_run_writer(&record,&provider,&run_authoring_profile(&descriptor).unwrap()).unwrap();
    let movements = home.join("ledger/movements.jsonl");let ledger = std::fs::read(&movements).unwrap();
    let blobs = home.join("ledger/blobs");let scratch = tempfile::tempdir().unwrap();let index = scratch.path().join("ledger-index.sqlite");
    let scratch_ledger = scratch.path().join("movements.jsonl");std::fs::write(&scratch_ledger,&ledger).unwrap();
    std::os::unix::fs::symlink(&blobs,scratch.path().join("blobs")).unwrap();
    std::os::unix::fs::symlink(home.join("ledger/metadata"),scratch.path().join("metadata")).unwrap();
    let log = octopus_ledger_store::DurableLog::open(&scratch_ledger).unwrap();
    let _index = octopus_ledger_index::LedgerSqliteIndex::open_healthy(&index,&log).unwrap();
    let evidence = CheckedRunEvidence {movements_path:&movements,ledger_index_path:&index,blobs_dir:&blobs,
        max_control_frame_bytes:1_048_576,max_manifest_bytes:1_048_576,max_payload_bytes:268_435_456};
    assert!(provider_input_payloads_ready(&evidence,&path,&bytes,&writer,&source).unwrap());
    eprintln!("selected Home: direct payload readiness passed against the exact current record");
    let mut reader = FileCheckedRunReader::open(&evidence,None).unwrap();
    let cutoff = ledger.split(|byte|*byte==b'\n').filter(|row|!row.is_empty()).count() as u64;
    assert!(input_revision::reader_payload_readiness(&mut reader,&path,&bytes,&writer,&source,cutoff).unwrap());
    assert_eq!(std::fs::read(path).unwrap(),bytes);assert_eq!(std::fs::read(movements).unwrap(),ledger);
    eprintln!("selected Home: both current and historical readiness prove newer independent completion before {cutoff}");
}

#[tokio::test]
async fn completed_input_artifact_proves_exact_terminal_history_without_replaying() {
    for mode in ["current", "later_normal", "no_context", "current_context", "multiple_prior", "nonterminal", "unadmitted", "unpublished", "late_mark",
        "repeated_terminal", "unfinished_target", "wrong_source", "wrong_hash", "wrong_current_source", "wrong_current_hash",
        "wrong_worker", "wrong_run", "wrong_provider", "unadmitted_input", "changed_record"] {
        let mut fixture = CompletedInputReplayFixture::new();
        let first = fixture.output(!["nonterminal", "unfinished_target"].contains(&mode), mode != "unadmitted", mode != "late_mark");
        if mode != "unpublished" { fixture.publish(&first); }
        if mode == "late_mark" { fixture.mark(&first); }
        if ["later_normal", "repeated_terminal", "unfinished_target"].contains(&mode) {
            let bytes = std::fs::read(&fixture.handoff.run_path).unwrap();
            let record = RunRecord::from_bytes(&bytes).unwrap();
            let cp = record.checkpoints.iter().find(|cp|record.active_checkpoints.contains(&cp.id)
                && cp.inbound_interface() == Some(&InterfaceId::new("room"))).unwrap().id.clone();
            let next_input = InterfaceArtifactFile::write(fixture.handoff.var_dir.join("next-input"),"tool.result",
                br#"{"label":"tool.result","tool":"read","correlation_id":"next-call","ok":true,"body":{"value":2}}"#).unwrap();
            fixture.handoff.source_movement_seq = fixture.rows.len() as u64;
            fixture.edge(&bytes,&cp,&next_input,"provider".into(),MovementKind::InterfaceOutbound);
            let later = fixture.output(true,true,true);
            fixture.publish(&later);
            if mode == "repeated_terminal" {
                fixture.handoff.source_movement_seq = 0;
                let repeated = fixture.output(true,true,true);
                fixture.publish(&repeated);
            }
        }
        if ["wrong_worker", "wrong_run", "unadmitted_input"].contains(&mode) {
            let mut metadata = fixture.rows[0].invocation_metadata.as_ref().unwrap().invocation().unwrap().clone();
            match mode {
                "wrong_worker" => metadata.runtime.as_mut().unwrap().worker = "other-worker".into(),
                "wrong_run" => metadata.runtime.as_mut().unwrap().run = "other-run".into(),
                _ => metadata.checkpoint_admission = None,
            }
            fixture.rows[0].invocation_metadata = Some(metadata.into()); fixture.save();
        }
        if mode == "wrong_source" {
            fixture.rows[0].artifact = fixture.rows[2].artifact.clone(); fixture.save();
        }
        if mode == "wrong_hash" {
            fixture.rows[0].artifact.as_mut().unwrap().content = ContentHash::of_bytes(b"other source bytes"); fixture.save();
        }
        if mode == "wrong_current_source" {
            fixture.rows[1].artifact = fixture.rows[2].artifact.clone(); fixture.save();
        }
        if mode == "wrong_current_hash" {
            fixture.rows[1].artifact.as_mut().unwrap().content = ContentHash::of_bytes(b"other current bytes"); fixture.save();
        }
        if mode == "wrong_provider" {
            fixture.rows[0].to = Endpoint::Interface("other-provider".into()); fixture.save();
        }
        if mode == "changed_record" {
            let mut value: serde_json::Value = serde_json::from_slice(&std::fs::read(&fixture.handoff.run_path).unwrap()).unwrap();
            value["status"] = serde_json::json!("unproved");
            std::fs::write(&fixture.handoff.run_path,serde_json::to_vec(&value).unwrap()).unwrap();
        }
        fixture.handoff.source_movement_seq = 1;
        if mode == "multiple_prior" {
            let bytes = std::fs::read(&fixture.handoff.run_path).unwrap();
            let record = RunRecord::from_bytes(&bytes).unwrap();
            let cp = record.checkpoints.iter().find(|cp|record.active_checkpoints.contains(&cp.id)
                && cp.inbound_interface() == Some(&InterfaceId::new("alternate_room"))).unwrap().id.clone();
            fixture.handoff.source_movement_seq = fixture.rows.len() as u64;
            fixture.edge(&bytes,&cp,&fixture.input_file.clone(),"provider".into(),MovementKind::InterfaceOutbound);
        }
        {
            let mut state = fixture.handoff.state.lock().await;
            if mode == "no_context" { state.session_context.artifacts.clear(); }
            if mode == "current_context" || mode == "multiple_prior" {
                state.session_context.artifacts[0].source_movement_seq = Some(fixture.handoff.source_movement_seq);
            }
        }
        fixture.handoff.session = ProviderHandoffSession::new(&fixture.handoff.ctx).unwrap();
        let bytes = std::fs::read(&fixture.handoff.run_path).unwrap();
        fixture.handoff.state.lock().await.run_record_bytes = bytes.clone();
        let ledger = std::fs::read(&fixture.handoff.ctx.movements_path).unwrap();
        let context = serde_json::to_value(fixture.handoff.current_session_context_artifacts().await.as_ref()).unwrap();
        let result = fixture.handoff.completed_input_artifact_source(&fixture.input).await;
        if mode == "unfinished_target" {
            let mut target = fixture.handoff.clone(); target.source_movement_seq = 0;
            let pending = persisted_pending_handoff(&RunRecord::from_bytes(&bytes).unwrap(), &target.config.provider_interface).unwrap().unwrap();
            assert!(target.recover_overtaken_source(&bytes, &pending).unwrap_err().contains("unfinished provider source"));
        }
        if ["current", "later_normal", "no_context", "current_context", "multiple_prior"].contains(&mode) {
            assert_eq!(result.unwrap(),Some(0),"{mode}");
            let mut changed = fixture.input.clone(); changed.content = ContentHash::of_bytes(b"different artifact");
            assert_eq!(fixture.handoff.completed_input_artifact_source(&changed).await.unwrap(),None);
            let mut current = fixture.handoff.clone(); current.source_movement_seq = 0;
            assert_eq!(current.completed_input_artifact_source(&fixture.input).await.unwrap(),None);
        } else if ["wrong_source", "wrong_hash", "wrong_run", "wrong_provider", "unadmitted_input"].contains(&mode) {
            assert_eq!(result.unwrap(),None,"{mode}");
        } else {
            assert!(result.is_err(),"{mode} accepted {result:?}");
        }
        assert_eq!(std::fs::read(&fixture.handoff.run_path).unwrap(),bytes,"{mode}");
        assert_eq!(std::fs::read(&fixture.handoff.ctx.movements_path).unwrap(),ledger,"{mode}");
        assert_eq!(fixture.handoff.state.lock().await.run_record_bytes,bytes,"{mode}");
        assert_eq!(serde_json::to_value(fixture.handoff.current_session_context_artifacts().await.as_ref()).unwrap(),context,"{mode}");
    }
}

#[test]
#[ignore = "requires explicitly selected read-only installation evidence"]
fn selected_home_exact_checked_install() {
    let home = PathBuf::from(std::env::var("OCTOPUS_REPLAY_HOME").unwrap());
    let call_seq = std::env::var("OCTOPUS_REPLAY_CALL").unwrap().parse::<u64>().unwrap();
    let result_seq = std::env::var("OCTOPUS_REPLAY_RESULT").unwrap().parse::<u64>().unwrap();
    let movements = home.join("ledger/movements.jsonl");
    let blobs = home.join("ledger/blobs");
    let scratch = tempfile::tempdir().unwrap();
    let index = scratch.path().join("index.sqlite");
    let evidence = CheckedRunEvidence { movements_path: &movements, ledger_index_path: &index,
        blobs_dir: &blobs, max_control_frame_bytes: 1_048_576,
        max_manifest_bytes: 1_048_576, max_payload_bytes: 268_435_456 };
    let mut reader = FileCheckedRunReader::open(&evidence, Some(call_seq - 1)).unwrap();
    let call = reader.admitted_movement(call_seq).unwrap().unwrap();
    let result = reader.admitted_movement(result_seq).unwrap().unwrap();
    let (manifest, bytes) = reader.artifact(call.artifact.as_ref().unwrap()).unwrap();
    let (result_manifest, _) = reader.artifact(result.artifact.as_ref().unwrap()).unwrap();
    let payload: octopus_tool_handlers::ToolCallArtifact = serde_json::from_slice(&bytes).unwrap();
    let runtime = call.runtime().unwrap();
    let declaration = reader.snapshot(&ContentHash::from_hex(runtime.run_record_hash.as_ref().unwrap()).unwrap()).unwrap();
    let Endpoint::Interface(source) = &call.from;
    let Endpoint::Interface(door) = &call.to;
    let Endpoint::Interface(owner) = &result.to;
    let writer = CheckedRunWriter { owner:owner.clone(), source:source.clone(), door:door.clone(),
        tool:payload.tool, call_label:manifest.label, result_label:result_manifest.label };
    let path = home.join("state/runs").join(format!("{}.json", runtime.run));
    let proof = CheckedRunInstall { call: ConsentEvidenceRef { movement_seq:call_seq, artifact:call.artifact.clone().unwrap() },
        result: ConsentEvidenceRef { movement_seq:result_seq, artifact:result.artifact.clone().unwrap() } };
    let checked = checked_declared_run_install(&evidence, &path, &declaration, &proof, vec![writer]).unwrap();
    assert_eq!(checked.call, call);
    assert_eq!(checked.result, result);
    assert_eq!(ContentHash::of_bytes(&checked.before).to_hex(), payload.arguments["expected_before_hash"]);
    assert_eq!(ContentHash::of_bytes(&checked.after).to_hex(), payload.arguments["expected_after_hash"]);
}

#[test]
#[ignore = "requires an explicitly selected stopped Home and its actual prepared settlement receipt"]
fn selected_home_materializes_exact_settlement_checkpoint_contract() {
    let home = PathBuf::from(std::env::var("OCTOPUS_REPLAY_HOME").unwrap());
    let provider = InterfaceId::new(std::env::var("OCTOPUS_REPLAY_PROVIDER").unwrap());
    let run = RunId::new(std::env::var("OCTOPUS_REPLAY_RUN").unwrap());
    let source: u64 = std::env::var("OCTOPUS_REPLAY_SOURCE").unwrap().parse().unwrap();
    let dir = home.join("registry/interface").join(provider.as_str());
    let descriptor: InterfaceDescriptor = serde_json::from_slice(&std::fs::read(dir.join("interface.json")).unwrap()).unwrap();
    let life: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join("life.json")).unwrap()).unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let (arrivals, receiver) = tokio::sync::mpsc::channel(1); drop(receiver);
    let ctx = InterfaceLiftContext::unprivileged(provider.clone(), dir, Some(descriptor.clone()),
        home.join("interfaces").join(provider.as_str()), home.join("ledger/movements.jsonl"), home.join("ledger/blobs"),
        scratch.path().join("unused-index.sqlite"), BTreeMap::from([
            ("run_records_root".into(), home.join("state/runs")), ("interface_hand_in_root".into(), home.join("interface-hand-in")),
        ]), arrivals, Default::default()).with_host_io_limits(serde_json::from_value(life["processes"][0]["io_limits"].clone()).unwrap()).unwrap();
    let path = home.join("state/runs").join(format!("{run}.json"));
    let before = std::fs::read(&path).unwrap(); let ledger = std::fs::read(&ctx.movements_path).unwrap();
    let cursors = ["ledger-cursor.json", "ledger-cursor.turn-control.json"].map(|name| {
        let path = ctx.interface_var_dir().join(name); let bytes = std::fs::read(&path).unwrap(); (path, bytes)
    });
    let mut replay = octopus_world_pyecho_life_runtime::LedgerReplayReader::open(&ctx).unwrap();
    let mut input = None;
    while let Some(batch) = replay.read_batch(256).unwrap() {
        if let Some(own) = batch.into_iter().find(|own|own.movement.seq.0 == source) { input = Some(replay.envelope(&own).unwrap()); break; }
    }
    let input = input.unwrap(); let pinned = ctx.pinned_run_record_bytes(&input).unwrap();
    let handoff = ProviderHandoffSession::new(&ctx).unwrap().from_invocation(&ctx,&descriptor,&run,&input,source,&pinned).unwrap();
    let record = RunRecord::from_bytes(&before).unwrap();
    let prior = persisted_pending_handoff(&record,&provider).unwrap().unwrap();
    assert_eq!(prior.source_movement_seq,source); assert!(prior.terminal);
    let predecessor = load_pending_artifact(&handoff.var_dir,&prior.items[0]).unwrap().1;
    let receipt = InterfaceArtifactFile::new(PathBuf::from(std::env::var("OCTOPUS_REPLAY_RECEIPT").unwrap())).unwrap();
    let payload_bytes = receipt.read_payload_bytes().unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&payload_bytes).unwrap();
    assert_eq!(payload["session_context"]["parent"],serde_json::json!(predecessor));
    assert_eq!(payload["completed_source_settlement"]["error_artifact"],serde_json::json!(predecessor));
    assert_eq!(payload["completed_source_settlement"]["result_artifact"],serde_json::json!(input.artifact));
    assert_eq!(payload["completed_source_settlement"]["result_movement_seq"],source);
    let generation = optional_u64(&record.interface_state[&provider],"context_generation").unwrap();
    assert_eq!(payload["codex_continuity"]["context_generation"],generation+1);
    let identity = artifact_identity(&receipt,&handoff.var_dir).unwrap();
    assert!(handoff.session_snapshot_labels.contains(&identity.label));
    let old = provider_context_bindings(&record,&provider).unwrap();
    let mut expected = old.iter().filter(|binding|binding.id != predecessor.id).cloned().collect::<Vec<_>>();
    expected.push(ArtifactBinding{id:identity.artifact.id.clone(),access:Default::default()});
    let target = expected.iter().map(|binding|binding.id.clone()).collect::<Vec<_>>();
    let output_checkpoint = handoff.config.output_checkpoint(&ProviderHandoffRoute::ClientProjection,&handoff.granted_tools,&handoff.granted_interfaces,&identity.label).unwrap();
    let mut authored = handoff.author_run_with_bindings(&before,&target,&[PreparedHandoff{identity,output_checkpoint,verified_inputs:Vec::new()}],true,None,false,true,&[]).unwrap();
    handoff.attach_source_settlement(&before,&mut authored,&predecessor,generation).unwrap();
    let pending = RunRecord::from_bytes(authored.content.as_bytes()).unwrap();
    assert_eq!(provider_context_bindings(&pending,&provider).unwrap(),old);
    let publication = handoff.author_context_publication(authored.content.as_bytes(),&authored.pending_handoff).unwrap();
    let published = RunRecord::from_bytes(publication.content.as_bytes()).unwrap();
    source_settlement::validate_transition(&pending,&published,&handoff.config).unwrap();
    validate_bound_run_authoring_topology(&published,&handoff.config,None).unwrap();
    assert_eq!(provider_context_bindings(&published,&provider).unwrap(),expected);
    assert!(handoff.config.checkpoints.iter().any(|template|template.include_context && !template.static_artifact_bindings.is_empty()));
    std::fs::write(scratch.path().join("settlement.json"),&authored.content).unwrap();
    std::fs::write(scratch.path().join("published.json"),&publication.content).unwrap();
    assert_eq!(std::fs::read(path).unwrap(),before);assert_eq!(std::fs::read(&ctx.movements_path).unwrap(),ledger);
    for (path,bytes) in cursors { assert_eq!(std::fs::read(path).unwrap(),bytes); }
    assert_eq!(receipt.read_payload_bytes().unwrap(),payload_bytes);
    eprintln!("actual source {source}: settlement and published active checkpoints preserve {} unique context bindings and exact rights",expected.len());
}

#[test]
#[ignore = "requires explicitly selected read-only outgoing-binding installation evidence"]
fn selected_home_outgoing_static_binding_recovers_exact_pending_output() {
    let home=PathBuf::from(std::env::var("OCTOPUS_REPLAY_HOME").unwrap());
    let provider=InterfaceId::new(std::env::var("OCTOPUS_REPLAY_PROVIDER").unwrap());
    let run=RunId::new(std::env::var("OCTOPUS_REPLAY_RUN").unwrap());
    let source:u64=std::env::var("OCTOPUS_REPLAY_SOURCE").unwrap().parse().unwrap();
    let call_seq:u64=std::env::var("OCTOPUS_REPLAY_CALL").unwrap().parse().unwrap();
    let result_seq:u64=std::env::var("OCTOPUS_REPLAY_RESULT").unwrap().parse().unwrap();
    let dir=home.join("registry/interface").join(provider.as_str());
    let descriptor:InterfaceDescriptor=serde_json::from_slice(&std::fs::read(dir.join("interface.json")).unwrap()).unwrap();
    let life:serde_json::Value=serde_json::from_slice(&std::fs::read(dir.join("life.json")).unwrap()).unwrap();
    let scratch=tempfile::tempdir().unwrap();let (arrivals,receiver)=tokio::sync::mpsc::channel(1);drop(receiver);
    let ctx=InterfaceLiftContext::unprivileged(provider.clone(),dir,Some(descriptor.clone()),scratch.path().join("provider"),
        home.join("ledger/movements.jsonl"),home.join("ledger/blobs"),home.join("ledger/ledger-index.sqlite"),
        BTreeMap::from([("run_records_root".into(),home.join("state/runs")),("interface_hand_in_root".into(),home.join("interface-hand-in"))]),arrivals,Default::default())
        .with_host_io_limits(serde_json::from_value(life["processes"][0]["io_limits"].clone()).unwrap()).unwrap();
    let path=home.join("state/runs").join(format!("{run}.json"));let current=std::fs::read(&path).unwrap();
    let ledger=std::fs::read(&ctx.movements_path).unwrap();let limits=ctx.required_host_io_limits().unwrap();
    let cursors=["ledger-cursor.json","ledger-cursor.turn-control.json"].map(|name| {
        let path=home.join("interfaces").join(provider.as_str()).join(name);let bytes=std::fs::read(&path).unwrap();(path,bytes)
    });
    let evidence=CheckedRunEvidence {movements_path:&ctx.movements_path,ledger_index_path:&ctx.ledger_index_path,blobs_dir:&ctx.blobs_dir,
        max_control_frame_bytes:limits.max_control_frame_bytes,max_manifest_bytes:limits.max_manifest_bytes,max_payload_bytes:limits.max_payload_bytes};
    let call=generated_artifacts::read_movement(&evidence,call_seq).unwrap();let result=generated_artifacts::read_movement(&evidence,result_seq).unwrap();
    let mut replay=octopus_world_pyecho_life_runtime::LedgerReplayReader::open(&ctx).unwrap();let mut input=None;
    while let Some(batch)=replay.read_batch(256).unwrap() {
        if let Some(row)=batch.into_iter().find(|row|row.movement.seq.0==source) {input=Some(replay.envelope(&row).unwrap());break;}
    }
    let input=input.unwrap();let pinned=ctx.pinned_run_record_bytes(&input).unwrap();let original=RunRecord::from_bytes(&pinned).unwrap();
    let writer=declared_provider_run_writer(&original,&provider,&run_authoring_profile(&descriptor).unwrap()).unwrap();
    let proof=CheckedRunInstall {call:ConsentEvidenceRef {movement_seq:call_seq,artifact:call.artifact.clone().unwrap()},
        result:ConsentEvidenceRef {movement_seq:result_seq,artifact:result.artifact.clone().unwrap()}};
    let checked=checked_declared_run_install(&evidence,&path,&pinned,&proof,vec![writer]).unwrap();
    assert_eq!(checked.before,pinned);assert_eq!(checked.after,current);
    let record=RunRecord::from_bytes(&current).unwrap();
    let prior=writer_run::binding(&original,&provider).unwrap().unwrap();let next=writer_run::binding(&record,&provider).unwrap().unwrap();
    assert_eq!(prior.role_artifact_bindings,next.role_artifact_bindings);
    for record in [&original,&record] {
        for item in persisted_pending_handoff(record,&provider).unwrap().unwrap().items {
            let target=ctx.interface_var_dir().join(&item.manifest_relative_path);std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::copy(home.join("interfaces").join(provider.as_str()).join(&item.manifest_relative_path),target).unwrap();
        }
    }
    let handoff=ProviderHandoffSession::new(&ctx).unwrap().from_invocation(&ctx,&descriptor,&run,&input,source,&pinned).unwrap();
    let pending=persisted_pending_handoff(&record,&provider).unwrap().unwrap();assert_eq!(pending.source_movement_seq,source);
    validate_pending_items_are_active(&record,&provider,&pending).unwrap();
    assert!(handoff.recover_authoring_failure(&record,&current).unwrap().is_none());
    assert!(matches!(handoff.recover_authoring_attempt(&record,&current).unwrap(),AuthoringAttemptRecovery::None));
    assert_eq!(handoff.pending_handoff_evidence(&pending).unwrap(),vec![None]);
    let (_,artifact)=load_pending_artifact(&handoff.var_dir,&pending.items[0]).unwrap();
    assert_eq!(artifact.id,pending.items[0].artifact_id);assert_eq!(artifact.content.to_hex(),pending.items[0].artifact_content_hash);
    assert_eq!(std::fs::read(path).unwrap(),current);assert_eq!(std::fs::read(&ctx.movements_path).unwrap(),ledger);
    for (path,bytes) in cursors {assert_eq!(std::fs::read(path).unwrap(),bytes);}
    eprintln!("exact checked write {call_seq}/{result_seq}: {} unchanged detached roles; source {source} reconstructs with no authoring retry and one exact unadmitted pending artifact {}",next.role_artifact_bindings.len(),artifact.id);
}

#[test]
#[ignore = "requires explicitly selected read-only historical ledger evidence"]
fn selected_ledger_checked_authoring_history() {
    let home = PathBuf::from(std::env::var("OCTOPUS_REPLAY_HOME").unwrap());
    let provider = InterfaceId::new(std::env::var("OCTOPUS_REPLAY_PROVIDER").unwrap());
    let run = RunId::new(std::env::var("OCTOPUS_REPLAY_RUN").unwrap());
    let (arrivals, _) = tokio::sync::mpsc::channel(1);
    let ctx = InterfaceLiftContext::unprivileged(provider.clone(), home.join("registry/interface").join(provider.as_str()),
        None, home.join("interfaces").join(provider.as_str()), home.join("ledger/movements.jsonl"), home.join("ledger/blobs"),
        home.join("ledger/ledger-index.sqlite"), BTreeMap::new(), arrivals, Default::default())
        .with_host_io_limits(octopus_interface_transport::InterfaceHostIoLimits { max_control_frame_bytes:1_048_576,
            max_stderr_bytes:262_144,max_manifest_bytes:1_048_576,max_payload_bytes:268_435_456 }).unwrap();
    let decoder = DurableMovementDecoder::for_log(&ctx.movements_path);
    let mut selected = BTreeMap::new();
    for (line, row) in std::io::BufReader::new(std::fs::File::open(&ctx.movements_path).unwrap()).lines().enumerate() {
        let row = row.unwrap(); let value: serde_json::Value = serde_json::from_str(&row).unwrap();
        let seq = value["seq"].as_u64().unwrap();
        if [1452,1456,1471,1473,1477,1478,1481].contains(&seq) {
            selected.insert(seq, decoder.decode(row.as_bytes(), line + 1).unwrap());
        }
    }
    let runtime = |seq| selected[&seq].runtime().unwrap();
    for seq in [1452,1456,1477,1481] {
        assert_eq!(runtime(seq).run, run);
        assert_eq!(selected[&seq].to, Endpoint::Interface(provider.clone()));
    }
    let snapshot = |seq| pinned_run_record(&ctx, &ContentHash::from_hex(
        runtime(seq).run_record_hash.as_ref().unwrap()).unwrap(), "historical ledger").unwrap();
    let (before, _) = snapshot(1452); let (after, record) = snapshot(1481);
    let (call_manifest, call_bytes) = read_pinned_artifact(&ctx, selected[&1471].artifact.as_ref().unwrap()).unwrap();
    let call: FileWriteCallEnvelope = serde_json::from_slice(&call_bytes).unwrap();
    let (result, _) = read_pinned_artifact(&ctx, selected[&1473].artifact.as_ref().unwrap()).unwrap();
    let profile = RunAuthoringProfile { file_write_tool:call.tool, file_write_label:call_manifest.label,
        file_write_result_label:result.label, result_timeout_ms:1000 };
    let config = config_from_persisted_profile(&record, &provider, &profile).unwrap();
    let evidence = CheckedRunEvidence { movements_path:&ctx.movements_path, ledger_index_path:&ctx.ledger_index_path,
        blobs_dir:&ctx.blobs_dir,max_control_frame_bytes:1_048_576,max_manifest_bytes:1_048_576,max_payload_bytes:268_435_456 };
    let writer = CheckedRunWriter { owner:provider.clone(),source:config.authoring_source_interface.clone(),
        door:config.checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringResult).unwrap().interface().clone(),
        tool:profile.file_write_tool,call_label:profile.file_write_label,result_label:profile.file_write_result_label };
    let start = concurrent_writer_evidence_start(&evidence, &call.arguments.path, &RunRecord::from_bytes(&before).unwrap(), &before, 1452, std::slice::from_ref(&writer)).unwrap();
    let chain = checked_run_file_chain_state(&evidence, &call.arguments.path, &before, &after, start, &[writer]).unwrap();
    for transition in &chain { validate_checked_provider_transition(transition).unwrap(); }
    assert_eq!(chain.len(), 3);
    assert_eq!(record.interface_state[&provider]["authoring_generation"], serde_json::json!(68));
    assert_eq!(persisted_pending_handoff(&record, &provider).unwrap().unwrap().source_movement_seq, 1456);
    assert_eq!(runtime(1477).checkpoint, runtime(1478).checkpoint);
    assert_ne!(runtime(1477).checkpoint, runtime(1481).checkpoint);
    eprintln!("historical ledger: 3 checked writes through generation 68; source 1452 precedes Stop 1456; message 1477 and Stop 1481 use distinct checkpoints");
}

#[tokio::test]
#[ignore = "requires explicitly selected read-only replay evidence"]
async fn selected_home_independent_handoff_replays_unconsumed_source() {
    let home = PathBuf::from(std::env::var("OCTOPUS_REPLAY_HOME").unwrap());
    let provider = InterfaceId::new(std::env::var("OCTOPUS_REPLAY_PROVIDER").unwrap());
    let run = RunId::new(std::env::var("OCTOPUS_REPLAY_RUN").unwrap());
    let source_movement_seq = std::env::var("OCTOPUS_REPLAY_SOURCE").unwrap().parse().unwrap();
    let run_path = home.join("state/runs").join(format!("{run}.json"));
    let current = std::fs::read(&run_path).unwrap();
    let record = RunRecord::from_bytes(&current).unwrap();
    let interface_dir = home.join("registry/interface").join(provider.as_str());
    let descriptor: InterfaceDescriptor = serde_json::from_slice(
        &std::fs::read(interface_dir.join("interface.json")).unwrap(),
    ).unwrap();
    let profile = run_authoring_profile(&descriptor).unwrap();
    let (config, bound_topology) = resolve_profile_invocation(&profile, &provider, &run, &record, None).unwrap();
    let (context_output_labels, session_snapshot_labels) = provider_context_label_sets(&descriptor).unwrap();
    let (arrivals, receiver) = tokio::sync::mpsc::channel(1);
    drop(receiver);
    let mut ctx = InterfaceLiftContext::unprivileged(
        provider.clone(), interface_dir, Some(descriptor), home.join("interfaces").join(provider.as_str()),
        home.join("ledger/movements.jsonl"), home.join("ledger/blobs"),
        home.join("ledger/ledger-index.sqlite"), BTreeMap::from([
            ("run_records_root".into(), home.join("state/runs")),
            ("interface_hand_in_root".into(), home.join("interface-hand-in")),
        ]), arrivals,
        tokio_util::sync::CancellationToken::new(),
    ).with_host_io_limits(octopus_interface_transport::InterfaceHostIoLimits {
        max_control_frame_bytes: 1_048_576, max_stderr_bytes: 262_144,
        max_manifest_bytes: 1_048_576, max_payload_bytes: 268_435_456,
    }).unwrap();
    let scratch = tempfile::tempdir().unwrap();
    ctx.ledger_index_path = scratch.path().join("unused-index.sqlite");
    let mut reader_ctx = ctx.clone();
    reader_ctx.var_dir = scratch.path().to_path_buf();
    let result_reader = LedgerTailReader::open_named(&reader_ctx, "replay-audit").unwrap();
    let handoff = ProviderOutputHandoff {
            invocation_metadata: InvocationMetadata::new(Default::default()),
        session: ProviderHandoffSession::new(&reader_ctx).unwrap(),
        hand_in_root: home.join("interface-hand-in").join(config.authoring_source_interface.as_str()),
        var_dir: ctx.var_dir.clone(), ctx, config, run_path, source_movement_seq,
        source_carries_context: true, failed_source: false,
        checked_settlement_output: None,
        context_output_labels, session_snapshot_labels, standing_candidate_labels: BTreeSet::new(),
        granted_tools: BTreeSet::new(), granted_interfaces: BTreeSet::new(), granted_mark_outcomes: BTreeSet::new(),
        bound_topology, delivery_mode: HandoffDeliveryMode::Rearm, invocation_session_snapshot: None,
        result_reader: Arc::new(tokio::sync::Mutex::new(result_reader)),
        state: Arc::new(tokio::sync::Mutex::new(HandoffState {
            validated_withdrawal_output: None,
            context_artifacts: Vec::new(), static_context_artifacts: Vec::new(),
            session_context: ResolvedSessionContext { provider_interface: provider.clone(), artifacts: Vec::new() },
            session_snapshot_head: None, run_record_bytes: current.clone(), run_record_evidence_seq: None,
            consumed_input: true, consumed_context_free_independent_input: None, consumed_verified_input: false, deferred_context_generations: 0,
        })),
    };
    let pending = persisted_pending_handoff(&record, &provider).unwrap().unwrap();
    assert!(pending.source_movement_seq > source_movement_seq);
    let recovered = handoff.recover_overtaken_source(&current, &pending).unwrap();
    assert_eq!((&recovered.0, &recovered.1), (&ProviderSourceRecovery::Process, &Vec::new()));
    assert!(pending.context_publication.is_none());
    assert!(handoff.recover_authoring_failure(&record, &current).unwrap().is_none());
    assert!(matches!(handoff.recover_authoring_attempt(&record, &current).unwrap(), AuthoringAttemptRecovery::None));
    let (_, source) = handoff.session.index().unwrap().exact_provider_source_outbound(
        &handoff.config.run, &handoff.config.worker, &provider, source_movement_seq,
    ).unwrap();
    let (manifest, payload) = read_pinned_artifact(&handoff.ctx, &source.artifact).unwrap();
    append_session_payload(&mut handoff.state.lock().await.session_context,
        &manifest.label, &payload, &source.artifact, Some(source_movement_seq), "replayed input").unwrap();
    assert_eq!(handoff.recover_source_with_outcomes().await.unwrap(), (recovered.0, recovered.1));
    let context = handoff.current_session_context_artifacts().await;
    assert_eq!(context.last().unwrap().source_artifact, source.artifact);
    assert_eq!(context.last().unwrap().source_movement_seq, Some(source_movement_seq));
    let mut unadmitted = pending.clone();
    unadmitted.items[0].checkpoint.epoch += 1;
    assert!(handoff.recover_overtaken_source(&current, &unadmitted).is_err());
    let mut unrelated = pending.clone();
    unrelated.source_movement_seq = source_movement_seq;
    assert!(handoff.recover_overtaken_source(&current, &unrelated).unwrap_err().contains("overtaken source requires a later checked handoff"));
    let mut incomplete = pending.clone();
    incomplete.terminal = false;
    assert!(handoff.recover_overtaken_source(&current, &incomplete).unwrap_err().contains("no completed terminal input handoff"));
    let mut changed: serde_json::Value = serde_json::from_slice(&current).unwrap();
    changed["status"] = serde_json::json!("running");
    assert!(handoff.recover_overtaken_source(&serde_json::to_vec(&changed).unwrap(), &pending).is_err());
    assert_eq!(std::fs::read(&handoff.run_path).unwrap(), current);
    eprintln!("verified source {source_movement_seq} remains Process behind admitted independent source {}", pending.source_movement_seq);
}

#[tokio::test]
#[ignore = "requires explicitly selected read-only replay evidence"]
async fn selected_home_failed_source_rearms_its_consumed_input() {
    let home = PathBuf::from(std::env::var("OCTOPUS_REPLAY_HOME").unwrap());
    let provider = InterfaceId::new(std::env::var("OCTOPUS_REPLAY_PROVIDER").unwrap());
    let run = RunId::new(std::env::var("OCTOPUS_REPLAY_RUN").unwrap());
    let source_movement_seq: u64 = std::env::var("OCTOPUS_REPLAY_SOURCE").unwrap().parse().unwrap();
    let interface_dir = home.join("registry/interface").join(provider.as_str());
    let descriptor: InterfaceDescriptor = serde_json::from_slice(
        &std::fs::read(interface_dir.join("interface.json")).unwrap(),
    ).unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let (arrivals, receiver) = tokio::sync::mpsc::channel(1);
    drop(receiver);
    let ctx = InterfaceLiftContext::unprivileged(
        provider.clone(), interface_dir, Some(descriptor.clone()), scratch.path().join("var"),
        home.join("ledger/movements.jsonl"), home.join("ledger/blobs"),
        scratch.path().join("unused-index.sqlite"),
        BTreeMap::from([
            ("run_records_root".to_string(), home.join("state/runs")),
            ("interface_hand_in_root".to_string(), scratch.path().join("hand-in")),
        ]),
        arrivals, tokio_util::sync::CancellationToken::new(),
    ).with_host_io_limits(octopus_interface_transport::InterfaceHostIoLimits {
        max_control_frame_bytes: 1_048_576, max_stderr_bytes: 262_144,
        max_manifest_bytes: 1_048_576, max_payload_bytes: 268_435_456,
    }).unwrap();
    std::fs::create_dir_all(&ctx.var_dir).unwrap();
    let mut replay = octopus_world_pyecho_life_runtime::LedgerReplayReader::open(&ctx).unwrap();
    let mut input = None;
    while input.is_none() {
        let Some(batch) = replay.read_batch(256).unwrap() else { break; };
        for own in batch {
            if own.movement.seq.0 == source_movement_seq {
                input = Some(replay.envelope(&own).unwrap());
            }
        }
    }
    let input = input.expect("selected source is not an outbound movement of this provider");
    let session = ProviderHandoffSession::new(&ctx).unwrap();
    eprintln!(
        "consumed context input sources: {:?}",
        session.consumed_context_input_sources(&ctx, &descriptor).unwrap()
    );
    let pinned = ctx.pinned_run_record_bytes(&input).unwrap();
    let canonical = session
        .from_invocation_state(&ctx, &descriptor, &run, &input, source_movement_seq, &pinned)
        .err();
    eprintln!("canonical handoff for source {source_movement_seq}: {}", canonical.as_ref().map(ToString::to_string).unwrap_or("formed".into()));
    if let Some(CheckedRunError::AwaitingResults { call_sequences }) = &canonical {
        let waiting = await_checked_evidence(&ctx, 0, || {
            session.from_invocation_state(&ctx, &descriptor, &run, &input, source_movement_seq, &pinned)
        }).await.err().expect("missing result must remain pending");
        assert!(matches!(waiting, CheckedRunError::AwaitingResults { call_sequences: actual } if actual == *call_sequences));
    }
    let failed = session
        .failed_source_handoff(&ctx, &descriptor, &run, &input, source_movement_seq)
        .unwrap();
    let error = InterfaceArtifactFile::write(
        ctx.var_dir.join("emitted"), "turn.error",
        br#"{"label":"turn.error","message":"replayed failed source"}"#,
    ).unwrap();
    let state = failed.state.lock().await;
    let before = RunRecord::from_bytes(&state.run_record_bytes).unwrap();
    let identity = artifact_identity(&error, &failed.var_dir).unwrap();
    let output_checkpoint = failed.config.output_checkpoint(
        &ProviderHandoffRoute::ClientProjection, &failed.granted_tools, &failed.granted_interfaces, &identity.label,
    ).unwrap();
    let authored = failed.author_run_with_bindings(
        &state.run_record_bytes, &state.context_artifacts,
        &[PreparedHandoff { identity, output_checkpoint, verified_inputs: Vec::new() }],
        state.consumed_input, state.consumed_context_free_independent_input.as_ref(),
        state.consumed_verified_input, true, &[],
    ).unwrap();
    let after = RunRecord::from_bytes(authored.content.as_bytes()).unwrap();
    let checkpoint = &input.runtime().unwrap().checkpoint;
    eprintln!(
        "failed source {source_movement_seq}: consumed_input={} checkpoint '{}' active before={} after={}",
        state.consumed_input, checkpoint.as_str(),
        before.active_checkpoints.contains(checkpoint), after.active_checkpoints.contains(checkpoint),
    );
    assert!(authored.pending_handoff.terminal && authored.pending_handoff.source_without_context);
    if let Some(template) = &state.consumed_context_free_independent_input {
        assert!(canonical.is_none(), "{}", canonical.unwrap());
        assert!(!after.active_checkpoints.contains(checkpoint));
        let rearmed = after.checkpoints.iter().filter(|candidate| {
            after.active_checkpoints.contains(&candidate.id)
                && checkpoint_record_matches_template(candidate, template, &run)
        }).collect::<Vec<_>>();
        assert_eq!(rearmed.len(), 1);
        assert_eq!(rearmed[0].artifact_bindings, before.checkpoint(checkpoint).unwrap().artifact_bindings);
        assert_ne!(rearmed[0].id, *checkpoint);
    } else {
        assert_eq!(after.active_checkpoints.contains(checkpoint), !state.consumed_input && before.active_checkpoints.contains(checkpoint));
    }
    assert_eq!(current_context_artifacts(&after, &failed.config).unwrap(), state.context_artifacts);
    assert_eq!(std::fs::read(home.join("state/runs").join(format!("{run}.json"))).unwrap(), state.run_record_bytes);
}

#[tokio::test]
async fn two_calls_in_one_turn_keep_every_door_in_reach() {
    use serde_json::json;
    fn call_through(fixture: &mut CompletedInputReplayFixture, door: &str, tool: &str, context: &[ArtifactId]) -> (AuthoredRun, ArtifactId) {
        let before = std::fs::read(&fixture.handoff.run_path).unwrap();
        let file = InterfaceArtifactFile::write(fixture.handoff.var_dir.join(format!("call-{tool}")), "tool.call",
            &serde_json::to_vec(&json!({"label":"tool.call","tool":tool,"arguments":{},"correlation_id":format!("{tool}-call")})).unwrap()).unwrap();
        let identity = artifact_identity(&file, &fixture.handoff.var_dir).unwrap();
        let call = identity.artifact.id.clone();
        let mut context = context.to_vec();
        append_artifact(&mut context, call.clone());
        let output_checkpoint = fixture.handoff.config.checkpoints.iter().find(|cp| cp.name == door).unwrap().clone();
        let authored = fixture.handoff.author_run_with_bindings(&before, &context, &[PreparedHandoff {
            identity, output_checkpoint, verified_inputs:Vec::new(),
        }], true, None, false, true, &[]).unwrap();
        fixture.install(&authored);
        fixture.edge(authored.content.as_bytes(), &authored.pending_handoff.items[0].checkpoint.id, &file, "worker".into(), MovementKind::InterfaceArrival);
        fixture.mark(&authored);
        (authored, call)
    }
    let doors = |record: &RunRecord, door: &str| record.active_checkpoints.iter().filter_map(|id| record.checkpoint(id))
        .filter(|cp| cp.inbound_interface() == Some(&InterfaceId::new("provider")) && cp.outbound_interfaces().any(|id| id.as_str() == door)).count();
    let mut fixture = CompletedInputReplayFixture::new();
    let h = &mut fixture.handoff;
    h.run_path = h.run_path.canonicalize().unwrap();
    h.ctx.root_slots.insert("run_records_root".into(), h.run_path.parent().unwrap().to_path_buf());
    h.granted_mark_outcomes.insert("tool.result.received".into());
    h.granted_tools.insert("search".into());
    let receiving = |tool: &str| serde_json::from_value::<RequiredVerifiedArtifact>(json!({
        "mark":"tool.result.received","artifact_label":format!("{tool}.result"),"verifier":"result_correspondence","params":{}
    })).unwrap();
    let tool = h.config.checkpoints.iter_mut().find(|cp| cp.name == "tool").unwrap();
    tool.outbound_interface_bindings = vec![AuthoredInterfaceBinding::all("result_room".into())];
    tool.result_receiving.insert("read".into(), receiving("read"));
    let mut search = tool.clone();
    search.name = "search".into();
    search.outbound_interface_bindings = vec![AuthoredInterfaceBinding::all("search_room".into())];
    search.required_verified_artifacts[0].params = json!({"tools":["search"]});
    search.result_receiving = BTreeMap::from([("search".into(), receiving("search"))]);
    h.config.checkpoints.push(search.clone());
    h.bound_topology.checkpoints = h.config.checkpoints.clone();
    let mut seed: serde_json::Value = serde_json::from_slice(&std::fs::read(&h.run_path).unwrap()).unwrap();
    seed["interface_state"]["provider"][RUN_AUTHORING_BINDING] = serde_json::to_value(&h.bound_topology).unwrap();
    seed["checkpoints"].as_array_mut().unwrap().iter_mut().find(|cp| cp["id"] == "tool").unwrap()["outbound_interface_bindings"] =
        json!([AuthoredInterfaceBinding::all("result_room".into())]);
    seed["checkpoints"].as_array_mut().unwrap().push(materialize_checkpoint(&search, "search", &h.config.run, &[]));
    seed["active_checkpoints"].as_array_mut().unwrap().push(json!("search"));
    seed["interface_state"]["provider"]["frontier_checkpoint_ids"].as_array_mut().unwrap().push(json!("search"));
    let initial = serde_json::to_vec(&seed).unwrap();
    std::fs::write(&h.run_path, &initial).unwrap();
    h.state.try_lock().unwrap().run_record_bytes = initial.clone();
    fixture.rows.clear();
    let input_file = fixture.input_file.clone();
    fixture.edge(&initial, &"input".into(), &input_file, "provider".into(), MovementKind::InterfaceOutbound);
    let mut context = current_context_artifacts(&RunRecord::from_bytes(&initial).unwrap(), &fixture.handoff.config).unwrap();
    append_artifact(&mut context, fixture.input.id.clone());
    let (first, first_call) = call_through(&mut fixture, "tool", "read", &context);
    assert!(first.pending_handoff.deferred_publication);
    let waiting = fixture.reload_definition();
    assert_eq!((doors(&waiting, "result_room"), doors(&waiting, "search_room")), (1, 1),
        "the run that waits for one result still reaches every door for its next call");
    let first_wait = fixture.handoff.config.checkpoints.iter().find(|cp| cp.role == RunAuthoringCheckpointRole::ResultInput).unwrap().clone();
    let receiving_first = waiting.checkpoints.iter().find(|cp| checkpoint_record_matches_template(cp, &first_wait, &waiting.id)).unwrap().clone();
    let written = std::fs::read(&fixture.handoff.run_path).unwrap();
    let result = InterfaceArtifactFile::write(fixture.handoff.var_dir.join("read-result"), "read.result",
        &serde_json::to_vec(&json!({"label":"read.result","tool":"read","correlation_id":"read-call","ok":true,"body":{}})).unwrap()).unwrap();
    let source = fixture.rows.len() as u64;
    fixture.edge(&written, &receiving_first.id, &result, "provider".into(), MovementKind::InterfaceOutbound);
    fixture.handoff.source_movement_seq = source;
    fixture.handoff.invocation_metadata = fixture.rows.last().unwrap().invocation_metadata.as_ref().unwrap().invocation().unwrap().clone();
    fixture.handoff.state.try_lock().unwrap().run_record_bytes = written.clone();
    assert_eq!(fixture.handoff.recover_source().await.unwrap(), ProviderSourceRecovery::Process);
    assert_eq!(std::fs::read(&fixture.handoff.run_path).unwrap(), written);
    let mut context = fixture.handoff.state.try_lock().unwrap().context_artifacts.clone();
    append_artifact(&mut context, artifact_identity(&result, &fixture.handoff.var_dir).unwrap().artifact.id);
    let (second, second_call) = call_through(&mut fixture, "search", "search", &context);
    let continued = fixture.reload_definition();
    assert!(second.pending_handoff.deferred_publication);
    assert!(continued.checkpoint(&receiving_first.id).is_none(), "the first result wait is consumed");
    assert!(current_context_artifacts(&continued, &fixture.handoff.config).unwrap().contains(&first_call));
    let second_wait = fixture.handoff.config.checkpoints.iter().find(|cp| cp.role == RunAuthoringCheckpointRole::ResultInput).unwrap().clone();
    assert!(continued.checkpoints.iter().find(|cp| checkpoint_record_matches_template(cp, &second_wait, &continued.id)).unwrap()
        .artifact_bindings.iter().any(|binding| binding.id == second_call));
    assert_eq!((doors(&continued, "result_room"), doors(&continued, "search_room")), (1, 1));
    result_inputs::validate_saved(&fixture.handoff.ctx, &waiting, &continued, &fixture.handoff.config).unwrap();
}
#[tokio::test]
async fn a_terminal_call_rides_one_write_until_its_result_is_consumed() {
    let mut fixture = CompletedInputReplayFixture::new();
    let h = &mut fixture.handoff;
    h.run_path = h.run_path.canonicalize().unwrap();
    h.ctx.root_slots.insert("run_records_root".into(), h.run_path.parent().unwrap().to_path_buf());
    h.granted_mark_outcomes.insert("tool.result.received".into());
    let door = vec![AuthoredInterfaceBinding::all("result_room".into())];
    {
        let tool = h.config.checkpoints.iter_mut().find(|cp| cp.role == RunAuthoringCheckpointRole::ProviderToolOutput).unwrap();
        tool.outbound_interface_bindings = door.clone();
        tool.result_receiving.insert("read".into(), serde_json::from_value(serde_json::json!({
            "mark":"tool.result.received","artifact_label":"read.result","verifier":"result_correspondence","params":{}
        })).unwrap());
    }
    h.bound_topology.checkpoints = h.config.checkpoints.clone();
    let mut seed: serde_json::Value = serde_json::from_slice(&std::fs::read(&h.run_path).unwrap()).unwrap();
    seed["interface_state"]["provider"][RUN_AUTHORING_BINDING] = serde_json::to_value(&h.bound_topology).unwrap();
    seed["checkpoints"].as_array_mut().unwrap().iter_mut().find(|cp| cp["id"] == "tool").unwrap()["outbound_interface_bindings"] =
        serde_json::to_value(door).unwrap();
    let initial = serde_json::to_vec(&seed).unwrap();
    std::fs::write(&h.run_path, &initial).unwrap();
    h.state.try_lock().unwrap().run_record_bytes = initial.clone();
    fixture.rows.clear();
    let input_file = fixture.input_file.clone();
    fixture.edge(&initial, &"input".into(), &input_file, "provider".into(), MovementKind::InterfaceOutbound);
    let call = fixture.output(true, true, true);
    assert!(call.pending_handoff.deferred_publication && call.pending_handoff.context_publication.is_some());
    let call_id = call.pending_handoff.items[0].artifact_id.clone();
    let record = fixture.reload_definition();
    let receipt = fixture.handoff.config.checkpoints.iter().find(|cp| cp.role == RunAuthoringCheckpointRole::ResultInput).unwrap().clone();
    assert!(!receipt.static_artifact_bindings.contains(&call_id), "the awaited call is context, not hidden evidence");
    let receiving = record.checkpoints.iter().find(|cp| checkpoint_record_matches_template(cp, &receipt, &record.id)).unwrap().clone();
    assert!(receiving.artifact_bindings.iter().any(|binding| binding.id == call_id));
    assert!(!provider_context_bindings(&record, &"provider".into()).unwrap().iter().any(|binding| binding.id == call_id));
    let written = std::fs::read(&fixture.handoff.run_path).unwrap();
    {
        let mut state = fixture.handoff.state.try_lock().unwrap().clone();
        state.run_record_bytes = written.clone();
        fixture.handoff.publish_admitted_context(&mut state).await.unwrap();
        assert_eq!(state.run_record_bytes, written, "a deferred publication takes no write of its own");
    }
    let correlation = receipt.required_verified_artifacts[0].params["correlation_id"].as_str().unwrap().to_string();
    let result = InterfaceArtifactFile::write(fixture.handoff.var_dir.join("received"), "read.result",
        &serde_json::to_vec(&serde_json::json!({"label":"read.result","tool":"read","correlation_id":correlation,"ok":true,"body":{}})).unwrap()).unwrap();
    let source = fixture.rows.len() as u64;
    fixture.edge(&written, &receiving.id, &result, "provider".into(), MovementKind::InterfaceOutbound);
    fixture.handoff.source_movement_seq = source;
    fixture.handoff.invocation_metadata = fixture.rows.last().unwrap().invocation_metadata.as_ref().unwrap().invocation().unwrap().clone();
    fixture.handoff.state.try_lock().unwrap().run_record_bytes = written.clone();
    assert_eq!(fixture.handoff.recover_source().await.unwrap(), ProviderSourceRecovery::Process);
    assert_eq!(std::fs::read(&fixture.handoff.run_path).unwrap(), written, "a synchronous result needs no bridge write");
    assert!(fixture.handoff.state.try_lock().unwrap().context_artifacts.contains(&call_id));
    let reply = InterfaceArtifactFile::write(fixture.handoff.var_dir.join("reply"), "reply", br#"{"text":"read done"}"#).unwrap();
    let mut context = fixture.handoff.state.try_lock().unwrap().context_artifacts.clone();
    append_artifact(&mut context, artifact_identity(&result, &fixture.handoff.var_dir).unwrap().artifact.id);
    let next = fixture.handoff.author_run_with_bindings(&written, &context, &[PreparedHandoff {
        identity:artifact_identity(&reply, &fixture.handoff.var_dir).unwrap(),
        output_checkpoint:fixture.handoff.config.checkpoint_for_role(RunAuthoringCheckpointRole::ProviderArtifactOutput).unwrap().clone(),
        verified_inputs:Vec::new(),
    }], true, None, false, true, &[]).unwrap();
    let consumed = RunRecord::from_bytes(next.content.as_bytes()).unwrap();
    assert!(!next.pending_handoff.deferred_publication);
    assert!(consumed.checkpoint(&receiving.id).is_none());
    assert!(current_context_artifacts(&consumed, &fixture.handoff.config).unwrap().contains(&call_id),
        "the consuming write publishes the call");
    result_inputs::validate_saved(&fixture.handoff.ctx, &record, &consumed, &fixture.handoff.config).unwrap();
}

fn prepare_definition_edit(
    path: PathBuf,
    current: &[u8],
    writer: &CheckedRunWriter,
    checkpoints: &[CheckpointId],
    context: Option<&[ArtifactBinding]>,
) -> Result<CheckedRunEdit, String> {
    let value = result_inputs::definition_value(current, writer, checkpoints, context)?;
    let record = RunRecord::from_bytes(current).map_err(|error| error.to_string())?;
    let generation = value["interface_state"][writer.owner.as_str()]["authoring_generation"]
        .as_u64()
        .ok_or("result retirement has no authoring generation")?;
    CheckedRunEdit::prepare(
        path,
        current,
        value,
        format!("provider-run-authoring:{}:{generation}", record.id),
    )
}

#[tokio::test]
async fn operation_results_wait_per_call_and_a_gated_answer_rearms_the_same_wait() {
    let mut fixture = CompletedInputReplayFixture::new();
    let h = &mut fixture.handoff;
    h.run_path = h.run_path.canonicalize().unwrap();
    h.ctx.root_slots.insert("run_records_root".into(), h.run_path.parent().unwrap().to_path_buf());
    h.granted_mark_outcomes.extend(["tool.result.corresponds".to_string(), "operator.approval".to_string()]);
    h.granted_interfaces.insert("world_app".into());
    let names = |label: &str| InterfaceBindingConfig { artifact_policy: BindingArtifactPolicy::Allow { names: vec![label.into()], namespaces: Vec::new() } };
    let output = RunAuthoringCheckpoint {
        name: "app_output".into(), role: RunAuthoringCheckpointRole::ProviderInterfaceOutput,
        result_receiving: BTreeMap::from([("app.result".to_string(), serde_json::from_value(serde_json::json!({
            "mark":"tool.result.corresponds","artifact_label":"app.result","verifier":"tool_result_correspondence","params":{}
        })).unwrap())]),
        static_artifact_bindings: Vec::new(), owner_authoring: None,
        interface_binding: AuthoredInterfaceBinding { interface: "provider".into(), config: names("app.request") },
        outbound_interface_bindings: vec![AuthoredInterfaceBinding { interface: "world_app".into(), config: names("app.request") }],
        include_context: false, required_verified_artifacts: Vec::new(),
    };
    h.config.checkpoints.push(output.clone());
    h.bound_topology.checkpoints = h.config.checkpoints.clone();
    h.config.validate(&h.ctx.interface, &h.config.run).unwrap();
    let mut leaking = h.config.clone();
    leaking.checkpoints[1].interface_binding = AuthoredInterfaceBinding::all("world_app".into());
    assert!(leaking.validate(&h.ctx.interface, &h.config.run).unwrap_err().contains("retains another live provider input"));
    leaking.checkpoints[1].interface_binding = AuthoredInterfaceBinding { interface: "world_app".into(), config: names("app.event") };
    leaking.validate(&h.ctx.interface, &h.config.run).unwrap();
    let mut seed: serde_json::Value = serde_json::from_slice(&std::fs::read(&h.run_path).unwrap()).unwrap();
    seed["interface_state"]["provider"][RUN_AUTHORING_BINDING] = serde_json::to_value(&h.bound_topology).unwrap();
    seed["checkpoints"].as_array_mut().unwrap().push(materialize_checkpoint(&output, "app_output", &h.config.run, &[]));
    seed["active_checkpoints"].as_array_mut().unwrap().push("app_output".into());
    seed["interface_state"]["provider"]["frontier_checkpoint_ids"].as_array_mut().unwrap().push("app_output".into());
    let initial = serde_json::to_vec(&seed).unwrap();
    std::fs::write(&h.run_path, &initial).unwrap();
    h.state.try_lock().unwrap().run_record_bytes = initial.clone();
    fixture.rows.clear();
    let input_file = fixture.input_file.clone();
    fixture.edge(&initial, &"input".into(), &input_file, "provider".into(), MovementKind::InterfaceOutbound);
    fixture.edge(&initial, &"alternate".into(), &input_file, "provider".into(), MovementKind::InterfaceOutbound);

    let mut selected = output.clone();
    assert!(result_inputs::select_call_result(&mut selected.clone(), &ProviderHandoffRoute::Interface { interface: "world_app".into() }).is_err());
    assert!(result_inputs::select_call_result(&mut selected.clone(), &ProviderHandoffRoute::Operation { interface: "world_app".into(), result_label: "app.other".into() }).is_err());
    result_inputs::select_call_result(&mut selected, &ProviderHandoffRoute::Operation { interface: "world_app".into(), result_label: "app.result".into() }).unwrap();
    let call_file = InterfaceArtifactFile::write(fixture.handoff.var_dir.join("app-call"), "app.request",
        br#"{"operation":"app.send","arguments":{"to":"a"},"correlation_id":"call-1"}"#).unwrap();
    let call = artifact_identity(&call_file, &fixture.handoff.var_dir).unwrap();
    let mut context = current_context_artifacts(&RunRecord::from_bytes(&initial).unwrap(), &fixture.handoff.config).unwrap();
    append_artifact(&mut context, fixture.input.id.clone());
    append_artifact(&mut context, call.artifact.id.clone());
    let armed = fixture.handoff.author_run_with_bindings(&initial, &context, &[PreparedHandoff {
        identity: call.clone(), output_checkpoint: selected, verified_inputs: Vec::new(),
    }], true, None, false, false, &[]).unwrap();
    fixture.install(&armed);
    fixture.edge(armed.content.as_bytes(), &armed.pending_handoff.items[0].checkpoint.id, &call_file, "world_app".into(), MovementKind::InterfaceArrival);
    let before = RunRecord::from_bytes(&initial).unwrap();
    let record = fixture.reload_definition();
    result_inputs::validate_saved(&fixture.handoff.ctx, &before, &record, &fixture.handoff.config).unwrap();
    let wait = fixture.handoff.config.checkpoints.iter().find(|cp| cp.role == RunAuthoringCheckpointRole::ResultInput).unwrap().clone();
    let required = &wait.required_verified_artifacts[0];
    assert_eq!((wait.interface().as_str(), required.artifact_label.as_str(), &required.params["tool"], &required.params["correlation_id"]),
        ("world_app", "app.result", &serde_json::json!("app.send"), &serde_json::json!("call-1")));
    fixture.publish(&armed);
    let published = fixture.reload_definition();
    let first_generation = published.checkpoints.iter().find(|cp| checkpoint_record_matches_template(cp, &wait, &published.id)).unwrap().clone();

    let var_dir = fixture.handoff.var_dir.clone();
    let answer = |name: &str, body: serde_json::Value| InterfaceArtifactFile::write(var_dir.join(name), "app.result",
        &serde_json::to_vec(&body).unwrap()).unwrap();
    let gated = answer("gated", serde_json::json!({"label":"app.result","operation":"app.send","correlation_id":"call-1","ok":true,
        "result":{"effect_performed":false,"operator_request":{"request_id":"blake3:00"}}}));
    let envelope = InterfaceInvocationEnvelope { artifact: artifact_identity(&gated, &fixture.handoff.var_dir).unwrap().artifact,
        artifact_file: gated.clone(), artifact_binding_payloads: vec![octopus_interfaces::ArtifactBindingPayload::new(
            call.artifact.clone(), call_file.read_payload_bytes().unwrap())], metadata: None };
    result_inputs::validate_invocation(&envelope, &wait).unwrap();
    let current = std::fs::read(&fixture.handoff.run_path).unwrap();
    fixture.edge(&current, &first_generation.id, &gated, "provider".into(), MovementKind::InterfaceOutbound);
    fixture.handoff.source_movement_seq = fixture.rows.len() as u64 - 1;
    fixture.handoff.invocation_metadata = fixture.rows.last().unwrap().invocation_metadata.as_ref().unwrap().invocation().unwrap().clone();
    let request = InterfaceArtifactFile::write(fixture.handoff.var_dir.join("approval"), "operator.approval.request", br#"{"request_id":"blake3:00"}"#).unwrap();
    let client = fixture.handoff.config.checkpoint_for_role(RunAuthoringCheckpointRole::ProviderArtifactOutput).unwrap().clone();
    let approval = fixture.handoff.config.verified_input_checkpoints(&ProviderHandoffRoute::ClientProjectionWithVerifiedInputs { inputs: vec![ProviderVerifiedInput {
        also_deliver_to_tool: None, also_deliver_to_interface: Some("world_app".into()), artifact_label: "operator.approval.response".into(),
        required_verified_artifact: serde_json::from_value(serde_json::json!({"mark":"operator.approval","artifact_label":"operator.approval.response",
            "verifier":"operator_approval","params":{"request_id":"blake3:00","call_id":"call-1"}})).unwrap(),
    }] }, &client, &[], &fixture.handoff.granted_mark_outcomes, &fixture.handoff.granted_tools, &fixture.handoff.granted_interfaces).unwrap();
    let context = current_context_artifacts(&published, &fixture.handoff.config).unwrap();
    let rearmed = fixture.handoff.author_run_with_bindings(&current, &context, &[PreparedHandoff {
        identity: artifact_identity(&request, &fixture.handoff.var_dir).unwrap(), output_checkpoint: client.clone(), verified_inputs: approval,
    }], false, None, false, true, &[]).unwrap();
    fixture.install(&rearmed);
    let waiting = fixture.reload_definition();
    let second_generation = waiting.checkpoints.iter().find(|cp| checkpoint_record_matches_template(cp, &wait, &waiting.id)).unwrap().clone();
    assert_eq!((second_generation.epoch.0, &second_generation.artifact_bindings), (0, &first_generation.artifact_bindings));
    result_inputs::validate_saved(&fixture.handoff.ctx, &published, &waiting, &fixture.handoff.config).unwrap();
    let mut forged = waiting.clone();
    forged.checkpoints.iter_mut().find(|cp| cp.id == second_generation.id).unwrap().artifact_bindings.pop();
    assert!(result_inputs::validate_transition(&published, &forged, &fixture.handoff.config).is_err());

    let delivered = answer("delivered", serde_json::json!({"label":"app.result","operation":"app.send","correlation_id":"call-1","ok":true,"result":{"sent":true}}));
    let current = std::fs::read(&fixture.handoff.run_path).unwrap();
    fixture.edge(&current, &second_generation.id, &delivered, "provider".into(), MovementKind::InterfaceOutbound);
    fixture.handoff.source_movement_seq = fixture.rows.len() as u64 - 1;
    fixture.handoff.invocation_metadata = fixture.rows.last().unwrap().invocation_metadata.as_ref().unwrap().invocation().unwrap().clone();
    let reply = InterfaceArtifactFile::write(fixture.handoff.var_dir.join("sent"), "reply", br#"{"text":"sent"}"#).unwrap();
    let context = current_context_artifacts(&waiting, &fixture.handoff.config).unwrap();
    let consumed = fixture.handoff.author_run_with_bindings(&current, &context, &[PreparedHandoff {
        identity: artifact_identity(&reply, &fixture.handoff.var_dir).unwrap(), output_checkpoint: client, verified_inputs: Vec::new(),
    }], false, None, false, true, &[]).unwrap();
    fixture.install(&consumed);
    let settled = fixture.reload_definition();
    assert!(!fixture.handoff.config.checkpoints.contains(&wait));
    result_inputs::validate_saved(&fixture.handoff.ctx, &waiting, &settled, &fixture.handoff.config).unwrap();
}
