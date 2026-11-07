use super::*;

#[test]
fn indeterminate_writer_requires_exact_failure_and_unadvanced_readback() {
    let writer = CheckedRunWriter {
        owner: InterfaceId::new("world_provider_test"), source: InterfaceId::new("system_authoring_test"),
        door: InterfaceId::new("system_room_test"), tool: "file.write.test".into(),
        call_label: "tool.call".into(), result_label: "file.write.test.result".into(),
    };
    let record = |id: &str| JsonFormat::Compact.encode(&json!({
        "id":"writer", "ee":"ee_test", "worker":"system_worker_test", "status":"idle", "trace":"trace:writer",
        "interface_state":{}, "active_checkpoints":[id], "checkpoints":[{
            "id":id, "run":"writer", "epoch":0, "trace":"trace:result", "artifact_bindings":[],
            "required_verified_artifacts":[], "interface_binding":{"interface":writer.door,"config":{"artifact_policy":{"mode":"all"}}},
            "outbound_interface_bindings":[{"interface":writer.owner,"config":{"artifact_policy":{"mode":"all"}}}]
        }]
    })).unwrap();
    let before = record("result_0");
    let after = record("result_1");
    let args = OrderedArguments { ordered:true, writes:vec![
        OrderedEntry::prepare(PathBuf::from("/runs/writer.json"), &before, &after, Some(JsonFormat::Compact)).unwrap(),
        OrderedEntry::Content(OrderedContentEntry {path:PathBuf::from("/runs/target.json"), content:"after".into(), expected_before_content:"before".into(), expected_before_hash:ContentHash::of_bytes(b"before").to_hex()})
    ]};
    let pinned = RunRecord::from_bytes(&before).unwrap();
    let cp = &pinned.checkpoints[0];
    let metadata = InvocationMetadata::new(Default::default())
        .with_runtime(octopus_ledger::RuntimeInvocationMetadata {run:pinned.id.clone(), ee:pinned.ee.clone(), worker:pinned.worker.clone(), checkpoint:cp.id.clone(), interface:writer.owner.clone(), epoch:0, run_record_hash:Some(ContentHash::of_bytes(&before).to_hex())})
        .with_outbound_interface_bindings(vec![writer.owner.clone()])
        .with_checkpoint_binding_policy(CheckpointBindingPolicySnapshot {inbound:cp.interface_binding.clone().unwrap(), outbound:cp.outbound_interface_bindings.clone()})
        .with_checkpoint_admission(octopus_ledger::CheckpointAdmissionMetadata {artifact_label:writer.result_label.clone(), required_artifact_labels:vec![], complete:true});
    let result: ToolResultArtifact = serde_json::from_value(json!({"tool":writer.tool,"label":writer.result_label,"ok":false,
        "error":{"code":"tool_effect_indeterminate","message":"disk full","details":{"source_movement_seq":10,"failure_details":{
            "source_movement_seq":10,"tool":writer.tool,"effect_state":"in_flight_outcome_unknown"}}}
    })).unwrap();
    assert_eq!(args.checked_result(&writer, &metadata, &before, &result, 1048576, 10).unwrap(), OrderedTargetEffect::UncertainWriter);
    assert!(args.checked_result(&writer, &metadata, &before, &result, 1048576, 11).is_err());
    assert!(args.checked_result(&writer, &metadata, &after, &result, 1048576, 10).is_err());
    let refused: ToolResultArtifact = serde_json::from_value(json!({"tool":writer.tool,"label":writer.result_label,"ok":false,
        "error":{"code":"file_write_evidence_failed","message":"configured root cannot be canonicalized"}})).unwrap();
    assert_eq!(args.checked_result(&writer, &metadata, &before, &refused, 1048576, 10).unwrap(), OrderedTargetEffect::UncertainWriter);
    assert!(definite_refusal(&refused).is_some() && definite_refusal(&result).is_none());
    let incomplete: ToolResultArtifact = serde_json::from_value(json!({"tool":writer.tool,"label":writer.result_label,"ok":false,
        "error":{"code":"file_write_incomplete","message":"partial"}})).unwrap();
    assert!(args.checked_result(&writer, &metadata, &before, &incomplete, 1048576, 10).is_err());
    let receipt = |path: &str, before: &[u8], after: &[u8], outcome: &str| json!({"path":path,"before_hash":ContentHash::of_bytes(before).to_hex(),
        "after_hash":ContentHash::of_bytes(after).to_hex(),"bytes":after.len(),"outcome":outcome,"observed_hash":null,"error":null});
    let stopped = |first: &str, second: &str| -> ToolResultArtifact { serde_json::from_value(json!({"tool":writer.tool,"label":writer.result_label,"ok":false,
        "error":{"code":"file_write_incomplete","message":"no space left on device","details":{"file_write":{"result":"ordered_file_writes","writes":[
            receipt("/runs/writer.json", &before, &after, first), receipt("/runs/target.json", b"before", b"after", second)]}}}})).unwrap() };
    assert_eq!(args.checked_result(&writer, &metadata, &before, &stopped("uncertain", "not_attempted"), 1048576, 10).unwrap(), OrderedTargetEffect::UncertainWriter);
    assert!(unadvanced_refusal(&stopped("refused", "not_attempted")).is_some());
    assert!(args.checked_result(&writer, &metadata, &after, &stopped("uncertain", "not_attempted"), 1048576, 10).is_err());
    assert!(args.checked_result(&writer, &metadata, &before, &stopped("uncertain", "uncertain"), 1048576, 10).is_err());
}
