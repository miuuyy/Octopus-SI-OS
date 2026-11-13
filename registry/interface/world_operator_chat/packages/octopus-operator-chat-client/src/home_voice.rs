use super::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct HomeVoiceConfig {
    provider_interface_id: String,
    contract_file: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct HomeVoiceContext {
    pub control_interface_id: String,
    pub provider_id: String,
    pub delegation_interface_id: String,
    pub run_id: String,
}

pub(super) struct PlannedVoice {
    pub context: HomeVoiceContext,
    pub source: String,
    pub door: String,
    pub writer: String,
    inputs: Vec<String>,
    outputs: Vec<String>,
    retained: bool,
}

impl PlannedVoice {
    pub fn interfaces(&self) -> [&String; 4] {
        [
            &self.context.control_interface_id,
            &self.context.provider_id,
            &self.source,
            &self.door,
        ]
    }
}

pub(super) fn select(
    root: &Path,
    config: &ClientConfig,
    identity: &PlannedIdentity,
    topology: &FormedCapabilityTopology,
    existing: Option<&Value>,
) -> Result<Option<PlannedVoice>, String> {
    if identity.surface_role != "home" {
        return Ok(None);
    }
    let selected = config
        .home_voice
        .as_ref()
        .ok_or("Home requires a declared voice provider contract")?;
    safe_segment(&selected.provider_interface_id, "Home voice provider")?;
    let parts = Path::new(&selected.contract_file)
        .components()
        .collect::<Vec<_>>();
    if !matches!(parts.as_slice(), [std::path::Component::Normal(_)]) {
        return Err("Home voice contract must name one local file".into());
    }
    let context = HomeVoiceContext {
        control_interface_id: format!("{}_voice_{}", config.interface, identity.chat_id),
        provider_id: format!("{}_{}", selected.provider_interface_id, identity.chat_id),
        delegation_interface_id: home_outputs::interfaces(identity)[2].clone(),
        run_id: format!("{}_voice", identity.run_id),
    };
    if let Some(existing) = existing {
        let accepted: HomeVoiceContext = serde_json::from_value(
            existing["interface_state"][&identity.chat_interface_id]["voice_context"].clone(),
        ).map_err(|e| format!("retained Home has no exact voice context: {e}"))?;
        if accepted != context {
            return Err("retained Home voice differs from its declared source identity".into());
        }
        let sidecar: OctopusOsInterfaceSidecar = serde_json::from_slice(
            &std::fs::read(root.join(&context.provider_id).join("octopus_os.json")).map_err(|e| e.to_string())?,
        ).map_err(|e| e.to_string())?;
        if sidecar.schema_version != 1 || sidecar.workspace != "octopus_os"
            || sidecar.component != "provider" || sidecar.surface_role != "home"
            || sidecar.interface != context.provider_id || sidecar.owner_interface != identity.chat_interface_id
        {
            return Err("retained Home voice provider has a different exact owner".into());
        }
    }
    let provider_id = if existing.is_some() { &context.provider_id } else { &selected.provider_interface_id };
    if !topology.interface_policies.contains_key(&InterfaceId::new(provider_id)) {
        return Err("Home voice provider is absent from the formed exact interface policy".into());
    }
    let descriptor = load_interface_descriptor(root, provider_id)?;
    let source_descriptor = load_interface_descriptor(root, &selected.provider_interface_id)?;
    if descriptor.role.0.as_str() != "provider" || source_descriptor.role.0.as_str() != "provider"
        || descriptor.interface_driver_ref != source_descriptor.interface_driver_ref
    {
        return Err("Home voice selection must name a provider interface".into());
    }
    let contract: Value = serde_json::from_slice(
        &std::fs::read(
            root.join(&selected.provider_interface_id)
                .join(&selected.contract_file),
        )
        .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    if contract["schema_version"] != 1 || contract["owner"] != selected.provider_interface_id {
        return Err("Home voice contract has a mismatched owner or version".into());
    }
    let keys = |field: &str| -> Result<Vec<String>, String> {
        Ok(contract[field]
            .as_object()
            .ok_or_else(|| format!("Home voice contract has no {field}"))?
            .keys()
            .cloned()
            .collect())
    };
    let inputs = keys("inputs")?;
    let mut outputs = keys("outputs")?;
    if ![
        "realtime.session.offer",
        "realtime.session.update",
        "realtime.session.stop",
    ]
    .iter()
    .all(|label| inputs.contains(&label.to_string()))
        || !outputs.iter().any(|label| label == "realtime.delegation")
    {
        return Err(
            "Home voice contract lacks its explicit control or delegation vocabulary".into(),
        );
    }
    outputs.retain(|label| label != "realtime.delegation");
    Ok(Some(PlannedVoice {
        context,
        source: format!(
            "{}_voice_{}",
            config.authoring.source_interface, identity.chat_id
        ),
        door: format!(
            "{}_voice_authoring_{}",
            config.authoring.room_interface, identity.chat_id
        ),
        writer: format!(
            "{}__voice_authoring_{}",
            config.authoring.tool, identity.chat_id
        ),
        inputs,
        outputs,
        retained: existing.is_some(),
    }))
}

pub(super) fn plan_descriptors(
    registry: &Path,
    config: &ClientConfig,
    topology: &FormedCapabilityTopology,
    identity: &PlannedIdentity,
    writes: &mut Vec<PlannedClientWrite>,
) -> Result<(), String> {
    let Some(voice) = &identity.voice else {
        return Ok(());
    };
    if voice.retained {
        return Ok(());
    }
    let selected = config
        .home_voice
        .as_ref()
        .ok_or("Home voice selection is absent")?;
    let root = registry.join("interface");
    for (source, exact, component, activate) in [
        (
            config.interface.as_str(),
            voice.context.control_interface_id.as_str(),
            "voice_control",
            true,
        ),
        (
            config.authoring.source_interface.as_str(),
            voice.source.as_str(),
            "authoring_source",
            true,
        ),
        (
            config.authoring.room_interface.as_str(),
            voice.door.as_str(),
            "authoring_environment",
            false,
        ),
    ] {
        let descriptor = load_interface_descriptor(&root, source)?;
        push_json_write(
            root.join(exact).join("interface.json"),
            sibling_descriptor_bytes(&descriptor, exact, None, None)?,
            format!("interface:{exact}"),
            writes,
        )?;
        plan_interface_life(&root, source, exact, writes)?;
        plan_octopus_os_sidecar(
            &root, exact, identity, component, false, None, false, writes,
        )?;
        if activate {
            push_json_write(
                root.join(exact).join("activation.json"),
                pretty_json(&json!({
                    "schema_version":1,"interface":exact,"activations":[{"activation_driver_ref":"app_server_transport","config":{
                        "listen_env":format!("OCTOPUS_{}_LISTEN", exact.to_uppercase()),"bind_interface_socket_when_env_unset":true
                    }}]
                }))?,
                format!("activation:{exact}"),
                writes,
            )?;
        }
    }
    let binding = topology
        .tool_bindings
        .get(&InterfaceId::new(&config.authoring.room_interface))
        .ok_or("Home voice writer has no formed room")?;
    plan_capability_sidecars(
        &root,
        &config.authoring.room_interface,
        &voice.door,
        binding,
        config,
        identity,
        writes,
    )?;
    let mut tool = load_tool_descriptor_from_dir(&registry.join("tool"), &config.authoring.tool)
        .map_err(|e| e.to_string())?
        .ok_or("Home voice file writer is absent")?;
    crate::copy_tool_descriptor(&mut tool, &voice.writer);
    tool.interface = InterfaceId::new(&voice.door);
    narrow_run_authoring_tool_authority(&mut tool)?;
    push_json_write(
        registry.join("tool").join(&voice.writer).join("tool.json"),
        pretty_json(&json!(tool))?,
        format!("tool:{}", voice.writer),
        writes,
    )?;

    let mut provider = load_interface_descriptor(&root, &selected.provider_interface_id)?;
    provider.interface_policy.values = topology
        .interface_policies
        .get(&provider.id)
        .ok_or("Home voice provider has no formed policy")?
        .context
        .clone();
    let mut driver = provider
        .interface_driver_config
        .clone()
        .ok_or("Home voice provider has no driver configuration")?;
    replace_provider_hidden_tools(
        &mut driver.private_config,
        &selected.provider_interface_id,
        &voice.writer,
        &BTreeMap::new(),
    )?;
    driver.private_config.insert("run_authoring_profile".into(), json!({
        "file_write_tool":voice.writer,"file_write_label":config.artifact_labels.tool_call,
        "file_write_result_label":format!("{}.result",voice.writer)
    }));
    push_json_write(
        root.join(&voice.context.provider_id).join("interface.json"),
        sibling_descriptor_bytes(&provider, &voice.context.provider_id, None, Some(&driver))?,
        format!("interface:{}", voice.context.provider_id),
        writes,
    )?;
    plan_interface_life(
        &root,
        &selected.provider_interface_id,
        &voice.context.provider_id,
        writes,
    )?;
    plan_octopus_os_sidecar(
        &root,
        &voice.context.provider_id,
        identity,
        "provider",
        false,
        None,
        false,
        writes,
    )?;
    let policy = writes
        .iter_mut()
        .find(|write| write.tag == format!("policy:{}", identity.worker_interface_id))
        .ok_or("Home voice has no exact worker policy")?;
    let mut value: Value = serde_json::from_str(&policy.content).map_err(|e| e.to_string())?;
    if let Some(room_policy) = value
        .pointer("/ceiling/interface_policy")
        .and_then(|policies| policies.get(&config.authoring.room_interface))
        .cloned()
    {
        value["ceiling"]["interface_policy"][&voice.door] = room_policy;
    }
    policy.content = pretty_json(&value)?;
    Ok(())
}

fn binding(interface: &str, names: &[String]) -> AuthoredInterfaceBinding {
    AuthoredInterfaceBinding {
        interface: InterfaceId::new(interface),
        config: InterfaceBindingConfig {
            artifact_policy: BindingArtifactPolicy::Allow {
                names: names.to_vec(),
                namespaces: Vec::new(),
            },
        },
    }
}

fn route(
    role: BoundCheckpointRole,
    source: &str,
    target: &str,
    names: &[String],
) -> Result<BoundCheckpointTemplate, String> {
    bound_checkpoint_template(
        role,
        binding(source, names),
        vec![binding(target, names)],
        Vec::new(),
    )
}

pub(super) fn wire(
    request: &OperatorChatPlanRequest,
    config: &ClientConfig,
    identity: &PlannedIdentity,
    roles: &[String],
    context: &[ArtifactBindingSeed],
    content: String,
) -> Result<(String, Vec<PlannedClientWrite>), String> {
    let Some(voice) = &identity.voice else {
        return Ok((content, Vec::new()));
    };
    let mut home: Value = serde_json::from_str(&content).map_err(|e| e.to_string())?;
    if home["interface_state"]
        .get(&voice.context.provider_id)
        .is_some()
    {
        return Err("Home and Voice still share execution state; explicitly repair their Run topology before continuing".into());
    }
    let role_bindings = roles.iter().map(|id| json!({"id":id})).collect::<Vec<_>>();
    let mut context_ids = roles.to_vec();
    for id in config
        .run
        .context_artifact_bindings
        .iter()
        .chain(context)
        .map(|binding| &binding.id)
    {
        if !context_ids.contains(id) {
            context_ids.push(id.clone());
        }
    }
    let text_provider_context = octopus_run_authoring::provider_context_bindings(
        &RunRecord::from_bytes(content.as_bytes()).map_err(|e| e.to_string())?,
        &InterfaceId::new(&identity.model_interface_id),
    )?
    .into_iter()
    .map(|binding| serde_json::to_value(binding).map_err(|e| e.to_string()))
    .collect::<Result<Vec<_>, _>>()?;
    let mut delegation = route(
        BoundCheckpointRole::Stable,
        &voice.context.delegation_interface_id,
        &identity.model_interface_id,
        &["realtime.delegation".into()],
    )?;
    delegation.static_artifact_bindings = context_ids
        .iter()
        .filter(|id| text_provider_context.iter().any(|binding| binding["id"].as_str() == Some(id.as_str())))
        .map(|id| octopus_descriptors::ArtifactId::new(id))
        .collect();
    let incoming = home["checkpoints"]
        .as_array_mut()
        .ok_or("Home has no checkpoints")?;
    let matching = incoming
        .iter()
        .filter(|cp| cp["interface_binding"] == json!(delegation.interface_binding))
        .collect::<Vec<_>>();
    if matching.len() > 1 {
        return Err("Home repeats its exact voice delegation input".into());
    }
    if let Some(cp) = matching.first() {
        if cp["outbound_interface_bindings"] != json!(delegation.outbound_interface_bindings) {
            return Err("Home voice delegation changed its declared recipient".into());
        }
    } else {
        append_checkpoint(
            &mut home,
            &identity.run_id,
            &delegation,
            &text_provider_context,
            &[],
        )?;
    }
    home["interface_state"][&identity.model_interface_id]["run_authoring_binding"]["checkpoints"]
        .as_array_mut()
        .ok_or("Home text provider has no topology")?
        .push(json!(delegation));

    let mut templates = vec![
        route(
            BoundCheckpointRole::Stable,
            &voice.context.control_interface_id,
            &voice.context.provider_id,
            &voice.inputs,
        )?,
        route(
            BoundCheckpointRole::ProviderArtifactOutput,
            &voice.context.provider_id,
            &voice.context.control_interface_id,
            &voice.outputs,
        )?,
        route(
            BoundCheckpointRole::ProviderInterfaceOutput,
            &voice.context.provider_id,
            &voice.context.delegation_interface_id,
            &["realtime.delegation".into()],
        )?,
        route(
            BoundCheckpointRole::IndependentInput,
            &home_outputs::interfaces(identity)[1],
            &voice.context.provider_id,
            &["realtime.session.update".into()],
        )?,
        bound_checkpoint_template(
            BoundCheckpointRole::AuthoringCall,
            AuthoredInterfaceBinding::all(InterfaceId::new(&voice.source)),
            vec![AuthoredInterfaceBinding::all(InterfaceId::new(&voice.door))],
            vec![
                serde_json::from_value(authoring_call_requirement(config, &voice.writer))
                    .map_err(|e| e.to_string())?,
            ],
        )?,
        bound_checkpoint_template(
            BoundCheckpointRole::AuthoringResult,
            AuthoredInterfaceBinding::all(InterfaceId::new(&voice.door)),
            vec![AuthoredInterfaceBinding::all(InterfaceId::new(
                &voice.context.provider_id,
            ))],
            Vec::new(),
        )?,
    ];
    let root = require_absolute(
        "run_records_root",
        request
            .run_records_root
            .as_deref()
            .ok_or("Home voice needs run_records_root")?,
    )?;
    let path = root.join(format!("{}.json", voice.context.run_id));
    if let Some(existing) = request.existing_run_record.as_ref() {
        if existing["interface_state"][&identity.chat_interface_id]["voice_context"]
            != json!(voice.context)
        {
            return Err(
                "Home voice execution binding changed; explicit topology repair is required".into(),
            );
        }
        let bytes = std::fs::read(&path)
            .map_err(|e| format!("declared Home voice Run is unreadable: {e}"))?;
        let record = RunRecord::from_bytes(&bytes).map_err(|e| e.to_string())?;
        let value: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        let bound: BoundRunAuthoringTopology = serde_json::from_value(
            value["interface_state"][&voice.context.provider_id]["run_authoring_binding"].clone(),
        )
        .map_err(|e| e.to_string())?;
        let static_seeds: Vec<octopus_descriptors::ArtifactId> = serde_json::from_value(
            value["interface_state"][&voice.context.control_interface_id]["static_artifact_bindings"].clone(),
        ).map_err(|e| format!("Home voice Run has no valid original static seeds; explicit topology repair is required: {e}"))?;
        if static_seeds.is_empty() {
            return Err("Home voice Run lacks its original static seeds; explicit topology repair is required".into());
        }
        let mut expected = templates;
        for cp in &mut expected {
            if cp.include_context {
                cp.static_artifact_bindings = static_seeds.clone();
            }
            if cp.role == BoundCheckpointRole::AuthoringCall {
                for requirement in &mut cp.required_verified_artifacts {
                    requirement.params["metadata_matches"][0]["payload"] =
                        json!("/arguments/writes/0/expected_before_hash");
                }
            }
        }
        if record.id.as_str() != voice.context.run_id
            || record.worker.as_str() != identity.worker_interface_id
            || bound.run != record.id
            || bound.worker != record.worker
            || bound.provider_interface.as_str() != voice.context.provider_id
            || bound.authoring_source_interface.as_str() != voice.source
            || bound.checkpoints != expected
            || bound.writer_run.is_none()
            || value["interface_state"][&voice.context.control_interface_id]
                != json!({"home_run":identity.run_id,"home_interface":identity.chat_interface_id,"static_artifact_bindings":static_seeds})
        {
            return Err(
                "Home voice Run differs from its exact independent execution contract".into(),
            );
        }
        octopus_run_authoring::provider_context_bindings(
            &record,
            &bound.provider_interface,
        )?;
        return Ok((pretty_json(&home)?, Vec::new()));
    }
    if path.try_exists().map_err(|e| e.to_string())? {
        return Err("Home voice Run already exists without its declared Home binding".into());
    }
    for template in &mut templates {
        if template.include_context {
            template.static_artifact_bindings = context_ids
                .iter()
                .map(|id| octopus_descriptors::ArtifactId::new(id))
                .collect();
        }
    }
    let mut record = json!({
        "id":voice.context.run_id,"ee":identity.ee_id,"worker":identity.worker_interface_id,
        "trace":format!("trace:{}",voice.context.run_id),"status":"idle",
        "interface_state":{&voice.context.control_interface_id:{"home_run":identity.run_id,"home_interface":identity.chat_interface_id,"static_artifact_bindings":context_ids}},
        "checkpoints":[],"active_checkpoints":[]
    });
    let mut frontier = Vec::new();
    for template in &templates {
        let id = append_checkpoint(
            &mut record,
            &voice.context.run_id,
            template,
            &role_bindings,
            &context_ids,
        )?;
        if !matches!(
            template.role,
            BoundCheckpointRole::Stable | BoundCheckpointRole::IndependentInput
        ) {
            frontier.push(id);
        }
    }
    record["interface_state"][&voice.context.provider_id] = json!({
        "context_generation":0,"authoring_generation":0,"frontier_checkpoint_ids":frontier,
        "run_authoring_binding":BoundRunAuthoringTopology {schema_version:2,run:octopus_descriptors::RunId::new(&voice.context.run_id),worker:InterfaceId::new(&identity.worker_interface_id),provider_interface:InterfaceId::new(&voice.context.provider_id),authoring_source_interface:InterfaceId::new(&voice.source),payload_artifact_bindings:Vec::new(),writer_run:None,checkpoints:templates}
    });
    let (content, mut writes) =
        writer_runs::wire_record(request, &voice.context.run_id, None, pretty_json(&record)?)?;
    RunRecord::from_bytes(content.as_bytes()).map_err(|e| e.to_string())?;
    writes.push(PlannedClientWrite {
        path,
        content,
        tag: format!("run:{}", voice.context.run_id),
        expected_before_hash: None,
    });
    Ok((pretty_json(&home)?, writes))
}

fn append_checkpoint(
    record: &mut Value,
    run_id: &str,
    template: &BoundCheckpointTemplate,
    roles: &[Value],
    context: &[String],
) -> Result<String, String> {
    let rows = record["checkpoints"]
        .as_array_mut()
        .ok_or("Home run has no checkpoints")?;
    let generation = rows.len();
    let id = format!("cp_home_voice_{}_{}", template.name, generation);
    let mut artifacts = roles.to_vec();
    let ids = if template.include_context {
        context.to_vec()
    } else {
        template
            .static_artifact_bindings
            .iter()
            .map(ToString::to_string)
            .collect()
    };
    for id in ids {
        if !artifacts.contains(&json!({"id":id})) {
            artifacts.push(json!({"id":id}));
        }
    }
    rows.push(json!({"id":id,"run":run_id,"trace":format!("trace:{run_id}:{id}"),"epoch":0,
        "interface_binding":template.interface_binding,"outbound_interface_bindings":template.outbound_interface_bindings,
        "artifact_bindings":artifacts,"required_verified_artifacts":template.required_verified_artifacts}));
    record["active_checkpoints"]
        .as_array_mut()
        .ok_or("Home run has no active checkpoints")?
        .push(json!(id));
    Ok(id)
}
