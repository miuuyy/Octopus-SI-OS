#[path = "../../../../../world_operator_chat/packages/octopus-operator-chat-client/tests/support/composition_fixture.rs"]
mod terminal_composition;

#[test]
fn native_terminal_answer_and_closure_reach_the_real_authoring_gate() {
    std::thread::Builder::new().stack_size(16 * 1024 * 1024).spawn(|| {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
            .block_on(native_terminal_handoff_cases(None));
    }).unwrap().join().unwrap();
}

#[test]
fn native_progress_is_admitted_before_the_next_provider_output() {
    std::thread::Builder::new().stack_size(16 * 1024 * 1024).spawn(|| {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
            .block_on(native_terminal_handoff_cases(Some(25)));
    }).unwrap().join().unwrap();
}

#[test]
fn registry_unstable_pending_keeps_native_turn_and_delivers_exact_tool_output() {
    std::thread::Builder::new().stack_size(16 * 1024 * 1024).spawn(|| {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
            .block_on(native_terminal_handoff_cases(Some(26)));
    }).unwrap().join().unwrap();
}

include!("retained_native_delivery.rs");

async fn native_terminal_handoff_cases(selected_phase: Option<usize>) {
    use octopus_interfaces::DeliveryVerdict;
    use octopus_run_authoring::ProviderSourceRecovery;
    use octopus_run_record::RunRecord;
    fn edge(ctx: &InterfaceLiftContext, bytes: &[u8], checkpoint: &octopus_run_record::CheckpointRecord,
        file: &InterfaceArtifactFile, kind: MovementKind, target: InterfaceId) -> Movement {
        let record = RunRecord::from_bytes(bytes).unwrap();
        let tool=&ctx.descriptor_snapshot.as_ref().unwrap().interface_driver_config.as_ref().unwrap().private_config["run_authoring_profile"]["file_write_tool"];
        let mut tools=vec![tool.clone()];
        let mut marks=record.checkpoints.iter().flat_map(|cp|cp.required_verified_artifacts.iter().map(|r|r.mark.clone())).collect::<BTreeSet<_>>();
        for template in record.interface_state[&ctx.interface].get("run_authoring_binding").and_then(|binding|binding["checkpoints"].as_array()).into_iter().flatten() {
            if template["role"]=="provider_tool_output" {
                tools.extend(template["required_verified_artifacts"][0]["params"]["tools"].as_array().unwrap().iter().cloned());
                for requirement in template["result_receiving"].as_object().into_iter().flat_map(|receiving| receiving.values()) {
                    marks.insert(serde_json::from_value(requirement["mark"].clone()).unwrap());
                }
            }
        }
        let interfaces=record.checkpoints.iter().flat_map(|cp|cp.inbound_interface().into_iter().chain(cp.outbound_interfaces())).cloned().collect::<BTreeSet<_>>();
        let artifact = cache_artifact(ctx, file);
        let hash = ContentHash::of_bytes(bytes);
        std::fs::write(ctx.blobs_dir.join(hash.to_hex()), bytes).unwrap();
        let meta: InvocationMetadata = serde_json::from_value(json!({
            "runtime":{"run":record.id,"ee":record.ee,"worker":record.worker,"checkpoint":checkpoint.id,
                "interface":if kind == MovementKind::InterfaceArrival { ctx.interface.clone() } else { target.clone() },"epoch":checkpoint.epoch.0,"run_record_hash":hash.to_hex()},
            "grant":{"tools":tools,"interfaces":interfaces,"worker_environment_kinds":["system_room"],"mark_outcomes":marks,
                "role_artifacts":checkpoint.artifact_bindings.iter().filter(|b|record.interface_state.get(&ctx.interface).and_then(|state|state.get("run_authoring_binding")).and_then(|binding|binding.get("checkpoints")).and_then(serde_json::Value::as_array).is_some_and(|templates|templates.iter().any(|template|template["static_artifact_bindings"].as_array().is_some_and(|ids|ids.contains(&json!(b.id))))) && !record.interface_state[&ctx.interface]["run_authoring_binding"]["payload_artifact_bindings"].as_array().is_some_and(|refs|refs.iter().any(|reference|reference["id"]==b.id.as_str()))).map(|b|b.id.clone()).collect::<Vec<_>>(),
                "artifact_access":checkpoint.artifact_bindings.iter().map(|b|(b.id.to_string(),b.access.clone())).collect::<BTreeMap<_,_>>()},
            "artifact_binding_refs":checkpoint.artifact_bindings.iter().map(|b| { let content=b.id.as_str().rsplit(':').next().and_then(ContentHash::from_hex).filter(|h|ctx.blobs_dir.join(h.to_hex()).exists()).unwrap_or_else(|| { let payload=std::fs::read(ctx.interface_dir.parent().unwrap().parent().unwrap().join("artifact").join(b.id.as_str()).join("artifact.json")).unwrap(); let hash=ContentHash::of_bytes(&payload); std::fs::write(ctx.blobs_dir.join(hash.to_hex()),payload).unwrap(); hash }); ArtifactRef{id:b.id.clone(),content} }).collect::<Vec<_>>(),
            "checkpoint_binding_policy":{"inbound":checkpoint.interface_binding,"outbound":checkpoint.outbound_interface_bindings},
            "required_verified_artifacts":checkpoint.required_verified_artifacts,
            "outbound_interface_bindings":checkpoint.outbound_interfaces().cloned().collect::<Vec<_>>(),
            "checkpoint_admission":{"complete":true,"artifact_label":file.read_manifest().unwrap().label,"required_artifact_labels":checkpoint.required_verified_artifacts.iter().map(|requirement|requirement.artifact_label.clone()).collect::<Vec<_>>()}
        })).unwrap();
        Movement { seq:Seq(0), lamport:Lamport(1), recorded_at_unix_ms:1, trace:checkpoint.trace.clone(), kind,
            from:Endpoint::Interface(checkpoint.inbound_interface().unwrap().clone()), to:Endpoint::Interface(target),
            artifact:Some(artifact), invocation_metadata:Some(meta.into()) }
    }
    fn append(ctx: &InterfaceLiftContext, mut row: Movement) -> u64 {
        let count=std::fs::read(&ctx.movements_path).unwrap().split(|b|*b==b'\n').filter(|r|!r.is_empty()).count() as u64;
        row.seq=Seq(count); row.lamport=Lamport(count+1); row.recorded_at_unix_ms=count+1;
        append_movement(&ctx.movements_path,&row); count
    }
    fn retain(ctx: &InterfaceLiftContext, incoming: &octopus_interface_transport::InterfaceActivationArrival, row: &Movement) -> Option<u64> {
        let retained=incoming.retained.as_ref()?;
        for line in std::fs::read_to_string(&ctx.movements_path).unwrap().lines() {
            let scope:Movement=serde_json::from_str(line).unwrap();
            if matches!(&scope.invocation_metadata,Some(octopus_ledger::MovementMetadata::ArrivalFanout(metadata)) if metadata.arrival_fanout.identity==retained.id) {
                assert_eq!(scope.artifact,row.artifact);return Some(scope.seq.0);
            }
        }
        let mut scope=row.clone();scope.kind=MovementKind::ArtifactMoved;scope.to=scope.from.clone();
        scope.invocation_metadata=Some(octopus_ledger::MovementMetadata::ArrivalFanout(octopus_ledger::ArrivalFanoutMetadata {
            arrival_fanout:octopus_ledger::ArrivalFanoutScope {identity:retained.id.clone(),targets:vec![row.invocation_metadata.as_ref().unwrap().invocation().unwrap().runtime.clone().unwrap()]},
        }));
        Some(append(ctx,scope))
    }
    async fn pump(gate_ctx: &InterfaceLiftContext, root: &Path, path: &Path, authoring_id: &InterfaceId,
        listener: &tokio::net::UnixListener, receiver: &mut tokio::sync::mpsc::Receiver<octopus_interface_transport::InterfaceActivationArrival>,
        expected_source: &ArtifactRef, delivered_before: usize, count: usize, with_answer: bool, phase: usize, ack_bootstrap: bool,
    ) -> Vec<(String,ArtifactRef,u64)> {
            let mut delivered=Vec::new(); let mut writes=0;
            let mut transient_scope = None;
            let mut before_scope_refused = false;
            loop { tokio::select! {
                accepted=listener.accept()=> {
                    let mut ws=tokio_tungstenite::accept_async(accepted.unwrap().0).await.unwrap();
                    let manifest:octopus_interfaces::InterfaceArtifactManifest=serde_json::from_str(&ws.next().await.unwrap().unwrap().into_text().unwrap()).unwrap();
                    if phase==8 || (phase==20 && writes==1) { break; }
                    let file=std::fs::read_dir(root.join("hand-in").join(authoring_id.as_str())).unwrap().map(|e|e.unwrap().path()).filter(|p|p.to_string_lossy().ends_with(".artifact.json")).find_map(|p| { let f=InterfaceArtifactFile::new(p).unwrap(); (serde_json::to_value(f.read_manifest().unwrap()).unwrap()==serde_json::to_value(&manifest).unwrap()).then_some(f) }).unwrap();
                    let target_before=std::fs::read(path).unwrap();
                    let selected:serde_json::Value=serde_json::from_slice(&target_before).unwrap();
                    let writer_path=selected["interface_state"][gate_ctx.interface.as_str()]["run_authoring_binding"]["writer_run"]["run"].as_str()
                        .map_or_else(||path.to_path_buf(),|run|root.join("runs").join(format!("{run}.json")));
                    let before=std::fs::read(&writer_path).unwrap(); let before_record=RunRecord::from_bytes(&before).unwrap();
                    let call_cp=before_record.checkpoints.iter().find(|cp|before_record.active_checkpoints.contains(&cp.id)&&cp.inbound_interface()==Some(authoring_id)).unwrap();
                    let door=call_cp.outbound_interfaces().next().unwrap().clone(); let seq=append(&gate_ctx,edge(&gate_ctx,&before,call_cp,&file,MovementKind::InterfaceOutbound,door.clone()));
                    let overlap=if phase==24 && writes==1 {
                        let record=RunRecord::from_bytes(&target_before).unwrap();
                        let cp=record.checkpoints.iter().find(|cp|record.active_checkpoints.contains(&cp.id)
                            && cp.outbound_interfaces().any(|id|id==&gate_ctx.interface) && cp.interface_binding.as_ref().unwrap().admits("operator.message")).unwrap();
                        let input=artifact(&root.join("clarification"),"operator.message",json!({"text":"clarification","message_id":"steer-pending"}));
                        Some(append(gate_ctx,edge(gate_ctx,&target_before,cp,&input,MovementKind::InterfaceOutbound,gate_ctx.interface.clone())))
                    } else {None};
                    let call:octopus_tool_handlers::ToolCallArtifact=serde_json::from_slice(&file.read_payload_bytes().unwrap()).unwrap();
                    let call_metadata=edge(gate_ctx,&before,call_cp,&file,MovementKind::InterfaceOutbound,door.clone()).invocation_metadata.unwrap().invocation().unwrap().clone();
                    let call_envelope=InterfaceInvocationEnvelope{artifact:cache_artifact(gate_ctx,&file),artifact_file:file,artifact_binding_payloads:vec![],metadata:Some(call_metadata)};
                    let body=octopus_system_room_env::execute_file_write_result(&call_envelope,&call.arguments,&["system_room".into()],&BTreeMap::from([("run_records_root".into(),root.join("runs"))]),&root.join(format!("effects-{writes}")),1<<24).unwrap();
                    if let Some(source)=overlap {
                        use octopus_run_authoring::{CheckedRunEvidence,RunAuthoringProfile,declared_provider_run_writer,checked_declared_run_advance,CheckedRunError};
                        let profile:RunAuthoringProfile=serde_json::from_value(gate_ctx.descriptor_snapshot.as_ref().unwrap().interface_driver_config.as_ref().unwrap().private_config["run_authoring_profile"].clone()).unwrap();
                        let writer=declared_provider_run_writer(&RunRecord::from_bytes(&target_before).unwrap(),&gate_ctx.interface,&profile).unwrap();
                        let limits=gate_ctx.required_host_io_limits().unwrap();
                        let evidence=CheckedRunEvidence{movements_path:&gate_ctx.movements_path,ledger_index_path:&gate_ctx.ledger_index_path,blobs_dir:&gate_ctx.blobs_dir,max_control_frame_bytes:limits.max_control_frame_bytes,max_manifest_bytes:limits.max_manifest_bytes,max_payload_bytes:limits.max_payload_bytes};
                        let pending=checked_declared_run_advance(&evidence,&path.canonicalize().unwrap(),&target_before,&std::fs::read(path).unwrap(),source,vec![writer]);
                        assert!(matches!(&pending, Err(CheckedRunError::AwaitingResults{call_sequences}) if call_sequences==&[seq]), "call={seq} source={source}: {:?}",pending.as_ref().err());
                    }
                    let after=std::fs::read(&writer_path).unwrap(); let after_record=RunRecord::from_bytes(&after).unwrap();
                    let result_cp=after_record.checkpoints.iter().find(|cp|after_record.active_checkpoints.contains(&cp.id)&&cp.inbound_interface()==Some(&door)&&cp.outbound_interfaces().any(|id|id==&gate_ctx.interface)).unwrap();
                    let label=gate_ctx.descriptor_snapshot.as_ref().unwrap().interface_driver_config.as_ref().unwrap().private_config["run_authoring_profile"]["file_write_result_label"].as_str().unwrap();
                    let receipt=artifact(&root.join("writes"),label,json!({"label":label,"tool":call.tool,"correlation_id":call.correlation_id,"ok":true,"body":body}));
                    append(&gate_ctx,edge(&gate_ctx,&after,result_cp,&receipt,MovementKind::InterfaceOutbound,gate_ctx.interface.clone()));
                    let selected_bytes=std::fs::read(path).unwrap();let selected=RunRecord::from_bytes(&selected_bytes).unwrap();
                    let pending=&selected.interface_state[&gate_ctx.interface]["pending_handoff"];
                    if count==2 && delivered.len()==1 && pending["withdrawal"].is_object() && pending["context_publication"].is_null() {
                        use octopus_run_authoring::{CheckedRunEvidence,RunAuthoringProfile,declared_provider_run_writer};
                        let profile:RunAuthoringProfile=serde_json::from_value(gate_ctx.descriptor_snapshot.as_ref().unwrap().interface_driver_config.as_ref().unwrap().private_config["run_authoring_profile"].clone()).unwrap();
                        let writer=declared_provider_run_writer(&selected,&gate_ctx.interface,&profile).unwrap();
                        let limits=gate_ctx.required_host_io_limits().unwrap();let evidence=CheckedRunEvidence {movements_path:&gate_ctx.movements_path,ledger_index_path:&gate_ctx.ledger_index_path,blobs_dir:&gate_ctx.blobs_dir,max_control_frame_bytes:limits.max_control_frame_bytes,max_manifest_bytes:limits.max_manifest_bytes,max_payload_bytes:limits.max_payload_bytes};
                        let source=selected.interface_state[&gate_ctx.interface]["run_authoring_binding"]["checkpoints"].as_array().unwrap().iter().find(|template|template["role"]=="stable"&&template["include_context"]==true).unwrap()["interface_binding"]["interface"].as_str().unwrap();
                        assert!(!octopus_run_authoring::provider_input_payloads_ready(&evidence,&path.canonicalize().unwrap(),&selected_bytes,&writer,&InterfaceId::new(source)).unwrap());
                    }
                    ws.send(tokio_tungstenite::tungstenite::Message::Text(serde_json::to_string(&DeliveryVerdict::Admitted{seq}).unwrap().into())).await.unwrap(); writes+=1;
                    if delivered_before+delivered.len()==count { break; }
                }
                incoming=receiver.recv()=> {
                    let incoming=incoming.unwrap(); let current=std::fs::read(&path).unwrap(); let current_record=RunRecord::from_bytes(&current).unwrap();
                    let label=incoming.artifact.read_manifest().unwrap().label;
                    let pending=&current_record.interface_state[&gate_ctx.interface]["pending_handoff"];
                    let items=pending["items"].as_array().unwrap();
                    let (cp,standing)=match items.iter().find(|item|item["artifact_label"]==label) {
                        Some(item) => {
                            if phase != 26 {
                                assert_eq!(items.len(),if ack_bootstrap || with_answer || count==2 || label==TURN_INTERRUPTED_RESULT_LABEL {1}else{2});
                                assert_eq!(pending["terminal"],ack_bootstrap || items.last().unwrap()["artifact_label"]==TURN_INTERRUPTED_RESULT_LABEL);
                            }
                            (current_record.checkpoint(&serde_json::from_value(item["checkpoint"]["id"].clone()).unwrap()).unwrap(),false)
                        }
                        None => {
                            let standing=current_record.checkpoints.iter().filter(|cp|current_record.active_checkpoints.contains(&cp.id)
                                && cp.id.as_str().contains("_standing_") && cp.inbound_interface()==Some(&gate_ctx.interface)
                                && cp.interface_binding.as_ref().unwrap().admits(&label)).collect::<Vec<_>>();
                            let [route]=standing.as_slice() else { panic!("'{label}' rides neither a pending item nor one standing route") };
                            (*route,true)
                        }
                    };
                    let mut row=edge(&gate_ctx,&current,cp,&incoming.artifact,MovementKind::InterfaceArrival,current_record.worker.clone());
                    if phase == 26 && !before_scope_refused {
                        before_scope_refused = true;
                        incoming.retained.unwrap().reply.send(Ok(octopus_ledger::ArtifactDelivery::Complete {
                            verdict: DeliveryVerdict::refused(octopus_ledger::RefusedReason::RegistryUnstable, json!({"attempts":4})),
                        })).unwrap();
                        continue;
                    }
                    let scope=retain(gate_ctx,&incoming,&row);
                    if (phase==2 && label==TURN_INTERRUPTED_RESULT_LABEL) || (!ack_bootstrap && with_answer && phase==0 && label=="turn.completed") {
                        incoming.retained.unwrap().reply.send(Err("fixture transport closed after retaining scope".into())).unwrap();break;
                    }
                    let mut metadata=row.invocation_metadata.as_ref().unwrap().invocation().unwrap().clone();metadata.delivery_scope_seq=scope;row.invocation_metadata=Some(metadata.into());
                    if phase == 26 && delivered.is_empty() {
                        let retained = incoming.retained.as_ref().unwrap();
                        let exact = (retained.id.clone(), scope.unwrap(), row.artifact.clone());
                        if let Some(original) = &transient_scope { assert_eq!(&exact, original); }
                        else {
                            transient_scope = Some(exact);
                            let mut refused = row.clone(); refused.kind = MovementKind::DeliveryRefused;
                            refused.invocation_metadata = Some(octopus_ledger::MovementMetadata::Delivery(octopus_ledger::DeliveryMetadata {
                                delivery_verdict: DeliveryVerdict::refused(octopus_ledger::RefusedReason::RegistryUnstable, json!({"message":"registry changed before admission"})),
                                runtime: row.invocation_metadata.as_ref().unwrap().invocation().unwrap().runtime.clone(),
                                source_movement_seq: None, delivery_scope_seq: scope,
                            }));
                            append(gate_ctx, refused);
                            incoming.retained.unwrap().reply.send(Ok(octopus_ledger::ArtifactDelivery::Pending { scope_seq: scope.unwrap() })).unwrap();
                            continue;
                        }
                    }
                    let seq=append(&gate_ctx,row);
                    let payload:serde_json::Value=serde_json::from_slice(&incoming.artifact.read_payload_bytes().unwrap()).unwrap_or(json!({}));
                    if let Some(continuity)=payload.get("codex_continuity") {
                        let persisted=&current_record.interface_state[&gate_ctx.interface]["context_generation"];
                        if standing { assert!(continuity["context_generation"].as_u64().unwrap()>=persisted.as_u64().unwrap()); }
                        else { assert_eq!(&continuity["context_generation"],persisted); }
                    }
                    if label==TURN_INTERRUPTED_RESULT_LABEL { assert_eq!(payload["source_artifact"],json!(expected_source)); }
                    delivered.push((label,cache_artifact(&gate_ctx,&incoming.artifact),seq));
                    if let Some(retained)=incoming.retained {retained.reply.send(Ok(octopus_ledger::ArtifactDelivery::Complete {verdict:DeliveryVerdict::Admitted{seq}})).unwrap();}
                    else {incoming.verdict_tx.unwrap().send(DeliveryVerdict::Admitted{seq}).unwrap();}
                    if phase==3 || phase==22 || (standing && delivered_before+delivered.len()==count) { break; }
                }
            }}
            delivered
    }
    fn source_envelope(ctx: &InterfaceLiftContext, root: &Path, row: &Movement, file: InterfaceArtifactFile) -> InterfaceInvocationEnvelope {
        let metadata=row.invocation_metadata.as_ref().unwrap().invocation().unwrap().clone();
        let payloads=metadata.artifact_binding_refs.iter().map(|reference| {
            let bytes=if reference.id.as_str().rsplit(':').next().and_then(ContentHash::from_hex).is_some() {
                let manifest:octopus_interfaces::InterfaceArtifactManifest=serde_json::from_slice(&std::fs::read(ctx.blobs_dir.join(reference.content.to_hex())).unwrap()).unwrap();
                std::fs::read(ctx.blobs_dir.join(manifest.payload_content_hash.unwrap())).unwrap()
            } else { std::fs::read(root.join("registry/artifact").join(reference.id.as_str()).join("artifact.json")).unwrap() };
            octopus_interfaces::ArtifactBindingPayload::new(reference.clone(),bytes)
        }).collect();
        InterfaceInvocationEnvelope {artifact:row.artifact.clone().unwrap(),artifact_file:file,artifact_binding_payloads:payloads,metadata:Some(metadata)}
    }
    async fn assert_saved_snapshot_authority(
        ctx:&InterfaceLiftContext,root:&Path,run_path:&Path,descriptor:&InterfaceDescriptor,run:&RunId,
        envelope:&InterfaceInvocationEnvelope,result_seq:u64,pinned:&[u8],selected:&[u8],ledger:&[u8],
        saved:&octopus_run_authoring::PendingSourceWithdrawal,authoring_id:&InterfaceId,phase:usize,stop_seq:u64,
        receiver:&mut tokio::sync::mpsc::Receiver<octopus_interface_transport::InterfaceActivationArrival>,
    ) {
    use octopus_run_authoring::{CheckedRunEdit,CheckedRunEvidence,declared_provider_run_writer,RunAuthoringProfile};
    let profile:RunAuthoringProfile=serde_json::from_value(descriptor.interface_driver_config.as_ref().unwrap().private_config["run_authoring_profile"].clone()).unwrap();
    let installed_record=RunRecord::from_bytes(selected).unwrap();
    let writer=declared_provider_run_writer(&installed_record,&descriptor.id,&profile).unwrap();
    let calls=root.join("hand-in").join(authoring_id.as_str());
    let (call_file,call_value)=std::fs::read_dir(&calls).unwrap().map(|entry|entry.unwrap().path())
        .filter(|path|path.to_string_lossy().ends_with(".artifact.json")).find_map(|path| {
            let file=InterfaceArtifactFile::new(path).unwrap();
            let value:serde_json::Value=serde_json::from_slice(&file.read_payload_bytes().unwrap()).unwrap();
            let target=&value["arguments"]["writes"][1];
            (target["expected_before_hash"]==ContentHash::of_bytes(&selected).to_hex()).then_some((file,value))
        }).unwrap();
    let target=&call_value["arguments"]["writes"][1];
    let mut candidate:serde_json::Value=serde_json::from_slice(&selected).unwrap();
    json_patch::patch(&mut candidate,&serde_json::from_value::<json_patch::Patch>(target["patch"].clone()).unwrap()).unwrap();
    let pending=&candidate["interface_state"][descriptor.id.as_str()]["pending_handoff"];
    let original:serde_json::Value=serde_json::from_slice(&saved.closure.read_payload_bytes().unwrap()).unwrap();
    let head=pending["withdrawal"]["parent"].clone();assert!(head.is_object());
    assert_eq!(original["session_context"]["parent"],head);
    let path=call_file.manifest_path().to_path_buf();let hidden=path.with_extension("fixture-held");std::fs::rename(&path,&hidden).unwrap();
    for fault in ["parents","retained-head","delta"] {
        let mut changed=candidate.clone();let mut payload=original.clone();
        let state=&mut changed["interface_state"][descriptor.id.as_str()];
        let proof=&mut state["pending_handoff"]["withdrawal"];
        if fault=="parents" {proof["parent"]=serde_json::Value::Null;payload["session_context"]["parent"]=serde_json::Value::Null;}
        if fault=="retained-head" {
            proof["parent"]=serde_json::Value::Null;payload["session_context"]["parent"]=serde_json::Value::Null;
            proof["replaced_context"].as_array_mut().unwrap().retain(|entry|entry[0]!=head);
            let replaced=proof["replaced_context"].as_array().unwrap().iter().map(|entry|entry[0]["id"].clone()).collect::<Vec<_>>();
            let mut publication=octopus_run_authoring::provider_context_bindings(&installed_record,&descriptor.id).unwrap()
                .into_iter().filter(|binding|!replaced.contains(&json!(binding.id))).map(|binding|json!(binding.id)).collect::<Vec<_>>();
            publication.push(pending["items"][0]["artifact_id"].clone());
            state["pending_handoff"]["context_publication"]=json!(publication);
        }
        if fault=="delta" {payload["session_context"]["artifacts"].as_array_mut().unwrap().reverse();}
        let forged=artifact(&ctx.interface_var_dir().join(format!("snapshot-{fault}")),TURN_INTERRUPTED_RESULT_LABEL,payload);
        let reference=cache_artifact(&ctx,&forged);
        let old=pending["items"][0]["artifact_id"].as_str().unwrap();
        let prefix=|id:&str| {use sha2::{Digest,Sha256};let mut h=Sha256::new();h.update(run.as_str().as_bytes());h.update([0]);h.update(id.as_bytes());h.update([0]);h.update(changed["interface_state"][descriptor.id.as_str()]["authoring_generation"].as_u64().unwrap().to_le_bytes());format!("cp_{}",&format!("{:x}",h.finalize())[..20])};
        let encoded=serde_json::to_string(&changed).unwrap().replace(&prefix(old),&prefix(reference.id.as_str())).replace(old,reference.id.as_str());
        changed=serde_json::from_str(&encoded).unwrap();
        let item=&mut changed["interface_state"][descriptor.id.as_str()]["pending_handoff"]["items"][0];
        item["artifact_content_hash"]=json!(reference.content.to_hex());
        item["manifest_relative_path"]=json!(forged.manifest_path().canonicalize().unwrap().strip_prefix(ctx.interface_var_dir().canonicalize().unwrap()).unwrap());
        let edit=CheckedRunEdit::prepare_with_format(run_path.canonicalize().unwrap(),&selected,changed,call_value["correlation_id"].as_str().unwrap().into(),serde_json::from_value(target["json_format"].clone()).unwrap()).unwrap();
        let mut value=call_value.clone();let target=&mut value["arguments"]["writes"][1];
        target["patch"]=serde_json::to_value(&edit.patch).unwrap();target["expected_after_hash"]=json!(edit.after_hash);
        let attempt=artifact(&calls,&writer.call_label,value);
        let probe=ProviderHandoffSession::new(&ctx).unwrap().from_invocation(&ctx,&descriptor,&run,&envelope,result_seq,&pinned).unwrap();
        let error=match Box::pin(probe.pending_source_withdrawal()).await {Err(error)=>error,Ok(_)=>panic!("forged snapshot accepted: {fault}")};
        assert!(error.contains(if fault=="delta" {"exact source context parent and delta"} else {"exact predecessor snapshot parent or complete dynamic replacement set"}),"{fault}: {error}");
        if phase==12 {
            let mut replay_ctx=ctx.clone();replay_ctx.movements_path=root.join(format!("forged-{fault}.jsonl"));replay_ctx.ledger_index_path=root.join(format!("forged-{fault}.sqlite"));
            std::fs::write(&replay_ctx.movements_path,&ledger).unwrap();
            let limits=ctx.required_host_io_limits().unwrap();
            let advance=&call_value["arguments"]["writes"][0];
            let preimage:octopus_run_authoring::Preimage=serde_json::from_value(advance["expected_before_content"].clone()).unwrap();
            let writer_before=preimage.decode(advance["expected_before_hash"].as_str().unwrap(),&mut octopus_run_authoring::DecodeBudget::new(limits.max_payload_bytes).unwrap()).unwrap().into_bytes();
            let before_record=RunRecord::from_bytes(&writer_before).unwrap();
            assert_eq!(before_record.id.as_str(),installed_record.interface_state[&descriptor.id]["run_authoring_binding"]["writer_run"]["run"].as_str().unwrap());
            let mut after_value:serde_json::Value=serde_json::from_slice(&writer_before).unwrap();
            json_patch::patch(&mut after_value,&serde_json::from_value::<json_patch::Patch>(advance["patch"].clone()).unwrap()).unwrap();
            let format:octopus_run_authoring::JsonFormat=serde_json::from_value(advance["json_format"].clone()).unwrap();
            let writer_after=format.encode(&after_value).unwrap();
            assert_eq!(ContentHash::of_bytes(&writer_after).to_hex(),advance["expected_after_hash"].as_str().unwrap());
            let call_cp=before_record.checkpoints.iter().find(|cp|before_record.active_checkpoints.contains(&cp.id)&&cp.inbound_interface()==Some(authoring_id)&&cp.outbound_interfaces().eq([&writer.door])).unwrap();
            append(&replay_ctx,edge(&replay_ctx,&writer_before,call_cp,&attempt,MovementKind::InterfaceOutbound,writer.door.clone()));
            let after_record=RunRecord::from_bytes(&writer_after).unwrap();
            let result_cp=after_record.checkpoints.iter().find(|cp|after_record.active_checkpoints.contains(&cp.id)&&cp.inbound_interface()==Some(&writer.door)&&cp.outbound_interfaces().eq([&descriptor.id])).unwrap();
            let result=artifact(&root.join("forged-result"),&writer.result_label,json!({"label":writer.result_label,"tool":writer.tool,"correlation_id":edit.correlation_id,"ok":true,"body":{"result":"ordered_file_writes","writes":[
                {"path":advance["path"],"before_hash":advance["expected_before_hash"],"after_hash":advance["expected_after_hash"],"bytes":writer_after.len(),"outcome":"applied","observed_hash":advance["expected_after_hash"]},
                {"path":edit.path,"before_hash":edit.before_hash,"after_hash":edit.after_hash,"bytes":edit.content.len(),"outcome":"applied","observed_hash":edit.after_hash}
            ]}}));
            append(&replay_ctx,edge(&replay_ctx,&writer_after,result_cp,&result,MovementKind::InterfaceOutbound,descriptor.id.clone()));
            let evidence=CheckedRunEvidence {movements_path:&replay_ctx.movements_path,ledger_index_path:&replay_ctx.ledger_index_path,blobs_dir:&ctx.blobs_dir,max_control_frame_bytes:limits.max_control_frame_bytes,max_manifest_bytes:limits.max_manifest_bytes,max_payload_bytes:limits.max_payload_bytes};
            let error=octopus_run_authoring::checked_run_file_chain(&evidence,&run_path.canonicalize().unwrap(),&selected,edit.content.as_bytes(),stop_seq,&[writer.clone()]).unwrap_err();
            assert!(error.contains(if fault=="delta" {"exact source context parent and delta"} else {"exact predecessor snapshot parent or complete dynamic replacement set"}),"checked {fault}: {error}");
        }
        std::fs::remove_file(attempt.manifest_path()).unwrap();
        assert_eq!(std::fs::read(&run_path).unwrap(),selected);assert_eq!(std::fs::read(&ctx.movements_path).unwrap(),ledger);assert!(receiver.try_recv().is_err());
    }
    std::fs::rename(hidden,path).unwrap();
    }
    async fn source_with_snapshot(
        ctx:&InterfaceLiftContext,root:&Path,run_path:&Path,descriptor:&InterfaceDescriptor,run:&RunId,
        first:octopus_run_authoring::ProviderOutputHandoff,input:InterfaceInvocationEnvelope,
        authoring_id:&InterfaceId,listener:&tokio::net::UnixListener,
        receiver:&mut tokio::sync::mpsc::Receiver<octopus_interface_transport::InterfaceActivationArrival>,
    ) -> (octopus_run_authoring::ProviderOutputHandoff,InterfaceInvocationEnvelope,u64) {
    let prior=artifact(&ctx.interface_var_dir().join("prior-answer"),"interface.artifact.output",json!({"label":"interface.artifact.output","text":"prior completed reply"}));
    let hand=Box::pin(first.hand_final(ProviderEmission {artifact:prior,route:ProviderEmissionRoute::ClientProjection}));
    let gate=pump(&ctx,root,&run_path,&authoring_id,&listener,receiver,&input.artifact,0,1,true,0,true);
    tokio::try_join!(hand,async {Ok::<_,String>(gate.await)}).unwrap();

    let current=std::fs::read(&run_path).unwrap();let current_record=RunRecord::from_bytes(&current).unwrap();
    let cp=current_record.checkpoints.iter().find(|cp|current_record.active_checkpoints.contains(&cp.id)
        && cp.outbound_interfaces().any(|id|id==&descriptor.id) && cp.interface_binding.as_ref().unwrap().admits("operator.message")).unwrap();
    let next=artifact(&root.join("source-with-head"),"operator.message",json!({"text":"now create the run"}));
    let row=edge(&ctx,&current,cp,&next,MovementKind::InterfaceOutbound,descriptor.id.clone());
    let input=source_envelope(&ctx,root,&row,next);let source=append(&ctx,row);
    let first=ProviderHandoffSession::new(&ctx).unwrap().from_invocation(&ctx,&descriptor,&run,&input,source,&current).unwrap();
    assert!(first.invocation_session_snapshot().is_some());
    (first,input,source)
    }
    // Construct this future outside `case`: its debug poll frame otherwise retains
    // the full withdrawal future while detached writer preimage compression runs.
    fn native_withdrawal_case<'a>(
        ctx:&'a InterfaceLiftContext,root:&'a Path,run_path:&'a Path,worker_interface_id:&'a str,
        descriptor:&'a InterfaceDescriptor,run:&'a RunId,record:&'a RunRecord,pinned:&'a [u8],
        input:&'a InterfaceInvocationEnvelope,state:&'a serde_json::Value,before_context:u64,
        operation:crate::run_affordances::NativePending,phase:usize,authoring_id:&'a InterfaceId,
        listener:&'a tokio::net::UnixListener,
        receiver:&'a mut tokio::sync::mpsc::Receiver<octopus_interface_transport::InterfaceActivationArrival>,
        selected_file:Option<&'a InterfaceArtifactFile>,
    ) -> std::pin::Pin<Box<impl std::future::Future<Output=()> + Send + 'a>> {
        Box::pin(async move {
                let result_cp=record.checkpoints.iter().find(|cp|record.active_checkpoints.contains(&cp.id)
                    && cp.inbound_interface()==Some(&InterfaceId::new(worker_interface_id)) && cp.outbound_interfaces().any(|id|id==&descriptor.id)).unwrap();
                let body=octopus_system_room_env::execute_file_write_result(&input,&operation.effect.arguments,&["system_room".into()],
                    &BTreeMap::from([("run_records_root".into(),root.join("runs"))]),&root.join("native-create-effect"),1<<24).unwrap();
                assert_eq!(body["writes"].as_array().unwrap().len(),44);
                if let crate::run_affordances::NativePhase::Writing {files,..}=&operation.state {
                    for (actual,expected) in body["writes"].as_array().unwrap().iter().zip(files) {
                        assert_eq!(*actual,serde_json::to_value(expected).unwrap());
                    }
                }
                let result_label=format!("{}.result",operation.effect.tool);
                let actual_result=json!({"label":result_label,"tool":operation.effect.tool,"correlation_id":operation.effect.correlation_id,"ok":true,"body":body});
                let result=artifact(&root.join("native-write-result"),&result_label,actual_result.clone());
                let row=edge(&ctx,&pinned,result_cp,&result,MovementKind::InterfaceOutbound,descriptor.id.clone());
                let envelope=source_envelope(&ctx,root,&row,result); let result_seq=append(&ctx,row);
                let crate::run_affordances::NativeStep::Effect {pending:next,effect,..}=operation.advance(&actual_result).unwrap() else {panic!("applied native write must retain its seed")};
                let mut seed_state=state.clone(); seed_state["context_generation"]=json!(before_context+2);
                seed_state["pending"]=json!({"kind":"dynamic_tool","turn_id":"turn-a","call_id":effect.correlation_id,"tool":effect.tool,"native_operation":next});
                let seed=artifact(&ctx.interface_var_dir().join("saved-seed"),"tool.call",json!({"tool":effect.tool,"arguments":effect.arguments,"correlation_id":effect.correlation_id,"codex_continuity":seed_state}));
                let session=ProviderHandoffSession::new(&ctx).unwrap();
                let handoff=session.from_invocation(&ctx,&descriptor,&run,&envelope,result_seq,&pinned).unwrap();
                let mut hand=Box::pin(handoff.hand_final(ProviderEmission {artifact:seed.clone(),route:ProviderEmissionRoute::Tool {tool:effect.tool}}));
                let gate=pump(&ctx,root,run_path,&authoring_id,&listener,receiver,&envelope.artifact,0,0,false,0,true);
                tokio::select! { _=gate=>{}, result=&mut hand=>panic!("must stop at installed unadmitted intent: {result:?}") }
                drop(hand);
                assert!(receiver.try_recv().is_err(),"crash boundary precedes hand-in and its dispatch marker");
                let installed=std::fs::read(run_path).unwrap();let installed_record=RunRecord::from_bytes(&installed).unwrap();
                let cp=installed_record.checkpoints.iter().find(|cp|installed_record.active_checkpoints.contains(&cp.id) && cp.outbound_interfaces().any(|id|id==&descriptor.id)
                    && cp.interface_binding.as_ref().unwrap().admits("operator.turn.control")).unwrap();
                let stop=artifact(&root.join("cold-stop"),"operator.turn.control",json!({"action":"stop","control_id":"cold-stop"}));
                let row=edge(&ctx,&installed,cp,&stop,MovementKind::InterfaceOutbound,descriptor.id.clone());
                let stop=source_envelope(&ctx,root,&row,stop);let stop_seq=append(&ctx,row);
                let _uncertain_arrival=if phase==11 {
                    let pending=ProviderHandoffSession::new(&ctx).unwrap().from_invocation(&ctx,&descriptor,&run,&envelope,result_seq,&pinned).unwrap();
                    let mut dispatch=Box::pin(pending.recover_source_with_outcomes());
                    let arrival=tokio::select! {arrival=receiver.recv()=>arrival.unwrap(),result=&mut dispatch=>panic!("expected an unacknowledged hand-in: {result:?}")};
                    assert_eq!(materialized_artifact_ref(&arrival.artifact).unwrap(),materialized_artifact_ref(&seed).unwrap());
                    drop(dispatch);Some(arrival)
                } else {None};
                if phase==7 {
                    let id=serde_json::from_value(installed_record.interface_state[&descriptor.id]["pending_handoff"]["items"][0]["checkpoint"]["id"].clone()).unwrap();
                    let cp=installed_record.checkpoint(&id).unwrap();
                    append(&ctx,edge(&ctx,&installed,cp,&seed,MovementKind::InterfaceArrival,installed_record.worker.clone()));
                }
                let (transport,_release,mut methods,server)=interrupted_drain_driver(root,&descriptor).await;
                let mut driver=Some(transport);let auth=CodexRuntimeAuth { manager:auth_manager_from_provider_home(root.join("auth")).await,
                    provider_home:root.join("provider"),auth_store_home:root.join("auth"),activation_secrets_home:root.join("secrets"),connection_mode:ProviderConnectionMode::Native };
                let controls:ActiveTurnControls=Default::default();let arbiter=InterruptedEffectArbiter::new(ctx.clone());let session=ProviderHandoffSession::new(&ctx).unwrap();
                Box::pin(restore_completed_provider_sources(&ctx,&descriptor,&session,&auth,&mut driver,&controls,&arbiter,result_seq)).await.unwrap();
                let broker=active_control(&controls,&run).await.expect("unconsumed installed intent must restore its control target");
                assert!(broker.is_recovered_pending());assert!(methods.try_recv().is_err());assert!(receiver.try_recv().is_err());
                let gates:ControlSourceGates=Default::default();
                if phase==10 { persist_control_journal(&ctx,stop_seq,&run,"cold-stop","stop",TurnControlJournalPhase::Intent).unwrap(); }
                let (_progress,progress_rx)=tokio::sync::watch::channel(MainReaderProgress {next_seq:result_seq,revision:0});
                let (fences,fence_rx)=tokio::sync::mpsc::unbounded_channel();
                let mut reader_task=tokio::spawn(control_reader_life(ctx.clone(),descriptor.clone(),session.clone(),controls.clone(),
                    gates.clone(),arbiter.clone(),progress_rx,fence_rx));
                if matches!(phase,7|11) {
                    let ledger=std::fs::read(&ctx.movements_path).unwrap();
                    let error=tokio::time::timeout(std::time::Duration::from_secs(20),&mut reader_task).await.unwrap().unwrap().unwrap_err();
                    assert!(error.contains(if phase==7 {"output already reached an edge"}else{"hand-in outcome is uncertain"}),"{error}");
                    assert_eq!(std::fs::read(run_path).unwrap(),installed);assert_eq!(std::fs::read(&ctx.movements_path).unwrap(),ledger);
                    assert!(receiver.try_recv().is_err());server.abort();return;
                }
                let (delivered,broker)=if matches!(phase,6|8|9|12|13) {
                    let gate=pump(&ctx,root,run_path,&authoring_id,&listener,receiver,&envelope.artifact,0,0,true,if phase>=8 {8}else{0},true);
                    tokio::select! {_=gate=>{},result=&mut reader_task=>panic!("must stop after retirement write: {result:?}")}
                    reader_task.abort();let _=reader_task.await;
                    let selected=std::fs::read(run_path).unwrap();let ledger=std::fs::read(&ctx.movements_path).unwrap();
                    let session=ProviderHandoffSession::new(&ctx).unwrap();
                    let restored=session.from_invocation(&ctx,&descriptor,&run,&envelope,result_seq,&pinned).unwrap();
                    assert!(Box::pin(restored.recover_source_with_outcomes()).await.unwrap_err().contains("requires provider validation"));
                    assert_eq!(std::fs::read(run_path).unwrap(),selected);assert_eq!(std::fs::read(&ctx.movements_path).unwrap(),ledger);assert!(receiver.try_recv().is_err());
                    let saved=Box::pin(restored.pending_source_withdrawal()).await.unwrap().unwrap();
                    if phase>=12 {Box::pin(assert_saved_snapshot_authority(&ctx,root,run_path,&descriptor,&run,&envelope,result_seq,&pinned,&selected,&ledger,&saved,&authoring_id,phase,stop_seq,receiver)).await;}
                    let prior=restored.current_session_context_artifacts().await.iter().filter(|entry|entry.source_movement_seq.is_some_and(|seq|seq<result_seq)).cloned().collect::<Vec<_>>();
                    let exact=crate::handler::stopped_result::prove_intent(&descriptor,&envelope,saved.intent,&prior).unwrap();
                    let value:serde_json::Value=serde_json::from_slice(&saved.closure.read_payload_bytes().unwrap()).unwrap();
                    let decoded=decode_admitted_turn_control(&ctx,&descriptor,&stop).unwrap().unwrap();
                    for bad in ["/withdrawn_source_intent/control_id","/withdrawn_source_intent/intent_artifact/id",
                        "/withdrawn_source_intent/applied_native_operation/installed","/codex_continuity/native_terminal/turn/status"] {
                        let mut forged=value.clone();*forged.pointer_mut(bad).unwrap()=json!("forged");
                        assert!(crate::handler::stopped_result::validate_saved_intent_closure(&exact,&envelope,&forged,&stop.artifact,stop_seq,&decoded,&codex_context_output_labels(&descriptor).unwrap()).is_err(),"{bad}");
                    }
                    let controls:ActiveTurnControls=Default::default();
                    Box::pin(restore_completed_provider_sources(&ctx,&descriptor,&session,&auth,&mut driver,&controls,&arbiter,result_seq)).await.unwrap();
                    let next_broker=active_control(&controls,&run).await.unwrap();
                    assert_eq!(next_broker.is_finished(),phase==6);assert_eq!(next_broker.is_recovered_pending(),phase>=8);
                    let (_progress,progress_rx)=tokio::sync::watch::channel(MainReaderProgress {next_seq:result_seq,revision:0});
                    let (fences,fence_rx)=tokio::sync::mpsc::unbounded_channel();
                    let mut task=tokio::spawn(control_reader_life(ctx.clone(),descriptor.clone(),session.clone(),controls,
                        gates.clone(),arbiter.clone(),progress_rx,fence_rx));
                    let gate=pump(&ctx,root,run_path,&authoring_id,&listener,receiver,&envelope.artifact,0,2,true,0,true);
                    let handling=async {tokio::select! {result=fence_control_reader(&fences,&run,Some(result_seq))=>match result {
                            Ok(watermark)=>Ok(watermark),Err(error)=>Err(format!("{error}; restarted control reader: {:?}",(&mut task).await)),
                        },
                        result=&mut task=>panic!("restarted withdrawal reader failed: {result:?}")}};
                    let (_,delivered)=tokio::time::timeout(std::time::Duration::from_secs(20),async{tokio::try_join!(handling,async {Ok::<_,String>(gate.await)})}).await.unwrap().unwrap();
                    task.abort();let _=task.await;
                    (delivered,next_broker)
                } else {
                    let gate=pump(&ctx,root,run_path,&authoring_id,&listener,receiver,&envelope.artifact,0,2,true,0,true);
                    let handled=async {tokio::select! {
                        result=fence_control_reader(&fences,&run,Some(result_seq))=>match result {
                            Ok(watermark)=>Ok(watermark),Err(error)=>Err(format!("{error}; cold control reader: {:?}",(&mut reader_task).await)),
                        },
                        result=&mut reader_task=>panic!("cold control reader failed: {result:?}"),
                    }};
                    let completed=tokio::time::timeout(std::time::Duration::from_secs(20),async{tokio::try_join!(handled,async {Ok::<_,String>(gate.await)})}).await;
                    if completed.is_err() { eprintln!("cold fence timeout: journal={}, finished={}, recovered={}",serde_json::to_string(&load_control_journal(&ctx,stop_seq,&run,"cold-stop","stop").unwrap()).unwrap(),broker.is_finished(),broker.is_recovered_pending()); }
                    let (watermark,delivered)=completed.unwrap().unwrap();
                    assert!(watermark>stop_seq);reader_task.abort();let _=reader_task.await;
                    (delivered,broker.clone())
                };
                assert_eq!(delivered.iter().map(|item|item.0.as_str()).collect::<Vec<_>>(),[TURN_INTERRUPTED_RESULT_LABEL,"turn.control.acknowledged"]);
                assert!(broker.is_finished());if phase!=10 { assert_eq!(methods.try_recv().unwrap(),"turn/interrupt"); }
                assert_eq!(methods.try_recv().unwrap(),"thread/read");assert_eq!(methods.try_recv().unwrap(),"thread/turns/list");assert_eq!(methods.try_recv().unwrap(),"thread/items/list");assert!(methods.try_recv().is_err());
                let after=std::fs::read(run_path).unwrap();let ledger=std::fs::read(&ctx.movements_path).unwrap();
                let replay=ProviderHandoffSession::new(&ctx).unwrap().from_invocation(&ctx,&descriptor,&run,&envelope,result_seq,&pinned).unwrap();
                assert_eq!(Box::pin(replay.recover_source_with_outcomes()).await.unwrap().0,ProviderSourceRecovery::Completed);
                assert_eq!(std::fs::read(run_path).unwrap(),after);assert_eq!(std::fs::read(&ctx.movements_path).unwrap(),ledger);
                let replay_control={let ctx=ctx.clone();let descriptor=descriptor.clone();let session=session.clone();let arbiter=arbiter.clone();let run=run.clone();let stop=stop.clone();
                    tokio::spawn(async move {let broker=CodexTurnControlBroker::new(None);broker.finish();let gates=Default::default();
                        let control=decode_admitted_turn_control(&ctx,&descriptor,&stop).unwrap().unwrap();
                        handle_control_source(&ctx,&descriptor,&session,&arbiter,&run,ControlSource {envelope:&stop,source_movement_seq:stop_seq,
                            broker:&broker,control_gates:&gates,control,terminal_control_fenced:false,independent_reader:false}).await})};
                replay_control.await.unwrap().unwrap();
                assert_eq!(std::fs::read(run_path).unwrap(),after);assert_eq!(std::fs::read(&ctx.movements_path).unwrap(),ledger);assert!(methods.try_recv().is_err());
                assert!(!String::from_utf8(ledger).unwrap().lines().any(|line|serde_json::from_str::<Movement>(line).unwrap().artifact==Some(materialized_artifact_ref(&seed).unwrap())));
                assert_eq!(serde_json::from_slice::<serde_json::Value>(&std::fs::read(root.join("runs/installed_child.json")).unwrap()).unwrap()["status"],"idle");
                {
                    use octopus_run_authoring::{CheckedRunEvidence,RunAuthoringProfile,declared_provider_run_writer};
                    let profile:RunAuthoringProfile=serde_json::from_value(descriptor.interface_driver_config.as_ref().unwrap().private_config["run_authoring_profile"].clone()).unwrap();
                    let writer=declared_provider_run_writer(&installed_record,&descriptor.id,&profile).unwrap();
                    let limits=ctx.required_host_io_limits().unwrap();let evidence=CheckedRunEvidence {movements_path:&ctx.movements_path,ledger_index_path:&ctx.ledger_index_path,blobs_dir:&ctx.blobs_dir,max_control_frame_bytes:limits.max_control_frame_bytes,max_manifest_bytes:limits.max_manifest_bytes,max_payload_bytes:limits.max_payload_bytes};
                    assert!(!octopus_run_authoring::checked_run_file_chain(&evidence,&run_path.canonicalize().unwrap(),&installed,&after,stop_seq,&[writer.clone()]).unwrap().is_empty());
                    assert!(octopus_run_authoring::provider_input_rearm_ready(&evidence,&RunRecord::from_bytes(&after).unwrap(),&descriptor.id).unwrap());
                    let source=installed_record.interface_state[&descriptor.id]["run_authoring_binding"]["checkpoints"].as_array().unwrap().iter().find(|template|template["role"]=="stable"&&template["include_context"]==true).unwrap()["interface_binding"]["interface"].as_str().unwrap();
                    assert!(octopus_run_authoring::provider_input_payloads_ready(&evidence,&run_path.canonicalize().unwrap(),&after,&writer,&InterfaceId::new(source)).unwrap());
                    if let Some(file)=&selected_file {
                        let reference=materialized_artifact_ref(file).unwrap();
                        let current=RunRecord::from_bytes(&after).unwrap();
                        for cp in current.checkpoints.iter().filter(|cp|current.active_checkpoints.contains(&cp.id)) {
                            for binding in cp.artifact_bindings.iter().filter(|binding|binding.id==reference.id) {assert_eq!(binding.access,octopus_runtime::ArtifactAccess::read_only());}
                        }
                    }
                }
                let current=RunRecord::from_bytes(&after).unwrap();
                let seed_ref=serde_json::to_value(materialized_artifact_ref(&seed).unwrap()).unwrap();
                assert!(!current.interface_state[&descriptor.id]["run_authoring_binding"]["checkpoints"].as_array().unwrap().iter()
                    .any(|template|template["role"]=="result_input" && template["required_verified_artifacts"][0]["params"]["call_artifact"]==seed_ref));
                let cp=current.checkpoints.iter().find(|cp|current.active_checkpoints.contains(&cp.id)
                    && cp.outbound_interfaces().any(|id|id==&descriptor.id) && cp.interface_binding.as_ref().unwrap().admits("operator.message")).unwrap();
                let next=artifact(&root.join("next-user"),"operator.message",json!({"text":"continue with a different task"}));
                let row=edge(&ctx,&after,cp,&next,MovementKind::InterfaceOutbound,descriptor.id.clone());
                let next=source_envelope(&ctx,root,&row,next);let next_seq=append(&ctx,row);
                let next=ProviderHandoffSession::new(&ctx).unwrap().from_invocation(&ctx,&descriptor,&run,&next,next_seq,&after).unwrap();
                assert_eq!(Box::pin(next.recover_source_with_outcomes()).await.unwrap().0,ProviderSourceRecovery::Process);
                server.abort();
        })
    }
    fn settlement_case<'a>(
        ctx:&'a InterfaceLiftContext,root:&'a Path,run_path:&'a Path,worker_interface_id:&'a str,
        descriptor:&'a InterfaceDescriptor,run:&'a RunId,record:&'a RunRecord,initial:&'a [u8],
        mut state:serde_json::Value,mut before_context:u64,native:serde_json::Value,phase:usize,
        authoring_id:&'a InterfaceId,listener:&'a tokio::net::UnixListener,
        receiver:&'a mut tokio::sync::mpsc::Receiver<octopus_interface_transport::InterfaceActivationArrival>,
        selected_file:Option<&'a InterfaceArtifactFile>,
    ) -> std::pin::Pin<Box<impl std::future::Future<Output=()> + Send + 'a>> {
        Box::pin(async move {
            let cp=record.checkpoints.iter().find(|cp|record.active_checkpoints.contains(&cp.id)
                && cp.outbound_interfaces().any(|id|id==&descriptor.id)
                && cp.interface_binding.as_ref().unwrap().admits("operator.message")).unwrap();
            let input=artifact(&root.join("original-input"),"operator.message",json!({"text":"use the tool"}));
            let row=edge(&ctx,&initial,cp,&input,MovementKind::InterfaceOutbound,descriptor.id.clone());
            let input=source_envelope(&ctx,root,&row,input); let source=append(&ctx,row);
            let first=ProviderHandoffSession::new(&ctx).unwrap().from_invocation(&ctx,&descriptor,&run,&input,source,&initial).unwrap();
            let (first,input,source)=if phase>=12 {
                before_context+=1;
                Box::pin(source_with_snapshot(&ctx,root,run_path,&descriptor,&run,first,input,&authoring_id,&listener,receiver)).await
            } else {(first,input,source)};
            state["context_generation"]=json!(before_context+1);
            let native_plan = if (5..=13).contains(&phase) {
                use crate::run_affordances::{NativePending,NativeEffect,NativePhase,FileProof,CreatedRun};
                let mut files=(0..44).map(|index| {
                    let content=if index==0 {json!({"id":"installed_child","ee":record.ee,"worker":record.worker,"trace":record.trace,"status":"idle","checkpoints":[],"active_checkpoints":[]}).to_string()} else {format!("created file {index}")};
                    let path=root.canonicalize().unwrap().join("runs").join(if index==0 {"installed_child.json".to_string()} else {format!("installed-{index}.txt")});
                    (FileProof {path,before_hash:None,after_hash:ContentHash::of_bytes(content.as_bytes()).to_hex(),bytes:content.len() as u64},content)
                }).collect::<Vec<_>>();
                files.sort_by(|(left,_),(right,_)|left.path.cmp(&right.path));
                let tools=record.interface_state[&descriptor.id]["run_authoring_binding"]["checkpoints"].as_array().unwrap().iter()
                    .filter(|template|template["role"]=="provider_tool_output").flat_map(|template|template["required_verified_artifacts"][0]["params"]["tools"].as_array().unwrap().iter())
                    .filter_map(serde_json::Value::as_str).collect::<Vec<_>>();
                let tool=tools.iter().find(|id|id.starts_with("file.write")).unwrap().to_string();
                let sender=tools.iter().find(|id|id.starts_with("artifact.send")).unwrap().to_string();
                let operation=NativePending {call_id:"native-create".into(),operation_name:"create_run".into(),source_run:run.to_string(),
                    effect:NativeEffect {tool:tool.clone(),correlation_id:"native-create:write".into(),arguments:json!({"writes":files.iter().map(|(file,content)|json!({"path":file.path,"content":content,"expected_before_hash":null})).collect::<Vec<_>>()})},
                    state:NativePhase::Writing {created:CreatedRun {run_id:"installed_child".into(),worker_interface_id:record.worker.to_string(),provider_interface_id:descriptor.id.to_string(),receive_interface_id:"seed_target".into(),return_interface_ids:vec![],model:"fixture".into(),reasoning_effort:"high".into(),service_tier:None},
                        files:files.into_iter().map(|(file,_)|file).collect(),seed:NativeEffect {tool:sender,correlation_id:"native-create:seed".into(),arguments:json!({"interface":"seed_target","text":"start"})},outgoing_bindings:vec![]} };
                state["pending"]=json!({"kind":"dynamic_tool","turn_id":"turn-a","call_id":operation.effect.correlation_id,"tool":operation.effect.tool,"native_operation":operation});
                Some(operation)
            } else {None};
            let original:crate::handler::CodexContinuityState=serde_json::from_value(state.clone()).unwrap();
            let effect=artifact(&ctx.interface_var_dir().join("effect"),"tool.call", if let Some(operation)=&native_plan {
                json!({"label":"tool.call","tool":operation.effect.tool,"arguments":operation.effect.arguments,"correlation_id":operation.effect.correlation_id,"codex_continuity":original})
            } else {json!({"label":"tool.call","tool":"coder.exec","arguments":{},"correlation_id":"call-a","codex_continuity":original})});
            let admit=Box::pin(first.hand_final(ProviderEmission{artifact:effect,route:ProviderEmissionRoute::ClientProjection}));
            let gate=pump(&ctx,root,run_path,&authoring_id,&listener,receiver,&input.artifact,0,1,true,0,true);
            tokio::try_join!(admit,async {Ok::<_,String>(gate.await)}).unwrap();
            let mut pinned=std::fs::read(run_path).unwrap();let record=RunRecord::from_bytes(&pinned).unwrap();
            if let Some(operation) = native_plan {
                native_withdrawal_case(&ctx,root,run_path,worker_interface_id,
                    &descriptor,&run,&record,&pinned,&input,&state,before_context,operation,phase,
                    &authoring_id,&listener,receiver,selected_file).await;
                return;
            }
            if phase==4 {
                let (transport,_release,mut methods,server)=interrupted_drain_driver(root,&descriptor).await;
                let mut driver=Some(transport);
                let auth=CodexRuntimeAuth { manager:auth_manager_from_provider_home(root.join("auth")).await,
                    provider_home:root.join("provider"),auth_store_home:root.join("auth"),activation_secrets_home:root.join("secrets"),connection_mode:ProviderConnectionMode::Native };
                let controls:ActiveTurnControls=Default::default();let arbiter=InterruptedEffectArbiter::new(ctx.clone());
                let session=ProviderHandoffSession::new(&ctx).unwrap();
                let committed=std::fs::read_to_string(&ctx.movements_path).unwrap().lines().count() as u64;
                assert!(source<committed);
                Box::pin(restore_completed_provider_sources(&ctx,&descriptor,&session,&auth,&mut driver,&controls,&arbiter,committed)).await.unwrap();
                let broker=active_control(&controls,&run).await.expect("consumed source must restore its pending effect owner");
                assert!(broker.is_recovered_pending());assert!(!broker.is_finished());assert!(methods.try_recv().is_err());
                assert_eq!(std::fs::read(run_path).unwrap(),pinned);
                let cp=record.checkpoints.iter().find(|cp|record.active_checkpoints.contains(&cp.id)
                    && cp.outbound_interfaces().any(|id|id==&descriptor.id) && cp.interface_binding.as_ref().unwrap().admits("operator.turn.control")).unwrap();
                let stop=artifact(&root.join("stop"),"operator.turn.control",json!({"action":"stop","control_id":"stop-a"}));
                let row=edge(&ctx,&pinned,cp,&stop,MovementKind::InterfaceOutbound,descriptor.id.clone());
                let stop=source_envelope(&ctx,root,&row,stop);let stop_seq=append(&ctx,row);
                assert!(matches!(control_reader_ownership(Some(&broker),committed,stop_seq,"operator.turn.control"),ControlReaderOwnership::Independent));
                let gates=Default::default();
                let control=decode_admitted_turn_control(&ctx,&descriptor,&stop).unwrap().unwrap();
                let admit=Box::pin(handle_control_source(&ctx,&descriptor,&session,&arbiter,&run,ControlSource {
                    envelope:&stop,source_movement_seq:stop_seq,broker:&broker,control_gates:&gates,control,
                    terminal_control_fenced:true,independent_reader:true,
                }));
                let gate=pump(&ctx,root,run_path,&authoring_id,&listener,receiver,&stop.artifact,0,1,true,0,true);
                let (result,delivered)=tokio::time::timeout(std::time::Duration::from_secs(5),async{tokio::join!(admit,gate)}).await.unwrap();
                assert_eq!(result.unwrap(),ControlSourceDisposition::Handled);assert_eq!(delivered[0].0,"turn.control.acknowledged");
                assert_eq!(methods.try_recv().unwrap(),"turn/interrupt");assert!(methods.try_recv().is_err());
                let interrupted=arbiter.durable_continuity(&run).await.unwrap().unwrap();
                let value=serde_json::to_value(&interrupted).unwrap();
                assert!(value["pending"].is_null());assert_eq!(value["interrupted_pending"],state["pending"]);
                assert_eq!(value["context_generation"],json!(before_context+2));assert!(!broker.is_finished());
                let saved_interrupt=std::fs::read(interrupted_effect_journal_path(&ctx,&run)).unwrap();
                let repeat_broker=CodexTurnControlBroker::new(None);repeat_broker.finish();
                let bytes=std::fs::read(run_path).unwrap();let record=RunRecord::from_bytes(&bytes).unwrap();
                let cp=record.checkpoints.iter().find(|cp|record.active_checkpoints.contains(&cp.id)
                    && cp.outbound_interfaces().any(|id|id==&descriptor.id) && cp.interface_binding.as_ref().unwrap().admits("operator.turn.control")).unwrap();
                let file=artifact(&root.join("repeat-stop"),"operator.turn.control",json!({"action":"stop","control_id":"stop-b"}));
                let row=edge(&ctx,&bytes,cp,&file,MovementKind::InterfaceOutbound,descriptor.id.clone());
                let repeat=source_envelope(&ctx,root,&row,file);let repeat_seq=append(&ctx,row);
                let control=decode_admitted_turn_control(&ctx,&descriptor,&repeat).unwrap().unwrap();
                let admit=Box::pin(handle_control_source(&ctx,&descriptor,&session,&arbiter,&run,ControlSource {
                    envelope:&repeat,source_movement_seq:repeat_seq,broker:&repeat_broker,control_gates:&gates,control,
                    terminal_control_fenced:true,independent_reader:true,
                }));
                let gate=pump(&ctx,root,run_path,&authoring_id,&listener,receiver,&repeat.artifact,0,1,true,0,true);
                let (result,delivered)=tokio::time::timeout(std::time::Duration::from_secs(5),async {
                    tokio::try_join!(admit,async {Ok::<_,String>(gate.await)})
                }).await.unwrap().unwrap();
                assert_eq!(result,ControlSourceDisposition::Handled);assert_eq!(delivered[0].0,"turn.control.acknowledged");
                assert_eq!(std::fs::read(interrupted_effect_journal_path(&ctx,&run)).unwrap(),saved_interrupt);
                assert!(methods.try_recv().is_err());
                let TurnControlJournalPhase::Delivered {result:repeat_result}=load_control_journal(&ctx,repeat_seq,&run,"stop-b","stop").unwrap().unwrap().phase else {panic!("repeat Stop receipt")};
                assert_eq!(repeat_result.detail["exact_result_pending"],true);
                assert_eq!(repeat_result.thread_id.as_deref(),Some("thread-a"));
                assert_eq!(repeat_result.turn_id.as_deref(),Some("turn-a"));
                let interrupted=repeat_result.continuity.unwrap();
                let ledger_before=std::fs::read(&ctx.movements_path).unwrap();
                let cold=ProviderHandoffSession::new(&ctx).unwrap();let cold_arbiter=InterruptedEffectArbiter::new(ctx.clone());
                let control=decode_admitted_turn_control(&ctx,&descriptor,&repeat).unwrap().unwrap();
                assert_eq!(handle_control_source(&ctx,&descriptor,&cold,&cold_arbiter,&run,ControlSource {
                    envelope:&repeat,source_movement_seq:repeat_seq,broker:&repeat_broker,control_gates:&gates,control,
                    terminal_control_fenced:true,independent_reader:true,
                }).await.unwrap(),ControlSourceDisposition::Handled);
                assert_eq!(std::fs::read(interrupted_effect_journal_path(&ctx,&run)).unwrap(),saved_interrupt);
                assert_eq!(std::fs::read(&ctx.movements_path).unwrap(),ledger_before);assert!(methods.try_recv().is_err());
                let current=std::fs::read(run_path).unwrap();let record=RunRecord::from_bytes(&current).unwrap();
                let cp=record.checkpoints.iter().find(|cp|record.active_checkpoints.contains(&cp.id)
                    && cp.inbound_interface()==Some(&InterfaceId::new(worker_interface_id)) && cp.outbound_interfaces().any(|id|id==&descriptor.id)).unwrap();
                let result=artifact(&root.join("result"),"coder.exec.result",json!({"label":"coder.exec.result","tool":"coder.exec","correlation_id":"call-a","ok":true,"body":{"applied":true}}));
                let row=edge(&ctx,&current,cp,&result,MovementKind::InterfaceOutbound,descriptor.id.clone());
                let envelope=source_envelope(&ctx,root,&row,result);let result_seq=append(&ctx,row);
                assert!(arbiter.begin_result(&run,result_seq,&envelope).await.unwrap());
                let handoff=session.from_invocation(&ctx,&descriptor,&run,&envelope,result_seq,&current).unwrap();
                let labels=codex_context_output_labels(&descriptor).unwrap();
                let original=arbiter.durable_continuity(&run).await.unwrap().unwrap();
                let checked=crate::handler::checked_interrupted_context_revision(&original,&handoff.current_session_context_artifacts().await,&labels,None).unwrap();
                assert_eq!(checked,interrupted);
                let ProviderStreamEmission::InterruptedResult {stream:Some(stream),retained_outputs,closure,..}=crate::handler::native_terminal_group_for_test(
                    &descriptor,&ctx.interface_var_dir().join("emitted"),&ctx.interface_var_dir().join("streams"),
                    &interrupted,Some(native.clone()),&envelope.artifact,result_seq,true,handoff.input_changes_context().await,
                ) else {panic!("exact interrupted result group")};
                let closure_ref=interrupted_result_closure_ref(&closure.artifact,&checked,&envelope.artifact,&labels).unwrap().unwrap();
                let mut emissions=vec![stream];emissions.extend(retained_outputs);emissions.push(closure);let count=emissions.len();
                let admit=Box::pin(admit_interrupted_result(&ctx,&handoff,&broker,Some(&arbiter),&run,result_seq,&closure_ref,emissions));
                let gate=pump(&ctx,root,run_path,&authoring_id,&listener,receiver,&envelope.artifact,0,count,false,1,false);
                let (result,delivered)=tokio::time::timeout(std::time::Duration::from_secs(5),async{tokio::join!(admit,gate)}).await.unwrap();
                assert_eq!(result.unwrap(),InterruptedResultAdmission::Admitted(count));assert_eq!(delivered.last().unwrap().0,TURN_INTERRUPTED_RESULT_LABEL);
                assert!(broker.is_finished());assert!(arbiter.reconciled(&run).await.unwrap());assert!(methods.try_recv().is_err());
                let (_,_,closure)=arbiter.reconciled_result(&run).await.unwrap().unwrap();
                checked_interrupted_result_continuity(&ctx,&descriptor,&run,&repeat,&original,
                    &DurableArtifactAdmission {artifact:envelope.artifact.clone(),movement_seq:result_seq},&closure,
                    &handoff.current_session_context_artifacts().await).await.unwrap();
                server.abort();return;
            }
            let cp=record.checkpoints.iter().find(|cp|record.active_checkpoints.contains(&cp.id)
                && cp.inbound_interface()==Some(&InterfaceId::new(worker_interface_id)) && cp.outbound_interfaces().any(|id|id==&descriptor.id)).unwrap();
            let result=artifact(&root.join("result"),"coder.exec.result",json!({"label":"coder.exec.result","tool":"coder.exec","correlation_id":"call-a","ok":true,"body":{"applied":true}}));
            let row=edge(&ctx,&pinned,cp,&result,MovementKind::InterfaceOutbound,descriptor.id.clone());
            let mut envelope=source_envelope(&ctx,root,&row,result);let result_row=row.clone();
            let mut result_seq=if phase>=14 {0}else{append(&ctx,row)};
            let cp=record.checkpoints.iter().find(|cp|record.active_checkpoints.contains(&cp.id)
                && cp.outbound_interfaces().any(|id|id==&descriptor.id) && cp.interface_binding.as_ref().unwrap().admits("operator.turn.control")).unwrap();
            let stop=artifact(&root.join("stop"),"operator.turn.control",json!({"action":"stop","control_id":"stop-a"}));
            let row=edge(&ctx,&pinned,cp,&stop,MovementKind::InterfaceOutbound,descriptor.id.clone());
            let stop=source_envelope(&ctx,root,&row,stop);let stop_seq=append(&ctx,row);
            if phase>14 {result_seq=append(ctx,result_row);}
            let control=ProviderHandoffSession::new(&ctx).unwrap().from_invocation(&ctx,&descriptor,&run,&stop,stop_seq,&pinned).unwrap();
            let arbiter=InterruptedEffectArbiter::new(ctx.clone());
            let actual_stop=if phase>=14 {
                state["interrupted_pending"]=state["pending"].take();
                state["context_generation"]=json!(before_context+2);
                state["native_terminal"]=json!({"thread_id":state["thread_id"],"turn":native});
                Some(CodexTurnControlResult {control_id:"stop-a".into(),action:"stop".into(),acknowledged:true,
                    thread_id:Some("thread-a".into()),turn_id:Some("turn-a".into()),continuity:Some(serde_json::from_value(state.clone()).unwrap()),
                    detail:json!({"already_terminal":true,"exact_result_pending":true,"terminal_evidence_source":"thread/read","native_terminal":state["native_terminal"]})})
            } else {None};
            let ack=if let Some(result)=&actual_stop {crate::handler::materialize_turn_control_result(&ctx.interface_var_dir().join("emitted"),stop_seq,result.clone()).unwrap()}
            else {artifact(&ctx.interface_var_dir().join("ack"),"turn.control.acknowledged",json!({
                "label":"turn.control.acknowledged","status":"acknowledged","control_id":"stop-a","action":"stop","source_movement_seq":stop_seq,
                "thread_id":null,"turn_id":null,"detail":{"already_terminal":true,"external_effect_dispatched":false,"linearization":"control_reader_terminal_fence"}
            }))};
            let admit=Box::pin(control.hand_final(ProviderEmission{artifact:ack,route:ProviderEmissionRoute::ClientProjection}));
            let gate=pump(&ctx,root,run_path,&authoring_id,&listener,receiver,&stop.artifact,0,1,true,0,true);
            let (result,acks)=tokio::join!(admit,gate);result.unwrap();
            let deferred=if phase==14 {
                let bytes=std::fs::read(run_path).unwrap();let record=RunRecord::from_bytes(&bytes).unwrap();
                let cp=record.checkpoints.iter().find(|cp|record.active_checkpoints.contains(&cp.id)
                    && cp.outbound_interfaces().any(|id|id==&descriptor.id) && cp.interface_binding.as_ref().unwrap().admits("operator.message")).unwrap();
                let file=artifact(&root.join("queued-before-settlement"),"operator.message",json!({"text":"keep this exact input"}));
                let row=edge(ctx,&bytes,cp,&file,MovementKind::InterfaceOutbound,descriptor.id.clone());
                let mut input=source_envelope(ctx,root,&row,file);
                let static_ids=record.interface_state[&descriptor.id]["run_authoring_binding"]["checkpoints"].as_array().unwrap().iter()
                    .flat_map(|template|template["static_artifact_bindings"].as_array().into_iter().flatten()).cloned().collect::<Vec<_>>();
                input.metadata.as_mut().unwrap().grant.role_artifacts.retain(|id|static_ids.contains(&json!(id)));
                let mut row=row;row.invocation_metadata=Some(input.metadata.clone().unwrap().into());let seq=append(ctx,row);
                let queued=QueuedTurn {seq,envelope:input};persist_deferred_provider_turn(ctx,run,&queued).unwrap();
                let cp=record.checkpoints.iter().find(|cp|record.active_checkpoints.contains(&cp.id)
                    && cp.inbound_interface()==Some(&InterfaceId::new(worker_interface_id)) && cp.outbound_interfaces().any(|id|id==&descriptor.id)).unwrap();
                let row=edge(ctx,&bytes,cp,&envelope.artifact_file,MovementKind::InterfaceOutbound,descriptor.id.clone());
                envelope=source_envelope(ctx,root,&row,envelope.artifact_file.clone());result_seq=append(ctx,row);pinned=bytes;
                assert!(queued.seq<result_seq);
                Some(DeferredProviderTurn {run:run.clone(),turn:queued,release:None})
            } else {None};
            if let Some(result)=actual_stop {
                persist_control_journal(ctx,stop_seq,run,"stop-a","stop",TurnControlJournalPhase::Delivered {result:result.clone()}).unwrap();
                arbiter.record_acknowledgement(run,stop_seq,"stop-a",DurableArtifactAdmission {artifact:acks[0].1.clone(),movement_seq:acks[0].2},result.continuity.unwrap()).await.unwrap();
                assert!(arbiter.begin_result(run,result_seq,&envelope).await.unwrap());
            }
            let failed=ProviderHandoffSession::new(&ctx).unwrap().from_invocation(&ctx,&descriptor,&run,&envelope,result_seq,&pinned).unwrap();
            if phase==14 {assert!(failed.input_changes_context().await);assert!(!envelope.artifact_binding_payloads.is_empty());}
            assert_eq!(Box::pin(failed.recover_source()).await.unwrap(),ProviderSourceRecovery::Process);
            if phase==19 {
                let source=arbiter.durable_continuity(run).await.unwrap().unwrap();
                let emissions=crate::handler::stopped_result::materialize_pending_interrupted(descriptor,&source,&envelope,result_seq,&ctx.interface_var_dir().join("emitted"),&ctx.interface_var_dir().join("streams"),failed.input_changes_context().await).unwrap().unwrap();
                let count=emissions.len();let reference=materialized_artifact_ref(&emissions.last().unwrap().artifact).unwrap();let broker=CodexTurnControlBroker::new(Some(result_seq));
                let admit=Box::pin(admit_interrupted_result(ctx,&failed,&broker,Some(&arbiter),run,result_seq,&reference,emissions));
                let gate=pump(ctx,root,run_path,&authoring_id,listener,receiver,&envelope.artifact,0,count,false,1,false);
                let (result,delivered)=tokio::join!(admit,gate);assert_eq!(result.unwrap(),InterruptedResultAdmission::Admitted(count));assert_eq!(delivered.last().unwrap().0,TURN_INTERRUPTED_RESULT_LABEL);
                assert!(arbiter.reconciled(run).await.unwrap());assert!(broker.is_finished());return;
            }
            state["context_generation"]=json!(before_context+3);
            let error=artifact(&ctx.interface_var_dir().join("error"),"turn.error",json!({
                "label":"turn.error","message":"error text is not recovery authority","will_retry":false,
                "additional_details":{"source_movement_seq":result_seq},"codex_continuity":state
            }));
            let admit=Box::pin(failed.hand_final(ProviderEmission{artifact:error,route:ProviderEmissionRoute::ClientProjection}));
            let gate=pump(&ctx,root,run_path,&authoring_id,&listener,receiver,&envelope.artifact,0,1,true,0,true);
            let (result,old)=tokio::join!(admit,gate);result.unwrap();
            let session=ProviderHandoffSession::new(&ctx).unwrap();
            assert!(session.completed_source_candidates(&ctx).unwrap().contains(&(run.clone(),result_seq)));
            let handoff=session.from_invocation(&ctx,&descriptor,&run,&envelope,result_seq,&pinned).unwrap();
            if phase>=14 {
                let turn=QueuedTurn {seq:result_seq,envelope:envelope.clone()};
                let journal=arbiter.journal(run).await.unwrap().unwrap();
                let saved=handoff.pending_source_settlement().await.unwrap().unwrap();assert!(saved.closure.is_none());
                let context=handoff.current_session_context_artifacts().await;
                let proof=prove_completed_interrupted_result(ctx,descriptor,run,&turn,&journal,&saved.predecessor_record,&context,&saved.predecessor_outcomes).unwrap();
                assert!(crate::handler::stopped_result::prove(descriptor,&envelope,result_seq,&saved.predecessor_record,&context,&saved.predecessor_outcomes).unwrap().is_none());
                for fault in ["result","ack","generation","correlation","terminal"] {
                    let mut forged=journal.clone();
                    if fault=="result" {forged.phase=InterruptedEffectPhase::Resolving {result_source_movement_seq:result_seq+1,source_artifact:envelope.artifact.clone()};}
                    if fault=="ack" {forged.acknowledgement.movement_seq+=1;}
                    if fault=="generation" {let mut value=serde_json::to_value(&forged.continuity).unwrap();value["context_generation"]=json!(value["context_generation"].as_u64().unwrap()+1);forged.continuity=serde_json::from_value(value).unwrap();}
                    if fault=="correlation" {let mut value=serde_json::to_value(&forged.continuity).unwrap();value["interrupted_pending"]["call_id"]=json!("different-call");forged.continuity=serde_json::from_value(value).unwrap();}
                    if fault=="terminal" {let mut value=serde_json::to_value(&forged.continuity).unwrap();value["native_terminal"]["turn"]["status"]=json!("completed");forged.continuity=serde_json::from_value(value).unwrap();}
                    assert!(prove_completed_interrupted_result(ctx,descriptor,run,&turn,&forged,&saved.predecessor_record,&context,&saved.predecessor_outcomes).is_err(),"{fault}");
                }
                let labels=codex_context_output_labels(descriptor).unwrap();
                let closure=crate::handler::stopped_result::materialize_interrupted(&proof,&envelope,&ctx.interface_var_dir().join("emitted"),&labels).unwrap();
                let expected:serde_json::Value=serde_json::from_slice(&closure.artifact.read_payload_bytes().unwrap()).unwrap();
                for field in ["correlation_id","source_artifact","ok","native_terminal_source_continuity","completed_source_settlement"] {
                    let mut forged=expected.clone();forged[field]=json!("forged");
                    assert!(crate::handler::stopped_result::validate_saved_settlement(&proof,&envelope,&forged,&labels).is_err(),"{field}");
                }
                let crash=match phase {14=>0,15=>2,16=>3,20|21=>20,_=>8};
                let admit=Box::pin(settle_completed_interrupted_result(ctx,descriptor,run,&turn,&handoff,&arbiter,&journal));
                let gate=pump(ctx,root,run_path,&authoring_id,listener,receiver,&envelope.artifact,0,1,true,crash,true);
                if crash==3 || crash==8 || crash==20 {
                    tokio::pin!(admit,gate);
                    tokio::select! {biased;_=&mut gate=>{},result=&mut admit=>panic!("settlement returned before crash: {result:?}")}
                } else {let (result,delivered)=tokio::join!(admit,gate);if crash==2 {assert!(result.is_err());assert!(delivered.is_empty());}else{result.unwrap();}}
                drop(handoff);
                let cold=InterruptedEffectArbiter::new(ctx.clone());
                if crash!=0 {
                    let restored=ProviderHandoffSession::new(ctx).unwrap().from_invocation(ctx,descriptor,run,&envelope,result_seq,&pinned).unwrap();
                    let journal=cold.journal(run).await.unwrap().unwrap();
                    let saved=restored.pending_source_settlement().await.unwrap().unwrap();assert!(saved.closure.is_some());
                    if crash==8 {
                        let before=std::fs::read(run_path).unwrap();let ledger=std::fs::read(&ctx.movements_path).unwrap();
                        let journal_bytes=std::fs::read(interrupted_effect_journal_path(ctx,run)).unwrap();
                        let calls=root.join("hand-in").join(authoring_id.as_str());
                        let (file,call)=std::fs::read_dir(&calls).unwrap().map(|entry|entry.unwrap().path())
                            .filter(|path|path.to_string_lossy().ends_with(".artifact.json")).find_map(|path| {
                                let file=InterfaceArtifactFile::new(path).unwrap();let value:serde_json::Value=serde_json::from_slice(&file.read_payload_bytes().unwrap()).unwrap();
                                let target=&value["arguments"]["writes"][1];
                                (target["expected_before_hash"]==ContentHash::of_bytes(&before).to_hex()).then_some((file,value))
                            }).unwrap();
                        let target=&call["arguments"]["writes"][1];
                        let mut candidate:serde_json::Value=serde_json::from_slice(&before).unwrap();
                        json_patch::patch(&mut candidate,&serde_json::from_value::<json_patch::Patch>(target["patch"].clone()).unwrap()).unwrap();
                        let mut payload:serde_json::Value=serde_json::from_slice(&saved.closure.as_ref().unwrap().read_payload_bytes().unwrap()).unwrap();
                        payload["correlation_id"]=json!("forged-saved-call");
                        let forged=artifact(&ctx.interface_var_dir().join("forged-settlement"),TURN_INTERRUPTED_RESULT_LABEL,payload);
                        let reference=cache_artifact(ctx,&forged);
                        let old=candidate["interface_state"][descriptor.id.as_str()]["pending_handoff"]["items"][0]["artifact_id"].as_str().unwrap();
                        let prefix=|id:&str| {use sha2::{Digest,Sha256};let mut hash=Sha256::new();hash.update(run.as_str().as_bytes());hash.update([0]);hash.update(id.as_bytes());hash.update([0]);hash.update(candidate["interface_state"][descriptor.id.as_str()]["authoring_generation"].as_u64().unwrap().to_le_bytes());format!("cp_{}",&format!("{:x}",hash.finalize())[..20])};
                        let encoded=serde_json::to_string(&candidate).unwrap().replace(&prefix(old),&prefix(reference.id.as_str())).replace(old,reference.id.as_str());candidate=serde_json::from_str(&encoded).unwrap();
                        let item=&mut candidate["interface_state"][descriptor.id.as_str()]["pending_handoff"]["items"][0];
                        item["artifact_content_hash"]=json!(reference.content.to_hex());item["manifest_relative_path"]=json!(forged.manifest_path().canonicalize().unwrap().strip_prefix(ctx.interface_var_dir().canonicalize().unwrap()).unwrap());
                        let edit=octopus_run_authoring::CheckedRunEdit::prepare_with_format(run_path.canonicalize().unwrap(),&before,candidate,call["correlation_id"].as_str().unwrap().into(),serde_json::from_value(target["json_format"].clone()).unwrap()).unwrap();
                        let mut changed=call.clone();let target=&mut changed["arguments"]["writes"][1];
                        target["patch"]=json!(edit.patch);target["expected_after_hash"]=json!(edit.after_hash);
                        let call_label=file.read_manifest().unwrap().label;
                        let hidden=file.manifest_path().with_extension("fixture-held");std::fs::rename(file.manifest_path(),&hidden).unwrap();
                        let attempt=artifact(&calls,&call_label,changed);
                        let probe=ProviderHandoffSession::new(ctx).unwrap().from_invocation(ctx,descriptor,run,&envelope,result_seq,&pinned).unwrap();
                        assert!(probe.pending_source_settlement().await.unwrap().unwrap().closure.is_some());
                        let error=settle_completed_interrupted_result(ctx,descriptor,run,&turn,&probe,&cold,&journal).await.unwrap_err();
                        assert!(error.contains("closure conflicts with durable arbitration"),"{error}");
                        let checked=materialized_artifact_ref(saved.closure.as_ref().unwrap()).unwrap();
                        let error=Box::pin(probe.recover_completed_source_settlement(&saved.predecessor,&checked)).await.unwrap_err();
                        assert!(error.contains("candidate changed after provider semantic validation"),"{error}");
                        assert_eq!(std::fs::read(run_path).unwrap(),before);assert_eq!(std::fs::read(&ctx.movements_path).unwrap(),ledger);
                        assert_eq!(std::fs::read(interrupted_effect_journal_path(ctx,run)).unwrap(),journal_bytes);assert!(receiver.try_recv().is_err());
                        std::fs::remove_file(attempt.manifest_path()).unwrap();std::fs::rename(hidden,file.manifest_path()).unwrap();
                    }
                    let resume=Box::pin(settle_completed_interrupted_result(ctx,descriptor,run,&turn,&restored,&cold,&journal));
                    let gate=pump(ctx,root,run_path,&authoring_id,listener,receiver,&envelope.artifact,usize::from(crash==3 || crash==20),1,true,0,true);
                    let (result,_)=tokio::time::timeout(std::time::Duration::from_secs(5),async{tokio::join!(resume,gate)}).await.unwrap();result.unwrap();
                }
                assert!(cold.reconciled(run).await.unwrap());
                let restored=ProviderHandoffSession::new(ctx).unwrap().from_invocation(ctx,descriptor,run,&envelope,result_seq,&pinned).unwrap();
                let before=std::fs::read(run_path).unwrap();let ledger=std::fs::read(&ctx.movements_path).unwrap();
                let (_,outcomes)=Box::pin(restored.recover_completed_source_with_outcomes()).await.unwrap();assert_eq!(outcomes.len(),1);
                let context=restored.current_session_context_artifacts().await;
                let closed=context.iter().find(|entry|entry.label==TURN_INTERRUPTED_RESULT_LABEL).unwrap();assert_eq!(closed.payload,expected);
                assert!(context.iter().any(|entry|entry.source_artifact==old[0].1));
                let current=RunRecord::from_bytes(&before).unwrap();
                let heads=octopus_run_authoring::provider_context_bindings(&current,&descriptor.id).unwrap();
                assert!(heads.iter().any(|binding|binding.id==closed.source_artifact.id));assert!(!heads.iter().any(|binding|binding.id==old[0].1.id));
                assert_eq!(std::fs::read(run_path).unwrap(),before);assert_eq!(std::fs::read(&ctx.movements_path).unwrap(),ledger);
                if let Some(deferred)=deferred {
                    let controls=ActiveTurnControls::default();
                    let DeferredTurnReadiness::Ready(released)=prepare_deferred_provider_turn(ctx,descriptor,run,&deferred,&controls).await.unwrap()
                        else {panic!("exact queued input remains blocked after settlement")};
                    assert_eq!(released.seq,deferred.turn.seq);assert_eq!(released.envelope.artifact,deferred.turn.envelope.artifact);
                    assert_eq!(released.envelope.read_artifact_payload().unwrap(),deferred.turn.envelope.read_artifact_payload().unwrap());
                    let mut saved=load_deferred_provider_turns(ctx,descriptor).unwrap();
                    let saved=saved.remove(&deferred.turn.seq).unwrap();
                    assert!(matches!(&saved.release,Some(DeferredProviderTurnRelease::InterruptedResult {result_source,closure,..})
                        if result_source.movement_seq==result_seq && result_source.artifact==envelope.artifact
                            && closure.movement_seq>result_seq && closure.artifact==closed.source_artifact));
                    std::fs::remove_file(interrupted_effect_journal_path(ctx,run)).unwrap();
                    let control_path=control_journal_path(ctx,stop_seq);let control_bytes=std::fs::read(&control_path).unwrap();
                    std::fs::remove_file(&control_path).unwrap();
                    assert!(prepare_deferred_provider_turn(ctx,descriptor,run,&saved,&controls).await.is_err());
                    std::fs::write(&control_path,&control_bytes).unwrap();
                    let DeferredTurnReadiness::Ready(replayed)=prepare_deferred_provider_turn(ctx,descriptor,run,&saved,&controls).await.unwrap()
                        else {panic!("persisted release did not restore its exact input")};
                    assert_eq!(replayed.seq,released.seq);assert_eq!(replayed.envelope.artifact,released.envelope.artifact);
                    assert_eq!(serde_json::to_value(&replayed.envelope.metadata).unwrap(),serde_json::to_value(&deferred.turn.envelope.metadata).unwrap());
                    let release=std::fs::read(deferred_provider_turn_path(ctx,replayed.seq)).unwrap();
                    for _ in 0..2 {
                        let source=ctx.pinned_run_record_bytes(&replayed.envelope).unwrap();
                        let replay=ProviderHandoffSession::new(ctx).unwrap().from_invocation(ctx,descriptor,run,&replayed.envelope,replayed.seq,&source).unwrap();
                        assert_eq!(tokio::time::timeout(std::time::Duration::from_secs(5),Box::pin(replay.recover_source_with_outcomes())).await.unwrap().unwrap(),(ProviderSourceRecovery::Process,vec![]));
                        assert!(replay.current_session_context_artifacts().await.iter().any(|entry|entry.source_artifact==closed.source_artifact));
                    }
                    assert_eq!(std::fs::read(deferred_provider_turn_path(ctx,replayed.seq)).unwrap(),release);assert!(receiver.try_recv().is_err());
                    assert_eq!(std::fs::read(run_path).unwrap(),before);assert_eq!(std::fs::read(&ctx.movements_path).unwrap(),ledger);
                }
                return;
            }
            let (recovery,outcomes)=Box::pin(handoff.recover_completed_source_with_outcomes()).await.unwrap();assert_eq!(recovery,ProviderSourceRecovery::Completed);
            let context=handoff.current_session_context_artifacts().await;
            let proof=crate::handler::stopped_result::prove(&descriptor,&envelope,result_seq,&handoff.current_run_record_bytes().await,&context,&outcomes).unwrap().unwrap();
            validate_stopped_result_control(&ctx,&descriptor,&run,&proof).unwrap();
            for forged in [false,true] {
                let mut changed=proof.clone();
                if forged { changed.acknowledgement.content=ContentHash::of_bytes(b"forged acknowledgement"); } else { changed.acknowledgement_seq+=1; }
                assert!(validate_stopped_result_control(&ctx,&descriptor,&run,&changed).is_err());
            }
            let labels=codex_context_output_labels(&descriptor).unwrap();let output_root=ctx.interface_var_dir().join("emitted");
            for status in ["inProgress","completed"] {
                let mut wrong=native.clone();wrong["status"]=json!(status);
                assert!(crate::handler::stopped_result::materialize_for_test(&proof,&envelope,wrong,&output_root,&labels).is_err());
            }
            let closure=crate::handler::stopped_result::materialize_for_test(&proof,&envelope,native.clone(),&output_root,&labels).unwrap();
            let expected:serde_json::Value=serde_json::from_slice(&closure.artifact.read_payload_bytes().unwrap()).unwrap();
            assert_eq!(expected["ok"],true);assert_eq!(expected["returned_to_model"],false);
            assert_eq!(expected["native_terminal_input_context"],false);assert_eq!(expected["codex_continuity"]["context_generation"],json!(before_context+4));
            assert_eq!(expected["completed_source_settlement"]["error_artifact"],json!(old[0].1));
            let admit=Box::pin(handoff.hand_completed_source_settlement(&proof.predecessor,proof.context_generation(),closure));
            let gate=pump(&ctx,root,run_path,&authoring_id,&listener,receiver,&envelope.artifact,0,1,true,phase,true);
            if phase==3 {
                tokio::pin!(admit,gate);
                tokio::select! { biased; delivered=&mut gate=>assert_eq!(delivered.len(),1), result=&mut admit=>panic!("settlement returned before crash boundary: {result:?}") }
            } else {
                let (result,delivered)=tokio::join!(admit,gate);
                if phase==2 { assert!(result.is_err());assert!(delivered.is_empty()); } else { result.unwrap();assert_eq!(delivered.len(),1); }
            }
            drop(handoff);
            let restored=ProviderHandoffSession::new(&ctx).unwrap().from_invocation(&ctx,&descriptor,&run,&envelope,result_seq,&pinned).unwrap();
            if phase!=0 {
                let replay=Box::pin(restored.recover_completed_source_with_outcomes());
                let gate=pump(&ctx,root,run_path,&authoring_id,&listener,receiver,&envelope.artifact,usize::from(phase==3),1,true,0,true);
                let (result,delivered)=tokio::time::timeout(std::time::Duration::from_secs(5),async{tokio::join!(replay,gate)}).await.unwrap();
                assert_eq!(result.unwrap().0,ProviderSourceRecovery::Completed);assert_eq!(delivered.len(),usize::from(phase==2));
            }
            let before=std::fs::read(run_path).unwrap();let ledger=std::fs::read(&ctx.movements_path).unwrap();
            let (recovery,outcomes)=Box::pin(restored.recover_completed_source_with_outcomes()).await.unwrap();assert_eq!(recovery,ProviderSourceRecovery::Completed);assert_eq!(outcomes.len(),1);
            let context=restored.current_session_context_artifacts().await;
            let admitted=context.iter().find(|entry|entry.label==TURN_INTERRUPTED_RESULT_LABEL).unwrap();
            assert_eq!(admitted.payload,expected);
            assert!(context.iter().any(|entry|entry.source_artifact==old[0].1));
            assert!(crate::handler::stopped_result::prove(&descriptor,&envelope,result_seq,&before,&context,&outcomes).unwrap().is_none());
            assert_eq!(std::fs::read(run_path).unwrap(),before);assert_eq!(std::fs::read(&ctx.movements_path).unwrap(),ledger);
            let current=RunRecord::from_bytes(&before).unwrap();
            let cp=current.checkpoints.iter().find(|cp|current.active_checkpoints.contains(&cp.id)
                && cp.outbound_interfaces().any(|id|id==&descriptor.id) && cp.interface_binding.as_ref().unwrap().admits("operator.message")).unwrap();
            let message=artifact(&root.join("next-message"),"operator.message",json!({"text":"inspect installed work"}));
            let row=edge(&ctx,&before,cp,&message,MovementKind::InterfaceOutbound,descriptor.id.clone());
            let message=source_envelope(&ctx,root,&row,message);let next_seq=append(&ctx,row);
            let next=ProviderHandoffSession::new(&ctx).unwrap().from_invocation(&ctx,&descriptor,&run,&message,next_seq,&before).unwrap();
            assert_eq!(Box::pin(next.recover_source()).await.unwrap(),ProviderSourceRecovery::Process);
            assert!(next.current_session_context_artifacts().await.iter().any(|entry|entry.label==TURN_INTERRUPTED_RESULT_LABEL));
        })
    }
    async fn case(with_stop:bool, with_answer:bool, native_interrupted:bool, settlement_phase:Option<usize>, legacy:bool) {
        eprintln!("native fixture stop={with_stop} answer={with_answer} native_interrupted={native_interrupted} phase={settlement_phase:?} legacy={legacy}");
        let temp=tempfile::tempdir().unwrap(); let root=temp.path();
        let mut request=terminal_composition::client_fixture(root);
        let selected_file=settlement_phase.filter(|phase|*phase>=12).map(|_| {
            InterfaceArtifactFile::write(root.join("selected-attachment"),"attachment.text",b"Selected plain text is not JSON.\n").unwrap()
        });
        if let Some(file)=&selected_file {request.payload_artifact_bindings.push(materialized_artifact_ref(file).unwrap());}
        let plan=octopus_operator_chat_client::run_composition::plan_run_composition(&request).unwrap();
        for write in plan.descriptor_writes.iter().chain(&plan.run_writes) { std::fs::create_dir_all(write.path.parent().unwrap()).unwrap(); std::fs::write(&write.path,&write.content).unwrap(); }
        let mut initial:serde_json::Value=serde_json::from_str(&plan.run_write.content).unwrap();
        let templates=&initial["interface_state"][&plan.provider_interface_id]["run_authoring_binding"]["checkpoints"];
        let index=templates.as_array().unwrap().iter().position(|template|template["role"]=="stable" && template["include_context"]==true).unwrap();
        let template=&templates[index];
        let row=initial["checkpoints"].as_array().unwrap().iter().find(|row|row["interface_binding"]==template["interface_binding"]
            && row["outbound_interface_bindings"]==template["outbound_interface_bindings"] && row["required_verified_artifacts"]==template["required_verified_artifacts"]).unwrap();
        let static_ids=row["artifact_bindings"].as_array().unwrap().iter().map(|binding|binding["id"].clone()).collect::<Vec<_>>();
        assert!(!static_ids.is_empty());
        initial["interface_state"][&plan.provider_interface_id]["run_authoring_binding"]["checkpoints"][index]["static_artifact_bindings"]=json!(static_ids);
        assert!(initial["interface_state"][&plan.provider_interface_id]["run_authoring_binding"]["writer_run"].is_object());
        let initial=serde_json::to_vec(&initial).unwrap(); let record=RunRecord::from_bytes(&initial).unwrap(); let run=record.id.clone();
        std::fs::create_dir_all(plan.run_write.path.parent().unwrap()).unwrap(); std::fs::write(&plan.run_write.path,&initial).unwrap();
        let mut descriptor:InterfaceDescriptor=serde_json::from_slice(&std::fs::read(root.join("registry/interface").join(&plan.provider_interface_id).join("interface.json")).unwrap()).unwrap();
        let canonical=super::tests::descriptor();
        if settlement_phase == Some(26) {
            descriptor.interface_driver_ref = canonical.interface_driver_ref.clone();
            for (key, value) in &canonical.interface_driver_config.as_ref().unwrap().private_config {
                descriptor.interface_driver_config.as_mut().unwrap().private_config.entry(key.clone()).or_insert_with(|| value.clone());
            }
        }
        for key in ["context_output_labels","client_projection_output_labels","session_snapshot_labels","live_stream"] {
            descriptor.interface_driver_config.as_mut().unwrap().private_config.insert(key.into(), canonical.interface_driver_config.as_ref().unwrap().private_config[key].clone());
        }
        if settlement_phase.is_some_and(|phase|phase>=14) {
            let native:InterfaceDescriptor=serde_json::from_slice(&std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../interface.json")).unwrap()).unwrap();
            for key in ["model_result_projection","native_tool_namespace"] {descriptor.interface_driver_config.as_mut().unwrap().private_config.insert(key.into(),native.interface_driver_config.as_ref().unwrap().private_config[key].clone());}
        }
        let (arrivals,mut receiver)=tokio::sync::mpsc::channel(8);
        let mut ctx=context(root,&descriptor); ctx.interface_dir=root.join("registry/interface").join(&plan.provider_interface_id); ctx.arrivals=arrivals;
        ctx.root_slots=BTreeMap::from([("run_records_root".into(),root.join("runs")),("interface_hand_in_root".into(),root.join("hand-in")),("app_server_control_root".into(),root.join("control"))]);
        std::fs::write(&ctx.movements_path,b"").unwrap();
        if let Some(file)=&selected_file {cache_artifact(&ctx,file);}
        let authoring=record.interface_state[&descriptor.id]["run_authoring_binding"]["authoring_source_interface"].as_str().unwrap();
        let authoring_id=InterfaceId::new(authoring);
        let socket=octopus_app_server_interface::interface_control_socket_path_from_root(&root.join("control"),&authoring_id);
        std::fs::create_dir_all(socket.parent().unwrap()).unwrap(); let listener=tokio::net::UnixListener::bind(socket).unwrap();
        let native=serde_json::to_value(octopus_appserver_proto::Turn { id:"turn-a".into(),items:if with_answer {vec![octopus_appserver_proto::ThreadItem::AgentMessage{id:"answer-a".into(),text:"Retained native answer".into(),phase:Some(octopus_protocol::models::MessagePhase::FinalAnswer)}]} else {vec![]},items_view:octopus_appserver_proto::TurnItemsView::Full,status:if with_answer {octopus_appserver_proto::TurnStatus::Completed} else if native_interrupted {octopus_appserver_proto::TurnStatus::Interrupted} else {octopus_appserver_proto::TurnStatus::Failed},error:None,started_at:Some(10),completed_at:Some(20),duration_ms:Some(10_000) }).unwrap();
        let before_context=record.interface_state[&descriptor.id]["context_generation"].as_u64().unwrap();
        let mut state=serde_json::to_value(continuity(before_context,Some(if with_stop {"interrupted_pending"}else{"pending"}))).unwrap();
        state["provider_interface"]=json!(descriptor.id); state["run"]=json!(run);
        if settlement_phase == Some(26) {
            let cp=record.checkpoints.iter().find(|cp|record.active_checkpoints.contains(&cp.id)
                && cp.outbound_interfaces().any(|id|id==&descriptor.id) && cp.interface_binding.as_ref().unwrap().admits("operator.message")).unwrap();
            let file=artifact(&root.join("message"),"operator.message",json!({"text":"start","message_id":"initial"}));
            let row=edge(&ctx,&initial,cp,&file,MovementKind::InterfaceOutbound,descriptor.id.clone());
            let input=source_envelope(&ctx,root,&row,file); let source_seq=append(&ctx,row);
            let handoff=ProviderHandoffSession::new(&ctx).unwrap().from_invocation(&ctx,&descriptor,&run,&input,source_seq,&initial).unwrap();
            let (stream, emissions)=ProviderOutputStream::channel();
            let (terminal_tx,terminal_rx)=tokio::sync::oneshot::channel();
            let control=CodexTurnControlBroker::new(Some(source_seq));
            let forward=forward_provider_emissions(EmissionForwarding {
                ctx:&ctx,descriptor:&descriptor,control:&control,handoff,
                interrupted_effects:InterruptedEffectArbiter::new(ctx.clone()),durable_interrupted_continuity:None,native_terminal_source:None,
                source_envelope:input.clone(),run:run.clone(),source_artifact:input.artifact.clone(),result_source_movement_seq:source_seq,resolving_interrupted_result:false,
            },emissions,terminal_rx);
            let forward=async {forward.await.inspect_err(|error| eprintln!("retained native forwarder: {error}"))};
            let native=retained_admission_native_turn(root,&descriptor,&input,&record,source_seq,&control,stream,terminal_tx);
            let gate=pump(&ctx,root,&plan.run_write.path,&authoring_id,&listener,&mut receiver,&input.artifact,0,usize::MAX,false,26,false);
            let forwarded=tokio::time::timeout(std::time::Duration::from_secs(10),async {
                tokio::select! {
                    result=async {tokio::join!(native,forward)} => result.1.unwrap(),
                    _=gate => panic!("admission gate ended before native turn"),
                }
            }).await.unwrap();
            assert!(forwarded.outputs >= 3);
            let rows=std::fs::read_to_string(&ctx.movements_path).unwrap().lines().map(|line|serde_json::from_str::<Movement>(line).unwrap()).collect::<Vec<_>>();
            let refused=rows.iter().find(|row|matches!(&row.invocation_metadata,Some(octopus_ledger::MovementMetadata::Delivery(meta)) if meta.delivery_verdict.awaits_current_source())).unwrap();
            let scope=match &refused.invocation_metadata {Some(octopus_ledger::MovementMetadata::Delivery(meta))=>meta.delivery_scope_seq.unwrap(),_=>unreachable!()};
            assert_eq!(rows.iter().filter(|row|row.kind==MovementKind::ArtifactMoved && row.artifact==refused.artifact).count(),1);
            assert_eq!(rows.iter().filter(|row|row.kind==MovementKind::InterfaceArrival && row.invocation_metadata.as_ref().and_then(|m|m.invocation()).is_some_and(|m|m.delivery_scope_seq==Some(scope))).count(),1);
            return;
        }
        if settlement_phase==Some(25) {
            let cp=record.checkpoints.iter().find(|cp|record.active_checkpoints.contains(&cp.id)
                && cp.outbound_interfaces().any(|id|id==&descriptor.id) && cp.interface_binding.as_ref().unwrap().admits("operator.message")).unwrap();
            let file=artifact(&root.join("message"),"operator.message",json!({"text":"start","message_id":"initial"}));
            let row=edge(&ctx,&initial,cp,&file,MovementKind::InterfaceOutbound,descriptor.id.clone());
            let input=source_envelope(&ctx,root,&row,file);let source_seq=append(&ctx,row);
            let handoff=ProviderHandoffSession::new(&ctx).unwrap().from_invocation(&ctx,&descriptor,&run,&input,source_seq,&initial).unwrap();
            state["context_generation"]=json!(before_context+1);
            let progress=artifact(&ctx.interface_var_dir().join("progress"),"turn.agent_message",json!({"label":"turn.agent_message","phase":"commentary","text":"Working","codex_continuity":state}));
            let (sender,receiver_emissions)=tokio::sync::mpsc::unbounded_channel();
            let (terminal_sender,terminal_receiver)=tokio::sync::oneshot::channel();
            sender.send(ProviderStreamEmission::Progress(ProviderEmission{artifact:progress,route:ProviderEmissionRoute::ClientProjection})).unwrap();
            let broker=CodexTurnControlBroker::new(Some(source_seq));
            let forward=forward_provider_emissions(EmissionForwarding {
                ctx:&ctx,descriptor:&descriptor,control:&broker,handoff:handoff.clone(),
                interrupted_effects:InterruptedEffectArbiter::new(ctx.clone()),
                durable_interrupted_continuity:None,native_terminal_source:None,
                source_envelope:input.clone(),run:run.clone(),source_artifact:input.artifact.clone(),
                result_source_movement_seq:source_seq,resolving_interrupted_result:false,
            },receiver_emissions,terminal_receiver);
            let gate=async {
                let first=pump(&ctx,root,&plan.run_write.path,&authoring_id,&listener,&mut receiver,&input.artifact,0,1,true,0,false).await;
                assert_eq!(first.len(),1);
                assert_eq!(first[0].0,"turn.agent_message");
                state["context_generation"]=json!(before_context+2);
                let output=artifact(&ctx.interface_var_dir().join("pending"),"interface.artifact.output",json!({"label":"interface.artifact.output","codex_continuity":state}));
                sender.send(ProviderStreamEmission::Turn(ProviderEmission{artifact:output,route:ProviderEmissionRoute::ClientProjection})).unwrap();
                drop(sender);
                terminal_sender.send(true).unwrap();
                pump(&ctx,root,&plan.run_write.path,&authoring_id,&listener,&mut receiver,&input.artifact,0,1,true,0,true).await
            };
            let (forwarded,delivered)=tokio::time::timeout(std::time::Duration::from_secs(5),async{tokio::join!(forward,gate)}).await.unwrap();
            assert_eq!(forwarded.unwrap().outputs,2);
            assert_eq!(delivered.len(),1);
            let current=RunRecord::from_bytes(&std::fs::read(&plan.run_write.path).unwrap()).unwrap();
            let settled=&current.interface_state[&descriptor.id]["pending_handoff"];
            assert_eq!((settled["source_movement_seq"].clone(),settled["terminal"].clone()),(json!(source_seq),json!(true)));
            return;
        }
        if settlement_phase==Some(22) {
            let cp=record.checkpoints.iter().find(|cp|record.active_checkpoints.contains(&cp.id)
                && cp.outbound_interfaces().any(|id|id==&descriptor.id) && cp.interface_binding.as_ref().unwrap().admits("operator.message")).unwrap();
            let file=artifact(&root.join("message"),"operator.message",json!({"text":"start","message_id":"initial"}));
            let row=edge(&ctx,&initial,cp,&file,MovementKind::InterfaceOutbound,descriptor.id.clone());
            let input=source_envelope(&ctx,root,&row,file);let source_seq=append(&ctx,row);
            let session=ProviderHandoffSession::new(&ctx).unwrap();
            let handoff=session.from_invocation(&ctx,&descriptor,&run,&input,source_seq,&initial).unwrap();
            state["context_generation"]=json!(before_context+1);
            let pending=artifact(&ctx.interface_var_dir().join("pending"),"interface.artifact.output",json!({"label":"interface.artifact.output","codex_continuity":state}));
            let hand=Box::pin(handoff.hand_final(ProviderEmission{artifact:pending,route:ProviderEmissionRoute::ClientProjection}));
            let gate=pump(&ctx,root,&plan.run_write.path,&authoring_id,&listener,&mut receiver,&input.artifact,0,1,true,24,true);
            let (result,_)=tokio::time::timeout(std::time::Duration::from_secs(5),async{tokio::join!(hand,gate)}).await.unwrap();result.unwrap();drop(handoff);
            let before=std::fs::read(&plan.run_write.path).unwrap();let selected=RunRecord::from_bytes(&before).unwrap();
            let file=artifact(&root.join("clarification"),"operator.message",json!({"text":"clarification","message_id":"steer-pending"}));
            let reference=cache_artifact(&ctx,&file);
            let row=std::fs::read_to_string(&ctx.movements_path).unwrap().lines().map(|line|serde_json::from_str::<Movement>(line).unwrap())
                .find(|row|row.artifact.as_ref()==Some(&reference)).unwrap();
            let seq=row.seq.0;let input=source_envelope(&ctx,root,&row,file);
            assert_ne!(input.metadata().unwrap().runtime.as_ref().unwrap().run_record_hash.as_deref(),Some(ContentHash::of_bytes(&before).to_hex().as_str()));
            let result=CodexTurnControlResult {control_id:"steer-pending".into(),action:"steer".into(),acknowledged:true,
                thread_id:Some("thread-a".into()),turn_id:Some("turn-a".into()),detail:json!({"stage":"accepted_by_app_server","consumption_proven":false}),continuity:None};
            persist_control_journal(&ctx,seq,&run,"steer-pending","steer",TurnControlJournalPhase::Result{result}).unwrap();
            let broker=CodexTurnControlBroker::new(Some(source_seq));broker.finish();let gates=Default::default();let arbiter=InterruptedEffectArbiter::new(ctx.clone());
            let source=|independent_reader|ControlSource {envelope:&input,source_movement_seq:seq,broker:&broker,control_gates:&gates,
                control:decode_admitted_turn_control(&ctx,&descriptor,&input).unwrap().unwrap(),terminal_control_fenced:false,independent_reader};
            handle_control_source(&ctx,&descriptor,&session,&arbiter,&run,source(true)).await.unwrap();
            assert!(matches!(load_control_journal(&ctx,seq,&run,"steer-pending","steer").unwrap().unwrap().phase,TurnControlJournalPhase::Result{..}));
            assert_eq!(std::fs::read(&plan.run_write.path).unwrap(),before);
            let hand=handle_control_source(&ctx,&descriptor,&session,&arbiter,&run,source(false));
            let gate=pump(&ctx,root,&plan.run_write.path,&authoring_id,&listener,&mut receiver,&input.artifact,0,1,true,22,true);
            let (_,delivered)=tokio::time::timeout(std::time::Duration::from_secs(5),async{tokio::try_join!(hand,async{Ok::<_,String>(gate.await)})}).await.unwrap().unwrap();
            assert_eq!(delivered[0].0,"turn.input.acknowledged");
            let after=std::fs::read(&plan.run_write.path).unwrap();let current=RunRecord::from_bytes(&after).unwrap();
            assert_eq!(current.interface_state[&descriptor.id]["context_generation"],selected.interface_state[&descriptor.id]["context_generation"]);
            assert_eq!(octopus_run_authoring::provider_context_bindings(&current,&descriptor.id).unwrap(),octopus_run_authoring::provider_context_bindings(&selected,&descriptor.id).unwrap());
            let ledger=std::fs::read(&ctx.movements_path).unwrap();
            let saved=load_control_journal(&ctx,seq,&run,"steer-pending","steer").unwrap().unwrap();
            let TurnControlJournalPhase::Delivered{result}=saved.phase else {panic!("durable steer receipt")};
            persist_control_journal(&ctx,seq,&run,"steer-pending","steer",TurnControlJournalPhase::Result{result}).unwrap();
            let cold=ProviderHandoffSession::new(&ctx).unwrap();
            handle_control_source(&ctx,&descriptor,&cold,&arbiter,&run,source(false)).await.unwrap();
            assert_eq!(std::fs::read(&plan.run_write.path).unwrap(),after);assert_eq!(std::fs::read(&ctx.movements_path).unwrap(),ledger);
            let cp=current.checkpoints.iter().find(|cp|current.active_checkpoints.contains(&cp.id)
                && cp.inbound_interface()==Some(&InterfaceId::new(&plan.worker_interface_id)) && cp.outbound_interfaces().any(|id|id==&descriptor.id)).unwrap();
            let file=artifact(&root.join("pending-result"),"coder.exec.result",json!({"label":"coder.exec.result","tool":"coder.exec","correlation_id":"call-a","ok":true,"body":{}}));
            let row=edge(&ctx,&after,cp,&file,MovementKind::InterfaceOutbound,descriptor.id.clone());
            let input=source_envelope(&ctx,root,&row,file);let seq=append(&ctx,row);
            let next=cold.from_invocation(&ctx,&descriptor,&run,&input,seq,&after).unwrap();
            assert_eq!(next.recover_source().await.unwrap(),ProviderSourceRecovery::Process);
            let recovered=recover_provider_result_continuity(&descriptor,&input,next.invocation_session_snapshot().as_ref(),&next.current_session_context_artifacts().await,&run,&after).unwrap().unwrap();
            assert_eq!(serde_json::to_value(recovered).unwrap(),state);
            assert!(receiver.try_recv().is_err());return;
        }
        if let Some(phase)=settlement_phase {
            settlement_case(&ctx,root,&plan.run_write.path,&plan.worker_interface_id,&descriptor,&run,&record,
                &initial,state,before_context,native,phase,&authoring_id,&listener,&mut receiver,selected_file.as_ref()).await;
            return;
        }
        let mut arbiter=InterruptedEffectArbiter::new(ctx.clone());
        if with_stop {
            let cp=record.checkpoints.iter().find(|cp|record.active_checkpoints.contains(&cp.id)
                && cp.outbound_interfaces().any(|id|id==&descriptor.id)
                && cp.interface_binding.as_ref().unwrap().admits("operator.turn.control")).unwrap();
            let control=artifact(&root.join("control-input"),"operator.turn.control",json!({"action":"stop","control_id":"stop-a"}));
            let source=edge(&ctx,&initial,cp,&control,MovementKind::InterfaceOutbound,descriptor.id.clone());
            let control=source_envelope(&ctx,root,&source,control); let control_seq=append(&ctx,source);
            let handoff=ProviderHandoffSession::new(&ctx).unwrap().from_invocation(&ctx,&descriptor,&run,&control,control_seq,&initial).unwrap();
            assert!(!handoff.input_changes_context().await);
            state["context_generation"]=json!(before_context+1);
            if !legacy {state["native_terminal"]=json!({"thread_id":"thread-a","turn":native});}
            let acknowledged:crate::handler::CodexContinuityState=serde_json::from_value(state.clone()).unwrap();
            let ack=artifact(&ctx.interface_var_dir().join("ack"),"turn.control.acknowledged",json!({"label":"turn.control.acknowledged","control_id":"stop-a","action":"stop","codex_continuity":acknowledged}));
            let admit=async {
                let result=Box::pin(handoff.hand_final(ProviderEmission{artifact:ack,route:ProviderEmissionRoute::ClientProjection})).await;
                if let Err(error)=&result { eprintln!("Stop ACK bootstrap: {error}"); } result
            };
            let gate=pump(&ctx,root,&plan.run_write.path,&authoring_id,&listener,&mut receiver,&control.artifact,0,1,true,0,true);
            let (result,delivered)=tokio::time::timeout(std::time::Duration::from_secs(5),async{tokio::join!(admit,gate)}).await.unwrap();
            assert_eq!(delivered.len(),1); assert_eq!(delivered[0].0,"turn.control.acknowledged");
            assert!(matches!(result.unwrap(),octopus_run_authoring::ProviderHandoffOutcome::Admitted{seq,artifact}
                if seq==delivered[0].2 && artifact==delivered[0].1));
            arbiter.record_acknowledgement(&run,control_seq,"stop-a",DurableArtifactAdmission{artifact:delivered[0].1.clone(),movement_seq:delivered[0].2},acknowledged).await.unwrap();
        }
        let initial=std::fs::read(&plan.run_write.path).unwrap(); let record=RunRecord::from_bytes(&initial).unwrap();
        let base_context=record.interface_state[&descriptor.id]["context_generation"].as_u64().unwrap();
        assert_eq!(base_context,before_context+u64::from(with_stop));
        assert_eq!(state["context_generation"],json!(base_context));
        let original:crate::handler::CodexContinuityState=serde_json::from_value(state).unwrap();
        let cp=record.checkpoints.iter().find(|cp|record.active_checkpoints.contains(&cp.id)
            && cp.inbound_interface()==Some(&InterfaceId::new(&plan.worker_interface_id))
            && cp.outbound_interfaces().any(|id|id==&descriptor.id)).unwrap();
        let result=artifact(&root.join("result"),"coder.exec.result",json!({"label":"coder.exec.result","tool":"coder.exec","correlation_id":"call-a","ok":false,"error":{"code":"tool_effect_indeterminate","message":"uncertain exact effect"}}));
        let source=edge(&ctx,&initial,cp,&result,MovementKind::InterfaceOutbound,descriptor.id.clone());
        let envelope=source_envelope(&ctx,root,&source,result); let result_ref=envelope.artifact.clone(); let source_seq=append(&ctx,source);
        if with_stop { assert!(arbiter.begin_result(&run,source_seq,&envelope).await.unwrap()); }
        let session=ProviderHandoffSession::new(&ctx).unwrap();
        let mut handoff=session.from_invocation(&ctx,&descriptor,&run,&envelope,source_seq,&initial).unwrap();
        let input_context=handoff.input_changes_context().await;
        let count=if legacy {2}else if with_answer {4}else{3};
        let settled_generation=base_context+u64::from(input_context)+if with_answer {2}else{1};
        let output_root=ctx.interface_var_dir().join("emitted"); let stream_root=ctx.interface_var_dir().join("streams");
        let ProviderStreamEmission::InterruptedResult { stream:Some(stream),retained_outputs,closure,.. }=crate::handler::native_terminal_group_for_test(&descriptor,&output_root,&stream_root,&original,(!legacy).then_some(native),&result_ref,source_seq,with_stop,input_context) else {panic!("sealed native group")};
        let source_closure=materialized_artifact_ref(&closure.artifact).unwrap();
        let turn=QueuedTurn{seq:source_seq,envelope:envelope.clone()};
        let (cold,cold_ref)=recover_sealed_interrupted_result(&ctx,&descriptor,&turn,&original).unwrap().unwrap();
        assert_eq!(cold_ref,source_closure); assert_eq!(cold.len(),count);
        let mut emissions=vec![stream]; emissions.extend(retained_outputs); emissions.push(closure);
        for (live,cold) in emissions.iter().zip(&cold) { assert_eq!(materialized_artifact_ref(&live.artifact).unwrap(),materialized_artifact_ref(&cold.artifact).unwrap()); assert_eq!(live.artifact.read_payload_bytes().unwrap(),cold.artifact.read_payload_bytes().unwrap()); }
        let closed:serde_json::Value=serde_json::from_slice(&emissions[count-1].artifact.read_payload_bytes().unwrap()).unwrap();
        assert_eq!(closed["source_artifact"],json!(result_ref)); assert_eq!(closed["codex_continuity"]["context_generation"],json!(settled_generation));
        assert_eq!(closed["native_terminal_input_context"],if legacy {json!(null)}else{json!(input_context)});
        if with_answer {
            assert_eq!(emissions[1].artifact.read_manifest().unwrap().label,"interface.artifact.output");
            assert_eq!(emissions[2].artifact.read_manifest().unwrap().label,"turn.completed");
            assert!(handoff.hand_observation(ProviderEmission{artifact:emissions[1].artifact.clone(),route:emissions[1].route.clone()},false).await.unwrap_err().contains("cannot change the selected context"));
        }
        let idle_stop_case=with_stop && !with_answer && !native_interrupted && !legacy;
        let delayed_stop=idle_stop_case.then(|| {
            let cp=record.checkpoints.iter().find(|cp|record.active_checkpoints.contains(&cp.id)
                && cp.outbound_interfaces().any(|id|id==&descriptor.id)
                && cp.interface_binding.as_ref().unwrap().admits("operator.turn.control")).unwrap();
            let file=artifact(&root.join("delayed-stop"),"operator.turn.control",json!({"action":"stop","control_id":"idle-0"}));
            let row=edge(&ctx,&initial,cp,&file,MovementKind::InterfaceOutbound,descriptor.id.clone());
            let input=source_envelope(&ctx,root,&row,file); (input,append(&ctx,row))
        });
        let mut all_delivered=Vec::new();
        let control = CodexTurnControlBroker::new(Some(source_seq));
        assert!(!control.is_finished());
        for phase in 0..if with_answer {2}else{1} {
        let gate=pump(&ctx,root,&plan.run_write.path,&authoring_id,&listener,&mut receiver,&result_ref,all_delivered.len(),count,with_answer,phase,false);
        let supplied=if phase==0 {emissions.iter().map(|e|ProviderEmission{artifact:e.artifact.clone(),route:e.route.clone()}).collect()} else {recover_sealed_interrupted_result(&ctx,&descriptor,&turn,&original).unwrap().unwrap().0};
        let admit=async {
            let result=admit_interrupted_result(&ctx,&handoff,&control,with_stop.then_some(&arbiter),&run,source_seq,&source_closure,supplied).await;
            if let Err(error)=&result { eprintln!("native terminal handoff stop={with_stop} answer={with_answer} phase={phase}: {error}"); }
            result
        };
        let (result,delivered)=tokio::time::timeout(std::time::Duration::from_secs(5),async {tokio::join!(admit,gate)}).await.unwrap();
        all_delivered.extend(delivered);
        if with_answer && phase==0 {
            assert!(!control.is_finished(),"a refused closure must retain its control owner");
            assert!(result.unwrap_err().contains("fixture transport closed")); assert_eq!(all_delivered.len(),2);
            let labels=codex_context_output_labels(&descriptor).unwrap(); let snapshots=codex_session_snapshot_labels(&descriptor).unwrap();
            assert_eq!(native_result_admitted_prefix(&ctx,&run,source_seq,&emissions,&snapshots,&labels).unwrap().len(),2);
            let mut shuffled=emissions.iter().map(|e|ProviderEmission{artifact:e.artifact.clone(),route:e.route.clone()}).collect::<Vec<_>>(); shuffled.swap(1,2);
            assert!(native_result_admitted_prefix(&ctx,&run,source_seq,&shuffled,&snapshots,&labels).unwrap_err().contains("contiguous prefix"));
            let mut changed:serde_json::Value=serde_json::from_slice(&emissions[1].artifact.read_payload_bytes().unwrap()).unwrap(); changed["text"]=json!("changed answer");
            shuffled.swap(1,2); shuffled[1].artifact=artifact(&root.join("conflict"),"interface.artifact.output",changed);
            assert!(native_result_admitted_prefix(&ctx,&run,source_seq,&shuffled,&snapshots,&labels).unwrap_err().contains("semantic payload"));
            drop(handoff); arbiter=InterruptedEffectArbiter::new(ctx.clone());
            handoff=ProviderHandoffSession::new(&ctx).unwrap().from_invocation_when_ready(&ctx,&descriptor,&run,&envelope,source_seq,&initial).await.unwrap();
        } else { assert_eq!(result.unwrap(),InterruptedResultAdmission::Admitted(count)); assert!(control.is_finished(),"an admitted closure must release the next operator input"); }
        }
        assert_eq!(all_delivered.len(),count); assert_eq!(all_delivered[count-1].0,TURN_INTERRUPTED_RESULT_LABEL); assert_ne!(all_delivered[count-1].1,source_closure);
        let current=RunRecord::from_bytes(&std::fs::read(&plan.run_write.path).unwrap()).unwrap();
        assert_eq!(current.interface_state[&descriptor.id]["context_generation"],json!(settled_generation));
        assert_eq!(Box::pin(handoff.recover_source()).await.unwrap(),ProviderSourceRecovery::Completed);
        if with_stop { assert!(arbiter.reconciled(&run).await.unwrap()); } else { assert!(!interrupted_effect_journal_path(&ctx,&run).exists()); }
        assert_eq!(recover_sealed_interrupted_result(&ctx,&descriptor,&turn,&original).unwrap().unwrap().1,source_closure);
        if let Some(delayed)=delayed_stop {
            for index in 0..2 {
                let (input,seq)=if index==0 {delayed.clone()} else {
                    let bytes=std::fs::read(&plan.run_write.path).unwrap();let record=RunRecord::from_bytes(&bytes).unwrap();
                    let cp=record.checkpoints.iter().find(|cp|record.active_checkpoints.contains(&cp.id)
                        && cp.outbound_interfaces().any(|id|id==&descriptor.id)
                        && cp.interface_binding.as_ref().unwrap().admits("operator.turn.control")).unwrap();
                    let file=artifact(&root.join("idle-stop"),"operator.turn.control",json!({"action":"stop","control_id":"idle-1"}));
                    let row=edge(&ctx,&bytes,cp,&file,MovementKind::InterfaceOutbound,descriptor.id.clone());
                    let input=source_envelope(&ctx,root,&row,file); (input,append(&ctx,row))
                };
                let id=format!("idle-{index}");let broker=CodexTurnControlBroker::new(None);broker.finish();
                let gates=ControlSourceGates::default();let session=ProviderHandoffSession::new(&ctx).unwrap();
                let admit=handle_control_source(&ctx,&descriptor,&session,&arbiter,&run,ControlSource {
                    envelope:&input,source_movement_seq:seq,broker:&broker,control_gates:&gates,
                    control:decode_admitted_turn_control(&ctx,&descriptor,&input).unwrap().unwrap(),
                    terminal_control_fenced:true,independent_reader:false,
                });
                let gate=pump(&ctx,root,&plan.run_write.path,&authoring_id,&listener,&mut receiver,&input.artifact,0,1,true,0,true);
                let (handled,delivered)=tokio::time::timeout(std::time::Duration::from_secs(5),async{tokio::join!(admit,gate)}).await.unwrap();
                assert_eq!(handled.unwrap(),ControlSourceDisposition::Handled);assert_eq!(delivered.len(),1);
                let saved=load_control_journal(&ctx,seq,&run,&id,"stop").unwrap().unwrap();
                let TurnControlJournalPhase::Delivered { result:saved }=saved.phase else {panic!("delivered control")};
                assert!(saved.acknowledged);
                let continuity=serde_json::to_value(saved.continuity.as_ref().unwrap()).unwrap();
                assert_eq!(continuity["context_generation"],json!(settled_generation+index+1));
                assert!(continuity["pending"].is_null()&&continuity["interrupted_pending"].is_null());
                persist_control_journal(&ctx,seq,&run,&id,"stop",TurnControlJournalPhase::Result {result:saved}).unwrap();
                let ledger=std::fs::read(&ctx.movements_path).unwrap();let bytes=std::fs::read(&plan.run_write.path).unwrap();
                let cold=ProviderHandoffSession::new(&ctx).unwrap();
                assert_eq!(handle_control_source(&ctx,&descriptor,&cold,&arbiter,&run,ControlSource {
                    envelope:&input,source_movement_seq:seq,broker:&broker,control_gates:&gates,
                    control:decode_admitted_turn_control(&ctx,&descriptor,&input).unwrap().unwrap(),
                    terminal_control_fenced:true,independent_reader:false,
                }).await.unwrap(),ControlSourceDisposition::Handled);
                assert_eq!(std::fs::read(&ctx.movements_path).unwrap(),ledger);
                assert_eq!(std::fs::read(&plan.run_write.path).unwrap(),bytes);
            }
            let bytes=std::fs::read(&plan.run_write.path).unwrap();let current=RunRecord::from_bytes(&bytes).unwrap();
            let cp=current.checkpoints.iter().find(|cp|current.active_checkpoints.contains(&cp.id)
                && cp.outbound_interfaces().any(|id|id==&descriptor.id)
                && cp.interface_binding.as_ref().unwrap().admits("operator.message")).unwrap();
            let file=artifact(&root.join("after-idle-stop"),"operator.message",json!({"text":"next message"}));
            let row=edge(&ctx,&bytes,cp,&file,MovementKind::InterfaceOutbound,descriptor.id.clone());
            let input=source_envelope(&ctx,root,&row,file);let seq=append(&ctx,row);
            let next=ProviderHandoffSession::new(&ctx).unwrap().from_invocation(&ctx,&descriptor,&run,&input,seq,&bytes).unwrap();
            let recovered=recover_provider_turn_continuity(&descriptor,&input,next.invocation_session_snapshot().as_ref(),&run,&bytes).unwrap().unwrap();
            assert_eq!(serde_json::to_value(recovered).unwrap()["context_generation"],current.interface_state[&descriptor.id]["context_generation"]);
            assert!(next.input_changes_context().await);
        }
        if native_interrupted {
            let history=root.join("native-history.jsonl");
            std::fs::write(&history,concat!("{\"type\":\"session_meta\",\"payload\":{\"id\":\"thread-a\"}}\n",
                "{\"type\":\"turn_context\",\"payload\":{\"turn_id\":\"turn-a\"}}\n",
                "{\"type\":\"response_item\",\"payload\":{\"type\":\"custom_tool_call\",\"call_id\":\"outer\",\"name\":\"exec\",\"input\":\"text(await tools.coder_exec({}));\"}}\n")).unwrap();
            let before=std::fs::read(&plan.run_write.path).unwrap();
            let cp=current.checkpoints.iter().find(|cp|current.active_checkpoints.contains(&cp.id)
                && cp.outbound_interfaces().any(|id|id==&descriptor.id)
                && cp.interface_binding.as_ref().unwrap().admits("operator.message")).unwrap();
            let input=artifact(&root.join("next-input"),"operator.message",json!({"text":"continue"}));
            let source=edge(&ctx,&before,cp,&input,MovementKind::InterfaceOutbound,descriptor.id.clone());
            let next_input=source_envelope(&ctx,root,&source,input); let next_seq=append(&ctx,source);
            let next_handoff=ProviderHandoffSession::new(&ctx).unwrap().from_invocation(&ctx,&descriptor,&run,&next_input,next_seq,&before).unwrap();
            let entries=next_handoff.current_session_context_artifacts().await;
            let receipt=crate::handler::native_history_receipt_for_test(&descriptor,&output_root,&history,
                serde_json::from_value(closed["codex_continuity"].clone()).unwrap(),&entries,&next_input.artifact,next_seq).await;
            let admit=Box::pin(next_handoff.hand_batch_with_outgoing_bindings(vec![receipt],false,&[]));
            let gate=pump(&ctx,root,&plan.run_write.path,&authoring_id,&listener,&mut receiver,&next_input.artifact,0,1,true,1,false);
            let (result,delivered)=tokio::time::timeout(std::time::Duration::from_secs(5),async { tokio::join!(admit,gate) }).await.unwrap();
            assert_eq!(delivered.len(),1); assert_eq!(delivered[0].0,"interface.context.updated");
            assert!(matches!(result.unwrap().as_slice(),[octopus_run_authoring::ProviderHandoffOutcome::Admitted{..}]));
            drop(next_handoff);
            let restored=ProviderHandoffSession::new(&ctx).unwrap().from_invocation(&ctx,&descriptor,&run,&next_input,next_seq,&before).unwrap();
            let (_,outcomes)=tokio::time::timeout(std::time::Duration::from_secs(5),restored.recover_source_with_outcomes()).await.unwrap().unwrap();
            assert_eq!(outcomes.len(),1);
            let entries=restored.current_session_context_artifacts().await;
            let receipt=entries.iter().find(|entry|entry.source_artifact==delivered[0].1).unwrap();
            assert_eq!(receipt.payload["codex_continuity"]["thread_id"],"history-replacement");
            assert_eq!(receipt.payload["codex_continuity"]["context_generation"],json!(settled_generation+1));
        }
    }
    if let Some(phase) = selected_phase {
        case(false,false,false,Some(phase),false).await;
        return;
    }
    for (with_stop, with_answer, native_interrupted, settlement_phase) in [
        (false,false,false,Some(22)),(false,true,false,None),(true,true,false,None),(false,false,false,None),(true,false,false,None),
        (false,false,true,None),(true,false,true,None),(false,false,true,Some(0)),(false,false,true,Some(2)),(false,false,true,Some(3)),(false,false,true,Some(4)),(false,false,true,Some(5)),(false,false,true,Some(6)),(false,false,true,Some(7)),(false,false,true,Some(8)),(false,false,true,Some(9)),(false,false,true,Some(10)),(false,false,true,Some(11)),(false,false,true,Some(12)),(false,false,true,Some(13)),
        (false,false,true,Some(14)),(false,false,true,Some(15)),(false,false,true,Some(16)),(false,false,true,Some(17)),(false,false,true,Some(18)),(false,false,true,Some(19)),
        (false,false,true,Some(20)),(false,false,true,Some(21)),
    ] {
        tokio::spawn(case(with_stop,with_answer,native_interrupted,settlement_phase,false)).await.unwrap();
    }
    tokio::spawn(case(true,false,false,None,true)).await.unwrap();
}
