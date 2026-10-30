use super::*;

#[test]
fn shipped_native_catalogs_accept_the_exact_artifact_reference_wire() {
    let reference = ArtifactRef { id: "capability-evidence".into(), content: ContentHash::of_bytes(b"verified") };
    let wire = json!(reference);
    assert_eq!(serde_json::from_value::<ArtifactRef>(wire.clone()).unwrap(), reference);
    fn check(value: &Value, wire: &Value, count: &mut usize) {
        match value {
            Value::Object(object) => {
                if value.pointer("/properties/id").is_some() && value.pointer("/properties/content").is_some() {
                    let validator = octopus_tool_registry::ToolArgumentValidator::compile(value).unwrap();
                    validator.validate(wire).unwrap();
                    let mut wrong = wire.clone(); wrong["content"] = json!("0".repeat(64));
                    assert!(validator.validate(&wrong).is_err());
                    *count += 1;
                }
                for child in object.values() { check(child, wire, count); }
            }
            Value::Array(array) => for child in array { check(child, wire, count); },
            _ => {}
        }
    }
    for owner in ["world_provider_codex", "world_provider_chatgpt"] {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..").join(owner).join("interface.json");
        let descriptor: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let mut count = 0;
        check(&descriptor["interface_driver_config"]["private_config"]["run_affordance_profile"]["catalog"], &wire, &mut count);
        assert!(count > 0, "{owner} has no artifact-bearing operation schemas");
    }
}

#[path = "../../../../../world_operator_chat/packages/octopus-operator-chat-client/tests/support/composition_fixture.rs"]
mod support;

#[test]
fn validation_report_retains_failures_without_repeating_registry_snapshots() {
    let issue = json!({"subject":"run/task", "checkpoint":"result", "reason":"missing receiving role"});
    let stage = json!({"after_write":1, "available":vec!["interface/repeated-owner"; 400], "issues":[issue.clone()]});
    let mut validation: RegistryCandidateValidation = serde_json::from_value(json!({
        "safe_to_apply":false, "checked_subjects":vec!["interface/repeated-owner"; 400],
        "stages":vec![stage; 80], "regressions":[issue.clone()],
    })).unwrap();
    validation.stages[0].issues.push(RegistryValidationIssue { subject: "interface/resolved".into(), checkpoint: None, reason: "dependency not written yet".into() });
    let report = validation_report(&validation);
    assert_eq!(report["safe_to_apply"], false);
    assert_eq!(report["regressions"], json!([issue.clone()]));
    assert_eq!(report["issues"], json!([issue]));
    assert_eq!(report["publication_stage_count"], 80);
    assert!(serde_json::to_vec(&report).unwrap().len() < 1024);
}

#[test]
fn native_creation_routes_use_authored_defaults_and_explicit_empty_overrides() {
    let temp = tempfile::tempdir().unwrap();
    let request = support::fixture(temp.path());
    let (mut affordances, metadata) = exact_affordances(&request);
    let selections = json!({
        "external_inputs":[{"checkpoint_id":"input", "interface_id":"system_worker_unselected"}],
        "external_relays":[{"checkpoint_id":"relay", "interface_id":"system_worker_parent", "outbound_interface_ids":["system_worker_unselected"]}],
        "model_outputs":[{"checkpoint_id":"output", "interface_id":"system_worker_parent", "required_verified_artifact":{
            "mark":"tool.call.allowed", "artifact_label":"tool.call", "verifier":"tool_call_allowed", "params":{"tools":["file.write"]}}}],
    });
    let mut defaults = json!({"ee_id":request.ee_id, "worker_template_interface_id":request.worker_template_interface_id,
        "provider_interface_id":request.provider_interface_id, "model":request.model, "reasoning_effort":request.reasoning_effort,
        "authoring":request.authoring, "worker_environment_kinds":request.selected_worker_environment_kinds,
        "tools":request.selected_tools, "door_host_slots":request.door_host_slots});
    let legacy: RunDefaults = serde_json::from_value(defaults.clone()).unwrap();
    assert!(legacy.external_inputs.is_empty() && legacy.external_relays.is_empty() && legacy.model_outputs.is_empty() && legacy.publications.is_empty());
    for (field, value) in selections.as_object().unwrap() { defaults[field] = value.clone(); }
    affordances.config.defaults = Some(serde_json::from_value(defaults).unwrap());
    let omitted: CreateRunRequest = serde_json::from_value(json!({"text":"Use authored defaults"})).unwrap();
    let selected = affordances.composition_request(&metadata, &request.admitted_parent_record, "selection", omitted).unwrap();
    for (field, value) in selections.as_object().unwrap() { assert_eq!(json!(selected)[field], *value); }
    let explicit: CreateRunRequest = serde_json::from_value(json!({"text":"No extra routes", "external_inputs":[], "external_relays":[], "model_outputs":[]})).unwrap();
    let empty = affordances.composition_request(&metadata, &request.admitted_parent_record, "empty", explicit).unwrap();
    assert!(empty.external_inputs.is_empty() && empty.external_relays.is_empty() && empty.model_outputs.is_empty());
    affordances.config.defaults = Some(legacy);
    let mut explicit = selections.clone(); explicit["text"] = json!("Use exact routes");
    let explicit = serde_json::from_value(explicit).unwrap();
    let selected = affordances.composition_request(&metadata, &request.admitted_parent_record, "explicit", explicit).unwrap();
    for (field, value) in selections.as_object().unwrap() { assert_eq!(json!(selected)[field], *value); }
    let descriptor: Value = serde_json::from_slice(&std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../interface.json")).unwrap()).unwrap();
    let operations = descriptor["interface_driver_config"]["private_config"]["run_affordance_profile"]["catalog"]["operations"].as_array().unwrap();
    for name in ["create_run", "preview_run"] {
        let schema = &operations.iter().find(|entry| entry["name"] == name).unwrap()["input_schema"];
        let validator = ToolArgumentValidator::compile(schema).unwrap();
        let mut args = selections.clone(); args["creation_key"] = json!("selection"); args["text"] = json!("Exact selected routes");
        validator.validate(&args).unwrap();
        let mut invalid = args.clone(); invalid["external_relays"][0].as_object_mut().unwrap().remove("outbound_interface_ids");
        assert!(validator.validate(&invalid).is_err());
        invalid = args.clone(); invalid["model_outputs"][0]["required_verified_artifact"].as_object_mut().unwrap().remove("verifier");
        assert!(validator.validate(&invalid).is_err());
        let mut pinned = args.clone(); pinned["publications"] = json!([{
            "source":ArtifactRef { id:"collection".into(), content:ContentHash::of_bytes(b"collection") },
            "entry_id":"published-entry", "revision":ContentHash::of_bytes(b"entry").to_hex(),
        }]);
        validator.validate(&pinned).unwrap();
        pinned["publications"][0]["revision"] = json!(vec![0_u8; 32]);
        assert!(validator.validate(&pinned).is_err());
        let mixed = json!({"creation_key":"mixed", "publications":[], "authored":{
            "run_id":"new-run", "writes":[{"path":"/tmp/new-run.json", "content":"{}", "expected_before_hash":null}], "outgoing_bindings":[],
        }});
        assert!(validator.validate(&mixed).is_err());
    }
    let mut selection = selections;
    for field in ["selected_tools", "selected_worker_environment_kinds", "context_artifact_bindings", "payload_artifact_bindings"] {
        selection[field] = json!([]);
    }
    selection["door_host_slots"] = json!({});
    for name in ["preview_current_run", "amend_current_run"] {
        let schema = &operations.iter().find(|entry| entry["name"] == name).unwrap()["input_schema"];
        let validator = ToolArgumentValidator::compile(schema).unwrap();
        validator.validate(&json!({"amendment_key":"selection", "selection":selection, "publications":[{
            "source":ArtifactRef { id:"collection".into(), content:ContentHash::of_bytes(b"collection") },
            "entry_id":"published-entry", "revision":ContentHash::of_bytes(b"entry").to_hex(),
        }]})).unwrap();
        let authored = json!({"expected_before_hash":"0".repeat(64), "expected_before":{}, "run_record":{}, "writes":[]});
        validator.validate(&json!({"amendment_key":"authored", "authored":authored})).unwrap();
        assert!(validator.validate(&json!({"amendment_key":"mixed", "authored":authored, "publications":[]})).is_err());
    }
}

#[test]
fn native_publication_pins_require_each_exact_readable_source_and_keep_empty_overrides() {
    let temp = tempfile::tempdir().unwrap();
    let request = support::fixture(temp.path());
    let (mut affordances, metadata) = exact_affordances(&request);
    let source = ArtifactRef { id:"bound-collection".into(), content:ContentHash::of_bytes(b"collection") };
    let pins = vec![
        CollectionSelection { source:source.clone(), entry_id:"entry-a".into(), revision:ContentHash::of_bytes(b"a") },
        CollectionSelection { source:source.clone(), entry_id:"entry-b".into(), revision:ContentHash::of_bytes(b"b") },
    ];
    affordances.config.defaults = Some(serde_json::from_value(json!({"ee_id":request.ee_id,
        "worker_template_interface_id":request.worker_template_interface_id, "provider_interface_id":request.provider_interface_id,
        "model":request.model, "reasoning_effort":request.reasoning_effort, "authoring":request.authoring, "publications":pins,
    })).unwrap());
    let args = || serde_json::from_value(json!({"text":"Selected publication"})).unwrap();
    assert!(affordances.composition_request(&metadata, &request.admitted_parent_record, "unbound", args()).unwrap_err().contains("exact readable metadata binding"));
    let mut granted = json!(metadata);
    granted["grant"]["artifact_access"][source.id.as_str()] = json!({"read":true});
    let read_without_binding = serde_json::from_value(granted.clone()).unwrap();
    assert!(affordances.composition_request(&read_without_binding, &request.admitted_parent_record, "no-ref", args()).unwrap_err().contains("exact readable metadata binding"));
    granted["artifact_binding_refs"].as_array_mut().unwrap().push(json!(source));
    let admitted: InvocationMetadata = serde_json::from_value(granted.clone()).unwrap();
    let selected = affordances.composition_request(&admitted, &request.admitted_parent_record, "admitted", args()).unwrap();
    assert_eq!(selected.publications, pins);
    assert!(selected.selected_tools.is_empty());
    assert!(selected.context_artifact_bindings.is_empty() && selected.payload_artifact_bindings.is_empty());
    let mut wrong = granted.clone(); wrong["artifact_binding_refs"].as_array_mut().unwrap().last_mut().unwrap()["content"] = json!(ContentHash::of_bytes(b"changed"));
    let wrong = serde_json::from_value(wrong).unwrap();
    assert!(affordances.composition_request(&wrong, &request.admitted_parent_record, "stale-source", args()).unwrap_err().contains("exact readable metadata binding"));
    granted["grant"]["artifact_access"][source.id.as_str()] = json!({"read":false});
    let denied = serde_json::from_value(granted).unwrap();
    assert!(affordances.composition_request(&denied, &request.admitted_parent_record, "denied", args()).unwrap_err().contains("exact readable metadata binding"));
    let empty = serde_json::from_value(json!({"text":"No publications", "publications":[]})).unwrap();
    assert!(affordances.composition_request(&metadata, &request.admitted_parent_record, "empty", empty).unwrap().publications.is_empty());
    let mixed = json!({"creation_key":"mixed", "publications":pins, "authored":{
        "run_id":"new-run", "writes":[{"path":request.roots.run_records_root.join("new-run.json"), "content":"{}", "expected_before_hash":null}], "outgoing_bindings":[],
    }});
    assert!(affordances.plan_call(&metadata, &request.admitted_parent_record, &request.admitted_parent_record, "mixed", "create_run", &mixed)
        .unwrap_err().contains("exact authored files cannot be combined"));
}

#[test]
fn native_publication_composition_validates_private_scope_and_stages_inline_context() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let mut request = support::client_fixture(&root);
    let baseline = plan_run_composition(&request).unwrap();
    let descriptor = json!({"id":"bound-publication", "label":"context.collection", "description":"Run guidance",
        "schema_version":1, "directory":"entries", "limits":{"max_entries":4, "max_entry_bytes":16384,
            "max_page_bytes":65536, "max_scan_bytes":65536}});
    let context = json!({"id":"selected-context", "label":"context.reference", "text":"Exact selected context."});
    support::write(&root, "registry/artifact/selected-context/artifact.json", context.clone());
    let context_ref = ArtifactRef { id:"selected-context".into(), content:ContentHash::of_canonical_json(&context) };
    let entry = json!({"schema_version":1, "id":"guidance", "title":"Run guidance", "description":"Exact selected context",
        "applicability":"Selected target worker", "state":"active", "scope":{"ee_ids":[request.ee_id],
            "worker_interfaces":[request.admitted_parent_record.worker, request.communication.receive_interface_id]},
        "content":{"text":"Preserve the selected interface topology.", "evidence":[ArtifactRef {
            id:"unbound-provenance".into(), content:ContentHash::of_bytes(b"provenance"),
        }]}, "selection":{"external_inputs":[{"checkpoint_id":"publication_input", "interface_id":"system_worker_unselected"}],
            "context_artifact_bindings":[{"id":"selected-context"}]}});
    support::write(&root, "registry/artifact/bound-publication/artifact.json", descriptor.clone());
    support::write(&root, "registry/artifact/bound-publication/entries/guidance.json", entry.clone());
    let source = ArtifactRef { id:"bound-publication".into(), content:ContentHash::of_canonical_json(&descriptor) };
    let pin = CollectionSelection { source:source.clone(), entry_id:"guidance".into(), revision:ContentHash::of_canonical_json(&entry) };
    let (_, metadata) = exact_affordances(&request);
    let mut admitted = json!(metadata);
    admitted["grant"]["artifact_access"][source.id.as_str()] = json!({"read":true});
    admitted["artifact_binding_refs"].as_array_mut().unwrap().push(json!(source));
    request.metadata = serde_json::from_value(admitted).unwrap();
    let publication_metadata = request.metadata.clone();
    request.publications = vec![pin.clone()];
    let plan = plan_run_composition(&request).unwrap();
    assert_eq!(plan.required_tool_ids, baseline.required_tool_ids);
    assert_eq!(plan.publication_evidence.len(), 1);
    assert_eq!(plan.publication_evidence[0].pin, pin);
    assert_eq!(plan.publication_evidence[0].entry, entry);
    let snapshot = plan.publication_evidence[0].snapshot.as_ref().unwrap();
    let snapshot_path = request.roots.registry_root.join("artifact").join(snapshot.id.as_str()).join("artifact.json");
    assert!(!snapshot_path.exists());
    assert_eq!(plan.descriptor_writes[0].path, snapshot_path);
    let value: Value = serde_json::from_str(&plan.descriptor_writes[0].content).unwrap();
    assert_eq!(ContentHash::of_canonical_json(&value), snapshot.content);
    assert_eq!(value["text"], entry["content"]["text"]);
    let record = RunRecord::from_bytes(plan.run_write.content.as_bytes()).unwrap();
    let input = record.checkpoints.iter().find(|row| row.id.as_str() == "publication_input").unwrap();
    assert_eq!(input.inbound_interface().unwrap().as_str(), "system_worker_unselected");
    assert!(input.artifact_bindings.iter().any(|binding| binding.id == snapshot.id && binding.access.read));
    assert!(record.checkpoints.iter().all(|row| row.artifact_bindings.iter().all(|binding| binding.id.as_str() != "unbound-provenance")));
    let sidecar: Value = serde_json::from_str(&plan.descriptor_writes.iter().find(|write|
        write.path.file_name().is_some_and(|name| name == "octopus_os.json")).unwrap().content).unwrap();
    assert_eq!(sidecar["authoring_profile"]["publication_evidence"], json!(plan.publication_evidence));
    assert_eq!(sidecar["authoring_profile"]["external_inputs"], entry["selection"]["external_inputs"]);
    let writes = composition_candidate_writes(&plan);
    let exact = plan_authored_run_composition(&request.roots, &request.admitted_parent_record, AuthoredRunComposition {
        run_id:plan.run_id.clone(), writes, outgoing_bindings:Vec::new(), publication_read_refs:Vec::new(), seed:None,
    }, false).unwrap();
    assert!(exact.validation.safe_to_apply, "{:?}", exact.validation.stages);
    let writer_writes = exact.authored.writes.iter().map(|write| PlannedClientWrite { path:write.path.clone(), content:write.content.clone(),
        expected_before_hash:write.expected_before_hash.clone(), tag:"selected-publication".into() }).collect::<Vec<_>>();
    let (_, proofs) = file_write_batch(&writer_writes, &request.roots.run_records_root.join("caller.json")).unwrap();
    assert!(proofs.iter().any(|proof| proof.path == snapshot_path));
    request.communication.receive_interface_id = "system_worker_private_other".into();
    assert!(plan_run_composition(&request).is_err());
    request.communication.receive_interface_id = plan.worker_interface_id;
    request.publications[0].revision = ContentHash::of_bytes(b"stale entry");
    assert!(plan_run_composition(&request).is_err());
    request.publications[0] = pin;
    request.metadata.grant.artifact_access.clear();
    assert!(plan_run_composition(&request).unwrap_err().contains("not readable"));
    assert!(!snapshot_path.exists());
    request.metadata = publication_metadata;
    request.communication.client_template_interface_id = None;
    request.communication.client_title = None;
    request.communication.client_interface_id = None;
    let identity = call_identity(request.admitted_parent_record.id.as_str(), "reuse").unwrap();
    request.identity_seed = identity.clone();
    request.communication.receive_interface_id = format!("system_worker_{identity}");
    let mut entry = entry.clone();
    entry["scope"]["worker_interfaces"].as_array_mut().unwrap().push(json!(request.communication.receive_interface_id));
    request.publications[0].revision = ContentHash::of_canonical_json(&entry);
    support::write(&root, "registry/artifact/bound-publication/entries/guidance.json", entry.clone());
    let first = plan_run_composition(&request).unwrap();
    let snapshot = first.publication_evidence[0].snapshot.as_ref().unwrap().clone();
    let snapshot_path = request.roots.registry_root.join("artifact").join(snapshot.id.as_str()).join("artifact.json");
    let snapshot_write = first.descriptor_writes.iter().find(|write| write.path == snapshot_path).unwrap();
    std::fs::create_dir_all(snapshot_path.parent().unwrap()).unwrap();
    std::fs::write(&snapshot_path, &snapshot_write.content).unwrap();
    let reused = plan_run_composition(&request).unwrap();
    assert!(!reused.descriptor_writes.iter().any(|write| write.path == snapshot_path));
    assert!(!reused.descriptor_writes.iter().any(|write| write.path.file_name().is_some_and(|name| name == "octopus_os.json")));
    let (mut affordances, _) = exact_affordances(&request);
    affordances.config.defaults = Some(RunDefaults {
        client_template_interface_id:None, output_selection:None, ee_id:Some(request.ee_id.clone()),
        worker_template_interface_id:request.worker_template_interface_id.clone(), provider_interface_id:request.provider_interface_id.clone(),
        model:request.model.clone(), reasoning_effort:request.reasoning_effort.clone(), service_tier:None,
        authoring:request.authoring.clone(), worker_environment_kinds:request.selected_worker_environment_kinds.clone(),
        tools:request.selected_tools.clone(), door_host_slots:request.door_host_slots.clone(), context_artifacts:vec![],
        external_inputs:request.external_inputs.clone(), external_relays:request.external_relays.clone(), model_outputs:request.model_outputs.clone(),
        publications:request.publications.clone(), return_interface_ids:request.communication.return_interface_ids.clone(), return_to_source:false,
    });
    let args = json!({"creation_key":"reuse", "text":"Exact selected context", "publications":request.publications});
    let NativeStep::Complete { result: preview } = affordances.plan_call(&request.metadata, &request.admitted_parent_record,
        &request.admitted_parent_record, "public-preview", "preview_run", &args).unwrap() else { panic!("expected public preview"); };
    assert_eq!(preview["ok"], true, "{preview}");
    assert_eq!(preview["authored"]["publication_read_refs"], json!(publication_read_refs(&reused.publication_evidence).unwrap()));
    assert!(preview.get("publication_read_refs").is_none());
    let authored_args = json!({"creation_key":"reuse", "authored":preview["authored"]});
    let descriptor: Value = serde_json::from_slice(&std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../interface.json")).unwrap()).unwrap();
    for name in ["preview_run", "create_run", "preview_revision", "revise_run"] {
        let schema = &descriptor["interface_driver_config"]["private_config"]["run_affordance_profile"]["catalog"]["operations"]
            .as_array().unwrap().iter().find(|row| row["name"] == name).unwrap()["input_schema"];
        ToolArgumentValidator::compile(schema).unwrap().validate(&authored_args).unwrap();
    }
    let mut withdrawn = entry.clone(); withdrawn["state"] = json!("withdrawn");
    support::write(&root, "registry/artifact/bound-publication/entries/guidance.json", withdrawn);
    let NativeStep::Effect { pending, .. } = affordances.plan_call(&request.metadata, &request.admitted_parent_record,
        &request.admitted_parent_record, "accepted-reuse", "create_run", &authored_args).unwrap() else { panic!("expected accepted public authored writer"); };
    let mut missing_reads = json!(pending);
    missing_reads["state"].as_object_mut().unwrap().remove("publication_read_refs");
    assert!(serde_json::from_value::<NativePending>(missing_reads).is_err());
    let restored: NativePending = serde_json::from_value(json!(pending)).unwrap();
    let NativePhase::PinnedApplying { files, publication_read_refs, .. } = &restored.state else { panic!("expected applying phase"); };
    assert_eq!(publication_read_refs.len(), 2);
    assert!(publication_read_refs.contains(&snapshot) && publication_read_refs.contains(&context_ref));
    assert!(files.iter().all(|proof| proof.path != snapshot_path));
    for write in restored.effect.arguments["writes"].as_array().unwrap() {
        let path = Path::new(write["path"].as_str().unwrap());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, write["content"].as_str().unwrap()).unwrap();
    }
    let receipt = json!(ToolResultArtifact::success(&restored.effect.tool, Some(restored.effect.correlation_id.clone()),
        json!({"result":"ordered_file_writes", "writes":files.iter().map(|proof| json!({"path":proof.path,
            "before_hash":proof.before_hash, "after_hash":proof.after_hash, "bytes":proof.bytes, "outcome":"applied",
            "observed_hash":proof.after_hash, "error":null})).collect::<Vec<_>>()})));
    let NativeStep::Effect { effect:seed, pending:seed_pending, .. } = restored.advance(&receipt).unwrap() else { panic!("unchanged read evidence must permit seed"); };
    assert_eq!(seed.tool, "artifact.send");
    let seed_pending: NativePending = serde_json::from_value(json!(seed_pending)).unwrap();
    let mut missing_seed_reads = json!(seed_pending);
    missing_seed_reads["state"].as_object_mut().unwrap().remove("publication_read_refs");
    assert!(serde_json::from_value::<NativePending>(missing_seed_reads).is_err());
    let NativePhase::PinnedAppliedSeeding { publication_read_refs: retained, .. } = &seed_pending.state else { panic!("expected seeding phase"); };
    assert_eq!(retained, publication_read_refs);
    let delivered = json!(ToolResultArtifact::success(&seed.tool, Some(seed.correlation_id.clone()), json!({"admitted":{"seq":17}})));
    let NativeStep::Complete { result } = seed_pending.advance(&delivered).unwrap() else { panic!("expected seed receipt"); };
    assert_eq!(result["seed_admitted"], true);
    for reference in [&snapshot, &context_ref] {
        let path = root.join("registry/artifact").join(reference.id.as_str()).join("artifact.json");
        let accepted = std::fs::read(&path).unwrap();
        let mut changed: Value = serde_json::from_slice(&accepted).unwrap();
        changed["text"] = json!("Valid same-ID text changed after native acceptance.");
        std::fs::write(&path, serde_json::to_vec(&changed).unwrap()).unwrap();
        let error = restored.advance(&receipt).unwrap_err();
        assert!(error.contains(reference.id.as_str()), "{error}");
        std::fs::write(path, accepted).unwrap();
    }
}

#[test]
fn current_run_requires_complete_effects_and_exact_final_readback_with_original_call_identity() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let request = support::fixture(&root);
    let composition = plan_run_composition(&request).unwrap();
    for write in composition_candidate_writes(&composition) {
        std::fs::create_dir_all(write.path.parent().unwrap()).unwrap();
        std::fs::write(write.path,write.content).unwrap();
    }
    let before: Value = serde_json::from_str(&composition.run_write.content).unwrap();
    let (mut affordances,_) = exact_affordances(&request);
    affordances.catalog.operations.push(RunCatalogEntry {name:"attach_here".into(),description:"Attach".into(),input_schema:json!({"type":"object"}),operation:RunOperation::AmendCurrentRun});
    affordances.enabled.insert("attach_here".into());
    let observed_hash=ContentHash::of_bytes(composition.run_write.content.as_bytes()).to_hex();
    let arguments=json!({"amendment_key":"amend","authored":{"expected_before_hash":observed_hash,"expected_before":before,"run_record":before,"writes":[]}});
    assert_eq!(affordances.planning_baseline("attach_here",&arguments).unwrap(),Some((observed_hash,before.clone())));
    assert!(affordances.planning_baseline("attach_here",&json!({"amendment_key":"amend","authored":{}})).is_err());
    assert!(affordances.planning_baseline("create_run",&json!({})).unwrap().is_none());
    let provider: InterfaceId = composition.provider_interface_id.clone().into();
    let descriptor: Value = serde_json::from_slice(&std::fs::read(request.roots.registry_root.join("interface").join(provider.as_str()).join("interface.json")).unwrap()).unwrap();
    let readback = NativeEffect {tool:"file.read".into(),arguments:json!({"path":composition.run_write.path}),correlation_id:"amend:readback".into()};
    let amendment = ProviderCapabilityAmendment {amendment_key:"amend".into(),provider_interface:provider.clone(),before:before.clone(),after:before.clone(),
        authoring_profile:serde_json::from_value(descriptor["interface_driver_config"]["private_config"]["run_authoring_profile"].clone()).unwrap(),
        bridge:ProviderCapabilityBridge {tool:composition.authoring.capability_tools["file.write"].clone(),correlation_id:"amend:activation".into(),
            arguments_hash:ContentHash::of_bytes(b"{}").to_hex(),return_interface:composition.authoring.capability_doors["system_room"].clone().into()}};
    let topology = before["interface_state"][provider.as_str()]["run_authoring_binding"].clone();
    let mut installed = before.clone();
    installed["interface_state"][provider.as_str()]["capability_amendment"] = json!({"amendment_key":"amend","bridge":amendment.bridge,
        "delta":{"before":topology,"after":topology,"static_artifact_additions":[],"checkpoints":[],"interface_state":{}},"ordinary_patch":[],
        "effect":{"manifest_path":root.join("effect.json"),"manifest":"{}","payload":"{}"}});
    let content = serde_json::to_string(&installed).unwrap();
    std::fs::write(&composition.run_write.path,&content).unwrap();
    let proof = |name: &str| { let path=root.join(name);std::fs::write(&path,b"{}").unwrap();FileProof {path,before_hash:None,after_hash:ContentHash::of_bytes(b"{}").to_hex(),bytes:2} };
    let dependency = proof("dependency.json"); let activation = proof("activation.json");
    let context_path = request.roots.registry_root.join("artifact/role-system_worker_template/artifact.json");
    let context_bytes = std::fs::read(&context_path).unwrap();
    let context: Value = serde_json::from_slice(&context_bytes).unwrap();
    let context_ref = ArtifactRef { id:"role-system_worker_template".into(), content:ContentHash::of_canonical_json(&context) };
    let snapshot = json!({"id":"accepted-publication-snapshot", "label":"context.reference", "text":"Accepted inline publication context."});
    support::write(&root, "registry/artifact/accepted-publication-snapshot/artifact.json", snapshot.clone());
    let accepted_refs = vec![context_ref, ArtifactRef { id:"accepted-publication-snapshot".into(), content:ContentHash::of_canonical_json(&snapshot) }];
    let effect = |stage: &str| NativeEffect {tool:"file.write".into(),arguments:json!({"ordered":true}),correlation_id:format!("amend:{stage}")};
    let pending = NativePending {call_id:"original-model-call".into(),operation_name:"amend_current_run".into(),source_run:composition.run_id.clone(),effect:effect("dependencies"),
        state:NativePhase::PinnedCurrentRun {stage:CurrentRunStage::Dependencies,publication_read_refs:accepted_refs.clone(),plan:Box::new(PendingCurrentRun {amendment,dependency_files:vec![dependency.clone()],activation_files:vec![activation.clone()],
            activation:Some(effect("activation")),readback,validation_roots:validation_roots(&request.roots),checked_subjects:vec![format!("run/{}",composition.run_id)],allowed_issues:vec![],receipts:vec![]})}};
    let receipt = |pending: &NativePending, proof: &FileProof| json!(ToolResultArtifact::success(&pending.effect.tool,Some(pending.effect.correlation_id.clone()),
        json!({"result":"ordered_file_writes","writes":[{"path":proof.path,"before_hash":proof.before_hash,"after_hash":proof.after_hash,
            "bytes":proof.bytes,"outcome":"applied","observed_hash":proof.after_hash,"error":null}]})));
    let mut legacy_wire = json!(pending);
    legacy_wire["state"]["phase"] = json!("current_run");
    legacy_wire["state"].as_object_mut().unwrap().remove("publication_read_refs");
    let legacy: NativePending = serde_json::from_value(legacy_wire.clone()).unwrap();
    assert_eq!(json!(legacy), legacy_wire);
    let mut incomplete = receipt(&pending,&dependency);
    incomplete["body"]["writes"][0]["outcome"]=json!("not_applied");
    assert!(pending.advance(&incomplete).unwrap_err().contains("uncertain"));
    let refuse_drift = |pending: &NativePending, receipt: &Value| {
        let mut missing = json!(pending);
        missing["state"].as_object_mut().unwrap().remove("publication_read_refs");
        assert!(serde_json::from_value::<NativePending>(missing).is_err());
        for reference in &accepted_refs {
            let path = request.roots.registry_root.join("artifact").join(reference.id.as_str()).join("artifact.json");
            let accepted = std::fs::read(&path).unwrap();
            let mut changed: Value = serde_json::from_slice(&accepted).unwrap();
            changed["text"] = json!("Changed after amendment acceptance.");
            std::fs::write(&path, serde_json::to_vec(&changed).unwrap()).unwrap();
            let error = pending.advance(receipt).unwrap_err();
            assert!(error.contains(reference.id.as_str()), "{error}");
            std::fs::write(path, accepted).unwrap();
        }
    };
    refuse_drift(&pending, &receipt(&pending,&dependency));
    let NativeStep::AmendmentEffect {pending:activation_pending,..}=pending.advance(&receipt(&pending,&dependency)).unwrap() else {panic!("dependency receipt must continue at checked amendment");};
    assert_eq!(activation_pending.call_id,"original-model-call");
    let mut failed=receipt(&activation_pending,&activation);failed["ok"]=json!(false);failed.as_object_mut().unwrap().remove("body");failed["error"]=json!({"code":"interrupted","message":"interrupted"});
    let NativeStep::Complete {result}=activation_pending.advance(&failed).unwrap() else {panic!("missing failed-stage result");};
    assert_eq!(result["installed"],Value::Null);assert_eq!(result["work_completed"],false);
    refuse_drift(&activation_pending, &receipt(&activation_pending,&activation));
    let NativeStep::Effect {pending:final_pending,..}=activation_pending.advance(&receipt(&activation_pending,&activation)).unwrap() else {panic!("activation must await readback");};
    let final_receipt=json!(ToolResultArtifact::success("file.read",Some(final_pending.effect.correlation_id.clone()),
        json!({"path":composition.run_write.path,"content":content,"content_hash":ContentHash::of_bytes(content.as_bytes()).to_hex(),"bytes":content.len()})));
    let mut wrong=final_receipt.clone();wrong["correlation_id"]=json!("another-call");
    assert!(final_pending.advance(&wrong).unwrap_err().contains("correlation"));
    wrong=final_receipt.clone();wrong["body"]["bytes"]=json!(0);
    assert!(final_pending.advance(&wrong).unwrap_err().contains("exact file receipt"));
    refuse_drift(&final_pending, &final_receipt);
    let restored:NativePending=serde_json::from_value(json!(final_pending)).unwrap();
    let NativeStep::Complete {result}=restored.advance(&final_receipt).unwrap() else {panic!("exact final readback did not complete");};
    assert_eq!(result["call_id"],"original-model-call");assert_eq!(result["installed"],true);assert_eq!(result["ok"],true,"{result}");
    assert_eq!(result["receipts"].as_array().unwrap().len(),3);assert_eq!(result["work_completed"],false);
    let NativeStep::AmendmentEffect { pending:legacy_activation, .. } = legacy.advance(&receipt(&legacy,&dependency)).unwrap() else { panic!("legacy dependency settlement lost activation"); };
    assert_eq!(json!(legacy_activation)["state"]["phase"], "current_run");
    let NativeStep::Effect { pending:legacy_readback, .. } = legacy_activation.advance(&receipt(&legacy_activation,&activation)).unwrap() else { panic!("legacy activation lost readback"); };
    let NativeStep::Complete { result:legacy_result } = legacy_readback.advance(&final_receipt).unwrap() else { panic!("legacy readback did not settle"); };
    assert_eq!(legacy_result, result);
}

#[test]
fn creation_uses_admitted_coordinates_after_the_authoring_frontier_advances() {
    let root = tempfile::tempdir().unwrap();
    let request = support::fixture(root.path());
    let admitted = request.admitted_parent_record;
    let mut current = admitted.clone();
    current.checkpoints.clear();
    current.active_checkpoints.clear();
    current.interface_state.insert("world_provider_fixture".into(), BTreeMap::from([("context_generation".into(), json!(7))]));
    let current_before = current.clone();
    let catalog_ref = ArtifactRef { id: "catalog".into(), content: ContentHash::of_bytes(b"catalog") };
    let mut metadata = json!(request.metadata);
    metadata["artifact_binding_refs"] = json!([catalog_ref]);
    metadata["grant"] = json!({
        "artifact_access":{"catalog":{"read":true}},"artifact_verbs":["create"],
        "tools":["file.write","artifact.send"],
        "interface_affordance_specs":[
            {"name":"file.write","description":"Write files","args_schema":{"type":"object"}},
            {"name":"artifact.send","description":"Send artifact","args_schema":{"type":"object"}}
        ]
    });
    let metadata: InvocationMetadata = serde_json::from_value(metadata).unwrap();
    let mut affordances = RunAffordances {
        config: RunAffordanceConfig {
            catalog_artifact:catalog_ref, file_write_tool:Some("file.write".into()), file_read_tool:None,
            artifact_send_tool:Some("artifact.send".into()), roots:Some(request.roots.clone()),
            defaults:Some(RunDefaults {
                client_template_interface_id:None,
                output_selection:None, ee_id:Some(request.ee_id), worker_template_interface_id:request.worker_template_interface_id,
                provider_interface_id:request.provider_interface_id, model:request.model, reasoning_effort:request.reasoning_effort,
                service_tier:None,
                authoring:request.authoring, worker_environment_kinds:request.selected_worker_environment_kinds,
                tools:request.selected_tools, door_host_slots:request.door_host_slots, context_artifacts:vec![],
                external_inputs:vec![], external_relays:vec![], model_outputs:vec![], publications:vec![],
                return_interface_ids:vec![], return_to_source:true,
            }),
        },
        catalog: RunCatalog { id:"catalog".into(), label:"provider.run.affordances".into(), text:None,
            operations:vec![RunCatalogEntry { name:"create_work".into(), description:"Create work".into(),
                input_schema:json!({"type":"object"}), operation:RunOperation::CreateRun }] },
        enabled:BTreeSet::from(["create_work".into()]),
    };
    let args = json!({"text":"Independent verification"});
    let step = affordances.plan_call(&metadata, &admitted, &current, "call-1", "create_work", &args).unwrap();
    let NativeStep::Effect { effect, pending, .. } = step else { panic!("expected file authoring"); };
    assert_eq!(effect.tool, "file.write");
    assert!(effect.arguments["writes"].as_array().unwrap().iter().all(|write|
        write["path"] != json!(request.roots.run_records_root.join("caller.json"))));
    let NativePhase::PinnedApplying {
        outgoing_bindings,
        seed,
        ..
    } = pending.state
    else {
        panic!("expected checked writes before seed");
    };
    assert_eq!(seed.unwrap().tool, "artifact.send");
    assert_eq!(outgoing_bindings.len(), 1);
    assert_eq!(current, current_before);
    assert!(!request.roots.run_records_root.exists());
    assert!(affordances.plan_call(&metadata, &current, &current, "call-2", "create_work", &args).unwrap_err().contains("admitted parent record disagree"));
    let mut stale = metadata.clone();
    stale.runtime.as_mut().unwrap().epoch += 1;
    assert!(affordances.plan_call(&stale, &admitted, &current, "call-3", "create_work", &args).unwrap_err().contains("epoch is stale"));
    affordances.config.defaults.as_mut().unwrap().context_artifacts = vec![ArtifactRef { id: "unbound-creation-document".into(), content: ContentHash::of_bytes(b"document") }];
    let input = InterfaceInvocationEnvelope {
        artifact: affordances.config.catalog_artifact.clone(),
        artifact_file: octopus_interfaces::InterfaceArtifactFile::write(root.path(), "catalog-load", b"{}").unwrap(),
        artifact_binding_payloads: vec![octopus_interfaces::ArtifactBindingPayload::new(
            affordances.config.catalog_artifact.clone(), serde_json::to_vec(&affordances.catalog).unwrap())],
        metadata: Some(metadata.clone()),
    };
    let loaded = RunAffordances::from_invocation(&input, &json!({"run_affordances": affordances.config})).unwrap().unwrap();
    assert!(loaded.contains("create_work"));
    assert!(loaded.plan_call(&metadata, &admitted, &current, "call-unbound", "create_work", &args).unwrap_err().contains("not an exact readable metadata binding"));

}

fn exact_affordances(request: &RunCompositionRequest) -> (RunAffordances, InvocationMetadata) {
    let reference = ArtifactRef {
        id: "exact-catalog".into(),
        content: ContentHash::of_bytes(b"catalog"),
    };
    let mut metadata = json!(request.metadata);
    metadata["artifact_binding_refs"] = json!([reference]);
    metadata["grant"] = json!({"artifact_access":{"exact-catalog":{"read":true}},"artifact_verbs":["create"],
        "tools":["file.write","artifact.send"],"interface_affordance_specs":[
            {"name":"file.write","description":"Write","args_schema":{"type":"object"}},
            {"name":"artifact.send","description":"Send","args_schema":{"type":"object"}}]});
    let operations = [
        ("preview_run", RunOperation::PreviewRun),
        ("create_run", RunOperation::CreateRun),
        ("preview_revision", RunOperation::PreviewRevision),
        ("revise_run", RunOperation::ReviseRun),
        ("inspect_authoring", RunOperation::InspectAuthoring),
    ]
    .map(|(name, operation)| RunCatalogEntry {
        name: name.into(),
        operation,
        description: name.into(),
        input_schema: json!({"type":"object"}),
    });
    let enabled = operations.iter().map(|entry| entry.name.clone()).collect();
    (
        RunAffordances {
            config: RunAffordanceConfig {
                catalog_artifact: reference,
                file_read_tool: None,
                file_write_tool: Some("file.write".into()),
                artifact_send_tool: Some("artifact.send".into()),
                roots: Some(request.roots.clone()),
                defaults: None,
            },
            catalog: RunCatalog {
                id: "exact-catalog".into(),
                label: "provider.run.affordances".into(),
                text: None,
                operations: operations.into(),
            },
            enabled,
        },
        serde_json::from_value(metadata).unwrap(),
    )
}

#[test]
fn full_authored_run_preserves_wait_return_and_verification_then_checks_real_ordered_readback() {
    use octopus_interfaces::InterfaceArtifactFile;
    use octopus_operator_chat_client::run_composition::RegistryCandidateWrite;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let request = support::fixture(&root);
    support::write(
        &root,
        "registry/mark/task.summary.received/mark.json",
        json!({"id":"task.summary.received","package":"task.summary.received","class":"success"}),
    );
    let plan = plan_run_composition(&request).unwrap();
    let mut authored = AuthoredRunComposition {
        run_id: plan.run_id.clone(),
        writes: composition_candidate_writes(&plan),
        publication_read_refs: Vec::new(),
        outgoing_bindings: vec![ProviderOutgoingBinding {
            interface: plan.receive_interface_id.clone().into(),
            artifact_label: plan.seed.label.clone(),
            artifact_bindings: vec!["role-system_worker_template".into()],
        }],
        seed: Some(plan.seed.clone()),
    };
    let policy = authored
        .writes
        .iter_mut()
        .find(|write| {
            write
                .path
                .ends_with(format!("{}/policy.json", plan.worker_interface_id))
        })
        .unwrap();
    let mut value: Value = serde_json::from_str(&policy.content).unwrap();
    value["ceiling"]["mark_outcomes"]
        .as_array_mut()
        .unwrap()
        .push(json!("task.summary.received"));
    policy.content = serde_json::to_string(&value).unwrap();
    let original = request.roots.registry_root.join("interface/system_worker_template/policy.json");
    let mut held: Value = serde_json::from_slice(&std::fs::read(&original).unwrap()).unwrap();
    held["ceiling"]["mark_outcomes"].as_array_mut().unwrap().push(json!("task.summary.received"));
    std::fs::write(&original, serde_json::to_vec(&held).unwrap()).unwrap();
    let run_write = authored.writes.last_mut().unwrap();
    let mut run: Value = serde_json::from_str(&run_write.content).unwrap();
    let context = run["checkpoints"]
        .as_array()
        .unwrap()
        .iter()
        .find(|cp| cp["interface_binding"]["interface"] == plan.receive_interface_id)
        .unwrap()["artifact_bindings"]
        .clone();
    let wait = json!({"id":"await-summary","run":plan.run_id,"trace":run["trace"],"epoch":0,
        "interface_binding":octopus_runtime::AuthoredInterfaceBinding::all("system_worker_parent".into()),
        "outbound_interface_bindings":[octopus_runtime::AuthoredInterfaceBinding::all("system_worker_parent".into())],
        "artifact_bindings":context,"required_verified_artifacts":[{"mark":"task.summary.received","artifact_label":"task.summary", "verifier":"artifact_exists","params":{"explicit_obligation":"Keep the result evidence"}}]});
    run["checkpoints"]
        .as_array_mut()
        .unwrap()
        .push(wait.clone());
    let expected_wait: octopus_run_record::CheckpointRecord =
        serde_json::from_value(wait.clone()).unwrap();
    run["active_checkpoints"]
        .as_array_mut()
        .unwrap()
        .push(json!("await-summary"));
    run_write.content = serde_json::to_string(&run).unwrap();
    let (affordances, metadata) = exact_affordances(&request);
    let args = |authored: &AuthoredRunComposition| json!({"creation_key":"full-contract","authored":authored});
    let NativeStep::Complete { result: missing } = affordances
        .plan_call(
            &metadata,
            &request.admitted_parent_record,
            &request.admitted_parent_record,
            "preview-1",
            "preview_run",
            &args(&authored),
        )
        .unwrap()
    else {
        panic!("preview mutated")
    };
    assert_eq!(missing["installed"], false);
    assert_eq!(missing["ok"], false);
    assert!(
        missing["validation"]["regressions"]
            .to_string()
            .contains("artifact_exists"),
        "{}",
        missing["validation"]
    );
    assert!(!plan.run_write.path.exists());
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../../..");
    let insertion = authored.writes.len() - 1;
    for (offset, file) in ["verifier.json", "verify.py"].iter().enumerate() {
        authored.writes.insert(
            insertion + offset,
            RegistryCandidateWrite {
                path: request
                    .roots
                    .registry_root
                    .join("verifier/artifact_exists")
                    .join(file),
                content: std::fs::read_to_string(
                    source.join("registry/verifier/artifact_exists").join(file),
                )
                .unwrap(),
                expected_before_hash: None,
            },
        );
    }
    let NativeStep::Complete { result: preview } = affordances
        .plan_call(
            &metadata,
            &request.admitted_parent_record,
            &request.admitted_parent_record,
            "preview-2",
            "preview_run",
            &args(&authored),
        )
        .unwrap()
    else {
        panic!("preview mutated")
    };
    assert_eq!(preview["ok"], true, "{}", preview["validation"]);
    assert!(
        preview["run_contract"]["checkpoints"]
            .as_array()
            .unwrap()
            .contains(&json!(expected_wait))
    );
    let NativeStep::Effect {
        effect,
        pending,
        outgoing_bindings,
    } = affordances
        .plan_call(
            &metadata,
            &request.admitted_parent_record,
            &request.admitted_parent_record,
            "apply",
            "create_run",
            &args(&authored),
        )
        .unwrap()
    else {
        panic!("create refused")
    };
    assert!(outgoing_bindings.is_empty());
    assert_eq!(effect.arguments["ordered"], true);
    let mut legacy_wire = json!(pending);
    legacy_wire["state"]["phase"] = json!("applying");
    legacy_wire["state"].as_object_mut().unwrap().remove("publication_read_refs");
    let legacy: NativePending = serde_json::from_value(legacy_wire.clone()).unwrap();
    assert_eq!(json!(legacy), legacy_wire);
    let payload = serde_json::to_vec(&json!({"tool":effect.tool,"arguments":effect.arguments,"correlation_id":effect.correlation_id})).unwrap();
    let kinds = vec!["system_room".to_string()];
    let envelope = InterfaceInvocationEnvelope {
        artifact: ArtifactRef {
            id: "tool.call".into(),
            content: ContentHash::of_bytes(&payload),
        },
        artifact_file: InterfaceArtifactFile::write(root.join("calls"), "tool.call", &payload)
            .unwrap(),
        artifact_binding_payloads: vec![],
        metadata: Some(
            serde_json::from_value(json!({"grant":{"worker_environment_kinds":kinds}})).unwrap(),
        ),
    };
    let execute = || {
        octopus_system_room_env::execute_file_write_result(
            &envelope,
            &effect.arguments,
            &kinds,
            &BTreeMap::from([
                ("registry_root".into(), request.roots.registry_root.clone()),
                (
                    "ee_instances_root".into(),
                    request.roots.ee_instances_root.clone(),
                ),
                (
                    "run_records_root".into(),
                    request.roots.run_records_root.clone(),
                ),
            ]),
            &root.join("effects"),
            8 * 1024 * 1024,
        )
        .unwrap()
    };
    let body = execute();
    assert_eq!(body, execute());
    let receipt = json!(ToolResultArtifact::success(
        &effect.tool,
        Some(effect.correlation_id.clone()),
        body
    ));
    std::fs::write(
        &plan.run_write.path,
        format!("{}\n", authored.writes.last().unwrap().content),
    )
    .unwrap();
    assert!(pending.advance(&receipt).unwrap_err().contains("readback"));
    std::fs::write(
        &plan.run_write.path,
        &authored.writes.last().unwrap().content,
    )
    .unwrap();
    let NativeStep::Effect {
        effect: seed,
        pending: seed_pending,
        outgoing_bindings,
    } = pending.advance(&receipt).unwrap()
    else {
        panic!("no seed after exact evidence")
    };
    assert_eq!(outgoing_bindings.len(), 1);
    assert_eq!(seed.arguments["payload"], plan.seed.payload);
    let NativeStep::Effect { effect:legacy_seed, pending:legacy_seeding, .. } = legacy.advance(&receipt).unwrap() else { panic!("legacy applied receipt must continue directly to seed"); };
    assert_eq!(json!(legacy_seed), json!(seed));
    let legacy_wire = json!(legacy_seeding);
    assert_eq!(legacy_wire["state"]["phase"], "applied_seeding");
    assert!(legacy_wire["state"].get("publication_read_refs").is_none());
    let legacy_seeding: NativePending = serde_json::from_value(legacy_wire.clone()).unwrap();
    assert_eq!(json!(legacy_seeding), legacy_wire);
    let NativeStep::Complete { result:legacy_result } = legacy_seeding.advance(&json!(ToolResultArtifact::success(
        &legacy_seed.tool, Some(legacy_seed.correlation_id), json!({"admitted":{"seq":17}})))).unwrap() else { panic!("legacy transfer receipt did not settle"); };
    assert_eq!(legacy_result["seed_admitted"], true);
    assert_eq!(legacy_result["call_id"], legacy.call_id);
    let installed = RunRecord::load(&plan.run_id.clone().into(), &request.roots.run_records_root)
        .unwrap()
        .unwrap();
    assert_eq!(
        installed.checkpoint(&"await-summary".into()).unwrap(),
        &expected_wait
    );
    assert_eq!(
        std::fs::read_to_string(&plan.run_write.path).unwrap(),
        authored.writes.last().unwrap().content
    );
    let NativeStep::Complete { result } = seed_pending
        .advance(&json!(ToolResultArtifact::success(
            &seed.tool,
            Some(seed.correlation_id),
            json!({"admitted":{"seq":17}})
        )))
        .unwrap()
    else {
        panic!("seed result absent")
    };
    assert_eq!(result["seed_admitted"], true);
    assert_eq!(result["work_completed"], false);
}

#[test]
fn exact_runs_allow_no_provider_or_client_and_revision_preserves_existing_contracts() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let request = support::fixture(&root);
    let (affordances, metadata) = exact_affordances(&request);
    let run = json!({"id":"parked","ee":"ee_existence","worker":"system_worker_template","trace":"parked-trace","status":"idle",
        "checkpoints":[{"id":"wait","run":"parked","trace":"parked-trace","epoch":0,
            "interface_binding":octopus_runtime::AuthoredInterfaceBinding::all("system_worker_parent".into()),
            "artifact_bindings":[{"id":"role-system_worker_template"},{"id":"role-system_worker_parent"}]}],"active_checkpoints":["wait"]});
    let authored = json!({"run_id":"parked","outgoing_bindings":[],"writes":[{"path":request.roots.run_records_root.join("parked.json"),"content":serde_json::to_string(&run).unwrap(),"expected_before_hash":null}]});
    let NativeStep::Complete { result } = affordances
        .plan_call(
            &metadata,
            &request.admitted_parent_record,
            &request.admitted_parent_record,
            "parked-preview",
            "preview_run",
            &json!({"creation_key":"parked","authored":authored}),
        )
        .unwrap()
    else {
        panic!("preview mutated")
    };
    assert_eq!(result["ok"], true, "{}", result["validation"]);
    assert!(
        result["run_contract"]["interface_state"]
            .as_object()
            .unwrap()
            .is_empty()
    );
    let before = serde_json::to_vec(&run).unwrap();
    let mut extended = run.clone();
    let mut next = run["checkpoints"][0].clone();
    next["id"] = json!("wait-next");
    next["epoch"] = json!(1);
    extended["checkpoints"].as_array_mut().unwrap().push(next);
    extended["active_checkpoints"]
        .as_array_mut()
        .unwrap()
        .push(json!("wait-next"));
    octopus_operator_chat_client::run_revision::validate_checkpoint_extension(
        &before,
        &serde_json::to_vec(&extended).unwrap(),
    )
    .unwrap();
    let run_path = request.roots.run_records_root.join("parked.json");
    std::fs::create_dir_all(&request.roots.run_records_root).unwrap();
    std::fs::write(&run_path, &before).unwrap();
    let args = json!({"creation_key":"extend-wait","authored":{"run_id":"parked","outgoing_bindings":[],
        "writes":[{"path":run_path,"content":serde_json::to_string(&extended).unwrap(),
            "expected_before_hash":ContentHash::of_bytes(&before).to_hex()}]}});
    let NativeStep::Complete { result } = affordances
        .plan_call(
            &metadata,
            &request.admitted_parent_record,
            &request.admitted_parent_record,
            "revision-preview",
            "preview_revision",
            &args,
        )
        .unwrap()
    else {
        panic!("revision preview mutated")
    };
    assert_eq!(
        result["ok"], true,
        "{}",
        result["validation"]["regressions"]
    );
    let NativeStep::Effect {
        effect,
        pending,
        outgoing_bindings,
    } = affordances
        .plan_call(
            &metadata,
            &request.admitted_parent_record,
            &request.admitted_parent_record,
            "revision-apply",
            "revise_run",
            &args,
        )
        .unwrap()
    else {
        panic!("revision refused")
    };
    assert!(outgoing_bindings.is_empty());
    let payload = serde_json::to_vec(&json!({"tool":effect.tool,"arguments":effect.arguments,"correlation_id":effect.correlation_id})).unwrap();
    let kinds = vec!["system_room".to_string()];
    let envelope = InterfaceInvocationEnvelope {
        artifact: ArtifactRef {
            id: "tool.call".into(),
            content: ContentHash::of_bytes(&payload),
        },
        artifact_file: octopus_interfaces::InterfaceArtifactFile::write(
            root.join("revision-calls"),
            "tool.call",
            &payload,
        )
        .unwrap(),
        artifact_binding_payloads: vec![],
        metadata: Some(
            serde_json::from_value(json!({"grant":{"worker_environment_kinds":kinds}})).unwrap(),
        ),
    };
    let body = octopus_system_room_env::execute_file_write_result(
        &envelope,
        &effect.arguments,
        &kinds,
        &BTreeMap::from([(
            "run_records_root".into(),
            request.roots.run_records_root.clone(),
        )]),
        &root.join("revision-effects"),
        8 * 1024 * 1024,
    )
    .unwrap();
    let NativeStep::Complete { result } = pending
        .advance(&json!(ToolResultArtifact::success(
            &effect.tool,
            Some(effect.correlation_id),
            body,
        )))
        .unwrap()
    else {
        panic!("parked revision requested an undeclared emission")
    };
    assert_eq!(result["installed"], true);
    assert_eq!(result["seed_admitted"], false);
    assert_eq!(result["work_completed"], false);
    let installed = RunRecord::from_bytes(&std::fs::read(&run_path).unwrap()).unwrap();
    assert_eq!(
        installed.checkpoints,
        RunRecord::from_bytes(&serde_json::to_vec(&extended).unwrap())
            .unwrap()
            .checkpoints
    );
    extended["checkpoints"][0]["epoch"] = json!(1);
    assert!(
        octopus_operator_chat_client::run_revision::validate_checkpoint_extension(
            &before,
            &serde_json::to_vec(&extended).unwrap()
        )
        .is_err()
    );
}
