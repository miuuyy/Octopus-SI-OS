use super::*;

pub(crate) async fn send_authoring_manifest(
    ctx: &InterfaceLiftContext,
    config: &RunAuthoringConfig,
    manifest_bytes: &[u8],
) -> Result<DeliveryVerdict, String> {
    let payload: serde_json::Value = serde_json::from_slice(manifest_bytes)
        .map_err(|error| format!("interface authoring manifest is malformed: {error}"))?;
    if !payload.is_object() {
        return Err("interface authoring manifest must be a JSON object".to_string());
    }
    let text = std::str::from_utf8(manifest_bytes)
        .map_err(|error| format!("interface authoring manifest is not UTF-8: {error}"))?.to_owned();
    let control_root = ctx.root_slot("app_server_control_root")?;
    let socket =
        octopus_app_server_interface::interface_control_socket_path_from_root(
            control_root,
            &config.authoring_source_interface,
        );
    loop {
        if ctx.cancel.is_cancelled() {
            return Err("interface authoring hand-in was cancelled".into());
        }
        let stream = match tokio::net::UnixStream::connect(&socket).await {
            Ok(stream) => stream,
            Err(error) if matches!(error.kind(), std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound) => {
                tokio::select! {
                    _ = ctx.cancel.cancelled() => return Err("interface authoring hand-in was cancelled".into()),
                    _ = tokio::time::sleep(Duration::from_millis(20)) => {}
                }
                continue;
            }
            Err(error) => {
                return Err(format!(
                    "interface authoring socket '{}' cannot be connected: {error}",
                    socket.display()
                ));
            }
        };
        let (mut ws, _) = tokio_tungstenite::client_async("ws://localhost/rpc", stream)
            .await
            .map_err(|error| format!("interface authoring websocket upgrade failed: {error}"))?;
        ws.send(WsMessage::Text(text.clone().into()))
            .await
            .map_err(|error| format!("interface authoring send failed: {error}"))?;
        let reply = ws
            .next()
            .await
            .ok_or_else(|| {
                "interface authoring socket closed before the delivery verdict".to_string()
            })?
            .map_err(|error| format!("interface authoring verdict read failed: {error}"))?;
        let reply = reply
            .into_text()
            .map_err(|error| format!("interface authoring verdict is not text: {error}"))?;
        let verdict: DeliveryVerdict = serde_json::from_str(&reply)
            .map_err(|error| format!("interface authoring verdict is malformed: {error}"))?;
        let _ = ws.close(None).await;
        if !verdict.awaits_current_source() {
            return Ok(verdict);
        }
        tokio::select! {
            _ = ctx.cancel.cancelled() => return Err("interface authoring hand-in was cancelled".into()),
            _ = tokio::time::sleep(Duration::from_millis(20)) => {}
        }
    }
}

pub(crate) fn run_authoring_profile(descriptor: &InterfaceDescriptor) -> Result<RunAuthoringProfile, String> {
    let interface_config = descriptor.interface_driver_config.as_ref().ok_or_else(|| {
        format!(
            "provider interface '{}' has no interface_driver_config",
            descriptor.id.as_str()
        )
    })?;
    if interface_config
        .private_config
        .contains_key("run_authoring")
    {
        return Err(format!(
            "provider interface '{}' declares removed private_config.run_authoring reverse topology",
            descriptor.id.as_str()
        ));
    }
    let value = interface_config
        .private_config
        .get("run_authoring_profile")
        .ok_or_else(|| {
            format!(
                "provider interface '{}' declares no private_config.run_authoring_profile",
                descriptor.id.as_str()
            )
        })?;
    let profile: RunAuthoringProfile = serde_json::from_value(value.clone()).map_err(|error| {
        format!(
            "provider interface '{}' run_authoring_profile is malformed: {error}",
            descriptor.id.as_str()
        )
    })?;
    profile.validate()?;
    let hidden_tools = interface_config
        .private_config
        .get("model_hidden_tools")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            format!(
                "provider interface '{}' private_config.model_hidden_tools must be an array containing its run-authoring tool",
                descriptor.id.as_str()
            )
        })?;
    let mut seen = BTreeSet::new();
    let mut hides_authoring_tool = false;
    for (index, value) in hidden_tools.iter().enumerate() {
        let name = value
            .as_str()
            .filter(|name| !name.is_empty() && name.trim() == *name)
            .ok_or_else(|| {
                format!(
                    "provider interface '{}' private_config.model_hidden_tools[{index}] must be a non-empty trimmed string",
                    descriptor.id.as_str()
                )
            })?;
        if !seen.insert(name.to_string()) {
            return Err(format!(
                "provider interface '{}' private_config.model_hidden_tools repeats '{name}'",
                descriptor.id.as_str()
            ));
        }
        if name == profile.file_write_tool.as_str() {
            hides_authoring_tool = true;
        }
    }
    if !hides_authoring_tool {
        return Err(format!(
            "provider interface '{}' exposes run-authoring tool '{}' to the model; private_config.model_hidden_tools must contain it",
            descriptor.id.as_str(),
            profile.file_write_tool
        ));
    }
    Ok(profile)
}

impl RunAuthoringProfile {
    pub(crate) fn validate(&self) -> Result<(), String> {
        for (name, value) in [
            ("file_write_tool", self.file_write_tool.as_str()),
            ("file_write_label", self.file_write_label.as_str()),
            (
                "file_write_result_label",
                self.file_write_result_label.as_str(),
            ),
        ] {
            non_empty_string(value, &format!("run_authoring_profile.{name}"))?;
        }
        Ok(())
    }
}

pub fn validate_admitted_run_writer(
    snapshot: &[u8],
    metadata: &InvocationMetadata,
    writer: &CheckedRunWriter,
) -> Result<(), String> {
    let runtime = metadata.runtime.as_ref().ok_or("coauthor proof has no admitted coordinate")?;
    let record = RunRecord::from_bytes(snapshot).map_err(|e| e.to_string())?;
    if runtime.run_record_hash.as_deref() != Some(ContentHash::of_bytes(snapshot).to_hex().as_str())
        || runtime.run != record.id || runtime.worker != record.worker || runtime.ee != record.ee
        || !record.active_checkpoints.contains(&runtime.checkpoint)
        || record.checkpoint(&runtime.checkpoint).is_none_or(|checkpoint| checkpoint.epoch.0 != runtime.epoch)
    {
        return Err("coauthor proof differs from its admitted Run snapshot".into());
    }
    let topology = validate_declared_provider_writer(&record, writer)?;
    topology.validate_identity(&record, &writer.owner)?;
    if !metadata.grant.interfaces.iter().any(|id| id == writer.owner.as_str())
        || metadata.grant.hard_blocked_tools.contains(&writer.tool)
    {
        return Err("coauthor owner or writer is denied by this formation".into());
    }
    if let Some(binding) = &topology.writer_run {
        binding.roles(&record, &writer.owner, &topology.checkpoints)?;
    } else if [&writer.source, &writer.door].iter().any(|id|
        !metadata.grant.interfaces.iter().any(|granted| granted == id.as_str()))
        || !metadata.grant.tools.contains(&writer.tool)
    {
        return Err("coauthor's local authoring edge is absent from this formation".into());
    }
    Ok(())
}

pub(crate) fn validate_declared_provider_writer(record: &RunRecord, writer: &CheckedRunWriter) -> Result<BoundRunAuthoringTopology, String> {
    let topology: BoundRunAuthoringTopology = serde_json::from_value(record.interface_state.get(&writer.owner)
        .and_then(|state| state.get(RUN_AUTHORING_BINDING)).cloned()
        .ok_or("independent provider writer has no owner-authored topology")?).map_err(|e| e.to_string())?;
    if topology.schema_version != 2 || topology.run != record.id || topology.worker != record.worker
        || topology.provider_interface != writer.owner || topology.authoring_source_interface != writer.source {
        return Err("independent provider writer differs from its exact owner topology".into());
    }
    let calls = topology.checkpoints.iter().filter(|cp| cp.role == RunAuthoringCheckpointRole::AuthoringCall).collect::<Vec<_>>();
    let results = topology.checkpoints.iter().filter(|cp| cp.role == RunAuthoringCheckpointRole::AuthoringResult).collect::<Vec<_>>();
    let ([call], [result]) = (calls.as_slice(), results.as_slice()) else { return Err("independent provider writer requires one exact call and result".into()); };
    let [required] = call.required_verified_artifacts.as_slice() else { return Err("independent provider writer requires one verified call".into()); };
    if call.interface() != &writer.source || call.outbound_interfaces().collect::<Vec<_>>() != [&writer.door]
        || result.interface() != &writer.door || result.outbound_interfaces().collect::<Vec<_>>() != [&writer.owner]
        || !call.interface_binding.admits(&writer.call_label) || !result.interface_binding.admits(&writer.result_label)
        || required.artifact_label != writer.call_label || required.verifier.as_str() != "tool_call_allowed"
        || required.params.get("tools") != Some(&serde_json::json!([writer.tool])) {
        return Err("independent provider writer differs from its exact declared source, door, tool or labels".into());
    }
    topology.config(&RunAuthoringProfile {
        file_write_tool:writer.tool.clone(), file_write_label:writer.call_label.clone(),
        file_write_result_label:writer.result_label.clone(),
    }).validate(&writer.owner, &record.id)?;
    Ok(topology)
}

impl BoundRunAuthoringTopology {
    pub(crate) fn config(&self, profile: &RunAuthoringProfile) -> RunAuthoringConfig {
        RunAuthoringConfig {
            run: self.run.clone(),
            worker: self.worker.clone(),
            provider_interface: self.provider_interface.clone(),
            authoring_source_interface: self.authoring_source_interface.clone(),
            file_write_tool: profile.file_write_tool.clone(),
            file_write_label: profile.file_write_label.clone(),
            file_write_result_label: profile.file_write_result_label.clone(),
            payload_artifact_bindings: self.payload_artifact_bindings.clone(),
            writer_run: self.writer_run.clone(),
            checkpoints: self.checkpoints.clone(),
        }
    }

    pub(crate) fn validate_identity(&self, record: &RunRecord, provider: &InterfaceId) -> Result<(), String> {
        if self.schema_version != 2 {
            return Err(format!(
                "provider run '{}' run_authoring_binding schema_version must be 2",
                record.id.as_str()
            ));
        }
        if self.run != record.id
            || self.worker != record.worker
            || self.provider_interface != *provider
        {
            return Err(format!(
                "provider run '{}' run_authoring_binding identity does not match run='{}' worker='{}' provider='{}'",
                record.id.as_str(),
                record.id.as_str(),
                record.worker.as_str(),
                provider.as_str()
            ));
        }
        for input in &self.checkpoints {
            let Some(writer) = &input.owner_authoring else { continue; };
            if writer.source == writer.owner { continue; }
            let owner = validate_declared_provider_writer(record, writer)?;
            if !owner.checkpoints.iter().any(|output| matches!(output.role, RunAuthoringCheckpointRole::ProviderInterfaceOutput | RunAuthoringCheckpointRole::IndependentInput)
                && output.interface_binding == input.interface_binding
                && output.outbound_interface_bindings == input.outbound_interface_bindings
                && output.static_artifact_bindings == input.static_artifact_bindings
                && output.include_context == input.include_context
                && output.required_verified_artifacts == input.required_verified_artifacts) {
                return Err("independent provider input has no matching owner-authored output".into());
            }
        }
        Ok(())
    }
}

pub(crate) fn validate_persisted_profile_binding(
    record: &RunRecord,
    config: &RunAuthoringConfig,
    expected: &BoundRunAuthoringTopology,
) -> Result<(), String> {
    let provider_state = record
        .interface_state
        .get(&config.provider_interface)
        .ok_or_else(|| {
            format!(
                "provider run '{}' has no interface_state for '{}'",
                record.id.as_str(),
                config.provider_interface.as_str()
            )
        })?;
    let value = provider_state.get(RUN_AUTHORING_BINDING).ok_or_else(|| {
        format!(
            "provider run '{}' has no durable run_authoring_binding",
            record.id.as_str()
        )
    })?;
    let persisted: BoundRunAuthoringTopology =
        serde_json::from_value(value.clone()).map_err(|error| {
            format!(
                "provider run '{}' run_authoring_binding is malformed: {error}",
                record.id.as_str()
            )
        })?;
    persisted.validate_identity(record, &config.provider_interface)?;
    if &persisted != expected {
        return Err(format!(
            "provider run '{}' changed its durable run_authoring_binding",
            record.id.as_str()
        ));
    }
    refuse_selected_consent_state(record, &config.provider_interface)?;
    validate_bound_run_authoring_topology(record, config, None)
}

pub(crate) fn resolve_run_authoring_invocation(
    profile: &RunAuthoringProfile,
    provider: &InterfaceId,
    run: &RunId,
    record: &RunRecord,
    source_checkpoint: &CheckpointId,
) -> Result<(RunAuthoringConfig, BoundRunAuthoringTopology), String> {
    resolve_profile_invocation(profile, provider, run, record, Some(source_checkpoint))
}

pub(crate) fn resolve_profile_invocation(
    profile: &RunAuthoringProfile,
    provider: &InterfaceId,
    run: &RunId,
    record: &RunRecord,
    source_checkpoint: Option<&CheckpointId>,
) -> Result<(RunAuthoringConfig, BoundRunAuthoringTopology), String> {
    profile.validate()?;
    if record.id != *run {
        return Err(format!(
            "run_authoring_profile invocation belongs to run '{}', pinned run is '{}'",
            run.as_str(),
            record.id.as_str()
        ));
    }
    let provider_state = record.interface_state.get(provider).ok_or_else(|| {
        format!(
            "provider run '{}' has no interface_state for '{}'",
            record.id.as_str(),
            provider.as_str()
        )
    })?;
    let topology = match provider_state.get(RUN_AUTHORING_BINDING) {
        Some(value) => {
            let generation = optional_u64(provider_state, "authoring_generation")?;
            let pending = persisted_pending_handoff(record, provider)?;
            if generation == 0 {
                if pending.is_some() {
                    return Err(format!(
                        "provider run '{}' declares pending handoff before provider authoring began",
                        record.id.as_str()
                    ));
                }
            } else if let Some(pending) = pending {
                pending.items.first().ok_or_else(|| {
                    format!(
                        "provider run '{}' persisted run_authoring_binding with an empty pending handoff batch",
                        record.id.as_str()
                    )
                })?;
            } else if optional_u64(provider_state, "context_generation")? != 0 {
                return Err("provider has context history without its pending handoff".into());
            }
            serde_json::from_value::<BoundRunAuthoringTopology>(value.clone()).map_err(|error| {
                format!(
                    "provider run '{}' run_authoring_binding is malformed: {error}",
                    record.id.as_str()
                )
            })?
        }
        None => {
            return Err(format!(
                "provider run '{}' has no explicitly authored run_authoring_binding",
                record.id.as_str()
            ));
        }
    };
    topology.validate_identity(record, provider)?;
    let config = topology.config(profile);
    config.validate(provider, run)?;
    refuse_selected_consent_state(record, provider)?;
    validate_bound_run_authoring_topology(record, &config, source_checkpoint)?;
    Ok((config, topology))
}

pub(crate) fn validate_retired_verified_input(
    ctx: &InterfaceLiftContext,
    session: &ProviderHandoffSession,
    initial_record: &RunRecord,
    current_record: &RunRecord,
    initial: &RunAuthoringConfig,
    current: &RunAuthoringConfig,
    initial_topology: &BoundRunAuthoringTopology,
    current_topology: &BoundRunAuthoringTopology,
    consumed: &RunAuthoringCheckpoint,
    source_movement_seq: u64,
) -> Result<(), String> {
    let mut without_verified = initial.clone();
    without_verified.checkpoints.retain(|checkpoint| checkpoint.role != RunAuthoringCheckpointRole::VerifiedInput);
    let mut topology_without_verified = initial_topology.clone();
    topology_without_verified.checkpoints = without_verified.checkpoints.clone();
    if without_verified == *initial
        || without_verified != *current
        || topology_without_verified != *current_topology
        || consumed.role != RunAuthoringCheckpointRole::Stable
        || !consumed.include_context
        || !current.checkpoints.contains(consumed)
    {
        return Err(
            "provider current run changed its profile-derived authoring topology".to_string(),
        );
    }
    let pending = persisted_pending_handoff(current_record, &current.provider_interface)?
        .ok_or_else(|| "retired verified input has no terminal provider handoff".to_string())?;
    if !pending.terminal || pending.source_movement_seq >= source_movement_seq {
        return Err("verified input retirement does not precede the queued source".to_string());
    }
    let (coordinate, evidence) = session.index()?.exact_provider_source_outbound(
        &current.run,
        &current.worker,
        &current.provider_interface,
        pending.source_movement_seq,
    )?;
    let pinned = indexed_source_run_record(ctx, &evidence, pending.source_movement_seq)?;
    let id = CheckpointId::new(&coordinate.checkpoint);
    let checkpoint = pinned
        .checkpoint(&id)
        .ok_or_else(|| "retirement source lost its pinned checkpoint".to_string())?;
    let candidates = initial
        .checkpoints
        .iter()
        .filter(|template| {
            template.role == RunAuthoringCheckpointRole::VerifiedInput
                && checkpoint_record_matches_template(checkpoint, template, &initial_record.id)
                && template.interface() == &evidence.source
                && template.outbound_interface_bindings.len() == 2
                && template
                    .outbound_interfaces()
                    .eq([&current.provider_interface, consumed.interface()])
        })
        .collect::<Vec<_>>();
    if pinned.id != initial_record.id
        || pinned.worker != initial_record.worker
        || !pinned.active_checkpoints.contains(&id)
        || checkpoint.epoch.0 != coordinate.epoch
        || candidates.len() != 1
        || evidence.checkpoint_binding_policy.as_ref()
            != Some(&candidates[0].binding_policy_snapshot())
        || evidence.required_verified_artifacts != candidates[0].required_verified_artifacts
        || !checkpoint
            .outbound_interfaces()
            .eq(evidence.outbound_interface_bindings.iter())
        || !artifact_refs_match_bindings(
            &evidence.artifact_binding_refs,
            &checkpoint.artifact_bindings,
        )
    {
        return Err(
            "queued tool result cannot adopt an unrelated verified-input retirement".to_string(),
        );
    }
    Ok(())
}
