use super::*;
use octopus_interfaces::InterfaceArtifactFile;

#[tokio::test]
async fn failed_source_projection_reaches_only_a_proven_standing_route() {
    let temp = tempfile::tempdir().unwrap();
    let ledger = temp.path().join("movements.jsonl");
    std::fs::write(&ledger, b"").unwrap();
    let descriptor: InterfaceDescriptor = serde_json::from_value(serde_json::json!({
        "id":"world_provider_fixture", "interface_type":"world", "role":"provider",
        "direction":"bidirectional", "source_names":["fixture"],
        "interface_driver_ref":"fixture",
    }))
    .unwrap();
    let (arrivals, mut receiver) = tokio::sync::mpsc::channel(1);
    let ctx = InterfaceLiftContext::unprivileged(
        descriptor.id.clone(),
        temp.path().into(),
        Some(descriptor.clone()),
        temp.path().join("var"),
        ledger,
        temp.path().join("blobs"),
        temp.path().join("index.sqlite"),
        Default::default(),
        arrivals,
        tokio_util::sync::CancellationToken::new(),
    )
    .with_host_io_limits(octopus_interface_transport::InterfaceHostIoLimits {
        max_control_frame_bytes: 65536,
        max_stderr_bytes: 1024,
        max_manifest_bytes: 65536,
        max_payload_bytes: 65536,
    })
    .unwrap();
    let input = InterfaceArtifactFile::write(temp.path().join("input"), "operator.input", br#"{}"#)
        .unwrap();
    let turn = QueuedTurn {
        seq: 7,
        envelope: InterfaceInvocationEnvelope {
            artifact: materialized_artifact_ref(&input).unwrap(),
            artifact_file: input,
            artifact_binding_payloads: Vec::new(),
            metadata: None,
        },
    };
    let session = ProviderHandoffSession::new(&ctx).unwrap();
    let run = RunId::new("run_fixture");
    let failure = record_invalid_provider_context(&ctx, &descriptor, &session, &run, &turn, "invalid fixture".into());
    let answer = async {
        let arrival = receiver.recv().await.expect("an unroutable failed source is recorded as the interface's own failure");
        assert_eq!(arrival.artifact.read_manifest().unwrap().label, "world_provider_fixture.failure");
        let payload: serde_json::Value = serde_json::from_slice(&arrival.artifact.read_payload_bytes().unwrap()).unwrap();
        assert_eq!(payload["source_movement_seq"], 7);
        assert_eq!(payload["failure_class"], "provider_source_unroutable");
        assert_eq!(payload["details"]["error"], "codex turn stopped because its admitted provider context is invalid (movement seq 7): invalid fixture");
        arrival.verdict_tx.unwrap().send(octopus_interfaces::DeliveryVerdict::Admitted { seq: 8 }).unwrap();
    };
    let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(5), async { tokio::join!(failure, answer) })
        .await.expect("the failure arrival is answered");
    result.unwrap();
    assert!(std::fs::read(&ctx.movements_path).unwrap().is_empty());
    assert!(!ctx.interface_var_dir().join("emitted").exists());
    assert!(!ctx.interface_var_dir().join("ledger-cursor.json").exists());
    let runs = temp.path().join("runs");
    std::fs::create_dir_all(&runs).unwrap();
    std::fs::write(runs.join("run_fixture.json"), serde_json::to_vec(&serde_json::json!({
        "id":"run_fixture","ee":"ee","worker":"worker","trace":"trace","status":"idle",
        "checkpoints":[{"id":"cp_standing","epoch":0,"run":"run_fixture","trace":"trace:run_fixture:cp_standing",
            "artifact_bindings":[],"outbound_interface_bindings":[],"required_verified_artifacts":[],
            "interface_binding":{"interface":"world_provider_fixture","config":{"artifact_policy":{"mode":"allow","names":["turn.completed","turn.error"],"namespaces":[]}}}}],
        "active_checkpoints":["cp_standing"],"interface_state":{},
    })).unwrap()).unwrap();
    let (arrivals, mut receiver) = tokio::sync::mpsc::channel(1);
    let routed = InterfaceLiftContext::unprivileged(
        descriptor.id.clone(), temp.path().into(), Some(descriptor.clone()), temp.path().join("routed"),
        temp.path().join("movements.jsonl"), temp.path().join("blobs"), temp.path().join("index.sqlite"),
        [("run_records_root".to_string(), runs)].into(), arrivals, tokio_util::sync::CancellationToken::new(),
    ).with_host_io_limits(octopus_interface_transport::InterfaceHostIoLimits {
        max_control_frame_bytes: 65536, max_stderr_bytes: 1024, max_manifest_bytes: 65536, max_payload_bytes: 65536,
    }).unwrap();
    let failure = tokio::spawn({
        let (routed, descriptor, run, turn) = (routed.clone(), descriptor.clone(), run.clone(), turn.clone());
        async move {
            let session = ProviderHandoffSession::new(&routed).unwrap();
            record_invalid_provider_context(&routed, &descriptor, &session, &run, &turn, "invalid fixture".into()).await
        }
    });
    let mut arrival = tokio::time::timeout(std::time::Duration::from_secs(5), receiver.recv()).await.unwrap().unwrap();
    assert_eq!(arrival.interface, descriptor.id);
    let body: serde_json::Value = serde_json::from_slice(&arrival.artifact.read_payload_bytes().unwrap()).unwrap();
    assert_eq!(arrival.artifact.read_manifest().unwrap().label, "turn.error");
    assert_eq!(body["message"], "codex turn stopped because its admitted provider context is invalid (movement seq 7): invalid fixture");
    assert_eq!(body["additional_details"]["source_movement_seq"], 7);
    arrival.verdict_tx.take().unwrap().send(octopus_ledger::DeliveryVerdict::Admitted { seq: 8 }).unwrap();
    failure.await.unwrap().expect("a run with a standing turn.error route receives its failure");
    for (label, independent, expected) in [
        ("client.surface.document", true, "turn.input.acknowledged"),
        ("client.action.result", false, "turn.error"),
        ("operator.turn.control", true, "turn.error"),
        ("client.surface.document", false, "turn.error"),
    ] {
        let file=InterfaceArtifactFile::write(temp.path().join("input"),label,b"{}").unwrap();
        let source=QueuedTurn { seq:9, envelope:InterfaceInvocationEnvelope {
            artifact:materialized_artifact_ref(&file).unwrap(),artifact_file:file,artifact_binding_payloads:Vec::new(),metadata:None,
        }};
        let result=materialize_consumed_source_outcome(&ctx,&descriptor,&source,"missing outcome",independent).unwrap();
        assert_eq!(result.read_manifest().unwrap().label,expected);
        let body:serde_json::Value=serde_json::from_slice(&result.read_payload_bytes().unwrap()).unwrap();
        if expected=="turn.input.acknowledged" {
            assert_eq!(body["source_artifact"],source.envelope.artifact.id.as_str());assert_eq!(body["source_movement_seq"],9);
            assert_eq!(body["status"],"acknowledged");assert!(body.get("message").is_none());
        } else { assert_eq!(body["message"],"missing outcome"); }
    }
}
