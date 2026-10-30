use super::*;

#[derive(Default)]
pub(crate) struct SessionSourceLedgerEvidence {
    pub(crate) movements: BTreeSet<u64>,
    pub(crate) provider_arrivals: BTreeSet<u64>,
    pub(crate) provider_inputs: BTreeMap<u64, String>,
}

pub(crate) struct SessionContextEvidenceScope<'a> {
    pub(crate) blobs_dir: &'a Path,
    pub(crate) limits: InterfaceHostIoLimits,
    pub(crate) run: &'a RunId,
    pub(crate) provider: &'a InterfaceId,
    pub(crate) context_output_labels: &'a BTreeSet<String>,
    pub(crate) session_snapshot_labels: &'a BTreeSet<String>,
}

pub(crate) fn validate_session_context_evidence(
    context: &mut ResolvedSessionContext,
    scope: SessionContextEvidenceScope<'_>,
    session: &ProviderHandoffSession,
) -> Result<(), String> {
    if context.artifacts.is_empty() {
        return Ok(());
    }
    validate_resolved_session_context(context, "bound provider history", scope.provider)?;
    let sources = context
        .artifacts
        .iter()
        .map(|entry| artifact_ref_key(&entry.source_artifact));
    let evidence = session
        .index()?
        .session_evidence(scope.run, scope.provider, sources)?;
    for (index, entry) in context.artifacts.iter_mut().enumerate() {
        validate_session_source_payload(
            entry,
            scope.blobs_dir,
            scope.limits,
            scope.session_snapshot_labels,
            index,
            scope.provider,
        )?;
        let key = artifact_ref_key(&entry.source_artifact);
        let found = evidence.get(&key).ok_or_else(|| {
            format!("session context artifact[{index}] has no ledger evidence slot")
        })?;
        if let Some(label) = entry.source_movement_seq
            .and_then(|sequence| found.provider_inputs.get(&sequence))
        {
            if label != &entry.label {
                return Err(format!(
                    "session context artifact[{index}] label differs from its exact admitted input"
                ));
            }
            continue;
        }
        let eligible = if scope.context_output_labels.contains(&entry.label) {
            &found.provider_arrivals
        } else {
            &found.movements
        };
        if eligible.is_empty() {
            return Err(format!(
                "session context artifact[{index}] source '{}' has no eligible movement in run '{}'",
                entry.source_artifact.id.as_str(),
                scope.run.as_str()
            ));
        }
        match entry.source_movement_seq {
            Some(sequence) if !eligible.contains(&sequence) => {
                return Err(format!(
                    "session context artifact[{index}] source movement {sequence} does not prove its artifact in run '{}'",
                    scope.run.as_str()
                ));
            }
            Some(_) => {}
            None => {
                entry.source_movement_seq = eligible.iter().next().copied();
            }
        }
    }
    Ok(())
}

pub(crate) fn validate_session_source_payload(
    entry: &SessionContextArtifact,
    blobs_dir: &Path,
    limits: InterfaceHostIoLimits,
    session_snapshot_labels: &BTreeSet<String>,
    index: usize,
    provider: &InterfaceId,
) -> Result<(), String> {
    let source_path = blobs_dir.join(entry.source_artifact.content.to_hex());
    let carrier_limit = limits.max_manifest_bytes.max(limits.max_payload_bytes);
    let carrier =
        octopus_interfaces::read_artifact_carrier_file_bounded(&source_path, carrier_limit)
            .map_err(|error| {
                format!(
                    "session context artifact[{index}] source blob '{}' cannot be read: {error}",
                    source_path.display()
                )
            })?;
    let carrier_hash = ContentHash::of_bytes(&carrier);
    if carrier_hash != entry.source_artifact.content {
        return Err(format!(
            "session context artifact[{index}] source blob hash does not match its ArtifactRef"
        ));
    }
    let payload = match serde_json::from_slice::<InterfaceArtifactManifest>(&carrier) {
        Ok(manifest) if !manifest.payload_path.as_os_str().is_empty() => {
            if carrier.len() as u64 > limits.max_manifest_bytes {
                return Err(format!(
                    "session context artifact[{index}] source manifest exceeds the declared limit"
                ));
            }
            if manifest.label != entry.label {
                return Err(format!(
                    "session context artifact[{index}] label '{}' differs from source manifest label '{}'",
                    entry.label, manifest.label
                ));
            }
            let expected_id = format!("{}:{}", manifest.label, carrier_hash.to_hex());
            if entry.source_artifact.id.as_str() != expected_id {
                return Err(format!(
                    "session context artifact[{index}] source id is not the pinned manifest identity"
                ));
            }
            let payload_hash = manifest
                .payload_content_hash
                .as_deref()
                .and_then(ContentHash::from_hex)
                .ok_or_else(|| {
                    format!(
                        "session context artifact[{index}] source manifest has no valid payload hash"
                    )
                })?;
            let payload_path = blobs_dir.join(payload_hash.to_hex());
            let payload = octopus_interfaces::read_artifact_carrier_file_bounded(
                &payload_path,
                limits.max_payload_bytes,
            )
            .map_err(|error| {
                format!(
                    "session context artifact[{index}] payload blob '{}' cannot be read: {error}",
                    payload_path.display()
                )
            })?;
            if ContentHash::of_bytes(&payload) != payload_hash {
                return Err(format!(
                    "session context artifact[{index}] payload blob hash does not match its manifest"
                ));
            }
            payload
        }
        _ => {
            if carrier.len() as u64 > limits.max_payload_bytes {
                return Err(format!(
                    "session context artifact[{index}] source payload exceeds the declared limit"
                ));
            }
            carrier
        }
    };
    let mut value: serde_json::Value = serde_json::from_slice(&payload).map_err(|error| {
        format!("session context artifact[{index}] source payload is not JSON: {error}")
    })?;
    if session_snapshot_labels.contains(&entry.label) {
        let owner = value.get(SESSION_CONTEXT_FIELD)
            .and_then(|snapshot| snapshot.get("provider_interface"))
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("session context artifact[{index}] snapshot source has no provider identity"))?;
        if owner != provider.as_str() {
            if value != entry.payload {
                return Err(format!("session context artifact[{index}] foreign snapshot differs from its pinned source artifact"));
            }
            return Ok(());
        }
        let removed = value
            .as_object_mut()
            .and_then(|object| object.remove(SESSION_CONTEXT_FIELD));
        if removed.is_none() {
            return Err(format!(
                "session context artifact[{index}] snapshot source has no session context carrier"
            ));
        }
    }
    if value != entry.payload {
        return Err(format!(
            "session context artifact[{index}] payload differs from its pinned source artifact"
        ));
    }
    Ok(())
}

pub(crate) fn bound_session_snapshot(
    value: &serde_json::Value,
    artifact: &ArtifactRef,
) -> Result<Option<SessionContextSnapshot>, String> {
    let Some(raw) = value.get(SESSION_CONTEXT_FIELD) else {
        return Ok(None);
    };
    serde_json::from_value(raw.clone())
        .map(Some)
        .map_err(|error| {
            format!(
                "bound session context artifact '{}' is malformed: {error}",
                artifact.id.as_str()
            )
        })
}

pub fn payload_artifact_bindings(
    record: &RunRecord,
    provider: &InterfaceId,
) -> Result<Vec<ArtifactRef>, String> {
    let Some(value) = record.interface_state.get(provider).and_then(|state| state.get(RUN_AUTHORING_BINDING)) else {
        return Ok(Vec::new());
    };
    let topology: BoundRunAuthoringTopology = serde_json::from_value(value.clone())
        .map_err(|error| format!("payload selection has malformed bound authoring topology: {error}"))?;
    topology.validate_identity(record, provider)?;
    validate_payload_artifact_selection(&topology.payload_artifact_bindings)?;
    Ok(topology.payload_artifact_bindings)
}

pub fn validate_payload_artifact_selection(selected: &[ArtifactRef]) -> Result<(), String> {
    let mut ids = BTreeSet::new();
    for reference in selected {
        non_empty_string(reference.id.as_str(), "payload_artifact_bindings.id")?;
        if reference.id.as_str().trim() != reference.id.as_str() || !ids.insert(&reference.id) {
            return Err("payload_artifact_bindings contains an untrimmed or repeated identity".into());
        }
    }
    Ok(())
}

pub(crate) fn validate_payload_read_bindings(
    selected: &[ArtifactRef],
    checkpoint: &octopus_run_record::CheckpointRecord,
    references: &[ArtifactRef],
    readable: &BTreeSet<ArtifactId>,
    roles: &[String],
) -> Result<(), String> {
    validate_payload_artifact_selection(selected)?;
    for reference in selected {
        if roles.iter().any(|id| id == reference.id.as_str()) {
            return Err("payload selection collides with a formed role artifact".into());
        }
        let authored = checkpoint.artifact_bindings.iter().filter(|binding| binding.id == reference.id).collect::<Vec<_>>();
        let admitted = references.iter().filter(|bound| bound.id == reference.id).collect::<Vec<_>>();
        if authored.is_empty() && admitted.is_empty() { continue; }
        if authored.len() != 1 || !authored[0].access.read
            || admitted != vec![reference] || !readable.contains(&reference.id) {
            return Err(format!("payload artifact '{}' differs from its exact authored and admitted read binding", reference.id));
        }
    }
    Ok(())
}

pub fn context_artifact_payloads(
    record: &RunRecord,
    metadata: &InvocationMetadata,
    payloads: &[octopus_interfaces::ArtifactBindingPayload],
) -> Result<Vec<octopus_interfaces::ArtifactBindingPayload>, String> {
    let runtime = metadata.runtime.as_ref().ok_or("context projection has no admitted runtime coordinate")?;
    let mut selected = payload_artifact_bindings(record, &runtime.interface)?;
    if let Some(reference) = result_inputs::bound_call_evidence(record, &runtime.interface, &runtime.checkpoint)? {
        if metadata.grant.artifact_access.get(reference.id.as_str()) != Some(&octopus_policy::ArtifactAccess::read_only()) {
            return Err("result context lost its exact read-only call evidence grant".into());
        }
        if !selected.contains(&reference) { selected.push(reference); }
    }
    for reference in input_artifacts::bound_sources(record, &runtime.interface, &runtime.checkpoint, &metadata.artifact_binding_refs)? {
        if !selected.contains(&reference) { selected.push(reference); }
    }
    if selected.is_empty() { return Ok(payloads.to_vec()); }
    if runtime.run != record.id || runtime.worker != record.worker {
        return Err("payload selection belongs to a different admitted run/worker".into());
    }
    let checkpoint = record.checkpoint(&runtime.checkpoint).ok_or("payload selection has no pinned admitted checkpoint")?;
    if checkpoint.epoch.0 != runtime.epoch || !record.active_checkpoints.contains(&checkpoint.id) {
        return Err("payload selection has a different pinned checkpoint epoch".into());
    }
    if payloads.iter().map(|bound| &bound.artifact).ne(metadata.artifact_binding_refs.iter()) {
        return Err("payload selection differs from actual admitted binding payloads".into());
    }
    let readable = metadata.grant.artifact_access.iter().filter(|(_, access)| access.read).map(|(id, _)| ArtifactId::new(id)).collect();
    validate_payload_read_bindings(&selected, checkpoint, &metadata.artifact_binding_refs, &readable, &metadata.grant.role_artifacts)?;
    Ok(payloads.iter().filter(|bound| !selected.contains(&bound.artifact)).cloned().collect())
}

pub(crate) fn bound_instruction_sources(
    payloads: &[octopus_interfaces::ArtifactBindingPayload],
    selected_files: &[ArtifactRef],
) -> Result<BTreeSet<ArtifactId>, String> {
    let bound = payloads.iter().map(|payload| (&payload.artifact.id, &payload.artifact))
        .collect::<BTreeMap<_, _>>();
    let mut sources = BTreeSet::new();
    for payload in payloads {
        if selected_files.contains(&payload.artifact) { continue; }
        let bytes = payload.read_bytes().map_err(|error| error.to_string())?;
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes.as_ref()) else { continue; };
        if value.get("label").and_then(serde_json::Value::as_str) != Some(CONTEXT_REFERENCE_LABEL) { continue; }
        let Some(refs) = value.get("sources") else { continue; };
        let refs: Vec<ArtifactRef> = serde_json::from_value(refs.clone()).map_err(|error| {
            format!("instruction artifact '{}' sources are malformed: {error}", payload.artifact.id)
        })?;
        for source in refs {
            if let Some(actual) = bound.get(&source.id) {
                if **actual != source || selected_files.contains(&source) {
                    return Err(format!("instruction source '{}' differs from its explicit context binding", source.id));
                }
                sources.insert(source.id);
            }
        }
    }
    Ok(sources)
}

pub(crate) fn collect_bound_context(
    ctx: &InterfaceLiftContext,
    static_context_artifacts: &mut Vec<ArtifactId>,
    session_context: &mut ResolvedSessionContext,
    session_snapshot_head: &mut Option<SessionSnapshotHead>,
    artifact: &ArtifactRef,
    payload: &[u8],
    provenance: BoundArtifactProvenance,
    session_snapshot_labels: &BTreeSet<String>,
    provider: &InterfaceId,
) -> Result<(), String> {
    collect_bound_context_from_cache(&ctx.blobs_dir, ctx.required_host_io_limits()?, static_context_artifacts, session_context, session_snapshot_head, artifact, payload, provenance, session_snapshot_labels, provider)
}

pub(crate) fn collect_bound_context_from_cache(
    blobs_dir: &Path,
    limits: InterfaceHostIoLimits,
    static_context_artifacts: &mut Vec<ArtifactId>,
    session_context: &mut ResolvedSessionContext,
    session_snapshot_head: &mut Option<SessionSnapshotHead>,
    artifact: &ArtifactRef,
    payload: &[u8],
    provenance: BoundArtifactProvenance,
    session_snapshot_labels: &BTreeSet<String>,
    provider: &InterfaceId,
) -> Result<(), String> {
    if artifact.id.as_str() == format!("{INSTRUCTION_TEXT_LABEL}:{}", artifact.content.to_hex()) {
        let (manifest, pinned_payload) = read_pinned_artifact_from_cache(blobs_dir, limits, artifact)?;
        if manifest.label != INSTRUCTION_TEXT_LABEL || pinned_payload != payload {
            return Err(format!("bound instruction '{}' differs from its pinned text carrier", artifact.id));
        }
        let text = std::str::from_utf8(payload)
            .map_err(|error| format!("bound instruction '{}' is not UTF-8: {error}", artifact.id))?;
        if text.trim().is_empty() {
            return Err(format!("bound instruction '{}' is empty", artifact.id));
        }
        append_artifact(static_context_artifacts, artifact.id.clone());
        return Ok(());
    }
    let value: serde_json::Value = serde_json::from_slice(payload).map_err(|error| {
        format!(
            "bound context artifact '{}' is not JSON: {error}",
            artifact.id.as_str()
        )
    })?;
    let label = value
        .get("label")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|label| !label.is_empty())
        .map(str::to_string)
        .or_else(|| {
            artifact
                .id
                .as_str()
                .rsplit_once(':')
                .and_then(|(label, suffix)| {
                    (suffix == artifact.content.to_hex())
                        .then(|| label.trim().to_string())
                        .filter(|label| !label.is_empty())
                })
        });
    if let Some(snapshot) = bound_session_snapshot(&value, artifact)? {
        validate_session_snapshot(
            &snapshot,
            artifact.id.as_str(),
            &snapshot.provider_interface,
        )?;
        if snapshot.provider_interface != *provider {
            return Ok(());
        }
        let label = label.as_deref().ok_or_else(|| {
            format!(
                "bound session context artifact '{}' has no self-identifying label",
                artifact.id.as_str()
            )
        })?;
        if !session_snapshot_labels.contains(label) {
            return Err(format!(
                "bound artifact '{}' carries reserved session context outside descriptor-declared snapshot labels",
                artifact.id.as_str()
            ));
        }
        if provenance.provider_arrival_seq.is_none() {
            return Err(format!(
                "bound session context artifact '{}' has no provider arrival evidence",
                artifact.id.as_str()
            ));
        }
        if session_snapshot_head.is_some() {
            return Err(
                "provider invocation carries multiple current session snapshots".to_string(),
            );
        }
        if !session_context.artifacts.is_empty() {
            return Err(
                "provider invocation places dynamic context before its current session snapshot"
                    .to_string(),
            );
        }
        let resolved_artifacts = resolve_session_snapshot_chain(
            blobs_dir,
            limits,
            artifact,
            payload,
            provenance.provider_arrival_seq,
            session_snapshot_labels,
            provider,
        )?;
        let covered_artifact_count = resolved_artifacts.len().saturating_sub(1);
        let covered_fingerprint =
            session_snapshot_prefix_fingerprint(&resolved_artifacts[..covered_artifact_count])?;
        session_context.artifacts.extend(resolved_artifacts);
        *session_snapshot_head = Some(SessionSnapshotHead {
            artifact: artifact.clone(),
            covered_artifact_count,
            covered_fingerprint,
        });
        return Ok(());
    }
    if label
        .as_ref()
        .is_some_and(|label| session_snapshot_labels.contains(label))
    {
        return Err(format!(
            "bound session snapshot artifact '{}' has no session context carrier",
            artifact.id.as_str()
        ));
    }
    if label.as_deref() == Some(CONTEXT_REFERENCE_LABEL) {
        append_artifact(static_context_artifacts, artifact.id.clone());
        return Ok(());
    }
    let label = label.ok_or_else(|| {
        format!(
            "bound context artifact '{}' has no self-identifying label",
            artifact.id.as_str()
        )
    })?;
    session_context.artifacts.push(SessionContextArtifact {
        label,
        payload: value,
        source_artifact: artifact.clone(),
        source_movement_seq: provenance.movement_seq,
    });
    Ok(())
}

pub(crate) struct SessionSnapshotCarrier {
    pub(crate) snapshot: SessionContextSnapshot,
    pub(crate) label: String,
    pub(crate) payload: serde_json::Value,
}

pub fn resolve_session_snapshot_chain(
    blobs_dir: &Path,
    limits: InterfaceHostIoLimits,
    head: &ArtifactRef,
    head_payload: &[u8],
    head_movement_seq: Option<u64>,
    session_snapshot_labels: &BTreeSet<String>,
    provider: &InterfaceId,
) -> Result<Vec<SessionContextArtifact>, String> {
    let mut reverse = Vec::new();
    let mut seen = BTreeSet::new();
    let mut current_artifact = head.clone();
    let mut current_payload = head_payload.to_vec();
    let mut current_label = None;
    let mut current_movement_seq = head_movement_seq;
    loop {
        let key = artifact_ref_key(&current_artifact);
        if !seen.insert(key) {
            return Err(format!(
                "session snapshot '{}' contains a parent cycle",
                head.id.as_str()
            ));
        }
        let carrier = decode_session_snapshot_carrier(
            &current_artifact,
            &current_payload,
            current_label.as_deref(),
            session_snapshot_labels,
            provider,
        )?;
        let parent = carrier.snapshot.parent.clone();
        reverse.push((current_artifact.clone(), current_movement_seq, carrier));
        let Some(parent) = parent else {
            break;
        };
        let (manifest, payload) = read_pinned_artifact_from_cache(blobs_dir, limits, &parent)?;
        current_artifact = parent;
        current_payload = payload;
        current_label = Some(manifest.label);
        current_movement_seq = None;
    }
    reverse.reverse();
    let mut resolved = ResolvedSessionContext {
        provider_interface: provider.clone(),
        artifacts: Vec::new(),
    };
    let head_index = reverse.len().saturating_sub(1);
    for (index, (artifact, source_movement_seq, carrier)) in reverse.into_iter().enumerate() {
        resolved.artifacts.extend(carrier.snapshot.artifacts);
        if index == head_index {
            resolved.artifacts.push(SessionContextArtifact {
                label: carrier.label,
                payload: carrier.payload,
                source_artifact: artifact,
                source_movement_seq,
            });
        }
    }
    validate_resolved_session_context(&resolved, head.id.as_str(), provider)?;
    Ok(resolved.artifacts)
}

pub(crate) fn decode_session_snapshot_carrier(
    artifact: &ArtifactRef,
    payload: &[u8],
    pinned_label: Option<&str>,
    session_snapshot_labels: &BTreeSet<String>,
    provider: &InterfaceId,
) -> Result<SessionSnapshotCarrier, String> {
    let mut value: serde_json::Value = serde_json::from_slice(payload).map_err(|error| {
        format!(
            "session snapshot artifact '{}' payload is not JSON: {error}",
            artifact.id.as_str()
        )
    })?;
    let payload_label = value
        .get("label")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|label| !label.is_empty())
        .map(str::to_string);
    let identity_label = artifact
        .id
        .as_str()
        .rsplit_once(':')
        .and_then(|(label, suffix)| (suffix == artifact.content.to_hex()).then_some(label))
        .map(str::trim)
        .filter(|label| !label.is_empty())
        .map(str::to_string);
    let label = pinned_label
        .map(str::to_string)
        .or_else(|| payload_label.clone())
        .or_else(|| identity_label.clone())
        .ok_or_else(|| {
            format!(
                "session snapshot artifact '{}' has no self-identifying label",
                artifact.id.as_str()
            )
        })?;
    if pinned_label.is_some_and(|pinned| {
        payload_label
            .as_deref()
            .is_some_and(|payload| payload != pinned)
    }) || pinned_label.is_some_and(|pinned| {
        identity_label
            .as_deref()
            .is_some_and(|identity| identity != pinned)
    }) {
        return Err(format!(
            "session snapshot artifact '{}' label differs across its pinned carrier",
            artifact.id.as_str()
        ));
    }
    if !session_snapshot_labels.contains(&label) {
        return Err(format!(
            "session snapshot artifact '{}' label '{}' is not descriptor-declared",
            artifact.id.as_str(),
            label
        ));
    }
    let raw = value
        .as_object_mut()
        .ok_or_else(|| {
            format!(
                "session snapshot artifact '{}' payload is not an object",
                artifact.id.as_str()
            )
        })?
        .remove(SESSION_CONTEXT_FIELD)
        .ok_or_else(|| {
            format!(
                "session snapshot artifact '{}' has no session context carrier",
                artifact.id.as_str()
            )
        })?;
    let snapshot: SessionContextSnapshot = serde_json::from_value(raw).map_err(|error| {
        format!(
            "session snapshot artifact '{}' is malformed: {error}",
            artifact.id.as_str()
        )
    })?;
    validate_session_snapshot(&snapshot, artifact.id.as_str(), provider)?;
    Ok(SessionSnapshotCarrier {
        snapshot,
        label,
        payload: value,
    })
}

pub(crate) fn append_session_payload(
    session_context: &mut ResolvedSessionContext,
    label: &str,
    payload: &[u8],
    source_artifact: &ArtifactRef,
    source_movement_seq: Option<u64>,
    source: &str,
) -> Result<bool, String> {
    let label = non_empty_string(label, "session artifact label")?;
    let value = serde_json::from_slice(payload)
        .map_err(|error| format!("{source} session payload is not JSON: {error}"))?;
    let entry = SessionContextArtifact {
        label,
        payload: value,
        source_artifact: source_artifact.clone(),
        source_movement_seq,
    };
    if let Some(existing) = session_context.artifacts.iter()
        .find(|existing| existing.source_artifact == entry.source_artifact)
    {
        if !session_context_entries_agree(existing, &entry) {
            return Err(format!("{source} session payload conflicts with its recorded artifact '{}'", source_artifact.id));
        }
        return Ok(false);
    }
    session_context.artifacts.push(entry);
    Ok(true)
}

pub(crate) fn session_context_entries_agree(
    existing: &SessionContextArtifact,
    recovered: &SessionContextArtifact,
) -> bool {
    existing.source_artifact == recovered.source_artifact
        && existing.label == recovered.label
        && existing.payload == recovered.payload
        && !matches!((existing.source_movement_seq, recovered.source_movement_seq),
            (Some(left), Some(right)) if left != right)
}

pub(crate) fn reconcile_recovered_session_snapshot(
    existing: &[SessionContextArtifact],
    recovered: &mut Vec<SessionContextArtifact>,
    publications: &ContextPublications,
    source: &ArtifactRef,
) -> Result<bool, String> {
    reconcile_session_context_order(existing, recovered, publications, source, false)
}

pub(crate) fn reconcile_session_context_order(
    existing: &[SessionContextArtifact], recovered: &mut Vec<SessionContextArtifact>,
    publications: &ContextPublications, source: &ArtifactRef, interleave_checked_history: bool,
) -> Result<bool, String> {
    let mismatch = |detail: &str| format!(
        "provider recovered snapshot '{}' differs from existing session context: {detail}",
        source.id.as_str()
    );
    let positions = existing.iter().enumerate().map(|(index, entry)| {
        (artifact_ref_key(&entry.source_artifact), index)
    }).collect::<BTreeMap<_, _>>();
    if positions.len() != existing.len() {
        return Err(mismatch("existing context repeats an artifact"));
    }
    let mut shared = BTreeSet::new();
    let mut last_shared = None;
    for candidate in recovered.iter_mut() {
        if let Some(index) = positions.get(&artifact_ref_key(&candidate.source_artifact)).copied() {
            let existing_entry = &existing[index];
            let same_delivery = existing_entry.label == candidate.label && existing_entry.payload == candidate.payload
                && publications.get(&artifact_ref_key(&candidate.source_artifact)).is_some_and(|sequences| {
                    existing_entry.source_movement_seq.is_some_and(|seq| sequences.contains(&seq))
                        && candidate.source_movement_seq.is_some_and(|seq| sequences.contains(&seq))
                });
            if last_shared.is_some_and(|previous| previous >= index)
                || !(session_context_entries_agree(existing_entry, candidate) || same_delivery)
            {
                return Err(mismatch("shared artifacts changed content or order"));
            }
            candidate.source_movement_seq = existing_entry.source_movement_seq.or(candidate.source_movement_seq);
            last_shared = Some(index);
            shared.insert(index);
        }
    }
    let omitted = shared.len() < existing.len();
    let additions = recovered.len() > shared.len();
    if !additions {
        return Ok(recovered.len() == existing.len());
    }
    if omitted && !publications.contains_key(&artifact_ref_key(source)) {
        return Err(mismatch("snapshot additions have no checked publication"));
    }
    let mut merged = Vec::with_capacity(existing.len() + recovered.len() - shared.len());
    let mut existing_position = 0;
    let mut pending = Vec::new();
    let append_gap = |merged: &mut Vec<SessionContextArtifact>, gap: &[SessionContextArtifact],
        pending: &mut Vec<SessionContextArtifact>| -> Result<(), String> {
        if !gap.is_empty() && !pending.is_empty() {
            if interleave_checked_history {
                let mut existing = gap.iter().peekable();
                for entry in std::mem::take(pending) {
                    let sequence = entry.source_movement_seq.ok_or_else(|| mismatch("inserted context has no admitted movement"))?;
                    while let Some(previous) = existing.peek() {
                        let before = previous.source_movement_seq.ok_or_else(|| mismatch("existing context has no admitted movement"))?;
                        if before == sequence { return Err(mismatch("distinct artifacts claim one movement")); }
                        if before > sequence { break; }
                        merged.push(existing.next().unwrap().clone());
                    }
                    merged.push(entry);
                }
                merged.extend(existing.cloned());
                return Ok(());
            }
            let first = pending.first().and_then(|entry| entry.source_movement_seq)
                .ok_or_else(|| mismatch("inserted context has no admitted movement"))?;
            if gap.iter().any(|entry| entry.source_movement_seq.is_none_or(|sequence| sequence >= first)) {
                return Err(mismatch("existing and inserted context have conflicting movement order"));
            }
        }
        merged.extend_from_slice(gap);
        merged.append(pending);
        Ok(())
    };
    for mut candidate in recovered.iter().cloned() {
        if let Some(index) = positions.get(&artifact_ref_key(&candidate.source_artifact)).copied() {
            append_gap(&mut merged, &existing[existing_position..index], &mut pending)?;
            merged.push(candidate);
            existing_position = index + 1;
        } else {
            let admitted = publications.get(&artifact_ref_key(&candidate.source_artifact));
            if (omitted || last_shared.is_some_and(|index| existing_position <= index)) && admitted.is_none() {
                return Err(mismatch(&format!("inserted artifact '{}' has no checked admission", candidate.source_artifact.id)));
            }
            if let Some(admitted) = admitted {
                if candidate.source_movement_seq.is_some_and(|sequence| !admitted.contains(&sequence)) {
                    return Err(mismatch("inserted artifact changed its admitted movement"));
                }
                candidate.source_movement_seq = candidate.source_movement_seq.or(admitted.first().copied());
            }
            pending.push(candidate);
        }
    }
    append_gap(&mut merged, &existing[existing_position..], &mut pending)?;
    *recovered = merged;
    Ok(true)
}

pub(crate) fn merge_failed_source_context(
    existing: &[SessionContextArtifact], recovered: &[SessionContextArtifact], source: u64,
    invocation_source: Option<u64>,
) -> Result<Vec<SessionContextArtifact>, String> {
    let input_successor = invocation_source.filter(|sequence| *sequence != source).and_then(|sequence| {
        existing.iter().position(|entry| entry.source_movement_seq == Some(sequence))
            .and_then(|position| existing[position + 1..].iter().find(|entry| entry.source_movement_seq != Some(sequence)))
            .map(|entry| entry.source_artifact.clone())
    });
    let (input, existing): (Vec<_>, Vec<_>) = existing.iter().cloned().partition(|entry| {
        invocation_source.is_some_and(|sequence| sequence != source && entry.source_movement_seq == Some(sequence))
    });
    let source_ref = recovered.iter().find(|entry| entry.source_movement_seq == Some(source))
        .ok_or("recovered failed context has no exact original source entry")?.source_artifact.clone();
    let publications = recovered.iter().map(|entry| {
        let sequence = entry.source_movement_seq.ok_or("recovered failed history has no exact movement")?;
        if sequence > source { return Err("recovered failed history follows its source".to_string()); }
        Ok((artifact_ref_key(&entry.source_artifact), BTreeSet::from([sequence])))
    }).collect::<Result<BTreeMap<_, _>, String>>()?;
    if publications.len() != recovered.len() { return Err("recovered failed history repeats an artifact".into()); }
    let mut merged = recovered.to_vec();
    if !reconcile_session_context_order(&existing, &mut merged, &publications, &source_ref, true)? {
        merged = existing;
    }
    let mut input_position = match input_successor {
        Some(successor) => merged.iter().position(|entry| entry.source_artifact == successor)
            .ok_or("failed context recovery lost the queued input's successor")?,
        None => merged.len(),
    };
    for entry in input {
        if merged.iter().any(|prior| prior.source_artifact == entry.source_artifact) {
            return Err("queued invocation input is already part of the failed source history".into());
        }
        merged.insert(input_position, entry);
        input_position += 1;
    }
    Ok(merged)
}

pub(crate) fn unpublished_context_start(state: &HandoffState) -> Result<usize, String> {
    match state.session_snapshot_head.as_ref() {
        Some(head) => {
            if state.session_context.artifacts.get(head.covered_artifact_count)
                .map(|entry| &entry.source_artifact) != Some(&head.artifact)
            {
                return Err("provider input recovery lost its pinned snapshot carrier".into());
            }
            Ok(head.covered_artifact_count + 1)
        }
        None => Ok(0),
    }
}

pub(crate) fn reattach_unpublished_context_inputs(
    state: &mut HandoffState,
    source_movement_seq: u64,
    context_artifacts: Vec<ArtifactId>,
) -> Result<(), String> {
    let covered = unpublished_context_start(state)?;
    state.context_artifacts = context_artifacts;
    let mut kept: Vec<SessionContextArtifact> = Vec::with_capacity(state.session_context.artifacts.len());
    for (position, entry) in std::mem::take(&mut state.session_context.artifacts).into_iter().enumerate() {
        if position >= covered && entry.source_movement_seq == Some(source_movement_seq) {
            if let Some(existing) = kept.iter().find(|kept| kept.source_artifact == entry.source_artifact) {
                if !session_context_entries_agree(existing, &entry) {
                    return Err(format!(
                        "provider recovered input '{}' differs from existing session context",
                        entry.source_artifact.id.as_str()
                    ));
                }
                state.consumed_input = false;
                continue;
            }
            append_artifact(&mut state.context_artifacts, entry.source_artifact.id.clone());
        }
        kept.push(entry);
    }
    state.session_context.artifacts = kept;
    Ok(())
}

pub(crate) fn take_unpublished_context_inputs(
    state: &mut HandoffState,
    source_movement_seq: u64,
) -> Result<Vec<SessionContextArtifact>, String> {
    let covered = unpublished_context_start(state)?;
    let mut inputs = Vec::new();
    let mut position = 0;
    state.session_context.artifacts.retain(|entry| {
        let take = position >= covered && entry.source_movement_seq == Some(source_movement_seq);
        position += 1;
        if take { inputs.push(entry.clone()); }
        !take
    });
    Ok(inputs)
}

pub(crate) fn restore_consumed_context_inputs(
    state: &mut HandoffState,
    inputs: Vec<SessionContextArtifact>,
) -> Result<(), String> {
    for input in inputs {
        if let Some(existing) = state.session_context.artifacts.iter()
            .find(|entry| entry.source_artifact == input.source_artifact)
        {
            if !session_context_entries_agree(existing, &input) {
                return Err(format!(
                    "provider recovered input '{}' differs from existing session context",
                    input.source_artifact.id.as_str()
                ));
            }
            state.consumed_input = false;
        } else {
            append_artifact(&mut state.context_artifacts, input.source_artifact.id.clone());
            state.session_context.artifacts.push(input);
        }
    }
    Ok(())
}

pub(crate) fn validate_resolved_session_context(
    context: &ResolvedSessionContext,
    source: &str,
    provider: &InterfaceId,
) -> Result<(), String> {
    if &context.provider_interface != provider {
        return Err(format!(
            "session context from '{source}' belongs to provider '{}' instead of '{}'",
            context.provider_interface.as_str(),
            provider.as_str()
        ));
    }
    let mut sources = BTreeSet::new();
    for (index, artifact) in context.artifacts.iter().enumerate() {
        non_empty_string(
            &artifact.label,
            &format!("session context from '{source}' artifact[{index}].label"),
        )?;
        let key = artifact_ref_key(&artifact.source_artifact);
        if !sources.insert((key, artifact.label.clone())) {
            return Err(format!(
                "session context from '{source}' repeats artifact source at index {index}"
            ));
        }
    }
    Ok(())
}

pub(crate) fn validate_session_snapshot(
    snapshot: &SessionContextSnapshot,
    source: &str,
    provider: &InterfaceId,
) -> Result<(), String> {
    if snapshot.schema_version != 3 {
        return Err(format!(
            "session snapshot from '{source}' has unsupported schema_version {}",
            snapshot.schema_version
        ));
    }
    let resolved = ResolvedSessionContext {
        provider_interface: snapshot.provider_interface.clone(),
        artifacts: snapshot.artifacts.clone(),
    };
    validate_resolved_session_context(&resolved, source, provider)?;
    if let Some(parent) = snapshot.parent.as_ref() {
        if parent.id.as_str().trim().is_empty() || parent.id.as_str().trim() != parent.id.as_str() {
            return Err(format!(
                "session snapshot from '{source}' has an invalid parent artifact identity"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn session_artifacts_fingerprint(
    artifacts: &[SessionContextArtifact],
) -> Result<ContentHash, String> {
    serde_json::to_vec(artifacts)
        .map(|bytes| ContentHash::of_bytes(&bytes))
        .map_err(|error| format!("session context fingerprint could not be encoded: {error}"))
}

pub(crate) fn session_snapshot_prefix_fingerprint(
    artifacts: &[SessionContextArtifact],
) -> Result<ContentHash, String> {
    // Movement coordinates are grounded independently against the ledger on
    // every read. Filling an absent coordinate must not invalidate the immutable
    // payload prefix and turn each incremental snapshot into a full-history copy.
    let sources = artifacts.iter().map(|entry| {
        (&entry.label, &entry.payload, &entry.source_artifact)
    }).collect::<Vec<_>>();
    serde_json::to_vec(&sources)
        .map(|bytes| ContentHash::of_bytes(&bytes))
        .map_err(|error| format!("session snapshot prefix could not be encoded: {error}"))
}

pub(crate) fn reconcile_resolved_session_snapshot_head(
    session_context: &ResolvedSessionContext,
    session_snapshot_head: &mut Option<SessionSnapshotHead>,
) -> Result<(), String> {
    let Some(head) = session_snapshot_head.as_ref() else {
        return Ok(());
    };
    if head.covered_artifact_count >= session_context.artifacts.len() {
        return Err(format!(
            "provider session snapshot head covers {} artifacts but resolved context has only {}",
            head.covered_artifact_count,
            session_context.artifacts.len()
        ));
    }
    if session_context.artifacts[head.covered_artifact_count].source_artifact != head.artifact {
        return Err(
            "provider resolved session context current carrier differs from its snapshot head"
                .to_string(),
        );
    }
    let grounded_fingerprint =
        session_snapshot_prefix_fingerprint(&session_context.artifacts[..head.covered_artifact_count])?;
    if grounded_fingerprint != head.covered_fingerprint {
        *session_snapshot_head = None;
    }
    Ok(())
}

pub(crate) fn session_snapshot_head_after_output(
    artifact: ArtifactRef,
    session_context: &ResolvedSessionContext,
) -> Result<SessionSnapshotHead, String> {
    let covered_artifact_count =
        session_context
            .artifacts
            .len()
            .checked_sub(1)
            .ok_or_else(|| {
                "provider snapshot output was not appended to session context".to_string()
            })?;
    if session_context.artifacts[covered_artifact_count].source_artifact != artifact {
        return Err(
            "provider snapshot output does not match the appended session carrier".to_string(),
        );
    }
    Ok(SessionSnapshotHead {
        artifact,
        covered_artifact_count,
        covered_fingerprint: session_snapshot_prefix_fingerprint(
            &session_context.artifacts[..covered_artifact_count],
        )?,
    })
}

pub(crate) fn session_snapshot_for_context(
    session_context: &ResolvedSessionContext,
    session_snapshot_head: Option<&SessionSnapshotHead>,
) -> Result<SessionContextSnapshot, String> {
    validate_resolved_session_context(
        session_context,
        "provider output",
        &session_context.provider_interface,
    )?;
    let covered_artifact_count = session_snapshot_head
        .map(|head| head.covered_artifact_count)
        .unwrap_or_default();
    if covered_artifact_count > session_context.artifacts.len() {
        return Err(format!(
            "provider session snapshot head covers {} artifacts but current context has only {}",
            covered_artifact_count,
            session_context.artifacts.len()
        ));
    }
    if let Some(head) = session_snapshot_head {
        if session_snapshot_prefix_fingerprint(&session_context.artifacts[..head.covered_artifact_count])?
            != head.covered_fingerprint
        {
            return Err(
                "provider session context no longer has its pinned snapshot prefix".to_string(),
            );
        }
        let carrier = session_context
            .artifacts
            .get(head.covered_artifact_count)
            .ok_or_else(|| {
                "provider session context lost its current snapshot carrier".to_string()
            })?;
        if carrier.source_artifact != head.artifact {
            return Err(
                "provider session context current carrier differs from its snapshot head"
                    .to_string(),
            );
        }
    }
    let snapshot = SessionContextSnapshot {
        schema_version: 3,
        provider_interface: session_context.provider_interface.clone(),
        parent: session_snapshot_head.map(|head| head.artifact.clone()),
        artifacts: session_context.artifacts[covered_artifact_count..].to_vec(),
    };
    validate_session_snapshot(
        &snapshot,
        "provider output",
        &session_context.provider_interface,
    )?;
    Ok(snapshot)
}

pub(crate) fn materialize_session_snapshot(
    artifact: &InterfaceArtifactFile,
    session_context: &ResolvedSessionContext,
    session_snapshot_head: Option<&SessionSnapshotHead>,
) -> Result<InterfaceArtifactFile, String> {
    let snapshot = session_snapshot_for_context(session_context, session_snapshot_head)?;
    let payload = artifact
        .read_payload_bytes()
        .map_err(|error| format!("provider output payload cannot be read: {error}"))?;
    let mut value: serde_json::Value = serde_json::from_slice(&payload)
        .map_err(|error| format!("provider output payload is not JSON: {error}"))?;
    let object = value
        .as_object_mut()
        .ok_or_else(|| "provider session snapshot output must be a JSON object".to_string())?;
    if object.contains_key(SESSION_CONTEXT_FIELD) {
        return Err(format!(
            "provider output already contains reserved field '{SESSION_CONTEXT_FIELD}'"
        ));
    }
    object.insert(
        SESSION_CONTEXT_FIELD.to_string(),
        serde_json::to_value(snapshot)
            .map_err(|error| format!("session context could not be encoded: {error}"))?,
    );
    let bytes = serde_json::to_vec(&value)
        .map_err(|error| format!("provider session snapshot could not be encoded: {error}"))?;
    artifact
        .rewrite_payload(&bytes)
        .map_err(|error| format!("provider session snapshot could not be materialized: {error}"))
}

pub(crate) fn checkpoint_prefix(run: &RunId, artifact: &ArtifactId, generation: u64) -> String {
    let mut hasher = Sha256::new();
    hasher.update(run.as_str().as_bytes());
    hasher.update([0]);
    hasher.update(artifact.as_str().as_bytes());
    hasher.update([0]);
    hasher.update(generation.to_le_bytes());
    let digest = format!("{:x}", hasher.finalize());
    format!("cp_{}", &digest[..20])
}
