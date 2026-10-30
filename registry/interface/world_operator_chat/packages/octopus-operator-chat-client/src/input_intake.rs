use std::path::Path;

use octopus_descriptors::InterfaceId;
use octopus_ledger::RequiredVerifiedArtifact;
use octopus_run_authoring::{CheckedRunWriter, RunAuthoringProfile};
use octopus_run_record::ArtifactBinding;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{PlannedClientWrite, PlannedIdentity, pretty_json, push_json_write, safe_segment};

pub const INPUT_INTAKE_DRIVER: &str = "checked_input_intake";
pub const INPUT_INTAKE_LABEL: &str = "provider.peer.input";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputNotificationAction {
    #[default]
    Queue,
    Followup,
}

impl InputNotificationAction {
    fn is_queue(&self) -> bool {
        *self == Self::Queue
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputIntakeConfig {
    pub schema_version: u32,
    pub run: String,
    pub provider_interface: InterfaceId,
    pub writer: CheckedRunWriter,
    pub writer_required_verified_artifacts: Vec<RequiredVerifiedArtifact>,
    pub profile: RunAuthoringProfile,
    pub provider_profile: RunAuthoringProfile,
    pub input_label: String,
    #[serde(default, skip_serializing_if = "InputNotificationAction::is_queue")]
    pub notification_action: InputNotificationAction,
    pub artifact_bindings: Vec<ArtifactBinding>,
}

impl InputIntakeConfig {
    pub fn matches_declaration(&self, declaration: &Value) -> Result<bool, String> {
        let declared: Self = serde_json::from_value(declaration.clone()).map_err(|e| e.to_string())?;
        Ok(serde_json::to_value(&declared).map_err(|e| e.to_string())?
            == serde_json::to_value(self).map_err(|e| e.to_string())?)
    }

    pub fn validate(&self, interface: &InterfaceId) -> Result<(), String> {
        safe_segment(&self.run, "intake run")?;
        if self.schema_version != 1
            || self.writer.owner != *interface
            || self.writer.source != *interface
            || self.writer.door == *interface
            || self.provider_interface == *interface
            || self.provider_interface == self.writer.door
            || self.input_label != INPUT_INTAKE_LABEL
            || self.profile.file_write_tool != self.writer.tool
            || self.profile.file_write_label != self.writer.call_label
            || self.profile.file_write_result_label != self.writer.result_label
            || self.profile.result_timeout_ms == 0
            || self.writer.call_label == self.input_label
        {
            return Err(
                "intake declaration has inconsistent exact interfaces or writer profile".into(),
            );
        }
        validate_writer_requirements(&self.writer_required_verified_artifacts, &self.writer)?;
        let mut ids = std::collections::BTreeSet::new();
        for binding in &self.artifact_bindings {
            if !binding.access.read || !ids.insert(&binding.id) {
                return Err("intake static bindings must be distinct and readable".into());
            }
        }
        Ok(())
    }
}

fn validate_writer_requirements(
    requirements: &[RequiredVerifiedArtifact],
    writer: &CheckedRunWriter,
) -> Result<(), String> {
    let [requirement] = requirements else {
        return Err("intake writer requires exactly one required_verified_artifact".into());
    };
    requirement.validate("intake writer")?;
    if requirement.artifact_label != writer.call_label
        || requirement.params != crate::authoring_call_params(&writer.tool)
    {
        return Err("intake writer requirement must pin its exact call label, tool and preimage metadata match".into());
    }
    Ok(())
}

pub(crate) struct IntakeWiring {
    pub interface: String,
    pub door: String,
    pub tool: String,
}

pub(crate) fn wire(
    registry: &Path,
    identity: &PlannedIdentity,
    writes: &mut Vec<PlannedClientWrite>,
    content: String,
) -> Result<(String, Option<IntakeWiring>), String> {
    let provider_path = registry
        .join("interface")
        .join(&identity.model_interface_id)
        .join("interface.json");
    let mut descriptor = planned_json(writes, &provider_path)?;
    let Some(selection) =
        descriptor.pointer("/interface_driver_config/private_config/input_intake")
    else {
        return Ok((content, None));
    };
    let template = selection
        .get("template_interface_id")
        .and_then(Value::as_str)
        .ok_or("input_intake requires an explicit template_interface_id")?;
    safe_segment(template, "input intake template")?;
    let template_path = registry
        .join("interface")
        .join(template)
        .join("interface.json");
    let mut intake: Value =
        serde_json::from_slice(&std::fs::read(&template_path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    if intake["id"] != template
        || intake["interface_type"] != "system"
        || intake["role"] != "service"
        || intake["interface_driver_ref"] != INPUT_INTAKE_DRIVER
    {
        return Err("input intake template must declare the exact System service driver".into());
    }
    let suffix = blake3::hash(identity.run_id.as_bytes())
        .to_hex()
        .to_string();
    let id = format!("system_service_input_{}", &suffix[..24]);
    let door = format!("system_input_writer_{}", &suffix[..24]);
    let tool = format!("file.write.input_{}", &suffix[..24]);
    let profile: RunAuthoringProfile = serde_json::from_value(
        descriptor
            .pointer("/interface_driver_config/private_config/run_authoring_profile")
            .cloned()
            .ok_or("intake provider has no checked authoring profile")?,
    )
    .map_err(|e| e.to_string())?;
    let mut record: Value = serde_json::from_str(&content).map_err(|e| e.to_string())?;
    let provider_state = &record["interface_state"][&identity.model_interface_id];
    let templates = provider_state["run_authoring_binding"]["checkpoints"]
        .as_array()
        .ok_or("intake provider has no topology")?;
    let call = templates
        .iter()
        .find(|cp| cp["role"] == "authoring_call")
        .ok_or("intake provider has no writer call")?;
    let call_row = record["checkpoints"]
        .as_array()
        .ok_or("intake run has no checkpoints")?
        .iter()
        .find(|cp| {
            cp["interface_binding"] == call["interface_binding"]
                && record["active_checkpoints"]
                    .as_array()
                    .is_some_and(|ids| ids.contains(&cp["id"]))
        })
        .ok_or("intake provider writer is not active")?;
    let static_bindings: Vec<ArtifactBinding> =
        serde_json::from_value(call_row["artifact_bindings"].clone()).map_err(|e| e.to_string())?;
    if call_row["required_verified_artifacts"] != call["required_verified_artifacts"] {
        return Err("intake provider writer call requirement differs from its template".into());
    }
    let mut writer_requirements: Vec<RequiredVerifiedArtifact> =
        serde_json::from_value(call_row["required_verified_artifacts"].clone())
            .map_err(|e| e.to_string())?;
    let [requirement] = writer_requirements.as_mut_slice() else {
        return Err("intake provider writer requires exactly one required_verified_artifact".into());
    };
    requirement.validate("intake provider writer")?;
    if requirement.artifact_label != profile.file_write_label
        || requirement.params != crate::authoring_call_params(&profile.file_write_tool)
    {
        return Err("intake provider writer requirement does not pin its exact preimage".into());
    }
    requirement.params["tools"] = json!([tool]);
    let notification_action = match record["interface_state"].get(&id) {
        Some(prior) => serde_json::from_value::<InputIntakeConfig>(prior["input_intake"].clone())
            .map_err(|e| e.to_string())?.notification_action,
        None => serde_json::from_value(intake.pointer("/interface_driver_config/private_config/notification_action")
            .cloned().ok_or("input intake template requires notification_action")?)
            .map_err(|e| e.to_string())?,
    };
    let config = InputIntakeConfig {
        schema_version: 1,
        run: identity.run_id.clone(),
        provider_interface: InterfaceId::new(&identity.model_interface_id),
        writer: CheckedRunWriter {
            owner: InterfaceId::new(&id),
            source: InterfaceId::new(&id),
            door: InterfaceId::new(&door),
            tool: tool.clone(),
            call_label: profile.file_write_label.clone(),
            result_label: format!("{tool}.result"),
        },
        writer_required_verified_artifacts: writer_requirements,
        profile: RunAuthoringProfile {
            file_write_tool: tool.clone(),
            file_write_label: profile.file_write_label.clone(),
            file_write_result_label: format!("{tool}.result"),
            result_timeout_ms: profile.result_timeout_ms,
        },
        provider_profile: profile,
        input_label: INPUT_INTAKE_LABEL.into(),
        notification_action,
        artifact_bindings: static_bindings,
    };
    config.validate(&InterfaceId::new(&id))?;
    let declaration = serde_json::to_value(&config).map_err(|e| e.to_string())?;
    crate::copy_json_descriptor(&mut intake, &id)?;
    intake["interface_driver_config"]["private_config"].as_object_mut()
        .ok_or("input intake template private config must be an object")?.remove("notification_action");
    intake["interface_driver_config"]["private_config"]["input_intake"] = declaration.clone();
    push_json_write(
        registry.join("interface").join(&id).join("interface.json"),
        pretty_json(&intake)?,
        format!("interface:{id}"),
        writes,
    )?;
    crate::plan_interface_life(&registry.join("interface"), template, &id, writes)?;
    let source_door = registry
        .join("interface")
        .join(&identity.authoring_environment_id);
    let copies = writes
        .iter()
        .filter(|w| w.path.starts_with(&source_door))
        .cloned()
        .collect::<Vec<_>>();
    for mut copy in copies {
        let relative = copy
            .path
            .strip_prefix(&source_door)
            .map_err(|e| e.to_string())?;
        if relative == Path::new("octopus_os.json") {
            continue;
        }
        let target = registry.join("interface").join(&door).join(relative);
        if relative == Path::new("interface.json") {
            let mut value: Value =
                serde_json::from_str(&copy.content).map_err(|e| e.to_string())?;
            crate::copy_json_descriptor(&mut value, &door)?;
            copy.content = pretty_json(&value)?;
        }
        push_json_write(
            target,
            copy.content,
            format!("intake_writer:{door}"),
            writes,
        )?;
    }
    let original_tool = registry
        .join("tool")
        .join(&identity.authoring_tool_id)
        .join("tool.json");
    let mut tool_descriptor = planned_json(writes, &original_tool)?;
    crate::copy_json_descriptor(&mut tool_descriptor, &tool)?;
    tool_descriptor["interface"] = json!(door);
    push_json_write(
        registry.join("tool").join(&tool).join("tool.json"),
        pretty_json(&tool_descriptor)?,
        format!("tool:{tool}"),
        writes,
    )?;
    let policy_path = registry
        .join("interface")
        .join(&identity.worker_interface_id)
        .join("policy.json");
    let mut value = planned_json(writes, &policy_path)?;
    for (pointer, additions) in [
        ("/ceiling/interfaces", vec![&id, &door]),
        ("/ceiling/tools", vec![&tool]),
    ] {
        let entries = value
            .pointer_mut(pointer)
            .and_then(Value::as_array_mut)
            .ok_or("intake worker ceiling is malformed")?;
        for addition in additions {
            if !entries.contains(&json!(addition)) {
                entries.push(json!(addition));
            }
        }
    }
    if let Some(policy) = value
        .pointer("/ceiling/interface_policy")
        .and_then(|p| p.get(&identity.authoring_environment_id))
        .cloned()
    {
        value["ceiling"]["interface_policy"][&door] = policy;
    }
    replace_json(
        writes,
        policy_path,
        &value,
        format!("policy:{}", identity.worker_interface_id),
    )?;
    let private = descriptor
        .pointer_mut("/interface_driver_config/private_config")
        .and_then(Value::as_object_mut)
        .ok_or("provider private config is absent")?;
    private
        .get_mut("input_intake")
        .ok_or("intake selection disappeared")?["interface_id"] = json!(id);
    let hidden = private
        .get_mut("model_hidden_tools")
        .and_then(Value::as_array_mut)
        .ok_or("intake requires the declared hidden authoring tool list")?;
    if !hidden.contains(&json!(tool)) {
        hidden.push(json!(tool));
    }
    replace_json(
        writes,
        provider_path,
        &descriptor,
        format!("interface:{}", identity.model_interface_id),
    )?;
    let templates = record["interface_state"][&identity.model_interface_id]["run_authoring_binding"]["checkpoints"].as_array_mut().ok_or("provider templates absent")?;
    templates.retain(|cp| cp["interface_binding"]["interface"] != id);
    templates.push(json!({"name":format!("input_{id}"),"role":"owner_input","owner_authoring":config.writer,"interface_binding":binding(&id, INPUT_INTAKE_LABEL),"outbound_interface_bindings":[crate::AuthoredInterfaceBinding::all(config.provider_interface.clone())],"include_context":true,"required_verified_artifacts":[]}));
    if let Some(prior) = record["interface_state"].get(&id) {
        let mut expected: InputIntakeConfig = serde_json::from_value(prior["input_intake"].clone())
            .map_err(|e| e.to_string())?;
        expected.artifact_bindings = config.artifact_bindings.clone();
        if !expected.matches_declaration(&declaration)? {
            return Err("intake continuation changed its exact declaration".into());
        }
        let owned: Vec<String> = serde_json::from_value(prior["owned_checkpoint_ids"].clone()).map_err(|e| e.to_string())?;
        for cp in record["checkpoints"].as_array_mut().ok_or("intake run has no checkpoints")? {
            if owned.iter().any(|id| cp["id"] == *id) { cp["artifact_bindings"] = json!(config.artifact_bindings); }
        }
        record["interface_state"][&id]["input_intake"] = declaration.clone();
        if let Some(writer) = record["interface_state"][&id].get_mut("writer_run") {
            writer["role_artifact_bindings"] = json!(config.artifact_bindings);
        }
    } else {
        let (rows, ids) = initial_checkpoints(&config, &record["trace"], "initial")?;
        record["checkpoints"]
            .as_array_mut()
            .ok_or("checkpoints absent")?
            .extend(rows);
        record["active_checkpoints"]
            .as_array_mut()
            .ok_or("active checkpoints absent")?
            .extend(ids.iter().map(|id| json!(id)));
        record["interface_state"][&id] = json!({"input_intake":declaration,"owned_checkpoint_ids":ids,"frontier_checkpoint_ids":[ids[1],ids[2]]});
    }
    reconcile_input_context(&mut record, &config)?;
    let content = pretty_json(&record)?;
    octopus_run_record::RunRecord::from_bytes(content.as_bytes())
        .map_err(|e| e.to_string())?;
    Ok((
        content,
        Some(IntakeWiring {
            interface: id,
            door,
            tool,
        }),
    ))
}

fn reconcile_input_context(record: &mut Value, config: &InputIntakeConfig) -> Result<(), String> {
    let parsed =
        octopus_run_record::RunRecord::from_bytes(pretty_json(record)?.as_bytes())
            .map_err(|e| e.to_string())?;
    let owner = config.writer.owner.as_str();
    let owned: Vec<String> =
        serde_json::from_value(record["interface_state"][owner]["owned_checkpoint_ids"].clone())
            .map_err(|e| e.to_string())?;
    let expected_count = if record["interface_state"][owner].get("writer_run").is_some() { 1 } else { 3 };
    if owned.len() != expected_count
        || owned
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != expected_count
    {
        return Err("intake must own distinct input/call/result checkpoints".into());
    }
    let context = octopus_run_authoring::input_artifacts::read_bindings(
        &parsed, &config.writer.owner,
        octopus_run_authoring::provider_context_bindings(&parsed, &config.provider_interface)?,
    )?;
    let topology: crate::BoundRunAuthoringTopology = serde_json::from_value(
        parsed.interface_state[&config.provider_interface]["run_authoring_binding"].clone(),
    ).map_err(|e| e.to_string())?;
    let declared_static = topology.checkpoints.iter()
        .filter(|template| template.role == crate::BoundCheckpointRole::Stable && template.include_context)
        .flat_map(|template| template.static_artifact_bindings.iter())
        .collect::<std::collections::BTreeSet<_>>();
    for id in &declared_static {
        let bindings = context.iter().filter(|binding| &binding.id == *id).collect::<Vec<_>>();
        if !matches!(bindings.as_slice(), [binding] if binding.access.read) {
            return Err("intake static context requires one exact readable provider binding".into());
        }
    }
    let static_ids = context.iter().filter(|binding| declared_static.contains(&binding.id))
        .map(|binding| binding.id.clone()).collect::<Vec<_>>();
    if owned.iter().any(|id| {
        !parsed
            .checkpoints
            .iter()
            .any(|cp| cp.id.as_str() == id && parsed.active_checkpoints.contains(&cp.id))
    }) {
        return Err("intake checkpoint ownership is not fully active".into());
    }
    let input_binding: crate::AuthoredInterfaceBinding =
        serde_json::from_value(binding(owner, &config.input_label)).map_err(|e| e.to_string())?;
    let output_binding = vec![crate::AuthoredInterfaceBinding::all(
        config.provider_interface.clone(),
    )];
    let matches = parsed
        .checkpoints
        .iter()
        .enumerate()
        .filter(|(_, cp)| {
            parsed.active_checkpoints.contains(&cp.id)
                && cp.interface_binding.as_ref() == Some(&input_binding)
                && cp.outbound_interface_bindings == output_binding
                && cp.required_verified_artifacts.is_empty()
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let [index] = matches.as_slice() else {
        return Err("intake requires one exact active provider input".into());
    };
    let id = parsed.checkpoints[*index].id.as_str();
    if !owned.iter().any(|owned| owned == id)
        || record["interface_state"]
            .as_object()
            .ok_or("intake run has no interface state")?
            .iter()
            .filter(|(interface, _)| interface.as_str() != owner)
            .any(|(_, state)| {
                [
                    "owned_checkpoint_ids",
                    "frontier_checkpoint_ids",
                    "cycle_checkpoint_ids",
                ]
                .iter()
                .any(|field| {
                    state[*field]
                        .as_array()
                        .is_some_and(|ids| ids.contains(&json!(id)))
                })
            })
    {
        return Err("intake input is not exclusively owned by its declared interface".into());
    }
    let templates = record["interface_state"][config.provider_interface.as_str()]["run_authoring_binding"]["checkpoints"]
        .as_array_mut().ok_or("intake provider templates are absent")?;
    let matches = templates.iter_mut().filter(|template| {
        template["role"] == "owner_input"
            && template["owner_authoring"] == json!(config.writer)
            && template["interface_binding"] == json!(input_binding)
            && template["outbound_interface_bindings"] == json!(output_binding)
            && template["include_context"] == true
    }).collect::<Vec<_>>();
    let [template]: [&mut Value; 1] = matches.try_into().map_err(|_| "intake requires one exact declared provider input")?;
    if static_ids.is_empty() {
        template.as_object_mut().ok_or("intake template must be an object")?.remove("static_artifact_bindings");
    } else {
        template["static_artifact_bindings"] = json!(static_ids);
    }
    record["checkpoints"][*index]["artifact_bindings"] = json!(context);
    Ok(())
}

fn planned_json(writes: &[PlannedClientWrite], path: &Path) -> Result<Value, String> {
    match writes.iter().find(|write| write.path == path) {
        Some(write) => serde_json::from_str(&write.content).map_err(|e| e.to_string()),
        None => serde_json::from_slice(
            &std::fs::read(path)
                .map_err(|e| format!("intake descriptor {} cannot be read: {e}", path.display()))?,
        )
        .map_err(|e| e.to_string()),
    }
}

fn replace_json(
    writes: &mut Vec<PlannedClientWrite>,
    path: std::path::PathBuf,
    value: &Value,
    tag: String,
) -> Result<(), String> {
    let content = pretty_json(value)?;
    if let Some(write) = writes.iter_mut().find(|write| write.path == path) {
        write.content = content;
        Ok(())
    } else {
        push_json_write(path, content, tag, writes)
    }
}

pub fn binding(interface: &str, label: &str) -> Value {
    json!({"interface":interface,"config":{"artifact_policy":{"mode":"allow","names":[label],"namespaces":[]}}})
}

pub fn initial_checkpoints(
    config: &InputIntakeConfig,
    trace: &Value,
    generation: &str,
) -> Result<(Vec<Value>, Vec<String>), String> {
    config.validate(&config.writer.owner)?;
    let owner = config.writer.owner.as_str();
    let ids = ["input", "call", "result"]
        .iter()
        .map(|kind| format!("cp_{owner}_{kind}_{generation}"))
        .collect::<Vec<_>>();
    let routes = [
        (
            owner,
            config.provider_interface.as_str(),
            config.input_label.as_str(),
        ),
        (
            owner,
            config.writer.door.as_str(),
            config.writer.call_label.as_str(),
        ),
        (
            config.writer.door.as_str(),
            owner,
            config.writer.result_label.as_str(),
        ),
    ];
    let rows = routes.iter().zip(&ids).enumerate().map(|(index, ((from, to, label), id))| {
        json!({
            "id": id,
            "run": config.run,
            "epoch": 0,
            "trace": trace,
            "interface_binding": binding(from, label),
            "outbound_interface_bindings": [crate::AuthoredInterfaceBinding::all(InterfaceId::new(*to))],
            "artifact_bindings": config.artifact_bindings,
            "required_verified_artifacts": if index == 1 {
                config.writer_required_verified_artifacts.as_slice()
            } else {
                &[]
            },
        })
    }).collect();
    Ok((rows, ids))
}
