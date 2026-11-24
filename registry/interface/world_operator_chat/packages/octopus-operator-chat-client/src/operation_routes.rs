use super::*;

pub(crate) struct OperationRoute {
    pub(crate) catalog: String,
    pub(crate) request_label: String,
    pub(crate) input_labels: Vec<String>,
    pub(crate) result_labels: BTreeSet<String>,
    pub(crate) effective_operations: Vec<String>,
    pub(crate) attach_verifier: bool,
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
        topology: &FormedCapabilityTopology,
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
                if input.allowed_operations.is_some() && connected.contains(input) {
                    return Err(format!(
                        "external input '{}' specifies allowed_operations for non-operation interface '{}'",
                        input.checkpoint_id, input.interface_id
                    ));
                }
                continue;
            };
            routes.declared.insert(input.interface_id.clone());
            if !connected.contains(input) || routes.connected.contains_key(&input.interface_id) {
                continue;
            }
            let route = load_route(registry_root, input, config, topology)?;
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

    pub(crate) fn standing_input_labels(&self, inputs: &[ExternalInputSeed]) -> Result<Vec<String>, String> {
        let mut labels = BTreeSet::new();
        for input in inputs {
            if let Some(AuthoredInterfaceBinding {
                config: InterfaceBindingConfig { artifact_policy: BindingArtifactPolicy::Allow { names, .. } },
                ..
            }) = self.standing_input(input)?
            {
                labels.extend(names);
            }
        }
        Ok(labels.into_iter().collect())
    }

    pub(crate) fn catalog_bindings(&self) -> impl Iterator<Item = ArtifactBindingSeed> + '_ {
        self.connected
            .values()
            .map(|route| ArtifactBindingSeed { id: route.catalog.clone() })
    }
}

fn load_route(
    registry_root: &Path,
    input: &ExternalInputSeed,
    config: &serde_json::Map<String, serde_json::Value>,
    topology: &FormedCapabilityTopology,
) -> Result<OperationRoute, String> {
    let interface = input.interface_id.as_str();
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
    let mut catalog_ops = Vec::with_capacity(operations.len());
    let mut catalog_index = BTreeMap::new();
    let mut all_result_labels = BTreeSet::new();
    for operation in operations {
        let name = text(&operation["name"]).ok_or_else(|| reconnect("has an operation without a valid name"))?;
        if text(&operation["request_label"]).as_deref() != Some(request_label.as_str()) {
            return Err(reconnect("has an operation catalog with a foreign request label"));
        }
        let output_label = text(&operation["output_label"]).ok_or_else(|| reconnect("has an operation without a result label"))?;
        if catalog_index.insert(name.clone(), output_label.clone()).is_some() {
            return Err(reconnect("has duplicate operation names in its catalog"));
        }
        all_result_labels.insert(output_label.clone());
        catalog_ops.push((name, output_label));
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
        || input_labels.iter().any(|label| all_result_labels.contains(label))
    {
        return Err(reconnect("has an operation catalog that differs from its descriptor"));
    }
    let policy_context = topology
        .interface_policies
        .get(&InterfaceId::new(interface))
        .map(|policy| &policy.context);
    let policy_allowed = parse_policy_operation_set(policy_context, "allowed_operations", interface)?;
    let policy_denied = parse_policy_operation_set(policy_context, "denied_operations", interface)?;
    let policy_narrowed = policy_allowed.is_some() || policy_denied.is_some();
    let policy_effective = catalog_ops
        .iter()
        .filter(|(name, _)| {
            policy_allowed
                .as_ref()
                .is_none_or(|allowed| allowed.contains(name))
                && !policy_denied
                    .as_ref()
                    .is_some_and(|denied| denied.contains(name))
        })
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>();
    let policy_effective_set = policy_effective.iter().map(String::as_str).collect::<BTreeSet<_>>();
    let effective_operations = match &input.allowed_operations {
        Some(requested) => {
            if requested.is_empty() {
                return Err(format!(
                    "external input '{}' allowed_operations must not be empty",
                    input.checkpoint_id
                ));
            }
            let mut requested_set = BTreeSet::new();
            for operation in requested {
                if operation.is_empty() || operation.trim() != operation {
                    return Err(format!(
                        "external input '{}' allowed_operations contains malformed operation '{operation}'",
                        input.checkpoint_id
                    ));
                }
                if !requested_set.insert(operation.as_str()) {
                    return Err(format!(
                        "external input '{}' allowed_operations repeats '{operation}'",
                        input.checkpoint_id
                    ));
                }
                if !catalog_index.contains_key(operation) {
                    return Err(format!(
                        "external input '{}' requests unknown operation '{operation}' not in application interface '{interface}' catalog",
                        input.checkpoint_id
                    ));
                }
                if !policy_effective_set.contains(operation.as_str()) {
                    return Err(format!(
                        "external input '{}' requests operation '{operation}' denied by formed interface_policy on '{interface}'",
                        input.checkpoint_id
                    ));
                }
            }
            catalog_ops
                .iter()
                .filter(|(name, _)| requested_set.contains(name.as_str()))
                .map(|(name, _)| name.clone())
                .collect::<Vec<_>>()
        }
        None => {
            if policy_effective.is_empty() {
                return Err(format!(
                    "application interface '{interface}' has no permitted operations under formed interface_policy"
                ));
            }
            policy_effective
        }
    };
    let has_mark = topology
        .granted_mark_outcomes
        .contains(&MarkId::new(octopus_run_authoring::OPERATION_CALL_MARK));
    if (input.allowed_operations.is_some() || policy_narrowed) && !has_mark {
        return Err(format!(
            "application interface '{interface}' operation allowlist requires mark '{}' in the worker grant",
            octopus_run_authoring::OPERATION_CALL_MARK
        ));
    }
    let result_labels = effective_operations
        .iter()
        .map(|operation| catalog_index[operation].clone())
        .collect::<BTreeSet<_>>();
    Ok(OperationRoute {
        catalog,
        request_label,
        input_labels,
        result_labels,
        effective_operations,
        attach_verifier: has_mark,
    })
}

fn parse_policy_operation_set(
    policy_context: Option<&BTreeMap<String, serde_json::Value>>,
    key: &str,
    interface: &str,
) -> Result<Option<BTreeSet<String>>, String> {
    let Some(value) = policy_context.and_then(|values| values.get(key)) else {
        return Ok(None);
    };
    let entries = value.as_array().ok_or_else(|| {
        format!("formed interface_policy.{key} on '{interface}' must be an array of strings")
    })?;
    let mut set = BTreeSet::new();
    for entry in entries {
        let name = entry
            .as_str()
            .filter(|text| !text.is_empty() && text.trim() == *text)
            .ok_or_else(|| {
                format!("formed interface_policy.{key} on '{interface}' contains a malformed operation id")
            })?;
        set.insert(name.to_string());
    }
    Ok(Some(set))
}
