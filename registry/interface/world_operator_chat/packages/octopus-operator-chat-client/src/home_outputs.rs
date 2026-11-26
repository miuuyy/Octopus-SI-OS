use super::*;
use serde_json::{Value, json};

pub(super) fn interfaces(identity: &PlannedIdentity) -> Vec<String> {
    if identity.surface_role != "home" {
        return Vec::new();
    }
    vec![
        format!("system_worker_home_presentation_{}", identity.chat_id),
        format!("system_worker_home_voice_update_{}", identity.chat_id),
        format!("system_worker_home_voice_delegation_{}", identity.chat_id),
    ]
}

pub(super) fn context_id(identity: &PlannedIdentity) -> String {
    format!("artifact-home-delivery-{}", identity.chat_id)
}

pub(super) fn template(
    identity: &PlannedIdentity,
    voice_update: bool,
) -> Result<BoundCheckpointTemplate, String> {
    let source = &interfaces(identity)[usize::from(voice_update)];
    let (label, recipient) = if voice_update {
        let voice = identity
            .voice
            .as_ref()
            .ok_or("Home delivery requires its declared voice topology")?;
        ("realtime.session.update", voice.context.provider_id.as_str())
    } else {
        (
            "client.surface.document",
            identity.chat_interface_id.as_str(),
        )
    };
    let binding = |id: &str| AuthoredInterfaceBinding {
        interface: InterfaceId::new(id),
        config: InterfaceBindingConfig {
            artifact_policy: BindingArtifactPolicy::Allow {
                names: if voice_update { vec![label.into()] } else { vec![label.into(), "client.action".into()] },
                namespaces: Vec::new(),
            },
        },
    };
    let mut result = bound_checkpoint_template(
        BoundCheckpointRole::IndependentInput,
        if voice_update { binding(source) } else { presentation_binding(source) },
        if voice_update {
            vec![binding(&identity.model_interface_id)]
        } else {
            vec![AuthoredInterfaceBinding {
                interface: InterfaceId::new(&identity.model_interface_id),
                config: InterfaceBindingConfig { artifact_policy: BindingArtifactPolicy::Allow {
                    names: vec!["client.surface.document".into()], namespaces: Vec::new(),
                } },
            }, binding(recipient)]
        },
        Vec::new(),
    )?;
    result.static_artifact_bindings =
        vec![octopus_descriptors::ArtifactId::new(context_id(identity))];
    Ok(result)
}

const PRESENTATION_LABELS: [&str; 2] = ["client.surface.document", "client.action"];

pub(super) const CLIENT_ACTION_RESULT_LABEL: &str = "client.action.result";

pub(super) fn operator_input_labels(_identity: &PlannedIdentity, message: &str) -> Vec<String> {
    vec![message.to_string()]
}

fn client_result_template(identity: &PlannedIdentity, topology: &FormedCapabilityTopology) -> Result<Option<BoundCheckpointTemplate>, String> {
    if !identity.capability_tools.contains_key("client.mcp") { return Ok(None); }
    let mut owners = topology.tool_bindings.iter().filter(|(_, binding)|
        binding.tools.iter().any(|tool| tool.tool.as_str() == "client.mcp"));
    let (owner, _) = owners.next().ok_or("Home client controls require their formed tool owner")?;
    if owners.next().is_some() { return Err("Home client controls have multiple tool owners".into()); }
    let door = identity.capability_doors.get(owner.as_str()).ok_or("Home client control owner has no exact planned room")?;
    let binding = |id: &str| AuthoredInterfaceBinding {
        interface: InterfaceId::new(id), config: InterfaceBindingConfig {
            artifact_policy: BindingArtifactPolicy::Allow { names: vec![CLIENT_ACTION_RESULT_LABEL.into()], namespaces: vec![] },
        },
    };
    bound_checkpoint_template(BoundCheckpointRole::IndependentInput,
        binding(&identity.chat_interface_id), vec![binding(door)], Vec::new()).map(Some)
}

fn presentation_binding(id: &str) -> AuthoredInterfaceBinding {
    AuthoredInterfaceBinding {
        interface: InterfaceId::new(id),
        config: InterfaceBindingConfig {
            artifact_policy: BindingArtifactPolicy::Allow {
                names: PRESENTATION_LABELS.into_iter().map(String::from).collect(),
                namespaces: Vec::new(),
            },
        },
    }
}

pub(super) fn plan(
    request: &OperatorChatPlanRequest,
    identity: &PlannedIdentity,
    private_worker_role_artifacts: &[String],
    writes: &mut Vec<PlannedClientWrite>,
) -> Result<Vec<ArtifactBindingSeed>, String> {
    let authoring = plan_task_coordinates(
        &request.registry_root, &request.ee_instances_root,
        request.run_records_root.as_deref(), identity, writes,
    )?;
    if identity.surface_role != "home" { return Ok(vec![authoring]); }
    let registry = &request.registry_root;
    request
        .run_records_root
        .as_ref()
        .filter(|path| path.is_absolute())
        .ok_or("Home context requires its explicit absolute run_records_root")?;
    let root = registry.join("interface");
    let source = load_interface_descriptor(&root, &identity.worker_choice_interface)?;
    for id in interfaces(identity)
        .into_iter()
        .take(if identity.voice.is_some() { 3 } else { 1 })
    {
        push_json_write(
            root.join(&id).join("interface.json"),
            sibling_descriptor_bytes(&source, &id, None, None)?,
            format!("interface:{id}"),
            writes,
        )?;
        push_json_write(
            root.join(&id).join("policy.json"),
            pretty_json(
                &json!({"id":id,"ceiling":{"interfaces":[],"tools":[],"wek":[],"mark_outcomes":[],"hard_blocked_tools":[]},"role_artifacts":private_worker_role_artifacts}),
            )?,
            format!("policy:{id}"),
            writes,
        )?;
        plan_octopus_os_sidecar(
            &root,
            &id,
            identity,
            "artifact_return",
            false,
            None,
            false,
            writes,
        )?;
    }
    let ids = interfaces(identity);
    let mut contract = json!({
        "widget":{"interface":ids[0],"label":"client.surface.document","recipient_interface":identity.chat_interface_id,"document_contract":"artifact-home-widget-authoring"},
        "client_action":{"interface":ids[0],"label":"client.action","recipient_interface":identity.chat_interface_id,"schema":"octopus.client_action","schema_version":1,"actions":["open_surface","inspect_client","list_widgets","set_widget"],"result_label":CLIENT_ACTION_RESULT_LABEL},
        "run":identity.run_id,
        "authoring_artifact":authoring.id
    });
    if let Some(voice) = &identity.voice {
        contract["voice_update"] = json!({"run":voice.context.run_id,"interface":ids[1],"label":"realtime.session.update","recipient_interface":voice.context.provider_id,"payload":{"call_id":"copy from the admitted realtime.delegation artifact","correlation_id":"same admitted call correlation","update_id":"new stable identity for this one semantic update","delegation_id":"same admitted delegation or null","channel":"commentary","text":"Model-selected public progress or result, grounded in Task evidence"}});
    }
    let text = format!(
        "Exact Home artifact delivery addresses for this run:\n{}\n\nFor widgets, client Extensions and client navigation use the actual granted client.mcp tool and its inspect_client/list_widgets/set_widget/publish_widget/open_file/open_client_surface catalogue operations with the stated System presentation interface. inspect_client reports the running client's Extensions folder, contracts, host vocabulary and every Extension's admission, review and runtime state before you change the client. The MCP adapter uses artifact.send under the same grant. inspect_client, list_widgets and set_widget wait for the client and return its actual saved state within the same tool call. For a long text document, save it with granted file tools and call open_file; it returns the readable target and requests its tab in the right Files pane. The correlated client.action.result returns to the system room; it does not become a new operator message. One set_widget call carries every change for any number of widgets from one inventory, with their exact widget_id/source_digest. Use that tool result to continue the request immediately, then give one concise confirmation. Do not end the turn to wait or poll for a result. These controls do not require Computer Use or source edits. During a live call, the client sends public progress to the voice commentary channel and streams public answers to the speakable channel, preserving the exact delegation and sending each text fragment once. The voice provider delegates a request to this run; use the same client controls and wait for their actual result before replying. Answer an admitted voice delegation with ordinary public output; do not call artifact.send merely to repeat that output. For independent Task returns or a deliberately separate voice update, use the actual granted artifact.send tool with the stated System interface, label, and payload. The runtime admits this artifact once and delivers it both to the listed recipient and to Home's exact text provider. Task workers can use the same return pins only after authoring those exact bindings, policy and readable role closure in their own runs. Never resend an admitted artifact to make it visible. Voice updates must copy call_id/correlation_id/delegation_id from actual admitted voice semantics, not private transport state or guessed values. A sent update is not speech-playback proof; inspect its realtime.session.updated evidence. Stopping voice does not cancel Tasks. Use artifact-home-widget-authoring for native/sandbox and persistent/transient document payloads and for installed client Extensions with their data sources.",
        pretty_json(&contract)?
    );
    let text = format!("{text}\n\nThe authoring_artifact is a separate bound reference for exact registry, run, tool and filesystem coordinates. Read it before editing configuration or selecting a source path. Widget controls need only the client_action address above and their tool schema.");
    let id = context_id(identity);
    push_json_write(
        registry.join("artifact").join(&id).join("artifact.json"),
        pretty_json(
            &json!({"id":id,"label":"context.reference","description":"Exact Home semantic artifact delivery addresses","text":text,"delivery":contract}),
        )?,
        format!("artifact:{id}"),
        writes,
    )?;
    Ok(vec![authoring, ArtifactBindingSeed { id }])
}

const AUTHORING_COORDINATE_GUIDANCE: &str = "Read authoring for this run's record paths, exact tool identities and declared scopes. Use the current callable tools and their schemas. Coordinates describe the authored installation; the current formed grant determines authority. A host_binding_not_supplied slot has no resolved path here. Inspect the relevant door when a path or execution capability is needed. Registry roots are installed state, not a source checkout. Follow the affected owner's current contract and related inputs and outputs before changing its records.";

pub(super) fn plan_task_coordinates(
    registry_root: &Path,
    ee_instances_root: &Path,
    run_records_root: Option<&Path>,
    identity: &PlannedIdentity,
    writes: &mut Vec<PlannedClientWrite>,
) -> Result<ArtifactBindingSeed, String> {
    let authoring = authoring_coordinates(registry_root, ee_instances_root, run_records_root, identity, writes)?;
    let id = format!("artifact-run-authoring-coordinates-{}", identity.chat_id);
    push_json_write(
        registry_root.join("artifact").join(&id).join("artifact.json"),
        pretty_json(&json!({
            "id":id,"label":"context.reference",
            "description":"Exact run, registry, tool ids and declared filesystem scopes; read before authoring configuration or choosing a source path.",
            "text":AUTHORING_COORDINATE_GUIDANCE,
            "authoring":authoring,
        }))?,
        format!("artifact:{id}"),
        writes,
    )?;
    Ok(ArtifactBindingSeed { id })
}

fn authoring_coordinates(
    registry: &Path,
    ee_instances_root: &Path,
    runs: Option<&Path>,
    identity: &PlannedIdentity,
    writes: &[PlannedClientWrite],
) -> Result<Value, String> {
    require_absolute("authoring coordinate registry_root", registry)?;
    require_absolute("authoring coordinate ee_instances_root", ee_instances_root)?;
    if let Some(root) = runs {
        require_absolute("authoring coordinate run_records_root", root)?;
    }
    let mut scopes = Vec::new();
    for (source, exact) in &identity.capability_tools {
        let tool: ToolDescriptor = match writes.iter().find(|write| write.tag == format!("tool:{exact}")) {
            Some(write) => serde_json::from_str(&write.content).map_err(|error| error.to_string())?,
            None => load_tool_descriptor_from_dir(&registry.join("tool"), exact)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| format!("authoring coordinates have no exact tool '{exact}'"))?,
        };
        if tool.id.as_str() != exact {
            return Err(format!("authoring coordinate tool '{exact}' has a different identity"));
        }
        let door: InterfaceDescriptor = match writes.iter().find(|write| write.tag == format!("interface:{}", tool.interface)) {
            Some(write) => serde_json::from_str(&write.content).map_err(|error| error.to_string())?,
            None => load_interface_descriptor(&registry.join("interface"), tool.interface.as_str())?,
        };
        if door.id != tool.interface {
            return Err("authoring coordinate door identity differs from its tool".into());
        }
        if let Some(mut scope) = tool_path_scope(source, &tool, &door)? {
            scope["tool_descriptor"] = json!(registry.join("tool").join(exact).join("tool.json"));
            scope["door_descriptor"] = json!(registry.join("interface").join(tool.interface.as_str()).join("interface.json"));
            scopes.push(scope);
        }
    }
    let registries = ["interface", "artifact", "ee_template", "tool", "mark", "verifier", "worker_environment_kind"]
        .into_iter().map(|kind| (kind.to_owned(), json!(registry.join(kind))))
        .collect::<serde_json::Map<_, _>>();
    Ok(json!({
        "run":identity.run_id,"ee":identity.ee_id,"worker":identity.worker_interface_id,
        "provider_interface":identity.model_interface_id,
        "registry_root":registry,"registry_roots":registries,
        "ee_instances_root":ee_instances_root,"run_records_root":runs,
        "ee_file":ee_instances_root.join(format!("{}.json",identity.ee_id)),
        "run_file":runs.map(|root|root.join(format!("{}.json",identity.run_id))),
        "worker_policy":registry.join("interface").join(&identity.worker_interface_id).join("policy.json"),
        "provider_descriptor":registry.join("interface").join(&identity.model_interface_id).join("interface.json"),
        "tools":identity.capability_tools,"tool_scopes":scopes,
        "source_workspace":"Resolve from a bound source/workspace artifact or an exact tool door override; registry_root does not identify a source checkout.",
    }))
}

fn tool_path_scope(
    source: &str,
    tool: &ToolDescriptor,
    door: &InterfaceDescriptor,
) -> Result<Option<Value>, String> {
    let authority = &tool.process.authority;
    let mut slots = authority.read_root_slots.iter()
        .chain(&authority.write_root_slots).chain(&authority.executable_root_slots)
        .chain(&authority.optional_read_root_slots).chain(&authority.optional_write_root_slots)
        .chain(&authority.optional_executable_root_slots).chain(&tool.process.coordinate_root_slots)
        .cloned().collect::<BTreeSet<_>>();
    for path in authority.read_argument_paths.iter().chain(&authority.write_argument_paths)
        .chain(&authority.executable_argument_paths) {
        slots.extend(path.within_root_slots.iter().cloned());
    }
    slots.extend(authority.executable_root_subpaths.iter().map(|path|path.root_slot.clone()));
    if slots.is_empty() && authority.read_paths.is_empty() && authority.write_paths.is_empty()
        && authority.executable_paths.is_empty() && authority.executable_commands.is_empty() {
        return Ok(None);
    }
    let mut roots = serde_json::Map::new();
    for slot in slots {
        let path = door.interface_driver_config.as_ref().and_then(|config|config.host_slots.get(&slot));
        let value = if let Some(binding) = path {
            require_absolute("authoring coordinate root override", &binding.path)?;
            json!({"status":"exact_interface_override","path":binding.path})
        } else {
            json!({"status":"host_binding_not_supplied"})
        };
        roots.insert(slot, value);
    }
    Ok(Some(json!({
        "source_tool":source,"tool":tool.id,"interface":tool.interface,
        "worker_environment_kinds":tool.supported_worker_environment_kinds,
        "tool_descriptor":format!("tool/{}/tool.json",tool.id),
        "door_descriptor":format!("interface/{}/interface.json",tool.interface),
        "process_bin":tool.process.bin,"root_slots":roots,
        "read":{"root_slots":authority.read_root_slots,"optional_root_slots":authority.optional_read_root_slots,
            "paths":authority.read_paths,"argument_paths":authority.read_argument_paths},
        "write":{"root_slots":authority.write_root_slots,"optional_root_slots":authority.optional_write_root_slots,
            "paths":authority.write_paths,"argument_paths":authority.write_argument_paths},
        "execute":{"root_slots":authority.executable_root_slots,"optional_root_slots":authority.optional_executable_root_slots,
            "paths":authority.executable_paths,"commands":authority.executable_commands,
            "root_subpaths":authority.executable_root_subpaths,"argument_paths":authority.executable_argument_paths},
        "coordinate_root_slots":tool.process.coordinate_root_slots,
    })))
}

pub(super) fn wire(
    identity: &PlannedIdentity,
    topology: &FormedCapabilityTopology,
    roles: &[String],
    context: &[ArtifactBindingSeed],
    content: String,
) -> Result<String, String> {
    if identity.surface_role != "home" {
        return Ok(content);
    }
    let mut record: Value = serde_json::from_str(&content).map_err(|e| e.to_string())?;
    let provider_context = record["interface_state"][&identity.model_interface_id]["run_authoring_binding"]["checkpoints"]
        .as_array()
        .ok_or("Home provider has no declared checkpoint topology")?
        .iter()
        .filter(|template| template["role"] == "stable" && template["include_context"] == true)
        .flat_map(|template| template["static_artifact_bindings"].as_array().into_iter().flatten())
        .filter_map(Value::as_str)
        .map(String::from)
        .collect::<std::collections::BTreeSet<_>>();
    let mut deliveries = vec![template(identity, false)?];
    if identity.voice.is_some() {
        deliveries.push(template(identity, true)?);
    }
    if let Some(reply) = client_result_template(identity, topology)? { deliveries.push(reply); }
    for mut template in deliveries {
        for binding in context {
            let id = octopus_descriptors::ArtifactId::new(&binding.id);
            if !roles.contains(&binding.id) && !template.static_artifact_bindings.contains(&id)
                && provider_context.contains(&binding.id) {
                template.static_artifact_bindings.push(id);
            }
        }
        let mut artifacts = roles.iter().map(|id| json!({"id":id})).collect::<Vec<_>>();
        for id in &template.static_artifact_bindings {
            if !artifacts.iter().any(|binding| binding["id"] == id.as_str()) {
                artifacts.push(json!({"id":id}));
            }
        }
        let rows = record["checkpoints"]
            .as_array_mut()
            .ok_or("Home has no checkpoints")?;
        let matching = rows
            .iter()
            .filter(|cp| {
                cp["interface_binding"] == json!(template.interface_binding)
                    && cp["outbound_interface_bindings"]
                        == json!(template.outbound_interface_bindings)
            })
            .map(|cp| cp["id"].clone())
            .collect::<Vec<_>>();
        if matching.len() > 1 {
            return Err("Home delivery has duplicate exact receiving checkpoints".into());
        }
        if let Some(id) = matching.first() {
            let row = rows
                .iter_mut()
                .find(|cp| cp["id"] == *id)
                .ok_or("Home delivery row disappeared")?;
            row["artifact_bindings"] = json!(artifacts);
        }
        if matching.is_empty() {
            let id = format!("cp_home_delivery_{}", template.name);
            rows.push(json!({"id":id,"run":identity.run_id,"trace":format!("trace:{}:{id}",identity.run_id),"epoch":0,"interface_binding":template.interface_binding,"outbound_interface_bindings":template.outbound_interface_bindings,"artifact_bindings":artifacts,"required_verified_artifacts":[]}));
            record["active_checkpoints"]
                .as_array_mut()
                .ok_or("Home has no active checkpoints")?
                .push(json!(id));
        }
        // The room's reply route is independent of provider continuation. Ordinary
        // checked run edits preserve it as an external checkpoint.
        if !template.outbound_interface_bindings.iter().any(|binding| binding.interface.as_str() == identity.model_interface_id) {
            continue;
        }
        let templates=record["interface_state"][&identity.model_interface_id]["run_authoring_binding"]["checkpoints"].as_array_mut().ok_or("Home text provider has no authoring topology")?;
        if templates.iter().any(|cp| cp["name"] == template.name) {
            return Err(
                "Home delivery template already exists in the newly authored provider topology"
                    .into(),
            );
        }
        templates.push(json!(template));
    }
    record["interface_state"][&identity.chat_interface_id]["delivery_artifact"] =
        json!(context_id(identity));
    pretty_json(&record)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_coordinates_preserve_exact_overrides_and_unresolved_machine_slots() {
        let tool: ToolDescriptor = serde_json::from_str(include_str!("../../../../../tool/file.read/tool.json")).unwrap();
        let mut door: InterfaceDescriptor = serde_json::from_value(json!({
            "id":"system_room","interface_type":"system","role":"system",
            "direction":"bidirectional","source_names":["room"],"interface_driver_ref":"system_room",
            "interface_driver_config":{"host_slots":{"registry_tool_root":{"path":"/declared/tools"}}},
        })).unwrap();
        let value = tool_path_scope("file.read", &tool, &door).unwrap().unwrap();
        assert_eq!(value["root_slots"]["registry_tool_root"], json!({"status":"exact_interface_override","path":"/declared/tools"}));
        assert_eq!(value["root_slots"]["run_records_root"], json!({"status":"host_binding_not_supplied"}));
        assert_eq!(value["read"]["argument_paths"], json!(tool.process.authority.read_argument_paths));
        assert_eq!(value["write"]["argument_paths"], json!([]));
        assert!(value.get("read_env_paths").is_none());
        door.interface_driver_config.as_mut().unwrap().host_slots.get_mut("registry_tool_root").unwrap().path = "relative/tools".into();
        assert!(tool_path_scope("file.read", &tool, &door).is_err());
    }
}
