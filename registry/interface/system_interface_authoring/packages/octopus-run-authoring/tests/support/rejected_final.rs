#[tokio::test]
async fn rejected_final_replacement_preserves_proof_and_recovers_crashes() {
    tokio::time::timeout(Duration::from_secs(20), async {
    use octopus_ledger::{ArrivalFanoutMetadata, ArrivalFanoutScope, ArtifactDelivery, MovementMetadata, RefusedReason};
    for mode in ["valid", "real_gate", "partial", "admitted", "unchanged", "tamper", "before_write", "after_write", "peer_advance"] {
        let mut f = CompletedInputReplayFixture::with_required_final(true);
        f.handoff.granted_interfaces = ["client", "room", "writer_room", "provider"].map(InterfaceId::new).into_iter().collect();
        f.handoff.run_path = f.handoff.run_path.canonicalize().unwrap();
        let control = f._root.path().join("control");
        f.handoff.ctx.root_slots.insert("app_server_control_root".into(), control.clone());
        f.handoff.ctx.root_slots.insert("run_records_root".into(), f._root.path().to_path_buf());
        f.handoff.ctx.root_slots.insert("interface_hand_in_root".into(), f.handoff.hand_in_root.parent().unwrap().to_path_buf());
        let socket = octopus_app_server_interface::interface_control_socket_path_from_root(&control, &f.handoff.config.authoring_source_interface);
        std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
        let listener = tokio::net::UnixListener::bind(socket).unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::channel(1); f.handoff.ctx.arrivals = tx;
        let original = InterfaceArtifactFile::write(f.handoff.var_dir.join("answer"), "answer", br#"{"value":"rejected"}"#).unwrap();
        let initial = std::fs::read(&f.handoff.run_path).unwrap();
        let first = f.handoff.author_run(&initial, &[], &[PreparedHandoff {
            identity:artifact_identity(&original,&f.handoff.var_dir).unwrap(),
            output_checkpoint:f.handoff.config.checkpoint_for_role(RunAuthoringCheckpointRole::ProviderArtifactOutput).unwrap().clone(),verified_inputs:vec![],
        }],true,None,false,true).unwrap();
        f.install(&first);
        f.handoff.state.lock().await.run_record_bytes = first.content.as_bytes().to_vec();
        let h = f.handoff.clone();
        let rejection = h.hand_pending_items(&first.pending_handoff, first.content.as_bytes());
        let gate = async {
            let incoming = rx.recv().await.unwrap(); let retained = incoming.retained.unwrap();
            let item = &first.pending_handoff.items[0];
            f.edge(first.content.as_bytes(), &item.checkpoint.id, &original, "worker".into(), MovementKind::InterfaceArrival);
            let mut arrival = f.rows.pop().unwrap();
            let mut metadata = arrival.invocation_metadata.as_ref().unwrap().invocation().unwrap().clone();
            let runtime = metadata.runtime.clone().unwrap(); let scope_seq = f.rows.len() as u64;
            let mut targets = vec![runtime.clone()];
            let mut sibling = runtime.clone(); sibling.run = "peer-run".into(); sibling.checkpoint = "peer".into();
            if matches!(mode,"partial"|"admitted") { targets.push(sibling.clone()); }
            let mut scope = arrival.clone(); scope.kind=MovementKind::ArtifactMoved; scope.to=scope.from.clone();
            scope.invocation_metadata=Some(MovementMetadata::ArrivalFanout(ArrivalFanoutMetadata {arrival_fanout:ArrivalFanoutScope {identity:retained.id,targets}}));
            f.rows.push(scope);
            metadata.delivery_scope_seq=Some(scope_seq);
            metadata.verification_failure=Some(serde_json::from_value(serde_json::json!({
                "error":"typed rejection", "verifier":"tool_call_allowed", "mark":"tool.call.allowed", "artifact_label":"answer", "run":"run", "checkpoint":item.checkpoint.id
            })).unwrap());
            arrival.invocation_metadata=Some(metadata.clone().into()); arrival.seq.0=f.rows.len() as u64;arrival.lamport.0=arrival.seq.0;f.rows.push(arrival.clone());
            arrival.kind=MovementKind::VerificationFailed;arrival.seq.0=f.rows.len() as u64;arrival.lamport.0=arrival.seq.0;f.rows.push(arrival.clone());
            if mode=="admitted" {
                metadata.runtime=Some(sibling); metadata.verification_failure=None; metadata.required_verified_artifacts.clear(); metadata.checkpoint_admission=None;
                arrival.kind=MovementKind::InterfaceArrival;arrival.seq.0=f.rows.len() as u64;arrival.lamport.0=arrival.seq.0;arrival.invocation_metadata=Some(metadata.into());f.rows.push(arrival);
            }
            f.save();
            retained.reply.send(Ok(ArtifactDelivery::Complete {verdict:DeliveryVerdict::refused(RefusedReason::VerificationFailed,serde_json::json!({}))})).unwrap();
        };
        let (rejected, ()) = tokio::join!(rejection,gate); assert!(rejected.is_err());
        f.handoff.session=ProviderHandoffSession::new(&f.handoff.ctx).unwrap();
        let selected=f.handoff.rejected_final_output().await;
        if matches!(mode,"partial"|"admitted") { assert!(selected.is_err(),"{mode}");continue; }
        let token=selected.unwrap().unwrap();
        let corrected=if mode=="unchanged" {original.clone()} else {InterfaceArtifactFile::write(f.handoff.var_dir.join("corrected"),"answer",br#"{"value":"corrected"}"#).unwrap()};
        let old_bytes=std::fs::read(original.manifest_path()).unwrap(); let old_ledger=std::fs::read(&f.handoff.ctx.movements_path).unwrap();
        if mode=="unchanged" {
            assert!(f.handoff.hand_replacement_final(&token,ProviderEmission {artifact:corrected,route:ProviderEmissionRoute::ClientProjection}).await.unwrap_err().contains("unchanged"));
            assert_eq!(std::fs::read(&f.handoff.ctx.movements_path).unwrap(),old_ledger);assert!(rx.try_recv().is_err());continue;
        }
        for attempt in 0..if matches!(mode,"before_write"|"after_write") {2}else{1} {
            let h=f.handoff.clone();
            let mut sending:std::pin::Pin<Box<dyn std::future::Future<Output=Result<ProviderHandoffOutcome,String>> + '_>> = if attempt==0 {
                Box::pin(h.hand_replacement_final(&token,ProviderEmission {artifact:corrected.clone(),route:ProviderEmissionRoute::ClientProjection}))
            } else {
                let changed=InterfaceArtifactFile::write(h.var_dir.join("changed-producer"),"answer",br#"{"value":"producer changed after crash"}"#).unwrap();
                assert_ne!(artifact_identity(&changed,&h.var_dir).unwrap().artifact,artifact_identity(&corrected,&h.var_dir).unwrap().artifact);
                Box::pin(async {h.recover_replacement_final().await?.ok_or("saved replacement missing".into())})
            };
            let record_bytes=std::fs::read(&f.handoff.run_path).unwrap(); let record=RunRecord::from_bytes(&record_bytes).unwrap();
            if !(mode=="after_write" && attempt==1) {
                let stream=tokio::select! { accepted=listener.accept()=>accepted.unwrap().0, result=&mut sending=>panic!("{mode}: early replacement {result:?}") };
                let mut ws=tokio::select! {ws=tokio_tungstenite::accept_async(stream)=>ws.unwrap(),result=&mut sending=>panic!("{result:?}")};
                let _=tokio::select! {message=ws.next()=>message.unwrap().unwrap(),result=&mut sending=>panic!("{result:?}")};
                let AuthoringAttemptRecovery::InFlight(saved)=h.recover_authoring_attempt(&record,&record_bytes).unwrap() else {panic!("replacement attempt missing")};
                assert!(saved.authored.pending_handoff.replacement.is_some());
                if mode=="peer_advance" {
                    let mut peer:serde_json::Value=serde_json::from_slice(&record_bytes).unwrap();peer["interface_state"]["peer"]=serde_json::json!({"generation":1});
                    let peer_bytes=serde_json::to_vec(&peer).unwrap();
                    std::fs::write(&h.run_path,&peer_bytes).unwrap();
                    let error=replacement_checked_write(&mut f,&saved.authored).unwrap_err();
                    assert!(error.contains("optimistic hash mismatch"),"{error}");
                    assert!(authoring_retry::rebase_authored_run(&record_bytes,&serde_json::to_vec(&peer).unwrap(),&saved.authored,&h.config,saved.authored.json_format).unwrap_err().contains("refusing rebase before write"));
                    drop(sending);assert_eq!(std::fs::read(&h.run_path).unwrap(),peer_bytes);assert!(rx.try_recv().is_err());break;
                }
                if mode=="tamper" {
                    let mut value:serde_json::Value=serde_json::from_str(&saved.authored.content).unwrap();
                    value["interface_state"]["provider"]["pending_handoff"]["replacement"]["refusals"][0]["seq"]=serde_json::json!(0);
                    let mut tampered=saved.authored.clone();
                    tampered.content=String::from_utf8(tampered.json_format.unwrap_or(JsonFormat::Compact).encode(&value).unwrap()).unwrap();
                    tampered.after_hash=ContentHash::of_bytes(tampered.content.as_bytes()).to_hex();
                    tampered.patch=json_patch::diff(&serde_json::from_slice(&record_bytes).unwrap(),&value);
                    tampered.pending_handoff=serde_json::from_value(value["interface_state"]["provider"]["pending_handoff"].clone()).unwrap();
                    drop(sending);
                    let mut state=h.state.lock().await;
                    let error=h.install_authored_run(&mut state,&tampered).await.err().expect("tampered proof was accepted");
                    assert!(error.contains("refusal evidence changed"),"{error}");
                    assert_eq!(std::fs::read(&h.run_path).unwrap(),record_bytes);
                    assert!(!h.var_dir.join("replacement-effects").exists());
                    assert!(tokio::time::timeout(Duration::from_millis(20),listener.accept()).await.is_err());
                    break;
                }
                if mode=="before_write" && attempt==0 {drop(sending);} else {
                    let seq=f.rows.len() as u64;
                    if mode=="real_gate" {replacement_checked_write(&mut f,&saved.authored).unwrap();} else {f.install(&saved.authored);}
                    if mode=="after_write" && attempt==0 {drop(sending);} else {
                        ws.send(WsMessage::Text(serde_json::to_string(&DeliveryVerdict::Admitted {seq}).unwrap().into())).await.unwrap();
                        finish_replacement(&mut f,&mut rx,sending).await;
                    }
                }
            } else {finish_replacement(&mut f,&mut rx,sending).await;}
            f.handoff.state.lock().await.run_record_bytes=std::fs::read(&f.handoff.run_path).unwrap();
            f.handoff.session=ProviderHandoffSession::new(&f.handoff.ctx).unwrap();
        }
        if matches!(mode,"tamper"|"peer_advance") {continue;}
        let before=std::fs::read(&f.handoff.run_path).unwrap();let ledger=std::fs::read(&f.handoff.ctx.movements_path).unwrap();
        assert!(matches!(f.handoff.recover_replacement_final().await.unwrap(),Some(ProviderHandoffOutcome::Admitted {artifact,..}) if artifact==artifact_identity(&corrected,&f.handoff.var_dir).unwrap().artifact));
        assert_eq!(std::fs::read(&f.handoff.run_path).unwrap(),before);assert_eq!(std::fs::read(&f.handoff.ctx.movements_path).unwrap(),ledger);
        assert!(ledger.starts_with(&old_ledger));assert_eq!(std::fs::read(original.manifest_path()).unwrap(),old_bytes);
        assert_eq!(RunRecord::from_bytes(&before).unwrap().interface_state[&InterfaceId::new("provider")]["context_generation"],serde_json::json!(1));
    }
    }).await.expect("rejected final contract exceeded its test deadline");
}

async fn finish_replacement(
    f:&mut CompletedInputReplayFixture,rx:&mut tokio::sync::mpsc::Receiver<octopus_interface_transport::InterfaceActivationArrival>,
    mut sending:std::pin::Pin<Box<dyn std::future::Future<Output=Result<ProviderHandoffOutcome,String>> + '_>>,
) {
    use octopus_ledger::{ArrivalFanoutMetadata,ArrivalFanoutScope,ArtifactDelivery,MovementMetadata};
    let incoming=tokio::select! {incoming=rx.recv()=>incoming.unwrap(),result=&mut sending=>panic!("replacement before admission: {result:?}")};
    let current=std::fs::read(&f.handoff.run_path).unwrap();let record=RunRecord::from_bytes(&current).unwrap();let pending=persisted_pending_handoff(&record,&"provider".into()).unwrap().unwrap();
    let retained=incoming.retained.unwrap();let item=&pending.items[0];
    f.edge(&current,&item.checkpoint.id,&incoming.artifact,"worker".into(),MovementKind::InterfaceArrival);
    let mut row=f.rows.pop().unwrap();let mut metadata=row.invocation_metadata.as_ref().unwrap().invocation().unwrap().clone();let scope_seq=f.rows.len() as u64;
    let mut scope=row.clone();scope.kind=MovementKind::ArtifactMoved;scope.to=scope.from.clone();
    scope.invocation_metadata=Some(MovementMetadata::ArrivalFanout(ArrivalFanoutMetadata {arrival_fanout:ArrivalFanoutScope {identity:retained.id,targets:vec![metadata.runtime.clone().unwrap()]}}));f.rows.push(scope);
    metadata.delivery_scope_seq=Some(scope_seq);row.invocation_metadata=Some(metadata.into());row.seq.0=f.rows.len() as u64;row.lamport.0=row.seq.0;let seq=row.seq.0;f.rows.push(row);
    let requirement=&item.checkpoint.required_verified_artifacts[0];
    let mut row=f.rows.last().unwrap().clone();
    let mark=octopus_interfaces::Mark::passed(requirement.mark.clone(),item.checkpoint.id.clone(),record.id,item.checkpoint.epoch,
        row.artifact.clone().unwrap(),requirement.verifier.clone(),octopus_ledger::VerifierPackagePin {
            descriptor_content:ContentHash::of_bytes(b"descriptor"),script_content:ContentHash::of_bytes(b"script"),
        },serde_json::json!({}));
    row.seq.0=f.rows.len() as u64;row.lamport.0=row.seq.0;row.kind=MovementKind::MarkOutcomeAdmitted(requirement.mark.clone());row.invocation_metadata=Some(mark.into());f.rows.push(row);
    f.save();
    retained.reply.send(Ok(ArtifactDelivery::Complete {verdict:DeliveryVerdict::Admitted {seq}})).unwrap();
    assert!(matches!(sending.await.unwrap(),ProviderHandoffOutcome::Admitted {..}));
}

fn replacement_checked_write(f:&mut CompletedInputReplayFixture,authored:&AuthoredRun)->Result<(),String> {
    let before=std::fs::read(&f.handoff.run_path).unwrap();
    let record=RunRecord::from_bytes(&before).unwrap();
    let cp=active_checkpoint_for_role(&record,&f.handoff.config,RunAuthoringCheckpointRole::AuthoringCall).unwrap().id.clone();
    let call=f.handoff.materialize_file_write_call(authored).unwrap();
    f.edge(&before,&cp,&call,authored.authoring_result_interface.clone(),MovementKind::InterfaceOutbound);
    let mut metadata=f.rows.last().unwrap().invocation_metadata.as_ref().unwrap().invocation().unwrap().clone();
    metadata.grant.worker_environment_kinds=vec!["system_room".into()];
    let envelope=InterfaceInvocationEnvelope {artifact:f.rows.last().unwrap().artifact.clone().unwrap(),artifact_file:call.clone(),artifact_binding_payloads:vec![],metadata:Some(metadata)};
    let payload:serde_json::Value=serde_json::from_slice(&call.read_payload_bytes().unwrap()).unwrap();
    let body=octopus_system_room_env::execute_file_write_result(&envelope,&payload["arguments"],&["system_room".into()],
        &BTreeMap::from([("run_records_root".into(),f.handoff.run_path.parent().unwrap().into())]),&f.handoff.var_dir.join("replacement-effects"),
        f.handoff.ctx.required_host_io_limits().unwrap().max_payload_bytes).map_err(|error|format!("{error:?}"))?;
    assert_eq!(std::fs::read(&f.handoff.run_path).unwrap(),authored.content.as_bytes());
    let result=InterfaceArtifactFile::write(f.handoff.var_dir.join("real-results"),"write.result",
        &serde_json::to_vec(&serde_json::json!({"label":"write.result","tool":"write","correlation_id":authored.correlation_id,"ok":true,"body":body})).unwrap()).unwrap();
    f.edge(authored.content.as_bytes(),&authored.authoring_result_checkpoint,&result,"provider".into(),MovementKind::InterfaceOutbound);
    let evidence=CheckedRunEvidence {movements_path:&f.handoff.ctx.movements_path,ledger_index_path:&f.handoff.ctx.ledger_index_path,blobs_dir:&f.handoff.ctx.blobs_dir,
        max_control_frame_bytes:1<<20,max_manifest_bytes:1<<20,max_payload_bytes:1<<24};
    let writer=declared_provider_run_writer(&record,&f.handoff.ctx.interface,&RunAuthoringProfile {file_write_tool:"write".into(),file_write_label:"tool.call".into(),file_write_result_label:"write.result".into()}).unwrap();
    let install=CheckedRunInstall {call:ConsentEvidenceRef {artifact:f.rows[f.rows.len()-2].artifact.clone().unwrap(),movement_seq:f.rows.len() as u64-2},
        result:ConsentEvidenceRef {artifact:f.rows.last().unwrap().artifact.clone().unwrap(),movement_seq:f.rows.len() as u64-1}};
    checked_declared_run_install(&evidence,&f.handoff.run_path,&before,&install,vec![writer])?;
    Ok(())
}

#[tokio::test]
async fn rejected_output_closes_through_its_failed_source_and_the_next_input_passes() {
    tokio::time::timeout(Duration::from_secs(20), async {
    use octopus_ledger::{ArrivalFanoutMetadata, ArrivalFanoutScope, ArtifactDelivery, MovementMetadata, RefusedReason};
    type Sending<'a> = std::pin::Pin<Box<dyn std::future::Future<Output=Result<ProviderHandoffOutcome,String>> + 'a>>;
    let mut f = CompletedInputReplayFixture::with_required_final(true);
    f.handoff.granted_interfaces = ["client", "room", "writer_room", "provider"].map(InterfaceId::new).into_iter().collect();
    f.handoff.run_path = f.handoff.run_path.canonicalize().unwrap();
    let control = f._root.path().join("control");
    f.handoff.ctx.root_slots.insert("app_server_control_root".into(), control.clone());
    f.handoff.ctx.root_slots.insert("run_records_root".into(), f._root.path().to_path_buf());
    f.handoff.ctx.root_slots.insert("interface_hand_in_root".into(), f.handoff.hand_in_root.parent().unwrap().to_path_buf());
    let socket = octopus_app_server_interface::interface_control_socket_path_from_root(&control, &f.handoff.config.authoring_source_interface);
    std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
    let listener = tokio::net::UnixListener::bind(socket).unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::channel(1); f.handoff.ctx.arrivals = tx;
    let only = |label: &str| BindingArtifactPolicy::Allow { names: vec![label.into()], namespaces: Vec::new() };
    let client = f.handoff.config.checkpoints.iter_mut().find(|cp| cp.name == "client").unwrap();
    client.interface_binding.config.artifact_policy = only("answer");
    client.outbound_interface_bindings[0].config.artifact_policy = only("answer");
    let client = client.clone();
    let mut errors = client.clone();
    errors.name = "errors".into();
    errors.required_verified_artifacts.clear();
    errors.interface_binding.config.artifact_policy = only("turn.error");
    errors.outbound_interface_bindings[0].config.artifact_policy = only("turn.error");
    f.handoff.config.checkpoints.push(errors.clone());
    f.handoff.bound_topology.checkpoints = f.handoff.config.checkpoints.clone();
    let mut seed: serde_json::Value = serde_json::from_slice(&std::fs::read(&f.handoff.run_path).unwrap()).unwrap();
    seed["interface_state"]["provider"][RUN_AUTHORING_BINDING] = serde_json::to_value(&f.handoff.bound_topology).unwrap();
    *seed["checkpoints"].as_array_mut().unwrap().iter_mut().find(|cp| cp["id"] == "client").unwrap() =
        materialize_checkpoint(&client, "client", &f.handoff.config.run, &[]);
    seed["checkpoints"].as_array_mut().unwrap().push(materialize_checkpoint(&errors, "errors", &f.handoff.config.run, &[]));
    seed["active_checkpoints"].as_array_mut().unwrap().push(serde_json::json!("errors"));
    seed["interface_state"]["provider"]["frontier_checkpoint_ids"].as_array_mut().unwrap().push(serde_json::json!("errors"));
    let initial = serde_json::to_vec(&seed).unwrap();
    std::fs::write(&f.handoff.run_path, &initial).unwrap();
    f.handoff.state.lock().await.run_record_bytes = initial.clone();
    f.rows.clear();
    let input_file = f.input_file.clone();
    f.edge(&initial, &"input".into(), &input_file, "provider".into(), MovementKind::InterfaceOutbound);
    f.edge(&initial, &"alternate".into(), &input_file, "provider".into(), MovementKind::InterfaceOutbound);
    let rejected_file = InterfaceArtifactFile::write(f.handoff.var_dir.join("answer"), "answer", br#"{"value":"rejected"}"#).unwrap();
    let first = f.handoff.author_run(&initial, &[], &[PreparedHandoff {
        identity:artifact_identity(&rejected_file,&f.handoff.var_dir).unwrap(),
        output_checkpoint:f.handoff.config.checkpoints.iter().find(|cp| cp.name == "client").unwrap().clone(),verified_inputs:vec![],
    }],true,None,false,true).unwrap();
    f.install(&first);
    f.handoff.state.lock().await.run_record_bytes = first.content.as_bytes().to_vec();
    let h = f.handoff.clone();
    let rejection = h.hand_pending_items(&first.pending_handoff, first.content.as_bytes());
    let gate = async {
        let incoming = rx.recv().await.unwrap(); let retained = incoming.retained.unwrap();
        let item = &first.pending_handoff.items[0];
        f.edge(first.content.as_bytes(), &item.checkpoint.id, &rejected_file, "worker".into(), MovementKind::InterfaceArrival);
        let mut arrival = f.rows.pop().unwrap();
        let mut metadata = arrival.invocation_metadata.as_ref().unwrap().invocation().unwrap().clone();
        let scope_seq = f.rows.len() as u64;
        let mut scope = arrival.clone(); scope.kind=MovementKind::ArtifactMoved; scope.to=scope.from.clone();
        scope.invocation_metadata=Some(MovementMetadata::ArrivalFanout(ArrivalFanoutMetadata {arrival_fanout:ArrivalFanoutScope {identity:retained.id,targets:vec![metadata.runtime.clone().unwrap()]}}));
        f.rows.push(scope);
        metadata.delivery_scope_seq=Some(scope_seq);
        metadata.verification_failure=Some(serde_json::from_value(serde_json::json!({
            "error":"verifier process timed out", "verifier":"tool_call_allowed", "mark":"tool.call.allowed", "artifact_label":"answer", "run":"run", "checkpoint":item.checkpoint.id
        })).unwrap());
        arrival.invocation_metadata=Some(metadata.into()); arrival.seq.0=f.rows.len() as u64;arrival.lamport.0=arrival.seq.0;f.rows.push(arrival.clone());
        arrival.kind=MovementKind::VerificationFailed;arrival.seq.0=f.rows.len() as u64;arrival.lamport.0=arrival.seq.0;f.rows.push(arrival);
        f.save();
        retained.reply.send(Ok(ArtifactDelivery::Complete {verdict:DeliveryVerdict::refused(RefusedReason::VerificationFailed,serde_json::json!({}))})).unwrap();
    };
    let (rejected, ()) = tokio::join!(rejection,gate); assert!(rejected.is_err());
    drop(h);

    let mut failed = f.handoff.clone();
    failed.failed_source = true;
    failed.session = ProviderHandoffSession::new(&failed.ctx).unwrap();
    {
        let mut state = failed.state.lock().await;
        let record = RunRecord::from_bytes(&state.run_record_bytes).unwrap();
        state.context_artifacts = failed.published_context_artifacts(&record, &failed.config).unwrap();
    }
    let error = InterfaceArtifactFile::write(f.handoff.var_dir.join("emitted"), "turn.error",
        br#"{"label":"turn.error","message":"the call was rejected","will_retry":false,"additional_details":{"source_movement_seq":0}}"#).unwrap();
    let closing: Sending<'_> = Box::pin(async {
        failed.hand_failed_source(ProviderEmission {artifact:error.clone(),route:ProviderEmissionRoute::ClientProjection}).await.map_err(|error| format!("{error:?}"))
    });
    let closed = write_then_admit(&mut f, &failed, &listener, &mut rx, closing).await;
    let pending = persisted_pending_handoff(&RunRecord::from_bytes(&closed).unwrap(), &"provider".into()).unwrap().unwrap();
    assert!(pending.terminal && pending.source_without_context && pending.source_movement_seq == 0);
    assert_eq!(pending.items.iter().map(|item| item.artifact_label.as_str()).collect::<Vec<_>>(), vec!["turn.error"],
        "the failed source replaces the rejected output it could never deliver");

    let mut next = f.handoff.clone();
    next.source_movement_seq = 1;
    next.session = ProviderHandoffSession::new(&next.ctx).unwrap();
    next.state.lock().await.run_record_bytes = closed.clone();
    let recovered = next.checked_failed_source_contexts(&closed, &[], None).unwrap();
    assert!(recovered[&0].0.artifacts.iter().any(|entry| entry.source_artifact == f.input),
        "the next input recovers what the failed source read, proven by the provider's own checked writes");
    let answer = InterfaceArtifactFile::write(f.handoff.var_dir.join("next"), "answer", br#"{"value":"after the rejection"}"#).unwrap();
    let passing: Sending<'_> = Box::pin(next.hand_final(ProviderEmission {artifact:answer.clone(),route:ProviderEmissionRoute::ClientProjection}));
    let passed = write_then_admit(&mut f, &next, &listener, &mut rx, passing).await;
    let pending = persisted_pending_handoff(&RunRecord::from_bytes(&passed).unwrap(), &"provider".into()).unwrap().unwrap();
    assert!(pending.source_movement_seq == 1 && pending.items[0].artifact_label == "answer");
    }).await.expect("rejected output recovery exceeded its test deadline");
}

async fn write_then_admit<'a>(
    f:&mut CompletedInputReplayFixture,handoff:&ProviderOutputHandoff,listener:&tokio::net::UnixListener,
    rx:&mut tokio::sync::mpsc::Receiver<octopus_interface_transport::InterfaceActivationArrival>,
    mut sending:std::pin::Pin<Box<dyn std::future::Future<Output=Result<ProviderHandoffOutcome,String>> + 'a>>,
) -> Vec<u8> {
    use octopus_ledger::{ArrivalFanoutMetadata,ArrivalFanoutScope,ArtifactDelivery,MovementMetadata};
    let stream=tokio::select! {accepted=listener.accept()=>accepted.unwrap().0,result=&mut sending=>panic!("ended before its write: {result:?}")};
    let mut ws=tokio::select! {ws=tokio_tungstenite::accept_async(stream)=>ws.unwrap(),result=&mut sending=>panic!("{result:?}")};
    let _=tokio::select! {message=ws.next()=>message.unwrap().unwrap(),result=&mut sending=>panic!("{result:?}")};
    let before=std::fs::read(&handoff.run_path).unwrap();
    let AuthoringAttemptRecovery::InFlight(saved)=handoff.recover_authoring_attempt(&RunRecord::from_bytes(&before).unwrap(),&before).unwrap() else {panic!("authoring attempt missing")};
    let seq=f.rows.len() as u64;
    replacement_checked_write(f,&saved.authored).unwrap();
    ws.send(WsMessage::Text(serde_json::to_string(&DeliveryVerdict::Admitted {seq}).unwrap().into())).await.unwrap();
    let incoming=tokio::select! {incoming=rx.recv()=>incoming.unwrap(),result=&mut sending=>panic!("ended before admission: {result:?}")};
    let current=std::fs::read(&handoff.run_path).unwrap();let record=RunRecord::from_bytes(&current).unwrap();
    let pending=persisted_pending_handoff(&record,&"provider".into()).unwrap().unwrap();
    let retained=incoming.retained.unwrap();let item=&pending.items[0];
    f.edge(&current,&item.checkpoint.id,&incoming.artifact,"worker".into(),MovementKind::InterfaceArrival);
    let mut row=f.rows.pop().unwrap();let mut metadata=row.invocation_metadata.as_ref().unwrap().invocation().unwrap().clone();let scope_seq=f.rows.len() as u64;
    let mut scope=row.clone();scope.kind=MovementKind::ArtifactMoved;scope.to=scope.from.clone();
    scope.invocation_metadata=Some(MovementMetadata::ArrivalFanout(ArrivalFanoutMetadata {arrival_fanout:ArrivalFanoutScope {identity:retained.id,targets:vec![metadata.runtime.clone().unwrap()]}}));f.rows.push(scope);
    metadata.delivery_scope_seq=Some(scope_seq);row.invocation_metadata=Some(metadata.into());row.seq.0=f.rows.len() as u64;row.lamport.0=row.seq.0;let seq=row.seq.0;f.rows.push(row);
    for requirement in item.checkpoint.required_verified_artifacts.iter().filter(|requirement| requirement.artifact_label==item.artifact_label) {
        let mut row=f.rows.last().unwrap().clone();
        let mark=octopus_interfaces::Mark::passed(requirement.mark.clone(),item.checkpoint.id.clone(),record.id.clone(),item.checkpoint.epoch,
            row.artifact.clone().unwrap(),requirement.verifier.clone(),octopus_ledger::VerifierPackagePin {
                descriptor_content:ContentHash::of_bytes(b"descriptor"),script_content:ContentHash::of_bytes(b"script"),
            },serde_json::json!({}));
        row.seq.0=f.rows.len() as u64;row.lamport.0=row.seq.0;row.kind=MovementKind::MarkOutcomeAdmitted(requirement.mark.clone());row.invocation_metadata=Some(mark.into());f.rows.push(row);
    }
    f.save();
    retained.reply.send(Ok(ArtifactDelivery::Complete {verdict:DeliveryVerdict::Admitted {seq}})).unwrap();
    assert!(matches!(sending.await.unwrap(),ProviderHandoffOutcome::Admitted {..}));
    current
}

