    use super::*;

    #[test]
    fn cloned_life_imports_its_source_owner_executable() {
        let mut life: SpawnLifeConfig = serde_json::from_value(serde_json::json!({
            "processes": [{
                "bin": "owner://packages/bin/octopus-app-server-activation-life",
                "io_limits": {
                    "max_control_frame_bytes": 1048576,
                    "max_stderr_bytes": 262144,
                    "max_manifest_bytes": 1048576,
                    "max_payload_bytes": 268435456
                },
                "args": [],
                "authority": {
                    "read_root_slots": [],
                    "write_root_slots": [],
                    "executable_root_slots": [],
                    "read_paths": [],
                    "write_paths": [],
                    "executable_paths": [],
                    "executable_commands": [],
                    "read_env_paths": [],
                    "write_env_paths": [],
                    "executable_env_paths": [],
                    "allow_network": false,
                    "allow_local_ipc": true,
                    "allow_application_launch": false
                },
                "read_handles": []
            }]
        }))
        .unwrap();

        import_owner_executables(&mut life, "world_operator_chat").unwrap();

        assert_eq!(
            life.processes[0].bin,
            "interface://world_operator_chat/packages/bin/octopus-app-server-activation-life"
        );
    }

    fn repository_registry_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../..")
            .canonicalize()
            .expect("repository root resolves")
    }

    #[test]
    fn exact_role_artifact_merge_preserves_instruction_order() {
        let mut planned = serde_json::json!({"role_artifacts": ["role", "grounding", "spawn"]});
        let additions = ["grounding", "role", "personal"].map(str::to_string);
        append_ordered_unique_ids(
            planned
                .as_object_mut()
                .expect("planned policy is an object"),
            "role_artifacts",
            additions.iter(),
        )
        .expect("exact-only role artifacts merge");
        assert_eq!(
            planned["role_artifacts"],
            serde_json::json!(["role", "grounding", "spawn", "personal"])
        );
    }

    #[test]
    fn retained_checkpoint_receives_readable_mandatory_role_bindings() {
        let mut checkpoint = serde_json::json!({
            "id": "cp_retained",
            "artifact_bindings": [
                {"id": "artifact-existing"},
                {
                    "id": "artifact-role-existing",
                    "access": {"read": false, "change": true}
                }
            ]
        });
        let mandatory = vec![
            serde_json::json!({"id": "artifact-role-existing"}),
            serde_json::json!({"id": "artifact-role-new"}),
        ];

        merge_mandatory_role_bindings(&mut checkpoint, &mandatory)
            .expect("retained checkpoint role bindings merge");

        let bindings = checkpoint["artifact_bindings"]
            .as_array()
            .expect("artifact bindings remain an array")
            .iter()
            .map(|value| {
                serde_json::from_value::<ArtifactBinding>(value.clone())
                    .expect("merged artifact binding parses")
            })
            .collect::<Vec<_>>();
        assert_eq!(bindings[0].id.as_str(), "artifact-existing");
        assert_eq!(bindings[1].id.as_str(), "artifact-role-existing");
        assert!(bindings[1].access.read);
        assert!(bindings[1].access.change);
        assert_eq!(bindings[2].id.as_str(), "artifact-role-new");
        assert!(bindings[2].access.read);
    }

    #[test]
    fn operator_chat_input_bindings_partition_message_and_turn_control() {
        let message =
            operator_chat_input_binding("world_operator_chat_test", "operator.message").unwrap();
        let control =
            operator_chat_input_binding("world_operator_chat_test", "operator.turn.control")
                .unwrap();
        assert!(message.admits("operator.message"));
        assert!(!message.admits("operator.turn.control"));
        assert!(control.admits("operator.turn.control"));
        assert!(!control.admits("operator.message"));
        for response in [
            "operator.approval.response",
            "operator.elicitation.response",
            "operator.user_input.response",
        ] {
            assert!(!message.admits(response));
            assert!(!control.admits(response));
        }
    }

    #[test]
    fn bound_turn_control_is_an_independent_context_free_input() {
        let identity = PlannedIdentity {
            voice: None,
            native_communication: None,
            chat_id: "chat_test".to_string(),
            run_id: "run_test".to_string(),
            ee_id: "ee_test".to_string(),
            chat_interface_id: "world_operator_chat_test".to_string(),
            model_interface_id: "world_provider_test".to_string(),
            worker_interface_id: "system_worker_test".to_string(),
            authoring_source_id: "system_authoring_source_test".to_string(),
            authoring_environment_id: "system_authoring_environment_test".to_string(),
            code_mode_owners: BTreeMap::new(),
            surface_role: "task".to_string(),
            provider_choice_interface: "world_provider_test".to_string(),
            worker_choice_interface: "system_worker_test".to_string(),
            capability_doors: BTreeMap::new(),
            capability_tools: BTreeMap::new(),
            capability_tool_sources: BTreeMap::new(),
            authoring_tool_id: "file.write__test".to_string(),
        };
        let checkpoint =
            |id: &str, inbound: AuthoredInterfaceBinding, outbound: AuthoredInterfaceBinding| {
                serde_json::json!({
                    "id": id,
                    "run": identity.run_id,
                    "trace": format!("trace:{id}"),
                    "epoch": 0,
                    "artifact_bindings": [],
                    "interface_binding": inbound,
                    "outbound_interface_bindings": [outbound],
                    "required_verified_artifacts": []
                })
            };
        let message =
            operator_chat_input_binding(&identity.chat_interface_id, "operator.message").unwrap();
        let control =
            operator_chat_input_binding(&identity.chat_interface_id, "operator.turn.control")
                .unwrap();
        let provider =
            AuthoredInterfaceBinding::all(InterfaceId::new(&identity.model_interface_id));
        let checkpoints = vec![
            checkpoint("cp_message", message, provider.clone()),
            checkpoint("cp_control", control, provider.clone()),
            checkpoint(
                "cp_authoring_call",
                AuthoredInterfaceBinding::all(InterfaceId::new(&identity.authoring_source_id)),
                AuthoredInterfaceBinding::all(InterfaceId::new(&identity.authoring_environment_id)),
            ),
            checkpoint(
                "cp_authoring_result",
                AuthoredInterfaceBinding::all(InterfaceId::new(&identity.authoring_environment_id)),
                provider.clone(),
            ),
            checkpoint(
                "cp_provider_output",
                provider,
                AuthoredInterfaceBinding::all(InterfaceId::new(&identity.chat_interface_id)),
            ),
        ];
        let topology = derive_bound_run_authoring_topology(
            &identity,
            &checkpoints,
            &[],
            &[serde_json::json!("cp_provider_output")],
            "operator.message",
            "tool_call_allowed",
        )
        .unwrap();

        let control = topology
            .checkpoints
            .iter()
            .find(|template| {
                binding_admits_named(&template.interface_binding, "operator.turn.control")
            })
            .unwrap();
        assert_eq!(control.role, BoundCheckpointRole::IndependentInput);
        assert!(!control.include_context);
        let message = topology
            .checkpoints
            .iter()
            .find(|template| binding_admits_named(&template.interface_binding, "operator.message"))
            .unwrap();
        assert_eq!(message.role, BoundCheckpointRole::Stable);
        assert!(message.include_context);
        let mut record = serde_json::json!({
            "id": identity.run_id, "ee": identity.ee_id, "worker": identity.worker_interface_id,
            "trace": "trace:context", "status": "idle",
            "active_checkpoints": checkpoints.iter().map(|row| row["id"].clone()).collect::<Vec<_>>(),
            "checkpoints": checkpoints,
            "interface_state": {&identity.model_interface_id: {"run_authoring_binding": topology}}
        });
        let source = ArtifactBindingSeed { id: "instruction-source".into() };
        context_bindings::bind_selected_context(
            &mut record, None, &identity.model_interface_id, [&source], [],
        ).unwrap();
        let control = record["checkpoints"].as_array().unwrap().iter()
            .find(|row| row["id"] == "cp_control").unwrap();
        assert_eq!(control["artifact_bindings"], serde_json::json!([{"id":"instruction-source"}]));
        let template = record["interface_state"][&identity.model_interface_id]["run_authoring_binding"]["checkpoints"]
            .as_array().unwrap().iter().find(|row| row["role"] == "independent_input").unwrap();
        assert_eq!(template["include_context"], false);
        assert_eq!(template["static_artifact_bindings"], serde_json::json!(["instruction-source"]));
    }

    #[test]
    fn provider_derived_checkpoint_identity_preserves_an_external_route() {
        let profile = OctopusOsAuthoringProfile {
            ee_id: "ee_test".to_string(),
            worker_choice_interface: "system_worker_test".to_string(),
            door_host_slots: BTreeMap::new(),
            external_inputs: vec![ExternalInputSeed {
                checkpoint_id: "cp_home_project_coordination".to_string(),
                interface_id: "system_home_project_coordination".to_string(),
                artifact_names: Vec::new(),
                required_verified_artifacts: Vec::new(),
            }],
            external_relays: Vec::new(),
            model_outputs: Vec::new(),
            context_artifact_bindings: Vec::new(),
            payload_artifact_bindings: Vec::new(),
            selected_worker_environment_kinds: Vec::new(),
            selected_tools: Vec::new(),
            publication_evidence: Vec::new(),
        };
        let checkpoint = serde_json::json!({
            "id": "cp_context_derived_exact",
            "run": "run_test",
            "interface_binding": AuthoredInterfaceBinding::all(InterfaceId::new("system_home_project_coordination")),
            "outbound_interface_bindings": [AuthoredInterfaceBinding::all(InterfaceId::new("world_provider_test"))],
            "required_verified_artifacts": []
        });
        let active = BTreeSet::from(["cp_context_derived_exact".to_string()]);
        let resolved = resolve_previous_external_route_checkpoints(
            Some(&profile),
            &[checkpoint],
            &active,
            "run_test",
            "world_provider_test",
            &OperationRoutes::default(),
        )
        .expect("the route is identified by its exact edge after provider authoring");
        assert_eq!(resolved, active);
    }

    pub(super) fn boot_operator_space_ee_instances(dir: &Path) {
        let instance = |ee: &str, template: &str, parent: Option<&str>| {
            let row = octopus_ee::EeInstance {
                name: None,
                ee_id: EeId::new(ee),
                template: octopus_descriptors::EeTemplateId::new(template),
                parent_ee: parent.map(EeId::new),
                goal: format!("boot instance of {template}"),
                config: Default::default(),
                policy_narrowing: Default::default(),
            };
            std::fs::write(
                dir.join(format!("{ee}.json")),
                serde_json::to_vec_pretty(&row).expect("EE instance encodes"),
            )
            .expect("EE instance writes");
        };
        instance("ee_existence", "existence", None);
        instance("ee_operator_space", "operator_space", Some("ee_existence"));
    }

    #[test]
    fn exact_door_replanning_preserves_operator_host_slot_coordinates() {
        let repository = repository_registry_root();
        let source =
            load_interface_descriptor(&repository.join("registry/interface"), "system_coder_env")
                .expect("source door loads");
        let interface_root = tempfile::tempdir().expect("temporary interface registry");
        let exact_id = "system_coder_env_personal";
        let exact_dir = interface_root.path().join(exact_id);
        std::fs::create_dir_all(&exact_dir).expect("exact door directory creates");
        let mut exact = source.clone();
        exact.id = InterfaceId::new(exact_id);
        exact
            .interface_driver_config
            .get_or_insert_default()
            .host_slots
            .insert(
                "repo_root".to_string(),
                octopus_interfaces::HostSlotOverride {
                    additional_paths: Vec::new(),
                    path: PathBuf::from("/tmp/personal-project"),
                },
            );
        std::fs::write(
            exact_dir.join("interface.json"),
            serde_json::to_vec_pretty(&exact).expect("exact door encodes"),
        )
        .expect("exact door writes");

        let preserved = preserve_exact_door_host_slots(interface_root.path(), &source, exact_id)
            .expect("exact door host slots preserve")
            .expect("driver config exists");
        assert_eq!(
            preserved.host_slots["repo_root"].path,
            PathBuf::from("/tmp/personal-project")
        );
    }

    fn assert_exact_writer_detachment(plan: &OperatorChatPlan, root: &Path) {
        use serde_json::{Value, json};
        std::fs::create_dir_all(root).unwrap();
        for write in &plan.run_writes {
            std::fs::write(&write.path, &write.content).unwrap();
        }
        let mut original: Value = serde_json::from_str(&plan.run_record_content).unwrap();
        let provider = &plan.model_interface_id;
        let writer = original["interface_state"][provider]["run_authoring_binding"]
            .as_object_mut().unwrap().remove("writer_run").unwrap();
        std::fs::remove_file(root.join(format!("{}.json", writer["run"].as_str().unwrap()))).unwrap();
        let ids = writer["initial_record"]["active_checkpoints"].as_array().unwrap();
        for cp in writer["initial_record"]["checkpoints"].as_array().unwrap() {
            let mut cp = cp.clone();
            cp["run"] = json!(plan.run_id);
            original["checkpoints"].as_array_mut().unwrap().push(cp);
        }
        original["active_checkpoints"].as_array_mut().unwrap().extend(ids.clone());
        original["interface_state"][provider]["frontier_checkpoint_ids"].as_array_mut().unwrap().extend(ids.clone());
        let output = original["checkpoints"].as_array().unwrap().iter()
            .find(|cp| cp["interface_binding"]["interface"] == *provider
                && serde_json::from_value::<AuthoredInterfaceBinding>(cp["interface_binding"].clone())
                    .unwrap().admits("turn.completed")
                && cp["outbound_interface_bindings"].as_array().unwrap().iter()
                    .any(|binding| binding["interface"] == plan.chat_interface_id)).unwrap().clone();
        original["interface_state"][provider]["pending_handoff"] = json!({
            "source_movement_seq":77,"terminal":true,"items":[{
                "artifact_id":"turn.completed:exact","artifact_content_hash":"7".repeat(64),
                "artifact_label":"turn.completed","manifest_relative_path":"emitted/exact.artifact.json",
                "checkpoint":{"id":output["id"],"epoch":output["epoch"],
                    "interface_binding":output["interface_binding"],"outbound_interface_bindings":output["outbound_interface_bindings"],
                    "artifact_bindings":output["artifact_bindings"].as_array().unwrap().iter().map(|b|b["id"].clone()).collect::<Vec<_>>(),
                    "required_verified_artifacts":output["required_verified_artifacts"]}
            }],
        });
        original["interface_state"][provider]["context_generation"] = json!(16);
        let bytes = pretty_json(&original).unwrap();
        let target = root.join(format!("{}.json", plan.run_id));
        std::fs::write(&target, &bytes).unwrap();
        let mut request = OperatorWriterRunPlanRequest {
            run_records_root:root.to_path_buf(),run_id:plan.run_id.clone(),expected_before_sha256:sha256_hex(bytes.as_bytes()),
        };
        let writes = plan_operator_writer_runs(&request).unwrap();
        assert_eq!(writes.len(),2);
        assert_eq!(std::fs::read(&target).unwrap(), bytes.as_bytes(), "planning cannot mutate its source");
        let next: Value = serde_json::from_str(&writes.last().unwrap().content).unwrap();
        for key in ["pending_handoff","context_generation"] {
            assert_eq!(next["interface_state"][provider][key],original["interface_state"][provider][key]);
        }
        assert_eq!(next["interface_state"][&plan.chat_interface_id],original["interface_state"][&plan.chat_interface_id]);
        let retained: Vec<_> = original["checkpoints"].as_array().unwrap().iter().filter(|cp| !ids.contains(&cp["id"])).cloned().collect();
        assert_eq!(next["checkpoints"],json!(retained));
        request.expected_before_sha256 = "0".repeat(64);
        assert!(plan_operator_writer_runs(&request).unwrap_err().contains("changed before planning"));
        for write in writes { std::fs::write(write.path,write.content).unwrap(); }
        request.expected_before_sha256 = sha256_hex(&std::fs::read(&target).unwrap());
        assert!(plan_operator_writer_runs(&request).unwrap().is_empty());
    }

    #[test]
    fn real_registry_home_plan_exports_authoring_and_exact_policy_requirements() {
        let repository = repository_registry_root();
        let registry_root = repository.join("registry");
        let ee_dir = tempfile::tempdir().expect("temp EE instances");
        boot_operator_space_ee_instances(ee_dir.path());

        let request = OperatorChatPlanRequest {
            registry_root: registry_root.clone(),
            ee_instances_root: ee_dir.path().to_path_buf(),
            ee_policy_root: None,
            run_records_root: Some(ee_dir.path().join("runs")),
            write_hash_algorithm: WriteHashAlgorithm::Sha256,
            interface_private_state_root: Some(ee_dir.path().join("private")),
            chat_id: "planprobe0000000000000001".to_string(),
            provider_interface_id: "world_provider_ollama".to_string(),
            model: None,
            reasoning_effort: None,
            service_tier: None,
            ee_id: Some("ee_operator_space".to_string()),
            worker_interface_id: Some("system_worker_coder_max".to_string()),
            client_surface_role: "task".to_string(),
            cycle: "cycleprobe000000000000001".to_string(),
            door_host_slots: BTreeMap::new(),
            external_inputs: Vec::new(),
            external_relays: Vec::new(),
            model_outputs: Vec::new(),
            context_artifact_bindings: Vec::new(),
            payload_artifact_bindings: Vec::new(),
            selected_worker_environment_kinds: [
                "audio_device_session",
                "browser_session",
                "code_mode",
                "container_session",
                "git_worktree",
                "mcp_stdio_session",
                "process_session",
                "remote_exec_server",
                "system_room",
                "web_search",
            ]
            .into_iter()
            .map(str::to_string)
            .collect(),
            selected_tools: Vec::new(),
            retain_current_capabilities: false,
            existing_run_record: None,
            publications: Vec::new(),
            conversation: ConversationSeed {
                ledger_offset: 0,
                artifact_bindings: Vec::new(),
                pinned_artifact_bindings: Vec::new(),
                pinned_context_items: Vec::new(),
                continued_from: None,
            },
        };
        let plan = plan_operator_chat_topology(&request).expect("the real registry plans a chat");
        let mut cli_request = request.clone();
        cli_request.provider_interface_id = "world_provider_claude_cli".into();
        cli_request.worker_interface_id = Some("system_worker_operator_task".into());
        cli_request.selected_worker_environment_kinds = vec!["system_room".into()];
        let cli_plan = plan_operator_chat_topology(&cli_request).expect("Claude tasks include their memory observer under the same provider grant");
        let cli_provider = cli_plan.interface_writes.iter().find(|write| write.tag == format!("interface:{}", cli_plan.model_interface_id)).unwrap();
        assert_eq!(serde_json::from_str::<serde_json::Value>(&cli_provider.content).unwrap()["copy_of"], "world_provider_claude_cli");
        assert!(cli_plan.run_writes.iter().any(|write| write.tag.starts_with("run:run_memory_observer_")));
        assert_exact_writer_detachment(&plan, request.run_records_root.as_ref().unwrap());
        assert_computer_use_planned_identities(&registry_root, ee_dir.path());
        assert_browser_only_persistent_plan(&registry_root, ee_dir.path());
        assert_home_and_task_browser_cu_profile(&registry_root, ee_dir.path());

        assert_eq!(
            plan.ee_template_chain,
            vec!["existence".to_string(), "operator_space".to_string()],
            "the EE chain is resolved from the live instance walk, not a declared pair"
        );
        assert_eq!(plan.client_surface_role, "task");
        assert_eq!(plan.authoring_profile.ee_id, "ee_operator_space");
        assert_eq!(
            plan.authoring_profile.worker_choice_interface,
            "system_worker_coder_max"
        );
        for exact in [
            &plan.chat_interface_id,
            &plan.model_interface_id,
            &plan.worker_interface_id,
            &plan.authoring_source_interface_id,
            &plan.authoring_environment_interface_id,
        ] {
            assert!(
                plan.required_interface_ids.contains(exact),
                "the exported policy requirement omits exact interface '{exact}'"
            );
        }
        assert!(plan.required_tool_ids.contains(&plan.authoring_tool_id));
        assert!(
            plan.capability_tools
                .values()
                .all(|tool| plan.required_tool_ids.contains(tool))
        );

        let record = RunRecord::from_bytes(plan.run_record_content.as_bytes())
            .expect("the planned record parses through the canonical run contract");
        let run_authoring_binding: BoundRunAuthoringTopology = serde_json::from_value(
            record.interface_state[&InterfaceId::new(&plan.model_interface_id)]
                ["run_authoring_binding"]
                .clone(),
        )
        .expect("a new task carries its explicit provider run-authoring binding");
        assert_eq!(run_authoring_binding.schema_version, 2);
        let writer = run_authoring_binding.writer_run.as_ref().expect("task authoring has an independent checked writer Run");
        let writer_plan = plan.run_writes.iter().find(|write| write.path.file_stem().unwrap() == writer.run.as_str()).unwrap();
        assert_eq!(serde_json::from_str::<serde_json::Value>(&writer_plan.content).unwrap(), writer.initial_record);
        assert_eq!(writer.initial_record["interface_state"][&plan.model_interface_id]["file_writer_target"]["run"], plan.run_id);
        assert!(!record.checkpoints.iter().any(|checkpoint| checkpoint.inbound_interface() == Some(&run_authoring_binding.authoring_source_interface)));
        assert_eq!(
            run_authoring_binding.provider_interface.as_str(),
            plan.model_interface_id
        );
        let operator_input = record
            .checkpoints
            .iter()
            .find(|checkpoint| checkpoint.id.as_str().starts_with("cp_operator_input_"))
            .expect("operator chat input checkpoint exists");
        assert_eq!(
            operator_input
                .interface_binding
                .as_ref()
                .expect("operator input has an exact binding")
                .config
                .artifact_policy,
            BindingArtifactPolicy::Allow {
                names: vec!["operator.message".to_string()],
                namespaces: Vec::new(),
            },
            "generic chat input cannot bypass a dynamically authored verified operator response edge"
        );
        let turn_control_input = record
            .checkpoints
            .iter()
            .find(|checkpoint| {
                checkpoint
                    .id
                    .as_str()
                    .starts_with("cp_operator_turn_control_")
            })
            .expect("operator turn-control checkpoint exists");
        assert_ne!(operator_input.id, turn_control_input.id);
        let message_binding = operator_input.interface_binding.as_ref().unwrap();
        let control_binding = turn_control_input.interface_binding.as_ref().unwrap();
        assert!(message_binding.admits("operator.message"));
        assert!(!message_binding.admits("operator.turn.control"));
        assert!(control_binding.admits("operator.turn.control"));
        assert!(!control_binding.admits("operator.message"));
        assert!(
            !turn_control_input.artifact_bindings.is_empty(),
            "every checkpoint carries the worker role dependencies that formation must prove"
        );
        assert!(
            !operator_input.artifact_bindings.is_empty(),
            "ordinary operator input keeps the authored conversation context"
        );
        let requirement_tools: BTreeSet<String> = record
            .checkpoints
            .iter()
            .flat_map(|checkpoint| checkpoint.required_verified_artifacts.iter())
            .map(|required| serde_json::to_value(required).unwrap())
            .chain(
                record
                    .interface_state
                    .values()
                    .filter_map(|state| state.get("writer_run")?["initial_record"]["checkpoints"].as_array())
                    .flatten()
                    .filter_map(|checkpoint| checkpoint["required_verified_artifacts"].as_array())
                    .flatten()
                    .cloned(),
            )
            .filter_map(|required| required["params"]["tools"].as_array().cloned())
            .flatten()
            .filter_map(|tool| tool.as_str().map(str::to_string))
            .collect();
        let private_policy_write = plan
            .interface_writes
            .iter()
            .find(|write| write.tag == format!("policy:{}", plan.worker_interface_id))
            .expect("the private worker policy is planned");
        let private_policy: WorkerInterfacePolicy =
            serde_json::from_str(&private_policy_write.content)
                .expect("private worker ceiling parses");
        let mut model_tools: BTreeSet<String> = private_policy
            .ceiling
            .tools
            .iter()
            .map(|tool| tool.as_str().to_string())
            .collect();
        assert!(model_tools.remove(&plan.authoring_tool_id));
        assert!(writer.initial_record["checkpoints"].as_array().unwrap().iter().any(|cp|
            cp["required_verified_artifacts"].as_array().unwrap().iter().any(|requirement|
                requirement["params"]["tools"].as_array().is_some_and(|tools| tools.contains(&serde_json::json!(plan.authoring_tool_id))))),
            "the private authoring writer has its own required verifier");
        assert_eq!(
            requirement_tools, model_tools,
            "every model-visible private tool is reachable through exactly one authored door verifier"
        );
        assert!(
            plan.capability_tools
                .iter()
                .all(|(base, tool)| tool != base && tool.ends_with(&request.chat_id)),
            "every formed capability receives a private exact tool identity"
        );

        let relay_pins: BTreeSet<&str> = plan.worker_relays.iter().map(String::as_str).collect();
        for pin in [
            "system_worker_coder",
            "system_worker_coder_max",
            "system_worker_planner",
            "system_worker_memory",
            "system_worker_web",
            "system_worker_flash",
            "system_worker_computer_use",
        ] {
            assert!(
                relay_pins.contains(pin),
                "granted worker relay '{pin}' is formed and reported by the plan; got {relay_pins:?}"
            );
        }
        let current_worker_roles = private_policy
            .role_artifacts
            .iter()
            .map(|artifact| artifact.as_str().to_string())
            .collect::<Vec<_>>();
        let relay_ids = plan
            .worker_relays
            .iter()
            .map(InterfaceId::new)
            .collect::<Vec<_>>();
        let mandatory_roles = formed_worker_role_artifacts(
            &repository_registry_root().join("registry/interface"),
            &current_worker_roles,
            relay_ids.iter(),
        )
        .expect("formed role dependency closure resolves");
        for checkpoint in &record.checkpoints {
            let bindings = checkpoint
                .artifact_bindings
                .iter()
                .map(|binding| binding.id.as_str())
                .collect::<BTreeSet<_>>();
            assert!(
                mandatory_roles
                    .iter()
                    .all(|artifact| bindings.contains(artifact.as_str())),
                "checkpoint '{}' omits a mandatory formed worker role dependency; expected {mandatory_roles:?}, got {bindings:?}",
                checkpoint.id.as_str()
            );
        }
        let context_bindings: BTreeSet<&str> = record
            .checkpoints
            .iter()
            .flat_map(|checkpoint| checkpoint.artifact_bindings.iter())
            .map(|binding| binding.id.as_str())
            .collect();
        assert!(
            context_bindings.contains("artifact-spawn-literacy"),
            "the worker pin's role artifacts ground on-the-fly sub-agency in the run's context; got {context_bindings:?}"
        );
        let exact_tool_owners = plan
            .interface_writes
            .iter()
            .filter(|write| write.tag.starts_with("tool:"))
            .map(|write| {
                serde_json::from_str::<ToolDescriptor>(&write.content)
                    .expect("private tool descriptor parses")
                    .interface
            })
            .collect::<BTreeSet<_>>();
        for door in plan
            .capability_doors
            .values()
            .chain(std::iter::once(&plan.authoring_environment_interface_id))
        {
            assert!(
                exact_tool_owners.contains(&InterfaceId::new(door)),
                "private door '{door}' owns at least one exact tool"
            );
        }
        let provider_write = plan
            .interface_writes
            .iter()
            .find(|write| write.tag == format!("interface:{}", plan.model_interface_id))
            .expect("the plan authors the private provider interface");
        let provider_value: serde_json::Value =
            serde_json::from_str(&provider_write.content).expect("provider descriptor parses");
        assert_eq!(
            provider_value.get("reference"),
            None,
            "a private provider is a self-contained exact interface without inherited descriptor identity"
        );
        assert_eq!(
            provider_value["interface_driver_ref"],
            serde_json::json!("codex_app_server"),
            "the private provider uses the declared Codex harness driver"
        );
        assert_eq!(
            provider_value["interface_driver_config"]["private_config"]["opencodex"]["credential_owner_interface"],
            serde_json::json!("world_provider_ollama"),
            "the private provider preserves the exact source interface as credential owner"
        );
        assert_eq!(
            provider_value["interface_driver_config"]["private_config"]["opencodex"]["adapter"],
            serde_json::json!("openai-chat"),
            "the private provider preserves the declared OpenCodex wire adapter"
        );
        let source_provider: serde_json::Value = serde_json::from_slice(
            &std::fs::read(registry_root.join("interface/world_provider_ollama/interface.json")).unwrap(),
        ).unwrap();
        assert_eq!(
            provider_value["interface_driver_config"]["private_config"]["model"],
            source_provider["interface_driver_config"]["private_config"]["model"],
            "the variant's own overrides ride into the private sibling"
        );
        assert_eq!(
            provider_value["interface_driver_config"]["private_config"]["run_authoring_profile"]["file_write_tool"],
            serde_json::json!(&plan.authoring_tool_id),
            "interface-owned run authoring uses its exact hidden file tool"
        );
        assert_eq!(
            provider_value["interface_driver_config"]["private_config"]["run_authoring_profile"]["file_write_result_label"],
            serde_json::json!(format!("{}.result", plan.authoring_tool_id)),
            "interface-owned run authoring awaits the exact hidden tool result"
        );
        let browser_source: serde_json::Value = serde_json::from_slice(
            &std::fs::read(registry_root.join("interface/system_browser/interface.json")).unwrap(),
        ).unwrap();
        let upload_resume = browser_source["interface_driver_config"]["private_config"]["browser_room"]["upload_approval"]["resume_tool"]
            .as_str().expect("selected browser declares its private approval continuation");
        let hidden = provider_value["interface_driver_config"]["private_config"]["model_hidden_tools"]
            .as_array()
            .expect("the private provider declares hidden tools")
            .iter()
            .map(|tool| tool.as_str().unwrap().to_string())
            .collect::<BTreeSet<_>>();
        assert!(
            hidden.contains(&plan.authoring_tool_id) && hidden.contains(&plan.capability_tools[upload_resume]),
            "authoring and the exact declared approval continuation stay callable by their owners and hidden from the model"
        );
        assert!(
            ["web.search", "web.extract"].iter().all(|tool| !hidden.iter().any(|entry| entry == tool || entry.starts_with(&format!("{tool}__")))),
            "a provider without native web search keeps the Octopus web tools visible to its model"
        );
        assert_eq!(
            provider_value["interface_driver_config"]["private_config"].get("run_authoring"),
            None,
            "the exact provider derives run topology from the attested run instead of reverse-owning it"
        );
        assert!(
            plan.interface_writes
                .iter()
                .any(|write| write.tag == format!("policy:{}", plan.worker_interface_id)),
            "the plan authors the private worker pin ceiling"
        );
        assert!(
            plan.interface_writes
                .iter()
                .any(|write| write.tag == format!("life:{}", plan.model_interface_id)),
            "the exact provider carries its own executable life instead of inheriting it"
        );
        assert!(
            plan.interface_writes
                .iter()
                .all(|write| !write.tag.starts_with("worker-environment-kind:")),
            "an exact task topology must not rewrite a shared code-kind descriptor"
        );
        let authoring_tool_write = plan
            .interface_writes
            .iter()
            .find(|write| write.tag == format!("tool:{}", plan.authoring_tool_id))
            .expect("the authoring room receives an exact tool descriptor");
        let authoring_tool: ToolDescriptor = serde_json::from_str(&authoring_tool_write.content)
            .expect("planned exact tool descriptor parses");
        assert_eq!(
            authoring_tool.interface,
            InterfaceId::new(&plan.authoring_environment_interface_id)
        );
        assert_eq!(
            authoring_tool
                .supported_worker_environment_kinds
                .iter()
                .map(|kind| kind.as_str())
                .collect::<Vec<_>>(),
            vec!["system_room"]
        );
        assert!(
            authoring_tool
                .process
                .authority
                .write_argument_paths
                .iter()
                .all(|path| path.within_root_slots
                    == BTreeSet::from(["run_records_root".to_string()]))
        );

        let mut existing_record: serde_json::Value =
            serde_json::from_str(&plan.run_record_content).expect("planned record re-parses");
        existing_record["checkpoints"]
            .as_array_mut()
            .expect("checkpoints remain an array")
            .push(serde_json::json!({
                "id": "cp_external_schedule",
                "run": plan.run_id,
                "trace": format!("trace:{}:cp_external_schedule", plan.run_id),
                "epoch": 0,
                "interface_binding": AuthoredInterfaceBinding::all(InterfaceId::new("world_cron")),
                "outbound_interface_bindings": [AuthoredInterfaceBinding::all(InterfaceId::new(&plan.model_interface_id))],
                "artifact_bindings": [],
                "required_verified_artifacts": []
            }));
        existing_record["active_checkpoints"]
            .as_array_mut()
            .expect("active checkpoints remain an array")
            .push(serde_json::json!("cp_external_schedule"));
        let previous_cycle =
            existing_record["interface_state"][&plan.chat_interface_id]["cycle_checkpoint_ids"]
                .as_array()
                .expect("client cycle ids remain an array")
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_string)
                .collect::<BTreeSet<_>>();
        let previous_frontier =
            existing_record["interface_state"][&plan.model_interface_id]["frontier_checkpoint_ids"]
                .as_array()
                .expect("provider frontier ids remain an array")
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_string)
                .collect::<BTreeSet<_>>();
        let mut provider_frontier = Vec::new();
        let mut reauthored_active = vec![serde_json::json!("cp_external_schedule")];
        for (index, checkpoint) in existing_record["checkpoints"]
            .as_array_mut()
            .expect("checkpoints remain an array")
            .iter_mut()
            .enumerate()
        {
            let id = checkpoint["id"]
                .as_str()
                .expect("checkpoint id remains a string")
                .to_string();
            if !previous_cycle.contains(&id) {
                continue;
            }
            let next_id = if previous_frontier.contains(&id) {
                format!("cp_provider_frontier_{index}")
            } else {
                format!("cp_provider_context_{index}")
            };
            checkpoint["id"] = serde_json::json!(next_id);
            reauthored_active.push(serde_json::json!(next_id));
            if previous_frontier.contains(&id) {
                provider_frontier.push(serde_json::json!(next_id));
            }
        }
        existing_record["active_checkpoints"] = serde_json::Value::Array(reauthored_active);
        existing_record["interface_state"][&plan.model_interface_id]["frontier_checkpoint_ids"] =
            serde_json::Value::Array(provider_frontier);
        let active = existing_record["active_checkpoints"]
            .as_array()
            .expect("active checkpoints remain an array")
            .iter()
            .filter_map(serde_json::Value::as_str)
            .collect::<BTreeSet<_>>();
        let active_exact_client_labels = existing_record["checkpoints"]
            .as_array()
            .expect("checkpoints remain an array")
            .iter()
            .filter(|checkpoint| {
                checkpoint["id"]
                    .as_str()
                    .is_some_and(|id| active.contains(id))
            })
            .filter_map(|checkpoint| {
                serde_json::from_value::<AuthoredInterfaceBinding>(
                    checkpoint.get("interface_binding")?.clone(),
                )
                .ok()
            })
            .filter(|binding| binding.interface.as_str() == plan.chat_interface_id)
            .flat_map(|binding| match binding.config.artifact_policy {
                BindingArtifactPolicy::Allow { names, .. } => names,
                BindingArtifactPolicy::All => Vec::new(),
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(
            active_exact_client_labels,
            BTreeSet::from([
                "operator.message".to_string(),
                "operator.turn.control".to_string(),
                "provider.models.list".to_string(),
            ]),
            "an existing run must preserve every active exact client binding"
        );
    }

    fn assert_computer_use_planned_identities(registry_root: &Path, ee_root: &Path) {
        let request: OperatorChatPlanRequest = serde_json::from_value(serde_json::json!({
            "registry_root":registry_root,"ee_instances_root":ee_root,"run_records_root":ee_root.join("runs"),"interface_private_state_root":ee_root.join("private"),
            "write_hash_algorithm":"sha256","chat_id":"cu_exact_identity",
            "provider_interface_id":"world_provider_codex","ee_id":"ee_operator_space",
            "worker_interface_id":"system_worker_computer_use","client_surface_role":"task",
            "cycle":"cu_exact_cycle","selected_worker_environment_kinds":["desktop_session"],
            "conversation":{"ledger_offset":0}
        }))
        .unwrap();
        let plan = plan_operator_chat_topology(&request).expect("actual Computer Use task plans");
        let initial = &plan.capability_tools["gui.app_state"];
        let resume = &plan.capability_tools["gui.app_resume"];
        assert_ne!(initial, "gui.app_state");
        assert_ne!(resume, "gui.app_resume");
        let planned_json = |tag: String| -> serde_json::Value {
            serde_json::from_str(
                &plan
                    .interface_writes
                    .iter()
                    .find(|write| write.tag == tag)
                    .expect("exact planned write exists")
                    .content,
            )
            .unwrap()
        };
        let policy = planned_json(format!("policy:{}", plan.worker_interface_id));
        for tool in [initial, resume] {
            assert!(plan.required_tool_ids.contains(tool));
            assert!(
                policy["ceiling"]["tools"]
                    .as_array()
                    .unwrap()
                    .contains(&serde_json::json!(tool))
            );
            let descriptor = planned_json(format!("tool:{tool}"));
            assert_eq!(descriptor["id"], *tool);
            assert_eq!(descriptor["interface"], plan.capability_doors["system_gui"]);
        }
        let provider = planned_json(format!("interface:{}", plan.model_interface_id));
        let hidden = provider["interface_driver_config"]["private_config"]["model_hidden_tools"]
            .as_array()
            .unwrap();
        assert!(hidden.contains(&serde_json::json!(resume)));
        assert!(!hidden.contains(&serde_json::json!(initial)));
        assert!(!hidden.contains(&serde_json::json!("gui.app_resume")));
        let desktop = planned_json(format!(
            "sidecar:{}:desktop.json",
            plan.capability_doors["system_gui"]
        ));
        assert_eq!(
            desktop["app_approval"]["operator_interface"],
            plan.chat_interface_id
        );
        assert_eq!(desktop["app_approval"]["resume_tool"], *resume);
        let source: serde_json::Value = serde_json::from_slice(
            &std::fs::read(registry_root.join("interface/system_gui/desktop.json")).unwrap(),
        )
        .unwrap();
        let mut unchanged = desktop.clone();
        unchanged["app_approval"] = source["app_approval"].clone();
        assert_eq!(unchanged, source);

        let mut private =
            serde_json::json!({"run_authoring_profile":{"file_write_tool":"file.write"},
            "model_hidden_tools":["file.write","gui.app_resume","declared.unselected"]})
            .as_object()
            .unwrap()
            .clone();
        replace_provider_hidden_tools(
            &mut private,
            "provider",
            &plan.authoring_tool_id,
            &plan.capability_tools,
        )
        .unwrap();
        assert_eq!(
            private["model_hidden_tools"],
            serde_json::json!([plan.authoring_tool_id, resume, "declared.unselected"])
        );
        private.insert(
            "model_hidden_tools".to_string(),
            serde_json::json!(["file.write", "gui.app_resume"]),
        );
        replace_provider_hidden_tools(
            &mut private,
            "provider",
            "file.write__home",
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(
            private["model_hidden_tools"],
            serde_json::json!(["file.write__home", "gui.app_resume"])
        );

        let interface_root = registry_root.join("interface");
        let config = load_client_config(&interface_root).unwrap();
        let chain = resolve_live_ee_policy_chain(ee_root, &EeId::new("ee_operator_space")).unwrap();
        let topology = select_capability_topology(
            form_capability_topology(
                registry_root,
                &CapabilityFormationRequest::from_live_chain(
                    InterfaceId::new("system_worker_computer_use"),
                    chain,
                ),
            )
            .unwrap(),
            &config,
            &request.selected_worker_environment_kinds,
            &[],
        )
        .unwrap();
        let binding = &topology.tool_bindings[&InterfaceId::new("system_gui")];
        let provider =
            resolve_provider(&interface_root, "world_provider_codex", "", "", None, &topology).unwrap();
        let identity = plan_identity(
            &config,
            &request.chat_id,
            &provider,
            &topology,
            &resolve_run_selection(&request, &config).unwrap(),
        )
        .unwrap();
        for invalid in [
            serde_json::json!({"resume_tool":"gui.app_resume"}),
            serde_json::json!({"operator_interface":"world_foreign","resume_tool":"gui.app_resume"}),
            serde_json::json!({"operator_interface":config.interface,"resume_tool":"file.write"}),
        ] {
            let mut candidate = source.clone();
            candidate["app_approval"] = invalid;
            assert!(
                plan_desktop_app_approval(&candidate.to_string(), binding, &config, &identity)
                    .is_err()
            );
        }

        let mut persistent_request = request;
        persistent_request.chat_id = "cu_persistent_identity".into();
        persistent_request.cycle = "cu_persistent_cycle".into();
        persistent_request.selected_worker_environment_kinds.push("code_mode".into());
        let isolated = plan_operator_chat_topology(&persistent_request).unwrap();
        let exact_json = |plan: &OperatorChatPlan, tag: String| -> serde_json::Value {
            serde_json::from_str(&plan.interface_writes.iter().find(|w| w.tag == tag).expect("exact planned source exists").content).unwrap()
        };
        let isolated_sidecar = exact_json(&isolated,format!("sidecar:{}:code_mode.json",isolated.capability_doors["system_code_mode"]));
        assert_eq!(isolated_sidecar["context"]["mode"],"isolated");
        assert!(isolated_sidecar.get("handoff").is_none());
        let standalone = std::fs::read(registry_root.join("interface/system_code_mode/code_mode.json")).unwrap();
        persistent_request.context_artifact_bindings.push(ArtifactBindingSeed { id:"artifact-code-mode-cua-prelude".into() });
        let payload_root = tempfile::tempdir().unwrap();
        let file = octopus_interfaces::InterfaceArtifactFile::write(payload_root.path().to_path_buf(), "fixture.payload", &[0xff,0x00,0x81]).unwrap();
        let content = octopus_ledger::ContentHash::of_bytes(&std::fs::read(file.manifest_path()).unwrap());
        let payload_ref = octopus_ledger::ArtifactRef { id:octopus_descriptors::ArtifactId::new(format!("fixture.payload:{}",content.to_hex())), content };
        persistent_request.payload_artifact_bindings = vec![payload_ref.clone()];
        let persistent = plan_operator_chat_topology(&persistent_request).expect("selected CU persistent owner plans through the real registry");
        assert_eq!(persistent.authoring_profile.payload_artifact_bindings, vec![payload_ref.clone()]);
        persistent_request.payload_artifact_bindings.push(payload_ref.clone());
        assert!(plan_operator_chat_topology(&persistent_request).is_err());
        persistent_request.payload_artifact_bindings = vec![octopus_ledger::ArtifactRef { id:octopus_descriptors::ArtifactId::new("artifact-code-mode-cua-prelude"), content }];
        assert!(plan_operator_chat_topology(&persistent_request).is_err());
        persistent_request.payload_artifact_bindings = vec![payload_ref.clone()];
        let owner = &persistent.capability_doors["system_code_mode"];
        let gui = &persistent.capability_doors["system_gui"];
        let sidecar = exact_json(&persistent,format!("sidecar:{owner}:code_mode.json"));
        assert_eq!(sidecar["context"]["mode"],"persistent");
        assert_eq!(sidecar["continuation"]["resume_tool"],persistent.capability_tools["code_mode.resume"]);
        let prelude = &sidecar["context"]["prelude_artifact"];
        let topology_ref = &sidecar["handoff"]["topology_artifact"];
        let owner_topology = exact_json(&persistent,format!("artifact:{}",topology_ref["id"].as_str().unwrap()));
        let prelude_body = exact_json(&persistent,format!("artifact:{}",prelude["id"].as_str().unwrap()));
        for (reference,body) in [(prelude,&prelude_body),(topology_ref,&owner_topology)] {
            let reference:octopus_ledger::ArtifactRef=serde_json::from_value(reference.clone()).unwrap();
            assert_eq!(body["id"],reference.id.as_str());
            assert_eq!(body["run"],persistent.run_id);
            assert_eq!(reference.content,octopus_ledger::ContentHash::of_bytes(&octopus_descriptors::canonical_bytes(body)));
        }
        assert_eq!(owner_topology["prelude_artifact"],*prelude);
        assert_eq!(owner_topology["owner_interface"],*owner);
        assert_eq!(owner_topology["coauthor"]["tool"],persistent.authoring_tool_id);
        let writer_tool = owner_topology["file_write_tool"].as_str().unwrap();
        let writer = exact_json(&persistent,format!("tool:{writer_tool}"));
        assert_ne!(writer_tool,persistent.authoring_tool_id);
        assert_ne!(writer["interface"],persistent.authoring_environment_interface_id);
        assert!(persistent.required_tool_ids.iter().any(|tool| tool == writer_tool));
        let run:serde_json::Value=serde_json::from_str(&persistent.run_record_content).unwrap();
        let templates = run["interface_state"][&persistent.model_interface_id]["run_authoring_binding"]["checkpoints"].as_array().unwrap();
        let returned = templates.iter().filter(|cp| cp["role"] == "owner_input" && cp["interface_binding"]["interface"] == *owner).collect::<Vec<_>>();
        assert_eq!(returned.len(),1);
        assert_eq!(returned[0]["interface_binding"]["interface"],*owner);
        assert_eq!(returned[0]["outbound_interface_bindings"][0]["interface"],persistent.model_interface_id);
        assert_eq!(returned[0]["owner_authoring"]["tool"],writer_tool);
        assert_eq!(returned[0]["owner_authoring"]["door"],writer["interface"]);
        assert_eq!(returned[0]["include_context"], true);
        let mandatory = run["interface_state"][&persistent.model_interface_id]["run_authoring_binding"]["writer_run"]["initial_record"]["checkpoints"][0]["artifact_bindings"].as_array().unwrap();
        let mut selected_static = config.run.context_artifact_bindings.iter()
            .filter(|binding| !mandatory.iter().any(|role| role["id"] == binding.id))
            .map(|binding| serde_json::json!(binding.id)).collect::<Vec<_>>();
        selected_static.push(run["interface_state"][&persistent.model_interface_id]["collection_catalogue_adoption"]["snapshot"]["id"].clone());
        selected_static.push(serde_json::json!(format!("artifact-run-authoring-coordinates-{}", persistent_request.chat_id)));
        assert_eq!(returned[0]["static_artifact_bindings"], serde_json::json!(selected_static));
        assert_eq!(run["interface_state"][&persistent.model_interface_id]["run_authoring_binding"]["payload_artifact_bindings"], serde_json::json!([payload_ref]));
        let rows = run["checkpoints"].as_array().unwrap();
        let writer = &run["interface_state"][&persistent.model_interface_id]["run_authoring_binding"]["writer_run"];
        let authoring_row = writer["initial_record"]["checkpoints"].as_array().unwrap().iter().find(|cp| cp["interface_binding"]["interface"] == persistent.authoring_source_interface_id
            && cp["outbound_interface_bindings"][0]["interface"] == persistent.authoring_environment_interface_id).unwrap();
        let output_template = templates.iter().find(|cp| cp["role"] == "provider_tool_output"
            && cp["outbound_interface_bindings"][0]["interface"] == *owner).unwrap();
        let mut output_static = vec![prelude["id"].clone(),topology_ref["id"].clone(),serde_json::json!(payload_ref.id)];
        output_static.extend(selected_static);
        assert_eq!(output_template["static_artifact_bindings"],serde_json::json!(output_static));
        let mut exact_bindings = authoring_row["artifact_bindings"].as_array().unwrap().clone();
        exact_bindings.extend(output_template["static_artifact_bindings"].as_array().unwrap().iter().map(|id| serde_json::json!({"id":id})));
        let owner_call = rows.iter().find(|cp| cp["interface_binding"] == output_template["interface_binding"]
            && cp["outbound_interface_bindings"] == output_template["outbound_interface_bindings"]).unwrap();
        let bindings = |value: &serde_json::Value| serde_json::from_value::<Vec<ArtifactBinding>>(value.clone()).unwrap();
        assert_eq!(bindings(&owner_call["artifact_bindings"]), bindings(&serde_json::json!(exact_bindings)));
        for id in run["interface_state"][owner]["owned_checkpoint_ids"].as_array().unwrap() {
            let row = rows.iter().find(|cp| cp["id"] == *id).unwrap();
            assert_eq!(bindings(&row["artifact_bindings"]), bindings(&serde_json::json!(exact_bindings)), "owned execution checkpoint bindings differ from the exact provider tool output template");
        }
        let seed = ArtifactBindingSeed { id: "fixture.selected.context".into() };
        for inherit in [true, false] {
            let mut rebound = run.clone();
            let mut call = authoring_row.clone();
            call["run"] = rebound["id"].clone();
            rebound["active_checkpoints"].as_array_mut().unwrap().push(call["id"].clone());
            rebound["checkpoints"].as_array_mut().unwrap().push(call);
            let owned = rebound["interface_state"][owner]["owned_checkpoint_ids"].as_array().unwrap().clone();
            let templates = rebound["interface_state"][&persistent.model_interface_id]["run_authoring_binding"]["checkpoints"].as_array_mut().unwrap();
            let returned = templates.iter_mut().find(|cp| cp["role"] == "owner_input" && cp["interface_binding"]["interface"] == *owner).unwrap();
            returned["include_context"] = serde_json::json!(inherit);
            if !inherit { returned["static_artifact_bindings"] = output_template["static_artifact_bindings"].clone(); }
            let rights = serde_json::json!({"read":true,"change":true,"create":true,"delete":true});
            for row in rebound["checkpoints"].as_array_mut().unwrap().iter_mut().filter(|row| owned.contains(&row["id"])) {
                row["artifact_bindings"].as_array_mut().unwrap().iter_mut().find(|binding| binding["id"] == prelude["id"]).unwrap()["access"] = rights.clone();
            }
            context_bindings::bind_selected_context(&mut rebound, None, &persistent.model_interface_id, [&seed], []).unwrap();
            let returned = rebound["interface_state"][&persistent.model_interface_id]["run_authoring_binding"]["checkpoints"].as_array().unwrap().iter().find(|cp| cp["role"] == "owner_input" && cp["interface_binding"]["interface"] == *owner).unwrap();
            assert_eq!(returned["include_context"], inherit);
            assert!(returned["static_artifact_bindings"].as_array().unwrap().contains(&serde_json::json!(seed.id)));
            for row in rebound["checkpoints"].as_array().unwrap().iter().filter(|row| owned.contains(&row["id"])) {
                let actual = bindings(&row["artifact_bindings"]);
                assert_eq!(row["artifact_bindings"].as_array().unwrap().iter().find(|binding| binding["id"] == prelude["id"]).unwrap()["access"], rights);
                assert_eq!(actual.iter().find(|binding| binding.id.as_str() == seed.id).unwrap().access, octopus_runtime::ArtifactAccess::read_only());
            }
            let denied = rebound["checkpoints"].as_array_mut().unwrap().iter_mut().find(|row| owned.contains(&row["id"])).unwrap()["artifact_bindings"].as_array_mut().unwrap().iter_mut().find(|binding| binding["id"] == seed.id).unwrap();
            denied["access"] = serde_json::json!({"read":false,"create":true});
            assert!(context_bindings::bind_selected_context(&mut rebound, None, &persistent.model_interface_id, [&seed], []).unwrap_err().contains("cannot widen an existing denied read"));
        }
        assert!(!templates.iter().any(|cp| cp["interface_binding"]["interface"] == *gui || cp["outbound_interface_bindings"].as_array().unwrap().iter().any(|out| out["interface"] == *gui)));
        let provider = exact_json(&persistent,format!("interface:{}",persistent.model_interface_id));
        let hidden = provider["interface_driver_config"]["private_config"]["model_hidden_tools"].as_array().unwrap();
        let routes = owner_topology["routes"].as_array().unwrap();
        assert_nested_owner_returns(&persistent, &owner_topology);
        for (base,tool) in &persistent.capability_tools {
            let descriptor = exact_json(&persistent,format!("tool:{tool}"));
            if descriptor["interface"] == *gui {
                assert!(hidden.contains(&serde_json::json!(tool)));
                if base != "gui.app_resume" {
                    let route = routes.iter().find(|route| route["tool"] == *tool).expect("formed nested GUI method remains callable in the room");
                    assert_eq!(route["call"]["binding"]["outbound"][0]["interface"],*gui);
                    assert_eq!(route["app_approval"]["resume_tool"],persistent.capability_tools["gui.app_resume"]);
                    let result = &route["result"];
                    let cp = run["checkpoints"].as_array().unwrap().iter().find(|cp| cp["id"] == result["name"]).expect("nested return is initially formed");
                    assert_eq!(cp["interface_binding"],result["binding"]["inbound"]);
                    assert!(run["interface_state"][owner]["owned_checkpoint_ids"].as_array().unwrap().contains(&cp["id"]));
                    assert!(run["active_checkpoints"].as_array().unwrap().contains(&cp["id"]));
                }
            }
        }
        assert!(hidden.contains(&serde_json::json!(writer_tool)));
        assert!(hidden.contains(&serde_json::json!(persistent.capability_tools["code_mode.resume"])));
        assert!(!hidden.contains(&serde_json::json!(persistent.capability_tools["code_mode.run"])));
        let run_descriptor = exact_json(&persistent,format!("tool:{}",persistent.capability_tools["code_mode.run"]));
        assert!(run_descriptor["description"].as_str().unwrap().contains("prelude namespace"));
        assert_eq!(prelude_body["exports"],serde_json::json!(["cua"]));
        for cp in run["checkpoints"].as_array().unwrap() {
            assert!(!cp["artifact_bindings"].as_array().unwrap().iter().any(|binding| binding["id"] == "artifact-code-mode-cua-prelude"));
        }
        assert_eq!(standalone,std::fs::read(registry_root.join("interface/system_code_mode/code_mode.json")).unwrap());
    }

    fn assert_nested_owner_returns(plan: &OperatorChatPlan, topology: &serde_json::Value) {
        let exact = |tool: &str| -> ToolDescriptor {
            parse_strict(plan.interface_writes.iter().find(|write| write.tag == format!("tool:{tool}")).unwrap().content.as_bytes()).unwrap()
        };
        let routes = topology["routes"].as_array().unwrap();
        let mut groups = BTreeMap::<String, (serde_json::Value, BTreeSet<String>)>::new();
        for route in routes {
            let descriptor = exact(route["tool"].as_str().unwrap());
            assert_eq!(route["process_descriptor"], serde_json::to_value(&descriptor).unwrap());
            let process = nested_process_owner(&descriptor).unwrap();
            let name = format!("nested_result_{}", octopus_descriptors::DescriptorHash::of_canonical_json(&process).to_hex());
            assert_eq!(route["result"]["name"], name);
            let group = groups.entry(name).or_insert_with(|| (process.clone(), BTreeSet::new()));
            assert_eq!(group.0, process);
            group.1.insert(format!("{}.result", descriptor.id));
            if !route["app_approval"].is_null() {
                let resume = exact(route["app_approval"]["resume_tool"].as_str().unwrap());
                assert_eq!(route["app_approval"]["resume_descriptor"], serde_json::to_value(&resume).unwrap());
                assert_eq!(nested_process_owner(&resume).unwrap(), process);
                group.1.insert(format!("{}.result", resume.id));
            }
        }
        let mut disjoint = BTreeSet::new();
        let run: serde_json::Value = serde_json::from_str(&plan.run_record_content).unwrap();
        for (name, (owner, labels)) in groups {
            assert!(labels.iter().all(|label| disjoint.insert((owner["interface"].clone().to_string(), label.clone()))));
            let row = run["checkpoints"].as_array().unwrap().iter().find(|row| row["id"] == name).unwrap();
            let policy = serde_json::json!({"mode":"allow","names":labels,"namespaces":[]});
            for route in routes.iter().filter(|route| route["result"]["name"] == name) {
                assert_eq!(route["result"]["binding"]["inbound"]["config"]["artifact_policy"], policy);
                assert_eq!(route["result"]["binding"]["outbound"][0]["config"]["artifact_policy"], policy);
                assert_eq!(row["interface_binding"], route["result"]["binding"]["inbound"]);
                assert_eq!(row["outbound_interface_bindings"], route["result"]["binding"]["outbound"]);
            }
        }
    }

    #[test]
    fn task_agent_nested_selection_survives_model_change_and_current_amendment() {
        use crate::run_composition::{CompositionRoots, current_run::*};
        use octopus_run_authoring::capability_amendment::ProviderCapabilityBridge;
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let registry = root.join("registry");
        copy_planner_descriptors(&repository_registry_root().join("registry"), &registry);
        let ee = root.join("ee"); std::fs::create_dir(&ee).unwrap(); boot_operator_space_ee_instances(&ee);
        let agent = "system_worker_template_nested_agent";
        for file in ["interface.json", "policy.json"] {
            let mut value: serde_json::Value = serde_json::from_slice(&std::fs::read(registry.join("interface/system_worker_operator_task").join(file)).unwrap()).unwrap();
            value["id"] = serde_json::json!(agent);
            if file == "policy.json" { for id in value["ceiling"]["interfaces"].as_array_mut().unwrap() {
                if id == "system_worker_operator_task" { *id = serde_json::json!(agent); }
            } }
            let path = registry.join("interface").join(agent).join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap(); std::fs::write(path, pretty_json(&value).unwrap()).unwrap();
        }
        let topology = form_capability_topology(&registry, &CapabilityFormationRequest::from_live_chain(InterfaceId::new(agent), resolve_live_ee_policy_chain(&ee, &EeId::new("ee_existence")).unwrap())).unwrap();
        let selected = topology.tool_bindings.iter().flat_map(|(interface,binding)| binding.tools.iter().map(|tool| SelectedToolSeed {
            tool:tool.tool.to_string(), interface:interface.to_string(), worker_environment_kind:tool.worker_environment_kind.to_string(),
        })).collect::<Vec<_>>();
        let mut request: OperatorChatPlanRequest = serde_json::from_value(serde_json::json!({
            "registry_root":registry,"ee_instances_root":ee,"run_records_root":root.join("runs"),"interface_private_state_root":root.join("private"),
            "write_hash_algorithm":"sha256","chat_id":"nested_agent_task","provider_interface_id":"world_provider_codex","model":"gpt-6.1-sol","reasoning_effort":"xhigh",
            "ee_id":"ee_existence","worker_interface_id":agent,"client_surface_role":"task","cycle":"first",
            "selected_worker_environment_kinds":["browser_session","code_mode","desktop_session","system_room"],"selected_tools":selected,
            "context_artifact_bindings":[{"id":"artifact-code-mode-browser-prelude"},{"id":"artifact-code-mode-cua-prelude"}],
            "conversation":{"ledger_offset":0,"artifact_bindings":[],"pinned_artifact_bindings":[],"pinned_context_items":[],"continued_from":null}
        })).unwrap();
        let install = |plan: &OperatorChatPlan| {
            for write in plan.interface_writes.iter().chain(&plan.run_writes) {
                std::fs::create_dir_all(write.path.parent().unwrap()).unwrap(); std::fs::write(&write.path,&write.content).unwrap();
            }
            std::fs::create_dir_all(root.join("runs")).unwrap();
            std::fs::write(root.join("runs").join(format!("{}.json",plan.run_id)),&plan.run_record_content).unwrap();
        };
        let initial = plan_operator_chat_topology(&request).unwrap(); install(&initial);
        let roots = CompositionRoots {registry_root:registry.clone(),ee_instances_root:ee.clone(),run_records_root:root.join("runs"),interface_private_state_root:root.join("private")};
        let provider = InterfaceId::new(&initial.model_interface_id);
        let mut current = RunRecord::from_bytes(initial.run_record_content.as_bytes()).unwrap();
        let first = current_selection(&roots,&current,&provider).unwrap().unwrap();
        assert_eq!(first.selected_tools.len(),selected.len());
        assert!(selected.iter().all(|tool|first.selected_tools.contains(tool)));
        request.existing_run_record = Some(serde_json::to_value(&current).unwrap());
        request.retain_current_capabilities = true; request.cycle = "model_change".into(); request.model = Some("gpt-6-astra".into());
        let changed = plan_operator_chat_topology(&request).unwrap();
        assert_eq!(changed.capability_tools,initial.capability_tools); assert_eq!(changed.capability_doors,initial.capability_doors);
        install(&changed); current = RunRecord::from_bytes(changed.run_record_content.as_bytes()).unwrap();
        assert_eq!(current_selection(&roots,&current,&provider).unwrap().unwrap().selected_tools,first.selected_tools);
        let door = InterfaceId::new(&changed.capability_doors["system_room"]);
        let bridge = ProviderCapabilityBridge {tool:changed.capability_tools["file.read"].clone(),correlation_id:"nested-read".into(),arguments_hash:octopus_ledger::ContentHash::of_bytes(b"{}").to_hex(),return_interface:door.clone()};
        let retained = [(changed.capability_tools["file.read"].clone(),door)];
        let amendment = plan_current_run_amendment(&roots,&current,&provider,CurrentRunRequest {amendment_key:"nested-retain".into(),selection:Some(first.clone()),authored:None,publications:vec![]},bridge.clone(),&retained).unwrap();
        assert!(amendment.validation.safe_to_apply,"{:?}",amendment.validation.regressions);
        let result_mark = current.interface_state[&provider]["run_authoring_binding"]["checkpoints"].as_array().unwrap().iter()
            .find_map(|cp|cp["result_receiving"][&bridge.tool]["mark"].as_str()).unwrap();
        let policy_path = registry.join("interface").join(current.worker.as_str()).join("policy.json");
        let policy_bytes = std::fs::read(&policy_path).unwrap();
        let mut narrowed:serde_json::Value = serde_json::from_slice(&policy_bytes).unwrap();
        narrowed["ceiling"]["mark_outcomes"].as_array_mut().unwrap().retain(|mark|mark.as_str()!=Some(result_mark));
        let current_bytes = std::fs::read(roots.run_records_root.join(format!("{}.json",current.id))).unwrap();
        let authored = AuthoredCurrentRun {expected_before_hash:octopus_ledger::ContentHash::of_bytes(&current_bytes).to_hex(),
            expected_before:serde_json::json!(current),run_record:serde_json::json!(current),writes:vec![crate::run_composition::RegistryCandidateWrite {
                path:policy_path.clone(),content:narrowed.to_string(),expected_before_hash:Some(octopus_ledger::ContentHash::of_bytes(&policy_bytes).to_hex()),
            }]};
        assert!(plan_current_run_amendment(&roots,&current,&provider,CurrentRunRequest {amendment_key:"nested-mark-denied".into(),selection:None,authored:Some(authored),publications:vec![]},bridge,&retained)
            .unwrap_err().contains("removes the bridge result mark"));
        assert_eq!(std::fs::read(policy_path).unwrap(),policy_bytes);
        let after = RunRecord::from_bytes(&serde_json::to_vec(&amendment.amendment.after).unwrap()).unwrap();
        assert_eq!(current_selection(&roots,&after,&provider).unwrap().unwrap().selected_tools,first.selected_tools);
        let owner = &changed.capability_doors["system_code_mode"];
        let owner_state = &current.interface_state[&InterfaceId::new(owner)];
        assert_eq!(after.interface_state[&InterfaceId::new(owner)],*owner_state);
        let nested = registry.join("tool").join(&changed.capability_tools["browser.navigate"]).join("tool.json");
        let before = std::fs::read(&nested).unwrap();
        let mut descriptor: serde_json::Value = serde_json::from_slice(&before).unwrap(); descriptor["description"] = serde_json::json!("Changed after accepted selection");
        std::fs::write(&nested,pretty_json(&descriptor).unwrap()).unwrap();
        assert!(current_selection(&roots,&current,&provider).unwrap_err().contains("granted exact descriptor"));
        std::fs::write(&nested,before).unwrap();
        let mut unbound = current.clone();
        for checkpoint in &mut unbound.checkpoints {
            checkpoint.artifact_bindings.retain(|artifact| !artifact.id.as_str().starts_with("artifact-code-mode-topology__"));
        }
        assert!(current_selection(&roots,&unbound,&provider).unwrap_err().contains("not readable"));
        let mut wrong_return = current.clone();
        for checkpoint in &mut wrong_return.checkpoints {
            if checkpoint.inbound_interface().is_some_and(|id| id.as_str() == changed.capability_doors["system_browser"])
                && checkpoint.outbound_interfaces().any(|id| id.as_str() == owner) {
                checkpoint.outbound_interface_bindings = vec![AuthoredInterfaceBinding::all(InterfaceId::new(owner))];
            }
        }
        assert!(current_selection(&roots,&wrong_return,&provider).unwrap_err().contains("exact accepted call and return"));
    }

    fn assert_home_and_task_browser_cu_profile(registry_root: &Path, ee_root: &Path) {
        let repo = registry_root.parent().unwrap();
        let read = |path: &str| -> serde_json::Value {
            serde_json::from_slice(&std::fs::read(repo.join(path)).unwrap()).unwrap()
        };
        let pack = read("app/distribution/local-packages/packs/codex.json");
        let home = &pack["workspace_setup"]["home"];
        let kinds = home["selected_worker_environment_kinds"].as_array().unwrap().iter().map(|v|v.as_str().unwrap()).collect::<BTreeSet<_>>();
        assert_eq!(kinds,BTreeSet::from(["browser_session","desktop_session","code_mode","filesystem_workspace","system_room"]));
        for artifact in ["artifact-worker-role-template","artifact-grounding-system-architecture","artifact-codebase-layout","artifact-code-mode-browser-prelude","artifact-code-mode-cua-prelude"] {
            assert!(home["context_artifact_bindings"].as_array().unwrap().iter().any(|reference|reference["id"]==artifact));
        }
        for package in ["driver.browser_session", "driver.desktop_session"] {
            assert!(pack["components"].as_array().unwrap().iter().any(|component| component["package_id"] == package && component["selection"] == "included"));
            assert!(pack["workspace_setup"]["project_defaults"].as_array().unwrap().iter().any(|entry| entry["package_id"] == package && entry["default_for_new_tasks"] == true));
        }
        assert_eq!(pack["workspace_setup"]["agent_presets"].as_array().unwrap().iter().map(|preset| preset["id"].as_str().unwrap()).collect::<Vec<_>>(), vec!["computer-use"]);
        let manifest = read("app/distribution/local-packages/packages/world_operator_chat.json");
        let required = manifest["required_packages"].as_array().unwrap().iter().map(|v|v.as_str().unwrap()).collect::<BTreeSet<_>>();
        assert_eq!(required,BTreeSet::from(["driver.browser_session","driver.desktop_session","driver.filesystem_workspace","driver.system_room","driver.code_mode","artifact.codex_context","world_provider_realtime","system_memory"]));
        let desktop = read("app/distribution/local-packages/packages/driver.desktop_session.json");
        assert!(!desktop["required_packages"].as_array().unwrap().contains(&serde_json::json!("world_operator_chat")), "the client-to-desktop package dependency cannot cycle back to the client");
        let driver = read("app/distribution/local-packages/packages/driver.code_mode.json");
        assert_eq!(driver["required_packages"], serde_json::json!(["driver.system_room"]));
        for (kind, owner) in [("interface", "system_code_mode"), ("worker_environment_kind", "code_mode"), ("artifact", "artifact-code-mode-browser-prelude"), ("tool", "code_mode.run"), ("tool", "code_mode.wait"), ("tool", "code_mode.resume"), ("tool", "code_mode.cancel")] {
            assert!(driver["members"].as_array().unwrap().iter().any(|member| member["registry_kind"] == kind && member["owner"] == owner));
        }
        assert!(!driver["required_packages"].as_array().unwrap().contains(&serde_json::json!("world_operator_chat")));
        let source_policy = read("registry/interface/system_worker_operator_home/policy.json");
        assert_eq!(source_policy["role_artifacts"], serde_json::json!(["artifact-worker-role-template", "artifact-grounding-system-architecture", "artifact-worker-role-operator-home", "artifact-home-task-authoring", "artifact-trigger-authoring", "artifact-spawn-literacy", "artifact-home-widget-authoring", "artifact-context-collection-guide", "artifact-memory-guide"]));
        let overlay = tempfile::tempdir().unwrap();
        copy_planner_descriptors(registry_root, overlay.path());
        let registry_root = overlay.path();
        let instruction_content = octopus_ledger::ContentHash::of_bytes(b"Home instruction source");
        let instruction_source = octopus_ledger::ArtifactRef {
            id: format!("instruction.text:{}", instruction_content.to_hex()).into(),
            content: instruction_content,
        };
        std::fs::write(
            registry_root.join("artifact/artifact-worker-role-template/artifact.json"),
            serde_json::to_vec(&serde_json::json!({"id":"artifact-worker-role-template","label":"context.reference","sources":[instruction_source]})).unwrap(),
        ).unwrap();
        let make_request = |profile: &OctopusOsAuthoringProfile, role: &str, id: &str| -> OperatorChatPlanRequest {
            serde_json::from_value(serde_json::json!({
                "registry_root":registry_root,"ee_instances_root":ee_root,"run_records_root":ee_root.join("runs"),"write_hash_algorithm":"sha256",
                "interface_private_state_root":ee_root.join("interface-private"),
                "chat_id":id,"provider_interface_id":home["provider_interface_id"],"ee_id":profile.ee_id,
                "worker_interface_id":profile.worker_choice_interface,"client_surface_role":role,"cycle":format!("{id}_cycle"),
                "door_host_slots":profile.door_host_slots,"external_inputs":profile.external_inputs,"external_relays":profile.external_relays,
                "model_outputs":profile.model_outputs,"context_artifact_bindings":profile.context_artifact_bindings,"payload_artifact_bindings":profile.payload_artifact_bindings,
                "selected_worker_environment_kinds":profile.selected_worker_environment_kinds,"selected_tools":profile.selected_tools,
                "conversation":{"ledger_offset":0,"artifact_bindings":[],"pinned_artifact_bindings":[],"pinned_context_items":[],"continued_from":null}
            })).unwrap()
        };
        let source_profile: OctopusOsAuthoringProfile = serde_json::from_value(serde_json::json!({
            "ee_id":"ee_existence","worker_choice_interface":home["worker_interface_id"],"door_host_slots":home["door_host_slots"],
            "context_artifact_bindings":home["context_artifact_bindings"],"selected_worker_environment_kinds":home["selected_worker_environment_kinds"]
        })).unwrap();
        let mut inherited = source_profile.clone();
        let mut parent = "ee_existence".to_owned();
        let mut planned_ids = BTreeSet::new();
        for (role, id) in [("home", "browser_home_pack"), ("task", "browser_project_pack"), ("task", "browser_nested_project_pack")] {
            if role == "task" {
                let ee_id = format!("ee_{id}");
                let ee = octopus_ee::EeInstance { name: None, ee_id:EeId::new(&ee_id), template:octopus_descriptors::EeTemplateId::new("project"), parent_ee:Some(EeId::new(&parent)), goal:"Declared Browser/CU profile inheritance".into(), config:Default::default(), policy_narrowing:Default::default() };
                std::fs::write(ee_root.join(format!("{ee_id}.json")), serde_json::to_vec_pretty(&ee).unwrap()).unwrap();
                inherited.ee_id = ee_id.clone();
                parent = ee_id;
            }
            validate_octopus_os_authoring_profile(&inherited, None).unwrap();
            let mut request = make_request(&inherited, role, id);
            request.ee_policy_root = Some(ee_root.to_path_buf());
            request.ee_instances_root = ee_root.join("published-ee-instances");
            let plan = plan_operator_chat_topology(&request).expect("actual Home/task chain forms the declared Browser/CU defaults");
            let provider: serde_json::Value = serde_json::from_str(&plan.interface_writes.iter()
                .find(|write| write.tag == format!("interface:{}", plan.model_interface_id)).unwrap().content).unwrap();
            let native = &provider["interface_driver_config"]["private_config"]["run_affordances"];
            assert_eq!(native["file_read_tool"], serde_json::json!(plan.capability_tools["file.read"]));
            let catalog_id = format!("artifact-run-affordances-{id}");
            let catalog: serde_json::Value = serde_json::from_str(&plan.interface_writes.iter()
                .find(|write| write.tag == format!("artifact:{catalog_id}")).unwrap().content).unwrap();
            assert_eq!(native["catalog_artifact"], serde_json::json!(octopus_ledger::ArtifactRef {
                id: catalog_id.as_str().into(),
                content: octopus_ledger::ContentHash::of_bytes(&octopus_descriptors::canonical_bytes(&catalog)),
            }));
            assert_eq!(native["roots"]["ee_instances_root"], serde_json::json!(request.ee_instances_root));
            assert_eq!(native["file_write_tool"], serde_json::json!(plan.capability_tools["file.write"]));
            assert_ne!(native["file_write_tool"], serde_json::json!(plan.authoring_tool_id));
            let authored = RunRecord::from_bytes(plan.run_record_content.as_bytes()).unwrap();
            assert!(authored.checkpoints.iter().filter(|cp| cp.interface_binding.as_ref()
                .is_some_and(|binding| binding.interface.as_str() == plan.model_interface_id))
                .any(|cp| cp.artifact_bindings.iter().any(|binding| binding.id.as_str() == catalog_id && binding.access.read)));
            if role == "home" {
                assert!(provider["interface_driver_config"]["private_config"]["instruction_artifacts"].as_array().unwrap()
                    .contains(&serde_json::json!(format!("artifact-home-delivery-{id}"))));
                let delivery: serde_json::Value = serde_json::from_str(&plan.interface_writes.iter()
                    .find(|write| write.tag == format!("artifact:artifact-home-delivery-{id}")).unwrap().content).unwrap();
                assert!(delivery.to_string().len() < 12_000, "ordinary client controls fit a bounded artifact read");
                let coordinates_id = delivery["delivery"]["authoring_artifact"].as_str().unwrap();
                let coordinates: serde_json::Value = serde_json::from_str(&plan.interface_writes.iter()
                    .find(|write| write.tag == format!("artifact:{coordinates_id}")).unwrap().content).unwrap();
                assert_eq!(coordinates["authoring"]["ee_instances_root"], serde_json::json!(request.ee_instances_root));
                assert!(authored.checkpoints.iter().any(|cp| cp.artifact_bindings.iter()
                    .any(|binding| binding.id.as_str() == coordinates_id && binding.access.read)));
                let record = RunRecord::from_bytes(plan.run_record_content.as_bytes()).unwrap();
                let topology: BoundRunAuthoringTopology = serde_json::from_value(record.interface_state[&InterfaceId::new(&plan.model_interface_id)]["run_authoring_binding"].clone()).unwrap();
                let provider_context = octopus_run_authoring::provider_context_bindings(&record, &InterfaceId::new(&plan.model_interface_id)).unwrap();
                let stable = topology.checkpoints.iter().find(|cp| cp.role == BoundCheckpointRole::Stable
                    && cp.interface_binding.interface.as_str() == plan.chat_interface_id).unwrap();
                assert!(stable.interface_binding.admits("operator.message") && !stable.interface_binding.admits("client.action.result"));
                let client_result = record.checkpoints.iter().find(|cp| cp.interface_binding.as_ref().is_some_and(|binding| binding.interface.as_str() == plan.chat_interface_id && binding.admits("client.action.result"))).unwrap();
                let client_tool: ToolDescriptor = parse_strict(plan.interface_writes.iter()
                    .find(|write| write.tag == format!("tool:{}", plan.capability_tools["client.mcp"]))
                    .unwrap().content.as_bytes()).unwrap();
                assert_eq!(client_result.outbound_interface_bindings.len(), 1);
                assert_eq!(client_result.outbound_interface_bindings[0].interface, client_tool.interface);
                assert!(!client_result.outbound_interface_bindings.iter().any(|binding| binding.interface.as_str() == plan.model_interface_id));
                assert!(stable.include_context);
                for label in ["client.surface.document", "client.action", "realtime.session.update"] {
                    let template = topology.checkpoints.iter().find(|cp| cp.role == BoundCheckpointRole::IndependentInput && cp.interface_binding.admits(label)
                        && cp.outbound_interface_bindings.iter().any(|binding| binding.admits(label))).unwrap();
                    assert!(template.static_artifact_bindings.iter().all(|id| provider_context.iter().any(|binding| &binding.id == id)), "{label}");
                    assert!(!template.include_context);
                    assert!(template.static_artifact_bindings.contains(&instruction_source.id));
                    assert_eq!(template.outbound_interface_bindings.iter()
                        .any(|binding| binding.interface.as_str() == plan.model_interface_id && binding.admits(label)), label != "client.action", "{label}");
                    let checkpoint = record.checkpoints.iter().find(|cp| cp.interface_binding.as_ref() == Some(&template.interface_binding)
                        && cp.outbound_interface_bindings == template.outbound_interface_bindings).unwrap();
                    assert!(checkpoint.artifact_bindings.iter().any(|binding| binding.id == instruction_source.id && binding.access.read));
                }
            }
            assert_eq!(plan.authoring_profile, inherited);
            assert!(planned_ids.insert(plan.capability_doors["system_code_mode"].clone()));
            let gui = &plan.capability_doors["system_gui"];
            let browser = &plan.capability_doors["system_browser"];
            assert_ne!(gui,browser);
            assert!(plan.capability_tools.contains_key("gui.app_state"));
            assert!(plan.capability_tools.contains_key("browser.navigate"));
            let exact = |tag: String| -> serde_json::Value {
                serde_json::from_str(&plan.interface_writes.iter().find(|write|write.tag == tag).unwrap().content).unwrap()
            };
            let mcp_tool = exact(format!("tool:{}", plan.capability_tools["client.mcp"]));
            assert_eq!(mcp_tool["handler_config"], read("registry/tool/client.mcp/tool.json")["handler_config"]);
            let worker_policy = exact(format!("policy:{}", plan.worker_interface_id));
            let workspace = exact(format!("interface:{}", plan.capability_doors["system_workspace_env"]));
            for slot in ["workspace_root", "host_filesystem_root"] {
                assert_eq!(workspace["interface_driver_config"]["host_slots"][slot]["path"], "/");
            }
            let provider = exact(format!("interface:{}", plan.model_interface_id));
            assert_eq!(provider["interface_driver_config"]["private_config"]["approval_policy"], "never");
            for tool in ["workspace.read_file", "workspace.write_file", "workspace.exec", "workspace.write_stdin", "workspace.process.terminate"] {
                assert!(worker_policy["ceiling"]["tools"].as_array().unwrap().contains(&serde_json::json!(plan.capability_tools[tool])));
            }
            assert!(worker_policy["ceiling"]["tools"].as_array().unwrap().contains(&serde_json::json!(plan.capability_tools["client.mcp"])));
            let sidecar = exact(format!("octopus-os:{}", plan.chat_interface_id));
            let parsed: OctopusOsInterfaceSidecar = serde_json::from_value(sidecar.clone()).unwrap();
            assert_eq!(parsed.surface_role, role);
            assert_eq!(parsed.authoring_profile.as_ref(), Some(&inherited));
            let owner = &plan.capability_doors["system_code_mode"];
            let code_mode = exact(format!("sidecar:{owner}:code_mode.json"));
            assert_eq!(code_mode["context"]["mode"], "persistent");
            let prelude = exact(format!("artifact:{}",code_mode["context"]["prelude_artifact"]["id"].as_str().unwrap()));
            assert_eq!(prelude["exports"].as_array().unwrap().iter().map(|v|v.as_str().unwrap()).collect::<BTreeSet<_>>(),BTreeSet::from(["browser","cua"]));
            let gui_config=exact(format!("sidecar:{gui}:desktop.json"));
            assert_eq!(gui_config["app_approval"]["operator_interface"],plan.chat_interface_id);
            assert_eq!(gui_config["app_approval"]["resume_tool"],plan.capability_tools["gui.app_resume"]);
            let topology=exact(format!("artifact:{}",code_mode["handoff"]["topology_artifact"]["id"].as_str().unwrap()));
            for (initial,resume) in [("gui.app_state","gui.app_resume"),("browser.filechooser_set_files","browser.filechooser_resume")] {
                let route=topology["routes"].as_array().unwrap().iter().find(|route|route["tool"]==plan.capability_tools[initial]).unwrap();
                assert_eq!(route["app_approval"]["operator_interface"],plan.chat_interface_id);
                assert_eq!(route["app_approval"]["resume_tool"],plan.capability_tools[resume]);
            }
            assert!(prelude["module_source"].as_str().unwrap().contains("api.documentation"));
            let run_tool = exact(format!("tool:{}",plan.capability_tools["code_mode.run"]));
            assert!(run_tool["description"].as_str().unwrap().contains("prelude.browser"));
            assert!(run_tool["description"].as_str().unwrap().contains("documentation()"));
            let run:serde_json::Value = serde_json::from_str(&plan.run_record_content).unwrap();
            let provider_context = &run["checkpoints"];
            for artifact in ["artifact-worker-role-template", "artifact-grounding-system-architecture", "artifact-codebase-layout"] {
                assert!(provider_context.as_array().unwrap().iter().filter(|cp|cp["outbound_interface_bindings"].as_array().unwrap().iter().any(|binding|binding["interface"] == plan.model_interface_id)).any(|cp|cp["artifact_bindings"].as_array().unwrap().iter().any(|binding|binding["id"] == artifact)), "Home retains original model context {artifact}");
            }
            assert!(!run.to_string().contains("artifact-worker-role-web"));
            for write in plan.interface_writes.iter().chain(&plan.run_writes) {
                assert!(write.path.starts_with(registry_root) || (write.path.parent() == Some(ee_root.join("runs").as_path()) && write.tag.starts_with("run:") && write.expected_before_hash.is_none()));
                std::fs::create_dir_all(write.path.parent().unwrap()).unwrap();
                std::fs::write(&write.path,&write.content).unwrap();
            }
            let sidecar_path = registry_root.join("interface").join(&plan.chat_interface_id).join("octopus_os.json");
            let mut saved_sidecar = sidecar.clone();
            if role == "home" {
                let mut task_profile = serde_json::to_value(&inherited).unwrap();
                for field in ["external_inputs", "external_relays", "model_outputs", "selected_tools", "payload_artifact_bindings"] {
                    task_profile[field] = serde_json::json!([]);
                }
                saved_sidecar["authoring_profile"] = task_profile.clone();
                saved_sidecar["task_authoring_profiles"] = serde_json::json!([task_profile]);
                saved_sidecar["presentation"] = serde_json::json!({"name":"", "projects":[]});
                std::fs::write(&sidecar_path, pretty_json(&saved_sidecar).unwrap()).unwrap();
            }
            let mut continuation=make_request(&inherited,role,id);
            continuation.cycle=format!("{id}_next");
            let mut current_context = run.clone();
            let binding: BoundRunAuthoringTopology = serde_json::from_value(run["interface_state"][&plan.model_interface_id]["run_authoring_binding"].clone()).unwrap();
            let history = serde_json::to_value([
                ArtifactBinding { id:octopus_descriptors::ArtifactId::new("turn.interrupted.result:current"), access:Default::default() },
                ArtifactBinding { id:octopus_descriptors::ArtifactId::new("tool.result:current-tail"), access:octopus_policy::ArtifactAccess { change:true, ..Default::default() } },
            ]).unwrap();
            for checkpoint in current_context["checkpoints"].as_array_mut().unwrap() {
                if run["active_checkpoints"].as_array().unwrap().contains(&checkpoint["id"])
                    && binding.checkpoints.iter().any(|template| template.role == BoundCheckpointRole::Stable && template.include_context
                        && checkpoint_matches_bound_template(checkpoint, template, &plan.run_id).unwrap()) {
                    checkpoint["artifact_bindings"].as_array_mut().unwrap().extend(history.as_array().unwrap().clone());
                }
            }
            continuation.conversation.artifact_bindings = vec![ArtifactBindingSeed { id:"interface.artifact.output:stale".into() }];
            continuation.existing_run_record=Some(current_context);
            let next=plan_operator_chat_topology(&continuation).expect("ordinary existing Home/task continues the same selected Browser/CU path");
            let next_record = RunRecord::from_bytes(next.run_record_content.as_bytes()).unwrap();
            let context = octopus_run_authoring::provider_context_bindings(&next_record, &InterfaceId::new(&plan.model_interface_id)).unwrap();
            assert_eq!(serde_json::to_value(&context[context.len()-2..]).unwrap(), history);
            assert!(!context.iter().any(|binding| binding.id.as_str() == "interface.artifact.output:stale"));
            assert_eq!(next.run_id, plan.run_id);
            assert_eq!(next.chat_interface_id, plan.chat_interface_id);
            assert_eq!(next.model_interface_id, plan.model_interface_id);
            assert_eq!(next.worker_interface_id, plan.worker_interface_id);
            assert_eq!(next.capability_doors, plan.capability_doors);
            assert_eq!(next.capability_tools, plan.capability_tools);
            assert_eq!(next.authoring_profile, inherited);
            assert_eq!(serde_json::to_value(&next).unwrap()["authoring_profile"], saved_sidecar["authoring_profile"]);
            assert!(!next.interface_writes.iter().any(|write| write.path == sidecar_path), "ordinary continuation must not rewrite unrelated client configuration");
            if role == "home" {
                use crate::run_composition::{CompositionRoots, current_run::*};
                use octopus_run_authoring::capability_amendment::ProviderCapabilityBridge;
                let roots = CompositionRoots { registry_root:registry_root.to_path_buf(), ee_instances_root:request.ee_instances_root.clone(),
                    run_records_root:ee_root.join("runs"), interface_private_state_root:ee_root.join("interface-private") };
                std::fs::create_dir_all(&roots.ee_instances_root).unwrap();
                boot_operator_space_ee_instances(&roots.ee_instances_root);
                std::fs::write(roots.run_records_root.join(format!("{}.json",plan.run_id)), &plan.run_record_content).unwrap();
                let current = RunRecord::from_bytes(plan.run_record_content.as_bytes()).unwrap();
                let provider = InterfaceId::new(&plan.model_interface_id);
                let selection = current_selection(&roots,&current,&provider).unwrap().unwrap();
                for tool in ["context.discover", "context.read", "context.prepare_publication"] {
                    assert!(selection.selected_tools.iter().any(|selected| selected.tool == tool));
                }
                let voice: home_voice::HomeVoiceContext = serde_json::from_value(run["interface_state"][&plan.chat_interface_id]["voice_context"].clone()).unwrap();
                let voice_paths = [roots.run_records_root.join(format!("{}.json",voice.run_id)),
                    registry_root.join("interface").join(&voice.provider_id).join("interface.json"),
                    registry_root.join("interface").join(&voice.provider_id).join("life.json")];
                let accepted_voice = voice_paths.iter().map(|path| (path.clone(),std::fs::read(path).unwrap())).collect::<BTreeMap<_,_>>();
                let source_path = registry_root.join("interface/world_provider_realtime/interface.json");
                let source_bytes = std::fs::read(&source_path).unwrap();
                let mut source: serde_json::Value = serde_json::from_slice(&source_bytes).unwrap();
                source["interface_driver_config"]["private_config"]["session"]["audio"]["output"]["voice"] = serde_json::json!("changed source default");
                std::fs::write(&source_path,pretty_json(&source).unwrap()).unwrap();
                let mut retained = make_request(&inherited,role,id);
                retained.existing_run_record = Some(run.clone()); retained.retain_current_capabilities = true;
                let retained = plan_operator_chat_topology(&retained).unwrap();
                assert!(!retained.interface_writes.iter().any(|write| write.tag == format!("policy:{}",plan.worker_interface_id)));
                assert!(!retained.interface_writes.iter().chain(&retained.run_writes).any(|write| accepted_voice.contains_key(&write.path)));
                let door = InterfaceId::new(&plan.capability_doors["system_room"]);
                let bridge = ProviderCapabilityBridge { tool:plan.capability_tools["file.read"].clone(),correlation_id:"home-read".into(),
                    arguments_hash:octopus_ledger::ContentHash::of_bytes(b"{}").to_hex(),return_interface:door.clone() };
                let amend = |record: &RunRecord| plan_current_run_amendment(&roots,record,&provider,CurrentRunRequest {
                    amendment_key:"home-context-retained".into(),selection:Some(selection.clone()),authored:None,publications:vec![],
                },bridge.clone(),&[(plan.capability_tools["file.read"].clone(),door.clone())]);
                let preview = amend(&current).unwrap();
                assert!(preview.validation.safe_to_apply,"{:?}",preview.validation.regressions);
                assert_eq!(preview.amendment.after,amend(&current).unwrap().amendment.after);
                assert_eq!(preview.amendment.after["interface_state"][&plan.model_interface_id]["run_authoring_binding"],run["interface_state"][&plan.model_interface_id]["run_authoring_binding"]);
                assert!(!preview.dependency_writes.iter().chain(&preview.activation_writes).any(|write| accepted_voice.contains_key(&write.path)));
                for (path,bytes) in &accepted_voice { assert_eq!(&std::fs::read(path).unwrap(),bytes); }
                let policy_path = registry_root.join("interface").join(&plan.worker_interface_id).join("policy.json");
                let policy_bytes = std::fs::read(&policy_path).unwrap();
                let mut policy: serde_json::Value = serde_json::from_slice(&policy_bytes).unwrap();
                assert!(!policy["ceiling"]["interfaces"].as_array().unwrap().contains(&serde_json::json!("world_provider_realtime")));
                policy["ceiling"]["interfaces"].as_array_mut().unwrap().retain(|id| id != &serde_json::json!(voice.provider_id));
                if let Some(policies) = policy["ceiling"]["interface_policy"].as_object_mut() { policies.remove(&voice.provider_id); }
                std::fs::write(&policy_path,pretty_json(&policy).unwrap()).unwrap();
                assert!(amend(&current).unwrap_err().contains("Home voice provider is absent"));
                std::fs::write(&policy_path,policy_bytes).unwrap();
                let mut wrong = current.clone();
                wrong.interface_state.get_mut(&InterfaceId::new(&plan.chat_interface_id)).unwrap().get_mut("voice_context").unwrap()["provider_id"] = serde_json::json!("world_provider_realtime");
                assert!(amend(&wrong).unwrap_err().contains("declared source identity"));
                std::fs::write(&source_path,source_bytes).unwrap();
                continuation.context_artifact_bindings.push(ArtifactBindingSeed { id: "artifact-secret-handle-flow".into() });
                let amended = plan_operator_chat_topology(&continuation).unwrap();
                let edited: serde_json::Value = serde_json::from_str(&amended.interface_writes.iter().find(|write| write.path == sidecar_path).unwrap().content).unwrap();
                for field in ["task_authoring_profiles", "presentation"] {
                    assert_eq!(edited[field], saved_sidecar[field], "capability amendment preserves the authored {field}");
                }
                let mut expected_profile = saved_sidecar["authoring_profile"].clone();
                expected_profile["context_artifact_bindings"] = serde_json::to_value(&continuation.context_artifact_bindings).unwrap();
                assert_eq!(edited["authoring_profile"], expected_profile);
                assert_eq!(serde_json::to_value(&amended).unwrap()["authoring_profile"], expected_profile);
                saved_sidecar["task_authoring_profiles"][0]["undeclared"] = serde_json::json!(true);
                std::fs::write(&sidecar_path, pretty_json(&saved_sidecar).unwrap()).unwrap();
                assert!(plan_operator_chat_topology(&continuation).unwrap_err().contains("unknown field"));
                saved_sidecar["task_authoring_profiles"][0].as_object_mut().unwrap().remove("undeclared");
                saved_sidecar["presentation"] = serde_json::Value::Null;
                std::fs::write(&sidecar_path, pretty_json(&saved_sidecar).unwrap()).unwrap();
                continuation.context_artifact_bindings = inherited.context_artifact_bindings.clone();
                assert!(!plan_operator_chat_topology(&continuation).unwrap().interface_writes.iter().any(|write| write.path == sidecar_path));
            }
            let ee_path = ee_root.join(format!("{}.json", inherited.ee_id));
            let ee_bytes = std::fs::read(&ee_path).unwrap();
            let mut current_ee: octopus_ee::EeInstance = serde_json::from_slice(&ee_bytes).unwrap();
            current_ee.policy_narrowing.allowed_worker_environment_kinds = Some(
                source_policy["ceiling"]["wek"].as_array().unwrap().iter()
                    .filter_map(|kind| (kind != "desktop_session").then(|| kind.as_str().unwrap().to_owned()))
                    .collect(),
            );
            current_ee.policy_narrowing.allowed_tools = Some(
                source_policy["ceiling"]["tools"].as_array().unwrap().iter().filter_map(|tool| {
                    let id = tool.as_str().unwrap();
                    let descriptor = read(&format!("registry/tool/{id}/tool.json"));
                    (!descriptor["supported_worker_environment_kinds"].as_array().unwrap()
                        .contains(&serde_json::json!("desktop_session"))).then(|| id.to_owned())
                }).collect(),
            );
            std::fs::write(&ee_path, serde_json::to_vec_pretty(&current_ee).unwrap()).unwrap();
            let refused = plan_operator_chat_topology(&continuation)
                .err().expect("the same existing run cannot restore desktop outside its current EE policy");
            assert_eq!(refused, "worker environment kind 'desktop_session' is outside the formed worker/EE grant");
            std::fs::write(&ee_path, ee_bytes).unwrap();
            let mut narrowed=make_request(&inherited,role,&format!("{id}_narrowed"));
            narrowed.selected_worker_environment_kinds=vec!["browser_session".into(),"code_mode".into()];
            narrowed.door_host_slots.remove("system_workspace_env");
            narrowed.context_artifact_bindings.retain(|reference|reference.id!="artifact-code-mode-cua-prelude");
            let narrower=plan_operator_chat_topology(&narrowed).expect("explicitly deselected desktop is not restored by the default");
            assert!(!narrower.capability_doors.contains_key("system_gui"));
            inherited = plan.authoring_profile;
        }
    }

    pub(super) fn copy_planner_descriptors(source: &Path, target: &Path) {
        std::fs::create_dir_all(target).unwrap();
        for entry in std::fs::read_dir(source).unwrap() {
            let entry = entry.unwrap();
            let destination = target.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                // Cargo package implementations are not planner inputs; nested operation
                // catalogs and every other descriptor/sidecar directory remain byte-exact.
                if entry.file_name() != "packages" { copy_planner_descriptors(&entry.path(), &destination); }
            } else if entry.file_type().unwrap().is_file() {
                std::fs::copy(entry.path(), destination).unwrap();
            } else {
                panic!("source descriptor fixture contains a nonregular entry: {}",entry.path().display());
            }
        }
    }

    fn assert_browser_only_persistent_plan(registry_root: &Path, ee_root: &Path) {
        // Recovery reads the actual durable sidecars, not just an in-memory RunRecord.
        // Keep this source-only overlay and all planned writes for evidence; no executable runs.
        let overlay = tempfile::tempdir().unwrap().keep();
        copy_planner_descriptors(registry_root, &overlay);
        let registry_root = overlay.as_path();
        println!("browser planner source overlay retained at {}", registry_root.display());
        let mut request = OperatorChatPlanRequest {
            registry_root:registry_root.into(), ee_instances_root:ee_root.into(), ee_policy_root:None, run_records_root:Some(ee_root.join("runs")),
            write_hash_algorithm:WriteHashAlgorithm::Sha256, interface_private_state_root:Some(ee_root.join("private")),
            chat_id:"browser_persistent_identity".into(), provider_interface_id:"world_provider_codex".into(),
            model:None, reasoning_effort:None, service_tier:None, ee_id:Some("ee_operator_space".into()),
            worker_interface_id:Some("system_worker_coder_max".into()), client_surface_role:"task".into(),
            cycle:"browser_persistent_cycle".into(), door_host_slots:BTreeMap::new(),
            external_inputs:Vec::new(), external_relays:Vec::new(), model_outputs:Vec::new(),
            context_artifact_bindings:Vec::new(),
            payload_artifact_bindings:Vec::new(), publications:Vec::new(),
            selected_worker_environment_kinds:vec!["browser_session".into(),"code_mode".into()],
            selected_tools:Vec::new(), retain_current_capabilities:false, existing_run_record:None,
            conversation:ConversationSeed { ledger_offset:0, artifact_bindings:Vec::new(), pinned_artifact_bindings:Vec::new(), pinned_context_items:Vec::new(), continued_from:None },
        };
        let exact = |plan: &OperatorChatPlan, tag: String| -> serde_json::Value {
            serde_json::from_str(&plan.interface_writes.iter().find(|write|write.tag==tag).expect("exact source write").content).unwrap()
        };
        let unbound = plan_operator_chat_topology(&request).unwrap();
        let isolated = exact(&unbound,format!("sidecar:{}:code_mode.json",unbound.capability_doors["system_code_mode"]));
        assert_eq!(isolated["context"]["mode"],"isolated");
        request.selected_worker_environment_kinds.clear();
        let selected_browser_tool = |name: &str| SelectedToolSeed { tool:name.into(),interface:"system_browser".into(),worker_environment_kind:"browser_session".into() };
        request.selected_tools = vec![selected_browser_tool("browser.navigate")];
        let partial = plan_operator_chat_topology(&request).expect("non-upload partial browser selection does not require an unselected continuation");
        let partial_browser = exact(&partial,format!("interface:{}",partial.capability_doors["system_browser"]));
        assert!(partial_browser["interface_driver_config"]["private_config"]["browser_room"].get("upload_approval").is_none());
        assert!(!partial.capability_tools.contains_key("browser.filechooser_resume"));
        assert!(partial_browser["interface_driver_config"]["private_config"]["browser_room"].get("wait_completion").is_none());
        request.selected_tools.push(selected_browser_tool("browser.wait"));
        assert!(plan_operator_chat_topology(&request).is_err(), "a selected wait must include its declared independent completion");
        request.selected_tools.push(selected_browser_tool("browser.wait_complete"));
        let paired = plan_operator_chat_topology(&request).unwrap();
        let paired_browser = exact(&paired,format!("interface:{}",paired.capability_doors["system_browser"]));
        assert_eq!(paired_browser["interface_driver_config"]["private_config"]["browser_room"]["wait_completion"],serde_json::json!({"request_tools":[paired.capability_tools["browser.wait"]],"completion_tool":paired.capability_tools["browser.wait_complete"]}));
        request.selected_tools.retain(|tool| tool.tool != "browser.wait");
        assert!(plan_operator_chat_topology(&request).is_err(), "orphan completion cannot obtain a request role");
        request.selected_tools.retain(|tool| tool.tool != "browser.wait_complete");
        request.selected_tools.push(selected_browser_tool("browser.filechooser_set_files"));
        assert!(plan_operator_chat_topology(&request).is_err());
        request.selected_tools.push(selected_browser_tool("browser.filechooser_resume"));
        let selected = plan_operator_chat_topology(&request).unwrap();
        let selected_browser = exact(&selected,format!("interface:{}",selected.capability_doors["system_browser"]));
        assert_eq!(selected_browser["interface_driver_config"]["private_config"]["browser_room"]["upload_approval"]["request_tools"],serde_json::json!([selected.capability_tools["browser.filechooser_set_files"]]));
        request.selected_tools.clear();
        request.selected_worker_environment_kinds = vec!["browser_session".into(),"code_mode".into()];
        request.context_artifact_bindings.push(ArtifactBindingSeed { id:"artifact-code-mode-browser-prelude".into() });
        let plan = plan_operator_chat_topology(&request).expect("explicit browser-only persistent recipe uses existing Coder Max grants");
        assert!(!plan.capability_doors.contains_key("system_gui"));
        assert!(!plan.capability_tools.keys().any(|tool|tool.starts_with("gui.")));
        let owner = &plan.capability_doors["system_code_mode"];
        let browser = &plan.capability_doors["system_browser"];
        let sidecar = exact(&plan,format!("sidecar:{owner}:code_mode.json"));
        assert_eq!(sidecar["context"]["mode"],"persistent");
        let prelude_ref = &sidecar["context"]["prelude_artifact"];
        let topology_ref = &sidecar["handoff"]["topology_artifact"];
        let prelude = exact(&plan,format!("artifact:{}",prelude_ref["id"].as_str().unwrap()));
        let topology = exact(&plan,format!("artifact:{}",topology_ref["id"].as_str().unwrap()));
        assert_eq!(prelude["exports"],serde_json::json!(["browser"]));
        assert!(!prelude["module_source"].as_str().unwrap().contains("createCua"));
        let source_path = registry_root.join("artifact/artifact-code-mode-browser-prelude/artifact.json");
        let source_bytes = std::fs::read(&source_path).unwrap();
        let source_body:serde_json::Value = serde_json::from_slice(&source_bytes).unwrap();
        let duplicated_roles = br#"{"schema_version":1,"module_source":"function createBrowser(){}","export_factory":"createBrowser","export_name":"browser","tool_roles":{"tabs":{"tool":"browser.tabs","interface":"system_browser"},"tabs":{"tool":"browser.close","interface":"system_browser"}}}"#;
        assert!(serde_json::from_slice::<CodeModePreludeSource>(duplicated_roles).is_err());
        assert!(prelude["module_source"].as_str().unwrap().contains(source_body["module_source"].as_str().unwrap()));
        let run:serde_json::Value = serde_json::from_str(&plan.run_record_content).unwrap();
        let source = &run["interface_state"][owner]["prelude_sources"][0];
        assert_eq!(run["interface_state"][owner]["prelude_sources"].as_array().unwrap().len(),1);
        let source_ref:octopus_ledger::ArtifactRef=serde_json::from_value(source["source"].clone()).unwrap();
        assert_eq!(source_ref.id.as_str(),"artifact-code-mode-browser-prelude");
        assert_eq!(source_ref.content,octopus_ledger::ContentHash::of_bytes(&octopus_descriptors::canonical_bytes(&source_body)));
        assert_eq!(source["source_sha256"],format!("{:x}",Sha256::digest(&source_bytes)));
        let snapshot = plan.interface_writes.iter().find(|write|write.tag==format!("artifact:{}",source["snapshot"]["id"].as_str().unwrap())).unwrap();
        let snapshot_body:serde_json::Value=serde_json::from_str(&snapshot.content).unwrap();
        assert_eq!(snapshot_body["id"],source["snapshot"]["id"]);
        assert_eq!(snapshot_body["run"],plan.run_id);
        assert_eq!(snapshot_body["source"],source["source"]);
        assert_eq!(snapshot_body["source_content"].as_str().unwrap().as_bytes(),source_bytes);
        assert_eq!(snapshot_body["source_sha256"],source["source_sha256"]);
        let snapshot_ref:octopus_ledger::ArtifactRef=serde_json::from_value(source["snapshot"].clone()).unwrap();
        assert_eq!(snapshot_ref.content,octopus_ledger::ContentHash::of_bytes(&octopus_descriptors::canonical_bytes(&snapshot_body)));
        let provider = exact(&plan,format!("interface:{}",plan.model_interface_id));
        let hidden = provider["interface_driver_config"]["private_config"]["model_hidden_tools"].as_array().unwrap();
        let routes = topology["routes"].as_array().unwrap();
        let methods = plan.capability_tools.iter().filter(|(base,_)|base.starts_with("browser.")).collect::<Vec<_>>();
        let source_roles: CodeModePreludeSource = serde_json::from_slice(&source_bytes).unwrap();
        let source_browser:serde_json::Value=serde_json::from_slice(&std::fs::read(registry_root.join("interface/system_browser/interface.json")).unwrap()).unwrap();
        let source_approval=&source_browser["interface_driver_config"]["private_config"]["browser_room"]["upload_approval"];
        let resume_base=source_approval["resume_tool"].as_str().unwrap();
        let resume=&plan.capability_tools[resume_base];
        let expected_approval=serde_json::json!({"operator_interface":plan.chat_interface_id,"resume_tool":resume});
        let source_completion=&source_browser["interface_driver_config"]["private_config"]["browser_room"]["wait_completion"];
        let completion_base=source_completion["completion_tool"].as_str().unwrap();
        let completion=&plan.capability_tools[completion_base];
        let browser_descriptor=exact(&plan,format!("interface:{browser}"));
        let mut expected_config = expected_approval.clone();
        expected_config["request_tools"] = serde_json::json!(source_approval["request_tools"].as_array().unwrap().iter().map(|tool| &plan.capability_tools[tool.as_str().unwrap()]).collect::<Vec<_>>());
        expected_config["permission_policy"] = source_approval["permission_policy"].clone();
        assert_eq!(browser_descriptor["interface_driver_config"]["private_config"]["browser_room"]["upload_approval"],expected_config);
        let mut unchanged_room=browser_descriptor["interface_driver_config"]["private_config"]["browser_room"].clone();
        unchanged_room["upload_approval"]=source_approval.clone();
        assert_eq!(unchanged_room["wait_completion"],serde_json::json!({"request_tools":source_completion["request_tools"].as_array().unwrap().iter().map(|tool| &plan.capability_tools[tool.as_str().unwrap()]).collect::<Vec<_>>(),"completion_tool":completion}));
        unchanged_room["wait_completion"]=source_completion.clone();
        assert_eq!(unchanged_room,source_browser["interface_driver_config"]["private_config"]["browser_room"]);
        let mut source_tools = source_roles.tool_roles.values().map(|role| role.tool.as_str()).collect::<BTreeSet<_>>();
        assert!(source_tools.insert(resume_base),"private resume must not become an SDK export");
        assert!(source_tools.contains(completion_base), "the source facade declares its exact wait completion companion");
        assert_eq!(methods.iter().map(|(base, _)| base.as_str()).collect::<BTreeSet<_>>(), source_tools);
        assert_eq!(routes.len() + 1,methods.len());
        assert!(!routes.iter().any(|route|route["tool"]==*resume));
        let mut expected_nested_approval=expected_approval.clone();
        expected_nested_approval["resume_descriptor"]=exact(&plan,format!("tool:{resume}"));
        for (base,tool) in methods {
            assert!(hidden.contains(&serde_json::json!(tool)));
            if tool == resume { continue; }
            let route=routes.iter().find(|route|route["tool"]==*tool).expect("every exact selected browser tool stays callable");
            assert_eq!(route["call"]["binding"]["outbound"][0]["interface"],*browser);
            assert_eq!(route["result"]["binding"]["outbound"][0]["interface"],*owner);
            if source_approval["request_tools"].as_array().unwrap().iter().any(|request| request == base.as_str()) {
                assert_eq!(route["app_approval"],expected_nested_approval);
                assert_ne!(route["app_approval"]["resume_tool"],resume_base);
            } else { assert!(route["app_approval"].is_null()); }
        }
        assert_nested_owner_returns(&plan,&topology);
        let request_route=routes.iter().find(|route| route["tool"] == plan.capability_tools["browser.wait"]).unwrap();
        let completion_route=routes.iter().find(|route| route["tool"] == *completion).unwrap();
        assert_ne!(request_route["result"]["name"],completion_route["result"]["name"]);
        for base in ["browser.viewport_set","browser.viewport_reset","browser.download_arm","browser.download_wait","browser.download_cancel","browser.tab_mark","browser.filechooser_arm","browser.filechooser_wait","browser.filechooser_set_files","browser.filechooser_cancel","browser.filechooser_resume"] {
            assert!(plan.capability_tools.contains_key(base));
        }
        let templates=run["interface_state"][&plan.model_interface_id]["run_authoring_binding"]["checkpoints"].as_array().unwrap();
        assert_eq!(templates.iter().filter(|cp|cp["role"]=="owner_input" && cp["interface_binding"]["interface"]==*owner).count(),1);
        assert!(!templates.iter().any(|cp|cp["interface_binding"]["interface"]==*browser||cp["outbound_interface_bindings"].as_array().unwrap().iter().any(|out|out["interface"]==*browser)));
        for write in plan.interface_writes.iter().chain(&plan.run_writes) {
            assert!(write.path.starts_with(registry_root) || (write.path.parent() == Some(ee_root.join("runs").as_path()) && write.tag.starts_with("run:") && write.expected_before_hash.is_none()));
            std::fs::create_dir_all(write.path.parent().unwrap()).unwrap();
            std::fs::write(&write.path, &write.content).unwrap();
        }
        request.existing_run_record=Some(run.clone());
        request.cycle="browser_persistent_next".into();
        let resumed=plan_operator_chat_topology(&request).expect("replanning preserves exact source provenance and live owner pins");
        let resumed_run:serde_json::Value=serde_json::from_str(&resumed.run_record_content).unwrap();
        assert_eq!(resumed_run["interface_state"][owner]["prelude_sources"],run["interface_state"][owner]["prelude_sources"]);
        let published_path=registry_root.join("artifact").join(source["source"]["id"].as_str().unwrap()).join("artifact.json");
        let mut updated:serde_json::Value=serde_json::from_slice(&source_bytes).unwrap();
        updated["module_source"]=serde_json::json!(format!("{}\n// New package revision",updated["module_source"].as_str().unwrap()));
        std::fs::write(&published_path,updated.to_string()).unwrap();
        let retained=plan_operator_chat_topology(&request).expect("package updates preserve the existing exact prelude snapshot");
        assert!(!retained.interface_writes.iter().any(|write|write.tag==format!("artifact:{}",prelude["id"].as_str().unwrap())));
        let retained_run:serde_json::Value=serde_json::from_str(&retained.run_record_content).unwrap();
        assert_eq!(retained_run["interface_state"][owner]["prelude_sources"],run["interface_state"][owner]["prelude_sources"]);
        let snapshot_path=registry_root.join("artifact").join(source["snapshot"]["id"].as_str().unwrap()).join("artifact.json");
        let original=std::fs::read(&snapshot_path).unwrap();
        std::fs::write(&snapshot_path,b"{}").unwrap();assert!(plan_operator_chat_topology(&request).is_err());
        std::fs::remove_file(&snapshot_path).unwrap();assert!(plan_operator_chat_topology(&request).is_err());
        std::fs::write(&snapshot_path,original).unwrap();std::fs::write(published_path,&source_bytes).unwrap();
        request.existing_run_record.as_mut().unwrap()["interface_state"][owner]["prelude_sources"][0]["source_sha256"]=serde_json::json!("0".repeat(64));
        assert!(plan_operator_chat_topology(&request).is_err());
        request.existing_run_record=None;
        request.context_artifact_bindings.push(ArtifactBindingSeed { id:"artifact-code-mode-browser-prelude".into() });
        assert!(plan_operator_chat_topology(&request).is_err());
        request.context_artifact_bindings.pop();
        request.context_artifact_bindings.push(ArtifactBindingSeed { id:"artifact-code-mode-cua-prelude".into() });
        assert!(plan_operator_chat_topology(&request).is_err(),"a bound CUA source cannot obtain missing GUI roles through a browser-only task");
    }


    #[test]
    fn scheduled_task_plan_binds_external_time_private_auth_and_project_roots() {
        let repository = repository_registry_root();
        let registry_root = repository.join("registry");
        let ee_dir = tempfile::tempdir().expect("temp EE instances");
        let private_root = tempfile::tempdir().expect("temp private interface root");
        let project_root = tempfile::tempdir().expect("temp project root");
        boot_operator_space_ee_instances(ee_dir.path());
        let secondary_root = tempfile::tempdir().expect("temp secondary source folder");
        let folders = vec![project_root.path().canonicalize().unwrap(), secondary_root.path().canonicalize().unwrap()];
        let ee_file = ee_dir.path().join("ee_operator_space.json");
        let mut ee: serde_json::Value = serde_json::from_slice(&std::fs::read(&ee_file).unwrap()).unwrap();
        ee["config"]["octopus_os"]["source_folders"] = serde_json::json!(folders);
        std::fs::write(&ee_file, serde_json::to_vec(&ee).unwrap()).unwrap();

        let mut request = OperatorChatPlanRequest {
            registry_root,
            ee_instances_root: ee_dir.path().to_path_buf(),
            ee_policy_root: None,
            run_records_root: Some(ee_dir.path().join("runs")),
            write_hash_algorithm: WriteHashAlgorithm::Blake3,
            interface_private_state_root: Some(private_root.path().to_path_buf()),
            chat_id: "scheduledprobe000000000001".to_string(),
            provider_interface_id: "world_provider_codex".to_string(),
            model: Some("gpt-5.6-terra".to_string()),
            reasoning_effort: Some("ultra".to_string()),
            service_tier: Some("priority".to_string()),
            ee_id: Some("ee_operator_space".to_string()),
            worker_interface_id: Some("system_worker_coder_max".to_string()),
            client_surface_role: "task".to_string(),
            cycle: "scheduledprobe000000000001".to_string(),
            door_host_slots: BTreeMap::from([(
                "system_workspace_env".to_string(),
                BTreeMap::from([(
                    "workspace_root".to_string(),
                    project_root.path().to_path_buf(),
                )]),
            )]),
            external_inputs: vec![ExternalInputSeed {
                checkpoint_id: "cp_scheduled_probe".to_string(),
                interface_id: "world_cron".to_string(),
                artifact_names: vec!["cron.current_time.scheduledprobe".to_string()],
                required_verified_artifacts: Vec::new(),
            }],
            external_relays: vec![ExternalRelaySeed {
                checkpoint_id: "cp_project_result_probe".to_string(),
                interface_id: "system_worker_planner".to_string(),
                outbound_interface_ids: vec![
                    "system_worker_coder".to_string(),
                    "system_worker_web".to_string(),
                ],
                required_verified_artifacts: vec![RequiredVerifiedArtifact {
                    mark: octopus_descriptors::MarkId::new("task.summary.received"),
                    artifact_label: "task.summary".to_string(),
                    verifier: octopus_descriptors::VerifierId::new("artifact_exists"),
                    params: serde_json::json!({}),
                }],
            }],
            model_outputs: Vec::new(),
            context_artifact_bindings: Vec::new(),
            payload_artifact_bindings: Vec::new(),
            selected_worker_environment_kinds: vec!["filesystem_workspace".to_string()],
            selected_tools: Vec::new(),
            retain_current_capabilities: false,
            existing_run_record: None,
            publications: Vec::new(),
            conversation: ConversationSeed {
                ledger_offset: 0,
                artifact_bindings: Vec::new(),
                pinned_artifact_bindings: Vec::new(),
                pinned_context_items: Vec::new(),
                continued_from: None,
            },
        };
        let plan = plan_operator_chat_topology(&request).expect("scheduled task topology plans");
        request.model = Some("gpt-model-outside-formed-policy".to_string());
        let invalid_model = match plan_operator_chat_topology(&request) {
            Ok(_) => panic!("a model outside formed interface policy must fail closed"),
            Err(error) => error,
        };
        assert!(invalid_model.contains("formed model catalog does not admit selected model"));
        assert_eq!(
            plan.expected_before_hash_algorithm,
            WriteHashAlgorithm::Blake3
        );
        assert_eq!(plan.model, "gpt-5.6-terra");
        assert_eq!(plan.reasoning_effort, "ultra");
        assert_eq!(plan.service_tier.as_deref(), Some("priority"));
        assert_eq!(
            plan.model_interface_id,
            "world_provider_codex_scheduledprobe000000000001"
        );

        let provider: InterfaceDescriptor = serde_json::from_str(
            &plan
                .interface_writes
                .iter()
                .find(|write| write.tag == format!("interface:{}", plan.model_interface_id))
                .expect("exact provider descriptor write")
                .content,
        )
        .expect("exact provider descriptor parses");
        let provider_config = provider
            .interface_driver_config
            .as_ref()
            .expect("exact provider driver config");
        assert_eq!(
            provider_config.residual_ambient.as_ref().unwrap()["private_auth_store"]["auth_file_env"],
            "OCTOPUS_MACHINE_CODEX_AUTH_FILE"
        );
        assert!(
            !provider_config
                .host_slots
                .contains_key("private_auth_store_owner_root")
        );
        assert_eq!(
            provider_config.residual_ambient.as_ref().unwrap()["spawn"]["connect_on_ready"],
            false
        );
        assert_eq!(
            provider_config.private_config["model"],
            serde_json::json!("gpt-5.6-terra")
        );
        assert_eq!(
            provider_config.private_config["reasoning_effort"],
            serde_json::json!("ultra")
        );
        assert_eq!(provider_config.private_config["service_tier"], serde_json::json!("priority"));
        assert_eq!(provider_config.private_config["approval_policy"], serde_json::json!("on-request"));
        assert_eq!(
            serde_json::from_value::<BTreeSet<String>>(provider.interface_policy.values["models"].clone()).unwrap(),
            serde_json::from_value::<BTreeSet<String>>(load_interface_descriptor(&request.registry_root.join("interface"), &request.provider_interface_id)
                .expect("declared provider model ceiling")
                .interface_policy.variables["models"].default.clone()).unwrap()
        );

        let life: SpawnLifeConfig = serde_json::from_str(
            &plan
                .interface_writes
                .iter()
                .find(|write| write.tag == format!("life:{}", plan.model_interface_id))
                .expect("exact provider life write")
                .content,
        )
        .expect("exact provider life parses");
        assert!(life.processes.iter().all(|process| {
            process
                .authority
                .read_env_paths
                .contains("OCTOPUS_MACHINE_CODEX_AUTH_FILE")
                && process
                    .authority
                    .write_env_paths
                    .contains("OCTOPUS_MACHINE_CODEX_AUTH_FILE")
        }));

        let workspace_door = &plan.capability_doors["system_workspace_env"];
        let workspace: InterfaceDescriptor = serde_json::from_str(
            &plan
                .interface_writes
                .iter()
                .find(|write| write.tag == format!("interface:{workspace_door}"))
                .expect("exact direct workspace door write")
                .content,
        )
        .expect("exact direct workspace door parses");
        let roots = &workspace.interface_driver_config.unwrap().host_slots["workspace_root"];
        assert_eq!(roots.path, folders[0]);
        assert_eq!(roots.additional_paths, folders[1..]);
        let private_policy: WorkerInterfacePolicy = serde_json::from_str(
            &plan
                .interface_writes
                .iter()
                .find(|write| write.tag == format!("policy:{}", plan.worker_interface_id))
                .expect("exact worker policy write")
                .content,
        )
        .expect("exact worker policy parses");
        assert_eq!(
            private_policy
                .ceiling
                .wek
                .iter()
                .map(|kind| kind.as_str())
                .collect::<Vec<_>>(),
            vec!["filesystem_workspace", "system_room"]
        );

        let record = RunRecord::from_bytes(plan.run_record_content.as_bytes())
            .expect("scheduled task run parses");
        assert_eq!(record.status, serde_json::json!("idle"));
        let provider_operation = record
            .checkpoints
            .iter()
            .find(|checkpoint| {
                checkpoint
                    .id
                    .as_str()
                    .starts_with("cp_operator_provider_operation_")
            })
            .expect("Codex provider operations receive one independent client input");
        let provider_operation_binding = provider_operation
            .interface_binding
            .as_ref()
            .expect("provider operation has an exact client binding");
        assert_eq!(
            provider_operation_binding.interface.as_str(),
            plan.chat_interface_id
        );
        assert!(provider_operation_binding.admits("provider.models.list"));
        assert!(provider_operation_binding.admits("provider.account.read"));
        assert!(!provider_operation_binding.admits("operator.message"));
        assert!(
            !provider_operation.artifact_bindings.is_empty(),
            "provider control checkpoints carry mandatory worker role dependencies without optional conversation context"
        );
        assert_eq!(
            provider_operation
                .outbound_interface_bindings
                .iter()
                .map(|binding| binding.interface.as_str())
                .collect::<Vec<_>>(),
            vec![plan.model_interface_id.as_str()]
        );
        let checkpoint = record
            .checkpoints
            .iter()
            .find(|checkpoint| checkpoint.id.as_str() == "cp_scheduled_probe")
            .expect("scheduled external checkpoint");
        let scheduled_binding = checkpoint.interface_binding.as_ref().unwrap();
        assert_eq!(scheduled_binding.interface.as_str(), "world_cron");
        assert!(scheduled_binding.admits("cron.current_time.scheduledprobe"));
        assert!(!scheduled_binding.admits("cron.current_time.otherfiring"));
        assert!(!scheduled_binding.admits("cron.current_time"));
        assert_eq!(
            checkpoint
                .outbound_interface_bindings
                .iter()
                .map(|binding| &binding.interface)
                .cloned()
                .collect::<Vec<_>>(),
            vec![InterfaceId::new(&plan.model_interface_id)]
        );
        let relay = record
            .checkpoints
            .iter()
            .find(|checkpoint| checkpoint.id.as_str() == "cp_project_result_probe")
            .expect("external relay checkpoint");
        assert_eq!(
            relay.interface_binding.as_ref().unwrap().interface.as_str(),
            "system_worker_planner"
        );
        assert_eq!(
            relay
                .outbound_interface_bindings
                .iter()
                .map(|binding| binding.interface.as_str())
                .collect::<Vec<_>>(),
            vec!["system_worker_coder", "system_worker_web"]
        );
        assert_eq!(relay.required_verified_artifacts.len(), 1);
        assert_eq!(
            relay.required_verified_artifacts[0].mark.as_str(),
            "task.summary.received"
        );
        assert_eq!(
            relay.required_verified_artifacts[0].artifact_label,
            "task.summary"
        );
        assert_eq!(
            relay.required_verified_artifacts[0].verifier.as_str(),
            "artifact_exists"
        );
        for checkpoint in &record.checkpoints {
            assert!(
                checkpoint
                    .artifact_bindings
                    .iter()
                    .any(|binding| binding.id.as_str() == "artifact-worker-role-web"),
                "checkpoint '{}' omits the role artifact of an exact external-relay worker",
                checkpoint.id.as_str()
            );
        }
        let client_sidecar: serde_json::Value = serde_json::from_str(
            &plan
                .interface_writes
                .iter()
                .find(|write| write.tag == format!("octopus-os:{}", plan.chat_interface_id))
                .expect("exact client sidecar write")
                .content,
        )
        .expect("exact client sidecar parses");
        assert_eq!(
            client_sidecar["authoring_profile"]["external_inputs"][0]["interface_id"],
            "world_cron"
        );
        assert_eq!(
            client_sidecar["authoring_profile"]["external_relays"][0]["outbound_interface_ids"][0],
            "system_worker_coder"
        );
        assert_eq!(
            client_sidecar["authoring_profile"]["external_relays"][0]["required_verified_artifacts"]
                [0]["artifact_label"],
            "task.summary"
        );
        assert!(
            plan.interface_writes
                .iter()
                .all(|write| !write.tag.starts_with("ee-policy:")),
            "a broad non-narrowing EE does not accumulate private task ids"
        );
    }
