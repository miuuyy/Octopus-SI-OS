use super::*;

pub(crate) fn plan_chat_interface(
    interface_root: &Path,
    config: &ClientConfig,
    identity: &PlannedIdentity,
    requested_authoring_profile: Option<&OctopusOsAuthoringProfile>,
    replace_authoring_profile: bool,
    writes: &mut Vec<PlannedClientWrite>,
) -> Result<(), String> {
    let source_interface = load_interface_descriptor(interface_root, &config.interface)?;
    let descriptor =
        sibling_descriptor_bytes(&source_interface, &identity.chat_interface_id, None, None)?;
    push_json_write(
        interface_root
            .join(&identity.chat_interface_id)
            .join("interface.json"),
        descriptor,
        format!("interface:{}", identity.chat_interface_id),
        writes,
    )?;
    plan_interface_life(
        interface_root,
        &config.interface,
        &identity.chat_interface_id,
        writes,
    )?;
    let activation = serde_json::json!({
        "schema_version": 1,
        "interface": identity.chat_interface_id,
        "activations": [
            {
                "activation_driver_ref": "app_server_transport",
                "config": {
                    "listen_env": config.run.listen_env,
                    "bind_interface_socket_when_env_unset": true,
                },
            }
        ],
    });
    push_json_write(
        interface_root
            .join(&identity.chat_interface_id)
            .join("activation.json"),
        pretty_json(&activation)?,
        format!("activation:{}", identity.chat_interface_id),
        writes,
    )?;
    plan_octopus_os_sidecar(
        interface_root,
        &identity.chat_interface_id,
        identity,
        "client",
        true,
        requested_authoring_profile,
        replace_authoring_profile,
        writes,
    )
}

pub(crate) fn plan_capability_doors(
    registry_root: &Path,
    interface_root: &Path,
    config: &ClientConfig,
    topology: &FormedCapabilityTopology,
    identity: &PlannedIdentity,
    requested_host_slots: &BTreeMap<String, BTreeMap<String, PathBuf>>,
    source_folders: &[PathBuf],
    writes: &mut Vec<PlannedClientWrite>,
) -> Result<(), String> {
    let mut consumed_host_slots = BTreeSet::new();
    for (source_interface_id, binding) in &topology.tool_bindings {
        let Some(door_id) = identity.capability_doors.get(source_interface_id.as_str()) else {
            if binding.tools.iter().all(|tool| {
                identity
                    .capability_tools
                    .get(tool.tool.as_str())
                    .is_some_and(|private| private == &identity.authoring_tool_id)
            }) {
                continue;
            }
            return Err(format!(
                "capability interface '{}' has no planned exact door",
                source_interface_id
            ));
        };
        let mut driver_config = if identity.native_communication.is_some() {
            binding.interface.interface_driver_config.clone()
        } else {
            preserve_exact_door_host_slots(interface_root, &binding.interface, door_id)?
        };
        if let Some(overrides) = requested_host_slots.get(source_interface_id.as_str()) {
            consumed_host_slots.insert(source_interface_id.as_str().to_string());
            let config = driver_config.get_or_insert_with(Default::default);
            for (slot, path) in overrides {
                let slot = safe_segment(slot, "door_host_slots slot")?;
                require_absolute("door_host_slots path", path)?;
                let additional_paths = config.host_slots.get(&slot).filter(|binding| binding.path == *path)
                    .map(|binding| binding.additional_paths.clone()).unwrap_or_default();
                config.host_slots.insert(slot, octopus_interfaces::HostSlotOverride { additional_paths, path: path.clone() });
            }
        }
        if let Some(primary) = source_folders.first() {
            let mut uses_workspace = false;
            for kind in &binding.worker_environment_kinds {
                let file = registry_root.join("worker_environment_kind").join(kind.as_str()).join("worker_environment_kind.json");
                let descriptor: serde_json::Value = serde_json::from_slice(&std::fs::read(&file).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
                uses_workspace |= ["read_root_refs", "write_root_refs"].iter().any(|field| descriptor["boundary_defaults"][field]
                    .as_array().is_some_and(|roots| roots.iter().any(|root| root["path"].as_str() == Some("workspace_root"))));
            }
            if uses_workspace && !interface_root.join(door_id).join("interface.json").exists() {
                driver_config.get_or_insert_default().host_slots.insert("workspace_root".into(), octopus_interfaces::HostSlotOverride {
                    path: primary.clone(), additional_paths: source_folders[1..].to_vec(),
                });
            }
        }
        if let Some(owner) = identity.code_mode_owners.get(source_interface_id.as_str()) {
            driver_config.get_or_insert_default().private_config.insert("run_authoring_profile".into(), serde_json::json!({
                "file_write_tool":owner.authoring_tool,"file_write_label":config.artifact_labels.tool_call,
                "file_write_result_label":format!("{}.result",owner.authoring_tool),"result_timeout_ms":owner.recipe.result_timeout_ms
            }));
        }
        if let Some(approval) = planned_browser_upload_approval(binding, config, identity)? {
            let room = driver_config.get_or_insert_default().private_config.get_mut("browser_room")
                .and_then(serde_json::Value::as_object_mut).ok_or("planned browser upload approval lost its declared room configuration")?;
            room.insert("upload_approval".into(), serde_json::to_value(approval).map_err(|e| e.to_string())?);
        } else if let Some(room) = driver_config.as_mut().and_then(|driver| driver.private_config.get_mut("browser_room")).and_then(serde_json::Value::as_object_mut) {
            room.remove("upload_approval");
        }
        if let Some(history) = planned_browser_history_access(binding, config, identity)? {
            let room = driver_config.get_or_insert_default().private_config.get_mut("browser_room")
                .and_then(serde_json::Value::as_object_mut).ok_or("planned history lost its declared room configuration")?;
            room.insert("history_access".into(), serde_json::to_value(history).map_err(|e| e.to_string())?);
        } else if let Some(room) = driver_config.as_mut().and_then(|driver| driver.private_config.get_mut("browser_room")).and_then(serde_json::Value::as_object_mut) {
            room.remove("history_access");
        }
        if let Some(completion) = planned_browser_wait_completion(registry_root, binding, config, identity)? {
            let room = driver_config.get_or_insert_default().private_config.get_mut("browser_room")
                .and_then(serde_json::Value::as_object_mut).ok_or("planned browser wait lost its declared room configuration")?;
            room.insert("wait_completion".into(), serde_json::to_value(completion).map_err(|e| e.to_string())?);
        } else if let Some(room) = driver_config.as_mut().and_then(|driver| driver.private_config.get_mut("browser_room")).and_then(serde_json::Value::as_object_mut) {
            room.remove("wait_completion");
        }
        let descriptor =
            sibling_descriptor_bytes(&binding.interface, door_id, None, driver_config.as_ref())?;
        push_json_write(
            interface_root.join(door_id).join("interface.json"),
            descriptor,
            format!("interface:{door_id}"),
            writes,
        )?;
        plan_interface_life(
            interface_root,
            source_interface_id.as_str(),
            door_id,
            writes,
        )?;
        plan_octopus_os_sidecar(
            interface_root,
            door_id,
            identity,
            "worker_environment_door",
            false,
            None,
            false,
            writes,
        )?;
        plan_capability_sidecars(
            interface_root,
            source_interface_id.as_str(),
            door_id,
            binding,
            config,
            identity,
            writes,
        )?;
    }
    let unused = requested_host_slots
        .keys()
        .filter(|interface| !consumed_host_slots.contains(*interface))
        .cloned()
        .collect::<Vec<_>>();
    if !unused.is_empty() {
        return Err(format!(
            "door_host_slots names interfaces absent from the formed exact door topology: {}",
            unused.join(", ")
        ));
    }
    Ok(())
}

pub(crate) fn plan_authoring_interfaces(
    interface_root: &Path,
    config: &ClientConfig,
    topology: &FormedCapabilityTopology,
    identity: &PlannedIdentity,
    writes: &mut Vec<PlannedClientWrite>,
) -> Result<(), String> {
    let source_interface =
        load_interface_descriptor(interface_root, &config.authoring.source_interface)?;
    let descriptor =
        sibling_descriptor_bytes(&source_interface, &identity.authoring_source_id, None, None)?;
    push_json_write(
        interface_root
            .join(&identity.authoring_source_id)
            .join("interface.json"),
        descriptor,
        format!("interface:{}", identity.authoring_source_id),
        writes,
    )?;
    plan_interface_life(
        interface_root,
        &config.authoring.source_interface,
        &identity.authoring_source_id,
        writes,
    )?;
    plan_octopus_os_sidecar(
        interface_root,
        &identity.authoring_source_id,
        identity,
        "authoring_source",
        false,
        None,
        false,
        writes,
    )?;
    let activation = serde_json::json!({
        "schema_version": 1,
        "interface": identity.authoring_source_id,
        "activations": [
            {
                "activation_driver_ref": "app_server_transport",
                "config": {
                    "listen_env": format!(
                        "OCTOPUS_INTERFACE_AUTHORING_{}_LISTEN",
                        identity.chat_id.to_uppercase()
                    ),
                    "bind_interface_socket_when_env_unset": true,
                },
            }
        ],
    });
    push_json_write(
        interface_root
            .join(&identity.authoring_source_id)
            .join("activation.json"),
        pretty_json(&activation)?,
        format!("activation:{}", identity.authoring_source_id),
        writes,
    )?;

    let room_interface =
        load_interface_descriptor(interface_root, &config.authoring.room_interface)?;
    let descriptor = sibling_descriptor_bytes(
        &room_interface,
        &identity.authoring_environment_id,
        None,
        None,
    )?;
    push_json_write(
        interface_root
            .join(&identity.authoring_environment_id)
            .join("interface.json"),
        descriptor,
        format!("interface:{}", identity.authoring_environment_id),
        writes,
    )?;
    plan_interface_life(
        interface_root,
        &config.authoring.room_interface,
        &identity.authoring_environment_id,
        writes,
    )?;
    let binding = topology
        .tool_bindings
        .get(&InterfaceId::new(&config.authoring.room_interface))
        .ok_or_else(|| {
            format!(
                "authoring room interface '{}' is absent from formed capabilities",
                config.authoring.room_interface
            )
        })?;
    plan_capability_sidecars(
        interface_root,
        &config.authoring.room_interface,
        &identity.authoring_environment_id,
        binding,
        config,
        identity,
        writes,
    )?;
    plan_octopus_os_sidecar(
        interface_root,
        &identity.authoring_environment_id,
        identity,
        "authoring_environment",
        false,
        None,
        false,
        writes,
    )
}

pub(crate) fn plan_capability_sidecars(
    interface_root: &Path,
    source_interface_id: &str,
    target_interface_id: &str,
    binding: &crate::capability_formation::ToolCapabilityBinding,
    config: &ClientConfig,
    identity: &PlannedIdentity,
    writes: &mut Vec<PlannedClientWrite>,
) -> Result<(), String> {
    for sidecar in &binding.sidecars {
        let source = interface_root
            .join(source_interface_id)
            .join(&sidecar.file_name);
        let content = std::fs::read_to_string(&source).map_err(|error| {
            format!(
                "capability interface '{}' is missing required sidecar '{}': {error}",
                source_interface_id, sidecar.file_name
            )
        })?;
        let content = if sidecar.file_name == "desktop.json" {
            plan_desktop_app_approval(&content, binding, config, identity)?
        } else if sidecar.file_name == "code_mode.json" {
            plan_code_mode_references(&content, source_interface_id, config, identity)?
        } else {
            content
        };
        let content = if content.ends_with('\n') {
            content
        } else {
            format!("{content}\n")
        };
        push_text_write(
            interface_root
                .join(target_interface_id)
                .join(&sidecar.file_name),
            content,
            format!("sidecar:{target_interface_id}:{}", sidecar.file_name),
            writes,
        )?;
    }
    Ok(())
}

pub(crate) fn project_source_folders(ee_instances_root: &Path, ee_id: &str) -> Result<Vec<PathBuf>, String> {
    let file = ee_instances_root.join(format!("{}.json", safe_segment(ee_id, "Project EE")?));
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(file).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let Some(folders) = value.pointer("/config/octopus_os/source_folders") else { return Ok(Vec::new()); };
    let folders: Vec<PathBuf> = serde_json::from_value(folders.clone()).map_err(|e| format!("invalid Project source folders: {e}"))?;
    let mut seen = BTreeSet::new();
    for folder in &folders {
        require_absolute("Project source folder", folder)?;
        if !folder.is_dir() || !seen.insert(folder.clone()) { return Err("Project source folders must be existing, distinct directories".into()); }
    }
    Ok(folders)
}
