use super::*;

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InterfaceOperationCatalogArtifact {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) interface: InterfaceId,
    pub(crate) request_label: String,
    #[serde(default)]
    pub(crate) task_input_labels: BTreeSet<String>,
    pub(crate) operations: Vec<InterfaceOperationCatalogEntry>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InterfaceOperationCatalogEntry {
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) input_schema: serde_json::Value,
    pub(crate) request_label: String,
    pub(crate) output_label: String,
}

pub(crate) fn extract_interface_operation_affordances(
    input: &InterfaceInvocationEnvelope,
    names: &mut BTreeSet<String>,
    specs: &mut Vec<DynamicToolSpec>,
) -> Result<BTreeMap<String, InterfaceOperationAffordance>, InterfaceDriverError> {
    let allowed_interfaces = input
        .metadata()
        .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?
        .allowed_interfaces()
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let mut interface_operations = BTreeMap::new();
    let mut catalog_interfaces = BTreeSet::new();
    for bound in &input.artifact_binding_payloads {
        let bytes = bound.read_bytes()
            .map_err(|error| InterfaceDriverError::FailedClosed(error.to_string()))?;
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes.as_ref()) else {
            continue;
        };
        let Some(object) = value.as_object() else {
            continue;
        };
        if !(object.contains_key("id")
            && object.contains_key("label")
            && object.contains_key("interface")
            && object.contains_key("request_label")
            && object.contains_key("operations"))
        {
            continue;
        }
        let catalog: InterfaceOperationCatalogArtifact =
            serde_json::from_value(value).map_err(|error| {
                InterfaceDriverError::Protocol(format!(
                    "bound interface operation catalog '{}' is malformed: {error}",
                    bound.artifact.id.as_str()
                ))
            })?;
        for (field, value) in [
            ("id", catalog.id.as_str()),
            ("label", catalog.label.as_str()),
            ("request_label", catalog.request_label.as_str()),
        ] {
            if value.trim().is_empty() || value.trim() != value {
                return Err(InterfaceDriverError::Protocol(format!(
                    "bound interface operation catalog '{}' has malformed {field}",
                    bound.artifact.id.as_str()
                )));
            }
        }
        if catalog.id != bound.artifact.id.as_str() {
            return Err(InterfaceDriverError::FailedClosed(format!(
                "bound interface operation catalog '{}' self-identifies as '{}'",
                bound.artifact.id.as_str(),
                catalog.id
            )));
        }
        if !allowed_interfaces.contains(catalog.interface.as_str()) {
            return Err(InterfaceDriverError::FailedClosed(format!(
                "bound interface operation catalog '{}' names interface '{}' outside the formed grant",
                bound.artifact.id.as_str(),
                catalog.interface.as_str()
            )));
        }
        if !catalog_interfaces.insert(catalog.interface.clone()) {
            return Err(InterfaceDriverError::FailedClosed(format!(
                "formed context repeats an operation catalog for exact interface '{}'",
                catalog.interface.as_str()
            )));
        }
        if catalog.operations.is_empty() {
            return Err(InterfaceDriverError::Protocol(format!(
                "bound interface operation catalog '{}' is empty",
                bound.artifact.id.as_str()
            )));
        }
        for operation in catalog.operations {
            for (field, value) in [
                ("name", operation.name.as_str()),
                ("description", operation.description.as_str()),
                ("request_label", operation.request_label.as_str()),
                ("output_label", operation.output_label.as_str()),
            ] {
                if value.trim().is_empty() || value.trim() != value {
                    return Err(InterfaceDriverError::Protocol(format!(
                        "bound interface operation catalog '{}' has an operation with malformed {field}",
                        bound.artifact.id.as_str()
                    )));
                }
            }
            if catalog.task_input_labels.contains(&operation.output_label)
                || catalog.task_input_labels.iter().any(|label| label.trim().is_empty() || label.trim() != label)
            {
                return Err(InterfaceDriverError::Protocol(format!(
                    "bound interface operation catalog '{}' declares a malformed task input label or one that is also an operation result",
                    bound.artifact.id.as_str()
                )));
            }
            if operation.request_label != catalog.request_label {
                return Err(InterfaceDriverError::Protocol(format!(
                    "bound interface operation '{}' request label '{}' differs from catalog label '{}'",
                    operation.name, operation.request_label, catalog.request_label
                )));
            }
            if !operation.input_schema.is_object() {
                return Err(InterfaceDriverError::Protocol(format!(
                    "bound interface operation '{}' input_schema is not an object",
                    operation.name
                )));
            }
            if !names.insert(operation.name.clone()) {
                return Err(InterfaceDriverError::FailedClosed(format!(
                    "model-visible capability name '{}' is declared by more than one formed surface",
                    operation.name
                )));
            }
            specs.push(DynamicToolSpec::Function(DynamicToolFunctionSpec {
                name: encode_tool_name(&operation.name),
                description: operation.description,
                input_schema: operation.input_schema,
                defer_loading: false,
            }));
            let name = operation.name.clone();
            if interface_operations
                .insert(
                    name.clone(),
                    InterfaceOperationAffordance {
                        name,
                        interface: catalog.interface.clone(),
                        request_label: operation.request_label,
                        output_label: operation.output_label,
                        input_labels: catalog.task_input_labels.clone(),
                    },
                )
                .is_some()
            {
                return Err(InterfaceDriverError::FailedClosed(format!(
                    "formed interface operation '{}' has more than one exact owner",
                    operation.name
                )));
            }
        }
    }
    Ok(interface_operations)
}

pub(crate) fn extract_native_affordance_catalog(
    input: &InterfaceInvocationEnvelope,
    hidden_tools: &[String],
    namespace: &NativeToolNamespace,
    run_affordances: Option<&RunAffordances>,
    private_config: &serde_json::Map<String, serde_json::Value>,
) -> Result<NativeAffordanceCatalog, InterfaceDriverError> {
    let mut specs = extract_native_affordances_from_invocation(input, hidden_tools)?;
    let mut names = input
        .interface_affordance_specs()?
        .iter()
        .map(|spec| spec.name.as_str().to_string())
        .collect::<BTreeSet<_>>();
    let interface_operations = extract_interface_operation_affordances(input, &mut names, &mut specs)?;
    if let Some(affordances) = run_affordances {
        for affordance in affordances.affordances() {
            if !names.insert(affordance.name.as_str().to_string()) {
                return Err(InterfaceDriverError::FailedClosed("run affordance duplicates another formed tool name".into()));
            }
            specs.push(DynamicToolSpec::Function(DynamicToolFunctionSpec {
                name: encode_tool_name(affordance.name.as_str()), description: affordance.description,
                input_schema: affordance.args_schema, defer_loading: false,
            }));
        }
    }
    let tool_names = apply_model_tool_names(&mut specs, private_config)?;
    let specs = namespace_native_affordances(specs, namespace)?;
    Ok(NativeAffordanceCatalog {
        specs,
        tool_names,
        interface_operations,
    })
}

pub(crate) fn apply_model_tool_names(
    specs: &mut [DynamicToolSpec],
    private_config: &serde_json::Map<String, serde_json::Value>,
) -> Result<BTreeMap<String, String>, InterfaceDriverError> {
    let aliases: BTreeMap<String, String> = match private_config.get("model_tool_names") {
        Some(value) => serde_json::from_value(value.clone()).map_err(|error| InterfaceDriverError::Protocol(format!("model_tool_names is malformed: {error}")))?,
        None => BTreeMap::new(),
    };
    for (exact, name) in &aliases {
        if exact.trim().is_empty() || exact.trim() != exact || name.is_empty()
            || !name.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_') {
            return Err(InterfaceDriverError::Protocol("model_tool_names requires exact tool ids and nonempty alphanumeric/underscore names".into()));
        }
    }
    let mut names = BTreeMap::new();
    for spec in specs {
        let DynamicToolSpec::Function(function) = spec else {
            return Err(InterfaceDriverError::Protocol("model names must be formed before namespaces".into()));
        };
        tool_schema::make_literal_types_explicit(&mut function.input_schema);
        let exact = decode_tool_name(&function.name)?;
        if let Some(name) = aliases.get(&exact) {
            function.name = name.clone();
        }
        if names.insert(function.name.clone(), exact).is_some() {
            return Err(InterfaceDriverError::FailedClosed(format!("model tool name '{}' has more than one admitted target", function.name)));
        }
    }
    Ok(names)
}

pub(crate) fn instruction_artifact_selection(
    config: &serde_json::Map<String, serde_json::Value>,
) -> Result<Vec<String>, InterfaceDriverError> {
    let mut selected = Vec::new();
    let mut seen = BTreeSet::new();
    for key in ["worker_instruction_artifacts", "instruction_artifacts"] {
        let value = config.get(key).ok_or_else(|| InterfaceDriverError::Protocol(format!("provider descriptor declares no {key}")))?;
        let ids: Vec<String> = serde_json::from_value(value.clone())
            .map_err(|error| InterfaceDriverError::Protocol(format!("{key} is malformed: {error}")))?;
        let mut local = BTreeSet::new();
        for id in ids {
            if id.trim().is_empty() || id.trim() != id || !local.insert(id.clone()) {
                return Err(InterfaceDriverError::Protocol(format!("{key} requires unique nonempty artifact ids")));
            }
            if seen.insert(id.clone()) { selected.push(id); }
        }
    }
    Ok(selected)
}

pub(crate) fn namespace_native_affordances(
    specs: Vec<DynamicToolSpec>,
    namespace: &NativeToolNamespace,
) -> Result<Vec<DynamicToolSpec>, InterfaceDriverError> {
    if specs.is_empty() {
        return Ok(Vec::new());
    }
    let tools = specs
        .into_iter()
        .map(|spec| match spec {
            DynamicToolSpec::Function(function) => Ok(DynamicToolNamespaceTool::Function(function)),
            DynamicToolSpec::Namespace(_) => Err(InterfaceDriverError::FailedClosed(
                "formed interface affordances already contain a provider namespace".to_string(),
            )),
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(vec![DynamicToolSpec::Namespace(DynamicToolNamespaceSpec {
        name: namespace.name.clone(),
        description: namespace.description.clone(),
        tools,
    })])
}

pub(crate) fn read_codex_context_generation(
    record: &serde_json::Value,
    interface: &octopus_descriptors::InterfaceId,
) -> Result<u64, InterfaceDriverError> {
    let object = record.as_object().ok_or_else(|| {
        InterfaceDriverError::Protocol("codex interface run record must be a JSON object".into())
    })?;
    let Some(interface_state) = object.get("interface_state") else {
        return Ok(0);
    };
    let interface_state = interface_state.as_object().ok_or_else(|| {
        InterfaceDriverError::Protocol(
            "codex interface run record interface_state must be a JSON object".into(),
        )
    })?;
    let Some(state) = interface_state.get(interface.as_str()) else {
        return Ok(0);
    };
    let state = state.as_object().ok_or_else(|| {
        InterfaceDriverError::Protocol(format!(
            "codex interface run record interface_state.{} must be a JSON object",
            interface.as_str()
        ))
    })?;
    let Some(generation) = state.get("context_generation") else {
        return Ok(0);
    };
    generation.as_u64().ok_or_else(|| {
        InterfaceDriverError::Protocol(format!(
            "codex interface run record interface_state.{}.context_generation must be an unsigned integer",
            interface.as_str()
        ))
    })
}

pub(crate) fn invocation_runtime_metadata(
    input: &InterfaceInvocationEnvelope,
) -> Result<&RuntimeInvocationMetadata, InterfaceDriverError> {
    input
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.runtime.as_ref())
        .ok_or_else(|| {
            InterfaceDriverError::Protocol(
                "codex interface requires runtime metadata beside the artifact".into(),
            )
        })
}

pub(crate) fn parse_run_record_json(
    bytes: &[u8],
    expected_run: &str,
) -> Result<serde_json::Value, InterfaceDriverError> {
    let record: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| {
        InterfaceDriverError::Protocol(format!(
            "codex interface pinned run record is not JSON: {e}"
        ))
    })?;
    let observed_run = record
        .get("id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            InterfaceDriverError::Protocol(
                "codex interface pinned run record has no string id".to_string(),
            )
        })?;
    if observed_run != expected_run {
        return Err(InterfaceDriverError::Protocol(format!(
            "codex interface pinned run record belongs to '{}', not '{}'",
            observed_run, expected_run
        )));
    }
    Ok(record)
}

#[cfg(test)]
pub(crate) fn source_interface_for_checkpoint(
    record: &serde_json::Value,
    checkpoint_id: &str,
) -> Result<InterfaceId, InterfaceDriverError> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct CanonicalInterfaceBinding {
        interface: InterfaceId,
        #[serde(rename = "config")]
        _config: serde_json::Map<String, serde_json::Value>,
    }

    let active = record
        .get("active_checkpoints")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            InterfaceDriverError::Protocol(
                "codex interface pinned run record has no active_checkpoints array".to_string(),
            )
        })?;
    if !active
        .iter()
        .any(|value| value.as_str() == Some(checkpoint_id))
    {
        return Err(InterfaceDriverError::FailedClosed(format!(
            "codex invocation checkpoint '{checkpoint_id}' is not active in the pinned run"
        )));
    }
    let checkpoints = record
        .get("checkpoints")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            InterfaceDriverError::Protocol(
                "codex interface pinned run record has no checkpoints array".to_string(),
            )
        })?;
    let matches = checkpoints
        .iter()
        .filter(|checkpoint| {
            checkpoint.get("id").and_then(serde_json::Value::as_str) == Some(checkpoint_id)
        })
        .collect::<Vec<_>>();
    let [checkpoint] = matches.as_slice() else {
        return Err(InterfaceDriverError::FailedClosed(format!(
            "codex pinned run contains {} checkpoints named '{checkpoint_id}'",
            matches.len()
        )));
    };
    let binding = checkpoint
        .get("interface_binding")
        .cloned()
        .ok_or_else(|| {
            InterfaceDriverError::Protocol(format!(
                "codex invocation checkpoint '{checkpoint_id}' has no exact interface binding"
            ))
        })?;
    let binding: CanonicalInterfaceBinding = serde_json::from_value(binding).map_err(|error| {
        InterfaceDriverError::Protocol(format!(
            "codex invocation checkpoint '{checkpoint_id}' has a malformed canonical interface binding: {error}"
        ))
    })?;
    let interface = binding.interface.as_str();
    if interface.trim().is_empty() || interface.trim() != interface {
        return Err(InterfaceDriverError::Protocol(format!(
            "codex invocation checkpoint '{checkpoint_id}' has no exact interface binding"
        )));
    }
    Ok(binding.interface)
}
