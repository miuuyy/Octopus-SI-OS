use super::*;

const RETAINED_TOOL_OUTPUT_FEATURE: &str = "retain_client_tool_outputs";

pub(crate) fn validate_codex_projection_descriptor_contract(
    descriptor: &InterfaceDescriptor,
) -> Result<(), InterfaceDriverError> {
    codex_projection_output_labels(descriptor).map(|_| ())
}

pub(crate) fn codex_projection_output_labels(
    descriptor: &InterfaceDescriptor,
) -> Result<BTreeSet<String>, InterfaceDriverError> {
    if descriptor.interface_driver_ref != CODEX_APP_SERVER_INTERFACE_DRIVER_REF {
        return Err(InterfaceDriverError::Protocol(format!(
            "interface '{}' declares driver '{}' instead of '{}'",
            descriptor.id, descriptor.interface_driver_ref, CODEX_APP_SERVER_INTERFACE_DRIVER_REF
        )));
    }
    let config = descriptor.interface_driver_config.as_ref().ok_or_else(|| {
        InterfaceDriverError::FailedClosed(format!(
            "codex_app_server interface '{}' declares no interface_driver_config",
            descriptor.id
        ))
    })?;
    let context_output_labels =
        private_config_string_set(&config.private_config, "context_output_labels")?;
    let session_snapshot_labels =
        private_config_string_set(&config.private_config, "session_snapshot_labels")?;
    let client_projection_output_labels = validated_client_projection_output_labels(
        &config.private_config,
        &context_output_labels,
        &session_snapshot_labels,
    )?;
    LiveStreamRules::from_private_config_or_disabled(&config.private_config)?
        .validate_projection_labels(
            &context_output_labels,
            &session_snapshot_labels,
            &client_projection_output_labels,
        )?;
    operator_continuity_from_private_config(&config.private_config)?;
    let input_messages = input_message_contracts(&config.private_config)?;
    if let Some(retained) = RetainedToolOutput::from_private_config(&config.private_config)? {
        retained.refuse_operator_input(input_messages.keys())?;
        let pinned = crate::CodexInstanceConfig::from_descriptor(descriptor)
            .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?
            .compatibility
            .is_some_and(|requirements| {
                requirements.required_features.get(RETAINED_TOOL_OUTPUT_FEATURE) == Some(&true)
            });
        if !pinned {
            return Err(InterfaceDriverError::FailedClosed(format!(
                "interface '{}' declares retained_tool_output without pinning '{RETAINED_TOOL_OUTPUT_FEATURE}' on in residual_ambient.spawn.compatibility.required_features; compaction would drop the retained instruction",
                descriptor.id
            )));
        }
    }
    Ok(client_projection_output_labels)
}

pub(crate) fn operator_continuity_from_private_config(
    private_config: &serde_json::Map<String, serde_json::Value>,
) -> Result<OperatorContinuityConfig, InterfaceDriverError> {
    let value = private_config
        .get("operator_continuity")
        .cloned()
        .ok_or_else(|| {
            InterfaceDriverError::FailedClosed(
                "codex_app_server descriptor declares no operator_continuity".to_string(),
            )
        })?;
    serde_json::from_value::<OperatorContinuityConfig>(value)
        .map_err(|error| {
            InterfaceDriverError::Protocol(format!(
                "codex_app_server operator_continuity is malformed: {error}"
            ))
        })?
        .validate()
}

pub(crate) fn validated_client_projection_output_labels(
    private_config: &serde_json::Map<String, serde_json::Value>,
    context_output_labels: &BTreeSet<String>,
    session_snapshot_labels: &BTreeSet<String>,
) -> Result<BTreeSet<String>, InterfaceDriverError> {
    if !session_snapshot_labels.is_subset(context_output_labels) {
        return Err(InterfaceDriverError::Protocol(
            "codex_app_server session_snapshot_labels must be a subset of context_output_labels"
                .to_string(),
        ));
    }
    validate_durable_interruption_output_labels(context_output_labels, session_snapshot_labels)?;
    let labels = private_config_string_set(private_config, "client_projection_output_labels")?;
    let mut required = context_output_labels.clone();
    required.extend(
        REQUIRED_CLIENT_BOUNDARY_OUTPUT_LABELS
            .iter()
            .map(|label| (*label).to_string()),
    );
    required.extend(
        CLIENT_ACTIVITY_OUTPUT_LABELS
            .iter()
            .map(|label| (*label).to_string()),
    );
    if let Some(operations) = provider_operations_from_private_config(private_config)? {
        required.extend([
            operations.error_output_label,
            operations.account_login_completed_output_label,
            operations.account_updated_output_label,
            operations.account_rate_limits_updated_output_label,
            operations.account_login_start.output_label,
            operations.account_login_cancel.output_label,
            operations.account_login_status_read.output_label,
            operations.account_logout.output_label,
            operations.account_read.output_label,
            operations.account_rate_limits_read.output_label,
            operations.account_rate_limit_reset_consume.output_label,
            operations.account_usage_read.output_label,
            operations.account_workspace_messages_read.output_label,
            operations.capabilities_read.output_label,
        ]);
        if let Some(binding) = operations.thread_compact_start {
            required.insert(binding.output_label);
        }
        if let Some(binding) = operations.account_sessions_read {
            required.insert(binding.output_label);
        }
    }
    let missing = required.difference(&labels).cloned().collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(InterfaceDriverError::FailedClosed(format!(
            "codex_app_server client_projection_output_labels omit required boundary outputs: {}",
            missing.join(", ")
        )));
    }
    Ok(labels)
}

pub(crate) fn private_config_string_set(
    private_config: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<BTreeSet<String>, InterfaceDriverError> {
    let value = private_config.get(key).cloned().ok_or_else(|| {
        InterfaceDriverError::FailedClosed(format!("codex_app_server descriptor declares no {key}"))
    })?;
    let labels: Vec<String> = serde_json::from_value(value).map_err(|error| {
        InterfaceDriverError::Protocol(format!("codex_app_server {key} are malformed: {error}"))
    })?;
    let mut unique = BTreeSet::new();
    for label in labels {
        if label.trim().is_empty() || label.trim() != label || !unique.insert(label.clone()) {
            return Err(InterfaceDriverError::Protocol(format!(
                "codex_app_server {key} must be unique non-empty trimmed strings"
            )));
        }
    }
    if unique.is_empty() {
        return Err(InterfaceDriverError::Protocol(format!(
            "codex_app_server {key} must not be empty"
        )));
    }
    Ok(unique)
}

pub(crate) fn codex_session_snapshot_labels(
    descriptor: &InterfaceDescriptor,
) -> Result<BTreeSet<String>, InterfaceDriverError> {
    let config = descriptor.interface_driver_config.as_ref().ok_or_else(|| {
        InterfaceDriverError::FailedClosed(format!(
            "provider interface '{}' has no interface_driver_config",
            descriptor.id.as_str()
        ))
    })?;
    private_config_string_set(&config.private_config, "session_snapshot_labels")
}

pub(crate) fn codex_context_output_labels(
    descriptor: &InterfaceDescriptor,
) -> Result<BTreeSet<String>, InterfaceDriverError> {
    let config = descriptor.interface_driver_config.as_ref().ok_or_else(|| {
        InterfaceDriverError::FailedClosed("provider descriptor has no driver configuration".into())
    })?;
    private_config_string_set(&config.private_config, "context_output_labels")
}

pub(crate) fn is_provider_turn_input(
    descriptor: &InterfaceDescriptor,
    input: &InterfaceInvocationEnvelope,
) -> Result<bool, InterfaceDriverError> {
    let manifest = input.read_artifact_manifest()?;
    let config = descriptor.interface_driver_config.as_ref().ok_or_else(|| {
        InterfaceDriverError::FailedClosed("provider descriptor has no driver configuration".into())
    })?;
    if is_declared_input_message(descriptor, &manifest.label) { return Ok(true); }
    if let Some(peer) = admitted_peer_message(input, config.private_config.get("input_intake"))? {
        return Ok(peer.action != PeerAction::Interrupt);
    }
    if is_explicit_turn_control_label(&manifest.label)
        || manifest.label == PROVIDER_MODELS_LIST_LABEL
        || is_undeclared_provider_operation(&manifest.label, false)
        || configured_provider_operation(&config.private_config, &manifest.label)?.is_some()
        || provider_operations_from_private_config(&config.private_config)?
            .and_then(|operations| operations.thread_compact_start)
            .is_some_and(|operation| operation.request_label == manifest.label)
    {
        return Ok(false);
    }
    let bytes = input.read_artifact_payload()?;
    if decode_lifecycle_inbound(&manifest.label, &bytes)
        .map_err(InterfaceDriverError::Protocol)?.is_some()
    {
        return Ok(false);
    }
    if manifest.label == OPERATOR_MESSAGE_LABEL { return Ok(true); }
    let mut names = input.interface_affordance_specs()?.iter()
        .map(|spec| spec.name.as_str().to_string()).collect();
    let operations = extract_interface_operation_affordances(input, &mut names, &mut Vec::new())?;
    let source = input.metadata()?.checkpoint_binding_policy.as_ref()
        .map(|binding| &binding.inbound.interface);
    if !operations.is_empty() && source.is_none() {
        return Err(InterfaceDriverError::FailedClosed(
            "provider input has no admitted checkpoint binding snapshot".into(),
        ));
    }
    Ok(matches!(decode_provider_input(&bytes, &manifest.label, source, &operations, None, &config.private_config)?, ProviderInput::Turn(_)))
}
