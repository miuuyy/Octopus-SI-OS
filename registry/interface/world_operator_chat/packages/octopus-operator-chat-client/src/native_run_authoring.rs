use super::*;
use octopus_ledger::{ArtifactRef, ContentHash};
use serde::Deserialize;
use serde_json::{Value, json};


#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Profile {
    catalog: CatalogTemplate,
    file_write_tool: String,
    file_read_tool: Option<String>,
    artifact_send_tool: String,
    read_root_slots: Vec<String>,
    output_selection: run_composition::CompositionOutputSelection,
    return_to_source: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogTemplate {
    label: String,
    text: String,
    operations: Vec<Value>,
}

pub(super) struct PlanContext<'a> {
    pub registry_root: &'a Path,
    pub ee_instances_root: &'a Path,
    pub run_records_root: Option<&'a Path>,
    pub interface_private_state_root: Option<&'a Path>,
    pub config: &'a ClientConfig,
    pub identity: &'a PlannedIdentity,
    pub provider: &'a ResolvedProvider,
    pub topology: &'a FormedCapabilityTopology,
    pub door_host_slots: &'a BTreeMap<String, BTreeMap<String, PathBuf>>,
}

pub(super) fn plan(
    context: PlanContext<'_>,
    writes: &mut Vec<PlannedClientWrite>,
) -> Result<Option<ArtifactBindingSeed>, String> {
    let Some(raw) = context
        .provider
        .interface
        .interface_driver_config
        .as_ref()
        .and_then(|config| config.private_config.get("run_affordance_profile"))
    else {
        return Ok(None);
    };
    let profile = parse_profile(raw)?;
    let identity = context.identity;
    let provider_tag = format!("interface:{}", identity.model_interface_id);
    let provider_index = retain_write(
        writes,
        context.registry_root.join("interface").join(&identity.model_interface_id).join("interface.json"),
        provider_tag,
    )?;
    let mut provider: Value = serde_json::from_str(&writes[provider_index].content).map_err(|error| error.to_string())?;
    let private = provider.pointer_mut("/interface_driver_config/private_config")
        .and_then(Value::as_object_mut).ok_or("planned provider private_config must be an object")?;
    private.remove("run_affordances");
    let (Some(writer), Some(sender)) = (
        identity.capability_tools.get(&profile.file_write_tool),
        identity.capability_tools.get(&profile.artifact_send_tool),
    ) else {
        writes[provider_index].content = pretty_json(&provider)?;
        return Ok(None);
    };
    if writer == &identity.authoring_tool_id {
        writes[provider_index].content = pretty_json(&provider)?;
        return Ok(None);
    }
    let reader = profile.file_read_tool.as_ref().and_then(|tool| identity.capability_tools.get(tool));
    let required_root = |root: Option<&Path>, label: &str| -> Result<PathBuf, String> {
        root.filter(|path| path.is_absolute()).map(Path::to_path_buf)
            .ok_or_else(|| format!("native run authoring requires explicit absolute {label}"))
    };
    let roots = run_composition::CompositionRoots {
        registry_root: required_root(Some(context.registry_root), "registry_root")?,
        ee_instances_root: required_root(Some(context.ee_instances_root), "ee_instances_root")?,
        run_records_root: required_root(context.run_records_root, "run_records_root")?,
        interface_private_state_root: required_root(context.interface_private_state_root, "interface_private_state_root")?,
    };
    let life_index = retain_write(writes,
        context.registry_root.join("interface").join(&identity.model_interface_id).join("life.json"),
        format!("life:{}", identity.model_interface_id))?;
    let mut life: SpawnLifeConfig = serde_json::from_str(&writes[life_index].content).map_err(|error| error.to_string())?;
    for slot in &profile.read_root_slots {
        for process in &mut life.processes { process.authority.read_root_slots.insert(slot.clone()); }
    }
    writes[life_index].content = pretty_json(&serde_json::to_value(life).map_err(|error| error.to_string())?)?;
    let catalog_id = format!("artifact-run-affordances-{}", identity.chat_id);
    let tools = context.topology.tool_bindings.iter().flat_map(|(interface, binding)| {
        binding.tools.iter().map(|tool| SelectedToolSeed {
            tool: tool.tool.as_str().to_owned(), interface: interface.as_str().to_owned(),
            worker_environment_kind: tool.worker_environment_kind.as_str().to_owned(),
        })
    }).collect::<Vec<_>>();
    let config = context.config;
    let defaults = json!({
        "worker_template_interface_id":identity.worker_choice_interface,
        "provider_interface_id":identity.provider_choice_interface,"model":context.provider.model,
        "reasoning_effort":context.provider.reasoning_effort,"service_tier":context.provider.service_tier,
        "authoring":{
            "source_interface_id":config.authoring.source_interface,"room_interface_id":config.authoring.room_interface,
            "tool_id":config.authoring.tool,"tool_call_label":config.artifact_labels.tool_call,
            "tool_call_mark":config.run.tool_call_mark,"tool_call_verifier":config.run.tool_call_verifier,
        },
        "worker_environment_kinds":context.topology.granted_worker_environment_kinds,
        "tools":tools,"door_host_slots":context.door_host_slots,"context_artifacts":[],
        "return_interface_ids":[],
    });
    let (affordances, catalog) = render(&profile, &catalog_id, json!({
        "file_write_tool":writer,"file_read_tool":reader,"artifact_send_tool":sender,"roots":roots,"defaults":defaults,
    }), identity.owns_client().then_some(config.interface.as_str()))?;
    private.insert("run_affordances".into(), affordances);
    writes[provider_index].content = pretty_json(&provider)?;
    push_json_write(context.registry_root.join("artifact").join(&catalog_id).join("artifact.json"),
        pretty_json(&catalog)?, format!("artifact:{catalog_id}"), writes)?;
    Ok(Some(ArtifactBindingSeed { id: catalog_id }))
}

fn parse_profile(raw: &Value) -> Result<Profile, String> {
    let profile: Profile = serde_json::from_value(raw.clone())
        .map_err(|error| format!("provider run_affordance_profile: {error}"))?;
    for (field, value) in [
        ("file_write_tool", &profile.file_write_tool),
        ("artifact_send_tool", &profile.artifact_send_tool),
        ("catalog.label", &profile.catalog.label),
        ("catalog.text", &profile.catalog.text),
    ] {
        if value.is_empty() || value.trim() != value {
            return Err(format!(
                "run_affordance_profile.{field} must be nonempty and trimmed"
            ));
        }
    }
    if profile.file_write_tool == profile.artifact_send_tool
        || profile.catalog.operations.is_empty()
    {
        return Err(
            "run_affordance_profile requires distinct effect tools and a nonempty catalog".into(),
        );
    }
    if let Some(reader) = &profile.file_read_tool {
        if reader.is_empty() || reader.trim() != reader
            || reader == &profile.file_write_tool || reader == &profile.artifact_send_tool
        {
            return Err("run_affordance_profile.file_read_tool must name a distinct nonempty tool".into());
        }
    }
    if profile.catalog.operations.iter().any(current_run_operation) && profile.file_read_tool.is_none() {
        return Err("current-run authoring catalog requires an explicit file_read_tool".into());
    }
    let mut slots = BTreeSet::new();
    for slot in &profile.read_root_slots {
        safe_segment(slot, "native authoring read root slot")?;
        if !slots.insert(slot) {
            return Err("native authoring repeats a read root slot".into());
        }
    }
    Ok(profile)
}

fn current_run_operation(operation: &Value) -> bool {
    matches!(operation.get("operation").and_then(Value::as_str), Some("preview_current_run" | "amend_current_run"))
}

fn render(profile: &Profile, catalog_id: &str, mut affordances: Value, client_template: Option<&str>) -> Result<(Value, Value), String> {
    let reader = affordances.get("file_read_tool").filter(|value| !value.is_null());
    let operations = profile.catalog.operations.iter()
        .filter(|operation| reader.is_some() || !current_run_operation(operation))
        .collect::<Vec<_>>();
    let catalog = json!({
        "id":catalog_id,"label":profile.catalog.label,"text":profile.catalog.text,
        "operations":operations,
    });
    let reference = ArtifactRef {
        id: catalog_id.into(),
        content: ContentHash::of_bytes(&octopus_descriptors::canonical_bytes(&catalog)),
    };
    let fields = affordances.as_object_mut().ok_or("native affordances must be an object")?;
    fields.insert("catalog_artifact".into(), json!(reference));
    let defaults = fields.get_mut("defaults").and_then(Value::as_object_mut).ok_or("native defaults must be an object")?;
    if !defaults.contains_key("client_template_interface_id") {
        defaults.insert("client_template_interface_id".into(), json!(client_template));
    }
    defaults.entry("output_selection").or_insert_with(||json!(profile.output_selection));
    defaults.entry("return_to_source").or_insert_with(||json!(profile.return_to_source));
    Ok((affordances, catalog))
}

fn retain_write(
    writes: &mut Vec<PlannedClientWrite>,
    path: PathBuf,
    tag: String,
) -> Result<usize, String> {
    if let Some(index) = writes
        .iter()
        .position(|write| write.tag == tag && write.path == path)
    {
        return Ok(index);
    }
    let content = std::fs::read_to_string(&path).map_err(|error| {
        format!(
            "native authoring owner '{}' is unavailable: {error}",
            path.display()
        )
    })?;
    serde_json::from_str::<Value>(&content).map_err(|error| error.to_string())?;
    let index = writes.len();
    writes.push(PlannedClientWrite {
        path,
        expected_before_hash: Some(sha256_hex(content.as_bytes())),
        content,
        tag,
    });
    Ok(index)
}
