use octopus_context_collections::{CollectionSelection, ScopeContext};
use octopus_interfaces::{ArtifactBindingPayload, GrantProjection};
use octopus_ledger::{ArtifactRef, ContentHash};
use octopus_operator_chat_client::{
    ArtifactBindingSeed, OperatorChatPlanRequest, WriteHashAlgorithm, plan_operator_chat_topology,
    publication_selection::{
        PublicationSelectionSeeds, PublicationSourceAuthority, resolve_publication_selection,
        validate_publication_expansion_receipt, validate_publication_snapshot_refs,
    },
};
use octopus_run_record::RunRecord;
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path};

#[allow(dead_code)]
#[path = "../../../../../worker_environment_kind/system_room/packages/octopus-system-room-env/src/bound_surface.rs"]
mod bound_surface;
#[path = "support/composition_fixture.rs"]
mod composition;

const PROCEDURE: &str = "Read the exact evidence first.\nPrepare a checked ordinary file write.\nUse the writer receipt to report the outcome.";

fn allow_tool(root: &Path, tool: &str) {
    for (path, pointer) in [
        (
            "registry/interface/system_worker_template/policy.json",
            "/ceiling/tools",
        ),
        (
            "registry/ee_template/existence/ee_template.json",
            "/allowed_tools",
        ),
        ("ee/ee_existence.json", "/policy_narrowing/allowed_tools"),
    ] {
        let mut value: Value =
            serde_json::from_slice(&std::fs::read(root.join(path)).unwrap()).unwrap();
        value
            .pointer_mut(pointer)
            .unwrap()
            .as_array_mut()
            .unwrap()
            .push(json!(tool));
        composition::write(root, path, value);
    }
}

fn fixture(root: &Path) -> (OperatorChatPlanRequest, CollectionSelection, Value) {
    let composed = composition::client_fixture(root);
    let client_config = "registry/interface/world_operator_chat/client.json";
    let mut client: Value =
        serde_json::from_slice(&std::fs::read(root.join(client_config)).unwrap()).unwrap();
    client["run"]["context_artifact_bindings"] = json!([]);
    composition::write(root, client_config, client);
    for (path, interfaces, tools) in [
        (
            "registry/ee_template/existence/ee_template.json",
            "allowed_interfaces",
            "allowed_tools",
        ),
        (
            "ee/ee_existence.json",
            "policy_narrowing/allowed_interfaces",
            "policy_narrowing/allowed_tools",
        ),
    ] {
        let mut value: Value =
            serde_json::from_slice(&std::fs::read(root.join(path)).unwrap()).unwrap();
        value
            .pointer_mut(&format!("/{interfaces}"))
            .unwrap()
            .as_array_mut()
            .unwrap()
            .extend(
                [
                    "system_interface_authoring_publication",
                    "system_room_authoring_publication",
                    "system_room_publication",
                    "system_worker_template_publication",
                    "world_operator_chat_publication",
                    "world_provider_fixture_publication",
                ]
                .map(|id| json!(id)),
            );
        value
            .pointer_mut(&format!("/{tools}"))
            .unwrap()
            .as_array_mut()
            .unwrap()
            .extend(
                [
                    "file.write__authoring_publication",
                    "file.write__publication",
                    "file.read__publication",
                    "artifact.send__publication",
                ]
                .map(|id| json!(id)),
            );
        composition::write(root, path, value);
    }
    let source = json!({"id":"library","label":"context.collection","description":"Shared procedures","schema_version":1,
        "directory":"entries","limits":{"max_entries":8,"max_entry_bytes":16384,"max_page_bytes":32768,"max_scan_bytes":131072}});
    let entry = json!({"id":"procedure","schema_version":1,"title":"Checked publication","description":"Reusable procedure",
        "applicability":"When saving a reusable result","state":"active","scope":{"ee_ids":["ee_existence"]},
        "content":{"text":PROCEDURE,"evidence":[{"id":"private-proof","content":ContentHash::of_bytes(b"private")} ]},
        "selection":{"selected_tools":[{"tool":"file.write","interface":"system_room","worker_environment_kind":"system_room"}]},
        "requires":[{"kind":"worker_environment_kind","id":"system_room"},{"kind":"artifact","id":"opaque-requirement"}]});
    composition::write(
        root,
        "registry/artifact/library/artifact.json",
        source.clone(),
    );
    composition::write(
        root,
        "registry/artifact/library/entries/procedure.json",
        entry.clone(),
    );
    let opaque = root.join("registry/artifact/opaque-requirement/artifact.json");
    std::fs::create_dir_all(opaque.parent().unwrap()).unwrap();
    std::fs::write(
        opaque,
        b"opaque requirement; must not read this as an artifact body",
    )
    .unwrap();
    let pin = CollectionSelection {
        source: ArtifactRef {
            id: "library".into(),
            content: ContentHash::of_canonical_json(&source),
        },
        entry_id: "procedure".into(),
        revision: ContentHash::of_canonical_json(&entry),
    };
    let request = serde_json::from_value(json!({"registry_root":composed.roots.registry_root,"ee_instances_root":composed.roots.ee_instances_root,
        "run_records_root":composed.roots.run_records_root,"interface_private_state_root":composed.roots.interface_private_state_root,
        "write_hash_algorithm":"blake3","chat_id":"publication","provider_interface_id":"world_provider_fixture","model":"fixture-a",
        "reasoning_effort":"xhigh","ee_id":"ee_existence","worker_interface_id":"system_worker_template","client_surface_role":"task","cycle":"first",
        "selected_tools":composed.selected_tools,"context_artifact_bindings":[{"id":"library"}],"publications":[pin],"conversation":{"ledger_offset":0}})).unwrap();
    (request, pin, entry)
}

fn resolve(
    request: &OperatorChatPlanRequest,
    pins: &[CollectionSelection],
    seeds: PublicationSelectionSeeds,
) -> Result<octopus_operator_chat_client::publication_selection::ResolvedPublicationSelection, String>
{
    resolve_publication_selection(
        &request.registry_root,
        &ScopeContext {
            ee_id: "ee_existence".into(),
            worker_interface: "target".into(),
        },
        pins,
        seeds,
        PublicationSourceAuthority::Operator {
            context_bindings: &request.context_artifact_bindings,
            payload_bindings: &[],
        },
        WriteHashAlgorithm::Blake3,
    )
}

fn node_surface_plan(root: &Path, request: &OperatorChatPlanRequest) -> Value {
    let binary = Path::new(env!("CARGO_BIN_EXE_octopus-operator-chat-topology"));
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../../..")
        .canonicalize()
        .unwrap();
    composition::write(
        root,
        "host.json",
        json!({"host_manifest_version":1,"root_refs":{
        "run_records_root":request.run_records_root,"ee_instances_root":request.ee_instances_root}}),
    );
    std::fs::create_dir_all(root.join("ledger")).unwrap();
    std::fs::write(root.join("ledger/movements.jsonl"), b"").unwrap();
    let script = r#"let input=''; process.stdin.setEncoding('utf8');
process.stdin.on('data', chunk => input += chunk);
process.stdin.on('end', async () => { try {
  const x=JSON.parse(input);
  const plan=await require(x.client).planOperatorChatSurface({octopusHome:()=>x.root,
    registryRoot:()=>x.registry,installationRoot:()=>x.installation}, x.request);
  process.stdout.write(JSON.stringify({profile:plan.authoringProfile,writes:plan.writes}));
} catch(error) { process.stderr.write(error.stack); process.exitCode=1; } });"#;
    let mut child = std::process::Command::new(
        std::env::var_os("OCTOPUS_NODE_COMMAND").unwrap_or_else(|| "node".into()),
    )
    .args(["-e", script])
    .stdin(std::process::Stdio::piped())
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .spawn()
    .unwrap();
    serde_json::to_writer(child.stdin.take().unwrap(), &json!({"root":root,"registry":request.registry_root,
        "client":repository.join("registry/interface/world_operator_chat/client.cjs"),"installation":binary.parent().unwrap().parent().unwrap().parent().unwrap(),
        "request":{"chatId":request.chat_id,"cycle":request.cycle,"providerInterfaceId":request.provider_interface_id,
            "providerModel":request.model,"reasoningEffort":request.reasoning_effort,"eeId":request.ee_id,"workerInterfaceId":request.worker_interface_id,
            "clientSurfaceRole":request.client_surface_role,"doorHostSlots":request.door_host_slots,"contextArtifactBindings":request.context_artifact_bindings,
            "selectedTools":request.selected_tools.iter().map(|selected| json!({"tool":selected.tool,"interface":selected.interface,"workerEnvironmentKind":selected.worker_environment_kind})).collect::<Vec<_>>(),
            "selectedWorkerEnvironmentKinds":request.selected_worker_environment_kinds,"publications":request.publications}})).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn publication_snapshot_is_planned_bound_readable_and_keeps_provenance_non_authoritative() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let (request, pin, _) = fixture(&root);
    let resolved = resolve(&request, &[pin.clone()], Default::default()).unwrap();
    let snapshot = resolved.evidence[0].snapshot.as_ref().unwrap();
    assert_eq!(resolved.artifact_writes.len(), 1);
    assert!(!resolved.artifact_writes[0].path.exists());
    assert!(resolved.artifact_writes[0].expected_before_hash.is_none());
    assert_eq!(
        resolved.selection.context_artifact_bindings,
        vec![ArtifactBindingSeed {
            id: snapshot.id.to_string()
        }]
    );
    assert!(resolved.selection.payload_artifact_bindings.is_empty());
    let body: Value = serde_json::from_str(&resolved.artifact_writes[0].content).unwrap();
    assert_eq!(body["text"], PROCEDURE);
    assert_eq!(body["evidence"][0]["id"], "private-proof");
    assert!(body.get("sources").is_none());
    assert_eq!(body["publication_selection"]["context_sources"], json!([]));
    let plan = plan_operator_chat_topology(&request).unwrap();
    assert_eq!(plan.publication_evidence, resolved.evidence);
    assert_eq!(
        plan.authoring_profile.publication_evidence,
        resolved.evidence
    );
    let record = RunRecord::from_bytes(plan.run_record_content.as_bytes()).unwrap();
    let stable = record
        .checkpoints
        .iter()
        .find(|checkpoint| {
            checkpoint
                .outbound_interfaces()
                .any(|id| id.as_str() == plan.model_interface_id)
                && checkpoint
                    .artifact_bindings
                    .iter()
                    .any(|binding| binding.id == snapshot.id && binding.access.read)
        })
        .unwrap();
    let mut grant = GrantProjection::default();
    for binding in &stable.artifact_bindings {
        grant
            .artifact_access
            .insert(binding.id.to_string(), binding.access.clone());
    }
    let payload = ArtifactBindingPayload::new(
        snapshot.clone(),
        resolved.artifact_writes[0].content.as_bytes().to_vec(),
    );
    let surface =
        bound_surface::InvocationArtifactSurface::new(&[payload], &root, grant, None, 1 << 20).unwrap();
    let read: Value =
        serde_json::from_slice(&surface.read_impl(snapshot.id.as_str()).unwrap().unwrap()).unwrap();
    assert_eq!(read["text"], PROCEDURE);
    assert_eq!(surface.read_impl("private-proof").unwrap(), None);
    let selected_provider: Value = serde_json::from_str(
        &plan
            .interface_writes
            .iter()
            .find(|write| write.tag == format!("interface:{}", plan.model_interface_id))
            .unwrap()
            .content,
    )
    .unwrap();
    assert!(!selected_provider["interface_driver_config"]["private_config"]["worker_instruction_artifacts"].to_string().contains(snapshot.id.as_str()));
    let planned = &resolved.artifact_writes[0];
    std::fs::create_dir_all(planned.path.parent().unwrap()).unwrap();
    std::fs::write(&planned.path, &planned.content).unwrap();
    validate_publication_snapshot_refs(&request.registry_root, &[snapshot.clone()]).unwrap();
    let content = ContentHash::of_bytes(b"immutable context source");
    let immutable = ArtifactRef { id: format!("context.reference:{}", content.to_hex()).into(), content };
    validate_publication_snapshot_refs(&request.registry_root, &[immutable.clone()]).unwrap();
    let mut conflicting = immutable.clone();
    conflicting.content = ContentHash::of_bytes(b"different source");
    assert!(validate_publication_snapshot_refs(&request.registry_root, &[conflicting]).is_err());
    assert!(validate_publication_snapshot_refs(&request.registry_root, &[snapshot.clone(), immutable]).is_ok());
    assert!(
        resolve(&request, &[pin.clone()], Default::default())
            .unwrap()
            .artifact_writes
            .is_empty()
    );
    std::fs::write(
        &planned.path,
        serde_json::to_vec(
            &json!({"id":snapshot.id,"label":"context.reference","text":"conflict"}),
        )
        .unwrap(),
    )
    .unwrap();
    assert!(
        validate_publication_snapshot_refs(&request.registry_root, &[snapshot.clone()]).is_err()
    );
    assert!(
        resolve(&request, &[pin], Default::default())
            .unwrap_err()
            .contains("conflicting content")
    );
}

#[test]
fn publication_keeps_selected_instruction_provenance_out_of_nested_sources() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let (mut request, mut pin, mut entry) = fixture(&root);
    let instruction = json!({"id":"selected-instruction","label":"context.reference","text":"Use the declared publication route."});
    composition::write(&root, "registry/artifact/selected-instruction/artifact.json", instruction.clone());
    entry["selection"]["context_artifact_bindings"] = json!([{"id":"selected-instruction"}]);
    composition::write(&root, "registry/artifact/library/entries/procedure.json", entry.clone());
    pin.revision = ContentHash::of_canonical_json(&entry);
    request.publications = vec![pin.clone()];
    let resolved = resolve(&request, &[pin], Default::default()).unwrap();
    let body: Value = serde_json::from_str(&resolved.artifact_writes[0].content).unwrap();
    assert!(body.get("sources").is_none());
    assert_eq!(body["publication_selection"]["context_sources"], json!([{
        "id":"selected-instruction","content":ContentHash::of_canonical_json(&instruction)
    }]));
    assert!(resolved.selection.context_artifact_bindings.iter().any(|binding| binding.id == "selected-instruction"));
    plan_operator_chat_topology(&request).unwrap();
}

#[test]
fn ordinary_component_ids_and_a_new_dotted_tool_reach_real_authoring() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().to_path_buf();
    let (mut request, mut pin, mut entry) = fixture(&root);
    let source_id = format!("bibliothèque.procedure:{}", "x".repeat(100));
    let entry_id = format!("procédure.reuse:{}", "y".repeat(100));
    let owner = root.join("registry/artifact").join(&source_id);
    std::fs::rename(root.join("registry/artifact/library"), &owner).unwrap();
    let mut source: Value =
        serde_json::from_slice(&std::fs::read(owner.join("artifact.json")).unwrap()).unwrap();
    source["id"] = json!(source_id);
    std::fs::write(
        owner.join("artifact.json"),
        serde_json::to_vec(&source).unwrap(),
    )
    .unwrap();
    std::fs::rename(
        owner.join("entries/procedure.json"),
        owner.join("entries").join(format!("{entry_id}.json")),
    )
    .unwrap();
    entry["id"] = json!(entry_id);
    entry["selection"]["selected_tools"] = json!([{"tool":"file.read","interface":"system_room","worker_environment_kind":"system_room"}]);
    std::fs::write(
        owner.join("entries").join(format!("{entry_id}.json")),
        serde_json::to_vec(&entry).unwrap(),
    )
    .unwrap();
    pin.source = ArtifactRef {
        id: source_id.clone().into(),
        content: ContentHash::of_canonical_json(&source),
    };
    pin.entry_id = entry_id;
    pin.revision = ContentHash::of_canonical_json(&entry);
    request.context_artifact_bindings = vec![ArtifactBindingSeed { id: source_id }];
    request.publications = vec![pin.clone()];
    allow_tool(&root, "file.read");
    let plan = plan_operator_chat_topology(&request).unwrap();
    assert!(
        plan.authoring_profile
            .selected_tools
            .iter()
            .any(|selected| selected.tool == "file.read")
    );
    assert!(
        plan.required_tool_ids
            .contains(&plan.capability_tools["file.read"])
    );
    assert_eq!(plan.publication_evidence[0].pin, pin);
    validate_publication_expansion_receipt(
        &request.registry_root,
        plan.publication_expansion.as_ref().unwrap(),
    )
    .unwrap();
    #[cfg(unix)]
    {
        let tool = root.join("registry/tool/file.read/tool.json");
        let target = root.join("unbound-tool.json");
        std::fs::rename(&tool, &target).unwrap();
        std::os::unix::fs::symlink(&target, tool).unwrap();
        assert!(
            resolve(&request, &[pin], Default::default())
                .unwrap_err()
                .contains("symlink")
        );
    }
}

#[test]
fn native_amendment_and_continuation_preserve_accepted_capabilities_and_publication_evidence() {
    use octopus_operator_chat_client::run_composition::{CompositionRoots, current_run::*};
    use octopus_run_authoring::capability_amendment::ProviderCapabilityBridge;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let (mut request, pin, mut entry) = fixture(&root);
    request.selected_tools.clear();
    request.selected_worker_environment_kinds = vec!["system_room".into()];
    for id in ["accepted-context", "fresh-context"] {
        composition::write(
            &root,
            &format!("registry/artifact/{id}/artifact.json"),
            json!({"id":id,"label":"fixture.instructions","text":id}),
        );
    }
    let default_paths = [
        (
            "registry/interface/system_room/interface.json",
            "/interface_driver_config/private_config/context_artifact_bindings",
        ),
        (
            "registry/interface/world_provider_fixture/interface.json",
            "/interface_driver_config/private_config/context_artifact_bindings",
        ),
        (
            "registry/interface/world_operator_chat/client.json",
            "/run/context_artifact_bindings",
        ),
    ];
    let set_defaults = |id: &str| {
        for (path, pointer) in default_paths {
            let mut value: Value =
                serde_json::from_slice(&std::fs::read(root.join(path)).unwrap()).unwrap();
            if path.ends_with("client.json") {
                value["run"]["context_artifact_bindings"] = json!([{"id":id}]);
            } else {
                value["interface_driver_config"]["private_config"]["context_artifact_bindings"] =
                    json!([{"id":id}]);
            }
            assert_eq!(value.pointer(pointer).unwrap(), &json!([{"id":id}]));
            composition::write(&root, path, value);
        }
    };
    set_defaults("accepted-context");
    let accepted = plan_operator_chat_topology(&request).unwrap();
    for write in accepted.interface_writes.iter().chain(&accepted.run_writes) {
        std::fs::create_dir_all(write.path.parent().unwrap()).unwrap();
        std::fs::write(&write.path, &write.content).unwrap();
    }
    let roots = CompositionRoots {
        registry_root: request.registry_root.clone(),
        ee_instances_root: request.ee_instances_root.clone(),
        run_records_root: request.run_records_root.clone().unwrap(),
        interface_private_state_root: request.interface_private_state_root.clone().unwrap(),
    };
    let current = RunRecord::from_bytes(accepted.run_record_content.as_bytes()).unwrap();
    let templates = current.interface_state[&octopus_descriptors::InterfaceId::new(&accepted.model_interface_id)]["run_authoring_binding"]["checkpoints"].as_array().unwrap();
    assert!(
        templates
            .iter()
            .filter(|template| template["role"] == "stable")
            .all(|template| template["static_artifact_bindings"]
                .as_array()
                .unwrap()
                .contains(&json!("library")))
    );
    std::fs::create_dir_all(&roots.run_records_root).unwrap();
    std::fs::write(
        roots.run_records_root.join(format!("{}.json", current.id)),
        &accepted.run_record_content,
    )
    .unwrap();
    let provider: octopus_descriptors::InterfaceId = accepted.model_interface_id.clone().into();
    let input = current
        .checkpoints
        .iter()
        .find(|cp| {
            current.active_checkpoints.contains(&cp.id)
                && cp.outbound_interfaces().any(|id| id == &provider)
                && cp
                    .artifact_bindings
                    .iter()
                    .any(|binding| binding.id == pin.source.id)
        })
        .unwrap();
    let metadata: octopus_ledger::InvocationMetadata = serde_json::from_value(json!({"runtime":{"run":current.id,"ee":current.ee,
        "worker":current.worker,"checkpoint":input.id,"interface":provider,"epoch":input.epoch},"artifact_binding_refs":[pin.source],
        "grant":octopus_app_server::spine_root::inspect_registry_run_grant(&roots.registry_root,&roots.ee_instances_root,&current,&input.id).unwrap()})).unwrap();
    entry["id"] = json!("amendment");
    entry["content"]["text"] = json!("Read the newly selected complete instruction.");
    composition::write(
        &root,
        "registry/artifact/library/entries/amendment.json",
        entry.clone(),
    );
    let additional = CollectionSelection {
        entry_id: "amendment".into(),
        revision: ContentHash::of_canonical_json(&entry),
        ..pin.clone()
    };
    let door: octopus_descriptors::InterfaceId =
        accepted.capability_doors["system_room"].clone().into();
    let writer = accepted.capability_tools["file.write"].clone();
    let bridge = ProviderCapabilityBridge {
        tool: writer.clone(),
        correlation_id: "accepted-write".into(),
        arguments_hash: ContentHash::of_bytes(b"{}").to_hex(),
        return_interface: door.clone(),
    };
    let selected = current_selection(&roots, &current, &provider)
        .unwrap()
        .unwrap();
    let amendment = CurrentRunRequest {
        amendment_key: "publication".into(),
        publications: vec![additional],
        selection: Some(selected.clone()),
        authored: None,
    };
    let plan = plan_current_run_amendment_for_native(
        &roots,
        &current,
        &provider,
        amendment.clone(),
        bridge.clone(),
        &[(writer.clone(), door.clone())],
        &metadata,
    )
    .unwrap();
    assert!(
        plan.validation.safe_to_apply,
        "{:?}",
        plan.validation.regressions
    );
    assert_eq!(plan.publication_evidence.len(), 2);
    let snapshot = plan.publication_evidence[1].snapshot.as_ref().unwrap();
    assert!(plan.dependency_writes.iter().any(|write| {
        write.path
            == roots
                .registry_root
                .join("artifact")
                .join(snapshot.id.as_str())
                .join("artifact.json")
            && write.expected_before_hash.is_none()
    }));
    assert!(
        plan.dependency_writes
            .iter()
            .chain(&plan.activation_writes)
            .all(|write| !write.path.starts_with(
                roots
                    .registry_root
                    .join("interface")
                    .join(provider.as_str())
            ))
    );
    let mut denied = metadata.clone();
    denied
        .grant
        .artifact_access
        .get_mut("library")
        .unwrap()
        .read = false;
    assert!(
        plan_current_run_amendment_for_native(
            &roots,
            &current,
            &provider,
            amendment,
            bridge,
            &[(writer, door)],
            &denied
        )
        .unwrap_err()
        .contains("not readable")
    );
    let accepted_bindings =
        octopus_run_authoring::provider_context_bindings(&current, &provider).unwrap();
    set_defaults("fresh-context");
    let old_policy = std::fs::read(
        roots
            .registry_root
            .join("interface")
            .join(current.worker.as_str())
            .join("policy.json"),
    )
    .unwrap();
    allow_tool(&root, "file.read");
    let source_policy = "registry/interface/system_worker_template/policy.json";
    let mut changed: Value =
        serde_json::from_slice(&std::fs::read(root.join(source_policy)).unwrap()).unwrap();
    changed["role_artifacts"]
        .as_array_mut()
        .unwrap()
        .push(json!("new-role"));
    composition::write(&root, source_policy, changed);
    composition::write(
        &root,
        "registry/artifact/new-role/artifact.json",
        json!({"id":"new-role","label":"context.reference","text":"New preset guidance"}),
    );
    entry["id"] = json!(pin.entry_id);
    entry["state"] = json!("withdrawn");
    composition::write(
        &root,
        "registry/artifact/library/entries/procedure.json",
        entry,
    );
    PublicationSelectionSeeds::from_profile(&accepted.authoring_profile)
        .apply_operator(&mut request);
    request.publications.clear();
    request.existing_run_record = Some(json!(current));
    request.retain_current_capabilities = true;
    request.cycle = "second".into();
    let next = plan_operator_chat_topology(&request).unwrap();
    let next_record = RunRecord::from_bytes(next.run_record_content.as_bytes()).unwrap();
    let roles = |record: &RunRecord| -> Vec<octopus_run_record::ArtifactBinding> {
        serde_json::from_value(
            record.interface_state[&provider]["run_authoring_binding"]["writer_run"]
                ["role_artifact_bindings"]
                .clone(),
        )
        .unwrap()
    };
    let (accepted_roles, next_roles) = (roles(&current), roles(&next_record));
    assert!(next_roles.iter().any(|role| role.id.as_str() == "new-role"));
    assert!(accepted_roles.iter().all(|role| next_roles.contains(role)));
    assert_eq!(
        octopus_run_authoring::provider_context_bindings(&next_record, &provider).unwrap(),
        next_roles
            .iter()
            .chain(
                accepted_bindings
                    .iter()
                    .filter(|binding| !accepted_roles.contains(binding))
            )
            .cloned()
            .collect::<Vec<_>>()
    );
    assert_eq!(next.publication_evidence, accepted.publication_evidence);
    assert_eq!(next.authoring_profile, accepted.authoring_profile);
    assert_eq!(next.capability_tools, accepted.capability_tools);
    assert!(
        next.interface_writes.is_empty(),
        "unexpected descriptor writes: {:?}",
        next.interface_writes
            .iter()
            .map(|write| &write.tag)
            .collect::<Vec<_>>()
    );
    assert!(next.run_writes.is_empty());
    assert_eq!(
        std::fs::read(
            roots
                .registry_root
                .join("interface")
                .join(current.worker.as_str())
                .join("policy.json")
        )
        .unwrap(),
        old_policy
    );
    request.retain_current_capabilities = false;
    let reconfigured = plan_operator_chat_topology(&request).unwrap();
    assert!(reconfigured.capability_tools.contains_key("file.read"));
    let reconfigured_record =
        RunRecord::from_bytes(reconfigured.run_record_content.as_bytes()).unwrap();
    let fresh_bindings =
        octopus_run_authoring::provider_context_bindings(&reconfigured_record, &provider)
            .unwrap();
    assert!(
        fresh_bindings
            .iter()
            .any(|binding| binding.id.as_str() == "fresh-context")
    );
    assert!(
        !fresh_bindings
            .iter()
            .any(|binding| binding.id.as_str() == "accepted-context")
    );
}

#[test]
fn native_selection_requires_exact_readable_source_and_refuses_scope_revision_and_duplicate_conflicts()
 {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let (request, pin, entry) = fixture(&root);
    let mut metadata = composition::fixture(&root).metadata;
    let target = ScopeContext {
        ee_id: "ee_existence".into(),
        worker_interface: "target".into(),
    };
    let native = |metadata: &octopus_ledger::InvocationMetadata,
                  target: &ScopeContext,
                  pin: &CollectionSelection| {
        resolve_publication_selection(
            &request.registry_root,
            target,
            &[pin.clone()],
            Default::default(),
            PublicationSourceAuthority::Native { metadata },
            WriteHashAlgorithm::Blake3,
        )
    };
    assert!(native(&metadata, &target, &pin).is_err());
    metadata.artifact_binding_refs = vec![pin.source.clone()];
    metadata.grant.artifact_access.insert(
        "library".into(),
        octopus_policy::ArtifactAccess::read_only(),
    );
    assert!(native(&metadata, &target, &pin).is_ok());
    metadata.artifact_binding_refs[0].content = ContentHash::of_bytes(b"wrong source");
    assert!(native(&metadata, &target, &pin).is_err());
    metadata.artifact_binding_refs[0] = pin.source.clone();
    metadata
        .grant
        .artifact_access
        .get_mut("library")
        .unwrap()
        .read = false;
    assert!(native(&metadata, &target, &pin).is_err());
    metadata
        .grant
        .artifact_access
        .get_mut("library")
        .unwrap()
        .read = true;
    assert!(
        native(
            &metadata,
            &ScopeContext {
                ee_id: "outside".into(),
                ..target.clone()
            },
            &pin
        )
        .is_err()
    );
    let stale = CollectionSelection {
        revision: ContentHash::of_bytes(b"stale"),
        ..pin.clone()
    };
    assert!(native(&metadata, &target, &stale).is_err());
    assert!(
        resolve(&request, &[pin.clone(), stale], Default::default())
            .unwrap_err()
            .contains("conflict")
    );
    let mut withdrawn = entry;
    withdrawn["state"] = json!("withdrawn");
    composition::write(
        &root,
        "registry/artifact/library/entries/procedure.json",
        withdrawn.clone(),
    );
    let withdrawn = CollectionSelection {
        revision: ContentHash::of_canonical_json(&withdrawn),
        ..pin.clone()
    };
    assert!(native(&metadata, &target, &withdrawn).is_err());
    let source_path = root.join("registry/artifact/library/artifact.json");
    let mut source: Value = serde_json::from_slice(&std::fs::read(&source_path).unwrap()).unwrap();
    source["description"] = json!("different bound source");
    std::fs::write(source_path, serde_json::to_vec(&source).unwrap()).unwrap();
    assert!(native(&metadata, &target, &pin).is_err());
}

#[test]
fn explicit_defaults_use_the_resolver_and_target_formation_refuses_denied_selection() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let (mut request, pin, mut entry) = fixture(&root);
    request.publications.clear();
    let baseline = plan_operator_chat_topology(&request).unwrap();
    assert!(baseline.publication_evidence.is_empty());
    let provider_path = root.join("registry/interface/world_provider_fixture/interface.json");
    let mut provider: Value =
        serde_json::from_slice(&std::fs::read(&provider_path).unwrap()).unwrap();
    provider["interface_driver_config"]["private_config"]["publications"] = json!([pin]);
    std::fs::write(&provider_path, serde_json::to_vec(&provider).unwrap()).unwrap();
    let plan = plan_operator_chat_topology(&request).unwrap();
    assert_eq!(plan.publication_evidence.len(), 1);
    let bridged = node_surface_plan(&root, &request);
    assert_eq!(
        bridged["profile"],
        serde_json::to_value(&plan.authoring_profile).unwrap()
    );
    assert!(
        bridged["writes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|write| write["tag"]
                == format!(
                    "artifact:{}",
                    plan.publication_evidence[0].snapshot.as_ref().unwrap().id
                ))
    );
    let receipt = plan.publication_expansion.as_ref().unwrap();
    assert_eq!(receipt.requested, baseline.authoring_profile);
    assert!(receipt.publications.is_empty());
    assert_eq!(receipt.defaults.len(), 1);
    assert_eq!(
        receipt.defaults[0].descriptor_hash,
        ContentHash::of_canonical_json(&provider).to_hex()
    );
    validate_publication_expansion_receipt(&request.registry_root, receipt).unwrap();
    let mut tampered = receipt.clone();
    tampered.expanded.selected_tools.clear();
    assert!(validate_publication_expansion_receipt(&request.registry_root, &tampered).is_err());
    let mut tampered = receipt.clone();
    tampered.defaults[0].descriptor_hash = ContentHash::of_bytes(b"different descriptor").to_hex();
    assert!(validate_publication_expansion_receipt(&request.registry_root, &tampered).is_err());
    let mut ee: Value =
        serde_json::from_slice(&std::fs::read(root.join("ee/ee_existence.json")).unwrap()).unwrap();
    ee["policy_narrowing"]["allowed_artifact_verbs"] = json!([]);
    composition::write(&root, "ee/ee_existence.json", ee.clone());
    assert!(
        plan_operator_chat_topology(&request)
            .unwrap_err()
            .contains("target formation denies artifact read")
    );
    ee["policy_narrowing"]
        .as_object_mut()
        .unwrap()
        .remove("allowed_artifact_verbs");
    composition::write(&root, "ee/ee_existence.json", ee);
    entry["selection"]["selected_tools"] = json!([{"tool":"file.read","interface":"system_room","worker_environment_kind":"system_room"}]);
    composition::write(
        &root,
        "registry/artifact/library/entries/procedure.json",
        entry.clone(),
    );
    provider["interface_driver_config"]["private_config"]["publications"] =
        json!([CollectionSelection {
            revision: ContentHash::of_canonical_json(&entry),
            ..pin.clone()
        }]);
    std::fs::write(provider_path, serde_json::to_vec(&provider).unwrap()).unwrap();
    assert!(plan_operator_chat_topology(&request).is_err());
    entry["selection"]["door_host_slots"] =
        json!({"system_room":{"registry_artifact_root":root.join("registry/artifact")}});
    composition::write(
        &root,
        "registry/artifact/library/entries/procedure.json",
        entry.clone(),
    );
    let pin = CollectionSelection {
        revision: ContentHash::of_canonical_json(&entry),
        ..pin
    };
    let roots = PublicationSelectionSeeds {
        door_host_slots: BTreeMap::from([(
            "system_room".into(),
            BTreeMap::from([("registry_artifact_root".into(), root.join("other"))]),
        )]),
        ..Default::default()
    };
    assert!(
        resolve(&request, &[pin], roots)
            .unwrap_err()
            .contains("conflict")
    );
}
