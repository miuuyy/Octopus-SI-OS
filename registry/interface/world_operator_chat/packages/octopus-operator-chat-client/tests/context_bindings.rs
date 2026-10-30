use octopus_descriptors::InterfaceId;
use octopus_operator_chat_client::{
    context_bindings::{declared_context_bindings, with_instruction_source_bindings},
    ArtifactBindingSeed,
};
use serde_json::json;

const MAX_PAYLOAD_BYTES: u64 = 1 << 20;

#[allow(dead_code)]
#[path = "../../../../../worker_environment_kind/system_room/packages/octopus-system-room-env/src/bound_surface.rs"]
mod bound_surface;
#[path = "support/composition_fixture.rs"]
mod composition;

#[test]
fn created_artifact_is_durable_and_readable_only_after_explicit_binding() {
    use octopus_interfaces::{ArtifactBindingPayload, GrantProjection, InterfaceArtifactFile};
    use octopus_ledger::ContentHash;
    use octopus_policy::ArtifactAccess;

    let root = tempfile::tempdir().unwrap();
    let cache = root.path().join("blobs");
    let mut grant = GrantProjection::default();
    grant.artifact_verbs.push("create".into());
    let payload = br#"{"mandate":{"goals":[]}}"#;
    let missing = bound_surface::InvocationArtifactSurface::new(
        &[], root.path(), grant.clone(), None, MAX_PAYLOAD_BYTES,
    ).unwrap();
    assert!(missing.append_impl("mandate", payload).is_err());
    let surface = bound_surface::InvocationArtifactSurface::new(
        &[], root.path(), grant.clone(), Some(&cache), MAX_PAYLOAD_BYTES,
    ).unwrap();
    let artifact = surface.append_impl("mandate", payload).unwrap();
    let manifest_path = cache.join(artifact.content.to_hex());
    let bytes = std::fs::read(&manifest_path).unwrap();
    assert_eq!(ContentHash::of_bytes(&bytes), artifact.content);
    let manifest = InterfaceArtifactFile::new(manifest_path).unwrap();
    assert_eq!(manifest.read_payload_bytes().unwrap(), payload);
    assert_eq!(std::fs::read(cache.join(ContentHash::of_bytes(payload).to_hex())).unwrap(), payload);
    assert!(surface.list_impl().unwrap().is_empty());
    assert_eq!(surface.read_impl(artifact.id.as_str()).unwrap(), None);
    grant.artifact_access.insert(artifact.id.to_string(), ArtifactAccess::read_only());
    let bound = bound_surface::InvocationArtifactSurface::new(
        &[ArtifactBindingPayload::new(artifact.clone(), payload.to_vec())],
        root.path(), grant, None, MAX_PAYLOAD_BYTES,
    ).unwrap();
    assert_eq!(bound.read_impl(artifact.id.as_str()).unwrap(), Some(payload.to_vec()));
}

#[test]
fn composed_tool_context_retains_selected_instruction_sources_without_history() {
    use octopus_interfaces::{ArtifactBindingPayload, GrantProjection};
    use octopus_ledger::{ArtifactRef, ContentHash};
    use octopus_operator_chat_client::run_composition::{plan_run_composition, RunCompositionPlan};
    use octopus_run_record::RunRecord;

    let root = tempfile::tempdir().unwrap();
    let request = composition::fixture(root.path());
    let bytes = b"# Architecture\nInterfaces exchange artifacts under formed policy.\n";
    let content = ContentHash::of_bytes(bytes);
    let source = ArtifactRef {
        id: format!("instruction.text:{}", content.to_hex()).into(),
        content,
    };
    composition::write(
        root.path(),
        "registry/artifact/role-system_worker_template/artifact.json",
        json!({"label":"context.reference","sources":[source]}),
    );
    composition::write(
        root.path(),
        "registry/artifact/role-system_worker_unselected/artifact.json",
        json!({"label":"context.reference","sources":[{"id":"unselected-source","content":content}]}),
    );
    let planned = plan_run_composition(&request).unwrap();
    let retained: RunCompositionPlan =
        serde_json::from_slice(&serde_json::to_vec(&planned).unwrap()).unwrap();
    assert_eq!(planned.run_write.content, retained.run_write.content);
    let coordinates = retained.descriptor_writes.iter().find(|write| write.tag.starts_with("artifact:artifact-run-authoring-coordinates-")).unwrap();
    let coordinates: serde_json::Value = serde_json::from_str(&coordinates.content).unwrap();
    assert_eq!(coordinates["authoring"]["registry_root"], json!(request.roots.registry_root));
    assert_eq!(coordinates["authoring"]["run_records_root"], json!(request.roots.run_records_root));
    let record = RunRecord::from_bytes(retained.run_write.content.as_bytes()).unwrap();
    let provider = InterfaceId::new(&retained.provider_interface_id);
    let door = octopus_run_authoring::bound_tool_door(
        &serde_json::from_str(&retained.run_write.content).unwrap(),
        &provider,
        &retained.authoring.capability_tools["file.write"],
    )
    .unwrap();
    let call = record
        .checkpoints
        .iter()
        .find(|cp| {
            cp.inbound_interface() == Some(&provider)
                && cp
                    .outbound_interface_bindings
                    .iter()
                    .any(|b| b.interface == door)
        })
        .unwrap();
    let templates = record.interface_state[&provider]["run_authoring_binding"]["checkpoints"]
        .as_array()
        .unwrap();
    let template = templates
        .iter()
        .find(|t| {
            t["role"] == "provider_tool_output"
                && t["outbound_interface_bindings"][0]["interface"] == door.as_str()
        })
        .unwrap();
    assert_eq!(template["include_context"], false);
    assert_eq!(template["static_artifact_bindings"], json!([source.id, coordinates["id"]]));
    assert!(call
        .artifact_bindings
        .iter()
        .any(|b| b.id == source.id && b.access.read));
    assert!(!retained.run_write.content.contains("unselected-source"));
    assert!(!retained.run_write.content.contains("private-transcript"));
    let path = request.roots.registry_root.join("interface/system_room/interface.json");
    let mut descriptor: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    descriptor["interface_driver_config"]["private_config"]["provider_tool_include_context"] = json!(true);
    std::fs::write(&path, serde_json::to_vec(&descriptor).unwrap()).unwrap();
    let defaults = plan_run_composition(&request).unwrap();
    let defaults: serde_json::Value = serde_json::from_str(&defaults.run_write.content).unwrap();
    assert!(defaults["interface_state"][provider.as_str()]["run_authoring_binding"]["checkpoints"].as_array().unwrap().iter().any(|row| row["role"] == "provider_tool_output" && row["include_context"] == true));
    descriptor["interface_driver_config"]["private_config"]["provider_tool_include_context"] = json!("true");
    std::fs::write(&path, serde_json::to_vec(&descriptor).unwrap()).unwrap();
    assert!(plan_run_composition(&request).unwrap_err().contains("must be a boolean"));
    let mut grant = GrantProjection::default();
    let payloads = call
        .artifact_bindings
        .iter()
        .map(|binding| {
            grant
                .artifact_access
                .insert(binding.id.to_string(), binding.access.clone());
            let payload = if binding.id == source.id {
                bytes.to_vec()
            } else {
                let path = request.roots.registry_root.join("artifact")
                    .join(binding.id.as_str()).join("artifact.json");
                match retained.descriptor_writes.iter().find(|write| write.path == path) {
                    Some(write) => write.content.as_bytes().to_vec(),
                    None => std::fs::read(path).unwrap(),
                }
            };
            ArtifactBindingPayload::new(
                ArtifactRef {
                    id: binding.id.clone(),
                    content: ContentHash::of_bytes(&payload),
                },
                payload,
            )
        })
        .collect::<Vec<_>>();
    let surface =
        bound_surface::InvocationArtifactSurface::new(&payloads, root.path(), grant.clone(), None, MAX_PAYLOAD_BYTES)
            .unwrap();
    assert_eq!(
        surface.read_impl(source.id.as_str()).unwrap(),
        Some(bytes.to_vec())
    );
    assert!(surface
        .list_impl()
        .unwrap()
        .iter()
        .any(|entry| entry.artifact.id == source.id));
    assert_eq!(surface.read_impl("unselected-source").unwrap(), None);
    let missing = payloads
        .iter()
        .filter(|p| p.artifact.id != source.id)
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(
        bound_surface::InvocationArtifactSurface::new(&missing, root.path(), grant.clone(), None, MAX_PAYLOAD_BYTES)
            .unwrap()
            .read_impl(source.id.as_str())
            .unwrap(),
        None
    );
    grant
        .artifact_access
        .get_mut(source.id.as_str())
        .unwrap()
        .read = false;
    assert!(
        bound_surface::InvocationArtifactSurface::new(&payloads, root.path(), grant, None, MAX_PAYLOAD_BYTES)
            .unwrap()
            .read_impl(source.id.as_str())
            .is_err()
    );
}

#[test]
fn only_selected_interface_dependencies_reach_new_context() {
    let root = tempfile::tempdir().unwrap();
    let write = |path: &str, value: serde_json::Value| {
        let path = root.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
    };
    for (id, artifact) in [("selected", "instructions"), ("unselected", "private")] {
        write(
            &format!("interface/{id}/interface.json"),
            json!({"id":id,"interface_type":"system","role":"system","direction":"bidirectional","source_names":[id],"interface_driver_ref":"fixture","interface_driver_config":{"private_config":{"context_artifact_bindings":[{"id":artifact}]}}}),
        );
    }
    let source = octopus_ledger::ArtifactRef {
        id: "file".into(),
        content: octopus_ledger::ContentHash::of_bytes(b"manifest"),
    };
    write(
        "artifact/instructions/artifact.json",
        json!({"label":"context.reference","sources":[source]}),
    );
    let selected = declared_context_bindings(
        root.path(),
        [InterfaceId::new("selected")],
        &[ArtifactBindingSeed {
            id: "instructions".into(),
        }],
    )
    .unwrap();
    assert_eq!(selected.len(), 1);
    let expanded = with_instruction_source_bindings(
        root.path(),
        selected.iter().map(|r| r.id.clone()),
        &selected,
    )
    .unwrap();
    assert_eq!(
        expanded.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
        ["instructions", "file"]
    );
    assert!(
        declared_context_bindings(root.path(), [], &[])
            .unwrap()
            .is_empty()
    );
    assert!(declared_context_bindings(root.path(), [InterfaceId::new("absent")], &[]).is_err());
    assert!(
        declared_context_bindings(
            root.path(),
            [],
            &[ArtifactBindingSeed {
                id: "../private".into()
            }]
        )
        .is_err()
    );
}
