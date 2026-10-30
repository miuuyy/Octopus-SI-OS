use super::*;

pub(crate) fn deferred_provider_turn_dir(ctx: &InterfaceLiftContext) -> PathBuf {
    ctx.interface_var_dir().join("deferred-provider-turn")
}

pub(crate) fn deferred_provider_turn_path(ctx: &InterfaceLiftContext, source_movement_seq: u64) -> PathBuf {
    deferred_provider_turn_dir(ctx).join(format!("{source_movement_seq}.json"))
}

pub(crate) fn persist_deferred_provider_turn(
    ctx: &InterfaceLiftContext,
    run: &RunId,
    turn: &QueuedTurn,
) -> Result<(), String> {
    let mut journal = capture_deferred_provider_turn(ctx, run, turn)?;
    let path = deferred_provider_turn_path(ctx, turn.seq);
    match std::fs::read(&path) {
        Ok(bytes) => {
            let current =
                serde_json::from_slice::<DeferredProviderTurnJournal>(&bytes).map_err(|error| {
                    format!(
                        "deferred provider turn journal '{}' is malformed: {error}",
                        path.display()
                    )
                })?;
            journal.release = current.release.clone();
            if current != journal {
                return Err(format!(
                    "deferred provider turn journal '{}' conflicts with movement {}",
                    path.display(),
                    turn.seq
                ));
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            persist_json_file(&path, &journal, "deferred provider turn journal")
        }
        Err(error) => Err(format!(
            "deferred provider turn journal '{}' cannot be read: {error}",
            path.display()
        )),
    }
}

pub(crate) fn capture_deferred_provider_turn(
    ctx: &InterfaceLiftContext,
    run: &RunId,
    turn: &QueuedTurn,
) -> Result<DeferredProviderTurnJournal, String> {
    let limits = ctx.required_host_io_limits()?;
    let captured = turn
        .envelope
        .artifact_file
        .read_bounded(limits.max_manifest_bytes, limits.max_payload_bytes)
        .map_err(|error| {
            format!(
                "deferred provider turn {} cannot capture its source artifact: {error}",
                turn.seq
            )
        })?;
    let metadata = turn.envelope.metadata.clone().ok_or_else(|| {
        format!(
            "deferred provider turn {} has no invocation metadata",
            turn.seq
        )
    })?;
    let runtime = metadata.runtime().ok_or_else(|| {
        format!(
            "deferred provider turn {} has no runtime coordinates",
            turn.seq
        )
    })?;
    if &runtime.run != run || &runtime.interface != &ctx.interface {
        return Err(format!(
            "deferred provider turn {} runtime coordinates do not match interface '{}' run '{}'",
            turn.seq,
            ctx.interface.as_str(),
            run.as_str()
        ));
    }
    let artifact_bindings = turn
        .envelope
        .artifact_binding_payloads
        .iter()
        .map(|payload| {
            let bytes = payload.read_bytes().map_err(|error| error.to_string())?;
            Ok(DeferredArtifactBindingRef {
                artifact: payload.artifact.clone(),
                bytes_content: ContentHash::of_bytes(bytes.as_ref()),
                byte_count: bytes.len() as u64,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let journal = DeferredProviderTurnJournal {
        schema_version: 3,
        source_movement_seq: turn.seq,
        run_id: run.as_str().to_string(),
        artifact: turn.envelope.artifact.clone(),
        metadata_content_hash: deferred_metadata_hash(&metadata)?,
        payload_content_hash: ContentHash::of_bytes(&captured.payload_bytes).to_hex(),
        payload_byte_count: captured.payload_bytes.len() as u64,
        artifact_bindings,
        release: None,
    };
    let canonical = restore_deferred_provider_envelope(ctx, &journal)?;
    let restored = canonical.artifact_file
        .read_bounded(limits.max_manifest_bytes, limits.max_payload_bytes)
        .map_err(|error| error.to_string())?;
    if captured.manifest_bytes != restored.manifest_bytes
        || captured.payload_bytes != restored.payload_bytes
        || canonical.metadata.as_ref() != Some(&metadata)
        || canonical.artifact_binding_payloads.len() != turn.envelope.artifact_binding_payloads.len()
    {
        return Err(format!("deferred provider turn {} differs from its canonical source bytes", turn.seq));
    }
    for (pinned, observed) in canonical.artifact_binding_payloads.iter().zip(&turn.envelope.artifact_binding_payloads) {
        if pinned.artifact != observed.artifact
            || pinned.read_bytes().map_err(|error| error.to_string())?.as_ref()
                != observed.read_bytes().map_err(|error| error.to_string())?.as_ref()
        {
            return Err(format!("deferred provider turn {} differs from its canonical source bytes", turn.seq));
        }
    }
    Ok(journal)
}

pub(crate) fn persist_deferred_provider_turn_release(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    run: &RunId,
    turn: &QueuedTurn,
    release: &DeferredProviderTurnRelease,
) -> Result<(), String> {
    if let DeferredProviderTurnRelease::SessionRebase {
        rebased_session_carrier,
        ..
    } = release
    {
        validate_artifact_admission(ctx, run, turn.seq, rebased_session_carrier)?;
    }
    if let DeferredProviderTurnRelease::OriginalEnvelope {
        control_id,
        action,
        observed_ledger_next_seq,
    } = release
    {
        validate_original_envelope_release(
            ctx,
            descriptor,
            run,
            turn,
            control_id,
            action,
            *observed_ledger_next_seq,
        )?;
    }
    if let DeferredProviderTurnRelease::SettledEnvelope { observed_ledger_next_seq } = release {
        validate_settled_envelope_release(ctx, descriptor, run, turn, *observed_ledger_next_seq)?;
    }
    let path = deferred_provider_turn_path(ctx, turn.seq);
    let bytes = std::fs::read(&path).map_err(|error| {
        format!(
            "deferred provider turn journal '{}' cannot be read before release: {error}",
            path.display()
        )
    })?;
    let mut current =
        serde_json::from_slice::<DeferredProviderTurnJournal>(&bytes).map_err(|error| {
            format!(
                "deferred provider turn journal '{}' is malformed: {error}",
                path.display()
            )
        })?;
    let mut expected = capture_deferred_provider_turn(ctx, run, turn)?;
    expected.release = current.release.clone();
    if current != expected {
        return Err(format!(
            "deferred provider turn journal '{}' changed before release",
            path.display()
        ));
    }
    match current.release.as_ref() {
        Some(existing) if existing == release => return Ok(()),
        Some(_) => {
            return Err(format!(
                "deferred provider turn journal '{}' already names a different release carrier",
                path.display()
            ));
        }
        None => current.release = Some(release.clone()),
    }
    persist_json_file(&path, &current, "deferred provider turn journal")
}

pub(crate) fn deferred_metadata_hash(metadata: &octopus_ledger::InvocationMetadata) -> Result<String, String> {
    serde_json::to_vec(metadata)
        .map(|bytes| ContentHash::of_bytes(&bytes).to_hex())
        .map_err(|error| error.to_string())
}

pub(crate) fn read_deferred_binding_payload(
    ctx: &InterfaceLiftContext,
    binding: &DeferredArtifactBindingRef,
) -> Result<Vec<u8>, String> {
    let limits = ctx.required_host_io_limits()?;
    if binding.byte_count > limits.max_payload_bytes {
        return Err("deferred binding exceeds its captured payload contract".into());
    }
    let carrier = octopus_interfaces::read_artifact_carrier_file_bounded(
        &ctx.blobs_dir.join(binding.artifact.content.to_hex()),
        limits.max_manifest_bytes.max(limits.max_payload_bytes),
    ).map_err(|error| error.to_string())?;
    if ContentHash::of_bytes(&carrier) != binding.artifact.content {
        return Err("deferred binding carrier differs from its admitted reference".into());
    }
    let bytes = match serde_json::from_slice::<octopus_interfaces::InterfaceArtifactManifest>(&carrier) {
        Ok(manifest) if !manifest.payload_path.as_os_str().is_empty() => {
            if carrier.len() as u64 > limits.max_manifest_bytes || manifest.label.trim().is_empty()
                || !manifest.payload_path.is_absolute()
                || manifest.payload_content_hash.as_deref().and_then(ContentHash::from_hex) != Some(binding.bytes_content)
            {
                return Err("deferred binding bytes differ from their admitted manifest payload pin".into());
            }
            octopus_interfaces::read_artifact_carrier_file_bounded(
                &ctx.blobs_dir.join(binding.bytes_content.to_hex()), limits.max_payload_bytes,
            ).map_err(|error| error.to_string())?
        }
        _ => carrier,
    };
    if bytes.len() as u64 != binding.byte_count || ContentHash::of_bytes(&bytes) != binding.bytes_content {
        return Err("deferred binding bytes differ from their admitted payload digest or length".into());
    }
    Ok(bytes)
}

pub(crate) fn restore_deferred_provider_envelope(
    ctx: &InterfaceLiftContext,
    journal: &DeferredProviderTurnJournal,
) -> Result<InterfaceInvocationEnvelope, String> {
    if journal.schema_version != 3 || journal.run_id.trim().is_empty() {
        return Err("deferred provider receipt has an unsupported schema or empty run".into());
    }
    let run = RunId::new(journal.run_id.clone());
    let source = read_provider_source_movement(ctx, &run, journal.source_movement_seq)?;
    let metadata = source.invocation_metadata.as_ref()
        .and_then(octopus_ledger::MovementMetadata::invocation)
        .ok_or("deferred provider source has no original invocation metadata")?;
    if source.artifact.as_ref() != Some(&journal.artifact)
        || deferred_metadata_hash(metadata)? != journal.metadata_content_hash
        || metadata.artifact_binding_refs.len() != journal.artifact_bindings.len()
    {
        return Err("deferred provider receipt differs from its exact admitted source or grant".into());
    }
    let limits = ctx.required_host_io_limits()?;
    let manifest_path = ctx.blobs_dir.join(journal.artifact.content.to_hex());
    let manifest_bytes = octopus_interfaces::read_artifact_carrier_file_bounded(
        &manifest_path, limits.max_manifest_bytes,
    ).map_err(|error| error.to_string())?;
    if ContentHash::of_bytes(&manifest_bytes) != journal.artifact.content {
        return Err("deferred provider manifest differs from its admitted reference".into());
    }
    let manifest: octopus_interfaces::InterfaceArtifactManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|error| format!("deferred provider manifest is malformed: {error}"))?;
    let payload_hash = manifest.payload_content_hash.as_deref().and_then(ContentHash::from_hex)
        .ok_or("deferred provider manifest has no canonical payload pin")?;
    if !manifest.payload_path.is_absolute() || manifest.label.trim().is_empty()
        || !artifact_ref_has_label(&journal.artifact, &manifest.label)
        || payload_hash.to_hex() != journal.payload_content_hash
        || journal.payload_byte_count > limits.max_payload_bytes
    {
        return Err("deferred provider receipt differs from its admitted manifest payload pin".into());
    }
    let payload_bytes = octopus_interfaces::read_artifact_carrier_file_bounded(
        &ctx.blobs_dir.join(payload_hash.to_hex()), limits.max_payload_bytes,
    ).map_err(|error| error.to_string())?;
    if payload_bytes.len() as u64 != journal.payload_byte_count
        || ContentHash::of_bytes(&payload_bytes) != payload_hash
    {
        return Err("deferred provider payload differs from its admitted digest or length".into());
    }
    let artifact_file = octopus_interfaces::InterfaceArtifactFile::from_captured_snapshot(
        manifest_path, manifest_bytes, payload_bytes,
    ).map_err(|error| error.to_string())?;
    let mut artifact_binding_payloads = Vec::with_capacity(journal.artifact_bindings.len());
    for (expected, binding) in metadata.artifact_binding_refs.iter().zip(&journal.artifact_bindings) {
        if expected != &binding.artifact {
            return Err("deferred provider receipt changed its ordered artifact bindings".into());
        }
        artifact_binding_payloads.push(octopus_interfaces::ArtifactBindingPayload::new(
            binding.artifact.clone(), read_deferred_binding_payload(ctx, binding)?,
        ));
    }
    Ok(InterfaceInvocationEnvelope {
        artifact: journal.artifact.clone(), artifact_file, artifact_binding_payloads,
        metadata: Some(metadata.clone()),
    })
}

pub(crate) fn deferred_provider_journal_entries(ctx: &InterfaceLiftContext) -> Result<Vec<(PathBuf, Vec<u8>)>, String> {
    let directory = deferred_provider_turn_dir(ctx);
    let entries = match std::fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("deferred provider turn directory '{}' cannot be read: {error}", directory.display())),
    };
    let mut journals = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) == Some("tmp") { continue; }
        let kind = entry.file_type().map_err(|error| error.to_string())?;
        if !kind.is_file() || kind.is_symlink()
            || path.extension().and_then(|extension| extension.to_str()) != Some("json")
        {
            return Err(format!("deferred provider turn entry '{}' is not a regular JSON journal", path.display()));
        }
        let bytes = std::fs::read(&path).map_err(|error| error.to_string())?;
        journals.push((path, bytes));
    }
    journals.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(journals)
}

pub(crate) fn deferred_turn_from_journal(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    path: &Path,
    journal: &DeferredProviderTurnJournal,
) -> Result<DeferredProviderTurn, String> {
    if path != deferred_provider_turn_path(ctx, journal.source_movement_seq) {
        return Err(format!("deferred provider turn journal '{}' has a mismatched identity", path.display()));
    }
    let run = RunId::new(journal.run_id.clone());
    let envelope = restore_deferred_provider_envelope(ctx, journal)?;
    if let Some(DeferredProviderTurnRelease::SessionRebase { rebased_session_carrier, .. }) = &journal.release {
        validate_artifact_admission(ctx, &run, journal.source_movement_seq, rebased_session_carrier)?;
    }
    if !is_provider_turn_input(descriptor, &envelope).map_err(|error| error.to_string())? {
        return Err(format!("deferred provider turn journal '{}' does not preserve a new provider turn", path.display()));
    }
    Ok(DeferredProviderTurn {
        run, turn: QueuedTurn { envelope, seq: journal.source_movement_seq }, release: journal.release.clone(),
    })
}

pub(crate) fn load_deferred_provider_turns(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
) -> Result<BTreeMap<u64, DeferredProviderTurn>, String> {
    let mut turns = BTreeMap::new();
    for (path, bytes) in deferred_provider_journal_entries(ctx)? {
        let journal: DeferredProviderTurnJournal = serde_json::from_slice(&bytes)
            .map_err(|error| format!("deferred provider v3 journal '{}' is malformed: {error}", path.display()))?;
        let turn = deferred_turn_from_journal(ctx, descriptor, &path, &journal)?;
        if turns.insert(journal.source_movement_seq, turn).is_some() {
            return Err(format!("deferred provider turn movement {} is journaled more than once", journal.source_movement_seq));
        }
    }
    Ok(turns)
}

pub(crate) fn remove_deferred_provider_turn(
    ctx: &InterfaceLiftContext,
    source_movement_seq: u64,
) -> Result<(), String> {
    let path = deferred_provider_turn_path(ctx, source_movement_seq);
    std::fs::remove_file(&path).map_err(|error| {
        format!(
            "deferred provider turn journal '{}' cannot be removed after admission: {error}",
            path.display()
        )
    })?;
    let parent = path.parent().ok_or_else(|| {
        format!(
            "deferred provider turn journal '{}' has no parent directory",
            path.display()
        )
    })?;
    std::fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| {
            format!(
                "deferred provider turn directory '{}' cannot be synced: {error}",
                parent.display()
            )
        })
}

pub(crate) enum DeferredTurnReadiness {
    AwaitingSessionSettlement,
    Ready(QueuedTurn),
}

pub(crate) fn operator_turn_requires_deferral(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    run: &RunId,
    turn: &QueuedTurn,
    current: &[octopus_run_authoring::SessionContextArtifact],
) -> Result<bool, String> {
    let source_snapshot = bound_operator_session_snapshot(ctx, descriptor, turn)?;
    let continuity = recover_provider_turn_continuity(
        descriptor,
        &turn.envelope,
        source_snapshot.as_ref().map(|(_, snapshot)| snapshot),
        run,
        &ctx.pinned_run_record_bytes(&turn.envelope)?,
    )
    .map_err(|error| error.to_string())?;
    let continuity = reconcile_operator_session_continuity(
        descriptor, &turn.envelope, continuity, current,
    ).map_err(|error| error.to_string())?;
    if continuity
        .as_ref()
        .is_some_and(codex_continuity_waits_for_result)
    {
        return Ok(true);
    }
    let labels = codex_session_snapshot_labels(descriptor).map_err(|error| error.to_string())?;
    latest_provider_session_carrier(ctx, run, turn.seq, &labels, None)
        .map(|scan| scan.latest.is_some())
}

pub(crate) async fn wait_for_provider_handoff(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    session: &ProviderHandoffSession,
    run: &RunId,
    input: &InterfaceInvocationEnvelope,
    source_movement_seq: u64,
    pinned: &[u8],
) -> Result<ProviderOutputHandoff, String> {
    loop {
        if ctx.cancel.is_cancelled() { return Err("provider input preparation was cancelled".into()); }
        match session.from_invocation_when_ready(ctx, descriptor, run, input, source_movement_seq, pinned).await {
            Ok(handoff) => return Ok(handoff),
            Err(error @ (CheckedRunError::AwaitingResults { .. } | CheckedRunError::ChangedDuringRead { .. })) => {
                tracing::warn!(interface = ctx.interface.as_str(), run = run.as_str(), seq = source_movement_seq,
                    error = %error, "provider source still awaits checked run evidence");
            }
            Err(CheckedRunError::Invalid(error)) => return Err(error),
        }
    }
}

pub(crate) async fn operator_turn_requires_deferral_with_context(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    session: &ProviderHandoffSession,
    run: &RunId,
    turn: &QueuedTurn,
) -> Result<bool, String> {
    let independent = independent_provider_input(ctx, turn);
    if matches!(independent, Ok(Some(_))) && input_awaits_open_handoff(ctx, run, turn)? {
        return Ok(true);
    }
    if matches!(independent, Ok(Some(true))) {
        return Ok(false);
    }
    let pinned = ctx.pinned_run_record_bytes(&turn.envelope)?;
    let handoff = wait_for_provider_handoff(
        ctx, descriptor, session, run, &turn.envelope, turn.seq, &pinned,
    ).await.map_err(|error| error.to_string())?;
    operator_turn_requires_deferral(
        ctx, descriptor, run, turn, &handoff.current_session_context_artifacts().await,
    )
}

pub(crate) fn input_awaits_open_handoff(ctx: &InterfaceLiftContext, run: &RunId, turn: &QueuedTurn) -> Result<bool, String> {
    let path = ctx.root_slot("run_records_root")?.join(format!("{}.json", run.as_str()));
    let bytes = std::fs::read(&path)
        .map_err(|error| format!("provider run '{}' current record cannot be read: {error}", run.as_str()))?;
    let record = octopus_run_record::RunRecord::from_bytes(&bytes).map_err(|error| error.to_string())?;
    octopus_run_authoring::open_standing_handoff_before(ctx, &record, &ctx.interface, turn.seq)
}

pub(crate) fn independent_provider_input(ctx: &InterfaceLiftContext, turn: &QueuedTurn) -> Result<Option<bool>, String> {
    let record = octopus_run_record::RunRecord::from_bytes(&ctx.pinned_run_record_bytes(&turn.envelope)?)
        .map_err(|error| error.to_string())?;
    let metadata = turn.envelope.metadata().map_err(|error| error.to_string())?;
    let runtime = metadata.runtime.as_ref().ok_or("provider input has no runtime identity")?;
    octopus_run_authoring::independent_provider_input(&record, &ctx.interface, &runtime.checkpoint)
}

pub(crate) async fn prepare_deferred_provider_turn(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    run: &RunId,
    deferred: &DeferredProviderTurn,
    active_controls: &ActiveTurnControls,
) -> Result<DeferredTurnReadiness, String> {
    if active_control(active_controls, run).await.is_some_and(|broker| !broker.is_finished()) {
        return Ok(DeferredTurnReadiness::AwaitingSessionSettlement);
    }
    if deferred.release.is_none() && matches!(independent_provider_input(ctx, &deferred.turn), Ok(Some(true))) {
        return Ok(if input_awaits_open_handoff(ctx, run, &deferred.turn)? {
            DeferredTurnReadiness::AwaitingSessionSettlement
        } else {
            DeferredTurnReadiness::Ready(deferred.turn.clone())
        });
    }
    let source_snapshot = bound_operator_session_snapshot(ctx, descriptor, &deferred.turn)?;
    let source_session_carrier = source_snapshot.as_ref().map(|(carrier, _)| carrier.clone());
    let source_continuity = recover_provider_turn_continuity(
        descriptor,
        &deferred.turn.envelope,
        source_snapshot.as_ref().map(|(_, snapshot)| snapshot),
        run,
        &ctx.pinned_run_record_bytes(&deferred.turn.envelope)?,
    )
    .map_err(|error| error.to_string())?;
    if let Some(release) = deferred.release.as_ref() {
        match release {
            DeferredProviderTurnRelease::SettledEnvelope { observed_ledger_next_seq } => {
                validate_settled_envelope_release(ctx, descriptor, run, &deferred.turn, *observed_ledger_next_seq)?;
                return Ok(DeferredTurnReadiness::Ready(deferred.turn.clone()));
            }
            DeferredProviderTurnRelease::InterruptedResult { source_session_carrier: released_source, .. } => {
                if released_source != &source_session_carrier {
                    return Err(format!("deferred provider turn {} changed its interrupted-result source carrier", deferred.turn.seq));
                }
                return Ok(if checked_deferred_interrupted_result(
                    ctx, descriptor, run, &deferred.turn, source_continuity.as_ref(), Some(release), None,
                ).await?.is_some() {
                    DeferredTurnReadiness::Ready(deferred.turn.clone())
                } else { DeferredTurnReadiness::AwaitingSessionSettlement });
            }
            DeferredProviderTurnRelease::RecoveredResult {
                source_session_carrier: released_source,
                failed_source_movement_seq,
            } => {
                if released_source != &source_session_carrier
                    || recover_deferred_failed_result(ctx, descriptor, run, &deferred.turn, source_continuity.as_ref(), Some(*failed_source_movement_seq)).await?
                        != Some(*failed_source_movement_seq)
                {
                    return Err(format!("deferred provider turn {} lost its exact completed-result recovery evidence", deferred.turn.seq));
                }
                return Ok(DeferredTurnReadiness::Ready(deferred.turn.clone()));
            }
            DeferredProviderTurnRelease::SessionRebase {
                source_session_carrier: released_source,
                ..
            } => {
                if released_source != &source_session_carrier {
                    return Err(format!(
                        "deferred provider turn {} release names a different source session carrier",
                        deferred.turn.seq
                    ));
                }
                let (rebased, continuity) =
                    rebase_deferred_provider_turn(ctx, descriptor, run, &deferred.turn, release)?;
                if !codex_continuity_successor_is_settled(
                    source_continuity.as_ref(),
                    continuity.as_ref(),
                )? {
                    return Err(format!(
                        "deferred provider turn {} persisted a release whose continuity is still pending",
                        deferred.turn.seq
                    ));
                }
                return Ok(DeferredTurnReadiness::Ready(rebased));
            }
            DeferredProviderTurnRelease::OriginalEnvelope {
                control_id,
                action,
                observed_ledger_next_seq,
            } => {
                validate_original_envelope_release(
                    ctx,
                    descriptor,
                    run,
                    &deferred.turn,
                    control_id,
                    action,
                    *observed_ledger_next_seq,
                )?;
                if source_continuity
                    .as_ref()
                    .is_some_and(codex_continuity_waits_for_result)
                {
                    return Err(format!(
                        "deferred provider turn {} released its original envelope while continuity is pending",
                        deferred.turn.seq
                    ));
                }
                return Ok(DeferredTurnReadiness::Ready(deferred.turn.clone()));
            }
        }
    }

    if let Some((release, _)) = checked_deferred_interrupted_result(
        ctx, descriptor, run, &deferred.turn, source_continuity.as_ref(), None, None,
    ).await? {
        persist_deferred_provider_turn_release(ctx, descriptor, run, &deferred.turn, &release)?;
        return Ok(DeferredTurnReadiness::Ready(deferred.turn.clone()));
    }

    let labels = codex_session_snapshot_labels(descriptor).map_err(|error| error.to_string())?;
    let scan = latest_provider_session_carrier(ctx, run, deferred.turn.seq, &labels, None)?;
    let has_carrier = scan.latest.is_some();
    if let Some(rebased_session_carrier) = scan.latest {
        let release = DeferredProviderTurnRelease::SessionRebase {
            source_session_carrier: source_session_carrier.clone(), rebased_session_carrier,
        };
        let (rebased, continuity) = rebase_deferred_provider_turn(ctx, descriptor, run, &deferred.turn, &release)?;
        if codex_continuity_successor_is_settled(source_continuity.as_ref(), continuity.as_ref())? {
            persist_deferred_provider_turn_release(ctx, descriptor, run, &deferred.turn, &release)?;
            return Ok(DeferredTurnReadiness::Ready(rebased));
        }
    }
    if let Some(failed_source_movement_seq) = recover_deferred_failed_result(
        ctx, descriptor, run, &deferred.turn, source_continuity.as_ref(), None,
    ).await? {
        let release = DeferredProviderTurnRelease::RecoveredResult {
            source_session_carrier, failed_source_movement_seq,
        };
        persist_deferred_provider_turn_release(ctx, descriptor, run, &deferred.turn, &release)?;
        return Ok(DeferredTurnReadiness::Ready(deferred.turn.clone()));
    }
    if !has_carrier && scan.complete_boundary
        && !source_continuity.as_ref().is_some_and(codex_continuity_waits_for_result)
        && let Some((control_id, action)) = terminal_deferred_turn_ownership(
            ctx, descriptor, run, &deferred.turn.envelope, deferred.turn.seq,
        )?
    {
        let release = DeferredProviderTurnRelease::OriginalEnvelope {
            control_id, action, observed_ledger_next_seq: scan.next_seq,
        };
        persist_deferred_provider_turn_release(ctx, descriptor, run, &deferred.turn, &release)?;
        return Ok(DeferredTurnReadiness::Ready(deferred.turn.clone()));
    }
    if let Some(observed_ledger_next_seq) = settled_envelope_prefix(ctx, descriptor, run, &deferred.turn, None)? {
        let release = DeferredProviderTurnRelease::SettledEnvelope { observed_ledger_next_seq };
        persist_deferred_provider_turn_release(ctx, descriptor, run, &deferred.turn, &release)?;
        return Ok(DeferredTurnReadiness::Ready(deferred.turn.clone()));
    }
    Ok(DeferredTurnReadiness::AwaitingSessionSettlement)
}

pub(crate) fn settled_envelope_prefix(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    run: &RunId,
    turn: &QueuedTurn,
    stop_before_seq: Option<u64>,
) -> Result<Option<u64>, String> {
    if stop_before_seq.is_some_and(|stop| stop <= turn.seq) {
        return Err("settled input release has an invalid ledger boundary".into());
    }
    let pinned = ctx.pinned_run_record_bytes(&turn.envelope)?;
    let record = octopus_run_record::RunRecord::from_bytes(&pinned).map_err(|error| error.to_string())?;
    if record.id != *run || record.status != serde_json::json!("idle") { return Ok(None); }
    let Some(pending) = record.interface_state.get(&ctx.interface).and_then(|state| state.get("pending_handoff")) else {
        return Ok(None);
    };
    let terminal = pending.get("terminal").and_then(serde_json::Value::as_bool).ok_or("settled input has malformed source finality")?;
    let prior = pending.get("source_movement_seq").and_then(serde_json::Value::as_u64).ok_or("settled input has malformed source ownership")?;
    if !terminal || prior >= turn.seq { return Ok(None); }
    let snapshot = bound_operator_session_snapshot(ctx, descriptor, turn)?;
    let continuity = recover_provider_turn_continuity(descriptor, &turn.envelope,
        snapshot.as_ref().map(|(_, snapshot)| snapshot), run, &pinned).map_err(|error| error.to_string())?;
    if continuity.as_ref().is_some_and(codex_continuity_waits_for_result) { return Ok(None); }
    let (mut file, length, limit) = open_arbitration_snapshot(ctx)?;
    let decoder = DurableMovementDecoder::for_log(&ctx.movements_path);
    let (offset, expected) = authoritative_source_scan_start(ctx, &mut file, length, limit, &decoder, turn.seq);
    let mut source_verified = false;
    let mut advanced = false;
    let mut next_seq = turn.seq;
    let scan = scan_authoritative_snapshot(ctx, &mut file, length, limit, &decoder, offset, expected, stop_before_seq, |movement| {
        next_seq = movement.seq.0.checked_add(1).ok_or("settled input ledger sequence overflowed")?;
        if movement.seq.0 < turn.seq { return Ok(None::<()>); }
        if movement.seq.0 == turn.seq {
            if !movement_is_provider_source(ctx, run, &movement) || movement.artifact.as_ref() != Some(&turn.envelope.artifact) {
                return Err("settled input release changed its admitted source".into());
            }
            source_verified = true;
        } else if movement.from == Endpoint::Interface(ctx.interface.clone())
            && movement.runtime().is_some_and(|runtime| &runtime.run == run)
        {
            advanced = true;
        }
        Ok(None)
    })?;
    ensure_arbitration_snapshot_length(ctx, &file, length)?;
    if !source_verified {
        return Err("settled input release has no complete authoritative source prefix".into());
    }
    if !scan.complete_boundary && stop_before_seq.is_none() { return Ok(None); }
    if !scan.complete_boundary || stop_before_seq.is_some_and(|stop| stop != next_seq) {
        return Err("settled input release has no complete authoritative source prefix".into());
    }
    Ok((!advanced).then_some(next_seq))
}

pub(crate) fn validate_settled_envelope_release(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    run: &RunId,
    turn: &QueuedTurn,
    observed_ledger_next_seq: u64,
) -> Result<(), String> {
    if settled_envelope_prefix(ctx, descriptor, run, turn, Some(observed_ledger_next_seq))? != Some(observed_ledger_next_seq) {
        return Err("deferred input was not settled at its recorded release boundary".into());
    }
    Ok(())
}

pub(crate) async fn checked_deferred_interrupted_result(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    run: &RunId,
    turn: &QueuedTurn,
    source: Option<&crate::handler::CodexContinuityState>,
    released: Option<&DeferredProviderTurnRelease>,
    current: Option<&[octopus_run_authoring::SessionContextArtifact]>,
) -> Result<Option<(DeferredProviderTurnRelease, crate::handler::CodexContinuityState)>, String> {
    let Some(source) = source.filter(|state| codex_continuity_has_interrupted_pending(state)) else { return Ok(None); };
    let snapshot = bound_operator_session_snapshot(ctx, descriptor, turn)?;
    let carrier = snapshot.as_ref().map(|(carrier, _)| carrier.clone());
    let (result_source, closure) = match released {
        Some(DeferredProviderTurnRelease::InterruptedResult { source_session_carrier, result_source, closure }) => {
            if source_session_carrier != &carrier { return Err("interrupted release changed its source session carrier".into()); }
            (result_source.clone(), closure.clone())
        }
        Some(_) => return Err("interrupted release has a different release kind".into()),
        None => {
            let Some((interrupted, seq, closure)) = InterruptedEffectArbiter::new(ctx.clone()).reconciled_result(run).await? else { return Ok(None); };
            if &interrupted != source { return Ok(None); }
            let movement = read_provider_source_movement(ctx, run, seq)?;
            (DurableArtifactAdmission { artifact: movement.artifact.ok_or("interrupted result source has no artifact")?, movement_seq: seq }, closure)
        }
    };
    let checked;
    let current = if let Some(current) = current { current } else {
        let pinned = ctx.pinned_run_record_bytes(&turn.envelope)?;
        let session = ProviderHandoffSession::new(ctx)?;
        let handoff = wait_for_provider_handoff(ctx, descriptor, &session, run, &turn.envelope, turn.seq, &pinned).await?;
        checked = handoff.current_session_context_artifacts().await;
        &checked
    };
    let continuity = checked_interrupted_result_continuity(
        ctx, descriptor, run, &turn.envelope, source, &result_source, &closure,
        current,
    ).await?;
    if !codex_continuity_successor_is_settled(Some(source), Some(&continuity))? { return Ok(None); }
    Ok(Some((DeferredProviderTurnRelease::InterruptedResult {
        source_session_carrier: carrier, result_source, closure,
    }, continuity)))
}

pub(crate) async fn checked_interrupted_result_continuity(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    run: &RunId,
    input: &InterfaceInvocationEnvelope,
    interrupted: &crate::handler::CodexContinuityState,
    result_source: &DurableArtifactAdmission,
    closure: &DurableArtifactAdmission,
    current: &[octopus_run_authoring::SessionContextArtifact],
) -> Result<crate::handler::CodexContinuityState, String> {
    let result = read_provider_source_movement(ctx, run, result_source.movement_seq)?;
    if result.artifact.as_ref() != Some(&result_source.artifact) {
        return Err("interrupted release result ref differs from its exact source movement".into());
    }
    let runtime = input.runtime().map_err(|error| error.to_string())?;
    let result_runtime = result.runtime().ok_or("interrupted release result has no runtime coordinates")?;
    if result_runtime.run != runtime.run || result_runtime.worker != runtime.worker {
        return Err("interrupted release result crossed its admitted run scope".into());
    }
    validate_artifact_admission(ctx, run, result_source.movement_seq, closure)?;
    let (result_manifest, result_payload) = read_pinned_provider_artifact(ctx, &result_source.artifact, "interrupted result")?;
    if !exact_interrupted_result_payload(&result_manifest.label, &result_payload, interrupted)? {
        return Err("interrupted release result does not close its exact native pending call".into());
    }
    let (closure_manifest, closure_payload) = read_pinned_provider_artifact(ctx, &closure.artifact, "interrupted closure")?;
    let labels = codex_context_output_labels(descriptor).map_err(|error| error.to_string())?;
    let value: serde_json::Value = serde_json::from_slice(&closure_payload).map_err(|error| error.to_string())?;
    let settlement_source = if let Some(evidence) = value.get("completed_source_settlement") {
        let turn = replay_provider_source(ctx, run, result_source.movement_seq)?
            .ok_or("completed interrupted release lost its admitted result input")?;
        if turn.envelope.artifact != result_source.artifact {
            return Err("completed interrupted release changed its result input".into());
        }
        let pinned = ctx.pinned_run_record_bytes(&turn.envelope)?;
        let session = ProviderHandoffSession::new(ctx)?;
        let handoff = wait_for_provider_handoff(ctx, descriptor, &session, run, &turn.envelope, turn.seq, &pinned).await?;
        let (record, outcomes) = handoff.completed_source_settlement_predecessor(&closure.artifact).await?;
        let [ProviderHandoffOutcome::Admitted { seq, .. }] = outcomes.as_slice()
            else { return Err("completed interrupted release has no single admitted predecessor".into()); };
        let context = current.iter().filter(|entry| entry.source_movement_seq.is_some_and(|value| value <= *seq)).cloned().collect::<Vec<_>>();
        let acknowledgement: octopus_ledger::ArtifactRef = serde_json::from_value(evidence["stop_artifact"].clone()).map_err(|error| error.to_string())?;
        let acknowledgement_seq = evidence["stop_movement_seq"].as_u64().ok_or("completed interrupted release has no Stop ACK sequence")?;
        let control_source = evidence["stop_source_movement_seq"].as_u64().ok_or("completed interrupted release has no Stop input sequence")?;
        let ack = context.iter().find(|entry| entry.source_artifact == acknowledgement && entry.source_movement_seq == Some(acknowledgement_seq))
            .ok_or("completed interrupted release has no exact checked Stop ACK")?;
        let control_id = ack.payload["control_id"].as_str().ok_or("completed interrupted release has no Stop identity")?;
        let proof = crate::handler::stopped_result::prove_interrupted(descriptor, &turn.envelope, turn.seq,
            &record, &context, &outcomes, &crate::handler::stopped_result::InterruptedResultSource {
                continuity: interrupted, control_source, control_id, acknowledgement: &acknowledgement, acknowledgement_seq,
            }).map_err(|error| error.to_string())?;
        validate_completed_interrupted_result_control(ctx, descriptor, run, interrupted, &proof)?;
        crate::handler::stopped_result::validate_saved_settlement(&proof, &turn.envelope, &value, &labels)?;
        Some(proof.source)
    } else { None };
    let checked = crate::handler::checked_interrupted_context_revision(interrupted, current, &labels, Some(closure.movement_seq))?;
    let closed = interrupted_result_closure_payload(&closure_manifest.label, &closure_payload,
        settlement_source.as_ref().unwrap_or(&checked), &result_source.artifact, &labels)?
        .ok_or("interrupted release lost its exact native closure payload")?;
    let mut evidence = current.to_vec();
    let result_payload: serde_json::Value = serde_json::from_slice(&result_payload).map_err(|error| error.to_string())?;
    if let Some(existing) = evidence.iter().find(|entry| entry.source_artifact == result_source.artifact) {
        if existing.payload != result_payload { return Err("interrupted release result conflicts with checked current evidence".into()); }
    } else {
        evidence.push(octopus_run_authoring::SessionContextArtifact {
            label: result_manifest.label,
            payload: result_payload, source_artifact: result_source.artifact.clone(),
            source_movement_seq: Some(result_source.movement_seq),
        });
    }
    let settled = reconcile_operator_session_continuity(descriptor, input, Some(interrupted.clone()), &evidence)
        .map_err(|error| error.to_string())?;
    if settled.as_ref().is_some_and(codex_continuity_waits_for_result) {
        return settled.ok_or("interrupted release lost its current continuity".into());
    }
    reconcile_delivered_interrupted_continuity(Some(interrupted), settled, Some(closed))
        .map_err(|error| error.to_string())?.ok_or("interrupted release lost its checked closed continuity".into())
}

pub(crate) fn read_provider_source_movement(
    ctx: &InterfaceLiftContext, run: &RunId, seq: u64,
) -> Result<octopus_ledger::Movement, String> {
    let (mut file, snapshot_len, max_row_bytes) = open_arbitration_snapshot(ctx)?;
    let decoder = DurableMovementDecoder::for_log(&ctx.movements_path);
    let (offset, expected_seq) = authoritative_source_scan_start(ctx, &mut file, snapshot_len, max_row_bytes, &decoder, seq);
    let scan = scan_authoritative_snapshot(ctx, &mut file, snapshot_len, max_row_bytes, &decoder, offset, expected_seq, None,
        |movement| Ok((movement.seq.0 == seq).then_some(movement)))?;
    ensure_arbitration_snapshot_length(ctx, &file, snapshot_len)?;
    let movement = scan.found.ok_or_else(|| format!("interrupted result source {seq} is absent from the authoritative ledger"))?;
    if !movement_is_provider_source(ctx, run, &movement) { return Err("interrupted result source crossed its provider/run coordinates".into()); }
    Ok(movement)
}

pub(crate) async fn recover_deferred_failed_result(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    run: &RunId,
    turn: &QueuedTurn,
    continuity: Option<&crate::handler::CodexContinuityState>,
    released_source: Option<u64>,
) -> Result<Option<u64>, String> {
    let Some(continuity) = continuity.filter(|state| codex_continuity_waits_for_result(state)) else { return Ok(None); };
    let pinned = ctx.pinned_run_record_bytes(&turn.envelope)?;
    let record = octopus_run_record::RunRecord::from_bytes(&pinned).map_err(|error| error.to_string())?;
    if !record.interface_state.get(&descriptor.id).is_some_and(|state| state.contains_key("run_authoring_binding")) {
        return Ok(None);
    }
    let session = ProviderHandoffSession::new(ctx)?;
    let handoff = wait_for_provider_handoff(ctx, descriptor, &session, run, &turn.envelope, turn.seq, &pinned).await?;
    let failed_sources = match released_source {
        Some(sequence) if handoff.recovered_failed_context_source(sequence).await? => vec![sequence],
        Some(_) => return Err("released failed result lost its checked recovery proof".into()),
        None => handoff.recovered_failed_context_sources().await?,
    };
    if failed_sources.is_empty() { return Ok(None); }
    let context = handoff.current_session_context_artifacts().await;
    let mut matched = None;
    for failed_source in failed_sources {
        let boundary = turn.seq.max(failed_source.checked_add(1).ok_or("failed result sequence overflowed")?);
        let settled_context = context.iter().filter(|entry| entry.source_artifact != turn.envelope.artifact
            && entry.source_movement_seq.is_some_and(|seq| seq < boundary)).cloned().collect::<Vec<_>>();
        if let Some(source) = recovered_failed_result_source(descriptor, &turn.envelope, continuity, &settled_context, &[failed_source])
            .map_err(|error| error.to_string())?
            && matched.replace(source).is_some()
        {
            return Err("deferred turn has multiple failed sources for one pending result".into());
        }
    }
    Ok(matched)
}

pub(crate) fn bound_operator_session_snapshot(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    turn: &QueuedTurn,
) -> Result<Option<(ArtifactRef, ProviderInvocationSessionSnapshot)>, String> {
    let labels = codex_session_snapshot_labels(descriptor).map_err(|error| error.to_string())?;
    let mut matched = None;
    let record = octopus_run_record::RunRecord::from_bytes(&ctx.pinned_run_record_bytes(&turn.envelope)?)
        .map_err(|error| error.to_string())?;
    let payloads = octopus_run_authoring::context_artifact_payloads(
        &record, turn.envelope.metadata().map_err(|error| error.to_string())?, &turn.envelope.artifact_binding_payloads,
    )?;
    let payloads = octopus_model_context_render::resolve_instruction_context(
        turn.envelope.metadata().map_err(|error| error.to_string())?, &payloads,
    )?.remaining_payloads;
    for bound in &payloads {
        let bytes = bound.read_bytes().map_err(|error| error.to_string())?;
        let value: serde_json::Value = serde_json::from_slice(bytes.as_ref())
            .map_err(|error| format!("deferred bound context '{}' is not JSON: {error}", bound.artifact.id))?;
        let Some(session) = value.get("session_context") else {
            continue;
        };
        if session
            .get("provider_interface")
            .and_then(serde_json::Value::as_str)
            != Some(ctx.interface.as_str())
        {
            continue;
        }
        let artifacts = resolve_session_snapshot_chain(
            &ctx.blobs_dir,
            ctx.required_host_io_limits()?,
            &bound.artifact,
            bytes.as_ref(),
            None,
            &labels,
            &ctx.interface,
        )?;
        let snapshot = ProviderInvocationSessionSnapshot {
            carrier: bound.artifact.clone(),
            artifacts: Arc::new(artifacts),
        };
        if matched
            .replace((bound.artifact.clone(), snapshot))
            .is_some()
        {
            return Err(format!(
                "deferred provider turn {} carries multiple current session snapshots",
                turn.seq
            ));
        }
    }
    Ok(matched)
}

pub(crate) fn rebase_deferred_provider_turn(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    run: &RunId,
    turn: &QueuedTurn,
    release: &DeferredProviderTurnRelease,
) -> Result<(QueuedTurn, Option<crate::handler::CodexContinuityState>), String> {
    let DeferredProviderTurnRelease::SessionRebase {
        source_session_carrier,
        rebased_session_carrier,
    } = release
    else {
        return Err(format!(
            "deferred provider turn {} cannot rebase an original-envelope release",
            turn.seq
        ));
    };
    validate_artifact_admission(ctx, run, turn.seq, rebased_session_carrier)?;
    let labels = codex_session_snapshot_labels(descriptor).map_err(|error| error.to_string())?;
    let payload = read_deferred_artifact_payload(ctx, &rebased_session_carrier.artifact)?;
    let artifacts = resolve_session_snapshot_chain(
        &ctx.blobs_dir,
        ctx.required_host_io_limits()?,
        &rebased_session_carrier.artifact,
        &payload,
        Some(rebased_session_carrier.movement_seq),
        &labels,
        &ctx.interface,
    )?;
    let snapshot = ProviderInvocationSessionSnapshot {
        carrier: rebased_session_carrier.artifact.clone(),
        artifacts: Arc::new(artifacts),
    };
    let pinned = ctx.pinned_run_record_bytes(&turn.envelope)?;
    let record = octopus_run_record::RunRecord::from_bytes(&pinned).map_err(|error| error.to_string())?;
    let selected = octopus_run_authoring::payload_artifact_bindings(&record, &ctx.interface)?;
    octopus_run_authoring::context_artifact_payloads(
        &record, turn.envelope.metadata().map_err(|error| error.to_string())?, &turn.envelope.artifact_binding_payloads,
    )?;
    let mut envelope = turn.envelope.clone();
    rebase_deferred_session_bindings(
        turn.seq,
        &mut envelope,
        source_session_carrier.as_ref(),
        &rebased_session_carrier.artifact,
        payload,
        &snapshot,
        &selected,
    )?;
    let continuity = recover_provider_turn_continuity(descriptor, &envelope, Some(&snapshot), run, &ctx.pinned_run_record_bytes(&envelope)?)
        .map_err(|error| error.to_string())?;
    Ok((
        QueuedTurn {
            envelope,
            seq: turn.seq,
        },
        continuity,
    ))
}

pub(crate) fn rebase_deferred_session_bindings(
    turn_seq: u64,
    envelope: &mut InterfaceInvocationEnvelope,
    source_session_carrier: Option<&ArtifactRef>,
    rebased_session_carrier: &ArtifactRef,
    rebased_payload: Vec<u8>,
    snapshot: &ProviderInvocationSessionSnapshot,
    selected: &[ArtifactRef],
) -> Result<(), String> {
    let metadata = envelope
        .metadata
        .as_mut()
        .ok_or_else(|| format!("deferred provider turn {turn_seq} lost its invocation metadata"))?;
    if envelope.artifact_binding_payloads.len() != metadata.artifact_binding_refs.len()
        || envelope
            .artifact_binding_payloads
            .iter()
            .zip(&metadata.artifact_binding_refs)
            .any(|(bound, reference)| &bound.artifact != reference)
    {
        return Err(format!(
            "deferred provider turn {turn_seq} binding payloads disagree with invocation metadata"
        ));
    }

    if selected.contains(rebased_session_carrier)
        || source_session_carrier.is_some_and(|reference| selected.contains(reference))
    {
        return Err(format!("deferred provider turn {turn_seq} selected payload collides with a session carrier"));
    }
    let mut binding_matches = 0;
    if let Some(source_session_carrier) = source_session_carrier {
        for (bound, reference) in envelope
            .artifact_binding_payloads
            .iter_mut()
            .zip(&mut metadata.artifact_binding_refs)
        {
            if &bound.artifact != source_session_carrier {
                continue;
            }
            *bound = octopus_interfaces::ArtifactBindingPayload::new(
                rebased_session_carrier.clone(),
                rebased_payload.clone(),
            );
            *reference = rebased_session_carrier.clone();
            binding_matches += 1;
        }
    } else {
        envelope.artifact_binding_payloads.insert(
            0,
            octopus_interfaces::ArtifactBindingPayload::new(
                rebased_session_carrier.clone(),
                rebased_payload,
            ),
        );
        metadata
            .artifact_binding_refs
            .insert(0, rebased_session_carrier.clone());
        binding_matches = 1;
    }
    if binding_matches != 1 {
        return Err(format!(
            "deferred provider turn {turn_seq} cannot identify one exact source session binding"
        ));
    }

    let subsumed_sources = snapshot
        .artifacts
        .iter()
        .filter(|artifact| &artifact.source_artifact != rebased_session_carrier)
        .map(|artifact| {
            (
                artifact.source_artifact.id.as_str().to_string(),
                artifact.source_artifact.content.to_hex(),
            )
        })
        .collect::<BTreeSet<_>>();
    let mut retained_payloads = Vec::with_capacity(envelope.artifact_binding_payloads.len());
    let mut retained_refs = Vec::with_capacity(metadata.artifact_binding_refs.len());
    for (bound, reference) in std::mem::take(&mut envelope.artifact_binding_payloads)
        .into_iter()
        .zip(std::mem::take(&mut metadata.artifact_binding_refs))
    {
        let key = (
            bound.artifact.id.as_str().to_string(),
            bound.artifact.content.to_hex(),
        );
        if bound.artifact != *rebased_session_carrier && !selected.contains(&bound.artifact) && subsumed_sources.contains(&key) {
            continue;
        }
        retained_payloads.push(bound);
        retained_refs.push(reference);
    }
    envelope.artifact_binding_payloads = retained_payloads;
    metadata.artifact_binding_refs = retained_refs;

    let rebased_matches = envelope
        .artifact_binding_payloads
        .iter()
        .filter(|bound| bound.artifact == *rebased_session_carrier)
        .count();
    if rebased_matches != 1
        || envelope.artifact_binding_payloads.len() != metadata.artifact_binding_refs.len()
        || envelope
            .artifact_binding_payloads
            .iter()
            .zip(&metadata.artifact_binding_refs)
            .any(|(bound, reference)| &bound.artifact != reference)
    {
        return Err(format!(
            "deferred provider turn {turn_seq} produced an inconsistent rebased session binding"
        ));
    }
    Ok(())
}
