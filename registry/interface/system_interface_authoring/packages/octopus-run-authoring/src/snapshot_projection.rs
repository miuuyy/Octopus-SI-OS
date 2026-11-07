use super::*;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotWindowBounds {
    pub max_carriers: usize,
    pub max_total_bytes: u64,
    pub max_entries: usize,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotSourceScope {
    pub run: String,
    pub worker: String,
    pub provider: String,
    pub client: String,
    pub relay: String,
}

fn evidence<'a>(ctx: &'a InterfaceLiftContext) -> Result<CheckedRunEvidence<'a>, String> {
    let limits = ctx.required_host_io_limits()?;
    Ok(CheckedRunEvidence {
        movements_path: &ctx.movements_path,
        ledger_index_path: &ctx.ledger_index_path,
        blobs_dir: &ctx.blobs_dir,
        max_control_frame_bytes: limits.max_control_frame_bytes,
        max_manifest_bytes: limits.max_manifest_bytes,
        max_payload_bytes: limits.max_payload_bytes,
    })
}

pub fn verify_terminal_source(
    ctx: &InterfaceLiftContext,
    received: &Movement,
    artifact: &ArtifactRef,
    terminal_label: &str,
    scope: &SnapshotSourceScope,
) -> Result<(u64, String), String> {
    let evidence = evidence(ctx)?;
    let mut current = received.clone();
    for _ in 0..8 {
        if current.artifact.as_ref() != Some(artifact) {
            return Err("terminal causal chain changed its exact artifact".into());
        }
        let metadata = current
            .invocation_metadata
            .as_ref()
            .and_then(|m| m.invocation())
            .ok_or("terminal causal chain has no invocation evidence")?;
        let runtime = metadata
            .runtime
            .as_ref()
            .ok_or("terminal causal chain has no runtime scope")?;
        if current.kind == MovementKind::InterfaceOutbound
            && current.from == Endpoint::Interface(InterfaceId::new(&scope.provider))
            && current.to == Endpoint::Interface(InterfaceId::new(&scope.relay))
        {
            if runtime.run.as_str() != scope.run
                || runtime.worker.as_str() != scope.worker
                || runtime.interface.as_str() != scope.relay
                || metadata.verification_failure.is_some()
                || !metadata
                    .checkpoint_admission
                    .as_ref()
                    .is_some_and(|a| a.complete)
            {
                return Err(
                    "terminal origin differs from the exact source run/worker/provider/relay"
                        .into(),
                );
            }
            let hash = runtime
                .run_record_hash
                .as_deref()
                .and_then(ContentHash::from_hex)
                .ok_or("terminal origin has no pinned source record")?;
            let (_, record) = pinned_run_record(ctx, &hash, "context projection terminal")?;
            let checkpoint = record
                .checkpoint(&runtime.checkpoint)
                .ok_or("terminal origin checkpoint absent")?;
            if record.id != runtime.run
                || record.ee != runtime.ee
                || record.worker != runtime.worker
                || !record.active_checkpoints.contains(&checkpoint.id)
                || checkpoint.epoch.0 != runtime.epoch
                || checkpoint.interface_binding.as_ref().is_none_or(|binding| {
                    binding.interface.as_str() != scope.provider
                        || !binding.config.artifact_policy.admits(terminal_label)
                })
                || !checkpoint
                    .outbound_interface_bindings
                    .iter()
                    .any(|binding| {
                        binding.interface.as_str() == scope.relay
                            && binding.config.artifact_policy.admits(terminal_label)
                    })
            {
                return Err(
                    "terminal origin differs from its admitted exact source checkpoint contract"
                        .into(),
                );
            }
            return Ok((current.seq.0, record.ee.to_string()));
        }
        let parent = metadata
            .source_movement_seq
            .ok_or("terminal relay lost original source movement")?;
        if parent >= current.seq.0 {
            return Err("terminal relay has non-causal source evidence".into());
        }
        current = generated_artifacts::read_movement(&evidence, parent)?;
    }
    Err("terminal source exceeds its bounded relay ancestry".into())
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotInputRoute {
    pub interface_binding: AuthoredInterfaceBinding,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotInputOrigin {
    pub artifact_pointer: String,
    pub sequence_pointer: String,
    pub pointers: Vec<String>,
}

fn source_entry_record(
    ctx: &InterfaceLiftContext,
    entry: &SessionContextArtifact,
    scope: &SnapshotSourceScope,
) -> Result<(Movement, RunRecord), String> {
    let seq = entry
        .source_movement_seq
        .ok_or("source context has no movement evidence")?;
    let movement = generated_artifacts::read_movement(&evidence(ctx)?, seq)?;
    let metadata = movement
        .invocation_metadata
        .as_ref()
        .and_then(|value| value.invocation())
        .ok_or("source context has no invocation evidence")?;
    let runtime = metadata
        .runtime
        .as_ref()
        .ok_or("source context has no runtime scope")?;
    let hash = runtime
        .run_record_hash
        .as_deref()
        .and_then(ContentHash::from_hex)
        .ok_or("source context has no pinned run record")?;
    let (_, record) = pinned_run_record(ctx, &hash, "context projection source entry")?;
    let cp = record
        .checkpoint(&runtime.checkpoint)
        .ok_or("source context checkpoint absent")?;
    let inbound = cp
        .interface_binding
        .as_ref()
        .ok_or("source context checkpoint has no inbound binding")?;
    let edge = match movement.kind {
        MovementKind::InterfaceOutbound => {
            movement.from == Endpoint::Interface(inbound.interface.clone())
                && movement.to == Endpoint::Interface(runtime.interface.clone())
                && cp.outbound_interface_bindings.iter().any(|binding| {
                    binding.interface == runtime.interface && binding.admits(&entry.label)
                })
        }
        MovementKind::InterfaceArrival => {
            movement.from == Endpoint::Interface(inbound.interface.clone())
                && movement.to == Endpoint::Interface(record.worker.clone())
                && runtime.interface == inbound.interface
        }
        _ => false,
    };
    if !edge
        || movement.artifact.as_ref() != Some(&entry.source_artifact)
        || runtime.run.as_str() != scope.run
        || runtime.worker.as_str() != scope.worker
        || record.id != runtime.run
        || record.ee != runtime.ee
        || record.worker != runtime.worker
        || cp.run != record.id
        || !record.active_checkpoints.contains(&cp.id)
        || cp.epoch.0 != runtime.epoch
        || !inbound.admits(&entry.label)
        || metadata.verification_failure.is_some()
        || !metadata
            .checkpoint_admission
            .as_ref()
            .is_some_and(|admission| admission.complete)
        || metadata.checkpoint_binding_policy.as_ref()
            != Some(&CheckpointBindingPolicySnapshot {
                inbound: inbound.clone(),
                outbound: cp.outbound_interface_bindings.clone(),
            })
    {
        return Err(
            "source context differs from its exact admitted source edge and pinned contract".into(),
        );
    }
    validate_session_source_payload(
        entry,
        &ctx.blobs_dir,
        ctx.required_host_io_limits()?,
        &BTreeSet::new(),
        0,
        &InterfaceId::new(&scope.provider),
    )?;
    Ok((movement, record))
}

pub fn verify_source_context_entry(
    ctx: &InterfaceLiftContext,
    entry: &SessionContextArtifact,
    scope: &SnapshotSourceScope,
) -> Result<(), String> {
    let (movement, _) = source_entry_record(ctx, entry, scope)?;
    if movement.to != Endpoint::Interface(InterfaceId::new(&scope.provider)) {
        return Err(
            "selected source evidence was not admitted to its exact source provider".into(),
        );
    }
    Ok(())
}

pub fn verify_source_bound_entry(
    ctx: &InterfaceLiftContext,
    entry: &SessionContextArtifact,
    scope: &SnapshotSourceScope,
) -> Result<(), String> {
    let (movement, _) = source_entry_record(ctx, entry, scope)?;
    if movement.from != Endpoint::Interface(InterfaceId::new(&scope.provider))
        && movement.to != Endpoint::Interface(InterfaceId::new(&scope.provider))
    {
        return Err("selected evidence does not cross its exact source provider boundary".into());
    }
    Ok(())
}

pub fn read_source_artifact(
    ctx: &InterfaceLiftContext,
    reference: &ArtifactRef,
    max_bytes: u64,
) -> Result<serde_json::Value, String> {
    let mut limits = ctx.required_host_io_limits()?;
    limits.max_payload_bytes = limits.max_payload_bytes.min(max_bytes);
    let (_, bytes) = read_pinned_artifact_from_cache(&ctx.blobs_dir, limits, reference)?;
    serde_json::from_slice(&bytes).map_err(|e| e.to_string())
}

pub fn selected_source_input_binding(
    ctx: &InterfaceLiftContext,
    entry: &SessionContextArtifact,
    scope: &SnapshotSourceScope,
) -> Result<AuthoredInterfaceBinding, String> {
    let (movement, record) = source_entry_record(ctx, entry, scope)?;
    let checkpoint = movement
        .invocation_metadata
        .as_ref()
        .and_then(|value| value.invocation())
        .and_then(|metadata| metadata.runtime.as_ref())
        .map(|runtime| runtime.checkpoint.clone())
        .ok_or("source input runtime absent")?;
    record
        .checkpoint(&checkpoint)
        .and_then(|cp| cp.interface_binding.clone())
        .ok_or_else(|| "source input binding absent".into())
}

pub fn verify_selected_source_input(
    ctx: &InterfaceLiftContext,
    entry: &SessionContextArtifact,
    scope: &SnapshotSourceScope,
    routes: &[SnapshotInputRoute],
    origin: Option<&SnapshotInputOrigin>,
) -> Result<serde_json::Value, String> {
    let (movement, record) = source_entry_record(ctx, entry, scope)?;
    let metadata = movement
        .invocation_metadata
        .as_ref()
        .and_then(|value| value.invocation())
        .ok_or("source input metadata absent")?;
    let runtime = metadata
        .runtime
        .as_ref()
        .ok_or("source input runtime absent")?;
    let cp = record
        .checkpoint(&runtime.checkpoint)
        .ok_or("source input checkpoint absent")?;
    let inbound = cp
        .interface_binding
        .as_ref()
        .ok_or("source input binding absent")?;
    let topology: BoundRunAuthoringTopology = serde_json::from_value(
        record
            .interface_state
            .get(&InterfaceId::new(&scope.provider))
            .and_then(|state| state.get(RUN_AUTHORING_BINDING))
            .cloned()
            .ok_or("selected source input has no provider authoring declaration")?,
    )
    .map_err(|e| e.to_string())?;
    if runtime.interface.as_str() != scope.provider
        || topology.run != record.id
        || topology.worker != record.worker
        || topology.provider_interface.as_str() != scope.provider
        || !routes.iter().any(|route| {
            route.interface_binding == *inbound && route.interface_binding.admits(&entry.label)
        })
        || !topology.checkpoints.iter().any(|template| {
            template.role.is_provider_input()
                && checkpoint_record_matches_template(cp, template, &record.id)
        })
    {
        return Err(
            "selected input differs from the exact declared source provider input route".into(),
        );
    }
    let mut proof = serde_json::json!({"from_interface":inbound.interface,"to_interface":scope.provider,
        "source_artifact":entry.source_artifact,"source_movement_seq":entry.source_movement_seq});
    if let Some(origin) = origin {
        let reference: ArtifactRef = serde_json::from_value(
            entry
                .payload
                .pointer(&origin.artifact_pointer)
                .cloned()
                .ok_or("source input wrapper has no declared original reference")?,
        )
        .map_err(|e| e.to_string())?;
        let sequence = entry
            .payload
            .pointer(&origin.sequence_pointer)
            .and_then(serde_json::Value::as_u64)
            .ok_or("source input wrapper has no declared original sequence")?;
        if sequence >= movement.seq.0
            || !input_artifacts::selections(&record, &inbound.interface)?
                .iter()
                .any(|selection| {
                    selection.artifact == reference && selection.source_movement_seq == sequence
                })
        {
            return Err(
                "source input wrapper does not retain its exact checked original input binding"
                    .into(),
            );
        }
        let original_source = input_artifacts::InputArtifactBinding {
            artifact: reference.clone(),
            source_movement_seq: sequence,
        };
        let original_evidence = evidence(ctx)?;
        let mut reader = FileCheckedRunReader::open(&original_evidence, sequence.checked_sub(1))?;
        let (original, emitter) =
            input_artifacts::checked_source(&mut reader, &record, &inbound.interface, &original_source)?;
        let original_runtime = original
            .invocation_metadata
            .as_ref()
            .and_then(|value| value.invocation())
            .and_then(|metadata| metadata.runtime.as_ref())
            .ok_or("original source input has no runtime scope")?;
        proof["original"] = serde_json::json!({"source_artifact":reference,"source_movement_seq":sequence,"from":original.from,"to":original.to,
            "emitter_run":emitter.id,"emitter_ee":emitter.ee,"emitter_worker_interface":emitter.worker,"emitter_checkpoint":original_runtime.checkpoint,"emitter_epoch":original_runtime.epoch});
    }
    Ok(proof)
}

pub fn resolve_session_snapshot_window(
    blobs_dir: &Path,
    limits: InterfaceHostIoLimits,
    head: &ArtifactRef,
    head_payload: &[u8],
    head_movement_seq: u64,
    session_snapshot_labels: &BTreeSet<String>,
    provider: &InterfaceId,
    input_labels: &BTreeSet<String>,
    bounds: &SnapshotWindowBounds,
) -> Result<Vec<SessionContextArtifact>, String> {
    if bounds.max_carriers == 0
        || bounds.max_carriers > 1024
        || bounds.max_total_bytes == 0
        || bounds.max_total_bytes > 268_435_456
        || bounds.max_entries == 0
        || bounds.max_entries > 65_536
        || input_labels.is_empty()
        || input_labels
            .iter()
            .any(|label| label.is_empty() || label.trim() != label)
    {
        return Err(
            "snapshot window requires explicit finite ancestry bounds and input labels".into(),
        );
    }
    let mut reverse = Vec::new();
    let mut seen = BTreeSet::new();
    let mut current_artifact = head.clone();
    let mut current_payload = head_payload.to_vec();
    let mut current_label = None;
    let mut total_bytes = 0u64;
    let mut total_entries = 0usize;
    loop {
        total_bytes = total_bytes
            .checked_add(current_payload.len() as u64)
            .ok_or("snapshot ancestry byte count overflow")?;
        if reverse.len() >= bounds.max_carriers
            || total_bytes > bounds.max_total_bytes
            || !seen.insert(artifact_ref_key(&current_artifact))
        {
            return Err(
                "snapshot window exceeds its declared ancestry bound or contains a cycle".into(),
            );
        }
        let mut carrier = decode_session_snapshot_carrier(
            &current_artifact,
            &current_payload,
            current_label.as_deref(),
            session_snapshot_labels,
            provider,
        )?;
        total_entries = total_entries
            .checked_add(carrier.snapshot.artifacts.len())
            .ok_or("snapshot ancestry entry count overflow")?;
        if total_entries > bounds.max_entries {
            return Err("snapshot window exceeds its declared entry bound".into());
        }
        let input = carrier
            .snapshot
            .artifacts
            .iter()
            .rposition(|entry| input_labels.contains(&entry.label));
        let parent = carrier.snapshot.parent.clone();
        if let Some(input) = input {
            carrier.snapshot.artifacts.drain(..input);
        }
        reverse.push(carrier);
        if input.is_some() {
            break;
        }
        let parent = parent.ok_or("snapshot ancestry has no selected human input")?;
        if reverse.len() >= bounds.max_carriers || total_bytes >= bounds.max_total_bytes {
            return Err(
                "snapshot ancestry cannot reach selected input within declared bounds".into(),
            );
        }
        let mut bounded_limits = limits;
        bounded_limits.max_payload_bytes = bounded_limits
            .max_payload_bytes
            .min(bounds.max_total_bytes - total_bytes);
        let (manifest, payload) =
            read_pinned_artifact_from_cache(blobs_dir, bounded_limits, &parent)?;
        current_artifact = parent;
        current_payload = payload;
        current_label = Some(manifest.label);
    }
    reverse.reverse();
    let head_carrier = reverse.last().ok_or("snapshot window has no head")?;
    let head_entry = SessionContextArtifact {
        label: head_carrier.label.clone(),
        payload: head_carrier.payload.clone(),
        source_artifact: head.clone(),
        source_movement_seq: Some(head_movement_seq),
    };
    let mut artifacts = reverse
        .into_iter()
        .flat_map(|carrier| carrier.snapshot.artifacts)
        .collect::<Vec<_>>();
    artifacts.push(head_entry);
    let resolved = ResolvedSessionContext {
        provider_interface: provider.clone(),
        artifacts,
    };
    validate_resolved_session_context(&resolved, head.id.as_str(), provider)?;
    Ok(resolved.artifacts)
}
