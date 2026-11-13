use super::*;

pub(crate) fn load_client_config(interface_root: &Path) -> Result<ClientConfig, String> {
    load_named_client_config(interface_root, OPERATOR_CHAT_INTERFACE)
}

pub(crate) fn load_named_client_config(interface_root: &Path, source: &str) -> Result<ClientConfig, String> {
    safe_segment(source, "client template")?;
    let path = interface_root
        .join(source)
        .join("client.json");
    let bytes = std::fs::read(&path)
        .map_err(|error| format!("client config '{}' cannot be read: {error}", path.display()))?;
    let config: ClientConfig = serde_json::from_slice(&bytes)
        .map_err(|error| format!("client config '{}' is malformed: {error}", path.display()))?;
    if config.schema_version != 2 {
        return Err(format!(
            "client config '{}' has unsupported schema_version {}",
            path.display(),
            config.schema_version
        ));
    }
    if config.interface != source {
        return Err(format!(
            "client config '{}' belongs to interface '{}'",
            path.display(),
            config.interface
        ));
    }
    for (value, field) in [
        (&config.artifact_labels.message, "artifact_labels.message"),
        (
            &config.artifact_labels.turn_control,
            "artifact_labels.turn_control",
        ),
        (
            &config.artifact_labels.tool_call,
            "artifact_labels.tool_call",
        ),
        (
            &config.authoring.source_interface,
            "authoring.source_interface",
        ),
        (&config.authoring.room_interface, "authoring.room_interface"),
        (&config.authoring.tool, "authoring.tool"),
        (&config.run.interface_prefix, "run.interface_prefix"),
        (&config.run.run_prefix, "run.run_prefix"),
        (&config.run.ee, "run.ee"),
        (&config.run.worker, "run.worker"),
        (&config.run.tool_call_mark, "run.tool_call_mark"),
        (&config.run.tool_call_verifier, "run.tool_call_verifier"),
        (&config.run.listen_env, "run.listen_env"),
    ] {
        if value.trim().is_empty() {
            return Err(format!("client config field {field} is empty"));
        }
    }
    if let Some(result) = &config.run.tool_result_receiving { result.requirement(&config.authoring.tool)?; }
    let mut predecessors = BTreeSet::new();
    for prior in &config.result_receiving_updates {
        if safe_segment(&prior.worker_template_interface, "result receiving predecessor worker")? != prior.worker_template_interface {
            return Err("result receiving predecessor worker must be exact and trimmed".into());
        }
        for mark in &prior.mark_outcomes {
            if safe_policy_axis_value(mark.as_str(), "result receiving predecessor mark")? != mark.as_str()
                || matches!(mark.as_str(), "." | "..") { return Err("result receiving predecessor mark must name an exact registry folder".into()); }
        }
        if !predecessors.insert(&prior.worker_template_interface)
            || prior.mark_outcomes.iter().collect::<BTreeSet<_>>().len() != prior.mark_outcomes.len()
            || config.run.tool_result_receiving.as_ref().is_some_and(|result| prior.mark_outcomes.contains(&result.mark)) {
            return Err("result receiving predecessors require unique exact worker/mark lists without the new correspondence mark".into());
        }
    }
    Ok(config)
}
