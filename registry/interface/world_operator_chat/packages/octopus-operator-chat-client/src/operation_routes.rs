use super::*;

pub(crate) struct OperationRoute {
    pub(crate) catalog: String,
    pub(crate) request_label: String,
    pub(crate) input_labels: Vec<String>,
    pub(crate) result_labels: BTreeSet<String>,
}

#[derive(Default)]
pub(crate) struct OperationRoutes {
    pub(crate) declared: BTreeSet<String>,
    pub(crate) connected: BTreeMap<String, OperationRoute>,
}

pub(crate) fn operation_catalog_artifact_id(interface: &str) -> String {
    format!("artifact-operation-catalog-{interface}")
}

impl OperationRoutes {
    pub(crate) fn load<'a>(
        registry_root: &Path,
        connected: &'a [ExternalInputSeed],
        previous: impl IntoIterator<Item = &'a ExternalInputSeed>,
    ) -> Result<Self, String> {
        let interface_root = registry_root.join("interface");
        let mut routes = Self::default();
        for input in connected.iter().chain(previous) {
            let descriptor = load_interface_descriptor(&interface_root, &input.interface_id)?;
            let Some(config) = descriptor
                .interface_driver_config
                .as_ref()
                .map(|config| &config.private_config)
                .filter(|config| config.contains_key("operation_catalog_dir"))
            else {
                continue;
            };
            routes.declared.insert(input.interface_id.clone());
            if !connected.contains(input) || routes.connected.contains_key(&input.interface_id) {
                continue;
            }
            let route = load_route(registry_root, &input.interface_id, config)?;
            routes.connected.insert(input.interface_id.clone(), route);
        }
        Ok(routes)
    }

    pub(crate) fn standing_input(
        &self,
        input: &ExternalInputSeed,
    ) -> Result<Option<AuthoredInterfaceBinding>, String> {
        if !self.declared.contains(&input.interface_id) {
            return external_input_binding(input).map(Some);
        }
        if !input.artifact_names.is_empty() {
            return Err(format!(
                "external input '{}' narrows application interface '{}', whose task inputs are declared by its operation catalog",
                input.checkpoint_id, input.interface_id
            ));
        }
        match self.connected.get(&input.interface_id) {
            Some(route) if !route.input_labels.is_empty() => {
                operator_chat_input_binding_names(&input.interface_id, &route.input_labels).map(Some)
            }
            _ => Ok(None),
        }
    }

    pub(crate) fn catalog_bindings(&self) -> impl Iterator<Item = ArtifactBindingSeed> + '_ {
        self.connected
            .values()
            .map(|route| ArtifactBindingSeed { id: route.catalog.clone() })
    }
}

fn load_route(
    registry_root: &Path,
    interface: &str,
    config: &serde_json::Map<String, serde_json::Value>,
) -> Result<OperationRoute, String> {
    let catalog = operation_catalog_artifact_id(interface);
    let reconnect = |problem: &str| {
        format!("application interface '{interface}' {problem}; reconnect the application")
    };
    let path = registry_root.join("artifact").join(&catalog).join("artifact.json");
    let bytes = std::fs::read(&path).map_err(|_| reconnect("has no operation catalog"))?;
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| reconnect("has a malformed operation catalog"))?;
    let text = |value: &serde_json::Value| {
        value.as_str().filter(|text| !text.is_empty() && text.trim() == *text).map(str::to_string)
    };
    let request_label = text(&value["request_label"]).ok_or_else(|| reconnect("has a malformed operation catalog"))?;
    let operations = value["operations"].as_array().filter(|operations| !operations.is_empty())
        .ok_or_else(|| reconnect("has an empty operation catalog"))?;
    let mut result_labels = BTreeSet::new();
    for operation in operations {
        if text(&operation["request_label"]).as_deref() != Some(request_label.as_str()) {
            return Err(reconnect("has an operation catalog with a foreign request label"));
        }
        result_labels.insert(text(&operation["output_label"]).ok_or_else(|| reconnect("has an operation without a result label"))?);
    }
    let input_labels = match value.get("task_input_labels") {
        None => Vec::new(),
        Some(labels) => labels
            .as_array()
            .and_then(|labels| labels.iter().map(text).collect::<Option<Vec<_>>>())
            .ok_or_else(|| reconnect("has malformed task input labels"))?,
    };
    if text(&value["id"]).as_deref() != Some(catalog.as_str())
        || text(&value["interface"]).as_deref() != Some(interface)
        || config.get("operation_request_label").and_then(serde_json::Value::as_str) != Some(request_label.as_str())
        || input_labels.iter().any(|label| result_labels.contains(label))
    {
        return Err(reconnect("has an operation catalog that differs from its descriptor"));
    }
    Ok(OperationRoute { catalog, request_label, input_labels, result_labels })
}
