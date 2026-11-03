use super::*;

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DesktopAppApprovalReferences {
    pub(crate) operator_interface: InterfaceId,
    pub(crate) resume_tool: ToolDescriptorId,
}

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BrowserUploadApprovalReferences {
    pub(crate) request_tools: Vec<ToolDescriptorId>,
    pub(crate) operator_interface: InterfaceId,
    pub(crate) resume_tool: ToolDescriptorId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) permission_policy: Option<serde_json::Map<String, serde_json::Value>>,
}

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BrowserConsentAuthorityReferences {
    pub(crate) authority: octopus_ledger::ArtifactRef,
    pub(crate) client_interface: InterfaceId,
}

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BrowserHistoryAccessReferences {
    pub(crate) request_tools: Vec<ToolDescriptorId>,
    pub(crate) operator_interface: InterfaceId,
    pub(crate) resume_tool: ToolDescriptorId,
    pub(crate) consent_authority: BrowserConsentAuthorityReferences,
}

pub(crate) fn planned_browser_history_access(
    binding: &crate::capability_formation::ToolCapabilityBinding,
    config: &ClientConfig,
    _identity: &PlannedIdentity,
) -> Result<Option<BrowserHistoryAccessReferences>, String> {
    let Some(room) = binding.interface.interface_driver_config.as_ref()
        .and_then(|driver| driver.private_config.get("browser_room")) else { return Ok(None); };
    let room = room.as_object().ok_or("browser_room must be its declared object")?;
    let Some(value) = room.get("history_access") else { return Ok(None); };
    let Some(mut history) = serde_json::from_value::<Option<BrowserHistoryAccessReferences>>(value.clone())
        .map_err(|e| format!("browser_room.history_access has an invalid exact reference: {e}"))? else { return Ok(None); };
    let exact = |value: &str| !value.is_empty() && value.trim() == value && !value.contains('\0');
    let mut declared = BTreeSet::new();
    if !exact(history.resume_tool.as_str()) || !exact(history.operator_interface.as_str())
        || !exact(history.consent_authority.authority.id.as_str()) || !exact(history.consent_authority.client_interface.as_str())
        || history.request_tools.is_empty() || history.request_tools.iter().any(|tool| !exact(tool.as_str())
            || tool == &history.resume_tool || !declared.insert(tool.clone())) {
        return Err("history access must declare distinct exact request/resume and consent references".into());
    }
    history.request_tools.retain(|tool| binding.tools.iter().any(|selected| selected.tool.as_str() == tool.as_str()));
    let resume_selected = binding.tools.iter().any(|tool| tool.tool.as_str() == history.resume_tool.as_str());
    if history.request_tools.is_empty() {
        if resume_selected { return Err("selected history resume has no selected request tool".into()); }
        return Ok(None);
    }
    if !resume_selected || history.consent_authority.client_interface.as_str() != config.interface {
        return Err("selected history request requires its declared companion and exact source task client".into());
    }
    Err("selected Home history consent uses an unsupported contract; initialize a fresh Home".into())
}

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BrowserWaitCompletionReferences {
    pub(crate) request_tools: Vec<ToolDescriptorId>,
    pub(crate) completion_tool: ToolDescriptorId,
}

pub(crate) fn planned_browser_wait_completion(
    registry_root: &Path,
    binding: &crate::capability_formation::ToolCapabilityBinding,
    config: &ClientConfig,
    identity: &PlannedIdentity,
) -> Result<Option<BrowserWaitCompletionReferences>, String> {
    let Some(room) = binding.interface.interface_driver_config.as_ref()
        .and_then(|driver| driver.private_config.get("browser_room")) else { return Ok(None); };
    let room = room.as_object().ok_or("browser_room must be its declared object")?;
    let Some(value) = room.get("wait_completion") else { return Ok(None); };
    let mut pair: BrowserWaitCompletionReferences = serde_json::from_value(value.clone())
        .map_err(|e| format!("browser_room.wait_completion is malformed: {e}"))?;
    let mut unique = BTreeSet::new();
    if pair.completion_tool.as_str().trim().is_empty() || pair.completion_tool.as_str().trim() != pair.completion_tool.as_str()
        || pair.request_tools.is_empty() || pair.request_tools.iter().any(|tool| tool.as_str().trim().is_empty()
            || tool.as_str().trim() != tool.as_str() || tool == &pair.completion_tool || !unique.insert(tool.clone())) {
        return Err("browser wait pair must declare distinct exact request and completion tools".into());
    }
    pair.request_tools.retain(|tool| binding.tools.iter().any(|selected| selected.tool.as_str() == tool.as_str()));
    let completion_selected = binding.tools.iter().any(|selected| selected.tool.as_str() == pair.completion_tool.as_str());
    if pair.request_tools.is_empty() {
        if completion_selected { return Err("selected browser completion has no selected request tool".into()); }
        return Ok(None);
    }
    if !completion_selected { return Err("selected browser wait requires its declared completion tool".into()); }
    let completion = planned_tool_descriptor(registry_root, config, identity, &binding.interface.id, pair.completion_tool.as_str())?;
    if completion.handler_config.get("operation").and_then(serde_json::Value::as_str) != Some("wait_complete") {
        return Err("declared browser completion descriptor has another operation".into());
    }
    for tool in &mut pair.request_tools {
        let request = planned_tool_descriptor(registry_root, config, identity, &binding.interface.id, tool.as_str())?;
        if request.handler_config.get("operation").and_then(serde_json::Value::as_str) != Some("wait")
            || request.process == completion.process || request.supported_worker_environment_kinds != completion.supported_worker_environment_kinds {
            return Err("browser wait pair requires independently declared processes in the same kind".into());
        }
        *tool = request.id;
    }
    pair.completion_tool = completion.id;
    Ok(Some(pair))
}

pub(crate) fn planned_browser_upload_approval(
    binding: &crate::capability_formation::ToolCapabilityBinding,
    config: &ClientConfig,
    identity: &PlannedIdentity,
) -> Result<Option<BrowserUploadApprovalReferences>, String> {
    let Some(room) = binding.interface.interface_driver_config.as_ref()
        .and_then(|driver| driver.private_config.get("browser_room")) else { return Ok(None); };
    let room = room.as_object().ok_or("browser_room must be its declared object")?;
    let Some(value) = room.get("upload_approval") else { return Ok(None); };
    let Some(mut approval) = serde_json::from_value::<Option<BrowserUploadApprovalReferences>>(value.clone())
        .map_err(|e| format!("browser_room.upload_approval has an invalid exact reference: {e}"))? else { return Ok(None); };
    let mut declared = BTreeSet::new();
    let exact = |value: &str| !value.is_empty() && value.trim() == value && !value.contains('\0');
    if !exact(approval.operator_interface.as_str()) || !exact(approval.resume_tool.as_str())
        || approval.request_tools.is_empty() || approval.request_tools.iter().any(|tool| !exact(tool.as_str()) || tool == &approval.resume_tool || !declared.insert(tool.clone())) {
        return Err("browser upload request_tools must declare distinct exact public request tools".into());
    }
    approval.request_tools.retain(|tool| binding.tools.iter().any(|selected| selected.tool.as_str() == tool.as_str()));
    if approval.request_tools.is_empty() {
        if binding.tools.iter().any(|selected| selected.tool.as_str() == approval.resume_tool.as_str()) {
            return Err("selected upload resume has no selected request tool".into());
        }
        return Ok(None);
    }
    if !binding.tools.iter().any(|tool| tool.tool.as_str() == approval.resume_tool.as_str()) {
        return Err("selected browser upload request requires its exact client and paired resume tool".into());
    }
    if approval.operator_interface.as_str() != config.interface {
        return Err("upload approval names another source client".into());
    }
    approval.operator_interface = InterfaceId::new(&identity.chat_interface_id);
    for tool in &mut approval.request_tools {
        *tool = ToolDescriptorId::new(identity.capability_tools.get(tool.as_str()).ok_or("browser upload request has no exact planned tool identity")?);
    }
    approval.resume_tool = ToolDescriptorId::new(identity.capability_tools.get(approval.resume_tool.as_str())
        .ok_or("browser upload resume has no exact planned tool identity")?);
    Ok(Some(approval))
}

pub(crate) fn plan_desktop_app_approval(
    content: &str,
    binding: &crate::capability_formation::ToolCapabilityBinding,
    config: &ClientConfig,
    identity: &PlannedIdentity,
) -> Result<String, String> {
    let mut desktop: serde_json::Map<String, serde_json::Value> = serde_json::from_str(content)
        .map_err(|error| {
            format!(
                "desktop.json for '{}' is malformed: {error}",
                binding.interface.id
            )
        })?;
    let Some(value) = desktop.get("app_approval") else {
        return Ok(content.to_string());
    };
    let mut references: DesktopAppApprovalReferences = serde_json::from_value(value.clone())
        .map_err(|error| format!("desktop.json.app_approval is malformed: {error}"))?;
    if references.operator_interface.as_str() != config.interface {
        return Err(format!(
            "desktop.json.app_approval.operator_interface '{}' is not the planned source client '{}'",
            references.operator_interface, config.interface
        ));
    }
    if !binding
        .tools
        .iter()
        .any(|tool| tool.tool.as_str() == references.resume_tool.as_str())
    {
        return Err(format!(
            "desktop.json.app_approval.resume_tool '{}' is not formed under exact door '{}'",
            references.resume_tool, binding.interface.id
        ));
    }
    let exact_tool = identity
        .capability_tools
        .get(references.resume_tool.as_str())
        .ok_or_else(|| {
            format!(
                "application resume tool '{}' has no planned exact identity",
                references.resume_tool
            )
        })?;
    references.operator_interface = InterfaceId::new(&identity.chat_interface_id);
    references.resume_tool = ToolDescriptorId::new(exact_tool);
    desktop.insert(
        "app_approval".to_string(),
        serde_json::to_value(references)
            .map_err(|error| format!("application approval references did not encode: {error}"))?,
    );
    pretty_json(&serde_json::Value::Object(desktop))
}

pub(crate) fn planned_nested_resumes(
    registry_root: &Path,
    binding: &crate::capability_formation::ToolCapabilityBinding,
) -> Result<BTreeSet<String>, String> {
    if binding.sidecars.iter().any(|sidecar| sidecar.file_name == "desktop.json") {
        let path = registry_root.join("interface").join(binding.interface.id.as_str()).join("desktop.json");
        let desktop: serde_json::Value = serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        return desktop.get("app_approval").map(|value| {
            serde_json::from_value::<DesktopAppApprovalReferences>(value.clone())
                .map(|approval| BTreeSet::from([approval.resume_tool.to_string()])).map_err(|e| e.to_string())
        }).transpose().map(Option::unwrap_or_default);
    }
    let Some(room) = binding.interface.interface_driver_config.as_ref()
        .and_then(|driver| driver.private_config.get("browser_room")) else { return Ok(BTreeSet::new()); };
    let room = room.as_object().ok_or("browser_room must be its declared object")?;
    let mut resumes = BTreeSet::new();
    if let Some(value) = room.get("upload_approval") {
        if let Some(approval) = serde_json::from_value::<Option<BrowserUploadApprovalReferences>>(value.clone()).map_err(|e| e.to_string())? {
            resumes.insert(approval.resume_tool.to_string());
        }
    }
    if let Some(value) = room.get("history_access") {
        if let Some(history) = serde_json::from_value::<Option<BrowserHistoryAccessReferences>>(value.clone()).map_err(|e| e.to_string())? {
            if !resumes.insert(history.resume_tool.to_string()) {
                return Err("distinct browser approval roles declare the same private resume".into());
            }
        }
    }
    Ok(resumes)
}

pub(crate) fn planned_nested_approval(
    registry_root: &Path,
    config: &ClientConfig,
    identity: &PlannedIdentity,
    binding: &crate::capability_formation::ToolCapabilityBinding,
    exact_tool: &str,
) -> Result<Option<serde_json::Value>, String> {
    let mut choices = Vec::new();
    if binding.sidecars.iter().any(|sidecar| sidecar.file_name == "desktop.json") {
        let path = registry_root.join("interface").join(binding.interface.id.as_str()).join("desktop.json");
        let desktop: serde_json::Value = serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        let Some(value) = desktop.get("app_approval") else { return Ok(None); };
        let approval: DesktopAppApprovalReferences = serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
        if approval.operator_interface.as_str() != config.interface {
            return Err("nested GUI approval is not bound to the selected exact client".into());
        }
        let resume = identity.capability_tools.get(approval.resume_tool.as_str()).ok_or("nested GUI resume is not selected")?.clone();
        choices.push((resume, InterfaceId::new(&identity.chat_interface_id)));
    } else {
        if let Some(approval) = planned_browser_upload_approval(binding, config, identity)? {
            if approval.request_tools.iter().any(|tool| tool.as_str() == exact_tool) {
                choices.push((approval.resume_tool.to_string(), approval.operator_interface));
            }
        }
        if let Some(history) = planned_browser_history_access(binding, config, identity)? {
            if history.request_tools.iter().any(|tool| tool.as_str() == exact_tool) {
                choices.push((history.resume_tool.to_string(), history.operator_interface));
            }
        }
    }
    if choices.len() > 1 { return Err("selected request matches more than one declared approval role".into()); }
    let Some((resume, operator)) = choices.pop() else { return Ok(None); };
    let base_resumes = planned_nested_resumes(registry_root, binding)?;
    let base = base_resumes.iter().find(|base| identity.capability_tools.get(*base) == Some(&resume))
        .ok_or("nested approval has no exact declared source resume identity")?;
    if !binding.tools.iter().any(|tool| tool.tool.as_str() == base) {
        return Err("nested approval resume belongs to another selected door".into());
    }
    let descriptor = planned_tool_descriptor(registry_root, config, identity, &binding.interface.id, base)?;
    let result = serde_json::json!({"operator_interface":operator,"resume_tool":resume,"resume_descriptor":descriptor});
    Ok(Some(result))
}

pub(crate) fn nested_process_owner(descriptor: &ToolDescriptor) -> Result<serde_json::Value, String> {
    let [kind] = descriptor.supported_worker_environment_kinds.as_slice() else {
        return Err("selected nested tool must have one declared process kind".into());
    };
    Ok(serde_json::json!({"interface":descriptor.interface,"kind":kind,"process":descriptor.process}))
}

pub(crate) fn partition_nested_returns(
    owner_routes: &mut [serde_json::Value],
    routes: &mut [serde_json::Value],
) -> Result<(), String> {
    let mut groups: BTreeMap<String, (serde_json::Value, BTreeSet<String>)> = BTreeMap::new();
    for route in owner_routes.iter_mut() {
        let descriptor: ToolDescriptor = serde_json::from_value(route["process_descriptor"].clone()).map_err(|e| e.to_string())?;
        let owner = nested_process_owner(&descriptor)?;
        let name = format!("nested_result_{}", octopus_descriptors::DescriptorHash::of_canonical_json(&owner).to_hex());
        let group = groups.entry(name.clone()).or_insert_with(|| (owner.clone(), BTreeSet::new()));
        if group.0 != owner { return Err("nested process owner digest collision".into()); }
        group.1.insert(format!("{}.result", descriptor.id));
        if let Some(approval) = route.get("app_approval").filter(|value| !value.is_null()) {
            let resume: ToolDescriptor = serde_json::from_value(approval["resume_descriptor"].clone()).map_err(|e| e.to_string())?;
            if nested_process_owner(&resume)? != owner || approval["resume_tool"].as_str() != Some(resume.id.as_str()) {
                return Err("nested approval resume does not belong to its original process owner".into());
            }
            group.1.insert(format!("{}.result", resume.id));
        }
        route["result"]["name"] = serde_json::json!(name);
        let configured = routes.iter_mut().find(|configured| configured["tool"] == route["tool"])
            .ok_or("nested process route has no exact configured tool")?;
        configured["result_checkpoint"] = route["result"]["name"].clone();
    }
    let mut disjoint = BTreeSet::new();
    for (owner, names) in groups.values() {
        let door = owner["interface"].as_str().ok_or("nested owner interface is absent")?;
        if names.is_empty() || names.iter().any(|name| !disjoint.insert((door, name))) {
            return Err("nested process result names overlap or are empty".into());
        }
    }
    for route in owner_routes {
        let name = route["result"]["name"].as_str().ok_or("nested result name is absent")?;
        let names = &groups.get(name).ok_or("nested return lost its owner group")?.1;
        let policy = serde_json::json!({"mode":"allow","names":names,"namespaces":[]});
        route["result"]["binding"]["inbound"]["config"]["artifact_policy"] = policy.clone();
        route["result"]["binding"]["outbound"][0]["config"]["artifact_policy"] = policy;
    }
    Ok(())
}
