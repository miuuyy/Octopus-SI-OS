use super::*;
use octopus_app_server::{
    SharedBlobHandle, SharedBlobs, SpineRootConfig, admit_interface_artifact_arrival,
    build_embedded_spine,
};
use octopus_interfaces::InterfaceArtifactFile;
use octopus_ledger::{Endpoint, LiveMovementLog, MovementKind, MovementLog, Seq};
use octopus_ledger_store::{DurableBlobs, DurableLog};
use octopus_interface_transport::{InterfaceHostIoLimits, InterfaceLiftContext};
use octopus_run_authoring::ProviderHandoffSession;
use octopus_world_pyecho_life_runtime::LedgerTailReader;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

fn install(write: &PlannedClientWrite) {
    std::fs::create_dir_all(write.path.parent().unwrap()).unwrap();
    std::fs::write(&write.path, &write.content).unwrap();
}

#[tokio::test]
async fn home_accepts_independent_task_terminal_and_error_returns_through_real_arrival_policy() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let registry = root.join("registry");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../..");
    tests::copy_planner_descriptors(&source.join("registry"), &registry);
    let ees = root.join("ees");
    std::fs::create_dir(&ees).unwrap();
    tests::boot_operator_space_ee_instances(&ees);
    let runs = root.join("runs");
    std::fs::create_dir(&runs).unwrap();
    let request: OperatorChatPlanRequest = serde_json::from_value(json!({
        "registry_root":registry,"ee_instances_root":ees,"run_records_root":runs,"interface_private_state_root":root.join("interface-private-state"),"write_hash_algorithm":"sha256",
        "chat_id":"home_returns","provider_interface_id":"world_provider_codex","ee_id":"ee_existence",
        "worker_interface_id":"system_worker_operator_home","client_surface_role":"home","cycle":"first",
        "selected_worker_environment_kinds":["system_room"],
        "context_artifact_bindings":[{"id":"artifact-code-mode-browser-prelude"},{"id":"artifact-code-mode-cua-prelude"}],
        "conversation":{"ledger_offset":0,"artifact_bindings":[],"pinned_artifact_bindings":[],"pinned_context_items":[],"continued_from":null}
    })).unwrap();
    let plan = plan_operator_chat_topology(&request).unwrap();
    for write in plan.interface_writes.iter().chain(&plan.run_writes) {
        install(write);
    }
    let mut home: Value = serde_json::from_str(&plan.run_record_content).unwrap();
    assert!(home["interface_state"][&plan.model_interface_id]["run_authoring_binding"]["checkpoints"].as_array().unwrap().iter().any(|row| row["role"] == "provider_tool_output" && row["required_verified_artifacts"][0]["params"]["tools"].as_array().unwrap().contains(&json!(plan.capability_tools["artifact.read"])) && row["include_context"] == true));
    let path = runs.join(format!("{}.json", plan.run_id));
    std::fs::write(&path, &plan.run_record_content).unwrap();
    let voice = home["interface_state"][&plan.chat_interface_id]["voice_context"].clone();
    assert_ne!(voice["delegation_interface_id"], plan.model_interface_id);
    assert_ne!(voice["run_id"], plan.run_id);
    let voice_path = runs.join(format!("{}.json", voice["run_id"].as_str().unwrap()));
    let voice_before = std::fs::read(&voice_path).unwrap();
    let voice_record = RunRecord::from_bytes(&voice_before).unwrap();
    let voice_templates = voice_record.interface_state[&voice["provider_id"].as_str().unwrap().into()]["run_authoring_binding"]["checkpoints"]
        .as_array().unwrap();
    let voice_template = voice_templates.iter().find(|cp| cp["role"] == "stable").unwrap();
    assert!(voice_template["static_artifact_bindings"].as_array().unwrap().contains(&json!("artifact-code-mode-browser-prelude")));
    assert!(voice_template["static_artifact_bindings"].as_array().unwrap().contains(&json!("artifact-code-mode-cua-prelude")));
    assert!(home["interface_state"].get(voice["provider_id"].as_str().unwrap()).is_none());
    assert!(voice_record.interface_state.get(&plan.model_interface_id.clone().into()).is_none());
    let sidecar: Value = serde_json::from_slice(
        &std::fs::read(
            registry
                .join("interface")
                .join(&plan.chat_interface_id)
                .join("octopus_os.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(sidecar["voice_context"], voice);
    for tool in [
        "file.read",
        "file.list",
        "file.write",
        "artifact.send",
        "artifact.read",
    ] {
        assert!(plan.capability_tools.contains_key(tool), "{tool}");
    }
    let writer: Value = serde_json::from_slice(
        &std::fs::read(
            registry
                .join("tool")
                .join(&plan.capability_tools["file.write"])
                .join("tool.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_ne!(writer["interface"], plan.authoring_environment_interface_id);
    let blobs: SharedBlobs = Arc::new(Mutex::new(DurableBlobs::open(root.join("ledger/blobs")).unwrap()));
    let embedded = build_embedded_spine(SpineRootConfig {
        registry_root: registry.clone(),
        run: plan.run_id.clone().into(),
        blobs: blobs.clone(),
        interface_artifact_root: root.join("artifacts"),
        run_records_root: runs.clone(),
        ee_instances_root: ees.clone(),
    })
    .await
    .unwrap();
    let mut log = LiveMovementLog::new(DurableLog::open(root.join("ledger/movements.jsonl")).unwrap());
    let mut handle = SharedBlobHandle::new(blobs);
    let input = InterfaceArtifactFile::write(
        root.join("operator"),
        "operator.message",
        br#"{"text":"Please delegate an independent task"}"#,
    )
    .unwrap()
    .capture_bounded(1048576, 67108864)
    .unwrap();
    let admitted = admit_interface_artifact_arrival(
        &embedded,
        &mut log,
        &mut handle,
        &input,
        &plan.chat_interface_id.clone().into(),
    )
    .await
    .unwrap();
    let metadata = admitted
        .admission
        .records
        .iter()
        .flat_map(|r| &r.outbound_transfers)
        .find(|t| t.to_interface.as_str() == plan.model_interface_id)
        .unwrap()
        .invocation_metadata
        .clone();
    let toolset = &metadata.grant.tools;
    for tool in ["file.read", "file.list", "file.write", "artifact.send"] {
        assert!(toolset.contains(&plan.capability_tools[tool]));
    }
    let mut tasks = Vec::new();
    for (index, label) in ["interface.artifact.output", "turn.error"]
        .into_iter()
        .enumerate()
    {
        use run_composition::*;
        let task = plan_run_composition(&RunCompositionRequest {
            roots:CompositionRoots {registry_root:registry.clone(),ee_instances_root:ees.clone(),run_records_root:runs.clone(),interface_private_state_root:root.join("private")},
            metadata:metadata.clone(),admitted_parent_record:RunRecord::from_bytes(plan.run_record_content.as_bytes()).unwrap(),identity_seed:format!("task_{index}"),ee_id:"ee_existence".into(),
            worker_template_interface_id:"system_worker_coder_max".into(),provider_interface_id:"world_provider_ollama".into(),model:serde_json::from_slice::<Value>(&std::fs::read(registry.join("interface/world_provider_ollama/interface.json")).unwrap()).unwrap()["interface_driver_config"]["private_config"]["model"].as_str().unwrap().into(),reasoning_effort:String::new(),service_tier:None,
            authoring:CompositionAuthoring {source_interface_id:"system_interface_authoring".into(),room_interface_id:"system_room".into(),tool_id:"file.write".into(),tool_call_label:"tool.call".into(),tool_call_mark:"tool.call.allowed".into(),tool_call_verifier:"tool_call_allowed".into()},
            communication:CompositionCommunication {client_template_interface_id:None,client_title:None,receive_interface_id:format!("system_worker_task_{index}"),return_interface_ids:vec![plan.worker_interface_id.clone()],client_interface_id:None,output_selection:Some(CompositionOutputSelection {
                return_labels:vec!["interface.artifact.output".into(),"turn.error".into()],
                record_labels:serde_json::from_slice::<Value>(&std::fs::read(registry.join("interface/world_provider_ollama/interface.json")).unwrap()).unwrap()["interface_driver_config"]["private_config"]["client_projection_output_labels"].as_array().unwrap().iter().filter_map(|v| {let name=v.as_str().unwrap();(!["interface.artifact.output","turn.error"].contains(&name)).then(||name.to_owned())}).collect()
            })},selected_worker_environment_kinds:vec!["system_room".into()],selected_tools:Vec::new(),door_host_slots:BTreeMap::new(),external_inputs:Vec::new(),external_relays:Vec::new(),model_outputs:Vec::new(),context_artifact_bindings:Vec::new(),payload_artifact_bindings:Vec::new(),publications:Vec::new(),seed:CompositionSeed {label:"task.assignment".into(),payload:json!({"objective":"Independent Task","return_interface":plan.worker_interface_id})}
        }).unwrap();
        for write in task
            .descriptor_writes
            .iter()
            .chain(task.required_policy_updates.iter().map(|u| &u.write))
            .chain(std::iter::once(&task.run_write))
        {
            install(write);
        }
        let binding = &task.required_binding_updates[0];
        let template = bound_checkpoint_template(
            BoundCheckpointRole::ProviderInterfaceOutput,
            AuthoredInterfaceBinding {
                interface: plan.model_interface_id.clone().into(),
                config: InterfaceBindingConfig {
                    artifact_policy: BindingArtifactPolicy::Allow {
                        names: vec![task.seed.label.clone()],
                        namespaces: Vec::new(),
                    },
                },
            },
            vec![AuthoredInterfaceBinding {
                interface: task.receive_interface_id.clone().into(),
                config: InterfaceBindingConfig {
                    artifact_policy: BindingArtifactPolicy::Allow {
                        names: vec![task.seed.label.clone()],
                        namespaces: Vec::new(),
                    },
                },
            }],
            Vec::new(),
        )
        .unwrap();
        let mut template = template;
        let mut artifacts = home["checkpoints"][0]["artifact_bindings"]
            .as_array()
            .unwrap()
            .clone();
        for role in &binding.required_artifact_bindings {
            if !artifacts.iter().any(|a| a["id"] == role.id) {
                artifacts.push(json!({"id":role.id}));
            }
        }
        template.static_artifact_bindings = artifacts
            .iter()
            .map(|a| octopus_descriptors::ArtifactId::new(a["id"].as_str().unwrap()))
            .collect();
        home["checkpoints"].as_array_mut().unwrap().push(json!({"id":format!("address_{index}"),"run":plan.run_id,"trace":"authored-task-address","epoch":0,"interface_binding":template.interface_binding,"outbound_interface_bindings":template.outbound_interface_bindings,"artifact_bindings":artifacts,"required_verified_artifacts":[]}));
        home["active_checkpoints"]
            .as_array_mut()
            .unwrap()
            .push(json!(format!("address_{index}")));
        home["interface_state"][&plan.model_interface_id]["frontier_checkpoint_ids"]
            .as_array_mut()
            .unwrap()
            .push(json!(format!("address_{index}")));
        home["interface_state"][&plan.model_interface_id]["run_authoring_binding"]["checkpoints"]
            .as_array_mut()
            .unwrap()
            .push(json!(template));
        tasks.push((task, label));
    }
    std::fs::write(&path, serde_json::to_vec_pretty(&home).unwrap()).unwrap();
    let task_records = tasks
        .iter()
        .map(|(task, _)| std::fs::read(&task.run_write.path).unwrap())
        .collect::<Vec<_>>();
    let mut next = request;
    next.cycle = "next".into();
    next.existing_run_record = Some(home.clone());
    for replace in [false, true] {
        let mut altered: Value = serde_json::from_slice(&voice_before).unwrap();
        let seeds = altered["interface_state"][voice["provider_id"].as_str().unwrap()]["run_authoring_binding"]["checkpoints"][0]["static_artifact_bindings"].as_array_mut().unwrap();
        let index = seeds.iter().position(|id| id == "artifact-code-mode-browser-prelude").unwrap();
        if replace {
            seeds[index] = json!("forged-static-seed");
            for cp in altered["checkpoints"].as_array_mut().unwrap() {
                for binding in cp["artifact_bindings"].as_array_mut().unwrap() {
                    if binding["id"] == "artifact-code-mode-browser-prelude" { binding["id"] = json!("forged-static-seed"); }
                }
            }
        } else { seeds.remove(index); }
        let bytes = serde_json::to_vec(&altered).unwrap();
        octopus_run_authoring::provider_context_bindings(&RunRecord::from_bytes(&bytes).unwrap(), &voice["provider_id"].as_str().unwrap().into()).unwrap();
        std::fs::write(&voice_path, bytes).unwrap();
        assert!(plan_operator_chat_topology(&next).unwrap_err().contains("Home voice Run differs from its exact independent execution contract"));
    }
    let mut legacy: Value = serde_json::from_slice(&voice_before).unwrap();
    legacy["interface_state"][voice["control_interface_id"].as_str().unwrap()].as_object_mut().unwrap().remove("static_artifact_bindings");
    std::fs::write(&voice_path, serde_json::to_vec(&legacy).unwrap()).unwrap();
    assert!(plan_operator_chat_topology(&next).unwrap_err().contains("explicit topology repair is required"));
    std::fs::write(&voice_path, &voice_before).unwrap();
    let continued = plan_operator_chat_topology(&next).unwrap();
    for write in continued.interface_writes.iter().chain(&continued.run_writes) {
        install(write);
    }
    std::fs::write(&path, &continued.run_record_content).unwrap();
    let current = RunRecord::from_bytes(continued.run_record_content.as_bytes()).unwrap();
    octopus_run_authoring::provider_context_bindings(
        &voice_record,
        &voice["provider_id"].as_str().unwrap().into(),
    )
    .unwrap();
    assert_eq!(std::fs::read(&voice_path).unwrap(), voice_before, "Home Task amendments never rewrite Voice");
    let room = &plan.capability_doors["system_room"];
    let emitter = current
        .checkpoints
        .iter()
        .find(|cp| {
            cp.outbound_interfaces().any(|id| id.as_str() == room)
                && cp.required_verified_artifacts.iter().any(|r| {
                    r.params["tools"].as_array().is_some_and(|ids| {
                        ids.contains(&json!(plan.capability_tools["artifact.send"]))
                    })
                })
        })
        .unwrap();
    embedded
        .reload_registry_snapshot_on_artifact_arrival()
        .unwrap();
    for (task, _) in &tasks {
        let file = InterfaceArtifactFile::write(
            root.join(format!("seed-{}", task.run_id)),
            &task.seed.label,
            &serde_json::to_vec(&task.seed.payload).unwrap(),
        )
        .unwrap()
        .capture_bounded(1048576, 67108864)
        .unwrap();
        let sent = octopus_app_server::InterfaceArtifactArrivalFromInterface::new(
            &embedded.runtime,
            &mut log,
            &mut handle,
            embedded.arrival_dedup.clone(),
            current.trace.clone(),
            room.clone().into(),
        )
        .emit_artifact_to_interface(
            &file,
            &current.id,
            &emitter.id,
            emitter.epoch,
            octopus_ledger::Lamport(0),
            &task.receive_interface_id.clone().into(),
        )
        .await
        .unwrap();
        assert!(sent.outbound_movement.is_some());
        assert!(
            log.read_from(Seq(0))
                .iter()
                .any(|m| m.kind == MovementKind::InterfaceOutbound
                    && m.to == Endpoint::Interface(task.receive_interface_id.clone().into()))
        );
    }
    let second = InterfaceArtifactFile::write(
        root.join("second"),
        "operator.message",
        br#"{"text":"Are you still available?"}"#,
    )
    .unwrap()
    .capture_bounded(1048576, 67108864)
    .unwrap();
    admit_interface_artifact_arrival(
        &embedded,
        &mut log,
        &mut handle,
        &second,
        &plan.chat_interface_id.clone().into(),
    )
    .await
    .expect("Home input is admitted while both Tasks have no terminal outcome");
    for ((task, label), before) in tasks.iter().zip(&task_records) {
        assert_eq!(&std::fs::read(&task.run_write.path).unwrap(), before);
        let first_seq = log.len();
        let result = InterfaceArtifactFile::write(root.join(format!("result-{}",task.run_id)),label,&serde_json::to_vec(&json!({"text":"authored task outcome","status":if *label=="turn.error" {"failed"} else {"completed"}})).unwrap()).unwrap().capture_bounded(1048576,67108864).unwrap();
        admit_interface_artifact_arrival(
            &embedded,
            &mut log,
            &mut handle,
            &result,
            &task.provider_interface_id.clone().into(),
        )
        .await
        .unwrap();
        assert!(
            log.read_from(Seq(first_seq as u64))
                .iter()
                .any(|m| m.kind == MovementKind::InterfaceOutbound
                    && m.to == Endpoint::Interface(plan.model_interface_id.clone().into())
                    && m.artifact.is_some()),
            "{label} reaches Home independently through the exact System return pin"
        );
        let mut value: Value = serde_json::from_str(&continued.run_record_content).unwrap();
        for cp in value["checkpoints"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .filter(|cp| cp["interface_binding"]["interface"] == plan.worker_interface_id)
        {
            cp["epoch"] = json!(1);
        }
        std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    }
    let mut idle: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    idle["status"] = json!("idle");
    std::fs::write(&path, serde_json::to_vec(&idle).unwrap()).unwrap();
    let delivery_id = idle["interface_state"][&plan.chat_interface_id]["delivery_artifact"]
        .as_str()
        .unwrap();
    let contract: Value = serde_json::from_slice(
        &std::fs::read(
            registry
                .join("artifact")
                .join(delivery_id)
                .join("artifact.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let widget_pin = contract["delivery"]["widget"]["interface"]
        .as_str()
        .unwrap();
    let (task, _) = &tasks[1];
    let policy_path = registry
        .join("interface")
        .join(&task.worker_interface_id)
        .join("policy.json");
    let mut policy: Value = serde_json::from_slice(&std::fs::read(&policy_path).unwrap()).unwrap();
    policy["ceiling"]["interfaces"]
        .as_array_mut()
        .unwrap()
        .push(json!(widget_pin));
    std::fs::write(policy_path, serde_json::to_vec(&policy).unwrap()).unwrap();
    let mut task_record: Value =
        serde_json::from_slice(&std::fs::read(&task.run_write.path).unwrap()).unwrap();
    let roles = task_record["checkpoints"][0]["artifact_bindings"].clone();
    task_record["checkpoints"].as_array_mut().unwrap().push(json!({"id":"widget_address","run":task.run_id,"trace":"task-widget","epoch":0,
        "interface_binding":{"interface":task.provider_interface_id,"config":{"artifact_policy":{"mode":"allow","names":["client.surface.document"],"namespaces":[]}}},
        "outbound_interface_bindings":[{"interface":widget_pin,"config":{"artifact_policy":{"mode":"allow","names":["client.surface.document"],"namespaces":[]}}}],"artifact_bindings":roles,"required_verified_artifacts":[]}));
    task_record["active_checkpoints"]
        .as_array_mut()
        .unwrap()
        .push(json!("widget_address"));
    std::fs::write(
        &task.run_write.path,
        serde_json::to_vec(&task_record).unwrap(),
    )
    .unwrap();
    let task_record = RunRecord::from_bytes(&serde_json::to_vec(&task_record).unwrap()).unwrap();
    let task_room = &task.authoring.capability_doors["system_room"];
    let task_emitter = task_record
        .checkpoints
        .iter()
        .find(|cp| cp.outbound_interfaces().any(|id| id.as_str() == task_room))
        .unwrap();
    embedded
        .reload_registry_snapshot_on_artifact_arrival()
        .unwrap();
    let widget_contract: Value = serde_json::from_slice(
        &std::fs::read(registry.join("artifact/artifact-home-widget-authoring/artifact.json"))
            .unwrap(),
    )
    .unwrap();
    let mut document = widget_contract["contract"]["example"].clone();
    document["glyph"] = json!("weather");
    document["revision"] = json!(2);
    document["render"]["density"] = json!("comfortable");
    document["data"] = json!({"summary":"Admitted weather fixture: 10 degrees"});
    let widget = InterfaceArtifactFile::write(
        root.join("task-widget"),
        "client.surface.document",
        &serde_json::to_vec(&document).unwrap(),
    )
    .unwrap()
    .capture_bounded(1048576, 67108864)
    .unwrap();
    let begin = log.len();
    let sent = octopus_app_server::InterfaceArtifactArrivalFromInterface::new(
        &embedded.runtime,
        &mut log,
        &mut handle,
        embedded.arrival_dedup.clone(),
        task_record.trace.clone(),
        task_room.clone().into(),
    )
    .emit_artifact_to_interface(
        &widget,
        &task_record.id,
        &task_emitter.id,
        task_emitter.epoch,
        octopus_ledger::Lamport(0),
        &widget_pin.into(),
    )
    .await
    .unwrap();
    assert!(sent.outbound_movement.is_some());
    for recipient in [&plan.chat_interface_id, &plan.model_interface_id] {
        assert!(
            log.read_from(Seq(begin as u64))
                .iter()
                .any(|m| m.kind == MovementKind::InterfaceOutbound
                    && m.to == Endpoint::Interface(recipient.clone().into())),
            "Task widget reaches {recipient} while Home is idle"
        );
    }
    let published = sent.outbound_movement.as_ref().unwrap().artifact.as_ref().unwrap();
    let rows = log.read_from(Seq(begin as u64)).into_iter().filter(|row| row.artifact.as_ref() == Some(published)).map(|row| {
        let mut value = serde_json::to_value(row).unwrap(); value["payload"] = document.clone(); value
    }).collect::<Vec<_>>();
    let mut renderer = std::process::Command::new("node")
        .arg(source.join("app/scripts/test-home-widget-projection.cjs")).arg("--publication-fixture")
        .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn().unwrap();
    use std::io::Write;
    let input_result = renderer.stdin.take().unwrap().write_all(&serde_json::to_vec(&json!({"rows":rows,
        "scope":{"runId":plan.run_id,"interfaceId":plan.chat_interface_id},"summary":document["data"]["summary"]})).unwrap());
    if let Err(error) = input_result {
        assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe, "{error}");
    }
    let rendered = renderer.wait_with_output().unwrap();
    assert!(rendered.status.success(), "{}\n{}", String::from_utf8_lossy(&rendered.stdout), String::from_utf8_lossy(&rendered.stderr));
    let update_pin = contract["delivery"]["voice_update"]["interface"]
        .as_str()
        .unwrap();
    let offer=InterfaceArtifactFile::write(root.join("voice-offer"),"realtime.session.offer",&serde_json::to_vec(&json!({"offer_sdp":"v=0\r\n","correlation_id":"admitted-offer","delegation_interface":voice["delegation_interface_id"],"session":{"instructions":"Use exact declared task returns","initial_items":[]}})).unwrap()).unwrap().capture_bounded(1048576,67108864).unwrap();
    let admission = admit_interface_artifact_arrival(
        &embedded,
        &mut log,
        &mut handle,
        &offer,
        &voice["control_interface_id"].as_str().unwrap().into(),
    )
    .await
    .unwrap();
    let voice_grant = &admission
        .admission
        .records
        .iter()
        .flat_map(|record| &record.outbound_transfers)
        .find(|out| out.to_interface.as_str() == voice["provider_id"].as_str().unwrap())
        .unwrap()
        .invocation_metadata
        .grant;
    assert!(voice_grant.tools.is_empty(), "Voice delegates work; its writer executes under a separate Run grant");
    let voice_metadata = &admission.admission.records.iter().flat_map(|record| &record.outbound_transfers)
        .find(|out| out.to_interface.as_str() == voice["provider_id"].as_str().unwrap()).unwrap().invocation_metadata;
    assert_eq!(voice_template["static_artifact_bindings"], json!(voice_metadata.artifact_binding_refs.iter().map(|artifact| artifact.id.as_str()).collect::<Vec<_>>()));
    let provider: InterfaceId = voice["provider_id"].as_str().unwrap().into();
    let descriptor = load_interface_descriptor(&registry.join("interface"), provider.as_str()).unwrap();
    let (sender, _arrivals) = tokio::sync::mpsc::channel(1);
    let mut ctx = InterfaceLiftContext::unprivileged(provider.clone(), registry.join("interface").join(provider.as_str()), Some(descriptor),
        root.join("voice-provider"), root.join("ledger/movements.jsonl"), root.join("ledger/blobs"), root.join("ledger/index.sqlite"),
        BTreeMap::from([("run_records_root".into(), runs.clone())]), sender, Default::default());
    std::fs::create_dir(&ctx.var_dir).unwrap();
    ctx.root_slots.insert("interface_hand_in_root".into(), root.join("hand-in"));
    ctx.root_slots.insert("app_server_control_root".into(), root.join("control"));
    ctx.host_io_limits = Some(InterfaceHostIoLimits {max_control_frame_bytes:1<<20,max_manifest_bytes:1<<20,max_payload_bytes:1<<26,max_stderr_bytes:1024});
    let mut reader = LedgerTailReader::open(&ctx).unwrap();
    let rows = reader.read_new().unwrap();
    let source = rows.iter().find(|row| row.movement.artifact.as_ref().is_some_and(|artifact| artifact.id.as_str().starts_with("realtime.session.offer:"))).unwrap();
    let input = reader.envelope(source).unwrap();
    let pinned = ctx.pinned_run_record_bytes(&input).unwrap();
    ProviderHandoffSession::new(&ctx).unwrap().from_invocation(&ctx, ctx.descriptor_snapshot.as_ref().unwrap(), &voice_record.id, &input, source.movement.seq.0, &pinned).unwrap();
    assert!(!voice_grant.interfaces.contains(&plan.model_interface_id.clone().into()));
    assert!(admission.admission.records.iter().flat_map(|r| &r.outbound_transfers)
        .filter(|out| out.to_interface.as_str() == voice["provider_id"].as_str().unwrap())
        .all(|out| out.invocation_metadata.runtime.as_ref().unwrap().run == voice_record.id));
    let delegation = json!({"call_id":"admitted-call","correlation_id":"admitted-offer","delegation_id":"admitted-delegation","input_text":"Report the delegated Task evidence","transcript":[],"provider_event_id":"delegation-event"});
    let delegated = InterfaceArtifactFile::write(
        root.join("delegation"),
        "realtime.delegation",
        &serde_json::to_vec(&delegation).unwrap(),
    )
    .unwrap()
    .capture_bounded(1048576, 67108864)
    .unwrap();
    let delegated_from = log.len();
    admit_interface_artifact_arrival(
        &embedded,
        &mut log,
        &mut handle,
        &delegated,
        &voice["provider_id"].as_str().unwrap().into(),
    )
    .await
    .unwrap();
    assert!(log.read_from(Seq(delegated_from as u64)).iter().any(|m|
        m.kind == MovementKind::InterfaceOutbound && m.to == Endpoint::Interface(plan.model_interface_id.clone().into())
        && m.runtime().is_some_and(|meta| meta.run.as_str() == plan.run_id)), "Voice delegates into the independent Home Run");
    let mut home_ctx = ctx.clone();
    home_ctx.interface = plan.model_interface_id.clone().into();
    home_ctx.interface_dir = registry.join("interface").join(&plan.model_interface_id);
    home_ctx.descriptor_snapshot = Some(load_interface_descriptor(&registry.join("interface"), &plan.model_interface_id).unwrap());
    home_ctx.var_dir = root.join("home-provider");
    std::fs::create_dir(&home_ctx.var_dir).unwrap();
    let mut home_reader = LedgerTailReader::open(&home_ctx).unwrap();
    let home_rows = home_reader.read_new().unwrap();
    let delegated_source = home_rows.iter().find(|row| row.movement.seq.0 >= delegated_from as u64
        && row.movement.artifact.as_ref().is_some_and(|artifact| artifact.id.as_str().starts_with("realtime.delegation:"))).unwrap();
    let input = home_reader.envelope(delegated_source).unwrap();
    let pinned = home_ctx.pinned_run_record_bytes(&input).unwrap();
    let home_handoff = ProviderHandoffSession::new(&home_ctx).unwrap().from_invocation(&home_ctx,
        home_ctx.descriptor_snapshot.as_ref().unwrap(), &current.id, &input, delegated_source.movement.seq.0, &pinned).unwrap();
    assert!(!home_handoff.consumes_context_free_independent_input().await, "Voice delegation must reach model dispatch rather than the receipt-only path");
    let model_context = home_handoff.current_session_context_artifacts().await;
    assert!(model_context.iter().any(|entry| entry.label == "realtime.delegation" && entry.source_artifact == input.artifact && entry.payload == delegation));
    let instructions = &input.metadata().unwrap().artifact_binding_refs;
    assert!(instructions.iter().any(|entry| entry.id.as_str() == "artifact-worker-role-operator-home"));
    assert!(instructions.iter().any(|entry| entry.id.as_str() == delivery_id));
    let update = json!({"call_id":delegation["call_id"],"correlation_id":delegation["correlation_id"],"delegation_id":delegation["delegation_id"],"update_id":"selected-task-progress","channel":"commentary","text":"The delegated Task produced its artifact."});
    let file = InterfaceArtifactFile::write(
        root.join("voice-update"),
        "realtime.session.update",
        &serde_json::to_vec(&update).unwrap(),
    )
    .unwrap()
    .capture_bounded(1048576, 67108864)
    .unwrap();
    let begin = log.len();
    let sent = octopus_app_server::InterfaceArtifactArrivalFromInterface::new(
        &embedded.runtime,
        &mut log,
        &mut handle,
        embedded.arrival_dedup.clone(),
        current.trace.clone(),
        room.clone().into(),
    )
    .emit_artifact_to_interface(
        &file,
        &current.id,
        &emitter.id,
        emitter.epoch,
        octopus_ledger::Lamport(0),
        &update_pin.into(),
    )
    .await
    .unwrap();
    assert!(sent.outbound_movement.is_some());
    assert!(
        log.read_from(Seq(begin as u64))
            .iter()
            .any(|m| m.kind == MovementKind::InterfaceOutbound
                && m.to == Endpoint::Interface(voice["provider_id"].as_str().unwrap().into())),
        "granted artifact.send reaches the exact voice update input through its System address"
    );
}
