use super::*;
use std::collections::BTreeMap;
use octopus_descriptors::{ArtifactId, CheckpointId, EeId, InterfaceId, RunId};
use octopus_interfaces::{GrantProjection, InterfaceArtifactFile, InvocationMetadata, RuntimeInvocationMetadata};
use octopus_ledger::ArtifactRef;

fn invoke(root: &Path, source: &ArtifactBindingPayload, run: &str, ee: &str) -> InterfaceInvocationEnvelope {
    let payload = serde_json::to_vec(&json!({"tool":"context.discover","arguments":{}})).unwrap();
    let file = InterfaceArtifactFile::write(root.join(run), "tool.call", &payload).unwrap();
    let artifact = ArtifactRef { id:ArtifactId::new(format!("tool.call:{}",run)),
        content:ContentHash::of_bytes(&std::fs::read(file.manifest_path()).unwrap()) };
    let metadata = InvocationMetadata::new(GrantProjection {
        tools:vec!["context.discover".into(),"context.read".into(),"context.prepare_publication".into(),"file.write".into()],
        worker_environment_kinds:vec!["system_room".into()],
        artifact_access:serde_json::from_value(json!({source.artifact.id.as_str():{"read":true,"change":false,"create":false,"delete":false}})).unwrap(),
        ..Default::default()
    }).with_runtime(RuntimeInvocationMetadata { run:RunId::new(run),ee:EeId::new(ee),worker:InterfaceId::new("system_worker_library_test"),
        checkpoint:CheckpointId::new("input"), interface:InterfaceId::new("system_context"),epoch:0,run_record_hash:None });
    InterfaceInvocationEnvelope { artifact,artifact_file:file,artifact_binding_payloads:vec![source.clone()],
        metadata:Some(InvocationMetadata {artifact_binding_refs:vec![source.artifact.clone()], ..metadata}) }
}

fn call(input: &InterfaceInvocationEnvelope, root: &Path, operation: &str, arguments: Value) -> Result<Value,String> {
    let id = match operation {
        "collection_discover" => "context.discover", "collection_read" => "context.read",
        "collection_prepare" => "context.prepare_publication", _ => unreachable!(),
    };
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../..");
    let descriptor = octopus_tool_registry::load_tool_descriptor_from_dir(&repository.join("registry/tool"),id).unwrap().unwrap();
    octopus_tool_registry::ToolArgumentValidator::compile(&descriptor.args_schema).unwrap().validate(&arguments).map_err(|error|error.to_string())?;
    execute_inner(input,&ToolCall { tool_id:"context.test".into(),arguments,handler_config:json!({"operation":operation}) },Some(root),1024*1024)
}

fn library(root: &Path) -> (std::path::PathBuf, ArtifactBindingPayload) {
    let artifact_root = root.join("artifact");
    let descriptor = json!({"schema_version":1,"label":"context.collection","id":"artifact-project-library","description":"Project library",
        "directory":"entries","limits":{"max_entries":32,"max_entry_bytes":65536,"max_page_bytes":16384,"max_scan_bytes":1048576}});
    std::fs::create_dir_all(artifact_root.join("artifact-project-library")).unwrap();
    std::fs::write(artifact_root.join("artifact-project-library/artifact.json"),serde_json::to_vec_pretty(&descriptor).unwrap()).unwrap();
    let source = ArtifactBindingPayload::new(ArtifactRef {id:ArtifactId::new("artifact-project-library"),content:ContentHash::of_canonical_json(&descriptor)},
        octopus_descriptors::canonical_bytes(&descriptor));
    (artifact_root, source)
}

#[test]
fn a_collection_source_and_pin_from_any_context_result_are_accepted_verbatim() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    let (artifact_root, source) = library(&root);
    let entry = json!({"schema_version":1,"id":"guide","title":"Guide","description":"Reusable guide",
        "applicability":"Use for this project","state":"active","scope":{},"content":{"text":"Retained instructions"},"selection":{}});
    std::fs::create_dir_all(artifact_root.join("artifact-project-library/entries")).unwrap();
    std::fs::write(artifact_root.join("artifact-project-library/entries/guide.json"),serde_json::to_vec(&entry).unwrap()).unwrap();
    let input = invoke(&root,&source,"run_read","ee_project");
    let listed = call(&input,&artifact_root,"collection_discover",json!({})).unwrap()["sources"][0]["source"].clone();
    let page = call(&input,&artifact_root,"collection_discover",json!({"source":listed})).unwrap();
    assert_eq!(page["source"],listed);
    let pin = page["entries"][0]["pin"].clone();
    assert_eq!(pin["source"],listed);
    let read = call(&input,&artifact_root,"collection_read",pin.clone()).unwrap();
    assert_eq!(read["entry"],entry);
    assert_eq!(call(&input,&artifact_root,"collection_read",read["pin"].clone()).unwrap(),read);
    let mut stale = pin.clone();
    stale["source"]["content"] = json!(ContentHash::of_bytes(b"another revision"));
    assert!(call(&input,&artifact_root,"collection_read",stale).unwrap_err().contains("different content than requested"));
    let mut bare = pin;
    bare["source"] = json!(source.artifact.id);
    assert!(call(&input,&artifact_root,"collection_read",bare).is_err());
}

#[test]
fn publication_reaches_an_independent_run_through_existing_writer_and_bound_collection() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    let (artifact_root, source) = library(&root);
    let first = invoke(&root,&source,"run_publish","ee_project");
    let second = invoke(&root,&source,"run_later","ee_project");
    let other = invoke(&root,&source,"run_other","ee_other");
    let entry = json!({"schema_version":1,"id":"build-release","title":"Build release","description":"Procedure learned from a successful build",
        "applicability":"Use when preparing this project's release","state":"active","scope":{"ee_ids":["ee_project"]},
        "content":{"text":"Run the declared build checks before packaging; retain the output receipt."},
        "selection":{"selected_tools":[{"tool":"file.read","interface":"system_room","worker_environment_kind":"system_room"}]}});
    let prepared = call(&first,&artifact_root,"collection_prepare",json!({"source":source.artifact,"entry":entry,"expected_revision":null})).unwrap();
    assert_eq!(prepared["status"],"prepared");
    assert!(!artifact_root.join("artifact-project-library/entries/build-release.json").exists());
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../..");
    let writer = octopus_tool_registry::load_tool_descriptor_from_dir(&repository.join("registry/tool"),"file.write").unwrap().unwrap();
    let validator = octopus_tool_registry::ToolArgumentValidator::compile(&writer.args_schema).unwrap();
    let commit = |arguments: &Value, key: &str| {
        validator.validate(arguments).unwrap();
        let body = serde_json::to_vec(&json!({"tool":"file.write","arguments":arguments})).unwrap();
        let file = InterfaceArtifactFile::write(root.join(key),"tool.call",&body).unwrap();
        let mut effect = first.clone();
        effect.artifact = ArtifactRef {id:ArtifactId::new(format!("tool.call:{key}")),content:ContentHash::of_bytes(&std::fs::read(file.manifest_path()).unwrap())};
        effect.artifact_file = file;
        let roots = BTreeMap::from([("registry_artifact_root".into(),artifact_root.clone())]);
        let receipt = crate::execute_file_write_result(&effect,arguments,&["system_room".into()],&roots,&root.join("receipts"),1024*1024).unwrap();
        assert_eq!(crate::execute_file_write_result(&effect,arguments,&["system_room".into()],&roots,&root.join("receipts"),1024*1024).unwrap(),receipt);
    };
    commit(&prepared["file_write"],"publish");
    let sources = call(&second,&artifact_root,"collection_discover",json!({})).unwrap();
    assert_eq!(sources["sources"].as_array().unwrap().len(),1);
    let found = call(&second,&artifact_root,"collection_discover",json!({"source":source.artifact})).unwrap();
    assert_eq!(found["entries"].as_array().unwrap().len(),1);
    assert!(found["entries"][0].get("content").is_none());
    let pin = &found["entries"][0]["pin"];
    let arguments = json!({"source":source.artifact,"entry_id":pin["entry_id"],"revision":pin["revision"]});
    let read = call(&second,&artifact_root,"collection_read",arguments.clone()).unwrap();
    assert_eq!(read["entry"]["content"]["text"],entry["content"]["text"]);
    assert_eq!(read["entry"]["selection"],entry["selection"]);
    assert!(!second.metadata.as_ref().unwrap().grant.tools.contains(&"file.read".into()));
    let mut corpus = second.clone();
    let history = serde_json::to_vec(&json!({"label":"history","body":"x".repeat(32768)})).unwrap();
    let history_path = root.join("history");
    std::fs::write(&history_path, &history).unwrap();
    for index in 0..71 {
        let reference = ArtifactRef { id:ArtifactId::new(format!("history:{index}")), content:ContentHash::of_bytes(&history) };
        corpus.artifact_binding_payloads.push(ArtifactBindingPayload::from_pinned_file(reference.clone(), history_path.clone(), ContentHash::of_bytes(&history), 65536).unwrap());
        let metadata = corpus.metadata.as_mut().unwrap();
        let access = metadata.grant.artifact_access[source.artifact.id.as_str()].clone();
        metadata.grant.artifact_access.insert(reference.id.to_string(), access);
        metadata.artifact_binding_refs.push(reference);
    }
    assert_eq!(corpus.artifact_binding_payloads.len(),72);
    assert_eq!(call(&corpus,&artifact_root,"collection_discover",json!({"source":source.artifact})).unwrap(),found);
    assert_eq!(call(&corpus,&artifact_root,"collection_discover",json!({})).unwrap(),sources);
    let surface = crate::bound_surface::InvocationArtifactSurface::new(&corpus.artifact_binding_payloads, &root.join("emitted"), corpus.grant().unwrap().clone(), None, 65536).unwrap();
    assert_eq!(surface.read_impl("history:70").unwrap().unwrap(),history);
    assert!(surface.list_impl().is_err());
    std::fs::write(&history_path, vec![b'x'; history.len()]).unwrap();
    assert!(call(&corpus,&artifact_root,"collection_discover",json!({})).is_err());
    assert!(surface.read_impl("history:0").is_err());
    assert_eq!(call(&corpus,&artifact_root,"collection_discover",json!({"source":source.artifact})).unwrap(),found);
    assert_eq!(call(&other,&artifact_root,"collection_discover",json!({"source":source.artifact})).unwrap()["entries"],json!([]));
    assert!(call(&other,&artifact_root,"collection_read",arguments.clone()).is_err());
    let mut denied = second.clone();
    denied.metadata.as_mut().unwrap().grant.artifact_access.clear();
    assert_eq!(call(&denied,&artifact_root,"collection_discover",json!({})).unwrap()["sources"],json!([]));
    assert!(call(&denied,&artifact_root,"collection_read",arguments.clone()).is_err());
    let mut forged = second.clone();
    forged.metadata.as_mut().unwrap().artifact_binding_refs[0].content = ContentHash::of_bytes(b"unattested");
    assert!(call(&forged,&artifact_root,"collection_read",arguments.clone()).is_err());
    let mut withdrawn = entry;
    withdrawn["state"] = json!("withdrawn");
    assert!(call(&other,&artifact_root,"collection_prepare",json!({"source":source.artifact,"entry":withdrawn,"expected_revision":pin["revision"]})).is_err());
    let prepared = call(&first,&artifact_root,"collection_prepare",json!({"source":source.artifact,"entry":withdrawn,"expected_revision":pin["revision"]})).unwrap();
    commit(&prepared["file_write"],"withdraw");
    assert_eq!(call(&second,&artifact_root,"collection_discover",json!({"source":source.artifact})).unwrap()["entries"],json!([]));
    assert!(call(&second,&artifact_root,"collection_read",arguments).is_err());
    assert_eq!(read["entry"]["state"],"active");
}
