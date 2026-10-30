use octopus_operator_chat_client::{
    ConversationSeed, OperatorChatPlanRequest, PlannedClientWrite, WriteHashAlgorithm,
    maintenance_observer::wire_source_terminal_declaration, plan_operator_chat_topology,
    run_composition::*,
};
use octopus_run_record::RunRecord;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

#[allow(dead_code)]
#[path = "support/composition_fixture.rs"]
mod support;

#[cfg(target_os = "macos")]
#[path = "support/maintenance_writer_process.rs"]
mod maintenance_writer_process;

fn source() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../..")
}

fn read(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn capture_fixture(root: &Path) -> RunCompositionRequest {
    let mut request = support::client_fixture(root);
    let source = source();
    for (kind, id, files) in [
        (
            "interface",
            "system_worker_memory_observer",
            vec!["interface.json", "policy.json", "maintenance.json"],
        ),
        (
            "interface",
            "system_context_memory_projection",
            vec!["interface.json", "life.json"],
        ),
        (
            "interface",
            "system_memory",
            vec!["interface.json", "life.json"],
        ),
        (
            "interface",
            "system_context",
            vec!["interface.json", "life.json"],
        ),
        (
            "artifact",
            "artifact-role-memory-observer",
            vec!["artifact.json"],
        ),
        ("artifact", "artifact-system-library", vec!["artifact.json"]),
        (
            "artifact",
            "artifact-context-collection-guide",
            vec!["artifact.json"],
        ),
        ("tool", "memory.save", vec!["tool.json"]),
        ("tool", "memory.search", vec!["tool.json"]),
        ("tool", "memory.read", vec!["tool.json"]),
        ("tool", "memory.list", vec!["tool.json"]),
        ("mark", "memory.experience.admitted", vec!["mark.json"]),
        ("mark", "memory.capture.completed", vec!["mark.json"]),
        (
            "verifier",
            "memory_experience",
            vec!["verifier.json", "verify.py"],
        ),
        (
            "verifier",
            "memory_capture_outcome",
            vec!["verifier.json", "verify.py"],
        ),
    ] {
        for file in files {
            let relative = format!("registry/{kind}/{id}/{file}");
            let to = root.join(&relative);
            std::fs::create_dir_all(to.parent().unwrap()).unwrap();
            std::fs::copy(source.join(&relative), to).unwrap();
        }
    }
    std::fs::create_dir_all(root.join("registry/artifact/artifact-system-library/entries"))
        .unwrap();
    let worker_path = root.join("registry/interface/system_worker_memory_observer/policy.json");
    let mut worker = read(&worker_path);
    worker["ceiling"]["interfaces"] = json!([
        "world_provider_fixture",
        "system_worker_memory_observer",
        "system_context_memory_projection",
        "system_memory",
        "system_interface_authoring",
        "system_room"
    ]);
    std::fs::write(worker_path, serde_json::to_vec(&worker).unwrap()).unwrap();
    let worker_path = root.join("registry/interface/system_worker_template/policy.json");
    let mut worker = read(&worker_path);
    worker["ceiling"]["interfaces"].as_array_mut().unwrap().push(json!("system_worker_memory_observer"));
    std::fs::write(worker_path, serde_json::to_vec(&worker).unwrap()).unwrap();
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
        let mut value = read(&root.join(file));
        value
            .pointer_mut(interfaces)
            .unwrap()
            .as_array_mut()
            .unwrap()
            .extend([
                json!("system_worker_memory_observer"),
                json!("system_memory"),
                json!("system_context_memory_projection"),
            ]);
        value
            .pointer_mut(tools)
            .unwrap()
            .as_array_mut()
            .unwrap()
            .extend([
                json!("memory.save"),
                json!("memory.search"),
                json!("memory.read"),
                json!("memory.list"),
            ]);
        support::write(root, file, value);
    }
    let client_file = "registry/interface/world_operator_chat/client.json";
    let mut client = read(&root.join(client_file));
    client["run"]["maintenance_observer"] = json!({"template_interface_id":"system_worker_memory_observer","config_file":"maintenance.json"});
    client["run"]["context_artifact_bindings"] =
        json!([{"id":"artifact-system-library"},{"id":"artifact-context-collection-guide"}]);
    support::write(root, client_file, client);
    let provider_file = "registry/interface/world_provider_fixture/interface.json";
    let mut provider = read(&root.join(provider_file));
    let declared = read(&source.join("registry/interface/world_provider_codex/interface.json"));
    provider["interface_driver_config"]["private_config"]["client_projection_output_labels"] =
        declared["interface_driver_config"]["private_config"]["client_projection_output_labels"]
            .clone();
    support::write(root, provider_file, provider);
    request.seed.label = "provider.peer.message".into();
    request.seed.payload = json!({"action":"followup","control_id":"capture-task","text":"Check the exact applied receipt before reporting success."});
    request
}

fn install(writes: impl Iterator<Item = PlannedClientWrite>) {
    for write in writes {
        std::fs::create_dir_all(write.path.parent().unwrap()).unwrap();
        std::fs::write(write.path, write.content).unwrap();
    }
}

#[test]
fn selected_native_capture_keeps_companion_exact_on_operator_revisions() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let request = capture_fixture(&root);
    let provider_path = root.join("registry/interface/world_provider_fixture/interface.json");
    let mut provider = read(&provider_path);
    provider["interface_driver_config"]["private_config"]["instruction_artifacts"] =
        json!(["artifact-context-collection-guide"]);
    std::fs::write(&provider_path, serde_json::to_vec(&provider).unwrap()).unwrap();
    let client_path = root.join("registry/interface/world_operator_chat/client.json");
    let selected_client = read(&client_path);
    for filename in [
        "../maintenance.json",
        "maintenance.json ",
        " maintenance.json",
        "./maintenance.json",
    ] {
        let mut invalid = selected_client.clone();
        invalid["run"]["maintenance_observer"]["config_file"] = json!(filename);
        std::fs::write(&client_path, serde_json::to_vec(&invalid).unwrap()).unwrap();
        assert!(
            plan_run_composition(&request).is_err(),
            "accepted invalid configuration filename {filename:?}"
        );
    }
    std::fs::write(&client_path, serde_json::to_vec(&selected_client).unwrap()).unwrap();
    let plan = plan_run_composition(&request).unwrap();
    let record: Value = serde_json::from_str(&plan.run_write.content).unwrap();
    let declaration =
        record["interface_state"][&plan.provider_interface_id]["maintenance_observer"].clone();
    assert_eq!(declaration["source_run"], plan.run_id);
    assert_eq!(declaration["input_label"], "memory.experience.input");
    assert!(declaration.get("ee").is_none(), "the declaration keeps no copy of the source placement");
    let ownership = &declaration["lifecycle_ownership"];
    assert_eq!(ownership["schema_version"], 1);
    for (kind, field) in [
        ("interface", "private_interface_ids"),
        ("tool", "private_tool_ids"),
    ] {
        let ids = ownership[field].as_array().unwrap();
        assert!(!ids.is_empty());
        for id in ids {
            let id = id.as_str().unwrap();
            let path = root.join(format!("registry/{kind}/{id}/{kind}.json"));
            let write = plan
                .descriptor_writes
                .iter()
                .find(|w| w.path == path)
                .unwrap();
            assert!(write.expected_before_hash.is_none());
            assert_eq!(read_created_id(write), id);
        }
    }
    let interface_ids = ownership["private_interface_ids"].as_array().unwrap();
    assert!(interface_ids.contains(&declaration["source_relay_interface"]));
    for key in [
        "source_worker_interface",
        "source_provider_interface",
        "source_client_interface",
    ] {
        assert!(!interface_ids.contains(&declaration[key]));
    }
    assert!(plan.run_writes.len() >= 4);
    assert_eq!(
        wire_source_terminal_declaration(&plan.run_write.content, &declaration).unwrap(),
        plan.run_write.content
    );
    let observer = &declaration["initial_record"];
    let provider = declaration["provider_interface"].as_str().unwrap();
    assert!(
        observer["interface_state"][provider]["run_authoring_binding"]["writer_run"].is_object()
    );
    let projection = &declaration["projection_run"];
    assert_eq!(projection["initial_record"]["status"], "idle");
    assert!(projection["initial_record"]["interface_state"]
        [projection["provider_interface"].as_str().unwrap()]["run_authoring_binding"]
        ["writer_run"]
        .is_object());
    let formed = plan
        .descriptor_writes
        .iter()
        .find(|write| write.path.ends_with(format!("{provider}/interface.json")))
        .unwrap();
    let cloned: Value = serde_json::from_str(&formed.content).unwrap();
    assert_eq!(cloned["interface_driver_config"]["private_config"]["instruction_artifacts"], json!([]));
    assert_eq!(cloned["interface_driver_config"]["private_config"]["worker_instruction_artifacts"],
        json!(["artifact-role-memory-observer"]));
    assert_eq!(
        cloned["interface_driver_config"]["private_config"]["model"],
        request.model
    );
    assert_eq!(
        cloned["interface_driver_config"]["private_config"]["reasoning_effort"],
        request.reasoning_effort
    );
    assert!(
        cloned["interface_driver_config"]["private_config"]
            .get("inline_context_artifact_ids")
            .is_none()
    );
    install(
        plan.descriptor_writes
            .iter()
            .cloned()
            .chain(
                plan.required_policy_updates
                    .iter()
                    .map(|update| update.write.clone()),
            )
            .chain(plan.run_writes.iter().cloned())
            .chain(std::iter::once(plan.run_write.clone())),
    );
    let client = record["interface_state"]
        .as_object()
        .unwrap()
        .iter()
        .find(|(_, state)| state["client_surface_role"] == "task")
        .unwrap()
        .0
        .clone();
    let mut previous = record;
    for cycle in ["revision-a", "revision-b", "revision-interrupted"] {
        let mut request = OperatorChatPlanRequest {
            registry_root: root.join("registry"),
            ee_instances_root: root.join("ee"),
            ee_policy_root: None,
            run_records_root: Some(root.join("runs")),
            write_hash_algorithm: WriteHashAlgorithm::Blake3,
            interface_private_state_root: Some(root.join("private")),
            chat_id: client
                .strip_prefix(selected_client["run"]["interface_prefix"].as_str().unwrap())
                .unwrap()
                .into(),
            provider_interface_id: request.provider_interface_id.clone(),
            model: Some("fixture-a".into()),
            reasoning_effort: Some("xhigh".into()),
            service_tier: None,
            ee_id: Some("ee_existence".into()),
            worker_interface_id: Some(plan.worker_interface_id.clone()),
            client_surface_role: "task".into(),
            cycle: cycle.into(),
            door_host_slots: Default::default(),
            external_inputs: vec![],
            external_relays: vec![],
            model_outputs: vec![],
            context_artifact_bindings: vec![],
            payload_artifact_bindings: vec![],
            publications: vec![],
            selected_worker_environment_kinds: vec![],
            selected_tools: vec![],
            retain_current_capabilities: true,
            existing_run_record: Some(previous.clone()),
            conversation: ConversationSeed {
                ledger_offset: 0,
                artifact_bindings: vec![],
                pinned_artifact_bindings: vec![],
                pinned_context_items: vec![],
                continued_from: None,
            },
        };
        if cycle == "revision-interrupted" {
            let provider=&plan.provider_interface_id;let label="turn.interrupted.result";
            let mut pending=previous.clone();let parsed=RunRecord::from_bytes(&serde_json::to_vec(&pending).unwrap()).unwrap();
            let templates=pending["interface_state"][provider]["run_authoring_binding"]["checkpoints"].as_array().unwrap();
            let template=templates.iter().find(|t|t["role"]=="provider_artifact_output"
                && serde_json::from_value::<octopus_policy::AuthoredInterfaceBinding>(t["interface_binding"].clone()).unwrap().admits(label)).unwrap();
            let mut row=parsed.checkpoints.iter().find(|cp|parsed.active_checkpoints.contains(&cp.id)
                && json!(cp.interface_binding)==template["interface_binding"] && json!(cp.outbound_interface_bindings)==template["outbound_interface_bindings"]).unwrap().clone();
            let removed=parsed.checkpoints.iter().filter(|cp|cp.inbound_interface().is_some_and(|id|id.as_str()==provider) && cp.id!=row.id
                && !templates.iter().any(|t|(t["role"]=="provider_interface_output" || t["role"]=="provider_tool_output"
                    && t["result_receiving"].as_object().is_some_and(|r|!r.is_empty()))
                    && json!(cp.interface_binding)==t["interface_binding"] && json!(cp.outbound_interface_bindings)==t["outbound_interface_bindings"]))
                .map(|cp|json!(cp.id)).collect::<Vec<_>>();
            row.interface_binding.as_mut().unwrap().config.artifact_policy=octopus_policy::BindingArtifactPolicy::Allow {names:vec![label.into()],namespaces:vec![]};
            pending["active_checkpoints"].as_array_mut().unwrap().retain(|id|!removed.contains(id));
            pending["interface_state"][provider]["frontier_checkpoint_ids"].as_array_mut().unwrap().retain(|id|!removed.contains(id));
            *pending["checkpoints"].as_array_mut().unwrap().iter_mut().find(|cp|cp["id"]==row.id.as_str()).unwrap()=json!(row);
            let file=octopus_interfaces::InterfaceArtifactFile::write(root.join("private").join(provider),label,b"{\"status\":\"interrupted\"}").unwrap();
            let hash=octopus_ledger::ContentHash::of_bytes(&std::fs::read(file.manifest_path()).unwrap()).to_hex();
            pending["interface_state"][provider]["pending_handoff"]=json!({"terminal":true,"source_movement_seq":0,"items":[{
                "artifact_id":format!("{label}:{hash}"),"artifact_content_hash":hash,"artifact_label":label,
                "manifest_relative_path":file.manifest_path().strip_prefix(root.join("private").join(provider)).unwrap(),
                "checkpoint":{"id":row.id,"epoch":row.epoch,"interface_binding":row.interface_binding,"outbound_interface_bindings":row.outbound_interface_bindings,
                    "artifact_bindings":row.artifact_bindings.iter().map(|b|b.id.clone()).collect::<Vec<_>>(),"required_verified_artifacts":row.required_verified_artifacts}}]});
            request.existing_run_record=Some(pending.clone());std::fs::write(&plan.run_write.path,pending.to_string()).unwrap();
            octopus_run_authoring::provider_context_bindings(&RunRecord::from_bytes(pending.to_string().as_bytes()).unwrap(),&provider.clone().into()).unwrap();
            let mut unproven=request.clone();unproven.existing_run_record.as_mut().unwrap()["interface_state"][provider].as_object_mut().unwrap().remove("pending_handoff");
            assert!(plan_operator_chat_topology(&unproven).is_err());
        }
        let next = plan_operator_chat_topology(&request).unwrap();
        assert!(
            next.run_writes.is_empty(),
            "new retained run writes: {:?}",
            next.run_writes
                .iter()
                .map(|write| {
                    let value: Value = serde_json::from_str(&write.content).unwrap();
                    (
                        write.tag.clone(),
                        value["interface_state"]
                            .as_object()
                            .unwrap()
                            .keys()
                            .cloned()
                            .collect::<Vec<_>>(),
                    )
                })
                .collect::<Vec<_>>()
        );
        let content = next.run_record_content;
        let record = RunRecord::from_bytes(content.as_bytes()).unwrap();
        assert_eq!(
            record.interface_state[&plan.provider_interface_id.clone().into()]["maintenance_observer"],
            declaration
        );
        install(next.interface_writes.into_iter());
        previous = serde_json::from_str(&content).unwrap();
        std::fs::write(&plan.run_write.path, &content).unwrap();
    }
    if let Some(path) = std::env::var_os("OCTOPUS_MAINTENANCE_PLAN_EXPORT") {
        std::fs::write(
            path,
            serde_json::to_vec(&json!({"fixture_root":root,"plan":plan})).unwrap(),
        )
        .unwrap();
        let _ = dir.keep();
    }
}

fn read_created_id(write: &PlannedClientWrite) -> String {
    let descriptor: Value = serde_json::from_str(&write.content).unwrap();
    descriptor["id"].as_str().unwrap().into()
}

fn enable_intake(root: &Path) {
    for file in ["interface.json", "life.json"] {
        let relative = format!("registry/interface/system_service_input/{file}");
        let to = root.join(&relative);
        std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        std::fs::copy(source().join(relative), to).unwrap();
    }
    let path = root.join("registry/interface/world_provider_fixture/interface.json");
    let mut provider = read(&path);
    provider["interface_driver_config"]["private_config"]["input_intake"] =
        json!({"template_interface_id":"system_service_input"});
    provider["interface_driver_config"]["private_config"]["session_snapshot_labels"] =
        json!(["interface.artifact.output", "interface.context.updated"]);
    std::fs::write(path, serde_json::to_vec(&provider).unwrap()).unwrap();
    for (file, pointer) in [
        ("registry/ee_template/existence/ee_template.json", "/allowed_interfaces"),
        ("ee/ee_existence.json", "/policy_narrowing/allowed_interfaces"),
        ("registry/interface/system_worker_template/policy.json", "/ceiling/interfaces"),
    ] {
        let path = root.join(file);
        let mut value = read(&path);
        value.pointer_mut(pointer).unwrap().as_array_mut().unwrap().push(json!("system_service_input"));
        std::fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
    }
}

mod movement_gate {
    use super::*;
    use futures::{SinkExt, StreamExt};
    use octopus_app_server::{
        SharedBlobHandle, SpineRootConfig, admit_interface_artifact_arrival, build_embedded_spine,
    };
    use octopus_descriptors::InterfaceId;
    use octopus_interface_transport::{
        InterfaceActivationArrival, InterfaceHostIoLimits, InterfaceLiftContext,
    };
    use octopus_interfaces::{DeliveryVerdict, InterfaceArtifactFile, InterfaceInvocationEnvelope};
    use octopus_ledger::{
        ContentHash, Endpoint, LiveMovementLog, Movement, MovementKind, MovementLog, Seq,
        SharedBlobs,
    };
    use octopus_ledger_store::{DurableBlobs, DurableLog};
    use octopus_run_authoring::{
        declared_provider_run_writer, RunAuthoringProfile,
        ProviderEmission, ProviderEmissionRoute, ProviderHandoffOutcome, ProviderHandoffSession,
        ProviderSourceRecovery,
    };
    use octopus_world_pyecho_life_runtime::{LedgerReplayReader, LedgerTailReader};
    use std::{
        collections::BTreeMap,
        sync::{Arc, Mutex},
    };

    struct Boundary {
        root: PathBuf,
        runtime: octopus_app_server::EmbeddedRuntime,
        log: LiveMovementLog<DurableLog>,
        blobs: SharedBlobHandle,
        sender: tokio::sync::mpsc::Sender<InterfaceActivationArrival>,
        arrivals: tokio::sync::mpsc::Receiver<InterfaceActivationArrival>,
        sockets: Vec<(InterfaceId, InterfaceId, tokio::net::UnixListener)>,
        writes: usize,
    }
    impl Boundary {
        async fn new(
            root: &Path,
            request: &RunCompositionRequest,
            plan: &RunCompositionPlan,
        ) -> Self {
            install(
                plan.descriptor_writes
                    .iter()
                    .cloned()
                    .chain(plan.required_policy_updates.iter().map(|u| u.write.clone()))
                    .chain(plan.run_writes.iter().cloned())
                    .chain(std::iter::once(plan.run_write.clone())),
            );
            let mut parent = request.admitted_parent_record.clone();
            parent.checkpoints[0].artifact_bindings =
                vec![serde_json::from_value(json!({"id":"role-system_worker_parent"})).unwrap()];
            for update in &plan.required_binding_updates {
                if !parent.checkpoints[0]
                    .outbound_interface_bindings
                    .contains(&update.outbound_binding)
                {
                    parent.checkpoints[0]
                        .outbound_interface_bindings
                        .push(update.outbound_binding.clone());
                }
                for binding in &update.required_artifact_bindings {
                    if !parent.checkpoints[0]
                        .artifact_bindings
                        .iter()
                        .any(|prior| prior.id.as_str() == binding.id)
                    {
                        parent.checkpoints[0]
                            .artifact_bindings
                            .push(serde_json::from_value(json!({"id":binding.id})).unwrap());
                    }
                }
            }
            std::fs::write(
                request.roots.run_records_root.join("caller.json"),
                serde_json::to_vec(&parent).unwrap(),
            )
            .unwrap();
            let blobs: SharedBlobs = Arc::new(Mutex::new(
                DurableBlobs::open(root.join("ledger/blobs")).unwrap(),
            ));
            let runtime = build_embedded_spine(SpineRootConfig {
                registry_root: request.roots.registry_root.clone(),
                run: plan.run_id.clone().into(),
                blobs: blobs.clone(),
                interface_artifact_root: root.join("artifacts"),
                run_records_root: request.roots.run_records_root.clone(),
                ee_instances_root: request.roots.ee_instances_root.clone(),
            })
            .await
            .unwrap();
            let log = LiveMovementLog::new(
                DurableLog::open(root.join("ledger/movements.jsonl")).unwrap(),
            );
            let (sender, arrivals) = tokio::sync::mpsc::channel(32);
            let mut sockets = Vec::new();
            for write in plan
                .run_writes
                .iter()
                .chain(std::iter::once(&plan.run_write))
            {
                let record = RunRecord::from_bytes(write.content.as_bytes()).unwrap();
                for (provider, state) in &record.interface_state {
                    if state.get("run_authoring_binding").is_some() {
                        let descriptor = read(&root.join("registry/interface").join(provider.as_str()).join("interface.json"));
                        let profile: RunAuthoringProfile = serde_json::from_value(
                            descriptor["interface_driver_config"]["private_config"]["run_authoring_profile"].clone(),
                        ).unwrap();
                        let writer = declared_provider_run_writer(&record, provider, &profile).unwrap();
                        let author = writer.source;
                        if let Some((_, door, _)) = sockets.iter().find(|(id, _, _)| id == &author) {
                            assert_eq!(door, &writer.door);
                            continue;
                        }
                        let path =
                            octopus_app_server_interface::interface_control_socket_path_from_root(
                                &root.join("control"),
                                &author,
                            );
                        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                        sockets.push((author, writer.door, tokio::net::UnixListener::bind(path).unwrap()));
                    }
                }
            }
            Self {
                root: root.to_owned(),
                runtime,
                log,
                blobs: SharedBlobHandle::new(blobs),
                sender,
                arrivals,
                sockets,
                writes: 0,
            }
        }
        fn context(&self, id: &str) -> InterfaceLiftContext {
            let owner = self.root.join("registry/interface").join(id);
            let descriptor =
                serde_json::from_slice(&std::fs::read(owner.join("interface.json")).unwrap())
                    .unwrap();
            let var = self.root.join("interfaces").join(id);
            std::fs::create_dir_all(&var).unwrap();
            let mut ctx = InterfaceLiftContext::unprivileged(
                id.into(),
                owner,
                Some(descriptor),
                var,
                self.root.join("ledger/movements.jsonl"),
                self.root.join("ledger/blobs"),
                self.root.join("ledger/ledger-index.sqlite"),
                BTreeMap::from([
                    ("run_records_root".into(), self.root.join("runs")),
                    ("interface_hand_in_root".into(), self.root.join("hand-in")),
                    ("app_server_control_root".into(), self.root.join("control")),
                ]),
                self.sender.clone(),
                Default::default(),
            );
            ctx.host_io_limits = Some(InterfaceHostIoLimits {
                max_control_frame_bytes: 1 << 20,
                max_manifest_bytes: 1 << 20,
                max_payload_bytes: 1 << 26,
                max_stderr_bytes: 1 << 18,
            });
            if ctx.descriptor_snapshot.as_ref().unwrap().interface_driver_ref
                == octopus_context_projection::DRIVER
            {
                use octopus_interface_transport::process_authority::ProcessAuthoritySpecExecution;
                let launch = octopus_interface_transport::validate_spawn_life_config(
                    &self.root.join("registry"), &ctx.interface,
                ).unwrap().unwrap();
                let [process] = launch.config.processes.as_slice() else { panic!("projection must declare one life process") };
                assert_eq!(process.authority.write_root_slots, std::collections::BTreeSet::from(["interface_hand_in_root".into()]));
                assert!(process.authority.allow_local_ipc);
                assert!(!process.authority.allow_network);
                let handles = serde_json::to_value(&process.read_handles).unwrap();
                for handle in ["ledger", "ledger_index", "blobs"] {
                    assert!(handles.as_array().unwrap().contains(&json!(handle)));
                }
                let mut exposed = process.authority.resolve(
                    &ctx.interface_dir, &ctx.root_slots, &BTreeMap::new(),
                    std::iter::empty(), [ctx.var_dir.clone()], std::iter::empty(), std::iter::empty(),
                ).unwrap().root_slots;
                for slot in process.coordinate_root_slots.iter().chain(
                    launch.interface_descriptor.interface_driver_config.as_ref().into_iter()
                        .flat_map(|driver| driver.host_slots.keys()),
                ) {
                    exposed.insert(slot.clone(), ctx.root_slot(slot).unwrap().to_path_buf());
                }
                ctx.root_slots = exposed;
                ctx.host_io_limits = Some(process.io_limits);
            }
            ctx
        }
        fn file(&self, id: &str, label: &str, value: &Value) -> InterfaceArtifactFile {
            InterfaceArtifactFile::write(
                self.context(id).var_dir.join("emitted"),
                label,
                &serde_json::to_vec(value).unwrap(),
            )
            .unwrap()
        }
        async fn admit(
            &mut self,
            file: &InterfaceArtifactFile,
            source: &InterfaceId,
        ) -> DeliveryVerdict {
            let captured = file.clone().capture_bounded(1 << 20, 1 << 26).unwrap();
            match admit_interface_artifact_arrival(
                &self.runtime,
                &mut self.log,
                &mut self.blobs,
                &captured,
                source,
            )
            .await
            {
                Ok(outcome) => DeliveryVerdict::Admitted {
                    seq: outcome
                        .admission
                        .records
                        .first()
                        .unwrap()
                        .arrival_movement
                        .seq
                        .0,
                },
                Err(error) => error.verdict(),
            }
        }
        async fn native_input(&mut self, file: &InterfaceArtifactFile, intake: &str, outbound: bool) {
            let mut parent =
                RunRecord::from_bytes(&std::fs::read(self.root.join("runs/caller.json")).unwrap())
                    .unwrap();
            let source = if outbound {
                let mut target = parent.checkpoints[0].clone();
                target.id = "emitted_target".into();
                target.outbound_interface_bindings =
                    vec![octopus_runtime::AuthoredInterfaceBinding::all(intake.into())];
                parent.checkpoints[0].outbound_interface_bindings.retain(|binding| binding.interface.as_str() != intake);
                parent.checkpoints[0].outbound_interface_bindings.push(
                    octopus_runtime::AuthoredInterfaceBinding::all("system_room".into()),
                );
                parent.active_checkpoints.push(target.id.clone());
                parent.checkpoints.push(target);
                std::fs::write(self.root.join("runs/caller.json"), serde_json::to_vec(&parent).unwrap()).unwrap();
                InterfaceId::new("system_room")
            } else {
                parent.checkpoints[0].inbound_interface().unwrap().clone()
            };
            octopus_app_server::InterfaceArtifactArrivalFromInterface::new(
                &self.runtime.runtime,
                &mut self.log,
                &mut self.blobs,
                self.runtime.arrival_dedup.clone(),
                parent.trace.clone(),
                source,
            )
            .emit_artifact_to_interface(
                file,
                &parent.id,
                &parent.checkpoints[0].id,
                parent.checkpoints[0].epoch,
                octopus_ledger::Lamport(0),
                &intake.into(),
            )
            .await
            .unwrap();
        }
        fn input(
            &mut self,
            file: &InterfaceArtifactFile,
            id: &str,
        ) -> (Movement, InterfaceInvocationEnvelope) {
            self.input_in_run(file, id, None)
        }
        fn input_in_run(
            &mut self,
            file: &InterfaceArtifactFile,
            id: &str,
            run: Option<&str>,
        ) -> (Movement, InterfaceInvocationEnvelope) {
            let ctx = self.context(id);
            let expected = run.map(|run| {
                RunRecord::from_bytes(&std::fs::read(self.root.join("runs").join(format!("{run}.json"))).unwrap()).unwrap()
            });
            let mut reader = LedgerReplayReader::open(&ctx).unwrap();
            let artifact = file
                .capture_bounded(1 << 20, 1 << 26)
                .unwrap()
                .load_into_blob(&mut self.blobs)
                .unwrap()
                .artifact;
            let mut selected = None;
            while let Some(rows) = reader.read_batch(16).unwrap() {
                for row in rows {
                    if row.movement.artifact.as_ref() == Some(&artifact) {
                        let runtime = row.metadata().unwrap().runtime.as_ref().unwrap();
                        if expected.as_ref().is_some_and(|expected| {
                            runtime.run != expected.id || runtime.ee != expected.ee
                                || runtime.worker != expected.worker
                        }) {
                            continue;
                        }
                        assert_eq!(runtime.interface.as_str(), id);
                        assert_eq!(row.movement.to, Endpoint::Interface(id.into()));
                        let input = reader.envelope(&row).unwrap();
                        let pinned = RunRecord::from_bytes(&ctx.pinned_run_record_bytes(&input).unwrap()).unwrap();
                        assert_eq!(pinned.id, runtime.run);
                        assert_eq!(pinned.ee, runtime.ee);
                        assert_eq!(pinned.worker, runtime.worker);
                        let checkpoint = pinned.checkpoint(&runtime.checkpoint).unwrap();
                        assert!(pinned.active_checkpoints.contains(&runtime.checkpoint));
                        assert_eq!(checkpoint.run, runtime.run);
                        assert_eq!(checkpoint.epoch.0, runtime.epoch);
                        if expected.is_some() {
                            assert!(checkpoint.outbound_interfaces().any(|target| target.as_str() == id));
                        }
                        assert!(selected.is_none(), "ambiguous admitted input for {id}: {artifact:?}");
                        selected = Some(row);
                    }
                }
            }
            let row = selected.unwrap_or_else(|| {
                panic!("no exact admitted input for {id} run={run:?}: artifact={artifact:?}")
            });
            (row.movement.clone(), reader.envelope(&row).unwrap())
        }
        fn handoff(
            &mut self,
            file: &InterfaceArtifactFile,
            id: &str,
            run: &str,
        ) -> octopus_run_authoring::ProviderOutputHandoff {
            let (row, _) = self.input_in_run(file, id, Some(run));
            self.handoff_at(id, run, row.seq.0).0
        }
        fn handoff_at(
            &self,
            id: &str,
            run: &str,
            source: u64,
        ) -> (octopus_run_authoring::ProviderOutputHandoff, octopus_ledger::ArtifactRef) {
            let ctx = self.context(id);
            let mut reader = LedgerReplayReader::open(&ctx).unwrap();
            let row = loop {
                let rows = reader.read_batch(16).unwrap().expect("admitted provider movement absent");
                if let Some(row) = rows.into_iter().find(|row| row.movement.seq.0 == source) {
                    break row;
                }
            };
            assert_eq!(row.movement.to, Endpoint::Interface(id.into()));
            let input = reader.envelope(&row).unwrap();
            let pinned = ctx.pinned_run_record_bytes(&input).unwrap();
            let handoff = ProviderHandoffSession::new(&ctx)
                .unwrap()
                .from_invocation(
                    &ctx,
                    &ctx.resolved_interface_descriptor().unwrap(),
                    &run.into(),
                    &input,
                    source,
                    &pinned,
                )
                .unwrap_or_else(|error| panic!("handoff {id}/{run}, source {}, label {}, checkpoint {:?}: {error}",
                    source, input.artifact_file.read_manifest().unwrap().label, input.metadata().unwrap().runtime));
            (handoff, input.artifact)
        }
        async fn result_handoff(
            &mut self,
            file: &InterfaceArtifactFile,
            id: &str,
            run: &str,
        ) -> (octopus_run_authoring::ProviderOutputHandoff, u64) {
            let receipt = self.handoff(file, id, run);
            let (recovery, outcomes) = self.drive(receipt.recover_source_with_outcomes()).await;
            let ProviderSourceRecovery::Bridged { invocation_movement_seq } = recovery else {
                panic!("raw result did not create its checked invocation: {recovery:?}");
            };
            let [ProviderHandoffOutcome::Admitted { seq, artifact }] = outcomes.as_slice() else {
                panic!("result bridge has no exact admitted invocation: {outcomes:?}");
            };
            assert_eq!(*seq, invocation_movement_seq);
            let (next, admitted) = self.handoff_at(id, run, *seq);
            assert_eq!(&admitted, artifact);
            assert_eq!(self.drive(next.recover_source_with_outcomes()).await,
                (ProviderSourceRecovery::Process, Vec::new()));
            (next, *seq)
        }
        async fn stable_result_handoff(
            &mut self,
            previous: &octopus_run_authoring::ProviderOutputHandoff,
            file: &InterfaceArtifactFile,
            id: &str,
            run: &str,
        ) -> (octopus_run_authoring::ProviderOutputHandoff, u64) {
            let record: Value = serde_json::from_slice(&previous.current_run_record_bytes().await).unwrap();
            assert_eq!(record["id"], run);
            let pending = &record["interface_state"][id]["pending_handoff"];
            assert_eq!(pending["terminal"], true);
            let prior = pending["source_movement_seq"].as_u64().unwrap();
            let (movement, _) = self.input_in_run(file, id, Some(run));
            let source = movement.seq.0;
            assert!(source > prior);
            let next = self.handoff_at(id, run, source).0;
            let label = file.read_manifest().unwrap().label;
            let recovery = async {
                next.recover_source_with_outcomes().await.map_err(|error|
                    format!("Stable result {id}/{run}, source={source}, prior={prior}, label={label}: {error}"))
            };
            assert_eq!(self.drive(recovery).await, (ProviderSourceRecovery::Process, Vec::new()));
            (next, source)
        }
        async fn effect(
            &mut self,
            file: &InterfaceArtifactFile,
            author: &InterfaceId,
        ) -> (DeliveryVerdict, InterfaceArtifactFile, InterfaceId) {
            let captured = file.capture_bounded(1 << 20, 1 << 26).unwrap();
            let file = &captured;
            let verdict = self.admit(file, author).await;
            assert!(
                matches!(verdict, DeliveryVerdict::Admitted { .. }),
                "writer call: {verdict:?}"
            );
            let (result, door) = self.execute_admitted_effect(file);
            (verdict, result, door)
        }
        fn execute_admitted_effect(
            &mut self,
            file: &InterfaceArtifactFile,
        ) -> (InterfaceArtifactFile, InterfaceId) {
            let call: octopus_tool_handlers::ToolCallArtifact =
                serde_json::from_slice(&file.read_payload_bytes().unwrap()).unwrap();
            let tool = read(
                &self
                    .root
                    .join("registry/tool")
                    .join(&call.tool)
                    .join("tool.json"),
            );
            let door = tool["interface"].as_str().unwrap();
            let (_, input) = self.input(file, door);
            let descriptor = read(
                &self
                    .root
                    .join("registry/interface")
                    .join(door)
                    .join("interface.json"),
            );
            let mut roots = BTreeMap::from([("run_records_root".into(), self.root.join("runs"))]);
            for slot in ["run_records_root", "registry_artifact_root"] {
                if let Some(path) = descriptor
                    .pointer(&format!("/interface_driver_config/host_slots/{slot}/path"))
                    .and_then(Value::as_str)
                {
                    roots.insert(slot.into(), PathBuf::from(path));
                }
            }
            let body = octopus_system_room_env::execute_file_write_result(
                &input,
                &call.arguments,
                &["system_room".into()],
                &roots,
                &self.root.join("effects").join(&call.tool),
                1 << 26,
            )
            .unwrap();
            self.writes += 1;
            let result = octopus_tool_handlers::ToolResultArtifact::success(
                &call.tool,
                call.correlation_id,
                body,
            );
            (self.file(door, &result.label, &json!(result)), door.into())
        }
        fn drive<'a, T: 'a>(
            &'a mut self,
            future: impl std::future::Future<Output = Result<T, String>> + 'a,
        ) -> impl std::future::Future<Output = T> + 'a {
            enum Event {
                Finished,
                Arrival(InterfaceActivationArrival),
                Writer(usize, tokio::net::UnixStream),
            }
            let future = Box::pin(future);
            async move {
                let (finish, mut done) = tokio::sync::oneshot::channel();
                tokio::time::timeout(std::time::Duration::from_secs(20), async {
                let (result, ()) = tokio::join!(
                    async {
                        let result = future.await;
                        let _ = finish.send(());
                        result
                    },
                    async {
                        loop {
                            let event = {
                                let connections =
                                    self.sockets.iter().enumerate().map(|(i, (_, _, socket))| {
                                        Box::pin(async move { (i, socket.accept().await) })
                                    });
                                tokio::select! {
                                    _=&mut done=>Event::Finished,
                                    arrival=self.arrivals.recv()=>Event::Arrival(arrival.unwrap()),
                                    connection=futures::future::select_all(connections)=>{
                                        let ((i,connection),_,_)=connection;
                                        Event::Writer(i,connection.unwrap().0)
                                    }
                                }
                            };
                            match event {
                                Event::Finished => break,
                                Event::Arrival(arrival) => {
                                    if let Some(retained) = arrival.retained {
                                        let captured = arrival.artifact.capture_bounded(1<<20, 1<<26).unwrap();
                                        let delivery = Box::pin(octopus_app_server::admit_retained_interface_artifact_arrival(
                                            &self.runtime, &mut self.log, &mut self.blobs, &captured, &arrival.interface, &retained.id,
                                        )).await.map_err(|error| error.to_string());
                                        retained.reply.send(delivery).unwrap();
                                    } else {
                                        let verdict = self.admit(&arrival.artifact, &arrival.interface).await;
                                        arrival.verdict_tx.unwrap().send(verdict).unwrap();
                                    }
                                }
                                Event::Writer(i, connection) => {
                                    let author = self.sockets[i].0.clone();
                                    let door = self.sockets[i].1.clone();
                                    let mut ws =
                                        tokio_tungstenite::accept_async(connection).await.unwrap();
                                    let bytes = ws.next().await.unwrap().unwrap().into_data();
                                    let file = InterfaceArtifactFile::store_manifest_bounded(
                                        self.root.join("incoming"), &bytes, 1 << 20, 1 << 26,
                                    ).unwrap();
                                    let first = Seq(self.log.len() as u64);
                                    let verdict = self.admit(&file, &author).await;
                                    let effects = self.log.read_from(first).into_iter().filter(|row|
                                        row.kind == MovementKind::InterfaceOutbound
                                            && row.from == Endpoint::Interface(author.clone())
                                            && row.to == Endpoint::Interface(door.clone())
                                    ).collect::<Vec<_>>();
                                    match effects.as_slice() {
                                        [] => {}
                                        [edge] => {
                                            let captured = file.capture_bounded(1 << 20, 1 << 26).unwrap();
                                            let artifact = captured.load_into_blob(&mut self.blobs).unwrap().artifact;
                                            assert_eq!(edge.artifact.as_ref(), Some(&artifact));
                                            let (result, actual_door) = self.execute_admitted_effect(&captured);
                                            assert_eq!(actual_door, door);
                                            assert!(matches!(self.admit(&result, &door).await, DeliveryVerdict::Admitted { .. }));
                                        }
                                        _ => panic!("authoring hand-in admitted multiple file-tool edges"),
                                    }
                                    ws.send(tokio_tungstenite::tungstenite::Message::Text(
                                        serde_json::to_string(&verdict).unwrap().into(),
                                    ))
                                    .await
                                    .unwrap();
                                }
                            }
                        }
                    }
                );
                result.unwrap()
                })
                .await
                .unwrap()
            }
        }
        fn delivered(&self, id: &str, label: &str) -> InterfaceArtifactFile {
            let row = self
                .log
                .read_from(Seq(0))
                .into_iter()
                .rev()
                .find(|row| {
                    row.kind == MovementKind::InterfaceOutbound
                        && row.to == Endpoint::Interface(id.into())
                        && row.artifact.as_ref().is_some_and(|reference| {
                            reference.id.as_str().starts_with(&format!("{label}:"))
                        })
                })
                .unwrap_or_else(|| panic!("no '{label}' artifact was delivered to '{id}'"));
            InterfaceArtifactFile::new(
                self.root
                    .join("ledger/blobs")
                    .join(row.artifact.unwrap().content.to_hex()),
            )
            .unwrap()
            .with_payload_cache_root(self.root.join("ledger/blobs"))
        }
    }

    #[tokio::test]
    async fn scripted_provider_request_receives_exact_projected_evidence() {
        let fixture = scripted_provider_request_fixture();
        Box::pin(fixture).await;
    }

    async fn scripted_provider_request_fixture() {
        const MODE: &str = "OCTOPUS_CAPTURE_PROVIDER_FIXTURE";
        let Some(root) = std::env::var_os(MODE) else {
            use std::os::unix::fs::PermissionsExt;
            let temp = tempfile::Builder::new()
                .prefix("oct-capture-wire-")
                .tempdir_in("/tmp")
                .unwrap();
            let root = temp.path().canonicalize().unwrap();
            let script = root.join("scripted-provider");
            std::fs::write(
                &script,
                format!(
                    "#!/usr/bin/env python3\n{}",
                    include_str!("support/maintenance_provider.py")
                ),
            )
            .unwrap();
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
            let auth = root.join("test-auth/auth.json");
            std::fs::create_dir_all(auth.parent().unwrap()).unwrap();
            std::fs::write(&auth, b"{}").unwrap();
            std::fs::set_permissions(&auth, std::fs::Permissions::from_mode(0o600)).unwrap();
            let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "movement_gate::scripted_provider_request_receives_exact_projected_evidence",
                    "--nocapture",
                ])
                .env(MODE, &root)
                .env(octopus_provider_codex::ENV_APP_SERVER_SPAWN, &script)
                .env_remove(octopus_provider_codex::ENV_APP_SERVER_SPAWN_ARGS)
                .env_remove("CODEX_API_KEY")
                .env_remove("CODEX_ACCESS_TOKEN")
                .env("OCTOPUS_MACHINE_CODEX_AUTH_FILE", &auth)
                .env(
                    "OCTOPUS_TEST_CAPTURE_REQUEST_FILE",
                    root.join("requests.jsonl"),
                )
                .kill_on_drop(true);
            let output = tokio::time::timeout(std::time::Duration::from_secs(35), command.output())
                .await
                .unwrap()
                .unwrap();
            assert!(
                output.status.success(),
                "scripted provider fixture: {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        };
        let root = PathBuf::from(root);
        let request = capture_fixture(&root);
        let path = root.join("registry/interface/world_provider_fixture/interface.json");
        let mut provider = read(&path);
        provider["interface_driver_ref"] = json!("codex_app_server");
        let declared =
            read(&source().join("registry/interface/world_provider_codex/interface.json"));
        for field in ["max_attempts", "max_in_flight", "web_search_modes"] {
            provider["interface_policy"]["variables"][field] =
                declared["interface_policy"]["variables"][field].clone();
        }
        for field in [
            "operator_continuity",
            "native_tool_namespace",
            "private_thread_cold_start",
            "model_result_projection",
            "reasoning_summary",
        ] {
            provider["interface_driver_config"]["private_config"][field] =
                declared["interface_driver_config"]["private_config"][field].clone();
        }
        let isolated_cwd = root.join("isolated-cwd");
        std::fs::create_dir_all(&isolated_cwd).unwrap();
        provider["interface_driver_config"]["residual_ambient"] = json!({
            "spawn":{"subcommand":"app-server","isolated_cwd":isolated_cwd},
            "private_auth_store":{"auth_file_env":"OCTOPUS_MACHINE_CODEX_AUTH_FILE"}
        });
        std::fs::write(path, serde_json::to_vec(&provider).unwrap()).unwrap();
        let plan = plan_run_composition(&request).unwrap();
        let initial: Value = serde_json::from_str(&plan.run_write.content).unwrap();
        let declaration =
            &initial["interface_state"][&plan.provider_interface_id]["maintenance_observer"];
        let projection = declaration["projection_interface"].as_str().unwrap();
        let observer = declaration["provider_interface"].as_str().unwrap();
        let observer_run = declaration["run"].as_str().unwrap();
        let mut boundary = Boundary::new(&root, &request, &plan).await;
        let original =
            "Preserve my exact correction: only this selected task may publish a receipt.";
        let input = boundary.file(
            &plan.receive_interface_id,
            &request.seed.label,
            &json!({"action":"followup","control_id":"wire-source","text":original}),
        );
        assert!(matches!(
            boundary
                .admit(&input, &plan.receive_interface_id.clone().into())
                .await,
            DeliveryVerdict::Admitted { .. }
        ));
        let source = boundary.handoff(&input, &plan.provider_interface_id, &plan.run_id);
        let terminal = boundary.file(
            &plan.provider_interface_id,
            "interface.artifact.output",
            &json!({"text":"Selected source completed."}),
        );
        boundary
            .drive(source.hand_final(ProviderEmission {
                artifact: terminal,
                route: ProviderEmissionRoute::ClientProjection,
            }))
            .await;
        let terminal = boundary.delivered(projection, "interface.artifact.output");
        let (row, envelope) = boundary.input(&terminal, projection);
        let projection_ctx = boundary.context(projection);
        let config: octopus_context_projection::ContextProjectionConfig = serde_json::from_value(
            projection_ctx
                .resolved_interface_descriptor()
                .unwrap()
                .interface_driver_config
                .unwrap()
                .private_config["context_projection"]
                .clone(),
        )
        .unwrap();
        let session = ProviderHandoffSession::new(&projection_ctx).unwrap();
        boundary
            .drive(octopus_context_projection::process_admitted_input(
                &projection_ctx,
                &session,
                &config,
                &envelope,
                &row,
            ))
            .await;
        let projected = boundary.delivered(observer, "memory.experience.input");
        let projected_value: Value =
            serde_json::from_slice(&projected.read_payload_bytes().unwrap()).unwrap();
        let mut terminal_reader =
            LedgerTailReader::open_named(&projection_ctx, "scripted-observer-terminal").unwrap();
        let outcome_interface: InterfaceId = declaration["outcome_interface"].as_str().unwrap().into();
        let model_ctx = boundary.context(observer);
        let progress_ctx = model_ctx.clone();
        let source_sequence = boundary.input(&projected, observer).0.seq.0;
        let cancel = model_ctx.cancel.clone();
        boundary.drive(async {
            let mut life = tokio::spawn((octopus_provider_codex::interface_life())(model_ctx));
            let completed = async {
                let mut terminal_seen = false;
                loop {
                    terminal_seen |= terminal_reader.read_new()?.iter().any(|row| {
                        row.movement.from == Endpoint::Interface(outcome_interface.clone())
                            && row.movement.artifact.as_ref().is_some_and(|artifact| {
                                artifact.id.as_str().starts_with("interface.artifact.output:")
                            })
                    });
                    if terminal_seen && LedgerTailReader::open(&progress_ctx)?.next_seq() > source_sequence {
                        return Ok::<(),String>(());
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
            };
            tokio::select! {
                result=&mut life => return Err(format!("provider life ended before completing input: {result:?}")),
                result=completed => result?,
            }
            cancel.cancel();
            life.await.map_err(|error| format!("provider task failed: {error}"))?
        }).await;
        let completed_source = boundary.handoff(&projected, observer, observer_run);
        assert!(matches!(
            boundary.drive(completed_source.recover_source()).await,
            ProviderSourceRecovery::Completed
        ));
        let requests = std::fs::read_to_string(root.join("requests.jsonl")).unwrap();
        let turn: Value = requests
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .find(|value| value["method"] == "turn/start")
            .unwrap();
        assert_eq!(turn["params"]["model"], request.model);
        assert_eq!(turn["params"]["effort"], request.reasoning_effort);
        assert!(
            turn["params"]["input"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["text"] == projected_value["text"])
        );
        assert!(turn.to_string().contains(original));
        let terminal = boundary.delivered(projection, "interface.artifact.output");
        let (row, envelope) = boundary.input(&terminal, projection);
        boundary
            .drive(octopus_context_projection::process_admitted_input(
                &projection_ctx,
                &session,
                &config,
                &envelope,
                &row,
            ))
            .await;
        let outcome = boundary.delivered(
            declaration["capture_interface"].as_str().unwrap(),
            "memory.capture.outcome",
        );
        let value: Value = serde_json::from_slice(&outcome.read_payload_bytes().unwrap()).unwrap();
        assert_eq!(value["outcome"]["status"], "noop");
        assert!(value["evidence"].as_array().unwrap().is_empty());
    }

    async fn drain_projection_life(
        ctx: InterfaceLiftContext,
        output_ctx: InterfaceLiftContext,
        writer_run: &str,
        source_sequence: u64,
        output_label: &str,
    ) -> Result<(), String> {
        let result_label = octopus_run_authoring::RunAuthoringResultIdentity::from_descriptor(
            &ctx.resolved_interface_descriptor()?,
        )?.artifact_label().to_owned();
        let mut outputs = LedgerTailReader::open_named(&output_ctx, "projection-fixture-output")?;
        let mut receipts = LedgerTailReader::open_named(&ctx, "projection-fixture-receipts")?;
        let progress = ctx.clone();
        let cancel = ctx.cancel.clone();
        let mut life = tokio::spawn((octopus_context_projection::interface_life())(ctx));
        let settled = async {
            let mut output_seen = false;
            let mut receipt_sequence = None;
            loop {
                for row in outputs.read_new()? {
                    output_seen |= row.movement.seq.0 > source_sequence
                        && row.movement.artifact.as_ref().is_some_and(|artifact| {
                            artifact.id.as_str().starts_with(&format!("{output_label}:"))
                        });
                    outputs.commit_through(&row)?;
                }
                for row in receipts.read_new()? {
                    if row.movement.seq.0 > source_sequence
                        && row.metadata()?.runtime.as_ref().is_some_and(|runtime| runtime.run.as_str() == writer_run)
                        && row.movement.artifact.as_ref().is_some_and(|artifact| {
                            artifact.id.as_str().starts_with(&format!("{result_label}:"))
                        })
                    {
                        receipt_sequence = Some(row.movement.seq.0);
                    }
                    receipts.commit_through(&row)?;
                }
                let cursor = LedgerTailReader::open_named(&progress, "context-projection")?.next_seq();
                if output_seen && receipt_sequence.is_some_and(|sequence| cursor > sequence) {
                    return Ok::<(), String>(());
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        };
        let result = tokio::select! {
            result=&mut life => return Err(format!("projection life ended before consuming its writer result: {result:?}")),
            result=settled => result,
        };
        cancel.cancel();
        let ended = life.await.map_err(|error| format!("projection life task failed: {error}"))?;
        result?;
        ended
    }

    #[tokio::test]
    async fn two_source_completions_projection_observer_rearm_and_restart_use_checked_writes() {
        use octopus_run_authoring::{SessionContextArtifact,
            snapshot_projection::{SnapshotSourceScope, verify_selected_source_input}};
        let temp = tempfile::Builder::new()
            .prefix("oct-capture-")
            .tempdir_in("/tmp")
            .unwrap();
        let root = temp.path().canonicalize().unwrap();
        let request = capture_fixture(&root);
        enable_intake(&root);
        let plan = plan_run_composition(&request).unwrap();
        assert_ne!(plan.receive_interface_id, plan.worker_interface_id);
        let initial: Value = serde_json::from_str(&plan.run_write.content).unwrap();
        let intake: octopus_operator_chat_client::input_intake::InputIntakeConfig =
            serde_json::from_value(
                initial["interface_state"][&plan.receive_interface_id]["input_intake"].clone(),
            )
            .unwrap();
        let d = &initial["interface_state"][&plan.provider_interface_id]["maintenance_observer"];
        let projection = d["projection_interface"].as_str().unwrap();
        let observer = d["provider_interface"].as_str().unwrap();
        let experience = d["experience_interface"].as_str().unwrap();
        let outcome = d["outcome_interface"].as_str().unwrap();
        let capture = d["capture_interface"].as_str().unwrap();
        let observer_run = d["run"].as_str().unwrap();
        let source_provider = plan.provider_interface_id.as_str();
        let mut boundary = Boundary::new(&root, &request, &plan).await;
        let projection_ctx = boundary.context(projection);
        let config: octopus_context_projection::ContextProjectionConfig = serde_json::from_value(
            projection_ctx
                .resolved_interface_descriptor()
                .unwrap()
                .interface_driver_config
                .unwrap()
                .private_config["context_projection"]
                .clone(),
        )
        .unwrap();
        let mut projection_session = ProviderHandoffSession::new(&projection_ctx).unwrap();
        for turn in [1, 2, 3, 4] {
            let message = json!({"action":"followup","control_id":format!("turn-{turn}"),"text":format!("Original human-supplied task {turn}: preserve my explicit scope.")});
            let original_input =
                boundary.file(&plan.receive_interface_id, &request.seed.label, &message);
            boundary
                .native_input(&original_input, &plan.receive_interface_id, turn == 1)
                .await;
            let (original_row, _) = boundary.input(&original_input, &plan.receive_interface_id);
            let original_ref = original_row.artifact.as_ref().unwrap();
            let edit = octopus_provider_codex::intake_state::prepare_input(
                &intake,
                plan.run_write.path.clone(),
                &std::fs::read(&plan.run_write.path).unwrap(),
                original_row.seq.0,
                original_ref,
                original_row.seq.0,
                1 << 26,
            )
            .unwrap();
            let writer_call = edit
                .materialize(
                    &boundary.context(&plan.receive_interface_id).var_dir,
                    &intake,
                )
                .unwrap();
            let (_, checked_result, checked_door) =
                boundary.effect(&writer_call, &intake.writer.owner).await;
            assert!(matches!(
                boundary.admit(&checked_result, &checked_door).await,
                DeliveryVerdict::Admitted { .. }
            ));
            let input = boundary.file(&plan.receive_interface_id, &intake.input_label,
                &json!({"schema_version":1,"source_artifact":original_ref,"source_movement_seq":original_row.seq.0,"message":message}));
            assert!(matches!(
                boundary.admit(&input, &intake.writer.owner).await,
                DeliveryVerdict::Admitted { .. }
            ));
            let (wrapped, _) = boundary.input(&input, source_provider);
            let wrapped_ref = wrapped.artifact.unwrap();
            if turn == 1 {
                let selected = SessionContextArtifact {
                    label: intake.input_label.clone(), payload: serde_json::from_slice(&input.read_payload_bytes().unwrap()).unwrap(),
                    source_artifact: wrapped_ref.clone(), source_movement_seq: Some(wrapped.seq.0),
                };
                let scope = SnapshotSourceScope {
                    run: plan.run_id.clone(), worker: plan.worker_interface_id.clone(),
                    provider: source_provider.into(), client: config.source_client_interface.clone(), relay: config.source_relay_interface.clone(),
                };
                let origin = config.input_rules.iter().find(|rule| rule.labels.contains(&intake.input_label)).unwrap().origin.as_ref();
                let verify = || verify_selected_source_input(&projection_ctx, &selected, &scope, &config.input_routes, origin);
                assert_eq!(verify().unwrap()["original"]["from"], json!(Endpoint::Interface("system_room".into())));
                let ledger_path = root.join("ledger/movements.jsonl");
                let retained = std::fs::read(&ledger_path).unwrap();
                let rows = std::str::from_utf8(&retained).unwrap().lines()
                    .map(|line| serde_json::from_str::<Value>(line).unwrap()).collect::<Vec<_>>();
                for change in ["source", "reference", "metadata", "forwarded"] {
                    let mut changed = rows.clone();
                    let row = &mut changed[original_row.seq.0 as usize];
                    match change {
                        "source" => row["from"] = json!(Endpoint::Interface("foreign_sender".into())),
                        "reference" => row["artifact"]["content"] = json!(ContentHash::of_bytes(b"wrong-reference")),
                        "metadata" => row["invocation_metadata"]["runtime"]["epoch"] =
                            json!(row["invocation_metadata"]["runtime"]["epoch"].as_u64().unwrap() + 1),
                        _ => row["invocation_metadata"]["checkpoint_admission"] = json!({"complete":true,
                            "artifact_label":request.seed.label,"required_artifact_labels":[]}),
                    }
                    let bytes = changed.iter().map(|row| serde_json::to_string(row).unwrap()).collect::<Vec<_>>().join("\n") + "\n";
                    std::fs::write(&ledger_path, bytes).unwrap();
                    let result = verify();
                    std::fs::write(&ledger_path, &retained).unwrap();
                    assert!(result.unwrap_err().contains("exact admitted source edge"), "{change}");
                }
            }
            let mut handoff = boundary.handoff(&input, source_provider, &plan.run_id);
            let source_tool = plan
                .descriptor_writes
                .iter()
                .filter(|write| write.path.ends_with("tool.json"))
                .map(|write| serde_json::from_str::<Value>(&write.content).unwrap())
                .find(|tool| {
                    tool["id"].as_str().is_some_and(|id| {
                        id.starts_with("file.write__")
                            && config
                                .evidence_rules
                                .iter()
                                .any(|rule| rule.labels.contains(&format!("{id}.result")))
                    })
                })
                .unwrap();
            let source_tool_id = source_tool["id"].as_str().unwrap();
            let source_door = source_tool["interface"].as_str().unwrap();
            for step in [1, 2] {
                let update = boundary.file(
                    source_provider,
                    "interface.context.updated",
                    &json!({"text":format!("Source tool step {step}")}),
                );
                assert!(matches!(
                    boundary
                        .drive(handoff.hand(ProviderEmission {
                            artifact: update,
                            route: ProviderEmissionRoute::ClientProjection
                        }))
                        .await,
                    ProviderHandoffOutcome::Admitted { .. }
                ));
                let call = boundary.file(source_provider,"tool.call",&json!({"tool":source_tool_id,"correlation_id":format!("source-{turn}-{step}"),
                    "arguments":{"path":root.join("runs").join(format!("evidence-{turn}-{step}.txt")),"content":"Exact bounded source effect.","expected_before_hash":null}}));
                assert!(matches!(
                    boundary
                        .drive(handoff.hand(ProviderEmission {
                            artifact: call.clone(),
                            route: ProviderEmissionRoute::Tool {
                                tool: source_tool_id.into()
                            }
                        }))
                        .await,
                    ProviderHandoffOutcome::Admitted { .. }
                ));
                let (result, door) = boundary.execute_admitted_effect(&call);
                assert_eq!(door.as_str(), source_door);
                assert!(matches!(
                    boundary.admit(&result, &door).await,
                    DeliveryVerdict::Admitted { .. }
                ));
                handoff = boundary.result_handoff(&result, source_provider, &plan.run_id).await.0;
                let context = handoff.current_session_context_artifacts().await;
                assert!(context.iter().any(|entry| entry.source_artifact == wrapped_ref));
                assert!(!context.iter().any(|entry| entry.source_artifact == *original_ref));
            }
            let telemetry = boundary.file(source_provider, "turn.completed", &json!({"turn":turn}));
            assert!(matches!(
                boundary
                    .drive(handoff.hand(ProviderEmission {
                        artifact: telemetry,
                        route: ProviderEmissionRoute::ClientProjection
                    }))
                    .await,
                ProviderHandoffOutcome::Admitted { .. }
            ));
            let output = boundary.file(
                source_provider,
                "interface.artifact.output",
                &json!({"text":format!("Source reply {turn}")}),
            );
            assert!(matches!(
                boundary
                    .drive(handoff.hand_final(ProviderEmission {
                        artifact: output,
                        route: ProviderEmissionRoute::ClientProjection
                    }))
                    .await,
                ProviderHandoffOutcome::Admitted { .. }
            ));
            let terminal = boundary.delivered(projection, "interface.artifact.output");
            let terminal_value: Value =
                serde_json::from_slice(&terminal.read_payload_bytes().unwrap()).unwrap();
            assert!(terminal_value["session_context"]["parent"].is_object());
            assert!(
                !terminal_value["session_context"]["artifacts"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|entry| entry["label"] == intake.input_label)
            );
            let (row, envelope) = boundary.input(&terminal, projection);
            if turn == 1 {
                let ledger = std::fs::read(root.join("ledger/movements.jsonl")).unwrap();
                for bound in ["carriers", "fields", "entries", "output"] {
                    let mut limited = config.clone();
                    match bound {
                        "carriers" => limited.bounds.ancestry.max_carriers = 1,
                        "fields" => limited.bounds.max_field_bytes = 0,
                        "entries" => limited.bounds.max_snapshot_entries = 1,
                        "output" => {
                            limited.bounds.max_output_bytes = 1024;
                            limited.bounds.max_field_bytes = 512;
                        }
                        _ => unreachable!(),
                    }
                    let failure = octopus_context_projection::process_admitted_input(
                        &projection_ctx,
                        &projection_session,
                        &limited,
                        &envelope,
                        &row,
                    )
                    .await
                    .unwrap_err();
                    assert!(failure.contains("bound"), "{bound}: {failure}");
                    assert_eq!(
                        std::fs::read(root.join("ledger/movements.jsonl")).unwrap(),
                        ledger
                    );
                }
            }
            let writer_run = d["projection_run"]["initial_record"]["interface_state"][projection]
                ["run_authoring_binding"]["writer_run"]["run"].as_str().unwrap();
            boundary.drive(drain_projection_life(
                boundary.context(projection), boundary.context(observer), writer_run,
                row.seq.0, "memory.experience.input",
            )).await;
            let projected = boundary.delivered(observer, "memory.experience.input");
            let evidence: Value =
                serde_json::from_slice(&projected.read_payload_bytes().unwrap()).unwrap();
            assert!(
                evidence["text"]
                    .as_str()
                    .unwrap()
                    .contains(&format!("Original human-supplied task {turn}"))
            );
            assert!(evidence.get("session_context").is_none());
            assert_eq!(evidence["source"]["run"], plan.run_id);
            assert_eq!(
                evidence["selected_input"]["provenance"]["original"]["source_artifact"],
                json!(original_ref)
            );
            assert_eq!(
                evidence["selected_input"]["original_fields"]["/text"],
                message["text"]
            );
            assert_eq!(evidence["evidence"].as_array().unwrap().len(), 2);
            assert!(
                evidence["evidence"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|entry| entry["label"] == format!("{source_tool_id}.result"))
            );
            assert_eq!(evidence["omission"]["selected_fields_trimmed"], false);
            assert_eq!(evidence["omission"]["selected_evidence_trimmed"], false);
            assert!(
                !evidence["omission"]["unselected_entries"]
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
            assert!(
                evidence["evidence"][0]["absent_requested_pointers"]
                    .as_array()
                    .unwrap()
                    .contains(&json!("/body/exit_code"))
            );
            let mut observer_source = boundary.input(&projected, observer).0.seq.0;
            let mut model = boundary.handoff(&projected, observer, observer_run);
            let (receipts, preparation_outcome) = if turn == 1 {
                let writer = d["publication_tool"].as_str().unwrap();
                let door = d["publication_door_interface"].as_str().unwrap();
                let arguments = json!({"note":"Verify the exact applied file receipt before reporting a write.","title":"Use applied receipts"});
                let call = boundary.file(
                    observer,
                    "tool.call",
                    &json!({"tool":writer,"correlation_id":"publication-1","arguments":arguments}),
                );
                let saved_outcome = boundary
                    .drive(model.hand_final(ProviderEmission {
                        artifact: call.clone(),
                        route: ProviderEmissionRoute::Tool {
                            tool: writer.into()
                        }
                    }))
                    .await;
                assert!(matches!(saved_outcome, ProviderHandoffOutcome::Admitted { .. }));
                let (_, save_input) = boundary.input(&call, door);
                let tool = read(&root.join("registry/tool").join(writer).join("tool.json"));
                let saved = octopus_system_room_env::execute_memory_tool(
                    &save_input,
                    &octopus_tool_handlers::ToolCall {
                        tool_id: writer.into(),
                        arguments: arguments.clone(),
                        handler_config: tool["handler_config"].clone(),
                    },
                    &octopus_system_room_env::MemoryHost {
                        collection: d["collection_source"].as_str().unwrap(),
                        artifact_root: &root.join("registry/artifact"),
                        ee_instances_root: &root.join("ee"),
                        effect_root: Some(&root.join("memory-effects")),
                        maximum_bytes: 1 << 26,
                        observed_run: Some(&plan.run_id),
                    },
                )
                .unwrap()
                .body;
                assert_eq!(saved["result"], "memory_saved");
                assert_eq!(saved["memory"]["scope_ee_ids"], json!([request.ee_id]));
                let applied = octopus_tool_handlers::ToolResultArtifact::success(
                    writer,
                    Some("publication-1".into()),
                    saved,
                );
                let result = boundary.file(door, &applied.label, &json!(applied));
                assert!(matches!(
                    boundary.admit(&result, &door.into()).await,
                    DeliveryVerdict::Admitted { .. }
                ));
                (model, observer_source) = boundary.stable_result_handoff(&model, &result, observer, observer_run).await;
                (json!([{"tool":writer,"correlation_id":"publication-1"}]), Some(saved_outcome))
            } else {
                (json!([]), None)
            };
            let decision = json!({"schema_version":1,"status":if turn==1 {"published"}else{"noop"},"source_run":plan.run_id,"reason":"Current experience outcome.","receipts":receipts}).to_string();
            let text = if turn == 3 { format!("Result follows.\n```json\n{decision}\n```") } else { decision };
            let final_file=boundary.file(observer,"interface.artifact.output",&json!({"text":text}));
            assert!(matches!(
                boundary
                    .drive(model.hand_final(ProviderEmission {
                        artifact: final_file,
                        route: ProviderEmissionRoute::ClientProjection
                    }))
                    .await,
                ProviderHandoffOutcome::Admitted { .. }
            ));
            let final_file = boundary.delivered(projection, "interface.artifact.output");
            let (final_row, final_input) = boundary.input(&final_file, projection);
            assert_eq!(final_row.from, Endpoint::Interface(outcome.into()));
            let outcome_label = if turn == 3 { "memory.capture.error" } else { "memory.capture.outcome" };
            boundary.drive(drain_projection_life(
                boundary.context(projection), boundary.context(capture), writer_run,
                final_row.seq.0, outcome_label,
            )).await;
            let completed = boundary.delivered(capture, outcome_label);
            let outcome: Value =
                serde_json::from_slice(&completed.read_payload_bytes().unwrap()).unwrap();
            assert_eq!(
                outcome["outcome"]["status"],
                if turn == 3 { Value::Null } else { json!(if turn == 1 { "published" } else { "noop" }) }
            );
            if turn == 3 { assert!(outcome["outcome_parse_error"].as_str().unwrap().contains("not a JSON")); }
            if turn == 1 {
                assert!(outcome["evidence"].as_array().unwrap().len() >= 2);
            } else {
                assert!(outcome["evidence"].as_array().unwrap().is_empty());
            }
            let bytes = std::fs::read(completed.manifest_path()).unwrap();
            let writes = boundary.writes;
            let ledger = std::fs::read(root.join("ledger/movements.jsonl")).unwrap();
            projection_session = ProviderHandoffSession::new(&projection_ctx).unwrap();
            boundary
                .drive(octopus_context_projection::process_admitted_input(
                    &projection_ctx,
                    &projection_session,
                    &config,
                    &envelope,
                    &row,
                ))
                .await;
            boundary
                .drive(octopus_context_projection::process_admitted_input(
                    &projection_ctx,
                    &projection_session,
                    &config,
                    &final_input,
                    &final_row,
                ))
                .await;
            let restarted = boundary.handoff_at(observer, observer_run, observer_source).0;
            assert_eq!(
                restarted.recover_source().await.unwrap(),
                ProviderSourceRecovery::Completed
            );
            if turn == 1 {
                let original = boundary.handoff(&projected, observer, observer_run);
                assert_eq!(original.recover_source_with_outcomes().await.unwrap(),
                    (ProviderSourceRecovery::Completed, vec![preparation_outcome.unwrap()]));
            }
            assert_eq!(boundary.writes, writes);
            assert_eq!(
                std::fs::read(root.join("ledger/movements.jsonl")).unwrap(),
                ledger
            );
            assert_eq!(std::fs::read(completed.manifest_path()).unwrap(), bytes);
            assert_eq!(
                boundary
                    .delivered(experience, "memory.experience.input")
                    .read_manifest()
                    .unwrap()
                    .label,
                "memory.experience.input"
            );
        }
    }
}

#[test]
fn observer_tool_results_keep_call_specific_verified_receiving() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let request = capture_fixture(&root);
    let plan = plan_run_composition(&request).unwrap();
    let source: Value = serde_json::from_str(&plan.run_write.content).unwrap();
    let observer = &source["interface_state"][&plan.provider_interface_id]["maintenance_observer"];
    let provider = observer["provider_interface"].as_str().unwrap();
    let templates = observer["initial_record"]["interface_state"][provider]["run_authoring_binding"]["checkpoints"].as_array().unwrap();
    let output = templates.iter().find(|cp| cp["role"] == "provider_tool_output").unwrap();
    let tools = output["required_verified_artifacts"][0]["params"]["tools"].as_array().unwrap();
    let receiving = output["result_receiving"].as_object().expect("observer must declare verified result waits");
    assert_eq!(receiving.len(), tools.len());
    for tool in tools {
        let tool = tool.as_str().unwrap();
        assert_eq!(receiving[tool]["artifact_label"], format!("{tool}.result"));
        assert_eq!(receiving[tool]["mark"], "tool.result.corresponds");
        assert_eq!(receiving[tool]["verifier"], "tool_result_correspondence");
    }
    let door = &output["outbound_interface_bindings"][0]["interface"];
    assert!(!templates.iter().any(|cp| &cp["interface_binding"]["interface"] == door));
    let policy = root.join("registry/interface/system_worker_memory_observer/policy.json");
    let mut value = read(&policy);
    value["ceiling"]["mark_outcomes"].as_array_mut().unwrap().retain(|mark| mark != "tool.result.corresponds");
    std::fs::write(policy, serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(plan_run_composition(&request).unwrap_err().contains("tool-result correspondence mark"));
}
