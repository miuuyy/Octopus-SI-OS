use super::*;
use octopus_descriptors::InterfaceId;
use octopus_ledger::{Lamport, Seq, TraceId};
use std::io::Write;

#[test]
fn intake_receipts_follow_exact_inline_or_separate_writer_identity() {
    let config: InputIntakeConfig = serde_json::from_value(serde_json::json!({
        "schema_version":1,"run":"recipient","provider_interface":"provider","input_label":"provider.peer.input",
        "writer":{"owner":"intake","source":"intake","door":"writer","tool":"file.write","call_label":"tool.call","result_label":"file.write.result"},
        "profile":{"file_write_tool":"file.write","file_write_label":"tool.call","file_write_result_label":"file.write.result"},
        "provider_profile":{"file_write_tool":"file.write","file_write_label":"tool.call","file_write_result_label":"file.write.result"},
        "writer_required_verified_artifacts":[{"artifact_label":"tool.call","mark":"tool.call.allowed","verifier":"tool_call_allowed","params":{"tools":["file.write"],"metadata_matches":[{"metadata":"/runtime/run_record_hash","payload":"/arguments/expected_before_hash"}]}}],"artifact_bindings":[]
    })).unwrap();
    let source = octopus_ledger::ArtifactRef { id: "outcome".into(), content: ContentHash::of_bytes(b"outcome") };
    for (selection, action) in [(InputNotificationAction::Queue, PeerAction::Queue), (InputNotificationAction::Followup, PeerAction::Followup)] {
        let mut selected = config.clone();
        selected.notification_action = selection;
        let message = input_message(&selected, &selected.writer.owner, 9, "interface.artifact.output", br#"{"text":"completed result"}"#, &source).unwrap();
        assert_eq!(message.action, action);
        assert_eq!(message.artifacts, vec![source.clone()]);
        assert_eq!(message, input_message(&selected, &selected.writer.owner, 9, "interface.artifact.output", br#"{"text":"completed result"}"#, &source).unwrap());
        let explicit = input_message(&selected, &selected.writer.owner, 10, PEER_MESSAGE_LABEL, br#"{"action":"queue","control_id":"passive","text":"context only"}"#, &source).unwrap();
        assert_eq!(explicit.action, PeerAction::Queue);
    }
    let mut explicit_queue = serde_json::json!(config);
    explicit_queue["notification_action"] = serde_json::json!("queue");
    assert!(config.matches_declaration(&explicit_queue).unwrap());
    for invalid in [serde_json::Value::Null, serde_json::json!("interrupt")] {
        explicit_queue["notification_action"] = invalid;
        assert!(config.matches_declaration(&explicit_queue).is_err());
    }
    let (checkpoints, ids) = octopus_operator_chat_client::input_intake::initial_checkpoints(&config, &serde_json::json!("trace"), "initial").unwrap();
    let mut document = serde_json::json!({"id":"recipient","ee":"ee","worker":"worker","trace":"trace","status":"idle","checkpoints":checkpoints,"active_checkpoints":ids,"interface_state":{"intake":{"input_intake":config,"owned_checkpoint_ids":ids,"frontier_checkpoint_ids":[ids[1],ids[2]]}}});
    let payload = serde_json::to_vec(&ToolResultArtifact::success("file.write", Some("write:1".into()), serde_json::json!({"result":"files_written"}))).unwrap();
    for run in ["recipient", "separate_writer"] {
        if run == "separate_writer" {
            octopus_run_authoring::separate_owner_writer_run(&mut document, &config.writer, &octopus_descriptors::RunId::new(run)).unwrap();
        }
        let record = RunRecord::from_bytes(&serde_json::to_vec(&document).unwrap()).unwrap();
        let mut movement = Movement { seq:Seq(1),lamport:Lamport(1),recorded_at_unix_ms:0,trace:TraceId("trace".into()),kind:MovementKind::InterfaceOutbound,
            from:Endpoint::Interface(config.writer.door.clone()),to:Endpoint::Interface(config.writer.owner.clone()),artifact:Some(octopus_ledger::ArtifactRef{id:"receipt".into(),content:ContentHash::of_bytes(b"receipt")}),
            invocation_metadata:Some(serde_json::from_value::<octopus_ledger::InvocationMetadata>(serde_json::json!({"runtime":{"run":run,"ee":"ee","worker":"worker","checkpoint":"receipt","interface":"intake","epoch":0}})).unwrap().into()) };
        assert!(is_internal_writer_receipt(&config,&record,&movement,&config.writer.result_label,&payload).unwrap());
        assert!(!is_internal_writer_receipt(&config,&record,&movement,"interface.artifact.output",&payload).unwrap());
        assert!(is_internal_writer_receipt(&config,&record,&movement,&config.writer.result_label,b"{}").is_err());
        let mut metadata = movement.invocation_metadata.as_ref().unwrap().invocation().unwrap().clone();
        metadata.runtime.as_mut().unwrap().run = octopus_descriptors::RunId::new("foreign_writer");
        movement.invocation_metadata = Some(metadata.into());
        assert!(!is_internal_writer_receipt(&config,&record,&movement,&config.writer.result_label,&payload).unwrap());
    }
}

#[test]
fn receipt_tail_does_not_buffer_other_inputs_and_waits_for_complete_rows() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("movements.jsonl");
    let owner = InterfaceId::new("intake");
    let door = InterfaceId::new("writer");
    let (arrivals, _receiver) = tokio::sync::mpsc::channel(1);
    let mut ctx = InterfaceLiftContext::unprivileged(
        owner.clone(),
        dir.path().into(),
        None,
        dir.path().into(),
        path.clone(),
        dir.path().into(),
        dir.path().join("index.sqlite"),
        Default::default(),
        arrivals,
        tokio_util::sync::CancellationToken::new(),
    );
    ctx.host_io_limits = Some(octopus_interface_transport::InterfaceHostIoLimits {
        max_control_frame_bytes: 65536,
        max_stderr_bytes: 1024,
        max_manifest_bytes: 65536,
        max_payload_bytes: 65536,
    });
    let row = |seq| Movement {
        seq: Seq(seq),
        lamport: Lamport(seq),
        recorded_at_unix_ms: 0,
        trace: TraceId("source".into()),
        kind: MovementKind::InterfaceOutbound,
        from: Endpoint::Interface("sender".into()),
        to: Endpoint::Interface(owner.clone()),
        artifact: None,
        invocation_metadata: None,
    };
    let mut file = std::fs::File::create(&path).unwrap();
    for seq in 0..1000 {
        writeln!(file, "{}", serde_json::to_string(&row(seq)).unwrap()).unwrap();
    }
    let mut tail = EvidenceTail::open(&ctx).unwrap();
    tail.catch_up(&ctx, None, 7, &door).unwrap();
    assert_eq!(tail.next_seq, 1000);
    assert_eq!(tail.rows, vec![row(7)]);
    let mut result = row(1000);
    result.from = Endpoint::Interface(door.clone());
    let encoded = serde_json::to_vec(&result).unwrap();
    file.write_all(&encoded[..encoded.len() / 2]).unwrap();
    tail.catch_up(&ctx, None, 7, &door).unwrap();
    assert_eq!(tail.next_seq, 1000);
    file.write_all(&encoded[encoded.len() / 2..]).unwrap();
    file.write_all(b"\n").unwrap();
    tail.catch_up(&ctx, None, 7, &door).unwrap();
    assert_eq!(tail.rows, vec![row(7), result]);
    tail.retain_movement(row(7)).unwrap();
    tail.retain_movement(row(3)).unwrap();
    assert_eq!(tail.rows.iter().map(|movement| movement.seq.0).collect::<Vec<_>>(), vec![3, 7, 1000]);
    let mut changed = row(7);
    changed.trace = TraceId("different-source".into());
    assert!(tail.retain_movement(changed).is_err());
    let artifact = PreparedIntakeArtifact::new(&ctx,
        InterfaceArtifactFile::write(dir.path().join("emitted"), "input.delivery", b"{}").unwrap()).unwrap();
    let manifest = artifact.file.read_manifest().unwrap();
    std::fs::write(&manifest.payload_path, b"changed").unwrap();
    assert!(artifact.file.read_bounded(65536, 65536).is_err(), "retained artifacts must not rewrite or conceal poisoned bytes");
    file.set_len(0).unwrap();
    assert!(tail.catch_up(&ctx, None, 7, &door).is_err());
}

#[tokio::test(start_paused = true)]
async fn idle_intake_sleeps_at_its_cursor_and_a_woken_life_takes_the_input_from_files() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().canonicalize().unwrap();
    let path = home.join("movements.jsonl");
    let owner = InterfaceId::new("intake");
    let mut descriptor: octopus_interfaces::InterfaceDescriptor = serde_json::from_slice(&std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../system_service_input/interface.json")).unwrap()).unwrap();
    descriptor.id = owner.clone();
    descriptor.interface_driver_config.as_mut().unwrap().private_config.insert("input_intake".into(), serde_json::json!({
        "schema_version":1,"run":"recipient","provider_interface":"provider","input_label":"provider.peer.input",
        "writer":{"owner":"intake","source":"intake","door":"writer","tool":"file.write","call_label":"tool.call","result_label":"file.write.result"},
        "profile":{"file_write_tool":"file.write","file_write_label":"tool.call","file_write_result_label":"file.write.result"},
        "provider_profile":{"file_write_tool":"file.write","file_write_label":"tool.call","file_write_result_label":"file.write.result"},
        "writer_required_verified_artifacts":[{"artifact_label":"tool.call","mark":"tool.call.allowed","verifier":"tool_call_allowed","params":{"tools":["file.write"],"metadata_matches":[{"metadata":"/runtime/run_record_hash","payload":"/arguments/expected_before_hash"}]}}],"artifact_bindings":[]
    }));
    let life = || {
        let (arrivals, _receiver) = tokio::sync::mpsc::channel(1);
        let mut ctx = InterfaceLiftContext::unprivileged(
            owner.clone(), home.clone(), Some(descriptor.clone()), home.join("var"), path.clone(), home.join("blobs"),
            home.join("index.sqlite"), [("run_records_root".to_string(), home.join("runs"))].into(),
            arrivals, tokio_util::sync::CancellationToken::new(),
        );
        ctx.host_io_limits = Some(octopus_interface_transport::InterfaceHostIoLimits {
            max_control_frame_bytes: 65536, max_stderr_bytes: 1024, max_manifest_bytes: 65536, max_payload_bytes: 65536,
        });
        ctx
    };
    std::fs::create_dir_all(home.join("runs")).unwrap();
    std::fs::create_dir_all(home.join("blobs")).unwrap();
    let sent = Movement {
        seq: Seq(0), lamport: Lamport(0), recorded_at_unix_ms: 0, trace: TraceId("source".into()),
        kind: MovementKind::InterfaceOutbound, from: Endpoint::Interface("sender".into()),
        to: Endpoint::Interface("elsewhere".into()), artifact: None, invocation_metadata: None,
    };
    let mut ledger = std::fs::File::create(&path).unwrap();

    let idle = life();
    run(idle.clone()).await.unwrap();
    assert_eq!(idle.sleep_declaration().and_then(|sleep| sleep.through_seq), Some(0),
        "an idle intake sleeps at the first movement it has not read");

    let message = br#"{"action":"queue","control_id":"passive","text":"context only"}"#;
    let file = InterfaceArtifactFile::write(home.join("sent"), PEER_MESSAGE_LABEL, message).unwrap();
    std::fs::write(home.join("blobs").join(ContentHash::of_bytes(message).to_hex()), message).unwrap();
    let manifest = std::fs::read(file.manifest_path()).unwrap();
    let artifact = octopus_ledger::ArtifactRef { id: "provider.peer.message:woken".into(), content: ContentHash::of_bytes(&manifest) };
    std::fs::write(home.join("blobs").join(artifact.content.to_hex()), &manifest).unwrap();
    let mut input = sent;
    input.to = Endpoint::Interface(owner.clone());
    input.artifact = Some(artifact.clone());
    input.invocation_metadata = Some(octopus_ledger::InvocationMetadata::new(Default::default())
        .with_runtime(octopus_ledger::RuntimeInvocationMetadata {
            run: octopus_descriptors::RunId::new("recipient"), ee: octopus_descriptors::EeId::new("ee"),
            worker: InterfaceId::new("worker"), checkpoint: octopus_descriptors::CheckpointId::new("input"),
            interface: owner.clone(), epoch: 0, run_record_hash: None,
        }).into());
    writeln!(ledger, "{}", serde_json::to_string(&input).unwrap()).unwrap();

    let woken = life();
    let ended = run(woken.clone()).await;
    let state = load_state(&home.join("var").join("input-state.json"), 262144).unwrap();
    let pending = state.pending.unwrap_or_else(|| panic!("the woken life retains the input that arrived at its sleep boundary: {ended:?}"));
    assert_eq!((pending.source.seq, pending.delivery.source_artifact), (Seq(0), artifact));
    assert!(woken.sleep_declaration().is_none() && ended.is_err(),
        "a life holding an undelivered input never sleeps on it: {ended:?}");
}
