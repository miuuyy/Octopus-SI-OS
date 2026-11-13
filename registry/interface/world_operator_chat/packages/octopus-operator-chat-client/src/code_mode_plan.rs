use super::*;

#[derive(Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CodeModeTaskRecipe {
    pub(crate) prelude_artifacts: Vec<String>,
    pub(crate) resume_tool: String,
    pub(crate) wait_tool: String,
    pub(crate) authoring_room: String,
    pub(crate) authoring_tool: String,
    pub(crate) notification_interface: String,
    #[serde(default)]
    pub(crate) tool_descriptions: BTreeMap<String, String>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CodeModePreludeSource {
    pub(crate) id: String,
    pub(crate) schema_version: u32,
    pub(crate) module_source: String,
    pub(crate) export_factory: String,
    pub(crate) export_name: String,
    #[serde(deserialize_with = "unique_prelude_tool_roles")]
    pub(crate) tool_roles: BTreeMap<String, CodeModePreludeTool>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CodeModePreludeTool {
    pub(crate) tool: String,
    pub(crate) interface: String,
}

pub(crate) fn unique_prelude_tool_roles<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<String, CodeModePreludeTool>, D::Error> {
    struct Roles;
    impl<'de> serde::de::Visitor<'de> for Roles {
        type Value = BTreeMap<String, CodeModePreludeTool>;
        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("distinct declared prelude tool roles")
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(self, mut access: A) -> Result<Self::Value, A::Error> {
            let mut roles = BTreeMap::new();
            while let Some((role, tool)) = access.next_entry::<String, CodeModePreludeTool>()? {
                if roles.insert(role, tool).is_some() {
                    return Err(serde::de::Error::custom("prelude source repeats a tool role"));
                }
            }
            Ok(roles)
        }
    }
    deserializer.deserialize_map(Roles)
}

pub(crate) struct PlannedCodeModeSource {
    pub(crate) source: octopus_ledger::ArtifactRef,
    pub(crate) snapshot: octopus_ledger::ArtifactRef,
    pub(crate) source_sha256: String,
    pub(crate) content: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RetainedPreludeSource {
    pub(crate) source: octopus_ledger::ArtifactRef,
    pub(crate) snapshot: octopus_ledger::ArtifactRef,
    pub(crate) source_sha256: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RetainedPreludeSnapshot {
    pub(crate) id: String,
    pub(crate) run: String,
    pub(crate) source: octopus_ledger::ArtifactRef,
    pub(crate) source_sha256: String,
    pub(crate) source_content: String,
}

pub(crate) fn code_mode_source_bytes(
    registry: &Path, identity: &PlannedIdentity, owner: &str, source_id: &str,
    existing: Option<&serde_json::Value>,
) -> Result<Vec<u8>, String> {
    let state = match existing {
        Some(run) => run.get("interface_state").and_then(serde_json::Value::as_object)
            .ok_or("retained run has no interface state")?.get(owner),
        None => None,
    };
    let Some(state) = state else {
        return octopus_interfaces::read_artifact_carrier_file_bounded(
            &registry.join("artifact").join(source_id).join("artifact.json"), 1024 * 1024,
        ).map_err(|error| error.to_string());
    };
    let sources: Vec<RetainedPreludeSource> = serde_json::from_value(
        state.get("prelude_sources").cloned().ok_or("retained Code Mode owner has no source evidence")?,
    ).map_err(|error| error.to_string())?;
    let matches = sources.iter().filter(|source| source.source.id.as_str() == source_id).collect::<Vec<_>>();
    let [source] = matches.as_slice() else { return Err("retained Code Mode source is absent or duplicated".into()); };
    let snapshot_id = format!("{source_id}__source__{}", identity.chat_id);
    if source.snapshot.id.as_str() != snapshot_id { return Err("retained Code Mode snapshot has another owner".into()); }
    let bytes = octopus_interfaces::read_artifact_carrier_file_bounded(
        &registry.join("artifact").join(&snapshot_id).join("artifact.json"), 8 * 1024 * 1024,
    ).map_err(|error| error.to_string())?;
    let body: serde_json::Value = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    let snapshot: RetainedPreludeSnapshot = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    let content = snapshot.source_content.into_bytes();
    let source_body: serde_json::Value = serde_json::from_slice(&content).map_err(|error| error.to_string())?;
    if snapshot.id != snapshot_id || snapshot.run != identity.run_id || snapshot.source != source.source
        || source.snapshot.content != octopus_ledger::ContentHash::of_bytes(&octopus_descriptors::canonical_bytes(&body))
        || source.source.content != octopus_ledger::ContentHash::of_bytes(&octopus_descriptors::canonical_bytes(&source_body))
        || snapshot.source_sha256 != source.source_sha256 || source.source_sha256 != format!("{:x}", Sha256::digest(&content))
        || content.len() > 1024 * 1024
    { return Err("retained Code Mode source differs from its exact snapshot evidence".into()); }
    Ok(content)
}

pub(crate) struct PlannedCodeModeOwner {
    pub(crate) recipe: CodeModeTaskRecipe,
    pub(crate) prelude_sources: Vec<PlannedCodeModeSource>,
    pub(crate) authoring_environment: String,
    pub(crate) authoring_tool: String,
    pub(crate) prelude: octopus_ledger::ArtifactRef,
    pub(crate) prelude_body: serde_json::Value,
    pub(crate) exports: Vec<String>,
    pub(crate) topology: octopus_ledger::ArtifactRef,
    pub(crate) topology_body: serde_json::Value,
    pub(crate) sidecar: serde_json::Value,
    pub(crate) nested_doors: BTreeSet<String>,
    pub(crate) nested_tools: BTreeSet<String>,
}

pub(crate) fn plan_code_mode_owners(
    registry_root: &Path,
    config: &ClientConfig,
    capabilities: &FormedCapabilityTopology,
    identity: &mut PlannedIdentity,
    context: &[ArtifactBindingSeed],
    existing: Option<&serde_json::Value>,
) -> Result<(), String> {
    for (source, binding) in &capabilities.tool_bindings {
        let Some(recipe) = binding
            .interface
            .interface_driver_config
            .as_ref()
            .and_then(|c| c.private_config.get("operator_chat_persistent"))
        else {
            continue;
        };
        let recipe: CodeModeTaskRecipe = serde_json::from_value(recipe.clone())
            .map_err(|e| format!("invalid declared code-mode task recipe: {e}"))?;
        let mut declared_sources = BTreeSet::new();
        let mut selected_sources = Vec::new();
        if recipe.prelude_artifacts.is_empty() {
            return Err("persistent task recipe must declare at least one prelude artifact".into());
        }
        for id in &recipe.prelude_artifacts {
            let id = safe_segment(id, "prelude artifact")?;
            if !declared_sources.insert(id.clone()) {
                return Err("persistent task recipe repeats a prelude artifact".into());
            }
            match context.iter().filter(|binding| binding.id == id).count() {
                0 => {},
                1 => selected_sources.push(id),
                _ => return Err("persistent prelude requires exactly one explicit source binding".into()),
            }
        }
        if selected_sources.is_empty() {
            continue;
        }
        if recipe.authoring_room != config.authoring.room_interface
            || recipe.authoring_tool != config.authoring.tool
            || recipe.notification_interface != config.interface
        {
            return Err("persistent task recipe must use its exact selected client and existing authoring capability".into());
        }
        let owner = identity
            .capability_doors
            .get(source.as_str())
            .ok_or("persistent task recipe has no formed owner door")?;
        let exact_role = |base: &str| -> Result<String, String> {
            if !binding.tools.iter().any(|t| t.tool.as_str() == base) {
                return Err(format!(
                    "persistent owner role '{base}' was not selected under '{source}'"
                ));
            }
            identity
                .capability_tools
                .get(base)
                .cloned()
                .ok_or_else(|| format!("missing exact selected role '{base}'"))
        };
        let resume = exact_role(&recipe.resume_tool)?;
        let wait = exact_role(&recipe.wait_tool)?;
        for (tool, description) in &recipe.tool_descriptions {
            exact_role(tool)?;
            if description.trim().is_empty() {
                return Err("selected persistent tool description must be non-empty".into());
            }
        }
        if resume == wait {
            return Err("persistent resume and wait roles must be distinct".into());
        }
        let sidecar_path = registry_root
            .join("interface")
            .join(source.as_str())
            .join("code_mode.json");
        let mut sidecar: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&sidecar_path).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        if sidecar
            .pointer("/context/mode")
            .and_then(serde_json::Value::as_str)
            != Some("isolated")
            || sidecar.get("continuation").is_some()
            || sidecar.get("handoff").is_some()
            || sidecar
                .get("routes")
                .and_then(serde_json::Value::as_array)
                .is_none_or(|r| !r.is_empty())
        {
            return Err("task prelude recipe requires an explicit isolated source without concrete task refs".into());
        }
        let max_source_bytes = sidecar.get("max_source_bytes").and_then(serde_json::Value::as_u64)
            .ok_or("selected persistent source requires its declared source byte bound")?;
        let javascript_identifier = |v: &str| {
            !v.is_empty()
                && v.bytes().enumerate().all(|(i, c)| {
                    c.is_ascii_alphabetic()
                        || c == b'_'
                        || c == b'$'
                        || (i > 0 && c.is_ascii_digit())
                })
        };
        let mut prelude_sources = Vec::new();
        let mut modules = String::new();
        let mut source_bytes = 0u64;
        let mut exports = Vec::new();
        let mut declarations = BTreeSet::new();
        let mut role_names = BTreeSet::new();
        let mut nested_tools = BTreeSet::new();
        let mut javascript_names = BTreeSet::new();
        let mut routes = Vec::new();
        let mut owner_routes = Vec::new();
        let edge = |name: String, from: &str, to: &str| {
            serde_json::json!({"name":name,"binding":{
                "inbound":AuthoredInterfaceBinding::all(InterfaceId::new(from)),
                "outbound":[AuthoredInterfaceBinding::all(InterfaceId::new(to))]
            }})
        };
        for prelude_source_id in &selected_sources {
            let bytes = code_mode_source_bytes(registry_root, identity, owner, prelude_source_id, existing)?;
            source_bytes = source_bytes.checked_add(bytes.len() as u64)
                .filter(|total| *total <= max_source_bytes)
                .ok_or("selected prelude sources exceed the room source byte bound")?;
            let body: serde_json::Value = serde_json::from_slice(&bytes)
                .map_err(|e| format!("invalid declared prelude artifact: {e}"))?;
            let prelude: CodeModePreludeSource = serde_json::from_slice(&bytes)
                .map_err(|e| format!("invalid declared prelude artifact: {e}"))?;
            if prelude.id != *prelude_source_id || prelude.schema_version != 1
                || !javascript_identifier(&prelude.export_factory)
                || !javascript_identifier(&prelude.export_name)
                || prelude.module_source.trim().is_empty()
                || prelude.tool_roles.is_empty()
                || !declarations.insert(prelude.export_factory.clone())
                || !declarations.insert(prelude.export_name.clone())
            {
                return Err("selected preludes require distinct nonempty module/factory/export declarations".into());
            }
            let mut roles = serde_json::Map::new();
            for (role, declared) in &prelude.tool_roles {
                if role.trim().is_empty() || !role_names.insert(role.clone()) {
                    return Err("selected preludes repeat a tool role or declare an empty role".into());
                }
                let Some(nested) = capabilities
                    .tool_bindings
                    .get(&InterfaceId::new(&declared.interface))
                else {
                    continue;
                };
                if !nested
                    .tools
                    .iter()
                    .any(|t| t.tool.as_str() == declared.tool)
                {
                    continue;
                }
                let tool = identity
                    .capability_tools
                    .get(&declared.tool)
                    .ok_or("prelude tool has no exact planned identity")?;
                let door = identity
                    .capability_doors
                    .get(&declared.interface)
                    .ok_or("prelude tool has no exact planned door")?;
                if door == owner {
                    return Err("prelude cannot recursively route into its owner".into());
                }
                let normalized: String = tool
                    .chars()
                    .enumerate()
                    .map(|(i, c)| {
                        if c == '_'
                            || c == '$'
                            || c.is_ascii_alphabetic()
                            || (i > 0 && c.is_ascii_digit())
                        {
                            c
                        } else {
                            '_'
                        }
                    })
                    .collect();
                if normalized.is_empty()
                    || !nested_tools.insert(tool.clone())
                    || !javascript_names.insert(normalized.clone())
                {
                    return Err("persistent prelude repeats a tool or produces colliding exact JavaScript names".into());
                }
                roles.insert(
                    role.clone(),
                    serde_json::json!({"tool":tool,"name":normalized}),
                );
                let call_name = format!("nested_call_{tool}");
                let result_name = format!("nested_result_{door}");
                routes.push(serde_json::json!({"tool":tool,"interface":door,"checkpoint":call_name,"result_checkpoint":result_name}));
                let app_approval = planned_nested_approval(registry_root, config, identity, nested, tool)?;
                owner_routes.push(serde_json::json!({"tool":tool,"call":edge(call_name,owner,door),"result":edge(result_name,door,owner),"app_approval":app_approval,"process_descriptor":planned_tool_descriptor(registry_root,config,identity,&InterfaceId::new(&declared.interface),&declared.tool)?}));
            }
            if roles.is_empty() {
                return Err("selected prelude has no formed declared tool roles".into());
            }
            modules.push_str(&prelude.module_source);
            modules.push_str(&format!("\nexport const {} = {}({});\n", prelude.export_name, prelude.export_factory, serde_json::Value::Object(roles)));
            exports.push(prelude.export_name);
            let content = octopus_ledger::ContentHash::of_bytes(&octopus_descriptors::canonical_bytes(&body));
            let source = octopus_ledger::ArtifactRef { id:octopus_descriptors::ArtifactId::new(prelude_source_id.clone()), content };
            let snapshot_id = format!("{prelude_source_id}__source__{}",identity.chat_id);
            let source_sha256 = {
                    use sha2::{Digest, Sha256};
                    format!("{:x}", Sha256::digest(&bytes))
            };
            let snapshot = serde_json::json!({"id":snapshot_id,"run":identity.run_id,"source":source,"source_sha256":source_sha256,
                "source_content":String::from_utf8(bytes).map_err(|_| "prelude source is not UTF8")?});
            prelude_sources.push(PlannedCodeModeSource {
                source,
                snapshot: octopus_ledger::ArtifactRef { id:octopus_descriptors::ArtifactId::new(snapshot_id), content:octopus_ledger::ContentHash::of_bytes(&octopus_descriptors::canonical_bytes(&snapshot)) },
                source_sha256,
                content: pretty_json(&snapshot)?,
            });
        }
        let nested_doors = owner_routes.iter().filter_map(|route| route.pointer("/call/binding/outbound/0/interface").and_then(serde_json::Value::as_str)).map(str::to_owned).collect::<BTreeSet<_>>();
        for (nested_source, nested) in &capabilities.tool_bindings {
            let Some(door) = identity.capability_doors.get(nested_source.as_str()).filter(|door| nested_doors.contains(*door)) else { continue; };
            let internal_resumes = planned_nested_resumes(registry_root, nested)?;
            for selected in &nested.tools {
                let tool = identity.capability_tools.get(selected.tool.as_str()).ok_or("selected nested tool has no exact planned identity")?;
                if internal_resumes.iter().any(|base| identity.capability_tools.get(base) == Some(tool)) || !nested_tools.insert(tool.clone()) { continue; }
                let approval = planned_nested_approval(registry_root, config, identity, nested, tool)?;
                let call_name = format!("nested_call_{tool}");
                let result_name = format!("nested_result_{door}");
                routes.push(serde_json::json!({"tool":tool,"interface":door,"checkpoint":call_name,"result_checkpoint":result_name}));
                owner_routes.push(serde_json::json!({"tool":tool,"call":edge(call_name,owner,door),"result":edge(result_name,door,owner),"app_approval":approval,"process_descriptor":planned_tool_descriptor(registry_root,config,identity,nested_source,selected.tool.as_str())?}));
            }
        }
        partition_nested_returns(&mut owner_routes, &mut routes)?;
        for previous in identity.code_mode_owners.values() {
            if !previous.nested_doors.is_disjoint(&nested_doors) { return Err("selected persistent owners cannot share one nested door".into()); }
        }
        let prelude_id = format!("artifact-code-mode-prelude__{}__{}", source, identity.chat_id);
        let prelude_body = serde_json::json!({"id":prelude_id,"run":identity.run_id,"owner_interface":owner,"schema_version":1,"module_source":modules,"exports":exports});
        let artifact = |id: String, body: &serde_json::Value| octopus_ledger::ArtifactRef {
            id: octopus_descriptors::ArtifactId::new(id),
            content: octopus_ledger::ContentHash::of_bytes(&octopus_descriptors::canonical_bytes(
                body,
            )),
        };
        let prelude_ref = artifact(
            prelude_id,
            &prelude_body,
        );
        let authoring_environment = format!("{}_authoring_{}", recipe.authoring_room, owner);
        let authoring_tool = format!("{}__authoring_{}", recipe.authoring_tool, owner);
        if authoring_environment == identity.authoring_environment_id
            || authoring_tool == identity.authoring_tool_id
        {
            return Err(
                "persistent owner cannot share the provider's live authoring identity".into(),
            );
        }
        let topology_id = format!("artifact-code-mode-topology__{}__{}", source, identity.chat_id);
        let topology_body = serde_json::json!({"id":topology_id,"schema_version":1,"run":identity.run_id,"worker":identity.worker_interface_id,"owner_interface":owner,"prelude_artifact":prelude_ref,"file_write_tool":authoring_tool,
            "authoring_result":edge("authoring_result".into(),&authoring_environment,owner),
            "model_output":edge("model_output".into(),owner,&identity.model_interface_id),
            "notification":edge("notification".into(),owner,&identity.chat_interface_id),
            "receipt":edge("receipt".into(),owner,owner),"readiness":edge("readiness".into(),owner,owner),"routes":owner_routes,
            "coauthor":{"owner":identity.model_interface_id,"source":identity.authoring_source_id,"door":identity.authoring_environment_id,"tool":identity.authoring_tool_id,"call_label":config.artifact_labels.tool_call,"result_label":format!("{}.result",identity.authoring_tool_id)}});
        let topology_ref = artifact(
            topology_id,
            &topology_body,
        );
        if pretty_json(&prelude_body)?.len() as u64 > max_source_bytes {
            return Err("composed prelude exceeds the selected room source byte bound".into());
        }
        sidecar["context"] =
            serde_json::json!({"mode":"persistent","prelude_artifact":prelude_ref});
        sidecar["continuation"] = serde_json::json!({"mode":"admitted_artifact","resume_tool":resume,"notification_interface":identity.chat_interface_id});
        sidecar["handoff"] = serde_json::json!({"topology_artifact":topology_ref,"wait_tool":wait});
        sidecar["routes"] = serde_json::json!(routes);
        identity.code_mode_owners.insert(
            source.to_string(),
            PlannedCodeModeOwner {
                recipe,
                prelude_sources,
                authoring_environment,
                authoring_tool,
                prelude: prelude_ref,
                prelude_body,
                exports,
                topology: topology_ref,
                topology_body,
                sidecar,
                nested_doors,
                nested_tools,
            },
        );
    }
    Ok(())
}

pub(crate) fn plan_independent_code_mode_authoring(
    registry_root: &Path,
    config: &ClientConfig,
    capabilities: &FormedCapabilityTopology,
    identity: &PlannedIdentity,
    writes: &mut Vec<PlannedClientWrite>,
) -> Result<(), String> {
    let interface_root = registry_root.join("interface");
    for owner in identity.code_mode_owners.values() {
        let binding = capabilities
            .tool_bindings
            .get(&InterfaceId::new(&owner.recipe.authoring_room))
            .ok_or("owner authoring door is not in the selected formed capabilities")?;
        if !binding
            .tools
            .iter()
            .any(|t| t.tool.as_str() == owner.recipe.authoring_tool)
        {
            return Err("owner authoring tool is not in the selected formed capabilities".into());
        }
        push_json_write(
            interface_root
                .join(&owner.authoring_environment)
                .join("interface.json"),
            sibling_descriptor_bytes(&binding.interface, &owner.authoring_environment, None, None)?,
            format!("interface:{}", owner.authoring_environment),
            writes,
        )?;
        plan_interface_life(
            &interface_root,
            &owner.recipe.authoring_room,
            &owner.authoring_environment,
            writes,
        )?;
        plan_capability_sidecars(
            &interface_root,
            &owner.recipe.authoring_room,
            &owner.authoring_environment,
            binding,
            config,
            identity,
            writes,
        )?;
        plan_octopus_os_sidecar(
            &interface_root,
            &owner.authoring_environment,
            identity,
            "worker_environment_door",
            false,
            None,
            false,
            writes,
        )?;
        let mut descriptor = load_tool_descriptor_from_dir(
            &registry_root.join("tool"),
            &owner.recipe.authoring_tool,
        )
        .map_err(|e| e.to_string())?
        .ok_or("owner file tool descriptor is absent")?;
        if descriptor.interface.as_str() != owner.recipe.authoring_room {
            return Err("owner file tool belongs to another door".into());
        }
        copy_tool_descriptor(&mut descriptor, &owner.authoring_tool);
        descriptor.interface = InterfaceId::new(&owner.authoring_environment);
        narrow_run_authoring_tool_authority(&mut descriptor)?;
        let content = pretty_json(&serde_json::to_value(&descriptor).map_err(|e| e.to_string())?)?;
        parse_strict::<ToolDescriptor>(content.as_bytes()).map_err(|e| e.to_string())?;
        push_json_write(
            registry_root
                .join("tool")
                .join(&owner.authoring_tool)
                .join("tool.json"),
            content,
            format!("tool:{}", owner.authoring_tool),
            writes,
        )?;
    }
    Ok(())
}

pub(crate) fn write_code_mode_owner_artifacts(
    registry_root: &Path,
    identity: &PlannedIdentity,
    writes: &mut Vec<PlannedClientWrite>,
) -> Result<(), String> {
    for owner in identity.code_mode_owners.values() {
        for source in &owner.prelude_sources {
            let path = registry_root.join("artifact").join(source.snapshot.id.as_str()).join("artifact.json");
            match std::fs::read(&path) {
                Ok(bytes) if bytes == source.content.as_bytes() => {},
                Ok(_) => return Err("task-owned prelude source snapshot differs from its preserved bytes".into()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => writes.push(PlannedClientWrite {
                    path, content:source.content.clone(), tag:format!("artifact:{}",source.snapshot.id), expected_before_hash:None,
                }),
                Err(error) => return Err(format!("prelude source snapshot cannot be read: {error}")),
            }
        }
        for (artifact, body) in [
            (&owner.prelude, &owner.prelude_body),
            (&owner.topology, &owner.topology_body),
        ] {
            push_json_write(
                registry_root
                    .join("artifact")
                    .join(artifact.id.as_str())
                    .join("artifact.json"),
                pretty_json(body)?,
                format!("artifact:{}", artifact.id),
                writes,
            )?;
        }
    }
    Ok(())
}

#[derive(serde::Deserialize, serde::Serialize)]
pub(crate) struct CodeModeSidecarReferences {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) continuation: Option<CodeModeContinuationReferences>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) handoff: Option<CodeModeHandoffReferences>,
    #[serde(default)]
    pub(crate) routes: Vec<CodeModeRouteReferences>,
    #[serde(flatten)]
    pub(crate) settings: serde_json::Map<String, serde_json::Value>,
}

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CodeModeContinuationReferences {
    pub(crate) mode: String,
    pub(crate) resume_tool: String,
    pub(crate) notification_interface: Option<String>,
}

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CodeModeHandoffReferences {
    pub(crate) topology_artifact: octopus_ledger::ArtifactRef,
    pub(crate) wait_tool: String,
}

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CodeModeRouteReferences {
    pub(crate) tool: String,
    pub(crate) interface: String,
    pub(crate) checkpoint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) result_checkpoint: Option<String>,
}

pub(crate) fn plan_code_mode_references(
    content: &str,
    source_interface: &str,
    config: &ClientConfig,
    identity: &PlannedIdentity,
) -> Result<String, String> {
    if let Some(owner) = identity.code_mode_owners.get(source_interface) {
        return pretty_json(&owner.sidecar);
    }
    let mut sidecar: CodeModeSidecarReferences = serde_json::from_str(content)
        .map_err(|e| format!("code mode sidecar is malformed: {e}"))?;
    let tool = |base: &str| {
        identity
            .capability_tools
            .get(base)
            .cloned()
            .ok_or_else(|| format!("code mode role tool '{base}' was not selected and formed"))
    };
    let door = |base: &str| {
        identity
            .capability_doors
            .get(base)
            .cloned()
            .ok_or_else(|| format!("code mode route door '{base}' was not selected and formed"))
    };
    for route in &mut sidecar.routes {
        if route.interface == source_interface {
            return Err("code mode cannot recursively route into its own door".into());
        }
        route.tool = tool(&route.tool)?;
        route.interface = door(&route.interface)?;
    }
    if let Some(continuation) = &mut sidecar.continuation {
        if continuation.mode != "admitted_artifact" {
            return Err("code mode continuation mode is unknown".into());
        }
        continuation.resume_tool = tool(&continuation.resume_tool)?;
        if let Some(interface) = &mut continuation.notification_interface {
            if interface == &config.interface {
                *interface = identity.chat_interface_id.clone();
            } else {
                *interface = door(interface)?;
            }
        }
    }
    if let Some(handoff) = &mut sidecar.handoff {
        handoff.wait_tool = tool(&handoff.wait_tool)?;
        if sidecar.continuation.is_none() {
            return Err("code mode handoff has no admitted continuation".into());
        }
    }
    if sidecar
        .settings
        .get("context")
        .and_then(|c| c.get("mode"))
        .and_then(serde_json::Value::as_str)
        == Some("persistent")
        && sidecar.handoff.is_none()
    {
        return Err("selected persistent code mode requires an explicit independently owned topology artifact and authoring contract".into());
    }
    pretty_json(&serde_json::to_value(sidecar).map_err(|e| e.to_string())?)
}
