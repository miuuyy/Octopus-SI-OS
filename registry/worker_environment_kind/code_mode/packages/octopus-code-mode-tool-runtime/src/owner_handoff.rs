use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use octopus_code_mode_room::config::{
    CodeModeConfig, CodeModeContext, CodeModeOwnerTopology, OwnerCheckpoint,
};
use octopus_code_mode_room::continuation::{
    self, ApplicationDecision, ApplicationProjection, ApplicationRetirement, InstalledHandoff,
    PendingApplication, ReturnBoundary,
};
use octopus_descriptors::CheckpointId;
use octopus_interfaces::InterfaceArtifactFile;
use octopus_ledger::{ContentHash, Endpoint, Movement, MovementKind};
use octopus_run_authoring::{CheckedRunEdit, CheckedRunEvidenceReader, RunAuthoringProfile};
use octopus_tool_door_runtime::notify::{RecursiveMode, Watcher};
use octopus_tool_door_runtime::{
    DeliveryVerdict, InterfaceLiftContext, LedgerTailReader, ToolDoorOutputCallback,
    ToolDoorOutputDispatch, ToolDoorOutputFuture, ToolDoorOutputHandler, ToolDoorOutputSource,
};
use octopus_tool_handlers::{ToolCallArtifact, ToolResultArtifact};
use serde_json::{Value, json};

pub struct OwnerHandoff {
    states: tokio::sync::Mutex<BTreeMap<PathBuf, OwnerState>>,
    changed: Arc<tokio::sync::Notify>,
    #[cfg(test)]
    json_format: Option<octopus_run_authoring::JsonFormat>,
}

struct OwnerState {
    config: CodeModeConfig,
    topology: CodeModeOwnerTopology,
    expected_hash: String,
    authority: ToolDoorOutputSource,
    initial_source: octopus_ledger::Movement,
    idle: bool,
    generated_artifacts: Vec<octopus_run_authoring::generated_artifacts::GeneratedArtifact>,
}

struct OwnerOutputAuthority {
    source: ToolDoorOutputSource,
    installed_output: Option<Value>,
    observed_run_hash: String,
}

enum NativeFailureOutput {
    Closed(DeliveryVerdict),
    Return(OwnerOutputAuthority),
}

enum OutputAuthorityRefresh { NativeInput, Recorded }

enum ApplicationChange {
    Request(PendingApplication),
    Retire(PendingApplication),
}

struct OwnerOutputHandler(Arc<OwnerHandoff>);

impl ToolDoorOutputHandler for OwnerOutputHandler {
    fn output(&self, ctx: InterfaceLiftContext, source: ToolDoorOutputSource,
        artifact: InterfaceArtifactFile, dispatch: ToolDoorOutputDispatch) -> ToolDoorOutputFuture {
        let owner = Arc::clone(&self.0);
        Box::pin(async move { OwnerHandoff::output(&owner, &ctx, source, artifact, dispatch).await })
    }

    fn can_resume_unsubmitted(&self, ctx: &InterfaceLiftContext, source: &ToolDoorOutputSource,
        artifact: &InterfaceArtifactFile) -> Result<bool, String> {
        let config = OwnerHandoff::failure_config(ctx)?;
        if matches!(config.context, CodeModeContext::Isolated) { return Ok(false); }
        let Some(formation) = source.formation.as_ref() else { return Ok(false); };
        let root = octopus_tool_door_runtime::tool_door_durable_state_dir(ctx, &formation.worker_environment_id)?;
        continuation::first_output_is_unsubmitted(ctx, &root, &config, source, artifact)
    }
}

impl ApplicationChange {
    fn application(&self) -> &PendingApplication {
        match self { Self::Request(application) | Self::Retire(application) => application }
    }
    fn retiring(&self) -> bool { matches!(self, Self::Retire(_)) }
}

impl OwnerHandoff {
    async fn prepare_output_authority(ctx: &InterfaceLiftContext, root: &Path, state: &mut OwnerState) -> Result<(), String> {
        let mut reader = LedgerTailReader::open_named(ctx, "code-mode-output-authority")?;
        let cancel = ctx.owner_cancel();
        loop {
            if cancel.is_cancelled() { return Err("code mode output authority preparation cancelled".into()); }
            match continuation::output_authority(ctx, root, &state.config, &state.authority) {
                Ok(continuation::OutputAuthority::Admitted(source)) => { state.authority = source; return Ok(()); }
                Ok(continuation::OutputAuthority::Initial(artifact)) => {
                    continuation::prepare_record(&root.join("output-authority-dispatch.json"), &continuation::artifact_ref(&artifact)?)?;
                    match ctx.owner_hand_arrival(artifact).await? {
                        DeliveryVerdict::Admitted { .. } => {},
                        refused => return Err(format!("code mode output authority was refused: {refused:?}")),
                    }
                },
                Ok(continuation::OutputAuthority::Pending) => reader.wait_for_change(&cancel).await?,
                Err(octopus_run_authoring::CheckedRunError::AwaitingResults { .. }) => reader.wait_for_change(&cancel).await?,
                Err(error) => return Err(error.into()),
            }
        }
    }

    async fn reconcile_provider_advance(
        ctx: &InterfaceLiftContext,
        root: &Path,
        config: &CodeModeConfig,
        input: Option<&octopus_interfaces::InterfaceInvocationEnvelope>,
    ) -> Result<Option<InstalledHandoff>, String> {
        let mut reader = None;
        let cancel = ctx.owner_cancel();
        loop {
            if cancel.is_cancelled() { return Err("code mode output preparation cancelled".into()); }
            match continuation::reconcile_checked_provider_advance(ctx, root, config, input) {
                Ok(installed) => return Ok(installed),
                Err(octopus_run_authoring::CheckedRunError::AwaitingResults { .. }) => {
                    if reader.is_none() { reader = Some(LedgerTailReader::open_named(ctx, "code-mode-owner-output")?); }
                    reader.as_mut().expect("output wait reader was opened").wait_for_change(&cancel).await?;
                }
                Err(error) => return Err(error.into()),
            }
        }
    }

    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            states: tokio::sync::Mutex::new(BTreeMap::new()),
            changed: Arc::new(tokio::sync::Notify::new()),
            #[cfg(test)]
            json_format: Some(octopus_run_authoring::JsonFormat::Compact),
        })
    }

    #[cfg(test)]
    pub(crate) fn with_json_format(json_format: Option<octopus_run_authoring::JsonFormat>) -> Arc<Self> {
        Arc::new(Self { states: tokio::sync::Mutex::new(BTreeMap::new()), changed: Arc::new(tokio::sync::Notify::new()), json_format })
    }

    pub fn callback(self: &Arc<Self>) -> ToolDoorOutputCallback {
        Arc::new(OwnerOutputHandler(Arc::clone(self)))
    }

    fn output<'a>(
        &'a self,
        ctx: &'a InterfaceLiftContext,
        mut source: ToolDoorOutputSource,
        artifact: InterfaceArtifactFile,
        dispatch: ToolDoorOutputDispatch,
    ) -> Pin<Box<dyn Future<Output = Result<DeliveryVerdict, String>> + Send + 'a>> {
        Box::pin(async move {
            let authority = match Self::prepare_native_failure_output(ctx, &mut source, &artifact)? {
                Some(NativeFailureOutput::Closed(verdict)) => return Ok(verdict),
                Some(NativeFailureOutput::Return(authority)) => Some(authority),
                None => None,
            };
            self.output_future(ctx, source, artifact, authority, dispatch, OutputAuthorityRefresh::NativeInput).await
        })
    }

    fn prepare_native_failure_output(
        ctx: &InterfaceLiftContext,
        source: &mut ToolDoorOutputSource,
        artifact: &InterfaceArtifactFile,
    ) -> Result<Option<NativeFailureOutput>, String> {
        let limits = ctx.owner_io_limits()?;
        let captured = artifact.read_bounded(limits.max_manifest_bytes, limits.max_payload_bytes)
            .map_err(|e| e.to_string())?;
        if captured.manifest.label.ends_with(".result") {
            let result: ToolResultArtifact = serde_json::from_slice(&captured.payload_bytes).map_err(|e| e.to_string())?;
            if result.error.as_ref().is_some_and(|error|
                ["tool_effect_indeterminate", "tool_process_failure"].contains(&error.code.as_str())) {
                let seq = result.error.as_ref().and_then(|error| error.details.as_ref())
                    .and_then(|details| details.get("source_movement_seq")).and_then(Value::as_u64);
                if result.ok || result.body.is_some() || result.label != captured.manifest.label
                    || seq != Some(source.movement.seq.0) {
                    return Err("native owner failure changed its immutable source".into());
                }
                let config = Self::failure_config(ctx)?;
                if !matches!(config.context, CodeModeContext::Isolated) {
                    let topology = config.resolve_owner_topology(&source.envelope)?;
                    let (edge, delivered) = Self::delivered_result(ctx, source, &topology, &result)?;
                    if let Some(delivered) = delivered {
                        return Ok(Some(NativeFailureOutput::Closed(DeliveryVerdict::refused(octopus_interfaces::RefusedReason::CheckpointSpent,
                            json!({"source_movement_seq":source.movement.seq.0,
                                "recorded_result_movement_seq":delivered.seq.0,"recorded_result_artifact":delivered.artifact})))));
                    }
                    if source.formation.is_none() {
                        source.formation = Some(Self::retained_failure_formation(ctx, &config, source)?);
                    }
                    let root = octopus_tool_door_runtime::tool_door_durable_state_dir(ctx,
                        &source.formation.as_ref().ok_or("native failure has no retained room")?.worker_environment_id)?;
                    let reader = LedgerTailReader::open_named(ctx, "code-mode-owner-output")?;
                    let emission = continuation::artifact_ref(artifact)?.content.to_hex();
                    let authority = Self::recorded_install_authority(ctx, &reader, &root, &config, source, &edge, &emission)?;
                    return Ok(Some(NativeFailureOutput::Return(authority)));
                }
            }
        }
        Ok(None)
    }

    fn output_future<'a>(
        &'a self,
        ctx: &'a InterfaceLiftContext,
        source: ToolDoorOutputSource,
        artifact: InterfaceArtifactFile,
        authority: Option<OwnerOutputAuthority>,
        dispatch: ToolDoorOutputDispatch,
        refresh: OutputAuthorityRefresh,
    ) -> Pin<Box<dyn Future<Output = Result<DeliveryVerdict, String>> + Send + 'a>> {
        Box::pin(self.output_with_authority(ctx, source, artifact, authority, dispatch, refresh))
    }

    async fn output_with_authority(
        &self,
        ctx: &InterfaceLiftContext,
        source: ToolDoorOutputSource,
        artifact: InterfaceArtifactFile,
        authority: Option<OwnerOutputAuthority>,
        dispatch: ToolDoorOutputDispatch,
        refresh: OutputAuthorityRefresh,
    ) -> Result<DeliveryVerdict, String> {
        let config: CodeModeConfig = serde_json::from_slice(
            &octopus_interfaces::read_artifact_carrier_file_bounded(
                &ctx.interface_registry_dir().join("code_mode.json"),
                1024 * 1024,
            )
            .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        config.validate(ctx.owner_interface().as_str())?;
        if matches!(config.context, CodeModeContext::Isolated) {
            return dispatch(ctx.clone(), artifact).await;
        }
        let topology = config.resolve_owner_topology(&source.envelope)?;
        continuation::validate_admitted_coauthor(ctx, &source.envelope, &topology)?;
        let formation = source
            .formation
            .as_ref()
            .ok_or("owner output has no exact formed room")?;
        let root = octopus_tool_door_runtime::tool_door_durable_state_dir(
            ctx,
            &formation.worker_environment_id,
        )?;
        let runtime = source.envelope.runtime().map_err(|e| e.to_string())?;
        let mut states = self.states.lock().await;
        let authority = if authority.is_some() && matches!(refresh, OutputAuthorityRefresh::NativeInput) {
            match Self::prepare_native_failure_output(ctx, &mut source.clone(), &artifact)? {
                Some(NativeFailureOutput::Closed(verdict)) => return Ok(verdict),
                Some(NativeFailureOutput::Return(authority)) => Some(authority),
                None => return Err("retained failure authority lost its exact native failure".into()),
            }
        } else { authority };
        let state = states.entry(root.clone()).or_insert_with(|| OwnerState {
            config: config.clone(),
            topology: topology.clone(),
            expected_hash: runtime.run_record_hash.clone().unwrap_or_default(),
            authority: source.clone(),
            initial_source: source.movement.clone(),
            idle: false,
            generated_artifacts: Vec::new(),
        });
        if state.config != config || state.topology != topology {
            return Err("code mode output changed its admitted owner topology".into());
        }
        let installed_output = authority.as_ref().and_then(|authority| authority.installed_output.clone());
        let failure_run_hash = authority.as_ref().map(|authority| authority.observed_run_hash.clone());
        if let Some(authority) = authority {
            let authority = authority.source;
            let current = authority.envelope.runtime().map_err(|e| e.to_string())?;
            if authority.formation != source.formation || current.run != runtime.run
                || current.ee != runtime.ee || current.worker != runtime.worker
                || authority.movement.to != Endpoint::Interface(ctx.owner_interface().clone())
                || config.resolve_owner_topology(&authority.envelope)? != topology {
                return Err("owner output authority changed its exact admitted scope".into());
            }
            continuation::validate_admitted_coauthor(ctx, &authority.envelope, &topology)?;
            Self::reconcile_provider_advance(ctx, &root, &config, Some(&authority.envelope)).await?;
            state.authority = authority;
        }
        if let Some(installed) = Self::reconcile_provider_advance(ctx,&root,&config,None).await? {
            state.expected_hash = installed.after_hash;
        }
        if failure_run_hash.as_ref().is_some_and(|hash| hash != &state.expected_hash) {
            return Err("owner failure authority requires a fresh proof after checked advancement".into());
        }
        let source_hash = runtime
            .run_record_hash
            .as_deref()
            .ok_or("owner input has no pinned run bytes")?;
        if source_hash != state.expected_hash && source.movement != state.initial_source {
            let installed: InstalledHandoff =
                continuation::read_record(&root.join(format!("{source_hash}.installed.json")))?;
            if installed.after_hash != source_hash {
                return Err("owner output source is not an admitted installed snapshot".into());
            }
        }
        if failure_run_hash.is_none() && state.authority.movement == state.initial_source {
            Self::prepare_output_authority(ctx, &root, state).await?;
        }
        state.idle = false;
        let captured = artifact
            .read_bounded(1024 * 1024, config.max_output_bytes as u64)
            .map_err(|e| e.to_string())?;
        let artifact_ref = continuation::artifact_ref(&artifact)?;
        if captured.manifest.label == "code_mode.artifact_boundary" {
            let limits = ctx.owner_io_limits()?;
            let evidence = octopus_run_authoring::CheckedRunEvidence {
                movements_path:ctx.owner_movements_path(),ledger_index_path:ctx.owner_ledger_index_path(),blobs_dir:ctx.owner_blobs_dir(),
                max_control_frame_bytes:limits.max_control_frame_bytes,max_manifest_bytes:limits.max_manifest_bytes,max_payload_bytes:limits.max_payload_bytes,
            };
            let generated = octopus_run_authoring::generated_artifacts::admitted_generated_artifacts(&evidence,&source.movement,&source.envelope)?;
            if generated.is_empty() { return Err("artifact boundary has no admitted generated artifacts".into()); }
            for output in generated {
                if let Some(old) = state.generated_artifacts.iter().find(|old| old.artifact.id == output.artifact.id) {
                    if old != &output { return Err("generated artifact identity changed".into()); }
                } else { state.generated_artifacts.push(output); }
            }
            let edge = state.topology.receipt.clone();
            self.install(ctx,&root,state,&edge,&artifact_ref.content.to_hex(),&captured.manifest.label,None,None).await?;
            continuation::record_internal_receipt(&root,&artifact_ref,source.movement.seq.0)?;
            return dispatch(ctx.clone(), artifact).await;
        }
        if captured.manifest.label == "code_mode.application_boundary" {
            let body: Value =
                serde_json::from_slice(&captured.payload_bytes).map_err(|e| e.to_string())?;
            let key = body
                .get("pending_digest")
                .and_then(Value::as_str)
                .ok_or("application boundary has no exact pending call")?;
            let application = continuation::pending_application(&root, key)?;
            if body.get("request_id").and_then(Value::as_str)
                != Some(application.request.request_id.as_str())
            {
                return Err("application boundary has another request identity".into());
            }
            let phase = body
                .get("phase")
                .and_then(Value::as_str)
                .ok_or("application boundary has no phase")?;
            return match phase {
                "request" => {
                    if application.request_artifact != source.envelope.artifact {
                        return Err(
                            "application projection is not from its original admitted GUI result"
                                .into(),
                        );
                    }
                    let edge = state.topology.notification.clone().ok_or("application request has no exact operator output")?;
                    let label = "operator.approval.request";
                    let projection = InterfaceArtifactFile::write(root.join("application-artifacts"), label,
                        &serde_json::to_vec(&application.request.projection()).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
                    self.install(ctx,&root,state,&edge,&artifact_ref.content.to_hex(),label,Some(ApplicationChange::Request(application.clone())),None).await?;
                    dispatch(ctx.clone(), projection).await
                }
                "retire" => {
                    let decision: ApplicationDecision = continuation::read_record(
                        &root.join(format!("{key}.application-decision.json")),
                    )?;
                    if decision.artifact != source.envelope.artifact
                        || decision.source_seq != source.movement.seq.0
                    {
                        return Err("application retirement is not from its original admitted operator decision".into());
                    }
                    let edge = state.topology.receipt.clone();
                    self.install(
                        ctx,
                        &root,
                        state,
                        &edge,
                        &artifact_ref.content.to_hex(),
                        "code_mode.application_boundary",
                        Some(ApplicationChange::Retire(application)),
                        None,
                    )
                    .await?;
                    let receipt = InterfaceArtifactFile::write(root.join("application-artifacts"),"code_mode.application_boundary",&serde_json::to_vec(&json!({"correlation_id":format!("code-mode-decision:{}:{key}",formation.worker_environment_id),"request_id":decision.request_id,"phase":"retired"})).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
                    continuation::record_internal_receipt(
                        &root,
                        &continuation::artifact_ref(&receipt)?,
                        source.movement.seq.0,
                    )?;
                    dispatch(ctx.clone(), receipt).await
                }
                _ => Err("unknown application boundary phase".into()),
            };
        }
        let mut terminal = false;
        let delivered_artifact = artifact.clone();
        let edge = if captured.manifest.label == "tool.call" {
            let call: ToolCallArtifact =
                serde_json::from_slice(&captured.payload_bytes).map_err(|e| e.to_string())?;
            let pending = continuation::prepared_call_for_output(&root, &call, formation)?;
            state
                .topology
                .routes
                .iter()
                .find(|r| r.tool == pending.route().tool)
                .ok_or("nested call has no authored owner route")?
                .call
                .clone()
        } else if captured.manifest.label == "code_mode.notification" {
            let notification: Value =
                serde_json::from_slice(&captured.payload_bytes).map_err(|e| e.to_string())?;
            if notification
                .get("context_generation")
                .and_then(Value::as_str)
                != Some(continuation::context_generation(&root)?.as_str())
            {
                return Err("notification has another context generation".into());
            }
            state
                .topology
                .notification
                .clone()
                .ok_or("notification has no authored output")?
        } else {
            let result: ToolResultArtifact =
                serde_json::from_slice(&captured.payload_bytes).map_err(|e| e.to_string())?;
            if result.label != captured.manifest.label {
                return Err("owner result label mismatch".into());
            }
            terminal = true;
            let original: Value = serde_json::from_slice(
                &source
                    .envelope
                    .read_artifact_payload_bounded(config.max_output_bytes as u64)
                    .map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
            let internal = source
                .envelope
                .read_artifact_manifest()
                .map_err(|e| e.to_string())?
                .label
                != "tool.call";
            if internal {
                continuation::record_internal_receipt(
                    &root,
                    &continuation::artifact_ref(&delivered_artifact)?,
                    source.movement.seq.0,
                )?;
                state.topology.receipt.clone()
            } else {
                if original.get("tool").and_then(Value::as_str) != Some(result.tool.as_str())
                    || original.get("correlation_id").and_then(Value::as_str)
                        != result.correlation_id.as_deref()
                {
                    return Err(
                        "outer output does not match its original admitted model call".into(),
                    );
                }
                state.topology.model_output.clone()
            }
        };
        if let Some(output) = installed_output {
            let bytes = octopus_interfaces::read_artifact_carrier_file_bounded(
                &ctx.owner_root_slot("run_records_root")?.join(format!("{}.json", state.topology.run)),
                ctx.owner_io_limits()?.max_payload_bytes,
            ).map_err(|e| e.to_string())?;
            let current: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
            let id = output.get("id").ok_or("recorded owner output lost its checkpoint identity")?;
            if (edge != state.topology.model_output && edge != state.topology.receipt)
                || output.get("interface_binding") != Some(&json!(edge.binding.inbound))
                || output.get("outbound_interface_bindings") != Some(&json!(edge.binding.outbound))
                || !current.get("checkpoints").and_then(Value::as_array).is_some_and(|checkpoints|
                    checkpoints.iter().filter(|checkpoint| checkpoint.get("id") == Some(id)).eq([&output]))
                || !current.get("active_checkpoints").and_then(Value::as_array).is_some_and(|active| active.contains(id)) {
                return Err("recorded owner output changed its installed checkpoint".into());
            }
        } else {
            self.install(ctx, &root, state, &edge, &artifact_ref.content.to_hex(), &captured.manifest.label, None, failure_run_hash.as_deref()).await?;
        }
        let verdict = dispatch(ctx.clone(), delivered_artifact).await?;
        if terminal && matches!(verdict, DeliveryVerdict::Admitted { .. }) {
            let readiness = state.topology.readiness.clone();
            self.install(
                ctx,
                &root,
                state,
                &readiness,
                &format!("{}-idle", artifact_ref.content.to_hex()),
                "code_mode.frontier_ready",
                None,
                None,
            )
            .await?;
            state.idle = true;
            self.changed.notify_one();
        }
        Ok(verdict)
    }

    async fn install(
        &self,
        ctx: &InterfaceLiftContext,
        root: &Path,
        state: &mut OwnerState,
        edge: &OwnerCheckpoint,
        emission: &str,
        artifact_label: &str,
        application_change: Option<ApplicationChange>,
        failure_run_hash: Option<&str>,
    ) -> Result<(), String> {
        loop {
        if ctx.owner_cancel().is_cancelled() { return Err("code mode authoring cancelled".into()); }
        let reconciled = Self::reconcile_provider_advance(ctx,root,&state.config,None).await?;
        if let Some(installed) = &reconciled {
            state.expected_hash = installed.after_hash.clone();
        }
        if failure_run_hash.is_some_and(|hash| hash != state.expected_hash) {
            return Err("owner failure installation requires a fresh proof after checked advancement".into());
        }
        let descriptor = ctx.owner_descriptor()?;
        let private = &descriptor
            .interface_driver_config
            .as_ref()
            .ok_or("code mode door has no private config")?
            .private_config;
        let profile: RunAuthoringProfile = serde_json::from_value(
            private
                .get("run_authoring_profile")
                .cloned()
                .ok_or("code mode door has no independent checked authoring profile")?,
        )
        .map_err(|e| e.to_string())?;
        if profile.file_write_tool != state.topology.file_write_tool
            || profile.result_timeout_ms == 0
        {
            return Err("owner profile differs from admitted topology".into());
        }
        let run = &state.topology.run;
        if Path::new(run).components().count() != 1 || run == "." || run == ".." {
            return Err("malformed owner run id".into());
        }
        let run_path = ctx
            .owner_root_slot("run_records_root")?
            .canonicalize().map_err(|error| error.to_string())?
            .join(format!("{run}.json"));
        let current = octopus_interfaces::read_artifact_carrier_file_bounded(
            &run_path,
            ctx.owner_io_limits()?.max_payload_bytes,
        )
        .map_err(|e| e.to_string())?;
        if ContentHash::of_bytes(&current).to_hex() != state.expected_hash {
            if reconciled.is_none() {
                return Err("code mode authoring refuses a neighbor or external run-record change".into());
            }
            continue;
        }
        let before: Value = serde_json::from_slice(&current).map_err(|e| e.to_string())?;
        let mut next = before.clone();
        let owner = &state.topology.owner_interface;
        let previous_raw = before.pointer(&format!("/interface_state/{owner}/output_artifact_checkpoints"))
            .map(|value|serde_json::from_value::<Vec<String>>(value.clone()).map_err(|e|e.to_string())).transpose()?.unwrap_or_default();
        let recorded_generated = before.pointer(&format!("/interface_state/{owner}/generated_artifacts"))
            .map(|value|serde_json::from_value::<Vec<octopus_run_authoring::generated_artifacts::GeneratedArtifact>>(value.clone()).map_err(|e|e.to_string())).transpose()?.unwrap_or_default();
        for output in recorded_generated {
            if let Some(existing) = state.generated_artifacts.iter().find(|candidate|candidate.artifact.id == output.artifact.id) {
                if existing != &output { return Err("recorded generated artifact changed".into()); }
            } else { state.generated_artifacts.push(output); }
        }
        let owned = next
            .get("interface_state")
            .and_then(|s| s.get(owner))
            .and_then(|s| s.get("frontier_checkpoint_ids"))
            .and_then(Value::as_array)
            .ok_or("selected workflow did not author this owner's independent frontier")?
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .ok_or("owner frontier contains non-string id")
            })
            .collect::<Result<Vec<_>, _>>()?;
        let generation = continuation::digest(
            &json!({"before":state.expected_hash,"emission":emission,"edge":edge,"source_seq":state.authority.movement.seq.0}),
        )?;
        let prefix = format!("code_mode_{generation}");
        let output_id = format!("{prefix}_{}", edge.name);
        let result_id = format!("{prefix}_{}", state.topology.authoring_result.name);
        let mut bindings = state
            .authority
            .envelope
            .metadata()
            .map_err(|e| e.to_string())?
            .artifact_binding_refs
            .iter()
            .map(|r| json!({"id":r.id}))
            .collect::<Vec<_>>();
        let role_bindings = octopus_run_authoring::generated_artifacts::mandatory_role_bindings(
            &state.authority.envelope.metadata().map_err(|e| e.to_string())?.grant,
        )?;
        if role_bindings.iter().any(|role| !bindings.iter().any(|binding| binding.get("id") == Some(&json!(role.id)))) {
            return Err("owner output lost an admitted mandatory role artifact".into());
        }
        for output in &state.generated_artifacts {
            if !bindings.iter().any(|binding| binding.get("id") == Some(&json!(output.artifact.id))) {
                bindings.push(json!({"id":output.artifact.id}));
            }
        }
        let trace = next.get("trace").cloned().ok_or("owner run has no trace")?;
        let row = |id: &str, checkpoint: &OwnerCheckpoint| -> Result<Value, String> {
            let selected = if checkpoint == &state.topology.model_output {
                let record = octopus_run_record::RunRecord::from_bytes(&current)
                    .map_err(|error| error.to_string())?;
                let provider_writer = state.topology.coauthor.as_ref()
                    .ok_or("owner result has no declared provider writer")?;
                let owner_writer = octopus_run_authoring::CheckedRunWriter {
                    owner: ctx.owner_interface().clone(), source: ctx.owner_interface().clone(),
                    door: state.topology.authoring_result.binding.inbound.interface.clone(),
                    tool: profile.file_write_tool.clone(), call_label: profile.file_write_label.clone(),
                    result_label: profile.file_write_result_label.clone(),
                };
                octopus_run_authoring::declared_owner_input_bindings(
                    &record, provider_writer, &owner_writer, &checkpoint.binding,
                )?.into_iter().map(|binding| serde_json::to_value(binding)
                    .map_err(|error| error.to_string())).collect::<Result<Vec<_>, _>>()?
            } else if state.topology.notification.as_ref() == Some(checkpoint) {
                bindings.iter().filter(|binding|!state.generated_artifacts.iter().any(|output|binding.get("id") == Some(&json!(output.artifact.id)))).cloned().collect::<Vec<_>>()
            } else { bindings.clone() };
            Ok(json!({"id":id,"run":run,"trace":trace,"epoch":0,"interface_binding":checkpoint.binding.inbound,"outbound_interface_bindings":checkpoint.binding.outbound,"artifact_bindings":selected,"required_verified_artifacts":[]}))
        };
        let mut paired_to_remove = Vec::new();
        if let Some(ApplicationChange::Retire(application)) = &application_change {
            let projection: ApplicationProjection =
                continuation::read_record(&root.join(format!(
                    "{}.{}.application-projected.json",
                    application.pending_digest, state.expected_hash
                )))?;
            paired_to_remove = projection.checkpoints;
        }
        let mut paired_to_add = Vec::new();
        if let Some(change) = &application_change {
            if !change.retiring() {
                let application = change.application();
                application.request.validate_bound_files(state.authority.envelope.metadata().map_err(|e| e.to_string())?)?;
                let route = state.topology.routes.iter().find(|route| route.tool == application.request.original.tool)
                    .ok_or("application has no exact declared route")?;
                let client = state.topology.notification.as_ref().ok_or("application has no exact task client")?.binding.outbound[0].interface.clone();
                let owner_interface = state.topology.receipt.binding.outbound[0].interface.clone();
                let door = route.call.binding.outbound[0].interface.clone();
                application.request.validate_operator_interface(&client)?;
                if door != application.request.original.door_interface || application.operator_interface != client {
                    return Err("application approval would target another exact client/tool owner".into());
                }
                let exact = |interface: &octopus_descriptors::InterfaceId, label: &str| json!({"interface":interface,
                    "config":{"artifact_policy":{"mode":"allow","names":[label],"namespaces":[]}}});
                for (suffix,mark,verifier) in [("approved","operator.approval","operator_approval"),("denied","operator.input.provided","operator_input")] {
                    if !state.authority.envelope.metadata().map_err(|e| e.to_string())?.grant.mark_outcomes.iter().any(|m| m == mark) {
                        return Err("application mark is absent from this formed owner grant".into());
                    }
                    let id = format!("application_{}_{}",application.pending_digest,suffix);
                    let from = &application.operator_interface;
                    let targets = vec![exact(&owner_interface,"operator.approval.response"), exact(&door,"operator.approval.response")];
                    paired_to_add.push(json!({"id":id,"run":run,"trace":trace,"epoch":0,"interface_binding":exact(from,"operator.approval.response"),
                        "outbound_interface_bindings":targets,"artifact_bindings":bindings,"required_verified_artifacts":[{"mark":mark,"artifact_label":"operator.approval.response",
                        "verifier":verifier,"params":application.request.verification_params()}]}));
                }
            }
        }
        let checkpoints = next
            .get_mut("checkpoints")
            .and_then(Value::as_array_mut)
            .ok_or("owner run checkpoints are absent")?;
        checkpoints.retain(|checkpoint| !checkpoint.get("id").and_then(Value::as_str).is_some_and(|id|previous_raw.iter().any(|old|old == id)));
        for id in &owned {
            let existing = checkpoints
                .iter()
                .find(|cp| cp.get("id").and_then(Value::as_str) == Some(id.as_str()))
                .ok_or("owned frontier names a missing checkpoint")?;
            let inbound = existing
                .pointer("/interface_binding/interface")
                .and_then(Value::as_str)
                .ok_or("owned frontier checkpoint has no inbound identity")?;
            if inbound != owner
                && inbound
                    != state
                        .topology
                        .authoring_result
                        .binding
                        .inbound
                        .interface
                        .as_str()
            {
                return Err("owner frontier attempted to replace a neighboring checkpoint".into());
            }
        }
        for id in &paired_to_remove {
            if !checkpoints
                .iter()
                .any(|cp| cp.get("id").and_then(Value::as_str) == Some(id.as_str()))
            {
                return Err("application retirement lost an exact paired checkpoint".into());
            }
        }
        checkpoints.retain(|cp| {
            !cp.get("id")
                .and_then(Value::as_str)
                .is_some_and(|id| paired_to_remove.iter().any(|old| old == id))
        });
        for cp in &paired_to_add {
            if checkpoints.iter().any(|old| old.get("id") == cp.get("id")) {
                return Err("application approval was already projected".into());
            }
            checkpoints.push(cp.clone());
        }
        checkpoints.retain(|cp| {
            !cp.get("id")
                .and_then(Value::as_str)
                .is_some_and(|id| owned.iter().any(|old| old == id))
        });
        checkpoints.push(row(&output_id, edge)?);
        checkpoints.push(row(&result_id, &state.topology.authoring_result)?);
        let pending = continuation::prepared_calls(root)?;
        let mut raw_checkpoints = Vec::new();
        let mut pending_owners = std::collections::BTreeSet::new();
        let mut return_epochs = BTreeMap::new();
        for call in &pending {
            let owner = call.process_owner().ok_or("pending call has no immutable declared process owner")?;
            if !pending_owners.insert(owner) {
                return Err("multiple unresolved calls share one declared process owner".into());
            }
            let route = state
                .topology
                .routes
                .iter()
                .find(|r| r.tool == call.route().tool)
                .ok_or("pending call lost its authored route")?;
            if octopus_code_mode_room::config::DeclaredToolOwner::from_descriptor(&route.process_descriptor)? != *owner {
                return Err("pending return changed its immutable process owner".into());
            }
            let raw_id = format!("nested_artifact_{}",call.pending_digest());
            let expected = row(&route.result.name, &route.result)?;
            if let Some(existing) = checkpoints
                .iter_mut()
                .find(|cp| cp.get("id").and_then(Value::as_str) == Some(route.result.name.as_str()))
            {
                let mut retained = expected.clone();
                retained["artifact_bindings"] = existing["artifact_bindings"].clone();
                retained["epoch"] = existing["epoch"].clone();
                if existing != &retained {
                    return Err("pending return checkpoint was changed outside its owner".into());
                }
                if !previous_raw.contains(&raw_id)
                    || matches!(&application_change, Some(ApplicationChange::Request(application))
                        if application.pending_digest == call.pending_digest()) {
                    existing["epoch"] = json!(existing["epoch"].as_u64()
                        .ok_or("pending return checkpoint has no epoch")?
                        .checked_add(1).ok_or("pending return checkpoint epoch overflow")?);
                }
                return_epochs.insert(route.result.name.clone(), existing["epoch"].as_u64()
                    .ok_or("pending return checkpoint has no epoch")?);
            } else {
                checkpoints.push(expected);
                return_epochs.insert(route.result.name.clone(), 0);
            }
            let raw_binding = octopus_run_authoring::generated_artifacts::output_artifact_binding(route.result.binding.inbound.interface.clone(), &call.correlation())?;
            let raw = json!({"id":raw_id,"run":run,"trace":trace,"epoch":0,"interface_binding":raw_binding,"outbound_interface_bindings":[],"artifact_bindings":role_bindings,"required_verified_artifacts":[]});
            if let Some(existing) = checkpoints.iter().find(|cp| cp.get("id") == raw.get("id")) {
                if existing != &raw { return Err("pending raw artifact checkpoint changed".into()); }
            } else { checkpoints.push(raw); }
            raw_checkpoints.push(raw_id);
        }
        let active = next
            .get_mut("active_checkpoints")
            .and_then(Value::as_array_mut)
            .ok_or("owner run active frontier is absent")?;
        active.retain(|value|!value.as_str().is_some_and(|id|previous_raw.iter().any(|old|old == id)));
        active.retain(|cp| {
            !cp.as_str()
                .is_some_and(|id| owned.iter().any(|old| old == id))
        });
        active.retain(|cp| {
            !cp.as_str()
                .is_some_and(|id| paired_to_remove.iter().any(|old| old == id))
        });
        for cp in &paired_to_add {
            active.push(
                cp.get("id")
                    .cloned()
                    .ok_or("application checkpoint lost its identity")?,
            );
        }
        active.push(json!(output_id));
        active.push(json!(result_id));
        for call in &pending {
            let id = call
                .route()
                .result_checkpoint
                .as_deref()
                .ok_or("pending call has no result checkpoint")?;
            if !active.iter().any(|v| v.as_str() == Some(id)) {
                active.push(json!(id));
            }
        }
        for id in &raw_checkpoints {
            if !active.iter().any(|value| value.as_str() == Some(id)) { active.push(json!(id)); }
        }
        next.get_mut("interface_state")
            .and_then(|s| s.get_mut(owner))
            .and_then(Value::as_object_mut)
            .ok_or("owner state disappeared")?
            .insert(
                "frontier_checkpoint_ids".into(),
                json!([output_id, result_id]),
            );
        let mut complete_owned = next.get("interface_state").and_then(|s| s.get(owner)).and_then(|s| s.get("owned_checkpoint_ids")).and_then(Value::as_array).map(|ids| ids.iter().map(|id| id.as_str().map(str::to_owned).ok_or("invalid owned checkpoint identity")).collect::<Result<std::collections::BTreeSet<_>,_>>()).transpose()?.unwrap_or_default();
        complete_owned.retain(|id| !owned.contains(id) && !paired_to_remove.contains(id));
        complete_owned.retain(|id|!previous_raw.contains(id));
        complete_owned.extend([output_id.clone(),result_id.clone()]);
        complete_owned.extend(paired_to_add.iter().filter_map(|cp| cp.get("id").and_then(Value::as_str)).map(str::to_owned));
        complete_owned.extend(pending.iter().filter_map(|call| call.route().result_checkpoint.clone()));
        complete_owned.extend(raw_checkpoints.iter().cloned());
        let owner_state = next.get_mut("interface_state").and_then(|s|s.get_mut(owner)).and_then(Value::as_object_mut).ok_or("owner state disappeared")?;
        owner_state.insert("output_artifact_checkpoints".into(),json!(raw_checkpoints));
        owner_state.insert("generated_artifacts".into(),json!(state.generated_artifacts));
        next.get_mut("interface_state").and_then(|s| s.get_mut(owner)).and_then(Value::as_object_mut).ok_or("owner state disappeared")?.insert("owned_checkpoint_ids".into(),json!(complete_owned));
        let active_ids = next
            .get("active_checkpoints")
            .and_then(Value::as_array)
            .ok_or("active frontier disappeared")?;
        let active_owner_outputs = next
            .get("checkpoints")
            .and_then(Value::as_array)
            .ok_or("checkpoints disappeared")?
            .iter()
            .filter(|cp| {
                active_ids.iter().any(|id| Some(id) == cp.get("id"))
                    && cp
                        .pointer("/interface_binding/interface")
                        .and_then(Value::as_str)
                        == Some(owner.as_str())
            })
            .collect::<Vec<_>>();
        let mut admitted_outputs = Vec::new();
        for checkpoint in active_owner_outputs {
            let binding: octopus_interfaces::CheckpointBindingPolicySnapshot = serde_json::from_value(json!({
                "inbound":checkpoint.get("interface_binding").ok_or("owner output lost its binding")?,
                "outbound":checkpoint.get("outbound_interface_bindings").ok_or("owner output lost its recipients")?
            })).map_err(|e|e.to_string())?;
            if binding.inbound.admits(artifact_label) && binding.outbound.iter().all(|binding| binding.admits(artifact_label)) { admitted_outputs.push(checkpoint); }
        }
        if admitted_outputs.len() != 1
            || admitted_outputs[0].get("id").and_then(Value::as_str) != Some(output_id.as_str())
        {
            return Err("code mode output would fan out beyond its single authored output".into());
        }
        let correlation = format!(
            "code-mode-authoring:{}:{generation}",
            state
                .authority
                .formation
                .as_ref()
                .ok_or("authoring lost room formation")?
                .worker_environment_id
        );
        let proposal = root.join(format!("{generation}.edit.json"));
        #[cfg(not(test))]
        let json_format = Some(octopus_run_authoring::JsonFormat::Compact);
        #[cfg(test)]
        let json_format = self.json_format;
        let json_format = if proposal.try_exists().map_err(|error| error.to_string())? {
            continuation::read_record::<CheckedRunEdit>(&proposal)?.json_format
        } else if root.join(format!("{generation}.authoring-result.json")).try_exists().map_err(|error| error.to_string())? {
            return Err("code mode authoring result has no durable edit proposal; settle the incomplete historical authoring under its original contract".into());
        } else { json_format };
        let edit = CheckedRunEdit::prepare_with_format(run_path.clone(), &current, next, correlation, json_format)?;
        continuation::prepare_record(&proposal, &edit)?;
        for call in &pending {
            let route = state
                .topology
                .routes
                .iter()
                .find(|r| r.tool == call.route().tool)
                .ok_or("pending route disappeared")?;
            continuation::prepare_return(
                root,
                call,
                &ReturnBoundary {
                    checkpoint: route.result.name.clone(),
                    epoch: *return_epochs.get(&route.result.name)
                        .ok_or("pending return checkpoint lost its authored epoch")?,
                    run_record_hash: edit.after_hash.clone(),
                    binding: route.result.binding.clone(),
                },
            )?;
        }
        if edge == &state.topology.readiness {
            continuation::prepare_record(
                &root.join(format!("{}.wake-pin.json", edit.after_hash)),
                &ReturnBoundary {
                    checkpoint: output_id.clone(),
                    epoch: 0,
                    run_record_hash: edit.after_hash.clone(),
                    binding: edge.binding.clone(),
                },
            )?;
        }
        let result_boundary = ReturnBoundary {
            checkpoint: result_id.clone(),
            epoch: 0,
            run_record_hash: edit.after_hash.clone(),
            binding: state.topology.authoring_result.binding.clone(),
        };
        continuation::prepare_authoring_result(
            root,
            &edit.correlation_id,
            &profile.file_write_tool,
            &profile.file_write_result_label,
            &result_boundary,
            &edit.path,
            &edit.before_hash,
            &edit.after_hash,
            edit.content.len(),
        )?;
        let artifact = edit.materialize(&root.join("authoring-artifacts"), &profile)?;
        let call_payload = artifact.read_payload_bytes().map_err(|e| e.to_string())?;
        let file_door = state
            .topology
            .authoring_result
            .binding
            .inbound
            .interface
            .clone();
        let mut reader = LedgerTailReader::open_named(ctx, "code-mode-owner-authoring")?;
        let verdict = ctx
            .owner_hand_system_emission(artifact, &state.authority.movement, file_door.clone())
            .await?;
        let DeliveryVerdict::Admitted { seq: call_seq } = verdict else {
            return Err(format!("code mode authoring was refused: {verdict:?}"));
        };
        let cancel = ctx.owner_cancel();
        let timeout = std::time::Duration::from_millis(profile.result_timeout_ms);
        let limits = ctx.owner_io_limits()?;
        let evidence = octopus_run_authoring::CheckedRunEvidence {
            movements_path: ctx.owner_movements_path(), ledger_index_path: ctx.owner_ledger_index_path(), blobs_dir: ctx.owner_blobs_dir(),
            max_control_frame_bytes: limits.max_control_frame_bytes, max_manifest_bytes: limits.max_manifest_bytes, max_payload_bytes: limits.max_payload_bytes,
        };
        let admitted_call = octopus_run_authoring::generated_artifacts::read_movement(&evidence, call_seq)?;
        if admitted_call.kind != octopus_ledger::MovementKind::InterfaceOutbound
            || admitted_call.from != octopus_ledger::Endpoint::Interface(ctx.owner_interface().clone())
            || admitted_call.to != octopus_ledger::Endpoint::Interface(file_door.clone()) {
            return Err("owner write receipt does not name its exact admitted emission".into());
        }
        let call_artifact = admitted_call.artifact.ok_or("owner write receipt has no artifact")?;
        let admitted_file = InterfaceArtifactFile::new(ctx.owner_blobs_dir().join(call_artifact.content.to_hex()))
            .map_err(|e| e.to_string())?.with_payload_cache_root(ctx.owner_blobs_dir());
        if continuation::artifact_ref(&admitted_file)? != call_artifact
            || admitted_file.read_payload_bytes_bounded(limits.max_payload_bytes).map_err(|e| e.to_string())? != call_payload {
            return Err("owner write receipt changed its exact call bytes".into());
        }
        let writer = octopus_run_authoring::CheckedRunWriter {
            owner: ctx.owner_interface().clone(), source: ctx.owner_interface().clone(), door: file_door.clone(),
            tool: profile.file_write_tool.clone(), call_label: profile.file_write_label.clone(), result_label: profile.file_write_result_label.clone(),
        };
        let result = tokio::time::timeout(timeout, async {
            loop {
                match octopus_run_authoring::checked_failed_run_write(&evidence, &writer, &edit, call_seq,
                    state.topology.coauthor.iter().cloned().collect()) {
                    Ok(Some(refused)) => {
                        continuation::create_record(&root.join(format!("{generation}.refused.json")), &refused)?;
                        return Ok(None);
                    }
                    Ok(None) => {},
                    Err(octopus_run_authoring::CheckedRunError::AwaitingResults { .. }) => {
                        reader.wait_for_change(&cancel).await?;
                        continue;
                    }
                    Err(error) => return Err(String::from(error)),
                }
                let rows = reader.read_new_uncommitted_limit(256)?;
                for own in &rows {
                    let envelope = reader.envelope(own)?;
                    let body: Value = serde_json::from_slice(
                        &envelope
                            .read_artifact_payload_bounded(8 * 1024 * 1024)
                            .map_err(|e| e.to_string())?,
                    )
                    .map_err(|e| e.to_string())?;
                    if body.get("correlation_id").and_then(Value::as_str)
                        == Some(edit.correlation_id.as_str())
                    {
                        let installed = octopus_interfaces::read_artifact_carrier_file_bounded(
                            &ctx.owner_blobs_dir().join(envelope.runtime().map_err(|error| error.to_string())?.run_record_hash.as_deref().ok_or("authoring result has no pinned snapshot")?),
                            ctx.owner_io_limits()?.max_payload_bytes,
                        )
                        .map_err(|e| e.to_string())?;
                        edit.validate_result(
                            &profile,
                            &own.movement,
                            &envelope,
                            call_seq,
                            &file_door,
                            ctx.owner_interface(),
                            &CheckpointId::new(&result_id),
                            &installed,
                        )?;
                        reader.commit_through(own)?;
                        return Ok::<_, String>(Some((own.movement.clone(), envelope)));
                    }
                    reader.commit_through(own)?;
                }
                if rows.len() == 256 {
                    continue;
                }
                reader.wait_for_change(&cancel).await?;
            }
        })
        .await
        .map_err(|_| "code mode checked authoring result timed out".to_string())??;
        let Some(result) = result else {
            let advanced = Self::reconcile_provider_advance(ctx, root, &state.config, None).await?
                .ok_or("rejected authoring has no independent checked advance")?;
            if advanced.after_hash == edit.before_hash || advanced.after_hash == edit.after_hash {
                return Err("rejected authoring cannot retry without an independent checked effect".into());
            }
            state.expected_hash = advanced.after_hash;
            continue;
        };
        continuation::record_install(
            root,
            &InstalledHandoff {
                before_hash: edit.before_hash,
                after_hash: edit.after_hash.clone(),
                source_seq: call_seq,
                source_artifact: call_artifact,
                result_seq: result.0.seq.0,
                result_artifact: result.1.artifact.clone(),
            },
        )?;
        for application in continuation::applications(root)? {
            if application_change
                .as_ref()
                .is_some_and(|change| change.retiring() && change.application().pending_digest == application.pending_digest)
            {
                continue;
            }
            let after_record: octopus_ledger::ContentHash = ContentHash::of_bytes(edit.content.as_bytes());
            if after_record.to_hex() != edit.after_hash { return Err("application projection changed its installed prospective bytes".into()); }
            let suffixes = ["approved", "denied"];
            let checkpoints = suffixes.into_iter().map(|suffix|format!("application_{}_{}",application.pending_digest,suffix)).collect::<Vec<_>>();
            let after: Value = serde_json::from_str(&edit.content).map_err(|e| e.to_string())?;
            if checkpoints.iter().all(|id| {
                after
                    .get("active_checkpoints")
                    .and_then(Value::as_array)
                    .is_some_and(|ids| ids.iter().any(|v| v.as_str() == Some(id.as_str())))
            }) {
                continuation::record_application_projection(
                    root,
                    &ApplicationProjection {
                        request_id: application.request.request_id,
                        pending_digest: application.pending_digest,
                        checkpoints,
                        boundary: ReturnBoundary {
                            checkpoint: output_id.clone(),
                            epoch: 0,
                            run_record_hash: edit.after_hash.clone(),
                            binding: edge.binding.clone(),
                        },
                    },
                )?;
            }
        }
        if let Some(ApplicationChange::Retire(application)) = &application_change {
            let decision: ApplicationDecision = continuation::read_record(&root.join(format!(
                "{}.application-decision.json",
                application.pending_digest
            )))?;
            continuation::record_application_retirement(
                root,
                &ApplicationRetirement {
                    pending_digest: application.pending_digest.clone(),
                    before_hash: state.expected_hash.clone(),
                    after_hash: edit.after_hash.clone(),
                    decision_artifact: decision.artifact,
                    decision_seq: decision.source_seq,
                    paired_checkpoints: paired_to_remove,
                },
            )?;
        }
        state.expected_hash = edit.after_hash;
        state.authority = ToolDoorOutputSource {
            movement: result.0,
            envelope: result.1,
            formation: state.authority.formation.clone(),
        };
        return Ok(());
        }
    }

    fn failure_config(ctx: &InterfaceLiftContext) -> Result<CodeModeConfig, String> {
        let config: CodeModeConfig = serde_json::from_slice(&octopus_interfaces::read_artifact_carrier_file_bounded(
            &ctx.interface_registry_dir().join("code_mode.json"), 1024 * 1024,
        ).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        config.validate(ctx.owner_interface().as_str())?;
        Ok(config)
    }

    fn delivered_result(
        ctx: &InterfaceLiftContext,
        source: &ToolDoorOutputSource,
        topology: &CodeModeOwnerTopology,
        result: &ToolResultArtifact,
    ) -> Result<(OwnerCheckpoint, Option<Movement>), String> {
        let limits = ctx.owner_io_limits()?;
        let evidence = octopus_run_authoring::CheckedRunEvidence {
            movements_path: ctx.owner_movements_path(), ledger_index_path: ctx.owner_ledger_index_path(), blobs_dir: ctx.owner_blobs_dir(),
            max_control_frame_bytes: limits.max_control_frame_bytes, max_manifest_bytes: limits.max_manifest_bytes, max_payload_bytes: limits.max_payload_bytes,
        };
        let runtime = source.envelope.runtime().map_err(|e| e.to_string())?;
        let provider = Endpoint::Interface(topology.model_output.binding.outbound[0].interface.clone());
        let owner = Endpoint::Interface(ctx.owner_interface().clone());
        if source.movement.kind != MovementKind::InterfaceOutbound || source.movement.to != owner
            || source.movement.artifact.as_ref() != Some(&source.envelope.artifact)
            || source.movement.invocation_metadata.as_ref().and_then(|metadata| metadata.invocation())
                != Some(source.envelope.metadata().map_err(|e| e.to_string())?)
            || octopus_run_authoring::generated_artifacts::read_movement(&evidence, source.movement.seq.0)? != source.movement {
            return Err("recorded owner failure changed its exact admitted source".into());
        }
        let (call, edge, receipt_root) = if source.envelope.read_artifact_manifest().map_err(|e| e.to_string())?.label == "tool.call" {
            let call: ToolCallArtifact = serde_json::from_slice(&source.envelope.read_artifact_payload_bounded(limits.max_payload_bytes)
                .map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
            if source.movement.from != provider {
                return Err("recorded owner failure differs from its original admitted call".into());
            }
            (call, topology.model_output.clone(), None)
        } else {
            let (call, root) = continuation::historical_continuation_call(ctx, &source.envelope, &source.movement)?;
            (call, topology.receipt.clone(), Some(root))
        };
        if call.tool != result.tool || result.correlation_id.is_none() || call.correlation_id != result.correlation_id
            || result.label != format!("{}.result", call.tool) {
            return Err("recorded owner failure differs from its original admitted call".into());
        }
        let target = Endpoint::Interface(edge.binding.outbound[0].interface.clone());
        let mut prior = octopus_run_authoring::FileCheckedRunReader::open(&evidence, Some(source.movement.seq.0))?;
        while let Some(movement) = prior.movement(prior.next_sequence())? {
            if movement.seq <= source.movement.seq || movement.kind != MovementKind::InterfaceOutbound
                || movement.from != owner || movement.to != target { continue; }
            let coordinates = movement.runtime().ok_or("recorded owner output has no admitted scope")?;
            if coordinates.run != runtime.run || coordinates.ee != runtime.ee || coordinates.worker != runtime.worker { continue; }
            let output = movement.artifact.as_ref().ok_or("recorded owner output has no artifact")?;
            let (manifest, bytes) = prior.artifact(output)?;
            if manifest.label != result.label { continue; }
            let delivered: ToolResultArtifact = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
            if delivered.tool == result.tool && delivered.correlation_id == result.correlation_id {
                if delivered.label != manifest.label { return Err("recorded owner output changed its result label".into()); }
                if let Some(root) = &receipt_root {
                    let receipt: Value = continuation::read_record(&root.join(format!("{}.receipt.json", output.content.to_hex())))?;
                    if receipt != json!({"artifact":output,"source_seq":source.movement.seq.0}) {
                        return Err("recorded internal result changed its exact original source receipt".into());
                    }
                }
                return Ok((edge, Some(movement)));
            }
        }
        Ok((edge, None))
    }

    fn retained_failure_formation(
        ctx: &InterfaceLiftContext,
        config: &CodeModeConfig,
        source: &ToolDoorOutputSource,
    ) -> Result<octopus_worker_env::WorkerEnvironmentFormation, String> {
        if source.envelope.read_artifact_manifest().map_err(|e| e.to_string())?.label == "tool.call" {
            continuation::retained_output_formation(ctx, config, &source.envelope, source.movement.seq.0)
        } else {
            continuation::retained_continuation_formation(ctx, config, &source.envelope, &source.movement)
        }
    }

    fn recorded_install_authority(
        ctx: &InterfaceLiftContext,
        reader: &LedgerTailReader,
        root: &Path,
        config: &CodeModeConfig,
        source: &ToolDoorOutputSource,
        output_edge: &OwnerCheckpoint,
        emission: &str,
    ) -> Result<OwnerOutputAuthority, String> {
        continuation::reconcile_checked_provider_advance(ctx, root, config, None)?;
        let topology = config.resolve_owner_topology(&source.envelope)?;
        let (installed, observed_hash) = Self::installed_failure_authority(ctx, reader, root, &topology, output_edge, source, emission)?;
        if let Some(installed) = installed {
            return Ok(installed);
        }
        let runtime = source.envelope.runtime().map_err(|e| e.to_string())?;
        let limits = ctx.owner_io_limits()?;
        let path = ctx.owner_root_slot("run_records_root")?.canonicalize().map_err(|e| e.to_string())?
            .join(format!("{}.json", topology.run));
        let current = octopus_interfaces::read_artifact_carrier_file_bounded(&path, limits.max_payload_bytes).map_err(|e| e.to_string())?;
        if ContentHash::of_bytes(&current).to_hex() != observed_hash {
            return Err("owner failure selection requires a fresh proof after checked advancement".into());
        }
        let hash = runtime.run_record_hash.as_deref().ok_or("recorded failure source lost its Run snapshot")?;
        let pinned = octopus_interfaces::read_artifact_carrier_file_bounded(&ctx.owner_blobs_dir().join(hash), limits.max_payload_bytes)
            .map_err(|e| e.to_string())?;
        if ContentHash::of_bytes(&pinned).to_hex() != hash { return Err("recorded failure source Run integrity mismatch".into()); }
        if octopus_run_authoring::validate_unchanged_admitted_checkpoint(&pinned, &current, &runtime.checkpoint).is_ok() {
            return Ok(OwnerOutputAuthority { source: source.clone(), installed_output: None,
                observed_run_hash: ContentHash::of_bytes(&current).to_hex() });
        }
        let evidence = octopus_run_authoring::CheckedRunEvidence {
            movements_path: ctx.owner_movements_path(), ledger_index_path: ctx.owner_ledger_index_path(), blobs_dir: ctx.owner_blobs_dir(),
            max_control_frame_bytes: limits.max_control_frame_bytes, max_manifest_bytes: limits.max_manifest_bytes, max_payload_bytes: limits.max_payload_bytes,
        };
        let checkpoint = Self::authoring_frontier(&pinned, &topology)?;
        let mut anchor = None;
        for entry in std::fs::read_dir(root).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            if !entry.file_name().to_str().is_some_and(|name| name.ends_with(".installed.json")) { continue; }
            let installed: InstalledHandoff = continuation::read_record(&entry.path())?;
            if installed.result_seq >= source.movement.seq.0 { continue; }
            let result = octopus_run_authoring::generated_artifacts::read_movement(&evidence, installed.result_seq)?;
            if result.runtime().is_none_or(|runtime| runtime.checkpoint != checkpoint) { continue; }
            let transition = octopus_run_authoring::checked_declared_run_install(&evidence, &path, &pinned,
                &octopus_run_authoring::CheckedRunInstall {
                    call: octopus_run_authoring::ConsentEvidenceRef { movement_seq: installed.source_seq, artifact: installed.source_artifact },
                    result: octopus_run_authoring::ConsentEvidenceRef { movement_seq: installed.result_seq, artifact: installed.result_artifact },
                }, topology.coauthor.iter().cloned().collect())?;
            let selected = Self::checked_owner_authority(ctx, reader, root, config, &topology, source, &transition, &evidence)?;
            let observed = octopus_run_authoring::checked_declared_run_snapshot(&evidence, &path,
                &transition.after, transition.call.seq.0, hash, source.movement.seq.0,
                topology.coauthor.iter().cloned().collect()).map_err(String::from)?;
            if observed != pinned { return Err("retained owner anchor differs from the source's exact snapshot".into()); }
            octopus_run_authoring::validate_unchanged_admitted_checkpoint(&transition.after, &pinned, &checkpoint)?;
            if anchor.replace((transition, selected)).is_some() {
                return Err("retained owner source has ambiguous installed authority".into());
            }
        }
        let (anchor, authority) = anchor.ok_or("recorded owner failure has no faithful retained installed authority")?;
        let cancellation = ctx.owner_cancel();
        let selected = continuation::checked_recovery_observation(
            || octopus_interfaces::read_artifact_carrier_file_bounded(&path, limits.max_payload_bytes).map_err(|e| e.to_string()),
            |current| {
                let transitions = octopus_run_authoring::checked_declared_run_advance(&evidence, &path,
                    &anchor.after, current, anchor.call.seq.0, topology.coauthor.iter().cloned().collect())?;
                let mut selected = authority.clone();
                for transition in transitions {
                    let checkpoint = &selected.envelope.runtime().map_err(|e| e.to_string())?.checkpoint;
                    if octopus_run_authoring::validate_unchanged_admitted_checkpoint(&transition.before, &transition.after, checkpoint).is_ok() {
                        continue;
                    }
                    if Self::authoring_frontier(&transition.before, &topology)? != *checkpoint {
                        return Err("owner succession lost its exact prior frontier".into());
                    }
                    let successor = Self::checked_owner_authority(ctx, reader, root, config, &topology, source, &transition, &evidence)?;
                    if let (Some(installed), _) = Self::installed_failure_authority(ctx, reader, root, &topology, output_edge, &selected, emission)? {
                        if installed.source.movement != transition.result {
                            return Err("retained failure output did not follow its exact proven authority".into());
                        }
                        let checkpoint = &installed.source.envelope.runtime().map_err(|e| e.to_string())?.checkpoint;
                        octopus_run_authoring::validate_unchanged_admitted_checkpoint(
                            &Self::authority_snapshot(ctx, &installed.source)?, current, checkpoint)?;
                        return Ok(installed);
                    }
                    Self::validate_owner_succession(ctx, reader, root, config, &topology, &selected, &transition, &evidence)?;
                    selected = successor;
                }
                let runtime = selected.envelope.runtime().map_err(|e| e.to_string())?;
                let pin = Self::authority_snapshot(ctx, &selected)?;
                octopus_run_authoring::validate_unchanged_admitted_checkpoint(&pin, current, &runtime.checkpoint)?;
                Ok(OwnerOutputAuthority { source: selected, installed_output: None,
                    observed_run_hash: ContentHash::of_bytes(current).to_hex() })
            },
            || cancellation.is_cancelled(),
        )?;
        Ok(selected)
    }

    fn authoring_frontier(bytes: &[u8], topology: &CodeModeOwnerTopology) -> Result<CheckpointId, String> {
        let record: Value = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        let state = record.get("interface_state").and_then(|state| state.get(&topology.owner_interface))
            .ok_or("owner authority has no retained interface state")?;
        let frontier: Vec<String> = serde_json::from_value(state.get("frontier_checkpoint_ids").cloned()
            .ok_or("owner authority has no retained frontier")?).map_err(|e| e.to_string())?;
        let owned = state.get("owned_checkpoint_ids").and_then(Value::as_array).ok_or("owner authority has no owned checkpoints")?;
        let active = record.get("active_checkpoints").and_then(Value::as_array).ok_or("owner authority has no active checkpoints")?;
        let checkpoints = record.get("checkpoints").and_then(Value::as_array).ok_or("owner authority has no checkpoints")?;
        if frontier.len() != 2 || frontier[0] == frontier[1]
            || frontier.iter().any(|id| !owned.contains(&json!(id)) || !active.contains(&json!(id))) {
            return Err("owner authority is not an exact active owned frontier".into());
        }
        let matches = checkpoints.iter().filter(|checkpoint|
            checkpoint.get("id").and_then(Value::as_str).is_some_and(|id| frontier.iter().any(|owned| owned == id))
            && checkpoint.get("interface_binding") == Some(&json!(topology.authoring_result.binding.inbound))
            && checkpoint.get("outbound_interface_bindings") == Some(&json!(topology.authoring_result.binding.outbound))
            && checkpoint.get("required_verified_artifacts") == Some(&json!([]))
            && checkpoint.get("epoch") == Some(&json!(0))).collect::<Vec<_>>();
        let [checkpoint] = matches.as_slice() else { return Err("owner authority has no unique declared authoring result".into()); };
        Ok(CheckpointId::new(checkpoint["id"].as_str().ok_or("owner authority checkpoint has no id")?))
    }

    fn authority_snapshot(ctx: &InterfaceLiftContext, source: &ToolDoorOutputSource) -> Result<Vec<u8>, String> {
        let hash = source.envelope.runtime().map_err(|e| e.to_string())?.run_record_hash.as_deref()
            .ok_or("owner authority has no admitted snapshot")?;
        let pin = ContentHash::from_hex(hash).filter(|pin| pin.to_hex() == hash).ok_or("owner authority snapshot hash is not canonical")?;
        let bytes = octopus_interfaces::read_artifact_carrier_file_bounded(&ctx.owner_blobs_dir().join(hash),
            ctx.owner_io_limits()?.max_payload_bytes).map_err(|e| e.to_string())?;
        if ContentHash::of_bytes(&bytes) != pin { return Err("owner authority snapshot integrity mismatch".into()); }
        Ok(bytes)
    }

    fn checked_owner_authority(
        ctx: &InterfaceLiftContext, reader: &LedgerTailReader, root: &Path, config: &CodeModeConfig,
        topology: &CodeModeOwnerTopology, source: &ToolDoorOutputSource,
        transition: &octopus_run_authoring::CheckedRunTransition,
        evidence: &octopus_run_authoring::CheckedRunEvidence<'_>,
    ) -> Result<ToolDoorOutputSource, String> {
        if transition.writer.owner != *ctx.owner_interface() || transition.writer.source != *ctx.owner_interface()
            || transition.writer.door != topology.authoring_result.binding.inbound.interface || transition.writer.tool != topology.file_write_tool {
            return Err("retained failure authority changed its exact owner writer".into());
        }
        let after_hash = ContentHash::of_bytes(&transition.after).to_hex();
        let installed: InstalledHandoff = continuation::read_record(&root.join(format!("{after_hash}.installed.json")))?;
        if installed != (InstalledHandoff {
            before_hash: ContentHash::of_bytes(&transition.before).to_hex(), after_hash,
            source_seq: transition.call.seq.0, source_artifact: transition.call.artifact.clone().ok_or("owner write has no artifact")?,
            result_seq: transition.result.seq.0, result_artifact: transition.result.artifact.clone().ok_or("owner result has no artifact")?,
        }) { return Err("retained failure authority changed its checked installation".into()); }
        let mut artifacts = octopus_run_authoring::FileCheckedRunReader::open(evidence, None)?;
        let (_, payload) = artifacts.artifact(transition.call.artifact.as_ref().ok_or("owner write has no artifact")?)?;
        let call: ToolCallArtifact = serde_json::from_slice(&payload).map_err(|e| e.to_string())?;
        let correlation = call.correlation_id.as_deref().ok_or("owner succession has no authoring correlation")?;
        let formation = source.formation.as_ref().ok_or("owner succession has no retained formation")?;
        let prefix = format!("code-mode-authoring:{}:", formation.worker_environment_id);
        let generation = correlation.strip_prefix(&prefix).filter(|key| ContentHash::from_hex(key).is_some_and(|hash| hash.to_hex() == *key))
            .ok_or("owner succession changed its exact formation or generation")?;
        let edit: CheckedRunEdit = continuation::read_record(&root.join(format!("{generation}.edit.json")))?;
        let path = ctx.owner_root_slot("run_records_root")?.canonicalize().map_err(|e| e.to_string())?.join(format!("{}.json", topology.run));
        let expected = CheckedRunEdit::prepare_with_format(path, &transition.before,
            serde_json::from_slice(&transition.after).map_err(|e| e.to_string())?, correlation.to_owned(), edit.json_format)?;
        if call.arguments != edit.arguments()? || serde_json::to_value(&edit).map_err(|e| e.to_string())? != serde_json::to_value(expected).map_err(|e| e.to_string())? {
            return Err("owner succession differs from its full immutable edit proposal".into());
        }
        let envelope = reader.envelope_for_movement(&transition.result)?;
        let runtime = envelope.runtime().map_err(|e| e.to_string())?;
        let original = source.envelope.runtime().map_err(|e| e.to_string())?;
        if runtime.run != original.run || runtime.ee != original.ee || runtime.worker != original.worker
            || runtime.checkpoint.as_str() != format!("code_mode_{generation}_{}", topology.authoring_result.name)
            || Self::authoring_frontier(&transition.after, topology)? != runtime.checkpoint
            || config.resolve_owner_topology(&envelope)? != *topology
            || !continuation::validate_historical_authoring_result(ctx, &envelope, &transition.result)? {
            return Err("owner succession changed its exact installed return boundary".into());
        }
        use octopus_code_mode_room::config::ResolvedContext;
        let (ResolvedContext::Persistent { artifact: old_artifact, payload: old_payload, .. },
             ResolvedContext::Persistent { artifact, payload, .. }) = (config.resolve_context(&source.envelope)?, config.resolve_context(&envelope)?) else {
            return Err("owner succession lost its persistent context".into());
        };
        if artifact != old_artifact || payload != old_payload { return Err("owner succession changed its immutable prelude".into()); }
        continuation::validate_admitted_coauthor(ctx, &envelope, topology)?;
        Ok(ToolDoorOutputSource { movement: transition.result.clone(), envelope, formation: source.formation.clone() })
    }

    fn prospective_failure_output(
        ctx: &InterfaceLiftContext,
        reference: &octopus_interfaces::ArtifactRef,
        source_seq: u64,
    ) -> Result<(octopus_interfaces::InterfaceArtifactManifest, Vec<u8>), String> {
        let root = ctx.interface_var_dir().join("failure-artifacts");
        if root.canonicalize().map_err(|e| e.to_string())? != root {
            return Err("prospective failure carrier changed its private root".into());
        }
        let limits = ctx.owner_io_limits()?;
        let suffix = format!("-{}.artifact.json", reference.content.to_hex());
        let mut selected = None;
        for entry in std::fs::read_dir(&root).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            if !entry.file_name().to_str().is_some_and(|name| name.ends_with(&suffix)) { continue; }
            let path = entry.path();
            if !entry.file_type().map_err(|e| e.to_string())?.is_file()
                || path.canonicalize().map_err(|e| e.to_string())? != path {
                return Err("prospective failure manifest is not a private regular file".into());
            }
            let inspected = InterfaceArtifactFile::new(&path).map_err(|e| e.to_string())?
                .read_manifest_bounded(limits.max_manifest_bytes).map_err(|e| e.to_string())?;
            let manifest = &inspected.manifest;
            if ContentHash::of_bytes(&inspected.manifest_bytes) != reference.content
                || reference.id.as_str() != format!("{}:{}", manifest.label, reference.content.to_hex())
                || !manifest.payload_content_hash.as_deref().is_some_and(|hash|
                    ContentHash::from_hex(hash).is_some_and(|parsed| parsed.to_hex() == hash))
                || manifest.payload_path.parent() != Some(root.as_path())
                || manifest.payload_path.canonicalize().map_err(|e| e.to_string())? != manifest.payload_path {
                return Err("prospective failure changed its exact private carrier or immutable bytes".into());
            }
            let payload_bytes = octopus_interfaces::read_artifact_carrier_file_bounded(
                &manifest.payload_path, limits.max_payload_bytes).map_err(|e| e.to_string())?;
            let captured = InterfaceArtifactFile::from_captured_snapshot(path, inspected.manifest_bytes, payload_bytes)
                .map_err(|e| e.to_string())?.read_bounded(limits.max_manifest_bytes, limits.max_payload_bytes)
                .map_err(|e| e.to_string())?;
            let manifest = &captured.manifest;
            let result: ToolResultArtifact = serde_json::from_slice(&captured.payload_bytes).map_err(|e| e.to_string())?;
            if result.ok || result.body.is_some() || result.label != manifest.label
                || result.error.as_ref().is_none_or(|error|
                    !["tool_effect_indeterminate", "tool_process_failure"].contains(&error.code.as_str())
                    || error.details.as_ref().and_then(|details| details.get("source_movement_seq"))
                        .and_then(Value::as_u64) != Some(source_seq)) {
                return Err("prospective failure differs from its exact recorded cause".into());
            }
            if selected.replace((captured.manifest, captured.payload_bytes)).is_some() {
                return Err("prospective failure has ambiguous private carriers".into());
            }
        }
        selected.ok_or_else(|| "prospective failure has no exact retained private carrier".into())
    }

    fn validate_owner_succession(
        ctx: &InterfaceLiftContext, reader: &LedgerTailReader, root: &Path, config: &CodeModeConfig,
        topology: &CodeModeOwnerTopology, selected: &ToolDoorOutputSource,
        transition: &octopus_run_authoring::CheckedRunTransition,
        evidence: &octopus_run_authoring::CheckedRunEvidence<'_>,
    ) -> Result<(), String> {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Receipt { artifact: octopus_interfaces::ArtifactRef, source_seq: u64 }
        let mut artifacts = octopus_run_authoring::FileCheckedRunReader::open(evidence, None)?;
        let (_, payload) = artifacts.artifact(transition.call.artifact.as_ref().ok_or("owner write has no artifact")?)?;
        let body: ToolCallArtifact = serde_json::from_slice(&payload).map_err(|e| e.to_string())?;
        let formation = selected.formation.as_ref().ok_or("owner succession has no formation")?;
        let before_hash = ContentHash::of_bytes(&transition.before).to_hex();
        if Self::admitted_emission_succession(ctx, topology, selected, transition, evidence, &body)? {
            return Ok(());
        }
        let mut proven = false;
        for entry in std::fs::read_dir(root).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            if !entry.file_name().to_str().is_some_and(|name| name.ends_with(".receipt.json")) { continue; }
            let receipt: Receipt = continuation::read_record(&entry.path())?;
            let emission = receipt.artifact.content.to_hex();
            if entry.file_name().to_str() != Some(format!("{emission}.receipt.json").as_str()) {
                return Err("owner succession receipt changed its immutable artifact identity".into());
            }
            for (edge, emission, authority_seq) in [(&topology.receipt, emission.clone(), receipt.source_seq),
                (&topology.readiness, format!("{emission}-idle"), selected.movement.seq.0)] {
                let generation = continuation::digest(&json!({"before":before_hash,"emission":emission,"edge":edge,"source_seq":authority_seq}))?;
                if body.correlation_id.as_deref() != Some(format!("code-mode-authoring:{}:{generation}", formation.worker_environment_id).as_str()) { continue; }
                if receipt.source_seq >= transition.call.seq.0 { return Err("owner succession receipt has no earlier admitted cause".into()); }
                let movement = octopus_run_authoring::generated_artifacts::read_movement(evidence, receipt.source_seq)?;
                let envelope = reader.envelope_for_movement(&movement)?;
                let cause_formation = continuation::retained_continuation_formation(ctx, config, &envelope, &movement)?;
                if cause_formation != *formation || octopus_tool_door_runtime::tool_door_durable_state_dir(ctx, &cause_formation.worker_environment_id)? != root {
                    return Err("owner succession cause changed its immutable formation".into());
                }
                let cause = ToolDoorOutputSource { movement, envelope, formation: Some(cause_formation) };
                let mut artifacts = octopus_run_authoring::FileCheckedRunReader::open(evidence, Some(receipt.source_seq))?;
                let mut admitted = false;
                while let Some(movement) = artifacts.movement(artifacts.next_sequence())? {
                    if matches!(movement.kind, MovementKind::InterfaceArrival | MovementKind::InterfaceOutbound)
                        && movement.from == Endpoint::Interface(ctx.owner_interface().clone())
                        && movement.artifact.as_ref() == Some(&receipt.artifact) {
                        admitted = true;
                        break;
                    }
                }
                let (manifest, payload) = if admitted {
                    artifacts.artifact(&receipt.artifact)?
                } else {
                    Self::prospective_failure_output(ctx, &receipt.artifact, receipt.source_seq)?
                };
                let output: ToolResultArtifact = serde_json::from_slice(&payload).map_err(|e| e.to_string())?;
                if manifest.label != output.label { return Err("owner succession output receipt changed its exact label".into()); }
                let (output_edge, delivered) = Self::delivered_result(ctx, &cause, topology, &output)?;
                if output_edge != topology.receipt || delivered.as_ref().is_some_and(|row| row.artifact.as_ref() != Some(&receipt.artifact)) {
                    return Err("owner succession output differs from its exact internal receipt".into());
                }
                let authority = if edge == &topology.readiness {
                    let delivered = delivered.ok_or("owner readiness succession has no admitted output receipt")?;
                    let output_runtime = delivered.runtime().ok_or("owner succession output has no checkpoint")?;
                    let selected_runtime = selected.envelope.runtime().map_err(|e| e.to_string())?;
                    let prefix = selected_runtime.checkpoint.as_str().strip_suffix(&format!("_{}", topology.authoring_result.name))
                        .ok_or("owner readiness lost its installed authoring result")?;
                    if delivered.seq <= selected.movement.seq || delivered.seq >= transition.call.seq
                        || output_runtime.checkpoint.as_str() != format!("{prefix}_{}", topology.receipt.name) {
                        return Err("owner readiness did not follow its selected installed output".into());
                    }
                    let output_checkpoint = output_runtime.checkpoint.clone();
                    let delivered_source = ToolDoorOutputSource { envelope: reader.envelope_for_movement(&delivered)?, movement: delivered, formation: selected.formation.clone() };
                    octopus_run_authoring::validate_unchanged_admitted_checkpoint(&Self::authority_snapshot(ctx, selected)?,
                        &Self::authority_snapshot(ctx, &delivered_source)?, &output_checkpoint)?;
                    selected
                } else { &cause };
                let runtime = authority.envelope.runtime().map_err(|e| e.to_string())?;
                let call_runtime = transition.call.runtime().ok_or("owner succession write has no admitted coordinate")?;
                if call_runtime.checkpoint != runtime.checkpoint || call_runtime.epoch != runtime.epoch {
                    return Err("owner succession write did not use its actual admitted cause".into());
                }
                octopus_run_authoring::validate_unchanged_admitted_checkpoint(&Self::authority_snapshot(ctx, authority)?,
                    &transition.before, &runtime.checkpoint)?;
                if proven { return Err("owner succession has ambiguous immutable output receipts".into()); }
                proven = true;
            }
        }
        if !proven { return Err("owner succession has no exact admitted cause and immutable output receipt".into()); }
        Ok(())
    }

    fn admitted_emission_succession(
        ctx: &InterfaceLiftContext, topology: &CodeModeOwnerTopology, selected: &ToolDoorOutputSource,
        transition: &octopus_run_authoring::CheckedRunTransition,
        evidence: &octopus_run_authoring::CheckedRunEvidence<'_>, call: &ToolCallArtifact,
    ) -> Result<bool, String> {
        let runtime = selected.envelope.runtime().map_err(|e| e.to_string())?;
        let writer = transition.call.runtime().ok_or("owner succession write has no admitted scope")?;
        if writer.run != runtime.run || writer.ee != runtime.ee || writer.worker != runtime.worker
            || writer.checkpoint != runtime.checkpoint || writer.epoch != runtime.epoch {
            return Ok(false);
        }
        octopus_run_authoring::validate_unchanged_admitted_checkpoint(
            &Self::authority_snapshot(ctx, selected)?, &transition.before, &runtime.checkpoint)?;
        let formation = selected.formation.as_ref().ok_or("owner succession has no formation")?;
        let before_hash = ContentHash::of_bytes(&transition.before).to_hex();
        let before: Value = serde_json::from_slice(&transition.before).map_err(|e| e.to_string())?;
        let after: Value = serde_json::from_slice(&transition.after).map_err(|e| e.to_string())?;
        let edges = topology.routes.iter().map(|route| &route.call)
            .chain([&topology.model_output, &topology.receipt]).chain(topology.notification.iter()).collect::<Vec<_>>();
        let mut movements = octopus_run_authoring::FileCheckedRunReader::open(evidence, Some(selected.movement.seq.0))?;
        while let Some(movement) = movements.movement(movements.next_sequence())? {
            if movement.seq <= selected.movement.seq || movement.kind != MovementKind::InterfaceOutbound
                || movement.from != Endpoint::Interface(ctx.owner_interface().clone()) { continue; }
            let observed = movement.runtime().ok_or("owner emission has no admitted scope")?;
            if observed.run != runtime.run || observed.ee != runtime.ee || observed.worker != runtime.worker { continue; }
            let artifact = movement.artifact.as_ref().ok_or("owner emission has no artifact")?;
            let (edge, emission, record) = if movement.seq > transition.result.seq {
                let mut found = None;
                for edge in &edges {
                    let generation = continuation::digest(&json!({"before":before_hash,"emission":artifact.content.to_hex(),
                        "edge":edge,"source_seq":selected.movement.seq.0}))?;
                    if observed.checkpoint.as_str() == format!("code_mode_{generation}_{}", edge.name)
                        && found.replace(*edge).is_some() { return Err("owner emission has ambiguous declared edges".into()); }
                }
                let Some(edge) = found else { continue; };
                (edge, artifact.content.to_hex(), &after)
            } else if movement.seq < transition.call.seq {
                let (manifest, payload) = movements.artifact(artifact)?;
                if !manifest.label.ends_with(".result") { continue; }
                let result: ToolResultArtifact = serde_json::from_slice(&payload).map_err(|e| e.to_string())?;
                if result.label != manifest.label { return Err("owner readiness source changed its result label".into()); }
                (&topology.readiness, format!("{}-idle", artifact.content.to_hex()), &before)
            } else { continue; };
            let generation = continuation::digest(&json!({"before":before_hash,"emission":emission,
                "edge":edge,"source_seq":selected.movement.seq.0}))?;
            if call.correlation_id.as_deref() != Some(format!("code-mode-authoring:{}:{generation}", formation.worker_environment_id).as_str()) { continue; }
            let checkpoint = record.get("checkpoints").and_then(Value::as_array)
                .and_then(|checkpoints| checkpoints.iter().find(|checkpoint| checkpoint.get("id") == Some(&json!(observed.checkpoint))))
                .ok_or("owner succession emission lost its installed checkpoint")?;
            let valid_edge = if edge == &topology.readiness {
                let prefix = runtime.checkpoint.as_str().strip_suffix(&format!("_{}", topology.authoring_result.name))
                    .ok_or("owner readiness lost its selected authoring result")?;
                [&topology.model_output, &topology.receipt].into_iter().any(|output|
                    observed.checkpoint.as_str() == format!("{prefix}_{}", output.name)
                    && checkpoint.get("interface_binding") == Some(&json!(output.binding.inbound))
                    && checkpoint.get("outbound_interface_bindings") == Some(&json!(output.binding.outbound))
                    && output.binding.outbound.iter().any(|binding| movement.to == Endpoint::Interface(binding.interface.clone())))
            } else {
                checkpoint.get("interface_binding") == Some(&json!(edge.binding.inbound))
                    && checkpoint.get("outbound_interface_bindings") == Some(&json!(edge.binding.outbound))
                    && edge.binding.outbound.iter().any(|binding| movement.to == Endpoint::Interface(binding.interface.clone()))
            };
            if !record.get("active_checkpoints").and_then(Value::as_array).is_some_and(|ids| ids.contains(&json!(observed.checkpoint)))
                || checkpoint.get("epoch") != Some(&json!(observed.epoch)) || !valid_edge
                || checkpoint.get("required_verified_artifacts") != Some(&json!([])) {
                return Err("owner succession emission changed its exact installed return boundary".into());
            }
            return Ok(true);
        }
        Ok(false)
    }

    fn installed_failure_authority(
        ctx: &InterfaceLiftContext,
        reader: &LedgerTailReader,
        root: &Path,
        topology: &CodeModeOwnerTopology,
        edge: &OwnerCheckpoint,
        failure: &ToolDoorOutputSource,
        emission: &str,
    ) -> Result<(Option<OwnerOutputAuthority>, String), String> {
        use octopus_run_authoring::{CheckedRunInstall, ConsentEvidenceRef};
        let runtime = failure.envelope.runtime().map_err(|e| e.to_string())?;
        let limits = ctx.owner_io_limits()?;
        let hash = runtime.run_record_hash.as_deref().ok_or("owner failure has no pinned Run")?;
        let pin = ContentHash::from_hex(hash).filter(|pin| pin.to_hex() == hash)
            .ok_or("owner failure has a noncanonical Run snapshot hash")?;
        let pinned = octopus_interfaces::read_artifact_carrier_file_bounded(
            &ctx.owner_blobs_dir().join(hash),
            limits.max_payload_bytes,
        ).map_err(|e| e.to_string())?;
        if ContentHash::of_bytes(&pinned) != pin { return Err("owner failure Run snapshot integrity mismatch".into()); }
        let path = ctx.owner_root_slot("run_records_root")?.canonicalize().map_err(|e| e.to_string())?
            .join(format!("{}.json", topology.run));
        let evidence = octopus_run_authoring::CheckedRunEvidence {
            movements_path: ctx.owner_movements_path(), ledger_index_path: ctx.owner_ledger_index_path(), blobs_dir: ctx.owner_blobs_dir(),
            max_control_frame_bytes: limits.max_control_frame_bytes, max_manifest_bytes: limits.max_manifest_bytes, max_payload_bytes: limits.max_payload_bytes,
        };
        let cancellation = ctx.owner_cancel();
        continuation::checked_recovery_observation(
            || octopus_interfaces::read_artifact_carrier_file_bounded(&path, limits.max_payload_bytes).map_err(|e| e.to_string()),
            |current| {
        let transitions = octopus_run_authoring::checked_declared_run_advance(&evidence, &path, &pinned,
            current, failure.movement.seq.0, topology.coauthor.iter().cloned().collect())?;
        let mut artifacts = octopus_run_authoring::FileCheckedRunReader::open(&evidence, None)?;
        for transition in &transitions {
            if transition.writer.owner != *ctx.owner_interface() { continue; }
            let before_hash = ContentHash::of_bytes(&transition.before).to_hex();
            let generation = continuation::digest(&json!({"before":before_hash,
                "emission":emission,"edge":edge,"source_seq":failure.movement.seq.0}))?;
            let expected = format!("code-mode-authoring:{}:{generation}", failure.formation.as_ref()
                .ok_or("owner failure lost its formation")?.worker_environment_id);
            let source_artifact = transition.call.artifact.clone().ok_or("checked owner write has no artifact")?;
            let (_, payload) = artifacts.artifact(&source_artifact)?;
            let call: ToolCallArtifact = serde_json::from_slice(&payload).map_err(|e| e.to_string())?;
            if call.correlation_id.as_deref() != Some(expected.as_str()) { continue; }
            let after_hash = ContentHash::of_bytes(&transition.after).to_hex();
            let installed: InstalledHandoff = continuation::read_record(&root.join(format!("{after_hash}.installed.json")))?;
            if installed != (InstalledHandoff { before_hash, after_hash, source_seq: transition.call.seq.0,
                source_artifact, result_seq: transition.result.seq.0,
                result_artifact: transition.result.artifact.clone().ok_or("checked owner result has no artifact")? }) {
                return Err("checked failure write lacks its exact published installation".into());
            }
        }
        let current_record: Value = serde_json::from_slice(current).map_err(|e| e.to_string())?;
        let active = current_record.get("active_checkpoints").and_then(Value::as_array)
            .ok_or("retained failure current Run has no active checkpoints")?;
        let mut selected = None;
        for entry in std::fs::read_dir(root).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            if !entry.file_name().to_str().is_some_and(|name| name.ends_with(".installed.json")) { continue; }
            let installed: InstalledHandoff = continuation::read_record(&entry.path())?;
            if installed.source_seq <= failure.movement.seq.0 { continue; }
            let generation = continuation::digest(&json!({"before":installed.before_hash,
                "emission":emission,"edge":edge,"source_seq":failure.movement.seq.0}))?;
            let correlation = format!("code-mode-authoring:{}:{generation}", failure.formation.as_ref()
                .ok_or("owner failure lost its formation")?.worker_environment_id);
            let call = octopus_run_authoring::generated_artifacts::read_movement(&evidence, installed.source_seq)?;
            if call.artifact.as_ref() != Some(&installed.source_artifact) {
                return Err("recorded owner installation changed its admitted call".into());
            }
            let mut artifacts = octopus_run_authoring::FileCheckedRunReader::open(&evidence, None)?;
            let (_, payload) = artifacts.artifact(&installed.source_artifact)?;
            let body: ToolCallArtifact = serde_json::from_slice(&payload).map_err(|e| e.to_string())?;
            if body.correlation_id.as_deref() != Some(correlation.as_str()) { continue; }
            let proposal = root.join(format!("{generation}.edit.json"));
            let edit: CheckedRunEdit = continuation::read_record(&proposal)?;
            let transition = octopus_run_authoring::checked_declared_run_install(&evidence, &path, &pinned,
                &CheckedRunInstall {
                    call: ConsentEvidenceRef { movement_seq: installed.source_seq, artifact: installed.source_artifact.clone() },
                    result: ConsentEvidenceRef { movement_seq: installed.result_seq, artifact: installed.result_artifact.clone() },
                }, topology.coauthor.iter().cloned().collect())?;
            let expected = CheckedRunEdit::prepare_with_format(path.clone(), &transition.before,
                serde_json::from_slice(&transition.after).map_err(|e| e.to_string())?, correlation, edit.json_format)?;
            if transition.writer.owner != *ctx.owner_interface() || transition.writer.source != *ctx.owner_interface()
                || transition.writer.door != topology.authoring_result.binding.inbound.interface
                || transition.writer.tool != topology.file_write_tool
                || edit.before_hash != installed.before_hash || edit.after_hash != installed.after_hash
                || serde_json::to_value(&edit).map_err(|e| e.to_string())? != serde_json::to_value(expected).map_err(|e| e.to_string())? {
                return Err("recorded owner output differs from its exact prepared checked write".into());
            }
            octopus_run_authoring::validate_unchanged_admitted_checkpoint(&pinned, &transition.before, &runtime.checkpoint)?;
            let call_runtime = transition.call.invocation_metadata.as_ref().and_then(|metadata| metadata.invocation())
                .and_then(|metadata| metadata.runtime()).ok_or("recorded owner write lost its admitted authority")?;
            if call_runtime.checkpoint != runtime.checkpoint || call_runtime.epoch != runtime.epoch {
                return Err("recorded owner output used another admitted authority".into());
            }
            let envelope = reader.envelope_for_movement(&transition.result)?;
            let result_runtime = envelope.runtime().map_err(|e| e.to_string())?;
            if result_runtime.checkpoint.as_str() != format!("code_mode_{generation}_{}", topology.authoring_result.name)
                || result_runtime.epoch != 0 {
                return Err("recorded owner output has another checked result checkpoint".into());
            }
            let after: Value = serde_json::from_slice(&transition.after).map_err(|e| e.to_string())?;
            let output_id = format!("code_mode_{generation}_{}", edge.name);
            let output = after.get("checkpoints").and_then(Value::as_array)
                .and_then(|checkpoints| checkpoints.iter().find(|checkpoint| checkpoint.get("id").and_then(Value::as_str) == Some(output_id.as_str())))
                .ok_or("recorded owner output has no prepared output checkpoint")?.clone();
            if output.get("epoch").and_then(Value::as_u64) != Some(0) || output.get("required_verified_artifacts") != Some(&json!([]))
                || output.get("interface_binding") != Some(&json!(edge.binding.inbound))
                || output.get("outbound_interface_bindings") != Some(&json!(edge.binding.outbound)) {
                return Err("recorded owner output changed its prepared output binding".into());
            }
            if !active.contains(&json!(output_id)) { continue; }
            octopus_run_authoring::validate_unchanged_admitted_checkpoint(
                &transition.after, current, &CheckpointId::new(&output_id))?;
            if selected.replace(OwnerOutputAuthority {
                source: ToolDoorOutputSource { movement: transition.result, envelope, formation: failure.formation.clone() },
                installed_output: Some(output),
                observed_run_hash: ContentHash::of_bytes(current).to_hex(),
            }).is_some() { return Err("recorded owner failure has ambiguous installed outputs".into()); }
        }
        Ok((selected, ContentHash::of_bytes(current).to_hex()))
            },
            || cancellation.is_cancelled(),
        ).map_err(String::from)
    }

    pub(crate) fn reconcile_failure<'a>(
        &'a self,
        ctx: &'a InterfaceLiftContext,
        reader: &'a mut LedgerTailReader,
        failure: &'a Movement,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>> {
        Box::pin(async move {
        let owner = Endpoint::Interface(ctx.owner_interface().clone());
        let refused = failure.kind == MovementKind::DeliveryRefused && failure.from == owner;
        if !refused && (failure.kind != MovementKind::InterfaceOutbound || failure.from != owner || failure.to != owner) {
            return Ok(());
        }
        if refused && !matches!(failure.invocation_metadata.as_ref().and_then(|metadata| metadata.delivery_verdict()),
            Some(octopus_ledger::DeliveryVerdict::Refused { reason: octopus_ledger::RefusedReason::CheckpointSpent, .. })) {
            return Ok(());
        }
        let limits = ctx.owner_io_limits()?;
        let evidence = octopus_run_authoring::CheckedRunEvidence {
            movements_path: ctx.owner_movements_path(), ledger_index_path: ctx.owner_ledger_index_path(), blobs_dir: ctx.owner_blobs_dir(),
            max_control_frame_bytes: limits.max_control_frame_bytes, max_manifest_bytes: limits.max_manifest_bytes, max_payload_bytes: limits.max_payload_bytes,
        };
        let reference = failure.artifact.as_ref().ok_or("recorded owner failure lost its artifact")?;
        let file = InterfaceArtifactFile::new(ctx.owner_blobs_dir().join(reference.content.to_hex()))
            .map_err(|e| e.to_string())?.with_payload_cache_root(ctx.owner_blobs_dir());
        if continuation::artifact_ref(&file)? != *reference { return Err("recorded owner failure changed its exact artifact".into()); }
        let captured = file.read_bounded(limits.max_manifest_bytes, limits.max_payload_bytes).map_err(|e| e.to_string())?;
        if !captured.manifest.label.ends_with(".result") { return Ok(()); }
        let result: ToolResultArtifact = serde_json::from_slice(&captured.payload_bytes).map_err(|e| e.to_string())?;
        let Some(error) = result.error.as_ref().filter(|error|
            ["tool_effect_indeterminate", "tool_process_failure"].contains(&error.code.as_str())) else { return Ok(()); };
        if octopus_run_authoring::generated_artifacts::read_movement(&evidence, failure.seq.0)? != *failure {
            return Err("recorded owner failure differs from its durable evidence".into());
        }
        let source_seq = error.details.as_ref().and_then(|details| details.get("source_movement_seq"))
            .and_then(Value::as_u64).filter(|seq| *seq < failure.seq.0)
            .ok_or("recorded owner failure has no causally earlier source")?;
        if result.ok || result.body.is_some() || result.label != captured.manifest.label {
            return Err("recorded owner failure has a contradictory result body".into());
        }
        let source = octopus_run_authoring::generated_artifacts::read_movement(&evidence, source_seq)?;
        let input = reader.envelope_for_movement(&source)?;
        let runtime = input.runtime().map_err(|e| e.to_string())?;
        let failed_runtime = failure.runtime().ok_or("recorded owner failure has no exact refused or admitted scope")?;
        if runtime.run != failed_runtime.run || runtime.ee != failed_runtime.ee || runtime.worker != failed_runtime.worker
            || failed_runtime.interface != *ctx.owner_interface()
            || (refused && failure.to != Endpoint::Interface(runtime.worker.clone())) {
            return Err("recorded owner failure differs from its original admitted call".into());
        }
        let config = Self::failure_config(ctx)?;
        if matches!(config.context, CodeModeContext::Isolated) { return Ok(()); }
        let topology = config.resolve_owner_topology(&input)?;
        let mut output_source = ToolDoorOutputSource { movement: source, envelope: input, formation: None };
        let (edge, delivered) = Self::delivered_result(ctx, &output_source, &topology, &result)?;
        if let Some(delivered) = delivered {
            if refused || delivered.artifact.as_ref() == Some(reference) { return Ok(()); }
            return Err("recorded owner failure conflicts with an already delivered result".into());
        }
        if refused {
            let hash = failed_runtime.run_record_hash.as_deref().ok_or("recorded refusal lost its observed Run hash")?;
            let original_hash = output_source.envelope.runtime().map_err(|error| error.to_string())?
                .run_record_hash.as_deref().ok_or("recorded failure source lost its admitted Run pin")?;
            let original = octopus_interfaces::read_artifact_carrier_file_bounded(
                &ctx.owner_blobs_dir().join(original_hash), limits.max_payload_bytes).map_err(|error| error.to_string())?;
            if ContentHash::of_bytes(&original).to_hex() != original_hash {
                return Err("recorded failure source Run integrity mismatch".into());
            }
            let path = ctx.owner_root_slot("run_records_root")?.canonicalize().map_err(|error| error.to_string())?
                .join(format!("{}.json", topology.run));
            let cancel = ctx.owner_cancel();
            let bytes = loop {
                if cancel.is_cancelled() { return Err("recorded failure proof was cancelled".into()); }
                match octopus_run_authoring::checked_declared_run_snapshot(&evidence, &path,
                    &original, source_seq, hash, failure.seq.0, topology.coauthor.iter().cloned().collect()) {
                    Ok(bytes) => break bytes,
                    Err(octopus_run_authoring::CheckedRunError::AwaitingResults { .. }) => {
                        reader.wait_for_change(&cancel).await?;
                    }
                    Err(error) => return Err(String::from(error)),
                }
            };
            if ContentHash::of_bytes(&bytes).to_hex() != hash { return Err("recorded refusal Run integrity mismatch".into()); }
            let record: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
            let checkpoint = record.get("checkpoints").and_then(Value::as_array).and_then(|checkpoints|
                checkpoints.iter().find(|checkpoint| checkpoint.get("id") == Some(&json!(failed_runtime.checkpoint))))
                .ok_or("recorded refusal lost its exact output checkpoint")?;
            if checkpoint.get("epoch") != Some(&json!(failed_runtime.epoch))
                || checkpoint.get("interface_binding") != Some(&json!(edge.binding.inbound))
                || checkpoint.get("outbound_interface_bindings") != Some(&json!(edge.binding.outbound)) {
                return Err("recorded refusal differs from its exact owner output".into());
            }
        }
        let formation = Self::retained_failure_formation(ctx, &config, &output_source)?;
        output_source.formation = Some(formation.clone());
        let root = octopus_tool_door_runtime::tool_door_durable_state_dir(ctx, &formation.worker_environment_id)?;
        let mut authority = if refused {
            Self::recorded_install_authority(ctx, reader, &root, &config, &output_source, &edge, &reference.content.to_hex())?
        } else {
            OwnerOutputAuthority {
                source: ToolDoorOutputSource { movement: failure.clone(), envelope: reader.envelope_for_movement(failure)?, formation: Some(formation) },
                installed_output: None,
                observed_run_hash: failed_runtime.run_record_hash.clone().ok_or("recorded failure has no admitted Run hash")?,
            }
        };
        let artifact = InterfaceArtifactFile::store_manifest_bounded(ctx.interface_var_dir().join("failure-artifacts"),
            &captured.manifest_bytes, limits.max_manifest_bytes, limits.max_payload_bytes).map_err(|e| e.to_string())?;
        if continuation::artifact_ref(&artifact)? != *reference
            || artifact.read_payload_bytes_bounded(limits.max_payload_bytes).map_err(|e| e.to_string())? != captured.payload_bytes {
            return Err("recorded owner failure changed during private output materialization".into());
        }
        if authority.installed_output.is_none() {
            let (installed, current_hash) = Self::installed_failure_authority(ctx, reader, &root, &topology, &edge, &authority.source, &reference.content.to_hex())?;
            if let Some(installed) = installed { authority = installed; }
            else { authority.observed_run_hash = current_hash; }
        }
        let dispatch: ToolDoorOutputDispatch = Arc::new(|ctx, artifact| Box::pin(async move { ctx.owner_hand_arrival(artifact).await }));
        match self.output_future(ctx, output_source, artifact, Some(authority), dispatch, OutputAuthorityRefresh::Recorded).await? {
            DeliveryVerdict::Admitted { .. } => Ok(()),
            refused => Err(format!("recorded owner failure return was refused: {refused:?}")),
        }
        })
    }

    pub async fn observe(self: Arc<Self>, ctx: InterfaceLiftContext) -> Result<(), String> {
        let watch_root = ctx.interface_var_dir();
        let changed = Arc::clone(&self.changed);
        let watch_error = Arc::new(std::sync::Mutex::new(None));
        let capture_error = Arc::clone(&watch_error);
        let mut watcher = octopus_tool_door_runtime::notify::recommended_watcher(
            move |event: Result<
                octopus_tool_door_runtime::notify::Event,
                octopus_tool_door_runtime::notify::Error,
            >| {
                if let Err(error) = event {
                    if let Ok(mut slot) = capture_error.lock() {
                        *slot = Some(error.to_string());
                    }
                }
                changed.notify_one();
            },
        )
        .map_err(|e| format!("owner readiness observer could not start: {e}"))?;
        watcher
            .watch(&watch_root, RecursiveMode::Recursive)
            .map_err(|e| e.to_string())?;
        let cancel = ctx.owner_cancel();
        let mut reader = LedgerTailReader::open_named(&ctx, "code-mode-owner-failure-return")?;
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            for own in reader.read_new_uncommitted_with_refusals_limit(256)? {
                self.reconcile_failure(&ctx, &mut reader, &own.movement).await?;
                reader.commit_through(&own)?;
            }
            {
                let mut states = self.states.lock().await;
                for (root, state) in states.iter_mut() {
                    if !state.idle {
                        continue;
                    }
                    if let Some(ready) = continuation::ready_records(root)?.into_iter().next() {
                        if let Some(installed) = Self::reconcile_provider_advance(&ctx,root,&state.config,None).await? {
                            state.expected_hash = installed.after_hash;
                        }
                        if ready.context_generation != continuation::context_generation(root)? {
                            return Err("readiness has a stale heap identity".into());
                        }
                        let file = InterfaceArtifactFile::write(
                            root.join("readiness-artifacts"),
                            "code_mode.frontier_ready",
                            &serde_json::to_vec(&ready).map_err(|e| e.to_string())?,
                        )
                        .map_err(|e| e.to_string())?;
                        let verdict = ctx.owner_hand_arrival(file).await?;
                        continuation::create_record(
                            &root.join(format!("{}.wake-sent.json", ready.identity()?)),
                            &verdict,
                        )?;
                        match verdict {
                            DeliveryVerdict::Admitted { .. } => state.idle = false,
                            _ => {
                                return Err(format!(
                                    "fresh readiness admission refused: {verdict:?}"
                                ));
                            }
                        }
                    }
                }
            }
            if let Some(error) = watch_error
                .lock()
                .map_err(|_| "readiness observer error lock poisoned")?
                .take()
            {
                return Err(format!(
                    "owner readiness observation failed after reconciliation: {error}"
                ));
            }
            tokio::select! { _ = cancel.cancelled() => return Ok(()), _ = &mut notified => {}, changed = reader.wait_for_change(&cancel) => changed? }
        }
    }
}
