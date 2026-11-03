use super::*;

pub(crate) fn persisted_pending_handoff(
    record: &RunRecord,
    provider: &InterfaceId,
) -> Result<Option<PendingHandoff>, String> {
    let state = record.interface_state.get(provider).ok_or_else(|| {
        format!(
            "provider run '{}' has no interface_state for '{}'",
            record.id.as_str(),
            provider.as_str()
        )
    })?;
    state
        .get("pending_handoff")
        .cloned()
        .map(|value| {
            let pending: PendingHandoff = serde_json::from_value(value).map_err(|error| {
                format!(
                    "provider run '{}' pending_handoff is malformed: {error}",
                    record.id.as_str()
                )
            })?;
            if pending.items.is_empty() {
                return Err(format!(
                    "provider run '{}' pending_handoff contains no items",
                    record.id.as_str()
                ));
            }
            if pending.deferred_publication
                && (!pending.terminal || pending.context_publication.is_none()
                    || pending.withdrawal.is_some() || pending.settlement_of.is_some())
            {
                return Err(format!(
                    "provider run '{}' pending_handoff defers a publication outside a terminal call",
                    record.id.as_str()
                ));
            }
            if pending.continuation_of.is_some()
                && (!pending.terminal || pending.source_without_context || pending.settlement_of.is_some()
                    || pending.withdrawal.is_some() || pending.replacement.is_some() || !pending.standing.is_empty())
            {
                return Err(format!(
                    "provider run '{}' pending_handoff continues a source outside an ordinary terminal batch",
                    record.id.as_str()
                ));
            }
            let mut artifacts = BTreeSet::new();
            let mut checkpoints = BTreeSet::new();
            for (index, item) in pending.items.iter().enumerate() {
                non_empty_string(
                    item.artifact_label.as_str(),
                    &format!("pending_handoff.items[{index}].artifact_label"),
                )?;
                if ContentHash::from_hex(&item.artifact_content_hash).is_none() {
                    return Err(format!(
                        "provider run '{}' pending_handoff.items[{index}] has invalid artifact_content_hash",
                        record.id.as_str()
                    ));
                }
                if !artifacts.insert(item.artifact_id.clone()) {
                    return Err(format!(
                        "provider run '{}' pending_handoff repeats artifact '{}'",
                        record.id.as_str(),
                        item.artifact_id.as_str()
                    ));
                }
                non_empty_string(
                    item.checkpoint.id.as_str(),
                    &format!("pending_handoff.items[{index}].checkpoint.id"),
                )?;
                item.checkpoint.interface_binding.validate().map_err(|error| {
                    format!(
                        "provider run '{}' pending_handoff.items[{index}] inbound binding is invalid: {error}",
                        record.id.as_str()
                    )
                })?;
                if item.checkpoint.interface_binding.interface != *provider {
                    return Err(format!(
                        "provider run '{}' pending_handoff.items[{index}] checkpoint binds interface '{}' instead of provider '{}'",
                        record.id.as_str(),
                        item.checkpoint.interface_binding.interface.as_str(),
                        provider.as_str()
                    ));
                }
                if !item
                    .checkpoint
                    .interface_binding
                    .admits(&item.artifact_label)
                {
                    return Err(format!(
                        "provider run '{}' pending_handoff.items[{index}] inbound binding denies its own artifact label '{}'",
                        record.id.as_str(),
                        item.artifact_label
                    ));
                }
                let private_transport_dispatch = item.artifact_label
                    == PROVIDER_TRANSPORT_DISPATCH_LABEL
                    && item.checkpoint.outbound_interface_bindings.is_empty();
                if private_transport_dispatch
                    && (pending.terminal
                        || !item.checkpoint.required_verified_artifacts.is_empty())
                {
                    return Err(format!(
                        "provider run '{}' pending_handoff.items[{index}] private transport dispatch must be non-terminal and carry no verifier",
                        record.id.as_str()
                    ));
                }
                if item.checkpoint.outbound_interface_bindings.is_empty()
                    && !private_transport_dispatch
                {
                    let topology: BoundRunAuthoringTopology = serde_json::from_value(
                        state.get(RUN_AUTHORING_BINDING).ok_or("recorded provider output has no bound topology")?.clone()
                    ).map_err(|error| format!("recorded provider output topology is malformed: {error}"))?;
                    topology.validate_identity(record, provider)?;
                    if !topology.checkpoints.iter().any(|template| {
                        template.role == RunAuthoringCheckpointRole::ProviderArtifactOutput
                            && template.interface() == provider
                            && template.outbound_interface_bindings.is_empty()
                            && template.interface_binding.admits(&item.artifact_label)
                            && template.required_verified_artifacts == item.checkpoint.required_verified_artifacts
                    }) {
                        return Err(format!("provider run '{}' pending_handoff.items[{index}] has no declared recorded-output owner", record.id.as_str()));
                    }
                }
                let mut outbound = BTreeSet::new();
                for binding in &item.checkpoint.outbound_interface_bindings {
                    binding.validate().map_err(|error| {
                        format!(
                            "provider run '{}' pending_handoff.items[{index}] outbound binding is invalid: {error}",
                            record.id.as_str()
                        )
                    })?;
                    if !outbound.insert(&binding.interface) {
                        return Err(format!(
                            "provider run '{}' pending_handoff.items[{index}] checkpoint repeats outbound interface '{}'",
                            record.id.as_str(),
                            binding.interface.as_str()
                        ));
                    }
                }
                let mut bindings = BTreeSet::new();
                for artifact in &item.checkpoint.artifact_bindings {
                    non_empty_string(
                        artifact.as_str(),
                        &format!(
                            "pending_handoff.items[{index}].checkpoint.artifact_bindings"
                        ),
                    )?;
                    if !bindings.insert(artifact) {
                        return Err(format!(
                            "provider run '{}' pending_handoff.items[{index}] checkpoint repeats artifact binding '{}'",
                            record.id.as_str(),
                            artifact.as_str()
                        ));
                    }
                }
                let mut labels = BTreeSet::new();
                for (required_index, required) in item
                    .checkpoint
                    .required_verified_artifacts
                    .iter()
                    .enumerate()
                {
                    required
                        .validate(&format!(
                            "pending_handoff.items[{index}].checkpoint.required_verified_artifacts[{required_index}]"
                        ))
                        .map_err(|error| {
                            format!("provider pending checkpoint is malformed: {error}")
                        })?;
                    if !labels.insert(required.artifact_label.as_str()) {
                        return Err(format!(
                            "provider run '{}' pending_handoff.items[{index}] checkpoint repeats required artifact label '{}'",
                            record.id.as_str(),
                            required.artifact_label
                        ));
                    }
                }
                pending_required_evidence(item)?;
                if !checkpoints.insert(item.checkpoint.id.clone()) {
                    return Err(format!(
                        "provider run '{}' pending_handoff repeats checkpoint '{}'",
                        record.id.as_str(),
                        item.checkpoint.id.as_str()
                    ));
                }
            }
            for item in &pending.standing {
                if item.artifact_label.trim().is_empty()
                    || ContentHash::from_hex(&item.artifact_content_hash).is_none()
                    || !artifacts.insert(item.artifact_id.clone())
                    || item.checkpoint.interface_binding.interface != *provider
                    || !item.checkpoint.interface_binding.admits(&item.artifact_label)
                    || !item.checkpoint.required_verified_artifacts.is_empty()
                {
                    return Err(format!(
                        "provider run '{}' pending_handoff carries a malformed standing delivery '{}'",
                        record.id.as_str(),
                        item.artifact_id.as_str()
                    ));
                }
            }
            Ok(pending)
        })
        .transpose()
}

pub(crate) fn pending_handoff(
    record: &RunRecord,
    provider: &InterfaceId,
) -> Result<Option<PendingHandoff>, String> {
    let pending = persisted_pending_handoff(record, provider)?;
    if let Some(pending) = pending.as_ref() {
        validate_pending_items_are_active(record, provider, pending)?;
    }
    Ok(pending)
}

pub(crate) fn checkpoint_value_id(checkpoint: &serde_json::Value) -> Result<&str, String> {
    checkpoint
        .as_object()
        .and_then(|checkpoint| checkpoint.get("id"))
        .and_then(serde_json::Value::as_str)
        .filter(|id| !id.trim().is_empty() && id.trim() == *id)
        .ok_or_else(|| "provider run checkpoint has no non-empty trimmed id".to_string())
}

pub(crate) fn optional_u64(
    values: &std::collections::BTreeMap<String, serde_json::Value>,
    key: &str,
) -> Result<u64, String> {
    match values.get(key) {
        None => Ok(0),
        Some(value) => value
            .as_u64()
            .ok_or_else(|| format!("provider interface_state.{key} must be an unsigned integer")),
    }
}

pub(crate) fn non_empty_string(value: &str, name: &str) -> Result<String, String> {
    if value.is_empty() || value.trim() != value {
        return Err(format!(
            "run_authoring {name} must be non-empty and trimmed"
        ));
    }
    Ok(value.to_string())
}

pub(crate) fn provider_context_label_sets(
    descriptor: &InterfaceDescriptor,
) -> Result<(BTreeSet<String>, BTreeSet<String>), String> {
    let interface_config = descriptor.interface_driver_config.as_ref().ok_or_else(|| {
        format!(
            "provider interface '{}' has no interface_driver_config",
            descriptor.id.as_str()
        )
    })?;
    let read_labels = |name: &str| -> Result<BTreeSet<String>, String> {
        interface_config
            .private_config
            .get(name)
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| {
                format!(
                    "provider interface '{}' has no private_config.{name} array",
                    descriptor.id.as_str()
                )
            })?
            .iter()
            .enumerate()
            .map(|(index, value)| {
                non_empty_string(
                    value.as_str().unwrap_or_default(),
                    &format!("{name}[{index}]"),
                )
            })
            .collect::<Result<BTreeSet<_>, _>>()
    };
    let context_output_labels = read_labels("context_output_labels")?;
    if context_output_labels.is_empty() {
        return Err(format!(
            "provider interface '{}' context_output_labels must not be empty",
            descriptor.id.as_str()
        ));
    }
    let session_snapshot_labels = read_labels("session_snapshot_labels")?;
    if session_snapshot_labels.is_empty()
        || !session_snapshot_labels.is_subset(&context_output_labels)
    {
        return Err(format!(
            "provider interface '{}' session_snapshot_labels must be a non-empty subset of context_output_labels",
            descriptor.id.as_str()
        ));
    }
    Ok((context_output_labels, session_snapshot_labels))
}

pub(crate) fn validate_id_fragment(value: &str, name: &str) -> Result<(), String> {
    non_empty_string(value, name)?;
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return Err(format!(
            "run_authoring {name} must contain only ASCII letters, digits, '_', '-' or '.'"
        ));
    }
    Ok(())
}
