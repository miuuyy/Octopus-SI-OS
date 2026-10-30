#[path = "../../../../world_operator_chat/packages/octopus-operator-chat-client/tests/support/composition_fixture.rs"]
mod support;

use octopus_descriptors::InterfaceId;
use octopus_ledger::{
    ArtifactRef, ContentHash, Endpoint, InvocationMetadata, Lamport, Movement, MovementKind, Seq,
    TraceId,
};
use octopus_operator_chat_client::{
    input_intake::InputIntakeConfig, run_composition::plan_run_composition,
};
use octopus_provider_codex::intake_state::{
    InputDelivery, IntakeState, load_state, prepare_input, save_state,
};
use octopus_run_authoring::provider_context_bindings;
use octopus_run_authoring::{CheckedRunTransition, validate_checked_owner_transition};
use octopus_run_record::RunRecord;
use serde_json::{Value, json};

#[tokio::test]
async fn exact_intake_composition_rearms_only_owned_edges_and_retains_crash_intent() {
    for legacy in [false, true] {
    let temp = tempfile::Builder::new().prefix("oct").tempdir_in("/tmp").unwrap();
    let root = temp.path().canonicalize().unwrap();
    let mut request = if legacy { support::client_fixture(&root) } else { support::fixture(&root) };
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../..");
    for relative in [
        "interface/system_service_input/interface.json",
        "interface/system_service_input/life.json",
    ] {
        let value: Value =
            serde_json::from_slice(&std::fs::read(source.join("registry").join(relative)).unwrap())
                .unwrap();
        support::write(&request.roots.registry_root, relative, value);
    }
    for (file, pointer) in [
        ("registry/ee_template/existence/ee_template.json", "/allowed_interfaces"),
        ("ee/ee_existence.json", "/policy_narrowing/allowed_interfaces"),
        ("registry/interface/system_worker_template/policy.json", "/ceiling/interfaces"),
    ] {
        let mut value: Value = serde_json::from_slice(&std::fs::read(root.join(file)).unwrap()).unwrap();
        value.pointer_mut(pointer).unwrap().as_array_mut().unwrap().push(json!("system_service_input"));
        support::write(&root, file, value);
    }
    let path = request
        .roots
        .registry_root
        .join("interface/world_provider_fixture/interface.json");
    let mut provider: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    provider["interface_driver_config"]["private_config"]["input_intake"] =
        json!({"template_interface_id":"system_service_input"});
    provider["interface_driver_config"]["private_config"]["context_output_labels"] = json!(["interface.artifact.output"]);
    provider["interface_driver_config"]["private_config"]["session_snapshot_labels"] = json!(["interface.artifact.output"]);
    provider["interface_driver_config"]["private_config"]["context_artifact_bindings"] = json!([{"id":"fixture.collection"}]);
    std::fs::write(path, serde_json::to_vec(&provider).unwrap()).unwrap();
    let collection = json!({"id":"fixture.collection","label":"context.collection","schema_version":1,"description":"Selected procedures",
        "directory":"entries","limits":{"max_entries":8,"max_entry_bytes":16384,"max_page_bytes":32768,"max_scan_bytes":131072}});
    let entry = json!({"id":"procedure","schema_version":1,"title":"Check source","description":"An exact instruction",
        "applicability":"When reviewing evidence","state":"active","scope":{"ee_ids":["ee_existence"]},"selection":{},
        "content":{"text":"Read the exact source before reporting the result."}});
    support::write(&request.roots.registry_root,"artifact/fixture.collection/artifact.json",collection.clone());
    support::write(&request.roots.registry_root,"artifact/fixture.collection/entries/procedure.json",entry.clone());
    let collection_ref = ArtifactRef {id:"fixture.collection".into(),content:ContentHash::of_canonical_json(&collection)};
    request.metadata.artifact_binding_refs.push(collection_ref.clone());
    request.metadata.grant.artifact_access.insert("fixture.collection".into(), octopus_runtime::ArtifactAccess::read_only());
    request.publications = serde_json::from_value(json!([{"source":collection_ref,"entry_id":"procedure","revision":ContentHash::of_canonical_json(&entry).to_hex()}])).unwrap();
    let plan = plan_run_composition(&request).unwrap();
    assert_ne!(plan.receive_interface_id, plan.worker_interface_id);
    assert_eq!(plan.seed.interface_id, plan.receive_interface_id);
    assert!(
        plan.required_binding_updates.iter().all(|update| update
            .outbound_binding
            .interface
            .as_str()
            == plan.receive_interface_id)
    );
    assert!(
        plan.required_interface_ids
            .contains(&plan.receive_interface_id)
    );
    assert!(plan.required_policy_updates.iter().any(|update| {
        update
            .added_interface_ids
            .contains(&plan.receive_interface_id)
    }));
    let record = RunRecord::from_bytes(plan.run_write.content.as_bytes()).unwrap();
    let id = InterfaceId::new(&plan.receive_interface_id);
    let config: InputIntakeConfig =
        serde_json::from_value(record.interface_state[&id]["input_intake"].clone()).unwrap();
    config.validate(&id).unwrap();
    let provider_state = record.interface_state[&config.provider_interface].clone();
    let worker: Value = serde_json::from_str(
        &plan
            .descriptor_writes
            .iter()
            .find(|write| {
                write
                    .path
                    .ends_with(format!("{}/interface.json", plan.worker_interface_id))
            })
            .unwrap()
            .content,
    )
    .unwrap();
    assert!(
        worker
            .get("interface_driver_ref")
            .is_none_or(|value| value == "")
    );
    let endpoint: Value = serde_json::from_str(
        &plan
            .descriptor_writes
            .iter()
            .find(|write| {
                write
                    .path
                    .ends_with(format!("{}/interface.json", plan.receive_interface_id))
            })
            .unwrap()
            .content,
    )
    .unwrap();
    assert_eq!(endpoint["role"], "service");
    admitted_publication_seed(&request, &plan, &config, legacy).await;
    if legacy { continue; }
    let mut current = plan.run_write.content.into_bytes();
    let mut previous_input = None;
    for sequence in [10, 11, 12] {
        let edit = prepare_input(
            &config,
            plan.run_write.path.clone(),
            &current,
            sequence,
            &ArtifactRef { id: format!("source:{}", ContentHash::of_bytes(b"source").to_hex()).into(), content: ContentHash::of_bytes(b"source") },
            sequence, 8 * 1024 * 1024
        )
        .unwrap();
        let replay = prepare_input(
            &config,
            plan.run_write.path.clone(),
            &current,
            sequence,
            &ArtifactRef { id: format!("source:{}", ContentHash::of_bytes(b"source").to_hex()).into(), content: ContentHash::of_bytes(b"source") },
            sequence, 8 * 1024 * 1024
        )
        .unwrap();
        assert_eq!(edit.edit.content, replay.edit.content);
        let next = RunRecord::from_bytes(edit.edit.content.as_bytes()).unwrap();
        let metadata: InvocationMetadata = serde_json::from_value(json!({"grant":{"role_artifacts":config.artifact_bindings.iter().map(|binding| binding.id.as_str()).collect::<Vec<_>>(),"artifact_access":config.artifact_bindings.iter().map(|binding| (binding.id.to_string(),json!({"read":true,"change":false,"create":false,"delete":false}))).collect::<serde_json::Map<_,_>>()}})).unwrap();
        let call = Movement {
            seq: Seq(sequence),
            lamport: Lamport(sequence),
            recorded_at_unix_ms: 0,
            trace: TraceId("fixture".into()),
            kind: MovementKind::InterfaceOutbound,
            from: Endpoint::Interface(id.clone()),
            to: Endpoint::Interface(config.writer.door.clone()),
            artifact: None,
            invocation_metadata: Some(metadata.into()),
        };
        let transition = CheckedRunTransition {
            writer: config.writer.clone(),
            before: current.clone(),
            after: edit.edit.content.as_bytes().to_vec(),
            call: call.clone(),
            result: call,
            placement: false,
        };
        validate_checked_owner_transition(&transition).unwrap();
        let mut foreign = transition.clone();
        let mut changed: Value = serde_json::from_slice(&foreign.after).unwrap();
        changed["interface_state"][config.provider_interface.as_str()]["context_generation"] =
            json!(999);
        foreign.after = serde_json::to_vec(&changed).unwrap();
        assert!(validate_checked_owner_transition(&foreign).is_err());
        assert_eq!(
            next.interface_state[&config.provider_interface],
            provider_state
        );
        let owned: Vec<String> =
            serde_json::from_value(next.interface_state[&id]["owned_checkpoint_ids"].clone())
                .unwrap();
        assert_eq!(owned.len(), 3);
        let input = next
            .checkpoints
            .iter()
            .find(|cp| {
                cp.inbound_interface() == Some(&id)
                    && cp.outbound_interfaces().next() == Some(&config.provider_interface)
            })
            .unwrap();
        assert_eq!(
            input.artifact_bindings,
            octopus_run_authoring::input_artifacts::read_bindings(&next, &id, provider_context_bindings(&next, &config.provider_interface).unwrap()).unwrap()
        );
        if let Some(previous) = previous_input {
            assert!(next.checkpoint(&previous).is_none());
        }
        previous_input = Some(input.id.clone());
        assert_eq!(next.checkpoints.len(), record.checkpoints.len());
        current = edit.edit.content.into_bytes();
    }
    let path = temp.path().join("state.json");
    let state = IntakeState {
        completed_through: Some(12),
        pending: None,
    };
    save_state(&path, &state, 1024).unwrap();
    assert_eq!(load_state(&path, 1024).unwrap().completed_through, Some(12));
    assert!(load_state(&path, 1).is_err());
    let hash = ContentHash::of_bytes(b"delivery");
    let input = RunRecord::from_bytes(&current)
        .unwrap()
        .checkpoints
        .into_iter()
        .find(|cp| {
            cp.inbound_interface() == Some(&id)
                && cp.outbound_interfaces().next() == Some(&config.provider_interface)
        })
        .unwrap();
    let metadata: InvocationMetadata = serde_json::from_value(json!({"runtime":{"run":config.run,"ee":record.ee,"worker":record.worker,"checkpoint":input.id,"interface":config.provider_interface,"epoch":0},"checkpoint_binding_policy":{"inbound":input.interface_binding,"outbound":input.outbound_interface_bindings},"checkpoint_admission":{"artifact_label":config.input_label,"required_artifact_labels":[],"complete":true}})).unwrap();
    let delivered = Movement {
        seq: Seq(40),
        lamport: Lamport(40),
        recorded_at_unix_ms: 0,
        trace: TraceId("fixture".into()),
        kind: MovementKind::InterfaceOutbound,
        from: Endpoint::Interface(id.clone()),
        to: Endpoint::Interface(config.provider_interface.clone()),
        artifact: Some(ArtifactRef {
            id: format!("delivery:{}", hash.to_hex()).into(),
            content: hash,
        }),
        invocation_metadata: Some(metadata.into()),
    };
    assert_eq!(
        octopus_provider_codex::intake_state::admitted_input(&[], &config, &hash, 12).unwrap(),
        None
    );
    assert_eq!(
        octopus_provider_codex::intake_state::admitted_input(
            &[delivered.clone()],
            &config,
            &hash,
            12
        )
        .unwrap(),
        Some(40)
    );
    assert!(
        octopus_provider_codex::intake_state::admitted_input(
            &[delivered.clone(), delivered.clone()],
            &config,
            &hash,
            12
        )
        .is_err()
    );
    let delivery: InputDelivery = serde_json::from_value(json!({"schema_version":1,"source_movement_seq":12,"source_artifact":delivered.artifact,"message":{"action":"followup","control_id":"retained","text":"resume from evidence"}})).unwrap();
    let source_ref = delivered.artifact.clone().unwrap();
    let pending = octopus_provider_codex::intake_state::PendingInput {
        source: delivered.clone(),
        delivery,
        edit: Some(prepare_input(&config, plan.run_write.path.clone(), &current, 13, &source_ref, 40, 8 * 1024 * 1024).unwrap()),
    };
    let retained = IntakeState {
        completed_through: Some(11),
        pending: Some(pending),
    };
    save_state(&path, &retained, 1024 * 1024).unwrap();
    assert_eq!(
        serde_json::to_value(load_state(&path, 1024 * 1024).unwrap()).unwrap(),
        serde_json::to_value(retained).unwrap()
    );
    let mut corrupted = config.clone();
    corrupted.writer.door = id;
    assert!(prepare_input(&corrupted, plan.run_write.path, &current, 13, &source_ref, 13, 8 * 1024 * 1024).is_err());
    }
}

async fn admitted_publication_seed(
    request: &octopus_operator_chat_client::run_composition::RunCompositionRequest,
    plan: &octopus_operator_chat_client::run_composition::RunCompositionPlan,
    config: &InputIntakeConfig,
    legacy: bool,
) {
    use futures::{SinkExt, StreamExt};
    use octopus_app_server::{SharedBlobHandle, SpineRootConfig, admit_interface_artifact_arrival, build_embedded_spine};
    use octopus_interface_transport::{InterfaceHostIoLimits, InterfaceLiftContext};
    use octopus_interfaces::{DeliveryVerdict, InterfaceArtifactFile, InterfaceArtifactManifest, InterfaceInvocationEnvelope};
    use octopus_ledger::{LiveMovementLog, SharedBlobs};
    use octopus_ledger_store::{DurableBlobs, DurableLog};
    use octopus_run_authoring::{CheckedRunWriter, ProviderEmission, ProviderEmissionRoute, ProviderHandoffSession};
    use octopus_operator_chat_client::run_revision::{prepare_run_revision, verify_run_revision};
    use octopus_tool_handlers::{ToolCallArtifact, ToolResultArtifact};
    use octopus_world_pyecho_life_runtime::LedgerTailReader;
    use std::{collections::BTreeMap, sync::{Arc, Mutex}};

    fn envelope(ctx: &InterfaceLiftContext, file: &InterfaceArtifactFile, interface: &InterfaceId) -> (u64, InterfaceInvocationEnvelope) {
        let mut context = ctx.clone(); context.interface = interface.clone();
        let mut reader = LedgerTailReader::open_named(&context, "publication-seed").unwrap();
        let hash = ContentHash::of_bytes(&std::fs::read(file.manifest_path()).unwrap());
        let rows = reader.read_new().unwrap();
        let row = rows.iter().rev().find(|row| row.movement.artifact.as_ref().is_some_and(|a| a.content == hash)).unwrap();
        (row.movement.seq.0, reader.envelope(row).unwrap())
    }
    async fn write(runtime: &octopus_app_server::EmbeddedRuntime, log: &mut LiveMovementLog<DurableLog>, handle: &mut SharedBlobHandle,
        ctx: &InterfaceLiftContext, call: &InterfaceArtifactFile, writer: &CheckedRunWriter) -> DeliveryVerdict {
        let admitted = admit_interface_artifact_arrival(runtime,log,handle,&call.clone().capture_bounded(1<<20,1<<26).unwrap(),&writer.source).await.unwrap();
        let verdict = DeliveryVerdict::Admitted {seq:admitted.admission.records[0].arrival_movement.seq.0};
        let (_, input) = envelope(ctx,call,&writer.door);
        let tool: ToolCallArtifact = serde_json::from_slice(&input.read_artifact_payload().unwrap()).unwrap();
        let result = octopus_system_room_env::execute_file_write_result(&input,&tool.arguments,&["system_room".into()],
            &BTreeMap::from([("run_records_root".into(),ctx.root_slots["run_records_root"].clone())]),&ctx.var_dir.join("effects"),1<<26).unwrap();
        let returned = ToolResultArtifact::success(&tool.tool,tool.correlation_id,result);
        let file = InterfaceArtifactFile::write(ctx.var_dir.join("results"),&returned.label,&serde_json::to_vec(&returned).unwrap()).unwrap();
        admit_interface_artifact_arrival(runtime,log,handle,&file.capture_bounded(1<<20,1<<26).unwrap(),&writer.door).await.unwrap();
        verdict
    }
    let root = request.roots.registry_root.parent().unwrap();
    for edit in plan.descriptor_writes.iter().chain(&plan.run_writes).chain(plan.required_policy_updates.iter().map(|update| &update.write)).chain(std::iter::once(&plan.run_write)) {
        std::fs::create_dir_all(edit.path.parent().unwrap()).unwrap(); std::fs::write(&edit.path, &edit.content).unwrap();
    }
    if legacy {
        let mut original: Value = serde_json::from_str(&plan.run_write.content).unwrap();
        for template in original["interface_state"][config.provider_interface.as_str()]["run_authoring_binding"]["checkpoints"].as_array_mut().unwrap() {
            if template["role"] == "owner_input" { template.as_object_mut().unwrap().remove("static_artifact_bindings"); }
        }
        std::fs::write(&plan.run_write.path,serde_json::to_vec(&original).unwrap()).unwrap();
    }
    let mut parent = request.admitted_parent_record.clone();
    parent.checkpoints[0].artifact_bindings = vec![serde_json::from_value(json!({"id":"role-system_worker_parent"})).unwrap()];
    for update in &plan.required_binding_updates {
        parent.checkpoints[0].outbound_interface_bindings.push(update.outbound_binding.clone());
        for binding in &update.required_artifact_bindings {
            if !parent.checkpoints[0].artifact_bindings.iter().any(|b| b.id.as_str() == binding.id) {
                parent.checkpoints[0].artifact_bindings.push(serde_json::from_value(json!({"id":binding.id})).unwrap());
            }
        }
    }
    std::fs::write(request.roots.run_records_root.join("caller.json"), serde_json::to_vec(&parent).unwrap()).unwrap();
    let blobs: SharedBlobs = Arc::new(Mutex::new(DurableBlobs::open(root.join("ledger/blobs")).unwrap()));
    let runtime = build_embedded_spine(SpineRootConfig {registry_root:request.roots.registry_root.clone(),run:plan.run_id.clone().into(),blobs:blobs.clone(),
        interface_artifact_root:root.join("artifacts"),run_records_root:request.roots.run_records_root.clone(),ee_instances_root:request.roots.ee_instances_root.clone()}).await.unwrap();
    let mut log = LiveMovementLog::new(DurableLog::open(root.join("ledger/movements.jsonl")).unwrap());
    let mut handle = SharedBlobHandle::new(blobs);
    let (sender, mut receiver) = tokio::sync::mpsc::channel(8);
    let provider_root = request.roots.registry_root.join("interface").join(config.provider_interface.as_str());
    let descriptor = serde_json::from_slice(&std::fs::read(provider_root.join("interface.json")).unwrap()).unwrap();
    let mut ctx = InterfaceLiftContext::unprivileged(config.provider_interface.clone(),provider_root,Some(descriptor),root.join("provider"),
        root.join("ledger/movements.jsonl"),root.join("ledger/blobs"),root.join("ledger/index.sqlite"),
        BTreeMap::from([("run_records_root".into(),request.roots.run_records_root.clone()),("interface_hand_in_root".into(),root.join("hand-in"))]),sender,Default::default());
    ctx.host_io_limits = Some(InterfaceHostIoLimits {max_control_frame_bytes:1<<20,max_manifest_bytes:1<<20,max_payload_bytes:1<<26,max_stderr_bytes:1024});
    let file = |label: &str, body: Value| InterfaceArtifactFile::write(root.join("emitted"),label,&serde_json::to_vec(&body).unwrap()).unwrap();
    let seed = file(&plan.seed.label,plan.seed.payload.clone());
    octopus_app_server::InterfaceArtifactArrivalFromInterface::new(&runtime.runtime,&mut log,&mut handle,runtime.arrival_dedup.clone(),
        parent.trace.clone(),parent.checkpoints[0].inbound_interface().unwrap().clone())
        .emit_artifact_to_interface(&seed,&parent.id,&parent.checkpoints[0].id,parent.checkpoints[0].epoch,Lamport(0),&config.writer.owner).await.unwrap();
    let (mut seed_seq, mut seed_input) = envelope(&ctx,&seed,&config.writer.owner);
    for attempt in 0..if legacy {2} else {1} {
    let current = std::fs::read(&plan.run_write.path).unwrap();
    let edit = prepare_input(config,plan.run_write.path.clone(),&current,seed_seq,&seed_input.artifact,seed_seq,1<<26).unwrap();
    let call = edit.materialize(&root.join("write"),config).unwrap();
    write(&runtime,&mut log,&mut handle,&ctx,&call,&config.writer).await;
    let delivery = InputDelivery {schema_version:1,source_movement_seq:seed_seq,source_artifact:seed_input.artifact.clone(),
        message:serde_json::from_value(json!({"action":"queue","control_id":"publication-seed","text":"Use the selected procedure","artifacts":[seed_input.artifact]})).unwrap()};
    let delivered = file(&config.input_label,json!(delivery));
    admit_interface_artifact_arrival(&runtime,&mut log,&mut handle,&delivered.clone().capture_bounded(1<<20,1<<26).unwrap(),&config.writer.owner).await.unwrap();
    let (seq,input) = envelope(&ctx,&delivered,&config.provider_interface);
    let pinned = ctx.pinned_run_record_bytes(&input).unwrap();
    let admitted = RunRecord::from_bytes(&pinned).unwrap();
    let template = admitted.interface_state[&config.provider_interface]["run_authoring_binding"]["checkpoints"].as_array().unwrap().iter()
        .find(|template| template["interface_binding"]["interface"] == config.writer.owner.as_str()).unwrap();
    let static_ids = template["static_artifact_bindings"].as_array().cloned().unwrap_or_default();
    let snapshot = plan.publication_evidence[0].snapshot.as_ref().unwrap();
    for reference in [&request.publications[0].source,snapshot] {
        assert_eq!(static_ids.contains(&json!(reference.id)), !legacy || attempt == 1);
        assert!(input.metadata().unwrap().artifact_binding_refs.contains(reference));
        assert!(input.metadata().unwrap().grant.may_read_artifact(reference.id.as_str()));
    }
    assert!(!static_ids.contains(&json!(seed_input.artifact.id)));
    let sources = octopus_run_authoring::input_artifacts::selections(&admitted,&config.writer.owner).unwrap();
    assert_eq!(sources.len(),attempt+1); assert_eq!(sources.last().unwrap().artifact,seed_input.artifact); assert_eq!(sources.last().unwrap().source_movement_seq,seed_seq);
    let session = ProviderHandoffSession::new(&ctx).unwrap();
    let handoff = session.from_invocation(&ctx,ctx.descriptor_snapshot.as_ref().unwrap(),&admitted.id,&input,seq,&pinned);
    if legacy && attempt == 0 {
        let failure = handoff.err().unwrap(); assert!(failure.contains("has no movement"));
        ctx.root_slots.insert("app_server_control_root".into(),root.join("control"));
        let path = octopus_app_server_interface::interface_control_socket_path_from_root(&root.join("control"),&plan.authoring.source_interface_id.clone().into());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let socket = tokio::net::UnixListener::bind(path).unwrap();
        let descriptor = ctx.descriptor_snapshot.as_ref().unwrap();
        let failed = session.failed_source_handoff(&ctx,descriptor,&admitted.id,&input,seq).unwrap();
        let descriptor_value = serde_json::to_value(descriptor).unwrap();
        let profile = serde_json::from_value(descriptor_value.pointer("/interface_driver_config/private_config/run_authoring_profile").unwrap().clone()).unwrap();
        let writer = octopus_run_authoring::declared_provider_run_writer(&admitted,&config.provider_interface,
            &profile).unwrap();
        let error = InterfaceArtifactFile::write(ctx.var_dir.join("emitted"),"turn.error",
            &serde_json::to_vec(&json!({"label":"turn.error","message":failure,"will_retry":false,"additional_details":{"source_movement_seq":seq}})).unwrap()).unwrap();
        let (_, ()) = tokio::time::timeout(std::time::Duration::from_secs(20), async {tokio::join!(
            async {failed.hand_failed_source(ProviderEmission {artifact:error,route:ProviderEmissionRoute::ClientProjection}).await.unwrap()},
            async {
                let mut ws = tokio_tungstenite::accept_async(socket.accept().await.unwrap().0).await.unwrap();
                let bytes = ws.next().await.unwrap().unwrap().into_data();
                let call = InterfaceArtifactFile::store_manifest_bounded(&root.join("transport"),&bytes,1<<20,1<<26).unwrap();
                let verdict = write(&runtime,&mut log,&mut handle,&ctx,&call,&writer).await;
                ws.send(tokio_tungstenite::tungstenite::Message::Text(serde_json::to_string(&verdict).unwrap().into())).await.unwrap();
                let arrival = receiver.recv().await.unwrap();
                if let Some(retained) = arrival.retained {
                    let delivery = octopus_app_server::admit_retained_interface_artifact_arrival(&runtime,&mut log,&mut handle,&arrival.artifact,&config.provider_interface,&retained.id).await.unwrap();
                    retained.reply.send(Ok(delivery)).unwrap();
                } else {
                    let outcome = admit_interface_artifact_arrival(&runtime,&mut log,&mut handle,&arrival.artifact.capture_bounded(1<<20,1<<26).unwrap(),&config.provider_interface).await.unwrap();
                    arrival.verdict_tx.unwrap().send(DeliveryVerdict::Admitted {seq:outcome.admission.records[0].arrival_movement.seq.0}).unwrap();
                }
            }
        )}).await.unwrap();
        let before = std::fs::read_to_string(&plan.run_write.path).unwrap();
        let before_record: Value = serde_json::from_str(&before).unwrap();
        let client = before_record["interface_state"].as_object().unwrap().iter().find(|(_,state)|state["provider_runtime_interface"] == config.provider_interface.as_str()).unwrap().0;
        let corrected: Value = serde_json::from_str(&plan.run_write.content).unwrap();
        let desired = corrected["interface_state"][config.provider_interface.as_str()]["run_authoring_binding"]["checkpoints"].as_array().unwrap().iter().filter(|row|row["role"] == "owner_input").cloned().collect::<Vec<_>>();
        let scope = json!({"registry_root":request.roots.registry_root,"run_path":plan.run_write.path,"ledger_root":root.join("ledger"),"artifact_root":root.join("client"),"interface_hand_in_root":root.join("hand-in"),"provider":config.provider_interface,"source":client,"key":"old-child-static"});
        let mut revision = scope.clone(); revision["operation"] = json!({"kind":"owner_inputs","desired_inputs":desired}); revision["expected_before_content"] = json!(before);
        assert_eq!(prepare_run_revision(serde_json::from_value(revision.clone()).unwrap()).unwrap()["status"],"prepared");
        assert_eq!(prepare_run_revision(serde_json::from_value(revision).unwrap()).unwrap()["status"],"prepared");
        let ready = verify_run_revision(serde_json::from_value(scope.clone()).unwrap()).unwrap(); assert_eq!(ready["status"],"ready");
        let manifest: InterfaceArtifactManifest = serde_json::from_value(ready["manifest"].clone()).unwrap();
        let call = InterfaceArtifactFile::store_manifest_bounded(&root.join("revision"),&serde_json::to_vec(&manifest).unwrap(),1<<20,1<<26).unwrap();
        write(&runtime,&mut log,&mut handle,&ctx,&call,&writer).await;
        assert_eq!(verify_run_revision(serde_json::from_value(scope).unwrap()).unwrap()["status"],"installed");
        let after = RunRecord::from_bytes(&std::fs::read(&plan.run_write.path).unwrap()).unwrap();
        let prior = RunRecord::from_bytes(before.as_bytes()).unwrap();
        assert_eq!(prior.interface_state[&config.provider_interface]["pending_handoff"],after.interface_state[&config.provider_interface]["pending_handoff"]);
        assert_eq!(ctx.pinned_run_record_bytes(&input).unwrap(),pinned);
        let next = file(&plan.seed.label,json!({"text":"Continue in this same run with the selected publication"}));
        octopus_app_server::InterfaceArtifactArrivalFromInterface::new(&runtime.runtime,&mut log,&mut handle,runtime.arrival_dedup.clone(),parent.trace.clone(),parent.checkpoints[0].inbound_interface().unwrap().clone())
            .emit_artifact_to_interface(&next,&parent.id,&parent.checkpoints[0].id,parent.checkpoints[0].epoch,Lamport(0),&config.writer.owner).await.unwrap();
        (seed_seq,seed_input) = envelope(&ctx,&next,&config.writer.owner);
    } else {
        let handoff = handoff.unwrap();
        assert_eq!(handoff.recover_source().await.unwrap(),octopus_run_authoring::ProviderSourceRecovery::Process);
    }
    assert!(input.artifact_binding_payloads.iter().find(|bound| bound.artifact == *snapshot).unwrap().read_bytes().unwrap()
        .windows(b"Read the exact source".len()).any(|window| window == b"Read the exact source"));
    }
}

#[test]
fn input_delivery_identity_preserves_distinct_equal_messages_and_actions() {
    let reference =
        json!({"id":"source", "content":octopus_ledger::ContentHash::of_bytes(b"source")});
    let body = json!({"schema_version":1,"source_movement_seq":10,"source_artifact":reference,"message":{"action":"queue","control_id":"same","text":"same"}});
    let first: InputDelivery = serde_json::from_value(body.clone()).unwrap();
    first.validate().unwrap();
    let mut second = body.clone();
    second["source_movement_seq"] = json!(11);
    assert_ne!(
        serde_json::to_vec(&first).unwrap(),
        serde_json::to_vec(&serde_json::from_value::<InputDelivery>(second).unwrap()).unwrap()
    );
    for action in ["queue", "followup", "interrupt"] {
        let mut value = body.clone();
        value["message"]["action"] = json!(action);
        serde_json::from_value::<InputDelivery>(value)
            .unwrap()
            .validate()
            .unwrap();
    }
    let mut invalid = body;
    invalid["message"]["action"] = json!("hidden_resume");
    assert!(serde_json::from_value::<InputDelivery>(invalid).is_err());
}
