use super::*;

#[test]
fn outcome_keeps_every_publication_proof_and_only_omits_nonpublication_payloads() {
    let outcome: OutcomeProjectionConfig = serde_json::from_value(json!({"worker_interface":"worker","provider_interface":"provider",
        "relay_interface":"relay","experience_interface":"experience","input_routes":[],"tool_call_label":"tool.call",
        "tool_result_labels":["publish.result","search.result"]})).unwrap();
    let entry = |label: &str, seq: u64, payload: Value| SessionContextArtifact {label:label.into(),payload,
        source_artifact:serde_json::from_value(json!({"id":format!("entry:{seq}"),"content":vec![0u8;32]})).unwrap(),source_movement_seq:Some(seq)};
    let entries = vec![entry("tool.call",1,json!({"tool":"search","arguments":{"query":"prior knowledge"}})),
        entry("search.result",2,json!({"tool":"search","body":{"text":"x".repeat(200_000)}})),
        entry("tool.call",3,json!({"tool":"publish","correlation_id":"unreported","arguments":{"text":"exact note"}})),
        entry("publish.result",4,json!({"tool":"publish","correlation_id":"unreported","ok":false,"error":"exact failure"}))];
    let (evidence, omitted) = outcome_publication_evidence("publish", &outcome, 2, &entries).unwrap();
    assert_eq!(evidence, vec![json!(entries[2]),json!(entries[3])]);
    assert_eq!(omitted.iter().map(|entry|entry["source_movement_seq"].as_u64().unwrap()).collect::<Vec<_>>(),vec![1,2]);
    assert!(omitted.iter().all(|entry|entry.get("payload").is_none()));
    assert!(projection_bytes(&json!({"evidence":evidence,"omitted":omitted}),131072).is_ok());
    assert!(outcome_publication_evidence("publish", &outcome, 1, &entries).is_err());
    let mut malformed = entries[3].clone(); malformed.payload = json!({"tool":"other","body":"untrusted"});
    assert_eq!(outcome_publication_evidence("publish", &outcome, 1, &[malformed.clone()]).unwrap().0,vec![json!(malformed)]);
    malformed.label = "undeclared".into(); malformed.payload = json!({"tool":"publish"});
    assert!(outcome_publication_evidence("publish", &outcome, 1, &[malformed]).is_err());
    let mut large = entries[2].clone(); large.payload["arguments"]["text"] = json!("z".repeat(200_000));
    let (evidence, _) = outcome_publication_evidence("publish", &outcome, 1, &[large.clone()]).unwrap();
    assert_eq!(evidence,vec![json!(large)]);
    assert!(projection_bytes(&json!({"evidence":evidence}),131072).unwrap_err().contains("declared output bound"));
}

#[test]
#[ignore = "requires the explicitly selected read-only Fresh projection incidents"]
fn saved_projection_outcomes_fit_with_exact_publication_proof() {
    use octopus_run_authoring::{CheckedRunEvidence,CheckedRunEvidenceReader,FileCheckedRunReader};
    use std::{collections::BTreeMap,path::PathBuf,process::{Command,Stdio},io::Write};
    let home=PathBuf::from(std::env::var("OCTOPUS_PROJECTION_BOUND_HOME").unwrap());
    let scratch=tempfile::tempdir().unwrap();
    for (suffix,seq,old_bytes,new_bytes) in [("731743f3a8af1bdf375eda5b",50079,170891,26933),
        ("8e471c515103bc6a3dff8608",36297,145608,29095),("1d2e4bf9874f55f1b538830a",40374,156163,35010)] {
        let provider=octopus_descriptors::InterfaceId::new(format!("system_context_projection_{suffix}"));
        let dir=home.join("registry/interface").join(provider.as_str());
        let descriptor:octopus_interfaces::InterfaceDescriptor=serde_json::from_slice(&std::fs::read(dir.join("interface.json")).unwrap()).unwrap();
        let config:ContextProjectionConfig=serde_json::from_value(descriptor.interface_driver_config.as_ref().unwrap().private_config["context_projection"].clone()).unwrap();
        config.validate().unwrap();let outcome=config.outcome.as_ref().unwrap();
        let life:Value=serde_json::from_slice(&std::fs::read(dir.join("life.json")).unwrap()).unwrap();
        let (arrivals,_)=tokio::sync::mpsc::channel(1);
        let ctx=InterfaceLiftContext::unprivileged(provider,dir,Some(descriptor),scratch.path().join(suffix),home.join("ledger/movements.jsonl"),
            home.join("ledger/blobs"),home.join("ledger/ledger-index.sqlite"),BTreeMap::from([("run_records_root".into(),home.join("state/runs"))]),arrivals,Default::default())
            .with_host_io_limits(serde_json::from_value(life["processes"][0]["io_limits"].clone()).unwrap()).unwrap();
        let limits=ctx.required_host_io_limits().unwrap();
        let evidence=CheckedRunEvidence {movements_path:&ctx.movements_path,ledger_index_path:&ctx.ledger_index_path,blobs_dir:&ctx.blobs_dir,
            max_control_frame_bytes:limits.max_control_frame_bytes,max_manifest_bytes:limits.max_manifest_bytes,max_payload_bytes:limits.max_payload_bytes};
        let mut reader=FileCheckedRunReader::open(&evidence,Some(seq-1)).unwrap();
        let movement=reader.admitted_movement(seq).unwrap().unwrap();let artifact=movement.artifact.as_ref().unwrap();
        let (_,payload)=reader.artifact(artifact).unwrap();
        let (label,output)=project_outcome(&ctx,&config,outcome,artifact,&payload,&movement,None).unwrap();
        assert_eq!(label,OUTCOME_LABEL);
        let bytes=projection_bytes(&output,config.bounds.max_output_bytes).unwrap();assert_eq!(bytes.len(),new_bytes);
        let terminal:Value=serde_json::from_slice(&payload).unwrap();
        assert_eq!(output["outcome"],serde_json::from_str::<Value>(terminal["text"].as_str().unwrap()).unwrap());
        assert_eq!(output["source"]["terminal_artifact"],json!(artifact));
        let entries=resolve_session_snapshot_window(&ctx.blobs_dir,limits,artifact,&payload,output["source"]["terminal_source_movement_seq"].as_u64().unwrap(),
            &config.snapshot_labels,&octopus_descriptors::InterfaceId::new(&outcome.provider_interface),&BTreeSet::from([OUTPUT_LABEL.into()]),&config.bounds.ancestry).unwrap();
        let selected=entries.iter().find(|entry|entry.label==OUTPUT_LABEL).unwrap();
        assert_eq!(output["experience"],json!({"source_artifact":selected.source_artifact,"source_movement_seq":selected.source_movement_seq}));
        let mut prior=output.clone();let mut all=Vec::new();let mut omitted=Vec::new();
        for entry in &entries {
            if entry.label==outcome.tool_call_label || outcome.tool_result_labels.contains(&entry.label) {all.push(json!(entry));}
            else {omitted.push(json!({"label":entry.label,"source_artifact":entry.source_artifact,"source_movement_seq":entry.source_movement_seq}));}
        }
        prior["evidence"]=json!(all);prior["omission"]["unselected_entries"]=json!(omitted);
        assert_eq!(serde_json::to_vec(&prior).unwrap().len(),old_bytes);assert!(projection_bytes(&prior,config.bounds.max_output_bytes).is_err());
        let record_path=home.join("state/runs").join(format!("{}.json",config.projection_run));let record_bytes=std::fs::read(&record_path).unwrap();
        let record:Value=serde_json::from_slice(&record_bytes).unwrap();let mut nodes=vec![&record];let mut params=None;
        while let Some(node)=nodes.pop() {
            if node.get("verifier").and_then(Value::as_str)==Some("memory_capture_outcome") {params=Some(node["params"].clone());break;}
            match node {Value::Object(object)=>nodes.extend(object.values()),Value::Array(array)=>nodes.extend(array),_=>{}}
        }
        let path=scratch.path().join(format!("{seq}.json"));std::fs::write(&path,bytes).unwrap();
        let request=json!({"params":params.unwrap(),"payload_path":path,"artifact_label":label});
        let verifier=PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../../verifier/memory_capture_outcome/verify.py");
        let mut child=Command::new("python3").arg(verifier).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
        child.stdin.take().unwrap().write_all(&serde_json::to_vec(&request).unwrap()).unwrap();
        let result=child.wait_with_output().unwrap();assert!(result.status.success());
        let verdict:Value=serde_json::from_slice(&result.stdout).unwrap();assert_eq!(verdict["passed"],true,"{verdict}");
        assert_eq!(std::fs::read(record_path).unwrap(),record_bytes);
        println!("source={seq} before={old_bytes} after={new_bytes} bound={} publication_entries={} verifier=PASS",config.bounds.max_output_bytes,output["evidence"].as_array().unwrap().len());
    }
}
