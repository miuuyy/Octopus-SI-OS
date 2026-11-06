use super::*;

const DIRECTORY: &str = "completed-outputs";
const RETAINED_REHAND_INTERVAL_MS: u64 = 100;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct InputIdentity {
    sequence: u64,
    artifact: ArtifactRef,
    label: String,
    payload_hash: ContentHash,
    metadata: Option<InvocationMetadata>,
}

impl InputIdentity {
    fn capture(input: &ToolProcessEnvelope) -> Self {
        Self {
            sequence: input.source_movement_seq,
            artifact: input.artifact.clone(),
            label: input.label.clone(),
            payload_hash: ContentHash::of_bytes(&input.payload),
            metadata: input.metadata.clone(),
        }
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Output {
    manifest_path: PathBuf,
    label: String,
    manifest_hash: ContentHash,
    payload_hash: ContentHash,
    admission: OutputAdmission,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dispatch: Option<OutputIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    delivery_id: Option<String>,
    #[serde(default)]
    callback_completed: bool,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct OutputIdentity {
    manifest_path: PathBuf,
    label: String,
    manifest_hash: ContentHash,
    payload_hash: ContentHash,
}

impl OutputIdentity {
    fn artifact(&self, ctx: &InterfaceLiftContext) -> Result<InterfaceArtifactFile, String> {
        let artifact = capture_spawned_artifact(&self.manifest_path, &[ctx.interface_var_dir()], ctx.required_host_io_limits()?)?;
        let (observed, artifact) = Self::capture(ctx, &artifact)?;
        if observed != *self { return Err("retained dispatch changed its exact manifest or payload".into()); }
        Ok(artifact)
    }

    fn delivery_id(&self, input: &InputIdentity) -> Result<String, String> {
        let bytes = serde_json::to_vec(&("completed-output-delivery-v4", input, self)).map_err(|e| e.to_string())?;
        Ok(ContentHash::of_bytes(&bytes).to_hex())
    }

    fn capture(ctx: &InterfaceLiftContext, artifact: &InterfaceArtifactFile) -> Result<(Self, InterfaceArtifactFile), String> {
        let limits = ctx.required_host_io_limits()?;
        let artifact = capture_spawned_artifact(artifact.manifest_path(), &[ctx.interface_var_dir()], limits)?;
        let captured = artifact.read_bounded(limits.max_manifest_bytes, limits.max_payload_bytes).map_err(|e| e.to_string())?;
        Ok((Self {
            manifest_path: artifact.manifest_path().to_path_buf(), label: captured.manifest.label,
            manifest_hash: ContentHash::of_bytes(&captured.manifest_bytes),
            payload_hash: ContentHash::of_bytes(&captured.payload_bytes),
        }, artifact))
    }

    fn sync(&self, artifact: &InterfaceArtifactFile, ctx: &InterfaceLiftContext) -> Result<(), String> {
        let manifest = artifact.read_manifest_bounded(ctx.required_host_io_limits()?.max_manifest_bytes).map_err(|e| e.to_string())?;
        for path in [artifact.manifest_path(), manifest.manifest.payload_path.as_path()] {
            let mut options = std::fs::OpenOptions::new();
            options.read(true);
            #[cfg(unix)] {
                use std::os::unix::fs::OpenOptionsExt;
                options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
            }
            let file = options.open(path).map_err(|e| e.to_string())?;
            if !file.metadata().map_err(|e| e.to_string())?.is_file() { return Err("dispatched output carrier is not a regular file".into()); }
            octopus_file_storage::sync_ordered(&file).map_err(|e| e.to_string())?;
            sync_completed_output_parent(path.parent().ok_or("dispatched output has no parent")?)?;
        }
        if Self::capture(ctx, artifact)?.0 != *self { return Err("dispatched output changed while retaining its carrier".into()); }
        Ok(())
    }
}

impl Output {
    fn admission_identity(&self) -> OutputIdentity {
        self.dispatch.clone().unwrap_or_else(|| OutputIdentity {
            manifest_path: self.manifest_path.clone(), label: self.label.clone(),
            manifest_hash: self.manifest_hash, payload_hash: self.payload_hash,
        })
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
enum OutputAdmission {
    Unattempted,
    AwaitingVerdict { ledger_offset: u64 },
    Retained { scope_seq: Option<u64> },
    Settled { verdict: DeliveryVerdict },
}

enum RecordedDelivery {
    Settled(DeliveryVerdict),
    Pending,
    AwaitingCurrentSource,
}

enum RecoveryProgress {
    Ready,
    AwaitingLedgerTail,
    LegacyUnsubmitted { indices: Vec<usize>, ledger_length: u64 },
}

fn ledger_snapshot(ctx: &InterfaceLiftContext) -> Result<(std::io::BufReader<std::io::Take<std::fs::File>>, u64), String> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)] {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(&ctx.movements_path).map_err(|e| e.to_string())?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() { return Err("completed-output ledger is not a regular file".into()); }
    let length = metadata.len();
    Ok((std::io::BufReader::new(file.take(length)), length))
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CompletedOutputs {
    schema_version: u32,
    input: InputIdentity,
    formation: Option<WorkerEnvironmentFormation>,
    generation: PathBuf,
    invocation_manifest: PathBuf,
    outputs: Vec<Output>,
}

fn directory(var: &Path) -> PathBuf {
    var.join(".tool-process-host-state").join(DIRECTORY)
}

fn read_receipt(path: &Path, maximum: u64) -> Result<CompletedOutputs, String> {
    validate_tool_door_journal_metadata(path, &std::fs::symlink_metadata(path).map_err(|e| e.to_string())?)?;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)] {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|e| e.to_string())?;
    validate_tool_door_journal_metadata(path, &file.metadata().map_err(|e| e.to_string())?)?;
    let mut bytes = Vec::new();
    file.take(maximum.saturating_add(1)).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > maximum { return Err("completed-output receipt exceeds the host control-frame ceiling".into()); }
    let mut receipt: CompletedOutputs = serde_json::from_slice(&bytes).map_err(|e| format!("invalid completed-output receipt: {e}"))?;
    if !matches!(receipt.schema_version, 2 | 3 | 4) || receipt.outputs.is_empty()
        || receipt_name(&receipt)? != path.file_name().ok_or("receipt has no filename")?
    {
        return Err("completed-output receipt has an invalid version, source or output set".into());
    }
    for output in &mut receipt.outputs {
        if receipt.schema_version < 4 && (output.delivery_id.is_some() || matches!(output.admission, OutputAdmission::Retained { .. })) {
            return Err("legacy completed-output receipt carries retained delivery state from another schema".into());
        }
        if receipt.schema_version == 2 {
            if output.dispatch.is_some() || output.callback_completed {
                return Err("legacy completed-output receipt carries dispatch state from another schema".into());
            }
            output.callback_completed = matches!(output.admission, OutputAdmission::Settled { .. });
        } else if matches!(output.admission, OutputAdmission::AwaitingVerdict { .. } | OutputAdmission::Retained { .. }) && output.dispatch.is_none()
            || matches!(output.admission, OutputAdmission::Unattempted) && output.dispatch.is_some()
            || matches!(output.admission, OutputAdmission::Settled { .. }) && output.dispatch.is_none() && !output.callback_completed
            || output.callback_completed && !matches!(output.admission, OutputAdmission::Settled { .. })
        {
            return Err("completed-output receipt has inconsistent dispatch state".into());
        }
        if receipt.schema_version == 4 {
            match &output.admission {
                OutputAdmission::Unattempted if output.delivery_id.is_none() => {},
                OutputAdmission::Retained { .. } | OutputAdmission::Settled { .. } if output.delivery_id.is_some() => {
                    let identity = output.dispatch.as_ref().ok_or("retained delivery has no exact dispatch identity")?;
                    if output.delivery_id.as_deref() != Some(identity.delivery_id(&receipt.input)?.as_str()) {
                        return Err("completed-output retained delivery identity differs from its exact input and dispatch".into());
                    }
                }
                OutputAdmission::Settled { verdict } if !verdict.awaits_current_source() => {},
                _ => return Err("completed-output receipt has inconsistent retained delivery state".into()),
            }
        }
    }
    Ok(receipt)
}

fn receipt_paths(var: &Path) -> Result<Vec<PathBuf>, String> {
    let root = directory(var);
    let entries = match std::fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.to_string()),
    };
    if root.canonicalize().map_err(|e| e.to_string())? != root {
        return Err("completed-output receipt directory is aliased".into());
    }
    let mut paths = Vec::new();
    for entry in entries {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.extension().is_some_and(|extension| extension == "json") {
            receipt_coordinate(&path)?;
            validate_tool_door_journal_metadata(&path, &std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?)?;
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

fn receipt_name(receipt: &CompletedOutputs) -> Result<std::ffi::OsString, String> {
    Ok(format!("{}-{}.json", receipt.input.sequence, tool_process_generation_name(&receipt.generation)?).into())
}

fn receipt_coordinate(path: &Path) -> Result<(u64, &str), String> {
    let (sequence, generation) = path.file_stem().and_then(|s| s.to_str()).and_then(|s| s.split_once('-'))
        .ok_or("completed-output receipt filename is malformed")?;
    let sequence = sequence.parse().map_err(|_| "completed-output source sequence is malformed")?;
    if generation.len() != 64 || !generation.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("completed-output generation is malformed".into());
    }
    Ok((sequence, generation))
}

impl CompletedOutputs {
    pub(super) fn load(ctx: &InterfaceLiftContext, sequence: u64) -> Result<Option<Self>, String> {
        let mut selected = None;
        for path in receipt_paths(&ctx.interface_var_dir())? {
            if receipt_coordinate(&path)?.0 == sequence && selected.replace(path).is_some() {
                return Err("one completed input has conflicting generation receipts".into());
            }
        }
        selected.map(|path| read_receipt(&path, ctx.required_host_io_limits()?.max_control_frame_bytes)).transpose()
    }

    pub(super) fn retain(
        ctx: &InterfaceLiftContext,
        input: &ToolProcessEnvelope,
        completion: &ToolProcessCompletion,
    ) -> Result<Self, String> {
        if Self::load(ctx, input.source_movement_seq)?.is_some() {
            return Err("completed tool invocation already has a retained output receipt".into());
        }
        let limits = ctx.required_host_io_limits()?;
        let mut outputs = Vec::new();
        for artifact in &completion.artifacts {
            let captured = artifact.read_bounded(limits.max_manifest_bytes, limits.max_payload_bytes)
                .map_err(|e| e.to_string())?;
            for path in [artifact.manifest_path(), captured.manifest.payload_path.as_path()] {
                let mut options = std::fs::OpenOptions::new();
                options.read(true);
                #[cfg(unix)] {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
                }
                let file = options.open(path).map_err(|e| e.to_string())?;
                if !file.metadata().map_err(|e| e.to_string())?.is_file() {
                    return Err("completed output carrier is not a regular file".into());
                }
                octopus_file_storage::sync_ordered(&file).map_err(|e| e.to_string())?;
                sync_completed_output_parent(path.parent().ok_or("completed output has no parent")?)?;
            }
            outputs.push(Output {
                manifest_path: artifact.manifest_path().to_path_buf(),
                label: captured.manifest.label,
                manifest_hash: ContentHash::of_bytes(&captured.manifest_bytes),
                payload_hash: ContentHash::of_bytes(&captured.payload_bytes),
                admission: OutputAdmission::Unattempted,
                dispatch: None, delivery_id: None, callback_completed: false,
            });
        }
        let receipt = Self {
            schema_version: 4, input: InputIdentity::capture(input),
            formation: input.worker_environment.clone(), generation: completion.generation.clone(),
            invocation_manifest: completion.invocation_manifest.clone(), outputs,
        };
        receipt.save(ctx)?;
        Ok(receipt)
    }

    fn save(&self, ctx: &InterfaceLiftContext) -> Result<(), String> {
        let root = materialize_owner_private_descendant(&tool_process_host_private_root(ctx)?, &[DIRECTORY.into()])?;
        let path = root.join(receipt_name(self)?);
        let bytes = serde_json::to_vec(self).map_err(|e| e.to_string())?;
        if bytes.len() as u64 > ctx.required_host_io_limits()?.max_control_frame_bytes {
            return Err("completed-output receipt exceeds the host control-frame ceiling".into());
        }
        let (temporary, mut file) = create_tool_door_journal_temp(&root)?;
        let result = (|| {
            file.write_all(&bytes).map_err(|e| e.to_string())?;
            octopus_file_storage::sync_ordered(&file).map_err(|e| e.to_string())?;
            std::fs::rename(&temporary, &path).map_err(|e| e.to_string())?;
            sync_completed_output_parent(&root)
        })();
        if result.is_err() { let _ = std::fs::remove_file(temporary); }
        result
    }

    fn output(&self, ctx: &InterfaceLiftContext, index: usize) -> Result<InterfaceArtifactFile, String> {
        let output = &self.outputs[index];
        let limits = ctx.required_host_io_limits()?;
        let artifact = capture_spawned_artifact(&output.manifest_path, &[ctx.interface_var_dir()], limits)?;
        let captured = artifact.read_bounded(limits.max_manifest_bytes, limits.max_payload_bytes).map_err(|e| e.to_string())?;
        if ContentHash::of_bytes(&captured.manifest_bytes) != output.manifest_hash
            || ContentHash::of_bytes(&captured.payload_bytes) != output.payload_hash
            || captured.manifest.label != output.label
        {
            return Err("retained completed output changed its exact manifest or payload".into());
        }
        Ok(artifact)
    }

    pub(super) async fn deliver(
        &mut self,
        ctx: &InterfaceLiftContext,
        input: &ToolProcessEnvelope,
        callback: Option<&ToolDoorOutputCallback>,
        recover: bool,
    ) -> Result<ToolProcessBrokerResult, String> {
        if self.input != InputIdentity::capture(input) {
            return Err("completed output does not belong to the exact original invocation".into());
        }
        let mut source = input.output_source.clone();
        if let Some(source) = source.as_mut() { source.formation = self.formation.clone(); }
        if recover {
            let source = source.as_ref().ok_or("completed-output recovery requires the original admitted source")?;
            ctx.with_source_movement(source.movement.clone()).source_movement(&source.envelope)?;
        }
        if self.schema_version < 4 {
            let source = source.as_ref().ok_or("legacy completed-output recovery requires the original admitted source")?;
            let mut watch = None;
            loop {
                if ctx.cancel.is_cancelled() { return Err("completed-output ledger recovery was cancelled; receipt remains retained".into()); }
                match self.recover_admissions(ctx, source)? {
                    RecoveryProgress::Ready => break,
                    RecoveryProgress::AwaitingLedgerTail => match watch.as_mut() {
                        None => watch = Some(door_life::ledger_change_watch(&ctx.movements_path)?),
                        Some(watch) => watch.wait(&ctx.cancel, &ctx.movements_path).await?,
                    },
                    RecoveryProgress::LegacyUnsubmitted { indices, ledger_length } => {
                        let handler = callback.ok_or("completed output admission has no exact verdict; retained without retry")?;
                        for index in &indices {
                            let artifact = self.output(ctx, *index)?;
                            if !handler.can_resume_unsubmitted(ctx, source, &artifact)? {
                                return Err("completed output admission has no exact verdict; retained without retry".into());
                            }
                        }
                        match self.recover_admissions(ctx, source)? {
                            RecoveryProgress::LegacyUnsubmitted { indices: observed, ledger_length: length }
                                if observed == indices && length == ledger_length => {}
                            _ => return Err("legacy completed-output evidence changed during unsubmitted proof".into()),
                        }
                        for index in indices { self.outputs[index].admission = OutputAdmission::Unattempted; }
                        break;
                    }
                }
            }
            if self.outputs.iter().any(|output| matches!(output.admission, OutputAdmission::Unattempted)) {
                self.schema_version = 4;
                self.save(ctx)?;
            }
        }
        let mut refused = None;
        for index in 0..self.outputs.len() {
            loop {
                match &self.outputs[index].admission {
                    OutputAdmission::Settled { verdict } if verdict.awaits_current_source() => {
                        if self.outputs[index].delivery_id.is_some() {
                            self.outputs[index].admission = OutputAdmission::Retained { scope_seq: None };
                            self.outputs[index].callback_completed = false;
                            self.save(ctx)?;
                        }
                        return Err("completed output remains retained while its current source is unresolved".into());
                    }
                    OutputAdmission::Retained { .. } => {
                        self.resume_retained(ctx, index).await?;
                        continue;
                    }
                    OutputAdmission::Settled { verdict: DeliveryVerdict::Admitted { .. } }
                        if callback.is_none() || self.outputs[index].callback_completed => break,
                    OutputAdmission::Settled { verdict: DeliveryVerdict::Refused { reason, detail } }
                        if callback.is_none() || self.outputs[index].callback_completed => {
                        refused.get_or_insert((*reason, detail.clone()));
                        break;
                    }
                    OutputAdmission::AwaitingVerdict { .. } => return Err("completed output admission has no exact verdict; retained without retry".into()),
                    _ => {}
                }
                let artifact = self.output(ctx, index)?;
                if ctx.cancel.is_cancelled() { return Err("completed output remains retained while admission is cancelled".into()); }
                let retained = Arc::new(tokio::sync::Mutex::new(self.clone()));
                let dispatch = Self::dispatch(Arc::clone(&retained), ctx.clone(), index);
                let result = hand_tool_process_output_with_dispatch(ctx, source.as_ref(), callback, artifact, dispatch).await;
                *self = retained.lock().await.clone();
                let verdict = result?;
                if !matches!(&self.outputs[index].admission, OutputAdmission::Settled { verdict: saved } if *saved == verdict) {
                    return Err("completed-output callback returned without the exact tracked dispatch verdict".into());
                }
                self.outputs[index].callback_completed = true;
                self.save(ctx)?;
            }
        }
        Ok(match refused {
            Some((reason, detail)) => ToolProcessBrokerResult::Refused { reason, detail },
            None => ToolProcessBrokerResult::Completed,
        })
    }

    fn dispatch(retained: Arc<tokio::sync::Mutex<Self>>, owner_ctx: InterfaceLiftContext, index: usize) -> ToolDoorOutputDispatch {
        Arc::new(move |ctx, artifact| {
            let retained = Arc::clone(&retained);
            let owner_ctx = owner_ctx.clone();
            Box::pin(async move {
                if ctx.interface != owner_ctx.interface || ctx.interface_var_dir() != owner_ctx.interface_var_dir()
                    || ctx.movements_path != owner_ctx.movements_path || ctx.blobs_dir != owner_ctx.blobs_dir
                { return Err("completed-output dispatch changed its original door owner".into()); }
                let (identity, artifact) = OutputIdentity::capture(&owner_ctx, &artifact)?;
                let mut receipt = retained.lock().await;
                let output = &receipt.outputs[index];
                if output.dispatch.as_ref().is_some_and(|saved| *saved != identity) {
                    return Err("completed-output dispatch changed its pinned delivered artifact".into());
                }
                match &output.admission {
                    OutputAdmission::AwaitingVerdict { .. } => return Err("completed output admission has no exact verdict; retained without retry".into()),
                    OutputAdmission::Settled { verdict } if verdict.awaits_current_source() => {
                        return Err("completed output remains retained while its current source is unresolved".into());
                    }
                    OutputAdmission::Settled { verdict } => return Ok(verdict.clone()),
                    _ => {}
                }
                if owner_ctx.cancel.is_cancelled() { return Err("completed output remains retained while admission is cancelled".into()); }
                identity.sync(&artifact, &owner_ctx)?;
                let delivery_id = identity.delivery_id(&receipt.input)?;
                receipt.schema_version = 4;
                let output = &mut receipt.outputs[index];
                output.dispatch = Some(identity);
                if output.delivery_id.as_ref().is_some_and(|saved| saved != &delivery_id) {
                    return Err("completed-output dispatch changed its retained delivery identity".into());
                }
                output.delivery_id = Some(delivery_id.clone());
                output.callback_completed = false;
                if matches!(output.admission, OutputAdmission::Unattempted) {
                    output.admission = OutputAdmission::Retained { scope_seq: None };
                }
                receipt.save(&owner_ctx)?;
                drop(receipt);
                Self::hand_retained(&retained, &ctx, &owner_ctx, index, artifact, &delivery_id).await
            })
        })
    }

    async fn resume_retained(&mut self, ctx: &InterfaceLiftContext, index: usize) -> Result<DeliveryVerdict, String> {
        let output = &self.outputs[index];
        let identity = output.dispatch.as_ref().ok_or("retained delivery has no exact dispatch identity")?;
        let delivery_id = output.delivery_id.clone().ok_or("retained delivery has no retry identity")?;
        if delivery_id != identity.delivery_id(&self.input)? {
            return Err("completed-output retained delivery identity changed".into());
        }
        let artifact = identity.artifact(ctx)?;
        let retained = tokio::sync::Mutex::new(self.clone());
        let result = Self::hand_retained(&retained, ctx, ctx, index, artifact, &delivery_id).await;
        *self = retained.into_inner();
        result
    }

    async fn hand_retained(
        retained: &tokio::sync::Mutex<Self>,
        ctx: &InterfaceLiftContext,
        owner_ctx: &InterfaceLiftContext,
        index: usize,
        artifact: InterfaceArtifactFile,
        delivery_id: &str,
    ) -> Result<DeliveryVerdict, String> {
        let mut watch = door_life::ledger_change_watch(&owner_ctx.movements_path)?;
        loop {
            let delivery = tokio::select! {
                biased;
                _ = owner_ctx.cancel.cancelled() => return Err("completed output remains retained while admission is cancelled".into()),
                delivery = ctx.hand_arrival_retained(artifact.clone(), Vec::new(), delivery_id) => delivery?,
            };
            if let RecordedDelivery::Settled(verdict) = retained.lock().await.record_delivery(owner_ctx, index, delivery)? {
                return Ok(verdict);
            }
            tokio::select! {
                biased;
                _ = owner_ctx.cancel.cancelled() => return Err("completed output remains retained while admission is cancelled".into()),
                changed = watch.wait(&owner_ctx.cancel, &owner_ctx.movements_path) => changed?,
                _ = tokio::time::sleep(std::time::Duration::from_millis(RETAINED_REHAND_INTERVAL_MS)) => {}
            }
        }
    }

    fn record_delivery(&mut self, ctx: &InterfaceLiftContext, index: usize, delivery: octopus_ledger::ArtifactDelivery) -> Result<RecordedDelivery, String> {
        let output = &mut self.outputs[index];
        let recorded = match (&output.admission, delivery) {
            (OutputAdmission::Retained { .. }, octopus_ledger::ArtifactDelivery::Complete { verdict }) if verdict.awaits_current_source() => {
                return Ok(RecordedDelivery::AwaitingCurrentSource);
            }
            (OutputAdmission::Retained { scope_seq: prior }, octopus_ledger::ArtifactDelivery::Pending { scope_seq }) => {
                if prior.is_some_and(|prior| prior != scope_seq) {
                    return Err("completed-output retained delivery changed its original candidate scope".into());
                }
                if *prior == Some(scope_seq) {
                    return Ok(RecordedDelivery::Pending);
                }
                output.admission = OutputAdmission::Retained { scope_seq: Some(scope_seq) };
                RecordedDelivery::Pending
            }
            (OutputAdmission::Retained { .. }, octopus_ledger::ArtifactDelivery::Complete { verdict }) => {
                output.admission = OutputAdmission::Settled { verdict: verdict.clone() };
                RecordedDelivery::Settled(verdict)
            }
            (OutputAdmission::Settled { verdict: saved }, octopus_ledger::ArtifactDelivery::Complete { verdict }) if *saved == verdict => {
                RecordedDelivery::Settled(verdict)
            }
            _ => return Err("completed-output retained delivery returned conflicting completion evidence".into()),
        };
        self.save(ctx)?;
        Ok(recorded)
    }

    fn recover_admissions(&mut self, ctx: &InterfaceLiftContext, source: &ToolDoorOutputSource) -> Result<RecoveryProgress, String> {
        ctx.with_source_movement(source.movement.clone()).source_movement(&source.envelope)?;
        if self.schema_version >= 4 { return Err("retained delivery must recover through its original runtime scope".into()); }
        let mut indices = Vec::new();
        for (index, output) in self.outputs.iter().enumerate() {
            match &output.admission {
                OutputAdmission::AwaitingVerdict { .. } if self.schema_version == 2 && output.dispatch.is_none() => indices.push(index),
                OutputAdmission::AwaitingVerdict { .. } => return Err("legacy completed output has no exact verdict for its original fanout; retained without retry".into()),
                OutputAdmission::Settled { verdict } if verdict.awaits_current_source() => {
                    return Err("legacy completed output has no exact verdict for its original fanout; retained without retry".into());
                }
                OutputAdmission::Retained { .. } => return Err("legacy completed output carries a retained scope from another schema".into()),
                _ => {},
            }
        }
        if indices.is_empty() { return Ok(RecoveryProgress::Ready); }
        let limits = ctx.required_host_io_limits()?;
        let (mut reader, ledger_len) = ledger_snapshot(ctx)?;
        if indices.iter().any(|index| matches!(self.outputs[*index].admission,
            OutputAdmission::AwaitingVerdict { ledger_offset } if ledger_offset > ledger_len)) {
            return Err("completed-output ledger was truncated below an attempted admission".into());
        }
        let mut line = Vec::new();
        let mut sequence = 0;
        let mut offset = 0;
        let mut partial_tail = false;
        let mut source_seen = false;
        while offset < ledger_len {
            let start = offset;
            let read = read_bounded_tool_door_ledger_line(&mut reader, &mut line, limits.max_control_frame_bytes)?;
            if read == 0 { return Err("completed-output ledger ended before its inspected boundary".into()); }
            offset += read as u64;
            let Some(row) = line.strip_suffix(b"\n") else { partial_tail = true; break; };
            let route = decode_durable_movement_route(row, sequence).map_err(|e| e.to_string())?;
            if route.seq.0 != sequence as u64 { return Err("completed-output recovery ledger sequence changed".into()); }
            sequence += 1;
            if route.seq == source.movement.seq {
                let observed = decode_tool_door_recovery_movement(&ctx.movements_path, row, sequence - 1, limits.max_control_frame_bytes)?;
                if observed != source.movement { return Err("completed-output recovery source differs from its admitted movement".into()); }
                source_seen = true;
                continue;
            }
            if route.seq.0 <= self.input.sequence || route.from != Endpoint::Interface(ctx.interface.clone()) { continue; }
            let movement = decode_tool_door_recovery_movement(&ctx.movements_path, row, sequence - 1, limits.max_control_frame_bytes)?;
            if indices.iter().any(|index| {
                let output = &self.outputs[*index];
                let OutputAdmission::AwaitingVerdict { ledger_offset } = output.admission else { return false; };
                let identity = output.admission_identity();
                start >= ledger_offset && movement.artifact.as_ref().is_some_and(|artifact| {
                    artifact.content == identity.manifest_hash
                        && artifact.id.as_str() == format!("{}:{}", identity.label, identity.manifest_hash.to_hex())
                })
            }) {
                return Err("legacy submitted output has no retained original candidate scope; an aggregate edge verdict cannot complete its fanout".into());
            }
        }
        let observed_length = reader.get_ref().get_ref().metadata().map_err(|e| e.to_string())?.len();
        if observed_length < ledger_len { return Err("completed-output ledger was truncated during recovery".into()); }
        if !source_seen { return Err("completed-output recovery has no exact admitted input".into()); }
        if partial_tail { return Ok(RecoveryProgress::AwaitingLedgerTail); }
        if observed_length != ledger_len { return Err("legacy completed-output ledger is not a stable complete proof".into()); }
        Ok(RecoveryProgress::LegacyUnsubmitted { indices, ledger_length: ledger_len })
    }
}

pub(super) fn retains_generation(generation: &Path) -> Result<bool, String> {
    let var = generation.parent().and_then(Path::parent).and_then(Path::parent).ok_or("generation has no interface owner")?;
    for path in receipt_paths(var)? {
        if receipt_coordinate(&path)?.1 == tool_process_generation_name(generation)? { return Ok(true); }
    }
    Ok(false)
}

pub(super) fn prune_before(var: &Path, sequence: u64, maximum: u64) -> Result<(), String> {
    for path in receipt_paths(var)? {
        let receipt = read_receipt(&path, maximum)?;
        if receipt.input.sequence < sequence {
            std::fs::remove_file(&path).map_err(|e| e.to_string())?;
            sync_completed_output_parent(path.parent().ok_or("receipt has no parent")?)?;
        }
    }
    Ok(())
}

fn original_source(ctx: &InterfaceLiftContext, sequence: u64) -> Result<ToolDoorOutputSource, String> {
    let limits = ctx.required_host_io_limits()?;
    let mut reader = std::io::BufReader::new(std::fs::File::open(&ctx.movements_path).map_err(|e| e.to_string())?);
    let mut row = Vec::new();
    let mut next = 0;
    let movement = loop {
        if read_bounded_tool_door_ledger_line(&mut reader, &mut row, limits.max_control_frame_bytes)? == 0 {
            return Err("completed-output source is absent from the durable ledger".into());
        }
        let row = row.strip_suffix(b"\n").ok_or("completed-output source ledger has an incomplete row")?;
        let route = decode_durable_movement_route(row, next).map_err(|e| e.to_string())?;
        if route.seq.0 != next as u64 { return Err("completed-output source ledger is non-contiguous".into()); }
        if route.seq.0 == sequence {
            break decode_tool_door_recovery_movement(&ctx.movements_path, row, next, limits.max_control_frame_bytes)?;
        }
        next += 1;
    };
    if movement.kind != MovementKind::InterfaceOutbound || movement.to != Endpoint::Interface(ctx.interface.clone()) {
        return Err("completed-output source is not an admitted input to this door".into());
    }
    let metadata = movement.invocation_metadata.as_ref().and_then(|m| m.invocation())
        .ok_or("completed-output source has no invocation metadata")?.clone();
    let artifact = movement.artifact.clone().ok_or("completed-output source has no artifact")?;
    let (file, _) = tool_door_recovery_artifact(ctx, &artifact)?;
    let file = file.capture_bounded(limits.max_manifest_bytes, limits.max_payload_bytes).map_err(|e| e.to_string())?;
    let mut remaining = limits.max_payload_bytes;
    let artifact_binding_payloads = metadata.artifact_binding_refs.iter().map(|reference| {
        let path = ctx.blobs_dir.join(reference.content.to_hex());
        door_life::resolve_artifact_binding_payload(&path, reference, limits, &mut remaining)
    }).collect::<Result<Vec<_>, _>>()?;
    let envelope = InterfaceInvocationEnvelope { artifact, artifact_file: file, artifact_binding_payloads, metadata: Some(metadata) };
    ctx.with_source_movement(movement.clone()).source_movement(&envelope)?;
    Ok(ToolDoorOutputSource { movement, envelope, formation: None })
}

pub(super) async fn recover_consumed_sources(
    ctx: &InterfaceLiftContext,
    callback: Option<&ToolDoorOutputCallback>,
    cursor: u64,
) -> Result<(), String> {
    for path in receipt_paths(&ctx.interface_var_dir())? {
        let (sequence, _) = receipt_coordinate(&path)?;
        if sequence >= cursor { continue; }
        let mut receipt = read_receipt(&path, ctx.required_host_io_limits()?.max_control_frame_bytes)?;
        let source = original_source(ctx, sequence)?;
        let mut input = ToolProcessEnvelope::capture(&source.envelope, sequence, &ctx.required_host_io_limits()?)?;
        input.output_source = Some(source);
        match receipt.deliver(ctx, &input, callback, true).await? {
            ToolProcessBrokerResult::Completed | ToolProcessBrokerResult::Refused { .. } => {
                std::fs::remove_file(&path).map_err(|e| e.to_string())?;
                sync_completed_output_parent(path.parent().ok_or("receipt has no parent")?)?;
            }
            _ => return Err("completed-output recovery did not reach an admission verdict".into()),
        }
    }
    Ok(())
}

pub struct PreparedToolOutputRecovery {
    receipt: CompletedOutputs,
    source: Movement,
    invocation_manifest_hash: ContentHash,
}

impl PreparedToolOutputRecovery {
    pub fn evidence(&self) -> serde_json::Value {
        serde_json::json!({
            "source_movement_seq": self.source.seq.0,
            "source_movement_digest": tool_door_movement_digest(&self.source).expect("Movement serializes"),
            "input": self.receipt.input.artifact,
            "invocation_manifest": self.receipt.invocation_manifest,
            "invocation_manifest_hash": self.invocation_manifest_hash,
            "generation": self.receipt.generation,
            "outputs": self.receipt.outputs,
        })
    }

    pub fn import(self, ctx: &InterfaceLiftContext) -> Result<PathBuf, String> {
        let _quiescent = try_acquire_tool_process_owner_lease(&self.receipt.generation)?
            .ok_or("selected completion process is still owned by a live executor; stop its door before import")?;
        let source = original_source(ctx, self.source.seq.0)?;
        if source.movement != self.source { return Err("selected completion input changed after preparation".into()); }
        let input = ToolProcessEnvelope::capture(&source.envelope, self.source.seq.0, &ctx.required_host_io_limits()?)?;
        if self.receipt.input != InputIdentity::capture(&input) { return Err("selected completion payload changed after preparation".into()); }
        let limits = ctx.required_host_io_limits()?;
        let invocation = capture_spawned_artifact(&self.receipt.invocation_manifest, &[ctx.interface_var_dir()], limits)?
            .read_bounded(limits.max_manifest_bytes, limits.max_payload_bytes).map_err(|e| e.to_string())?;
        if ContentHash::of_bytes(&invocation.manifest_bytes) != self.invocation_manifest_hash
            || ContentHash::of_bytes(&invocation.payload_bytes) != self.receipt.input.payload_hash
        { return Err("selected process invocation changed after preparation".into()); }
        for index in 0..self.receipt.outputs.len() {
            let output = self.receipt.output(ctx, index)?;
            let manifest = output.read_manifest_bounded(limits.max_manifest_bytes).map_err(|e| e.to_string())?;
            for path in [output.manifest_path(), manifest.manifest.payload_path.as_path()] {
                let mut options = std::fs::OpenOptions::new();
                options.read(true);
                #[cfg(unix)] {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
                }
                let file = options.open(path).map_err(|e| e.to_string())?;
                if !file.metadata().map_err(|e| e.to_string())?.is_file() { return Err("selected output carrier is not regular".into()); }
                octopus_file_storage::sync_ordered(&file).map_err(|e| e.to_string())?;
                sync_completed_output_parent(path.parent().ok_or("selected output has no parent")?)?;
            }
        }
        if let Some(existing) = CompletedOutputs::load(ctx, self.source.seq.0)? {
            let mut planned = self.receipt.clone();
            let mut observed = existing;
            for receipt in [&mut planned, &mut observed] {
                receipt.schema_version = 4;
                for output in &mut receipt.outputs {
                    output.admission = OutputAdmission::Unattempted;
                    output.dispatch = None;
                    output.delivery_id = None;
                    output.callback_completed = false;
                }
            }
            if serde_json::to_value(planned).map_err(|e| e.to_string())? != serde_json::to_value(observed).map_err(|e| e.to_string())? {
                return Err("selected completion conflicts with the retained owner receipt".into());
            }
        } else { self.receipt.save(ctx)?; }
        Ok(directory(&ctx.interface_var_dir()).join(receipt_name(&self.receipt)?))
    }
}

pub fn prepare_tool_output_recovery(
    ctx: &InterfaceLiftContext,
    owner: &ToolDoorOwner,
    source_movement_seq: u64,
    invocation_manifest: &Path,
    result_manifest: &Path,
    decoder: ToolDoorArtifactDecoder,
) -> Result<PreparedToolOutputRecovery, String> {
    owner.validate()?;
    if owner.door_interface != ctx.interface { return Err("selected completion belongs to another door owner".into()); }
    let source = original_source(ctx, source_movement_seq)?;
    let source_ctx = ctx.with_source_movement(source.movement.clone());
    let call = decoder(&source_ctx, &source.envelope, ToolDoorArtifactDecodePurpose::HistoricalIdentity)
        .map_err(|error| error.to_string())?
        .ok_or("selected source is not a tool invocation of this owner")?;
    if call.correlation_id.as_deref().is_none_or(str::is_empty) {
        return Err("selected historical result requires an exact nonempty tool correlation".into());
    }
    if tool_door_has_exact_admitted_result(ctx, &source.movement, &call, &source.envelope, decoder)? {
        return Err("selected tool invocation already has an admitted result".into());
    }
    let artifact_dir = invocation_manifest.parent().ok_or("selected invocation has no artifact directory")?;
    let request_dir = artifact_dir.parent().ok_or("selected invocation has no request directory")?;
    let request: u64 = request_dir.file_name().and_then(|s| s.to_str()).ok_or("selected invocation id is malformed")?
        .parse().map_err(|_| "selected invocation id is not numeric")?;
    let invocations = request_dir.parent().ok_or("selected invocation has no input root")?;
    let generation = invocations.parent().and_then(Path::parent).ok_or("selected invocation has no process generation")?;
    let generation_name = tool_process_generation_name(generation)?;
    let expected = ctx.interface_var_dir().join(".tool-process-host-state/generations").join(generation_name)
        .join("state/invocations").join(request.to_string()).join("artifact");
    if artifact_dir != expected || artifact_dir.canonicalize().map_err(|e| e.to_string())? != expected
        || result_manifest.parent() != Some(expected.join("emitted").as_path())
    {
        return Err("selected result left the exact owner process invocation".into());
    }
    let limits = ctx.required_host_io_limits()?;
    let _input = ToolProcessPrimaryInput::capture(invocations, request, invocation_manifest, &limits)?;
    let input_file = capture_spawned_artifact(invocation_manifest, &[expected.clone()], limits)?;
    let input = ToolProcessEnvelope::capture(&source.envelope, source_movement_seq, &limits)?;
    if input_file.read_payload_bytes_bounded(limits.max_payload_bytes).map_err(|e| e.to_string())? != input.payload
        || input_file.read_manifest_bounded(limits.max_manifest_bytes).map_err(|e| e.to_string())?.manifest.label != input.label
    {
        return Err("saved process invocation differs from the exact admitted input".into());
    }
    let owner_record = read_tool_process_owner_record(generation)?.ok_or("selected completion has no process owner record")?;
    let id = WorkerEnvironmentId::parse(owner_record.formation_id.as_deref().ok_or("selected completion has no formed environment")?)
        .map_err(|e| e.to_string())?;
    let record = WorkerEnvironmentRecordStore::new(ctx.interface_var_dir()).read(&id).map_err(|e| e.to_string())?;
    let runtime = source.envelope.runtime().map_err(|e| e.to_string())?;
    if record.formation.door_interface != owner.door_interface || record.formation.worker_environment_kind != owner.worker_environment_kind
        || record.formation.run != runtime.run.as_str() || record.formation.ee != runtime.ee.as_str() || record.formation.worker != runtime.worker
        || record.opened_at_movement_seq > source_movement_seq || record.last_activity_movement_seq < source_movement_seq
        || !record.formation.granted_tools.contains(&call.tool)
    {
        return Err("selected completion formation does not prove its original run/worker/tool/owner".into());
    }
    let result = capture_spawned_artifact(result_manifest, &[expected.join("emitted")], limits)?;
    let captured = result.read_bounded(limits.max_manifest_bytes, limits.max_payload_bytes).map_err(|e| e.to_string())?;
    let payload: ToolResultArtifact = serde_json::from_slice(&captured.payload_bytes).map_err(|e| e.to_string())?;
    if payload.tool != call.tool || payload.correlation_id != call.correlation_id
        || payload.label != captured.manifest.label || payload.label != format!("{}.result", call.tool)
    {
        return Err("saved completed result does not match the exact admitted tool call".into());
    }
    Ok(PreparedToolOutputRecovery {
        source: source.movement,
        invocation_manifest_hash: ContentHash::of_bytes(&input_file.read_manifest_bounded(limits.max_manifest_bytes)
            .map_err(|e| e.to_string())?.manifest_bytes),
        receipt: CompletedOutputs {
            schema_version: 2, input: InputIdentity::capture(&input), formation: Some(record.formation),
            generation: generation.to_path_buf(), invocation_manifest: invocation_manifest.to_path_buf(),
            outputs: vec![Output { manifest_path: result_manifest.to_path_buf(), label: captured.manifest.label,
                manifest_hash: ContentHash::of_bytes(&captured.manifest_bytes), payload_hash: ContentHash::of_bytes(&captured.payload_bytes),
                admission: OutputAdmission::AwaitingVerdict { ledger_offset: 0 }, dispatch: None, delivery_id: None, callback_completed: false }],
        },
    })
}

fn sync_completed_output_parent(parent: &Path) -> Result<(), String> {
    octopus_file_storage::sync_directory_ordered(parent)
        .map_err(|error| format!("completed output directory '{}' cannot be synced: {error}", parent.display()))
}
