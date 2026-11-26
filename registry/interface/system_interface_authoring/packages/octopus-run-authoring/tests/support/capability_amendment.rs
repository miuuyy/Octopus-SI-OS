use super::*;

fn fixture() -> (RunAuthoringConfig, Value) {
    let binding = |id: &str| json!(AuthoredInterfaceBinding::all(InterfaceId::new(id)));
    let row = |name: &str, role: &str, source: &str, target: &str, tools: Option<Vec<&str>>| json!({
        "name":name,"role":role,"interface_binding":binding(source),"outbound_interface_bindings":[binding(target)],
        "include_context":role == "stable","required_verified_artifacts":tools.map(|tools| vec![json!({
            "mark":"tool.call.allowed","artifact_label":"tool.call","verifier":"tool_call_allowed","params":{"tools":tools}
        })]).unwrap_or_default(),
    });
    let config: RunAuthoringConfig = serde_json::from_value(json!({"run":"run","worker":"worker","provider_interface":"provider",
        "authoring_source_interface":"writer","file_write_tool":"write","file_write_label":"tool.call","file_write_result_label":"write.result","checkpoints":[
            row("input","stable","room","provider",None), row("output","provider_tool_output","provider","room",Some(vec!["read","write"])),
            row("client","provider_artifact_output","provider","client",None),
            row("writer_call","authoring_call","writer","writer_room",Some(vec!["write"])),
            row("writer_result","authoring_result","writer_room","provider",None)
        ]})).unwrap();
    let topology = BoundRunAuthoringTopology { schema_version:2, run:config.run.clone(),worker:config.worker.clone(),
        provider_interface:config.provider_interface.clone(), authoring_source_interface:config.authoring_source_interface.clone(),
        payload_artifact_bindings:Vec::new(), writer_run:None, checkpoints:config.checkpoints.clone() };
    let roles = vec![json!({"id":"role"})];
    let mut rows = config.checkpoints.iter().map(|cp| materialize_checkpoint(cp, &cp.name, &config.run, &roles)).collect::<Vec<_>>();
    rows.push(json!({"id":"accepted_wait","run":"run","trace":"run","epoch":7,"interface_binding":binding("peer"),
        "outbound_interface_bindings":[binding("waiting_owner")],"artifact_bindings":[{"id":"role"}],
        "required_verified_artifacts":[{"mark":"task.done","artifact_label":"task.result","verifier":"exact_result","params":{"request":"accepted-request"}}]}));
    let value = json!({"id":"run","ee":"ee","worker":"worker","trace":"run","status":"running","checkpoints":rows,
        "active_checkpoints":["input","output","client","writer_call","writer_result","accepted_wait"],
        "interface_state":{"provider":{"context_generation":0,"authoring_generation":0,
            "frontier_checkpoint_ids":["output","client","writer_call","writer_result"],"run_authoring_binding":topology},"peer":{}}});
    validate_checkpoint_topology(&record(&value).unwrap(), &config.provider_interface, &config.checkpoints, None).unwrap();
    (config, value)
}

fn proposal(config: &RunAuthoringConfig, before: &Value) -> ProviderCapabilityAmendment {
    let mut after = before.clone();
    let templates = after["interface_state"]["provider"][RUN_AUTHORING_BINDING]["checkpoints"].as_array_mut().unwrap();
    let mut input = templates.iter().find(|cp| cp["name"] == "input").unwrap().clone();
    input["name"] = json!("new_input"); input["interface_binding"]["interface"] = json!("new_room");
    let mut output = templates.iter().find(|cp| cp["name"] == "output").unwrap().clone();
    output["name"] = json!("new_output"); output["outbound_interface_bindings"][0]["interface"] = json!("new_room");
    output["required_verified_artifacts"][0]["params"]["tools"] = json!(["new_tool"]);
    templates.extend([input.clone(), output.clone()]);
    for value in [input, output] {
        let cp: RunAuthoringCheckpoint = serde_json::from_value(value).unwrap();
        let row = materialize_checkpoint(&cp, &cp.name, &config.run, &[json!({"id":"role"}),json!({"id":"new_context"})]);
        after["checkpoints"].as_array_mut().unwrap().push(row);
        after["active_checkpoints"].as_array_mut().unwrap().push(json!(cp.name));
        if cp.role == RunAuthoringCheckpointRole::ProviderToolOutput {
            after["interface_state"]["provider"][FRONTIER_CHECKPOINT_IDS].as_array_mut().unwrap().push(json!(cp.name));
        }
    }
    after["checkpoints"][0]["artifact_bindings"].as_array_mut().unwrap().push(json!({"id":"new_context"}));
    ProviderCapabilityAmendment { amendment_key:"attach".into(),provider_interface:config.provider_interface.clone(),before:before.clone(),after,
        authoring_profile:RunAuthoringProfile {file_write_tool:config.file_write_tool.clone(),file_write_label:config.file_write_label.clone(),
            file_write_result_label:config.file_write_result_label.clone()},
        bridge:ProviderCapabilityBridge { tool:"read".into(),correlation_id:"bridge-call".into(),
            arguments_hash:ContentHash::of_bytes(b"{}").to_hex(),return_interface:"room".into() } }
}

fn ordinary_effect(config: &RunAuthoringConfig, before: &Value, var_dir: &Path, proposal: Option<&ProviderCapabilityAmendment>, terminal: bool) -> AuthoredRun {
    ordinary_effect_padded(config, before, var_dir, proposal, terminal, 0)
}

fn ordinary_effect_padded(config: &RunAuthoringConfig, before: &Value, var_dir: &Path, proposal: Option<&ProviderCapabilityAmendment>, terminal: bool, padding: usize) -> AuthoredRun {
    let old = record(before).unwrap();
    let generation=before["interface_state"]["provider"]["authoring_generation"].as_u64().unwrap()+1;
    let mut payload=json!({"tool":"read","arguments":{},"correlation_id":"bridge-call"});
    if let Some(proposal)=proposal { payload["provider_continuity"]=json!({"native_operation":{"plan":{"amendment":proposal}}}); }
    if padding > 0 { payload["provider_continuity"]["padding"] = json!("p".repeat(padding)); }
    let artifact = InterfaceArtifactFile::write(var_dir,"tool.call",&serde_json::to_vec(&payload).unwrap()).unwrap();
    let identity = artifact_identity(&artifact,var_dir).unwrap();
    let effect = pending_output_checkpoint(&config.checkpoints[1], &format!("accepted_bridge_{generation}"), "tool.call", &["role".into()], &["role".into()]);
    let mut context=current_context_artifacts(&old,config).unwrap();append_artifact(&mut context,"dynamic_call".into());
    let pending = PendingHandoff {source_movement_seq:19,terminal,context_publication:Some(context.clone()),deferred_publication:false,source_without_context:false,settlement_of:None,continuation_of:None,dispatch_fenced:true,withdrawal:None,replacement:None,standing:Vec::new(),rebase:Vec::new(),
        items:vec![PendingHandoffItem {artifact_id:identity.artifact.id.clone(),artifact_content_hash:identity.artifact.content.to_hex(),
            artifact_label:identity.label,manifest_relative_path:identity.manifest_relative_path,checkpoint:effect.clone()}]};
    let mut value = before.clone();
    value["status"] = json!(if terminal {"idle"} else {"running"});
    let frontier = raw_frontier_checkpoint_ids(&old.interface_state[&config.provider_interface]).unwrap();
    value["checkpoints"].as_array_mut().unwrap().retain(|cp| !frontier.contains(cp["id"].as_str().unwrap()));
    value["active_checkpoints"].as_array_mut().unwrap().retain(|id| !frontier.contains(id.as_str().unwrap()));
    let mut rows = vec![materialize_pending_checkpoint(&effect,&config.run)];
    for cp in config.checkpoints.iter().filter(|cp|matches!(cp.role,RunAuthoringCheckpointRole::AuthoringCall | RunAuthoringCheckpointRole::AuthoringResult)) {
        rows.push(materialize_checkpoint(cp,&format!("new_{generation}_{}",cp.name),&config.run,&[json!({"id":"role"})]));
    }
    let ids = rows.iter().map(|cp|cp["id"].clone()).collect::<Vec<_>>();
    value["checkpoints"].as_array_mut().unwrap().extend(rows);
    value["active_checkpoints"].as_array_mut().unwrap().extend(ids.clone());
    replace_context_checkpoints(value.as_object_mut().unwrap(),config,&config.run,&format!("ordinary_{generation}"),&context.iter().map(|id|json!({"id":id})).collect::<Vec<_>>()).unwrap();
    let state = value["interface_state"]["provider"].as_object_mut().unwrap();
    state.insert(FRONTIER_CHECKPOINT_IDS.into(),json!(ids)); state.insert("authoring_generation".into(),json!(generation));
    state.insert("context_generation".into(),json!(before["interface_state"]["provider"]["context_generation"].as_u64().unwrap()+1));
    state.insert("pending_handoff".into(),json!(pending));state.insert("last_authored_artifact".into(),json!(identity.artifact.id));
    let content = serde_json::to_string(&value).unwrap();
    AuthoredRun {json_format:Some(JsonFormat::Compact),patch:json_patch::diff(before,&value),before_hash:ContentHash::of_bytes(&serde_json::to_vec(before).unwrap()).to_hex(),
        after_hash:ContentHash::of_bytes(content.as_bytes()).to_hex(),content,correlation_id:"writer:1".into(),
        authoring_result_checkpoint:format!("new_{generation}_writer_result").into(),authoring_result_interface:"writer_room".into(),pending_handoff:pending}
}

fn transition(before: &Value, authored: &AuthoredRun) -> CheckedRunTransition {
    let movement = |seq| Movement {seq:octopus_ledger::Seq(seq),lamport:octopus_ledger::Lamport(seq),recorded_at_unix_ms:seq,
        trace:octopus_ledger::TraceId("run".into()),kind:MovementKind::InterfaceOutbound,from:Endpoint::Interface("writer".into()),to:Endpoint::Interface("writer_room".into()),artifact:None,invocation_metadata:None};
    CheckedRunTransition { before:serde_json::to_vec(before).unwrap(),after:authored.content.as_bytes().to_vec(),call:movement(20),result:movement(21),
        writer:CheckedRunWriter {owner:"provider".into(),source:"writer".into(),door:"writer_room".into(),tool:"write".into(),call_label:"tool.call".into(),result_label:"write.result".into()},
        capability_payload:None, foreign:false }
}

#[test]
fn capability_attachment_preserves_real_pending_and_waits_and_validates_checked_history() {
    for terminal in [false,true] {
    let (config, before) = fixture();
    let temp=tempfile::tempdir().unwrap();
    let amendment = proposal(&config, &before);
    validate_plan(&amendment).unwrap();
    let mut authored = ordinary_effect(&config,&before,temp.path(),None,terminal);
    validate_checked_provider_transition(&transition(&before,&authored)).unwrap();
    let fresh=record(&serde_json::from_str(&authored.content).unwrap()).unwrap();
    let rebased=rebase_plan(&fresh,amendment.clone()).unwrap();
    assert_eq!(rebased.before,json!(fresh));
    assert_eq!(rebased.after["interface_state"]["provider"]["authoring_generation"],1);
    assert_eq!(rebased.after["interface_state"]["provider"]["pending_handoff"],json!(authored.pending_handoff));
    assert!(context(&record(&rebased.after).unwrap(),&topology(&record(&rebased.after).unwrap(),&config.provider_interface).unwrap()).unwrap().contains(&"dynamic_call".into()));
    let mut invalid_observed=amendment.clone();invalid_observed.after["interface_state"]["provider"]["authoring_generation"]=json!(20);
    assert!(rebase_plan(&fresh,invalid_observed).is_err());
    let accepted = authored.pending_handoff.clone();
    apply_to_authored_run(&serde_json::to_vec(&before).unwrap(), &mut authored, &amendment, &config,temp.path()).unwrap();
    assert_eq!(authored.pending_handoff.items,accepted.items);
    assert_eq!(authored.pending_handoff.terminal,terminal);
    assert_eq!(authored.pending_handoff.source_movement_seq,accepted.source_movement_seq);
    let installed = record(&serde_json::from_str(&authored.content).unwrap()).unwrap();
    assert_eq!(installed.checkpoint(&"accepted_wait".into()),record(&before).unwrap().checkpoint(&"accepted_wait".into()));
    assert!(current_context_artifacts(&installed,&config).unwrap().contains(&"new_context".into()));
    assert!(installed.active_checkpoints.iter().any(|id| installed.checkpoint(id).unwrap().inbound_interface() == Some(&"new_room".into())));
    validate_installed(&installed,&amendment).unwrap();
    let mut removed = transition(&before, &authored);
    removed.before = removed.after.clone();
    let mut value: Value = serde_json::from_slice(&removed.after).unwrap();
    value["interface_state"]["provider"].as_object_mut().unwrap().remove(AMENDMENT);
    removed.after = serde_json::to_vec(&value).unwrap();
    assert!(validate_checked_provider_transition(&removed).unwrap_err().contains("removed its capability transition evidence"));
    validate_checked_provider_transition(&transition(&before,&authored)).unwrap();
    let initial_topology = topology(&record(&before).unwrap(), &config.provider_interface).unwrap();
    let installed_topology = topology(&installed, &config.provider_interface).unwrap();
    assert!(checked_topology_advance(&config.provider_interface, &initial_topology, &installed_topology, &[transition(&before,&authored)]).unwrap());
    assert!(!checked_topology_advance(&config.provider_interface, &initial_topology, &installed_topology, &[]).unwrap());
    let mut unexplained = installed_topology.clone();
    unexplained.checkpoints.pop();
    assert!(!checked_topology_advance(&config.provider_interface, &initial_topology, &unexplained, &[transition(&before,&authored)]).unwrap());
    let mut context=vec!["role".into(),"dynamic_call".into()];
    let mut static_context=vec!["role".into()];
    adopt_amendment_context_bindings(&installed,&config.provider_interface,&[],&mut context,&mut static_context).unwrap();
    assert_eq!(static_context,vec![ArtifactId::from("role"),"new_context".into()]);
    assert_eq!(context,vec![ArtifactId::from("role"),"dynamic_call".into(),"new_context".into()]);
    let mut forged = transition(&before,&authored);
    let mut value:Value=serde_json::from_slice(&forged.after).unwrap();
    value["interface_state"]["provider"]["authoring_generation"]=json!(999);
    forged.after=serde_json::to_vec(&value).unwrap();
    assert!(validate_checked_provider_transition(&forged).is_err());
    assert!(checked_topology_advance(&config.provider_interface, &initial_topology, &installed_topology, &[forged]).is_err());
    let emission=ProviderHandoffEmission {artifact:InterfaceArtifactFile::write(temp.path(),"tool.call",br#"{"tool":"read","arguments":{},"correlation_id":"bridge-call"}"#).unwrap(),route:ProviderHandoffRoute::Tool{tool:"read".into()}};
    validate_emission(&amendment,&emission).unwrap();
    let mut wrong=amendment.clone(); wrong.bridge.correlation_id="other-call".into();
    assert!(validate_emission(&wrong,&emission).is_err());
    for mutation in 0..5 {
        let mut forged=transition(&before,&authored);
        let mut value:Value=serde_json::from_slice(&forged.after).unwrap();
        match mutation {
            0=>{
                let mut cp=before["checkpoints"][5].clone();cp["id"]=json!("smuggled");cp["outbound_interface_bindings"][0]["interface"]=json!("another_target");
                value["interface_state"]["provider"][AMENDMENT]["delta"]["checkpoints"].as_array_mut().unwrap().push(cp.clone());
                value["checkpoints"].as_array_mut().unwrap().push(cp);
                value["active_checkpoints"].as_array_mut().unwrap().push(json!("smuggled"));
            }
            1=>value["interface_state"]["provider"][AMENDMENT]["bridge"]["correlation_id"]=json!("forged"),
            2=>value["interface_state"]["provider"][AMENDMENT]["bridge"]["arguments_hash"]=json!(ContentHash::of_bytes(b"different").to_hex()),
            3=>{
                value["interface_state"]["provider"][AMENDMENT]["delta"]["interface_state"]["peer"]=json!({"forged":true});
                value["interface_state"]["peer"]=json!({"forged":true});
            }
            _=>{
                for pointer in [format!("/interface_state/provider/{AMENDMENT}/delta/after/checkpoints/5/include_context"),
                    format!("/interface_state/provider/{RUN_AUTHORING_BINDING}/checkpoints/5/include_context")] {
                    *value.pointer_mut(&pointer).unwrap()=json!(false);
                }
            }
        }
        forged.after=serde_json::to_vec(&value).unwrap();
        assert!(validate_checked_provider_transition(&forged).is_err(),"forged proof {mutation}");
    }
    let mut unpinned=transition(&before,&authored);
    let mut value:Value=serde_json::from_slice(&unpinned.after).unwrap();
    let mut proof:AppliedAmendment=serde_json::from_value(value["interface_state"]["provider"][AMENDMENT].clone()).unwrap();
    let mut manifest:Value=serde_json::from_str(&proof.effect.manifest).unwrap();
    manifest.as_object_mut().unwrap().remove("payload_content_hash");
    proof.effect.manifest=serde_json::to_string(&manifest).unwrap();
    let mut payload:Value=serde_json::from_str(match &proof.effect.payload { EffectPayload::Inline(value) => value, _ => panic!("expected inline fixture") }).unwrap();
    payload["arguments"]=json!({"forged":true});
    proof.effect.payload=EffectPayload::Inline(serde_json::to_string(&payload).unwrap());
    proof.bridge.arguments_hash=ContentHash::of_bytes(&serde_json::to_vec(&payload["arguments"]).unwrap()).to_hex();
    let hash=ContentHash::of_bytes(proof.effect.manifest.as_bytes()).to_hex();
    let id=format!("tool.call:{hash}");
    let mut ordinary=before.clone();
    json_patch::patch(&mut ordinary,&proof.ordinary_patch).unwrap();
    for candidate in [&mut value,&mut ordinary] {
        let state=&mut candidate["interface_state"]["provider"];
        state["pending_handoff"]["items"][0]["artifact_content_hash"]=json!(hash);
        state["pending_handoff"]["items"][0]["artifact_id"]=json!(id);
        state["last_authored_artifact"]=json!(id);
    }
    let mut base=unpinned.clone();base.after=serde_json::to_vec(&ordinary).unwrap();
    validate_checked_provider_transition(&base).unwrap();
    proof.ordinary_patch=json_patch::diff(&before,&ordinary);
    value["interface_state"]["provider"][AMENDMENT]=json!(proof);
    unpinned.after=serde_json::to_vec(&value).unwrap();
    assert!(validate_checked_provider_transition(&unpinned).unwrap_err().contains("requires a canonical payload_content_hash"));
    let mut current=serde_json::from_str::<Value>(&authored.content).unwrap();
    let current_config=topology(&record(&current).unwrap(),&config.provider_interface).unwrap().config(&amendment.authoring_profile);
    let mut sizes=Vec::new();
    for index in 0..8 {
        let proposal=prepare_plan(ProviderCapabilityAmendment {before:current.clone(),after:current.clone(),amendment_key:format!("repeat-{index}"),..amendment.clone()}).unwrap();
        assert!(proposal.before["interface_state"]["provider"].get(AMENDMENT).is_none());
        assert!(proposal.after["interface_state"]["provider"].get(AMENDMENT).is_none());
        let mut next=ordinary_effect(&current_config,&current,temp.path(),Some(&proposal),terminal);
        apply_to_authored_run(&serde_json::to_vec(&current).unwrap(),&mut next,&proposal,&current_config,temp.path()).unwrap();
        validate_checked_provider_transition(&transition(&current,&next)).unwrap();
        sizes.push(next.content.len());current=serde_json::from_str(&next.content).unwrap();
    }
    assert!(sizes.iter().max().unwrap() < &(sizes[0]*2),"recursive amendment sizes: {sizes:?}");
    eprintln!("repeated checked amendment bytes: {sizes:?}");
    }
}

#[test]
fn capability_plan_rejects_bridge_narrowing_and_private_or_accepted_contract_changes() {
    let (config,before)=fixture(); let amendment=proposal(&config,&before);
    for mutation in 0..5 {
        let mut invalid=amendment.clone();
        match mutation {
            0=>invalid.after["interface_state"]["provider"][RUN_AUTHORING_BINDING]["checkpoints"][1]["required_verified_artifacts"][0]["params"]["tools"]=json!(["write"]),
            1=>invalid.after["interface_state"]["provider"]["context_generation"]=json!(800),
            2=>invalid.after["checkpoints"][5]["required_verified_artifacts"]=json!([]),
            3=>invalid.after["interface_state"]["provider"][RUN_AUTHORING_BINDING]["authoring_source_interface"]=json!("new_writer"),
            _=>invalid.after["interface_state"]["provider"][RUN_AUTHORING_BINDING]["checkpoints"][5]["include_context"]=json!(false),
        }
        assert!(validate_plan(&invalid).is_err(),"mutation {mutation}");
    }
    let (mut config, mut before)=fixture();
    config.checkpoints[0].interface_binding=AuthoredInterfaceBinding::all("client_input".into());
    for tool in ["read","write"] {
        config.checkpoints[1].result_receiving.insert(tool.into(),serde_json::from_value(json!({
            "mark":"tool.result.corresponds","artifact_label":format!("{tool}.result"),"verifier":"tool_result_correspondence","params":{}
        })).unwrap());
    }
    before["checkpoints"][0]["interface_binding"]=json!(config.checkpoints[0].interface_binding);
    before["interface_state"]["provider"][RUN_AUTHORING_BINDING]["checkpoints"]=json!(config.checkpoints);
    let mut amendment=proposal(&config,&before);
    let templates=amendment.after["interface_state"]["provider"][RUN_AUTHORING_BINDING]["checkpoints"].as_array_mut().unwrap();
    templates[5]["interface_binding"]["interface"]=json!("new_context_input");
    templates[6]["result_receiving"]=json!({"new_tool":{"mark":"tool.result.corresponds","artifact_label":"new_tool.result","verifier":"tool_result_correspondence","params":{}}});
    amendment.after["checkpoints"][6]["interface_binding"]["interface"]=json!("new_context_input");
    validate_plan(&amendment).unwrap();
    let temp=tempfile::tempdir().unwrap();
    let mut authored=ordinary_effect(&config,&before,temp.path(),None,false);
    let mut ordinary:Value=serde_json::from_str(&authored.content).unwrap();
    let item=&authored.pending_handoff.items[0];
    let mut result=config.checkpoints[0].clone();
    result.role=RunAuthoringCheckpointRole::ResultInput;
    result.static_artifact_bindings=vec![item.artifact_id.clone()];
    result.interface_binding=AuthoredInterfaceBinding { interface:"room".into(),config:InterfaceBindingConfig {
        artifact_policy:BindingArtifactPolicy::Allow {names:vec!["read.result".into()],namespaces:Vec::new()}
    }};
    result.outbound_interface_bindings=vec![AuthoredInterfaceBinding {interface:"provider".into(),config:result.interface_binding.config.clone()}];
    let mut required=config.checkpoints[1].result_receiving["read"].clone();
    required.params=json!({"call_artifact":ArtifactRef {id:item.artifact_id.clone(),content:ContentHash::from_hex(&item.artifact_content_hash).unwrap()},
        "tool":"read","correlation_id":"bridge-call","params":{}});
    result.required_verified_artifacts=vec![required];
    result.name=derived_checkpoint_name(result.role,&result.interface_binding,&result.outbound_interface_bindings,true,&result.required_verified_artifacts).unwrap();
    let mut captured=provider_context_bindings(&record(&ordinary).unwrap(),&config.provider_interface).unwrap().into_iter().map(|binding|json!(binding)).collect::<Vec<_>>();
    captured.push(json!(ArtifactBinding {id:item.artifact_id.clone(),access:Default::default()}));
    let receipt=materialize_checkpoint(&result,"bridge_result",&config.run,&captured);
    ordinary["checkpoints"].as_array_mut().unwrap().push(receipt.clone());
    ordinary["active_checkpoints"].as_array_mut().unwrap().push(json!("bridge_result"));
    ordinary["interface_state"]["provider"][RUN_AUTHORING_BINDING]["checkpoints"].as_array_mut().unwrap().push(json!(result));
    authored.patch=json_patch::diff(&before,&ordinary);authored.content=ordinary.to_string();
    authored.after_hash=ContentHash::of_bytes(authored.content.as_bytes()).to_hex();
    validate_checked_provider_transition(&transition(&before,&authored)).unwrap();
    let rebased=rebase_plan(&record(&ordinary).unwrap(),amendment.clone()).unwrap();
    assert_eq!(rebased.after["checkpoints"].as_array().unwrap().iter().find(|cp|cp["id"]=="bridge_result"),Some(&receipt));
    let mut rewritten=rebased.clone();
    rewritten.after["checkpoints"].as_array_mut().unwrap().iter_mut().find(|cp|cp["id"]=="bridge_result").unwrap()["artifact_bindings"][0]["id"]=json!("unadmitted_context");
    assert!(validate_plan(&rewritten).is_err());
    apply_to_authored_run(&serde_json::to_vec(&before).unwrap(),&mut authored,&amendment,&config,temp.path()).unwrap();
    let installed=record(&serde_json::from_str(&authored.content).unwrap()).unwrap();
    assert_eq!(json!(installed.checkpoint(&"bridge_result".into()).unwrap()),receipt);
    let future=topology(&installed,&config.provider_interface).unwrap().checkpoints.into_iter().find(|cp|cp.name=="new_output").unwrap();
    assert_eq!(installed.active_checkpoints.iter().filter_map(|id|installed.checkpoint(id))
        .filter(|cp|checkpoint_record_matches_template(cp,&future,&config.run)).count(),1);
    assert!(current_context_artifacts(&installed,&config).unwrap().contains(&"new_context".into()));
    assert!(!captured.iter().any(|binding|binding["id"]=="new_context"));
    validate_installed(&installed,&amendment).unwrap();
    let mut removed = transition(&before, &authored);
    removed.before = removed.after.clone();
    let mut value: Value = serde_json::from_slice(&removed.after).unwrap();
    value["interface_state"]["provider"].as_object_mut().unwrap().remove(AMENDMENT);
    removed.after = serde_json::to_vec(&value).unwrap();
    assert!(validate_checked_provider_transition(&removed).unwrap_err().contains("removed its capability transition evidence"));
    validate_checked_provider_transition(&transition(&before,&authored)).unwrap();
    assert!(result_inputs::validate_transition(&record(&before).unwrap(),&installed,&config).is_err());
    let stepped=saved_ordinary(&serde_json::to_vec(&before).unwrap(),authored.content.as_bytes(),&config).unwrap().unwrap();
    result_inputs::validate_transition(&record(&before).unwrap(),&RunRecord::from_bytes(&stepped).unwrap(),&config).unwrap();
    result_inputs::saved_attempt_config(&installed,&RunRecord::from_bytes(&stepped).unwrap(),&config).unwrap();
    let mut advanced=before.clone();advanced["interface_state"]["peer"]=json!({"advanced":true});
    let retried=authoring_retry::rebase_authored_run(None, &serde_json::to_vec(&before).unwrap(),&serde_json::to_vec(&advanced).unwrap(),&authored,&topology(&installed,&config.provider_interface).unwrap().config(&amendment.authoring_profile),Some(JsonFormat::Compact)).unwrap();
    validate_checked_provider_transition(&transition(&advanced,&retried)).unwrap();
    let mut widened=amendment.clone();
    widened.after["interface_state"]["provider"][RUN_AUTHORING_BINDING]["checkpoints"][1]["result_receiving"]["read"]["params"]=json!({"changed":true});
    assert!(validate_plan(&widened).is_err());
    let mut ambiguous=before.clone();
    ambiguous["interface_state"]["provider"][RUN_AUTHORING_BINDING]["checkpoints"][0]["interface_binding"]["interface"]=json!("room");
    assert!(validate_tool_continuation(&record(&ambiguous).unwrap(),&config.provider_interface,"read",&"room".into()).is_err());
}

#[test]
fn saved_attempt_reads_through_the_provider_step_its_checked_reader_admits() {
    let (config,before)=fixture(); let temp=tempfile::tempdir().unwrap(); let mut authored=ordinary_effect(&config,&before,temp.path(),None,true);
    let mut value:Value=serde_json::from_str(&authored.content).unwrap(); let policy=json!({"artifact_policy":{"mode":"allow","names":["peer.message"],"namespaces":[]}});
    let outgoing:RunAuthoringCheckpoint=serde_json::from_value(json!({"name":"outgoing_peer","role":"provider_interface_output","include_context":false,"required_verified_artifacts":[],
        "interface_binding":{"interface":"provider","config":policy},"outbound_interface_bindings":[{"interface":"peer_input","config":policy}]})).unwrap();
    value["checkpoints"].as_array_mut().unwrap().push(materialize_checkpoint(&outgoing,"outgoing_peer",&config.run,&[json!({"id":"role"})]));
    for pointer in ["/active_checkpoints".to_string(),format!("/interface_state/provider/{FRONTIER_CHECKPOINT_IDS}")] { value.pointer_mut(&pointer).unwrap().as_array_mut().unwrap().push(json!("outgoing_peer")); }
    value["interface_state"]["provider"][RUN_AUTHORING_BINDING]["checkpoints"].as_array_mut().unwrap().push(json!(outgoing));
    authored.patch=json_patch::diff(&before,&value);authored.content=value.to_string();authored.after_hash=ContentHash::of_bytes(authored.content.as_bytes()).to_hex();
    let saved=record(&value).unwrap();
    assert_eq!(result_inputs::saved_attempt_config(&saved,&saved,&config).unwrap().checkpoints.last(),Some(&outgoing));
    let mut advanced=before.clone();advanced["interface_state"]["peer"]=json!({"advanced":true});
    let retried=authoring_retry::rebase_authored_run(None, &serde_json::to_vec(&before).unwrap(),&serde_json::to_vec(&advanced).unwrap(),&authored,&config,Some(JsonFormat::Compact)).unwrap();
    validate_checked_provider_transition(&transition(&advanced,&retried)).unwrap();
    let narrowed=RunAuthoringConfig {checkpoints:config.checkpoints[..2].to_vec(),..config.clone()};
    assert!(result_inputs::saved_attempt_config(&saved,&saved,&narrowed).unwrap_err().contains("beyond a provider step"));
}

struct ProofReader<'a>(&'a Path);
impl CheckedRunEvidenceReader for ProofReader<'_> {
    fn max_payload_bytes(&self) -> u64 { 16 * 1024 * 1024 }
    fn capability_payload_proof(&mut self, provider: &InterfaceId, reference: &EffectPayloadRef) -> Result<Vec<u8>, String> {
        if provider.as_str() != "provider" { return Err("wrong proof owner".into()); }
        read_payload_proof(self.0, reference, self.max_payload_bytes())
    }
    fn movement(&mut self, _: u64) -> Result<Option<Movement>, String> { Err("unused".into()) }
    fn artifact(&mut self, _: &ArtifactRef) -> Result<(InterfaceArtifactManifest, Vec<u8>), String> { Err("unused".into()) }
    fn snapshot(&mut self, _: &ContentHash) -> Result<Vec<u8>, String> { Err("unused".into()) }
}

#[test]
fn durable_capability_payload_transition_replay() {
    let (config, before) = fixture();
    let temp = tempfile::tempdir().unwrap();
    let amendment = proposal(&config, &before);
    let mut authored = ordinary_effect_padded(&config, &before, temp.path(), None, false, 256 * 1024);
    apply_to_authored_run(&serde_json::to_vec(&before).unwrap(), &mut authored, &amendment, &config, temp.path()).unwrap();
    let mut checked = transition(&before, &authored);
    assert!(validate_checked_provider_transition(&checked).unwrap_err().contains("durable payload proof"));
    let reference = transition_reference(&checked).unwrap().unwrap();
    let payload = read_payload_proof(temp.path(), &reference, 1024 * 1024).unwrap();
    assert!(read_payload_proof(temp.path(), &reference, 1024).is_err());
    capture_transition_payload(&mut ProofReader(temp.path()), &mut checked).unwrap();
    validate_checked_provider_transition(&checked).unwrap();
    saved_ordinary_in(&checked.before, &checked.after, &config, temp.path(), 1024 * 1024).unwrap().unwrap();
    let path = proof_directory(temp.path(), false).unwrap().join(&reference.content_hash);
    std::fs::remove_file(&path).unwrap();
    let mut cold = transition(&before, &authored);
    assert!(capture_transition_payload(&mut ProofReader(temp.path()), &mut cold).is_err());
    assert!(saved_ordinary_in(&cold.before, &cold.after, &config, temp.path(), 1024 * 1024).is_err());
    std::fs::write(&path, vec![b'x'; payload.len()]).unwrap();
    assert!(capture_transition_payload(&mut ProofReader(temp.path()), &mut cold).unwrap_err().contains("digest"));
    assert!(store_payload_proof(temp.path(), &payload).is_err());
    std::fs::remove_file(&path).unwrap();
    store_payload_proof(temp.path(), &payload).unwrap();
    capture_transition_payload(&mut ProofReader(temp.path()), &mut cold).unwrap();
    validate_checked_provider_transition(&cold).unwrap();
    let mut legacy: Value = serde_json::from_slice(&cold.after).unwrap();
    legacy["interface_state"]["provider"][AMENDMENT]["effect"]["payload"] = json!(String::from_utf8(payload).unwrap());
    cold.after = JsonFormat::Compact.encode(&legacy).unwrap();
    cold.capability_payload = None;
    validate_checked_provider_transition(&cold).unwrap();
    #[cfg(unix)] {
        std::fs::remove_file(&path).unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        std::os::unix::fs::symlink(outside.path(), &path).unwrap();
        assert!(capture_transition_payload(&mut ProofReader(temp.path()), &mut transition(&before, &authored)).is_err());
    }
}

#[test]
fn durable_capability_payload_generic_file_reader_uses_admitted_blob() {
    use octopus_interfaces::{CheckpointAdmissionMetadata, GrantProjection, InvocationMetadata, RuntimeInvocationMetadata};
    let (config, before) = fixture();
    let temp = tempfile::tempdir().unwrap();
    let owner = temp.path().join("owner"); std::fs::create_dir(&owner).unwrap();
    let amendment = proposal(&config, &before);
    let mut authored = ordinary_effect_padded(&config, &before, &owner, None, false, 256 * 1024);
    apply_to_authored_run(&serde_json::to_vec(&before).unwrap(), &mut authored, &amendment, &config, &owner).unwrap();
    let reference = transition_reference(&transition(&before, &authored)).unwrap().unwrap();
    let payload = read_payload_proof(&owner, &reference, 1024 * 1024).unwrap();
    let (ledger, blobs, index) = (temp.path().join("movements.jsonl"), temp.path().join("blobs"), temp.path().join("index.sqlite"));
    std::fs::write(&ledger, b"").unwrap(); std::fs::create_dir(&blobs).unwrap();
    let evidence = CheckedRunEvidence { movements_path: &ledger, ledger_index_path: &index, blobs_dir: &blobs,
        max_control_frame_bytes: 1024 * 1024, max_manifest_bytes: 1024 * 1024, max_payload_bytes: 1024 * 1024 };
    let run_path = temp.path().join("run.json");
    let before_bytes = serde_json::to_vec(&before).unwrap();
    let call = InterfaceArtifactFile::write(temp.path().join("calls"), "tool.call", &serde_json::to_vec(&json!({
        "tool":"write", "correlation_id":authored.correlation_id, "arguments":{
            "path":run_path, "expected_before_hash":authored.before_hash, "expected_after_hash":authored.after_hash,
            "patch":authored.patch, "json_format":"compact"
        }
    })).unwrap()).unwrap();
    let result = InterfaceArtifactFile::write(temp.path().join("results"), "write.result", &serde_json::to_vec(&json!({
        "label":"write.result", "tool":"write", "correlation_id":authored.correlation_id, "ok":true,
        "body":{"result":"files_written","writes":[{"path":run_path,"before_hash":authored.before_hash,"after_hash":authored.after_hash,"bytes":authored.content.len()}]}
    })).unwrap()).unwrap();
    let edge = |seq, bytes: &[u8], cp: &str, file: &InterfaceArtifactFile, target: &str| {
        let record = RunRecord::from_bytes(bytes).unwrap();
        let checkpoint = record.checkpoint(&cp.into()).unwrap();
        let hash = ContentHash::of_bytes(bytes).to_hex(); std::fs::write(blobs.join(&hash), bytes).unwrap();
        let manifest = std::fs::read(file.manifest_path()).unwrap(); let bytes = file.read_payload_bytes().unwrap();
        let manifest_hash = ContentHash::of_bytes(&manifest); let label = file.read_manifest().unwrap().label;
        std::fs::write(blobs.join(manifest_hash.to_hex()), manifest).unwrap();
        std::fs::write(blobs.join(ContentHash::of_bytes(&bytes).to_hex()), bytes).unwrap();
        let metadata = InvocationMetadata::new(GrantProjection {tools:vec!["write".into()],interfaces:vec![target.into()],..Default::default()})
            .with_runtime(RuntimeInvocationMetadata {run:record.id.clone(), ee:record.ee.clone(), worker:record.worker.clone(), checkpoint:cp.into(),interface:target.into(), epoch:checkpoint.epoch.0,run_record_hash:Some(hash)})
            .with_outbound_interface_bindings(checkpoint.outbound_interface_bindings.iter().map(|b|b.interface.clone()).collect())
            .with_checkpoint_binding_policy(CheckpointBindingPolicySnapshot {inbound:checkpoint.interface_binding.clone().unwrap(),outbound:checkpoint.outbound_interface_bindings.clone()})
            .with_required_verified_artifacts(checkpoint.required_verified_artifacts.clone())
            .with_checkpoint_admission(CheckpointAdmissionMetadata {artifact_label:label.clone(),required_artifact_labels:checkpoint.required_verified_artifacts.iter().map(|r|r.artifact_label.clone()).collect(),complete:true});
        Movement {seq:octopus_ledger::Seq(seq),lamport:octopus_ledger::Lamport(seq),recorded_at_unix_ms:seq,trace:octopus_ledger::TraceId("run".into()),kind:MovementKind::InterfaceOutbound,
            from:Endpoint::Interface(checkpoint.interface_binding.as_ref().unwrap().interface.clone()),to:Endpoint::Interface(target.into()),
            artifact:Some(ArtifactRef {id:format!("{label}:{}",manifest_hash.to_hex()).into(),content:manifest_hash}),invocation_metadata:Some(metadata.into())}
    };
    let mut zero = transition(&before, &authored).call; zero.seq = octopus_ledger::Seq(0); zero.from = Endpoint::Interface("unrelated".into());
    let mut rows = (0..20).map(|seq| { let mut row = zero.clone(); row.seq = octopus_ledger::Seq(seq); row.lamport = octopus_ledger::Lamport(seq); row }).collect::<Vec<_>>();
    rows.push(edge(20, &before_bytes, "writer_call", &call, "writer_room"));
    rows.push(edge(21, authored.content.as_bytes(), authored.authoring_result_checkpoint.as_str(), &result, "provider"));
    std::fs::write(&ledger, rows.iter().map(|row|format!("{}\n",serde_json::to_string(row).unwrap())).collect::<String>()).unwrap();
    let writer = transition(&before, &authored).writer;
    let full_chain = || checked_run_file_chain(&evidence, &run_path, &before_bytes, authored.content.as_bytes(), 0, &[writer.clone()]);
    assert!(full_chain().is_err(), "missing admitted proof cannot read an undeclared owner root");
    let mut reader = FileCheckedRunReader::open(&evidence, None).unwrap();
    let mut checked = transition(&before, &authored);
    assert!(capture_transition_payload(&mut reader, &mut checked).is_err());
    let blob = blobs.join(&reference.content_hash);
    std::fs::write(&blob, &payload).unwrap();
    let chain = full_chain().unwrap();
    assert_eq!(chain.len(), 1);
    validate_checked_provider_transition(&chain[0]).unwrap();
    capture_transition_payload(&mut reader, &mut checked).unwrap();
    validate_checked_provider_transition(&checked).unwrap();
    let initial = topology(&record(&before).unwrap(), &config.provider_interface).unwrap();
    let installed = topology(&RunRecord::from_bytes(&checked.after).unwrap(), &config.provider_interface).unwrap();
    assert!(checked_topology_advance(&config.provider_interface, &initial, &installed, &[checked]).unwrap());
    let (sender, _) = tokio::sync::mpsc::channel(1);
    let ctx = InterfaceLiftContext::unprivileged(config.provider_interface.clone(), temp.path().join("unused-interface"), None,
        owner, ledger.clone(), blobs.clone(), index.clone(), BTreeMap::new(), sender, Default::default());
    std::fs::write(&blob, vec![b'x'; payload.len()]).unwrap();
    assert!(full_chain().is_err());
    let mut owner_reader = FileCheckedRunReader::open(&evidence, None).unwrap().with_provider_context(&ctx);
    let mut cold = transition(&before, &authored);
    assert!(capture_transition_payload(&mut owner_reader, &mut cold).unwrap_err().contains("digest"));
    std::fs::remove_file(&blob).unwrap();
    capture_transition_payload(&mut owner_reader, &mut cold).unwrap();
    validate_checked_provider_transition(&cold).unwrap();
    let mut invalid = reference.clone(); invalid.schema_version = 2;
    assert!(owner_reader.capability_payload_proof(&config.provider_interface, &invalid).is_err());
    invalid = reference; invalid.content_hash = "../outside".into();
    assert!(owner_reader.capability_payload_proof(&config.provider_interface, &invalid).is_err());
    assert!(owner_reader.capability_payload_proof(&"other-provider".into(), &transition_reference(&cold).unwrap().unwrap()).is_err());
    #[cfg(unix)] {
        std::os::unix::fs::symlink(temp.path().join("missing-carrier"), &blob).unwrap();
        assert!(full_chain().is_err());
        assert!(capture_transition_payload(&mut owner_reader, &mut transition(&before, &authored)).unwrap_err().contains("symlink"));
    }
}
