use super::*;

const HOST_FILESYSTEM_ROOT_SLOT: &str = "host_filesystem_root";

pub(crate) fn plan_provider_interface(
    interface_root: &Path,
    config: &ClientConfig,
    identity: &PlannedIdentity,
    provider: &ResolvedProvider,
    topology: &FormedCapabilityTopology,
    worker_role_artifacts: &[String],
    inline_context_ids: &[String],
    interface_private_state_root: Option<&Path>,
    door_host_slots: &BTreeMap<String, BTreeMap<String, PathBuf>>,
    writes: &mut Vec<PlannedClientWrite>,
) -> Result<(), String> {
    let mut provider_interface = provider.interface.clone();
    let formed_policy = topology
        .interface_policies
        .get(&provider.interface.id)
        .ok_or_else(|| {
            format!(
                "provider interface '{}' has no exact formed interface policy",
                provider.interface.id.as_str()
            )
        })?;
    provider_interface.interface_policy.values = formed_policy.context.clone();
    let mut driver_config = provider
        .interface
        .interface_driver_config
        .clone()
        .unwrap_or_default();
    if driver_config.private_config.contains_key("instruction_artifacts") {
        driver_config.private_config.insert("worker_instruction_artifacts".into(), serde_json::json!(worker_role_artifacts));
    }
    collection_context::configure_inline_selection(&mut driver_config.private_config, inline_context_ids)?;
    if identity.surface_role == "home" {
        if let Some(instructions) = driver_config.private_config.get_mut("instruction_artifacts") {
            let instructions = instructions.as_array_mut().ok_or("instruction_artifacts must be an array")?;
            let delivery = home_outputs::context_id(identity);
            if !instructions.iter().any(|value| value.as_str() == Some(delivery.as_str())) {
                instructions.push(serde_json::json!(delivery));
            }
        }
    }
    if let Some(names) = driver_config.private_config.get_mut("model_tool_names") {
        let source_names: BTreeMap<String, String> = serde_json::from_value(names.clone())
            .map_err(|error| format!("model_tool_names is malformed: {error}"))?;
        let mut exact_names = BTreeMap::new();
        for (source, name) in &source_names {
            exact_names.insert(identity.capability_tools.get(source).unwrap_or(source).clone(), name.clone());
        }
        for (source, exact) in &identity.capability_tools {
            exact_names.entry(exact.clone()).or_insert_with(|| source.replace(['.', '-'], "_"));
        }
        *names = serde_json::json!(exact_names);
    }
    driver_config
        .private_config
        .insert("model".to_string(), serde_json::json!(provider.model));
    if !provider.reasoning_effort.is_empty() {
        driver_config.private_config.insert(
            "reasoning_effort".to_string(),
            serde_json::json!(provider.reasoning_effort),
        );
    } else {
        driver_config.private_config.remove("reasoning_effort");
    }
    if driver_config.private_config.contains_key("approval_policy") {
        let full_access = door_host_slots.values().any(|slots| slots.contains_key(HOST_FILESYSTEM_ROOT_SLOT));
        driver_config.private_config.insert(
            "approval_policy".into(),
            serde_json::json!(if full_access { "never" } else { "on-request" }),
        );
    }
    if let Some(tier) = &provider.service_tier {
        driver_config.private_config.insert("service_tier".into(), serde_json::json!(tier));
    } else {
        driver_config.private_config.remove("service_tier");
    }
    replace_provider_hidden_tools(
        &mut driver_config.private_config,
        provider.interface.id.as_str(),
        &identity.authoring_tool_id,
        &identity.capability_tools,
    )?;
    if let Some(value) = driver_config.private_config.get_mut("run_affordances") {
        let config = value.as_object_mut().ok_or("run_affordances must be an object")?;
        for key in ["file_write_tool", "file_read_tool", "artifact_send_tool"] {
            if let Some(value) = config.get_mut(key) {
                if value.is_null() { continue; }
                let original = value.as_str().filter(|id| !id.is_empty() && id.trim() == *id)
                    .ok_or_else(|| format!("run_affordances.{key} must name an exact tool"))?;
                if let Some(exact) = identity.capability_tools.get(original) {
                    *value = serde_json::json!(exact);
                }
            }
        }
    }
    for binding in topology.tool_bindings.values() {
        for resume in planned_nested_resumes(interface_root.parent().ok_or("provider interface root has no registry parent")?, binding)? {
            let Some(exact) = identity.capability_tools.get(&resume) else { continue; };
            let hidden = driver_config.private_config.get_mut("model_hidden_tools").and_then(serde_json::Value::as_array_mut)
                .ok_or("declared private continuation requires model_hidden_tools")?;
            if !hidden.iter().any(|entry| entry.as_str() == Some(exact.as_str())) { hidden.push(serde_json::json!(exact)); }
        }
    }
    if !identity.code_mode_owners.is_empty() {
        let hidden = driver_config
            .private_config
            .get_mut("model_hidden_tools")
            .and_then(serde_json::Value::as_array_mut)
            .ok_or("persistent owner requires declared model_hidden_tools")?;
        if !hidden
            .iter()
            .any(|v| v.as_str() == Some(identity.authoring_tool_id.as_str()))
        {
            return Err(
                "owner-private authoring capability must already be hidden by provider declaration"
                    .into(),
            );
        }
        for owner in identity.code_mode_owners.values() {
            for tool in std::iter::once(&owner.authoring_tool).chain(owner.nested_tools.iter()) {
                if !hidden.iter().any(|entry| entry.as_str() == Some(tool.as_str())) { hidden.push(serde_json::json!(tool)); }
            }
        }
    }
    driver_config.private_config.remove("run_authoring");
    driver_config.private_config.insert(
        "run_authoring_profile".to_string(),
        serde_json::json!({
            "file_write_tool": identity.authoring_tool_id,
            "file_write_label": config.artifact_labels.tool_call,
            "file_write_result_label": format!("{}.result", identity.authoring_tool_id),
            "result_timeout_ms": config.authoring.result_timeout_ms,
        }),
    );
    configure_provider_private_auth_store(&mut driver_config, provider.interface.id.as_str())?;
    let shares_provider_secret_store = configure_provider_secret_owner_root(
        &mut driver_config,
        provider.interface.id.as_str(),
        &identity.model_interface_id,
        interface_private_state_root,
    )?;
    let descriptor = sibling_descriptor_bytes(
        &provider_interface,
        &identity.model_interface_id,
        None,
        Some(&driver_config),
    )?;
    push_json_write(
        interface_root
            .join(&identity.model_interface_id)
            .join("interface.json"),
        descriptor,
        format!("interface:{}", identity.model_interface_id),
        writes,
    )?;
    let mut life_root_slots = Vec::new();
    if shares_provider_secret_store {
        life_root_slots.push(("provider_secret_owner_root", false));
    }
    if life_root_slots.is_empty() {
        plan_interface_life(
            interface_root,
            provider.interface.id.as_str(),
            &identity.model_interface_id,
            writes,
        )?;
    } else {
        plan_interface_life_with_root_slot_access(
            interface_root,
            provider.interface.id.as_str(),
            &identity.model_interface_id,
            &life_root_slots,
            writes,
        )?;
    }
    plan_octopus_os_sidecar(
        interface_root,
        &identity.model_interface_id,
        identity,
        "provider",
        false,
        None,
        false,
        writes,
    )
}

pub(crate) fn replace_provider_hidden_tools(
    private_config: &mut serde_json::Map<String, serde_json::Value>,
    provider_interface: &str,
    exact_authoring_tool: &str,
    planned_tools: &BTreeMap<String, String>,
) -> Result<(), String> {
    let base_authoring_tool = private_config
        .get("run_authoring_profile")
        .and_then(serde_json::Value::as_object)
        .and_then(|profile| profile.get("file_write_tool"))
        .and_then(serde_json::Value::as_str)
        .filter(|tool| !tool.is_empty() && tool.trim() == *tool)
        .ok_or_else(|| {
            format!(
                "provider interface '{provider_interface}' has no valid private_config.run_authoring_profile.file_write_tool"
            )
        })?
        .to_string();
    let hidden_tools = private_config
        .get_mut("model_hidden_tools")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or_else(|| {
            format!(
                "provider interface '{provider_interface}' private_config.model_hidden_tools must be an array"
            )
        })?;
    let mut source_tools = BTreeSet::new();
    let mut exact_tools = BTreeSet::new();
    let mut replaced = false;
    for (index, value) in hidden_tools.iter_mut().enumerate() {
        let current = value
            .as_str()
            .filter(|tool| !tool.is_empty() && tool.trim() == *tool)
            .ok_or_else(|| {
                format!(
                    "provider interface '{provider_interface}' private_config.model_hidden_tools[{index}] must be a non-empty trimmed string"
                )
            })?;
        if !source_tools.insert(current.to_string()) {
            return Err(format!(
                "provider interface '{provider_interface}' private_config.model_hidden_tools repeats '{current}'"
            ));
        }
        let next = if current == base_authoring_tool {
            replaced = true;
            exact_authoring_tool
        } else {
            planned_tools
                .get(current)
                .map(String::as_str)
                .unwrap_or(current)
        };
        if !exact_tools.insert(next.to_string()) {
            return Err(format!(
                "provider interface '{provider_interface}' private_config.model_hidden_tools collapses to duplicate '{next}'"
            ));
        }
        *value = serde_json::json!(next);
    }
    if !replaced {
        return Err(format!(
            "provider interface '{provider_interface}' does not hide its run-authoring tool '{base_authoring_tool}' from the model"
        ));
    }
    Ok(())
}

pub(crate) fn configure_provider_private_auth_store(
    driver_config: &mut octopus_interfaces::InterfaceDriverConfig,
    source_interface_id: &str,
) -> Result<(), String> {
    let Some(residual) = driver_config.residual_ambient.as_mut() else {
        return Ok(());
    };
    let residual = residual
        .as_object_mut()
        .ok_or_else(|| "provider residual_ambient must be a JSON object".to_string())?;
    let Some(store) = residual.get_mut("private_auth_store") else {
        return Ok(());
    };
    let store = store
        .as_object_mut()
        .ok_or_else(|| "private_auth_store must be a JSON object".to_string())?;
    if store.keys().any(|key| key != "auth_file_env") {
        return Err(format!(
            "selected provider '{source_interface_id}' private_auth_store must declare only auth_file_env"
        ));
    }
    let auth_file_env = store
        .get("auth_file_env")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "private_auth_store.auth_file_env must be a string".to_string())?;
    if auth_file_env != "OCTOPUS_MACHINE_CODEX_AUTH_FILE" {
        return Err(format!(
            "selected provider '{source_interface_id}' private_auth_store.auth_file_env must be 'OCTOPUS_MACHINE_CODEX_AUTH_FILE'"
        ));
    }
    Ok(())
}

pub(crate) fn configure_provider_secret_owner_root(
    driver_config: &mut octopus_interfaces::InterfaceDriverConfig,
    source_interface_id: &str,
    target_interface_id: &str,
    interface_private_state_root: Option<&Path>,
) -> Result<bool, String> {
    let Some(opencodex) = driver_config.private_config.get("opencodex") else {
        return Ok(false);
    };
    let opencodex = opencodex.as_object().ok_or_else(|| {
        format!(
            "provider interface '{source_interface_id}' private_config.opencodex must be an object"
        )
    })?;
    let credential_owner = opencodex
        .get("credential_owner_interface")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty() && value.trim() == *value)
        .ok_or_else(|| {
            format!(
                "provider interface '{source_interface_id}' private_config.opencodex has no exact credential owner"
            )
        })?;
    if credential_owner != source_interface_id {
        return Err(format!(
            "provider interface '{source_interface_id}' must own its OpenCodex credential boundary"
        ));
    }
    let transport = driver_config
        .transport
        .as_ref()
        .ok_or_else(|| format!("provider interface '{source_interface_id}' has no transport"))?;
    if transport.auth_secret_ref.is_none() {
        return Ok(false);
    }
    if target_interface_id == credential_owner {
        return Ok(false);
    }
    let private_root = interface_private_state_root.ok_or_else(|| {
        format!(
            "provider interface '{source_interface_id}' declares a private credential but interface_private_state_root is absent"
        )
    })?;
    require_absolute("interface_private_state_root", private_root)?;
    driver_config.host_slots.insert(
        "provider_secret_owner_root".to_string(),
        octopus_interfaces::HostSlotOverride {
            additional_paths: Vec::new(),
            path: private_root.join(credential_owner),
        },
    );
    Ok(true)
}
