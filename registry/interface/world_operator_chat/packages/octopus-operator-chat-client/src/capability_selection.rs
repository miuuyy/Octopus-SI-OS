use super::*;

pub(crate) fn exact_topology_interface_ids(
    identity: &PlannedIdentity,
    external_interfaces: &[String],
) -> Vec<String> {
    let mut ids = vec![
        identity.chat_interface_id.clone(),
        identity.model_interface_id.clone(),
        identity.worker_interface_id.clone(),
        identity.authoring_source_id.clone(),
        identity.authoring_environment_id.clone(),
    ];
    ids.extend(identity.capability_doors.values().cloned());
    ids.extend(
        identity
            .code_mode_owners
            .values()
            .map(|owner| owner.authoring_environment.clone()),
    );
    if let Some(voice) = &identity.voice { ids.extend(voice.interfaces().into_iter().cloned()); }
    ids.extend(home_outputs::interfaces(identity));
    ids.extend(external_interfaces.iter().cloned());
    ids.sort();
    ids.dedup();
    ids
}

pub(crate) fn ensure_external_interfaces_admitted_by_ee(
    topology: &FormedCapabilityTopology,
    interfaces: &[String],
) -> Result<(), String> {
    let missing = interfaces
        .iter()
        .filter(|interface| !topology.ee_admits_interface(&InterfaceId::new(interface.as_str())))
        .cloned()
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return Ok(());
    }
    Err(format!(
        "task connections exceed the formed EE interface allowance: {}",
        missing.join(", ")
    ))
}

pub(crate) fn select_capability_topology(
    mut topology: FormedCapabilityTopology,
    config: &ClientConfig,
    selected_worker_environment_kinds: &[String],
    selected_tools: &[SelectedToolSeed],
) -> Result<FormedCapabilityTopology, String> {
    let granted = topology
        .granted_worker_environment_kinds
        .iter()
        .map(|kind| kind.as_str())
        .collect::<BTreeSet<_>>();
    let mut selected = BTreeSet::new();
    for kind in selected_worker_environment_kinds {
        let kind = safe_segment(kind, "selected_worker_environment_kinds")?;
        if !selected.insert(kind.clone()) {
            return Err(format!(
                "selected_worker_environment_kinds repeats '{kind}'"
            ));
        }
        if !granted.contains(kind.as_str()) {
            return Err(format!(
                "worker environment kind '{kind}' is outside the formed worker/EE grant"
            ));
        }
    }

    let granted_tools = topology
        .granted_tools
        .iter()
        .map(|tool| tool.as_str())
        .collect::<BTreeSet<_>>();
    let mut selected_tool_ids = BTreeSet::new();
    let mut selected_tool_contracts = BTreeMap::new();
    for selection in selected_tools {
        let tool = safe_policy_axis_value(&selection.tool, "selected_tools.tool")?;
        let interface = safe_segment(&selection.interface, "selected_tools.interface")?;
        let kind = safe_segment(
            &selection.worker_environment_kind,
            "selected_tools.worker_environment_kind",
        )?;
        if !selected_tool_ids.insert(tool.clone()) {
            return Err(format!("selected_tools repeats '{tool}'"));
        }
        if !granted_tools.contains(tool.as_str()) {
            return Err(format!(
                "tool '{tool}' is outside the formed worker/EE grant"
            ));
        }
        selected_tool_contracts.insert(tool, (interface, kind));
    }

    let mut used_selected = BTreeSet::new();
    let mut used_selected_tools = BTreeSet::new();
    let mut retained_kinds = BTreeSet::new();
    topology.tool_bindings.retain(|source, binding| {
        binding.tools.retain(|tool| {
            let mandatory_authoring = source.as_str() == config.authoring.room_interface
                && tool.tool.as_str() == config.authoring.tool;
            let kind = &tool.worker_environment_kind;
            let exact_tool_selected =
                selected_tool_contracts
                    .get(tool.tool.as_str())
                    .is_some_and(|(interface, kind)| {
                        interface == source.as_str()
                            && kind == tool.worker_environment_kind.as_str()
                    });
            let retained =
                mandatory_authoring || exact_tool_selected || selected.contains(kind.as_str());
            if retained && selected.contains(kind.as_str()) {
                used_selected.insert(kind.as_str().to_string());
            }
            if exact_tool_selected {
                used_selected_tools.insert(tool.tool.as_str().to_string());
            }
            retained
        });
        let binding_kinds = binding
            .tools
            .iter()
            .map(|tool| &tool.worker_environment_kind)
            .map(|kind| kind.as_str().to_string())
            .collect::<BTreeSet<_>>();
        retained_kinds.extend(binding_kinds.iter().cloned());
        binding
            .worker_environment_kinds
            .retain(|kind| binding_kinds.contains(kind.as_str()));
        binding
            .sidecars
            .retain(|sidecar| binding_kinds.contains(sidecar.worker_environment_kind.as_str()));
        !binding.tools.is_empty()
    });
    let unused = selected
        .difference(&used_selected)
        .cloned()
        .collect::<Vec<_>>();
    if !unused.is_empty() {
        return Err(format!(
            "selected worker environment kinds expose no formed tools: {}",
            unused.join(", ")
        ));
    }
    let unused_tools = selected_tool_ids
        .difference(&used_selected_tools)
        .cloned()
        .collect::<Vec<_>>();
    if !unused_tools.is_empty() {
        return Err(format!(
            "selected tools expose no formed exact capability: {}",
            unused_tools.join(", ")
        ));
    }
    topology.granted_tools = topology
        .tool_bindings
        .values()
        .flat_map(|binding| binding.tools.iter().map(|tool| tool.tool.clone()))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    topology.granted_worker_environment_kinds = retained_kinds
        .into_iter()
        .map(octopus_descriptors::WorkerEnvironmentKindId::new)
        .collect();
    Ok(topology)
}

pub(crate) fn set_policy_axis(
    object: &mut serde_json::Map<String, serde_json::Value>,
    field: &str,
    values: impl IntoIterator<Item = String>,
) -> Result<(), String> {
    if !object.contains_key(field) {
        return Err(format!("private worker policy ceiling.{field} is absent"));
    }
    let mut exact = BTreeSet::new();
    for value in values {
        let value =
            safe_policy_axis_value(&value, &format!("private worker policy ceiling.{field}"))?;
        exact.insert(value);
    }
    object.insert(
        field.to_string(),
        serde_json::Value::Array(exact.into_iter().map(serde_json::Value::String).collect()),
    );
    Ok(())
}

pub(crate) fn validate_optional_policy_axis(
    values: &Option<Vec<String>>,
    field: &str,
    required: &[String],
    template_id: &str,
    admits: impl Fn(&BTreeSet<&str>, &str) -> bool,
) -> Result<(), String> {
    let Some(values) = values else {
        return Ok(());
    };
    let allowed = values.iter().map(String::as_str).collect::<BTreeSet<_>>();
    let missing = required
        .iter()
        .filter(|value| !admits(&allowed, value.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return Ok(());
    }
    Err(format!(
        "EE template '{template_id}' {field} does not admit exact task topology: {}",
        missing.join(", ")
    ))
}

pub(crate) fn validate_ee_capability_allowances(
    registry_root: &Path,
    chain: &[octopus_descriptors::EeTemplateId],
    exact_interfaces: &[String],
    exact_tools: &[String],
    lineage: &octopus_policy::CopyLineage,
) -> Result<(), String> {
    for template_id in chain {
        let path = registry_root
            .join("ee_template")
            .join(template_id.as_str())
            .join("ee_template.json");
        let bytes = std::fs::read(&path).map_err(|error| {
            format!(
                "EE template policy '{}' cannot be read: {error}",
                path.display()
            )
        })?;
        let descriptor = parse_strict::<EeTemplateDescriptor>(&bytes).map_err(|error| {
            format!(
                "EE template policy '{}' is malformed: {error}",
                path.display()
            )
        })?;
        if descriptor.id != *template_id {
            return Err(format!(
                "EE template policy '{}' declares a mismatched id",
                path.display()
            ));
        }
        validate_optional_policy_axis(
            &descriptor.allowed_interfaces,
            "allowed_interfaces",
            exact_interfaces,
            template_id.as_str(),
            |allowed, id| allowed.contains(id) || lineage.interface_original(&InterfaceId::new(id)).is_some_and(|original| allowed.contains(original.as_str())),
        )?;
        validate_optional_policy_axis(
            &descriptor.allowed_tools,
            "allowed_tools",
            exact_tools,
            template_id.as_str(),
            |allowed, id| allowed.contains(id) || lineage.tool_original(&octopus_descriptors::ToolId::new(id)).is_some_and(|original| allowed.contains(original.as_str())),
        )?;
    }
    Ok(())
}

pub(crate) fn capability_binding_tools(
    config: &ClientConfig,
    identity: &PlannedIdentity,
    source_interface_id: &InterfaceId,
    binding: &crate::capability_formation::ToolCapabilityBinding,
) -> Result<Vec<String>, String> {
    binding
        .tools
        .iter()
        .filter(|tool| {
            !(source_interface_id.as_str() == config.authoring.room_interface
                && tool.tool.as_str() == config.authoring.tool
                && identity.capability_tools.get(tool.tool.as_str()) == Some(&identity.authoring_tool_id))
        })
        .map(|tool| {
            identity
                .capability_tools
                .get(tool.tool.as_str())
                .cloned()
                .ok_or_else(|| {
                    format!(
                        "formed tool '{}' has no private exact identity",
                        tool.tool.as_str()
                    )
                })
        })
        .collect()
}
