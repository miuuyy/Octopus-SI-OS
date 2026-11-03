use super::*;

pub(crate) fn resolve_provider(
    interface_root: &Path,
    provider_interface_id: &str,
    requested_model: &str,
    requested_reasoning_effort: &str,
    requested_service_tier: Option<&str>,
    topology: &FormedCapabilityTopology,
) -> Result<ResolvedProvider, String> {
    let provider_id = safe_segment(provider_interface_id, "provider_interface_id")?;
    let interface = load_interface_descriptor(interface_root, &provider_id)?;
    if !interface.has_interface_driver() {
        return Err(format!(
            "provider interface '{}' resolves no interface driver",
            provider_id
        ));
    }
    let resolved_config = interface
        .interface_driver_config
        .clone()
        .unwrap_or_default();
    let configured_model = resolved_config
        .private_config
        .get("model")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty() && value.trim() == *value)
        .ok_or_else(|| format!("{provider_id} declares no selected private_config.model"))?
        .to_string();
    let configured_reasoning_effort = resolved_config
        .private_config
        .get("reasoning_effort")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let configured_service_tier = match resolved_config.private_config.get("service_tier") {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(tier)) => Some(tier.clone()),
        Some(_) => return Err(format!("{provider_id} service tier must be a native tier id string")),
    };
    let service_tier = requested_service_tier.map(str::to_string).or(configured_service_tier);
    if service_tier.as_ref().is_some_and(|tier| tier.is_empty() || tier.trim() != tier || tier.len() > 80 || tier.chars().any(char::is_control)) {
        return Err(format!("{provider_id} service tier must be a non-empty native tier id"));
    }
    let formed_models = formed_provider_models(topology, &interface.id)?;
    let catalog = resolved_provider_model_catalog(
        &resolved_config,
        &interface.id,
        &formed_models,
        &configured_model,
        &configured_reasoning_effort,
    )?;
    let requested_model = requested_model.trim();
    let model = if requested_model.is_empty() {
        configured_model.clone()
    } else {
        if requested_model.contains(['\r', '\n', '\0']) {
            return Err(format!("{provider_id} requested model is malformed"));
        }
        requested_model.to_string()
    };
    let selected = catalog
        .iter()
        .find(|preset| preset.model == model)
        .ok_or_else(|| {
            format!("{provider_id} formed model catalog does not admit selected model '{model}'")
        })?;
    let requested_reasoning_effort = requested_reasoning_effort.trim();
    let reasoning_effort = if !requested_reasoning_effort.is_empty() {
        requested_reasoning_effort.to_string()
    } else if model == configured_model && !configured_reasoning_effort.is_empty() {
        configured_reasoning_effort
    } else {
        selected.default_reasoning_effort.clone()
    };
    if reasoning_effort.contains(['\r', '\n', '\0']) {
        return Err(format!(
            "{provider_id} requested reasoning effort is malformed"
        ));
    }
    if !selected.supported_reasoning_efforts.is_empty()
        && !selected
            .supported_reasoning_efforts
            .contains(&reasoning_effort)
    {
        return Err(format!(
            "{provider_id} model '{model}' does not support reasoning effort '{reasoning_effort}'"
        ));
    }
    if !reasoning_effort.is_empty()
        && !resolved_config.supported_reasoning_efforts.is_empty()
        && !resolved_config
            .supported_reasoning_efforts
            .contains(&reasoning_effort)
    {
        return Err(format!(
            "{provider_id} reasoning effort '{reasoning_effort}' is outside its interface ceiling"
        ));
    }
    let life = interface_root.join(interface.id.as_str()).join("life.json");
    if !life.is_file() {
        return Err(format!(
            "provider interface '{}' has no self-owned life declaration at '{}'",
            interface.id.as_str(),
            life.display()
        ));
    }
    Ok(ResolvedProvider {
        client_operation_request_labels: provider_client_operation_request_labels(
            &resolved_config.private_config,
            interface.id.as_str(),
        )?,
        interface,
        model,
        reasoning_effort,
        service_tier,
    })
}

pub(crate) fn formed_provider_models(
    topology: &FormedCapabilityTopology,
    provider: &InterfaceId,
) -> Result<BTreeSet<String>, String> {
    let policy = topology.interface_policies.get(provider).ok_or_else(|| {
        format!(
            "provider interface '{}' has no formed interface policy",
            provider.as_str()
        )
    })?;
    let values = policy
        .context
        .get("models")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            format!(
                "provider interface '{}' formed models must be a string array",
                provider.as_str()
            )
        })?;
    let mut models = BTreeSet::new();
    for value in values {
        let model = value
            .as_str()
            .filter(|model| {
                !model.is_empty() && model.trim() == *model && !model.contains(['\r', '\n', '\0'])
            })
            .ok_or_else(|| {
                format!(
                    "provider interface '{}' formed models contains a malformed entry",
                    provider.as_str()
                )
            })?;
        if !models.insert(model.to_string()) {
            return Err(format!(
                "provider interface '{}' formed models repeats '{model}'",
                provider.as_str()
            ));
        }
    }
    if models.is_empty() {
        return Err(format!(
            "provider interface '{}' formed models is empty",
            provider.as_str()
        ));
    }
    Ok(models)
}

pub(crate) fn resolved_provider_model_catalog(
    config: &octopus_interfaces::InterfaceDriverConfig,
    provider: &InterfaceId,
    formed_models: &BTreeSet<String>,
    configured_model: &str,
    configured_reasoning_effort: &str,
) -> Result<Vec<ResolvedProviderModel>, String> {
    let mut catalog = Vec::new();
    let mut seen = BTreeSet::new();
    if let Some(values) = config
        .private_config
        .get("model_catalog")
        .and_then(serde_json::Value::as_array)
    {
        for value in values {
            let value = value.as_object().ok_or_else(|| {
                format!(
                    "provider interface '{}' model_catalog contains a non-object entry",
                    provider.as_str()
                )
            })?;
            let model = value
                .get("model")
                .and_then(serde_json::Value::as_str)
                .filter(|model| {
                    !model.is_empty()
                        && model.trim() == *model
                        && !model.contains(['\r', '\n', '\0'])
                })
                .ok_or_else(|| {
                    format!(
                        "provider interface '{}' model_catalog entry has no valid model",
                        provider.as_str()
                    )
                })?;
            if !seen.insert(model.to_string()) {
                return Err(format!(
                    "provider interface '{}' model_catalog repeats '{model}'",
                    provider.as_str()
                ));
            }
            if !formed_models.contains(model) {
                continue;
            }
            let supported_reasoning_efforts = parse_model_reasoning_efforts(
                value.get("supported_reasoning_efforts"),
                provider,
                model,
            )?;
            let default_reasoning_effort = value
                .get("default_reasoning_effort")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            if !default_reasoning_effort.is_empty()
                && !supported_reasoning_efforts.is_empty()
                && !supported_reasoning_efforts.contains(&default_reasoning_effort)
            {
                return Err(format!(
                    "provider interface '{}' model '{model}' default reasoning effort '{}' is outside its supported list",
                    provider.as_str(),
                    default_reasoning_effort
                ));
            }
            catalog.push(ResolvedProviderModel {
                model: model.to_string(),
                default_reasoning_effort,
                supported_reasoning_efforts,
            });
        }
    }
    if catalog.is_empty() && formed_models.contains(configured_model) {
        catalog.push(ResolvedProviderModel {
            model: configured_model.to_string(),
            default_reasoning_effort: configured_reasoning_effort.to_string(),
            supported_reasoning_efforts: config.supported_reasoning_efforts.clone(),
        });
    }
    if catalog.is_empty() {
        return Err(format!(
            "provider interface '{}' has no model catalog entry inside its formed models",
            provider.as_str()
        ));
    }
    Ok(catalog)
}

pub(crate) fn parse_model_reasoning_efforts(
    value: Option<&serde_json::Value>,
    provider: &InterfaceId,
    model: &str,
) -> Result<Vec<String>, String> {
    let Some(values) = value else {
        return Ok(Vec::new());
    };
    let values = values.as_array().ok_or_else(|| {
        format!(
            "provider interface '{}' model '{model}' supported_reasoning_efforts is not an array",
            provider.as_str()
        )
    })?;
    let mut efforts = BTreeSet::new();
    for value in values {
        let effort = value.as_str().or_else(|| {
            value
                .as_object()
                .and_then(|entry| entry.get("effort"))
                .and_then(serde_json::Value::as_str)
        });
        let effort = effort
            .filter(|effort| {
                !effort.is_empty()
                    && effort.trim() == *effort
                    && !effort.contains(['\r', '\n', '\0'])
            })
            .ok_or_else(|| {
                format!(
                    "provider interface '{}' model '{model}' has a malformed reasoning effort",
                    provider.as_str()
                )
            })?;
        if !efforts.insert(effort.to_string()) {
            return Err(format!(
                "provider interface '{}' model '{model}' repeats reasoning effort '{effort}'",
                provider.as_str()
            ));
        }
    }
    Ok(efforts.into_iter().collect())
}

pub(crate) fn provider_client_operation_request_labels(
    private_config: &serde_json::Map<String, serde_json::Value>,
    provider: &str,
) -> Result<Vec<String>, String> {
    let mut labels = BTreeSet::new();
    for key in ["provider_operations", "model_catalog_operation"] {
        let Some(value) = private_config.get(key) else {
            continue;
        };
        collect_provider_operation_request_labels(value, provider, key, &mut labels)?;
    }
    Ok(labels.into_iter().collect())
}

pub(crate) fn collect_provider_operation_request_labels(
    value: &serde_json::Value,
    provider: &str,
    field: &str,
    labels: &mut BTreeSet<String>,
) -> Result<(), String> {
    let Some(object) = value.as_object() else {
        return Err(format!(
            "provider interface '{provider}' private_config.{field} is not an object"
        ));
    };
    if let Some(request_label) = object.get("request_label") {
        let request_label = request_label
            .as_str()
            .filter(|label| {
                !label.is_empty()
                    && label.trim() == *label
                    && !label.contains(['\r', '\n', '\0'])
            })
            .ok_or_else(|| {
                format!(
                    "provider interface '{provider}' private_config.{field}.request_label is malformed"
                )
            })?;
        labels.insert(request_label.to_string());
        return Ok(());
    }
    for (name, child) in object {
        if child.is_object() {
            collect_provider_operation_request_labels(
                child,
                provider,
                &format!("{field}.{name}"),
                labels,
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_registry_provider_catalog_admits_its_default_model_and_effort() {
        let interface_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../../registry/interface")
            .canonicalize()
            .unwrap();
        let mut checked = 0;
        for entry in std::fs::read_dir(&interface_root).unwrap() {
            let path = entry.unwrap().path().join("interface.json");
            let Ok(bytes) = std::fs::read(&path) else { continue };
            let provider = parse_strict::<InterfaceDescriptor>(&bytes).unwrap();
            let Some(config) = provider.interface_driver_config.as_ref() else { continue };
            if !config.private_config.contains_key("model_catalog") {
                continue;
            }
            let id = provider.id.as_str();
            let formed = provider.interface_policy.variables["models"]
                .default
                .as_array()
                .unwrap()
                .iter()
                .map(|model| model.as_str().unwrap().to_string())
                .collect::<BTreeSet<_>>();
            let model = config.private_config["model"].as_str().unwrap();
            let configured_effort = config
                .private_config
                .get("reasoning_effort")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .trim();
            let catalog = resolved_provider_model_catalog(
                config,
                &provider.id,
                &formed,
                model,
                configured_effort,
            )
            .unwrap_or_else(|error| panic!("{id}: {error}"));
            let selected = catalog
                .iter()
                .find(|preset| preset.model == model)
                .unwrap_or_else(|| panic!("{id}: catalog does not admit default model '{model}'"));
            let effort = if configured_effort.is_empty() {
                selected.default_reasoning_effort.as_str()
            } else {
                configured_effort
            };
            assert!(
                selected.supported_reasoning_efforts.is_empty()
                    || selected.supported_reasoning_efforts.iter().any(|supported| supported == effort),
                "{id}: default model '{model}' does not support default effort '{effort}'"
            );
            assert!(
                effort.is_empty()
                    || config.supported_reasoning_efforts.is_empty()
                    || config.supported_reasoning_efforts.iter().any(|supported| supported == effort),
                "{id}: default effort '{effort}' is outside its interface ceiling"
            );
            checked += 1;
        }
        assert!(checked >= 30, "only {checked} provider catalogs were checked");
    }
}
