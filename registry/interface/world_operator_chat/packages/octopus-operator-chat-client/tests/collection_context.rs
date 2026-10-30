use octopus_context_collections::{
    BoundCollection, CatalogueLimits, CollectionCatalogue, ListRequest, ScopeContext,
};
use octopus_ledger::{ArtifactRef, ContentHash};
use octopus_operator_chat_client::{
    ArtifactBindingSeed, OperatorChatPlanRequest,
    collection_context::{CaptureAuthority, capture},
    plan_operator_chat_topology,
    run_composition::*,
};
use octopus_policy::{ArtifactAccess, ArtifactVerb, CapSet, ContextGrant};
use octopus_run_record::RunRecord;
use serde_json::{Value, json};
use std::path::Path;

#[allow(dead_code)]
#[path = "support/composition_fixture.rs"]
mod fixture;

fn library(root: &Path) -> (ArtifactRef, Value) {
    let source = json!({"id":"library","label":"context.collection","description":"Published knowledge","schema_version":1,
        "directory":"entries","limits":{"max_entries":128,"max_entry_bytes":131072,"max_page_bytes":4096,"max_scan_bytes":1048576}});
    fixture::write(
        root,
        "registry/artifact/library/artifact.json",
        source.clone(),
    );
    let entry = json!({"schema_version":1,"id":"useful","title":"Check evidence","description":"Read the exact receipt before reporting success.",
        "applicability":"When reporting a write","state":"active","scope":{"ee_ids":["ee_existence"]},
        "content":{"text":"DETAIL BODY MUST NOT INLINE"},"selection":{"context_artifact_bindings":[{"id":"NEVER_ACTIVATE"}]},"requires":[]});
    fixture::write(
        root,
        "registry/artifact/library/entries/useful.json",
        entry.clone(),
    );
    (
        ArtifactRef {
            id: "library".into(),
            content: ContentHash::of_canonical_json(&source),
        },
        entry,
    )
}

fn request(root: &Path) -> RunCompositionRequest {
    let mut request = fixture::fixture(root);
    let (source, _) = library(root);
    request.context_artifact_bindings.push(ArtifactBindingSeed {
        id: "library".into(),
    });
    request
        .metadata
        .grant
        .artifact_access
        .insert("library".into(), ArtifactAccess::read_only());
    request.metadata.artifact_binding_refs.push(source);
    request
}

fn catalogue(plan: &RunCompositionPlan) -> Value {
    plan.descriptor_writes
        .iter()
        .filter_map(|write| serde_json::from_str::<Value>(&write.content).ok())
        .find(|value| value.get("collection_catalogue").is_some())
        .unwrap()
}

fn scope() -> ScopeContext {
    ScopeContext {
        ee_id: "ee_existence".into(),
        worker_interface: "system_worker_private_first".into(),
    }
}

fn grant() -> ContextGrant {
    ContextGrant {
        artifact_verbs: CapSet::from_allowed([ArtifactVerb::Read]),
        artifact_access: [("library".into(), ArtifactAccess::read_only())].into(),
        ..Default::default()
    }
}

#[test]
fn selected_client_context_defaults_reach_native_catalogue_without_tool_selection() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let mut request = fixture::client_fixture(&root);
    let (source, _) = library(&root);
    let path = "registry/interface/world_operator_chat/client.json";
    let mut config: Value = serde_json::from_slice(&std::fs::read(root.join(path)).unwrap()).unwrap();
    config["run"]["context_artifact_bindings"] = json!([{"id":"library"}]);
    fixture::write(&root, path, config);
    request.metadata.grant.artifact_access.insert("library".into(), ArtifactAccess::read_only());
    request.metadata.artifact_binding_refs.push(source);
    let plan = plan_run_composition(&request).unwrap();
    let value = catalogue(&plan);
    assert_eq!(value["collection_catalogue"]["sources"][0]["entries"][0]["pin"]["entry_id"], "useful");
    assert!(request.context_artifact_bindings.is_empty());
    assert!(!request.selected_tools.iter().any(|tool| tool.tool.starts_with("context.")));
}

#[test]
fn new_run_inlines_exact_scoped_metadata_without_bodies_or_activation() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let mut request = request(&root);
    let unrelated = json!({"label":"context.reference","text":"Ordinary instructions without a collection identity".repeat(6000)});
    fixture::write(
        &root,
        "registry/artifact/unrelated/artifact.json",
        unrelated.clone(),
    );
    request.context_artifact_bindings.push(ArtifactBindingSeed {
        id: "unrelated".into(),
    });
    request
        .metadata
        .grant
        .artifact_access
        .insert("unrelated".into(), ArtifactAccess::read_only());
    request.metadata.artifact_binding_refs.push(ArtifactRef {
        id: "unrelated".into(),
        content: ContentHash::of_canonical_json(&unrelated),
    });
    let (_, mut entry) = library(&root);
    entry["selection"]["context_artifact_bindings"] = json!(
        (0..1500)
            .map(|_| json!({"id":"NEVER_ACTIVATE"}))
            .collect::<Vec<_>>()
    );
    fixture::write(
        &root,
        "registry/artifact/library/entries/useful.json",
        entry.clone(),
    );
    for (id, scope, state) in [
        ("hidden", "other_ee", "active"),
        ("withdrawn", "ee_existence", "withdrawn"),
    ] {
        let mut hidden = entry.clone();
        hidden["id"] = json!(id);
        hidden["scope"] = json!({"ee_ids":[scope]});
        hidden["state"] = json!(state);
        fixture::write(
            &root,
            &format!("registry/artifact/library/entries/{id}.json"),
            hidden,
        );
    }
    let source_bytes = std::fs::read(root.join("registry/artifact/library/artifact.json")).unwrap();
    let plan = plan_run_composition(&request).unwrap();
    let descriptor = catalogue(&plan);
    let projected = CollectionCatalogue::validate_descriptor(&descriptor).unwrap();
    assert_eq!(projected.scope, scope());
    assert_eq!(projected.sources[0].entries.len(), 1);
    assert_eq!(
        projected.sources[0].entries[0].pin.revision,
        ContentHash::of_canonical_json(&entry)
    );
    assert!(!descriptor.to_string().contains("DETAIL BODY"));
    assert!(!descriptor.to_string().contains("NEVER_ACTIVATE"));
    let snapshot = ArtifactRef {
        id: descriptor["id"].as_str().unwrap().into(),
        content: ContentHash::of_canonical_json(&descriptor),
    };
    let record = RunRecord::from_bytes(plan.run_write.content.as_bytes()).unwrap();
    assert_eq!(
        record.interface_state[&plan.provider_interface_id.clone().into()]["collection_catalogue_adoption"]
            ["snapshot"],
        json!(snapshot)
    );
    assert!(
        octopus_run_authoring::provider_context_bindings(
            &record,
            &plan.provider_interface_id.clone().into()
        )
        .unwrap()
        .iter()
        .any(|binding| binding.id == snapshot.id && binding.access.read)
    );
    let provider: Value = serde_json::from_str(
        &plan
            .descriptor_writes
            .iter()
            .find(|write| write.tag == format!("interface:{}", plan.provider_interface_id))
            .unwrap()
            .content,
    )
    .unwrap();
    assert!(
        provider["interface_driver_config"]["private_config"]["instruction_artifacts"]
            .as_array()
            .unwrap()
            .contains(&json!(snapshot.id))
    );
    assert!(
        !plan
            .authoring
            .capability_tools
            .contains_key("NEVER_ACTIVATE")
    );
    assert_eq!(
        std::fs::read(root.join("registry/artifact/library/artifact.json")).unwrap(),
        source_bytes
    );
}

#[test]
fn compact_projection_is_bounded_and_cursor_continues_the_native_collection() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let (source, entry) = library(&root);
    for number in 0..40 {
        let mut entry = entry.clone();
        entry["id"] = json!(format!("entry-{number:02}"));
        fixture::write(
            &root,
            &format!("registry/artifact/library/entries/entry-{number:02}.json"),
            entry,
        );
    }
    let bindings = [ArtifactBindingSeed {
        id: "library".into(),
    }];
    let limits = CatalogueLimits {
        max_snapshot_bytes: 6000,
        ..Default::default()
    };
    let value = capture(
        &root.join("registry"),
        &scope(),
        &bindings,
        &grant(),
        CaptureAuthority::Operator,
        limits,
    )
    .unwrap()
    .unwrap();
    assert!(serde_json::to_vec(&value).unwrap().len() <= 6000);
    let projected = CollectionCatalogue::validate_descriptor(&value).unwrap();
    let page = &projected.sources[0];
    assert!(page.has_more);
    let payload: Value = serde_json::from_slice(
        &std::fs::read(root.join("registry/artifact/library/artifact.json")).unwrap(),
    )
    .unwrap();
    let collection =
        BoundCollection::open(&root.join("registry/artifact"), &source, &payload).unwrap();
    let next = collection
        .list(
            &scope(),
            &ListRequest {
                query: None,
                limit: 1,
                cursor: page.cursor.clone(),
            },
        )
        .unwrap();
    assert!(next.entries[0].pin.entry_id > page.entries.last().unwrap().pin.entry_id);
    let limits = CatalogueLimits {
        max_scan_bytes: 1024,
        ..Default::default()
    };
    assert!(
        capture(
            &root.join("registry"),
            &scope(),
            &bindings,
            &grant(),
            CaptureAuthority::Operator,
            limits
        )
        .is_err()
    );
}

#[test]
fn native_authority_and_target_scope_are_independent_and_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let mut request = request(&root);
    request.metadata.grant.artifact_access.clear();
    std::fs::write(
        root.join("registry/artifact/library/entries/useful.json"),
        b"invalid entry",
    )
    .unwrap();
    let projected = CollectionCatalogue::validate_descriptor(&catalogue(
        &plan_run_composition(&request).unwrap(),
    ))
    .unwrap();
    assert!(projected.sources.is_empty());
    assert_eq!(projected.uninspected_bindings[0].id, "library");
    request
        .metadata
        .grant
        .artifact_access
        .insert("library".into(), ArtifactAccess::read_only());
    assert!(
        plan_run_composition(&request)
            .unwrap_err()
            .contains("integrity")
    );
    let (_, mut entry) = library(&root);
    entry["scope"] = json!({"worker_interfaces":[scope().worker_interface]});
    fixture::write(
        &root,
        "registry/artifact/library/entries/useful.json",
        entry,
    );
    let accepted = catalogue(&plan_run_composition(&request).unwrap());
    let projected = CollectionCatalogue::validate_descriptor(&accepted).unwrap();
    assert!(projected.sources[0].entries.is_empty());
    assert!(projected.sources[0].scope_limited);
    assert!(projected.sources[0].cursor.is_none());
    let path = root.join("registry/artifact/library/entries/useful.json");
    let mut hidden: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    hidden["description"] = json!("Hidden revision changed");
    hidden["state"] = json!("withdrawn");
    std::fs::write(&path, serde_json::to_vec(&hidden).unwrap()).unwrap();
    assert_eq!(
        catalogue(&plan_run_composition(&request).unwrap()),
        accepted
    );
    std::fs::remove_file(path).unwrap();
    assert_eq!(
        catalogue(&plan_run_composition(&request).unwrap()),
        accepted
    );
    request.metadata.artifact_binding_refs[0].content = ContentHash::of_bytes(b"stale");
    assert!(
        plan_run_composition(&request)
            .unwrap_err()
            .contains("admitted ref")
    );
    request.metadata.artifact_binding_refs.clear();
    let file = "registry/ee_template/existence/ee_template.json";
    let mut ee: Value = serde_json::from_slice(&std::fs::read(root.join(file)).unwrap()).unwrap();
    ee["allowed_artifact_verbs"] = json!([]);
    fixture::write(&root, file, ee);
    assert!(
        plan_run_composition(&request)
            .unwrap_err()
            .contains("target formation denies read")
    );
}

#[test]
fn existing_run_keeps_its_accepted_catalogue_after_library_change_and_restart() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let composed = fixture::client_fixture(&root);
    let mut child = composed.clone();
    let (source, mut entry) = library(&root);
    let file = "registry/interface/world_operator_chat/client.json";
    let mut client: Value =
        serde_json::from_slice(&std::fs::read(root.join(file)).unwrap()).unwrap();
    client["run"]["context_artifact_bindings"] = json!([]);
    fixture::write(&root, file, client);
    for (file, interfaces, tools) in [
        (
            "registry/ee_template/existence/ee_template.json",
            "/allowed_interfaces",
            "/allowed_tools",
        ),
        (
            "ee/ee_existence.json",
            "/policy_narrowing/allowed_interfaces",
            "/policy_narrowing/allowed_tools",
        ),
    ] {
        let mut value: Value =
            serde_json::from_slice(&std::fs::read(root.join(file)).unwrap()).unwrap();
        value
            .pointer_mut(interfaces)
            .unwrap()
            .as_array_mut()
            .unwrap()
            .extend(
                [
                    "system_interface_authoring",
                    "system_room_authoring",
                    "system_room",
                    "system_worker_template",
                    "world_operator_chat",
                    "world_provider_fixture",
                ]
                .map(|id| json!(format!("{id}_catalogue"))),
            );
        value
            .pointer_mut(tools)
            .unwrap()
            .as_array_mut()
            .unwrap()
            .extend(
                [
                    "file.write__authoring_catalogue",
                    "file.write__catalogue",
                    "artifact.send__catalogue",
                ]
                .map(|id| json!(id)),
            );
        fixture::write(&root, file, value);
    }
    let mut request: OperatorChatPlanRequest = serde_json::from_value(json!({"registry_root":composed.roots.registry_root,"ee_instances_root":composed.roots.ee_instances_root,
        "run_records_root":composed.roots.run_records_root,"interface_private_state_root":composed.roots.interface_private_state_root,"write_hash_algorithm":"blake3","chat_id":"catalogue",
        "provider_interface_id":"world_provider_fixture","model":"fixture-a","reasoning_effort":"xhigh","ee_id":"ee_existence","worker_interface_id":"system_worker_template",
        "client_surface_role":"task","cycle":"first","selected_tools":composed.selected_tools,"context_artifact_bindings":[{"id":"library"}],"conversation":{"ledger_offset":0}})).unwrap();
    let accepted = plan_operator_chat_topology(&request).unwrap();
    for write in accepted.interface_writes.iter().chain(&accepted.run_writes) {
        std::fs::create_dir_all(write.path.parent().unwrap()).unwrap();
        std::fs::write(&write.path, &write.content).unwrap();
    }
    let before: Value = serde_json::from_str(&accepted.run_record_content).unwrap();
    let adopted =
        before["interface_state"][&accepted.model_interface_id]["collection_catalogue_adoption"]
            .clone();
    entry["description"] = json!("Later knowledge");
    fixture::write(
        &root,
        "registry/artifact/library/entries/useful.json",
        entry,
    );
    request.existing_run_record = Some(before.clone());
    request.retain_current_capabilities = true;
    request.cycle = "next".into();
    let next = plan_operator_chat_topology(&request).unwrap();
    let after: Value = serde_json::from_str(&next.run_record_content).unwrap();
    assert_eq!(
        after["interface_state"][&accepted.model_interface_id]["collection_catalogue_adoption"],
        adopted
    );
    assert!(!next.interface_writes.iter().any(|write| {
        write
            .tag
            .starts_with("artifact:artifact-collection-catalogue-")
    }));
    assert_eq!(
        octopus_run_authoring::provider_context_bindings(
            &RunRecord::from_bytes(accepted.run_record_content.as_bytes()).unwrap(),
            &accepted.model_interface_id.clone().into()
        )
        .unwrap(),
        octopus_run_authoring::provider_context_bindings(
            &RunRecord::from_bytes(next.run_record_content.as_bytes()).unwrap(),
            &accepted.model_interface_id.clone().into()
        )
        .unwrap()
    );
    request.retain_current_capabilities = false;
    let configured = plan_operator_chat_topology(&request).unwrap();
    let configured: Value = serde_json::from_str(&configured.run_record_content).unwrap();
    assert_eq!(
        configured["interface_state"][&accepted.model_interface_id]["collection_catalogue_adoption"],
        adopted
    );
    let mut wrong_capture = before.clone();
    wrong_capture["interface_state"][&accepted.model_interface_id]["collection_catalogue_adoption"]
        ["scope"]["ee_id"] = json!("ee_uncaptured");
    request.existing_run_record = Some(wrong_capture);
    assert!(
        plan_operator_chat_topology(&request)
            .unwrap_err()
            .contains("different capture scope")
    );
    request.existing_run_record = Some(before.clone());
    request.retain_current_capabilities = true;
    let ee_file = "ee/ee_existence.json";
    let current_ee: Value =
        serde_json::from_slice(&std::fs::read(root.join(ee_file)).unwrap()).unwrap();
    let mut denied_ee = current_ee.clone();
    denied_ee["policy_narrowing"]["allowed_artifact_verbs"] = json!([]);
    fixture::write(&root, ee_file, denied_ee);
    assert!(
        plan_operator_chat_topology(&request)
            .unwrap_err()
            .contains("snapshot is not readable by its target")
    );
    fixture::write(&root, ee_file, current_ee);
    request.retain_current_capabilities = false;
    child.provider_interface_id = accepted.model_interface_id.clone();
    child.context_artifact_bindings = vec![ArtifactBindingSeed {
        id: "library".into(),
    }];
    child
        .metadata
        .grant
        .artifact_access
        .insert("library".into(), ArtifactAccess::read_only());
    child.metadata.artifact_binding_refs = vec![source];
    let file = "registry/interface/system_worker_template/policy.json";
    let mut policy: Value =
        serde_json::from_slice(&std::fs::read(root.join(file)).unwrap()).unwrap();
    policy["ceiling"]["interfaces"]
        .as_array_mut()
        .unwrap()
        .push(json!(accepted.model_interface_id));
    policy["ceiling"]["interface_policy"][&accepted.model_interface_id] =
        json!({"models":["fixture-a"]});
    fixture::write(&root, file, policy);
    let fresh = plan_run_composition(&child).unwrap();
    let child_catalogue = catalogue(&fresh);
    assert!(
        child_catalogue["text"]
            .as_str()
            .unwrap()
            .contains("Later knowledge")
    );
    let provider: Value = serde_json::from_str(
        &fresh
            .descriptor_writes
            .iter()
            .find(|write| write.tag == format!("interface:{}", fresh.provider_interface_id))
            .unwrap()
            .content,
    )
    .unwrap();
    assert_eq!(
        provider["interface_driver_config"]["private_config"]["collection_catalogue_instruction_artifacts"],
        json!([child_catalogue["id"]])
    );
    assert!(
        !provider["interface_driver_config"]["private_config"]["instruction_artifacts"]
            .as_array()
            .unwrap()
            .contains(&adopted["snapshot"]["id"])
    );
    let child_record = RunRecord::from_bytes(fresh.run_write.content.as_bytes()).unwrap();
    assert!(
        !octopus_run_authoring::provider_context_bindings(
            &child_record,
            &fresh.provider_interface_id.into()
        )
        .unwrap()
        .iter()
        .any(|binding| binding.id.as_str() == adopted["snapshot"]["id"].as_str().unwrap())
    );
    let path = root
        .join("registry/artifact")
        .join(adopted["snapshot"]["id"].as_str().unwrap())
        .join("artifact.json");
    std::fs::write(path, b"{}").unwrap();
    request.existing_run_record = Some(before);
    assert!(
        plan_operator_chat_topology(&request)
            .unwrap_err()
            .contains("snapshot changed")
    );
}
