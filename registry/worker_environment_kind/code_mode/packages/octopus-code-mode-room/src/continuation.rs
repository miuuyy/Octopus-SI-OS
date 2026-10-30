use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use octopus_descriptors::{ArtifactId, DescriptorHash, InterfaceId};
use octopus_interfaces::{ArtifactRef, InterfaceInvocationEnvelope};
use octopus_ledger::{ContentHash, Endpoint, Movement};
use octopus_tool_door_runtime::{DeliveryVerdict, ToolDoorArtifactDecodePurpose};
use octopus_tool_handlers::{ToolCallArtifact, ToolResultArtifact};
use octopus_worker_env::{WorkerEnvironmentFormation, WorkerEnvironmentId};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::config::{CodeModeConfig, CodeModeRoute, ResolvedContext};

const RECORD_BYTES: usize = 96 * 1024 * 1024;
const MANIFEST_BYTES: u64 = 1024 * 1024;
static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

#[cfg(test)]
std::thread_local! {
    pub(crate) static INSTALL_PUBLICATION_BARRIER: std::cell::RefCell<Option<std::sync::Arc<std::sync::Barrier>>> = const { std::cell::RefCell::new(None) };
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OriginalRun {
    pub tool: String,
    pub correlation_id: String,
    pub artifact: ArtifactRef,
    pub source_movement_seq: u64,
    pub payload_hash: String,
    pub run_record_hash: String,
}

impl OriginalRun {
    pub(crate) fn from_envelope(
        envelope: &InterfaceInvocationEnvelope,
        seq: u64,
    ) -> Result<Self, String> {
        let captured = envelope
            .read_artifact_bounded(MANIFEST_BYTES, RECORD_BYTES as u64)
            .map_err(|e| e.to_string())?;

        if captured.manifest.label != "tool.call" {
            return Err("code mode original Run is not a tool.call artifact".into());
        }
        let call: ToolCallArtifact =
            serde_json::from_slice(&captured.payload_bytes).map_err(|e| e.to_string())?;
        let correlation_id = call
            .correlation_id
            .ok_or("persistent code mode Run requires an exact correlation_id")?;
        Ok(Self {
            tool: call.tool,
            correlation_id,
            artifact: envelope.artifact.clone(),
            source_movement_seq: seq,
            payload_hash: DescriptorHash::of_bytes(&captured.payload_bytes).to_hex(),
            run_record_hash: envelope
                .runtime()
                .map_err(|e| e.to_string())?
                .run_record_hash
                .clone()
                .filter(|hash| !hash.is_empty())
                .ok_or("persistent code mode requires its admitted run record hash")?,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContextIdentity {
    schema_version: u32,
    formation: WorkerEnvironmentFormation,
    prelude_artifact: ArtifactRef,
    prelude_payload_hash: String,
    config: CodeModeConfig,
    owner_topology: Option<crate::config::CodeModeOwnerTopology>,
    first_run: OriginalRun,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContextRecord {
    identity: ContextIdentity,
    generation: String,
}

#[derive(Clone)]
pub(crate) struct ContinuationContext {
    root: PathBuf,
    record: ContextRecord,
}

impl ContinuationContext {
    pub(crate) fn create(
        root: &Path,
        formation: &WorkerEnvironmentFormation,
        config: &CodeModeConfig,
        resolved: &ResolvedContext,
        envelope: &InterfaceInvocationEnvelope,
        original: OriginalRun,
    ) -> Result<Self, String> {
        validate_directory(root)?;
        if root
            .join("context.json")
            .try_exists()
            .map_err(|e| e.to_string())?
        {
            return Err("code mode context_lost: the durable context exists but its live heap is absent; re-execution is refused".into());
        }
        let ResolvedContext::Persistent {
            artifact, payload, ..
        } = resolved
        else {
            return Err("admitted continuation requires a persistent context".into());
        };
        formation.validate().map_err(|e| e.to_string())?;
        config.validate(formation.door_interface.as_str())?;
        let identity = ContextIdentity {
            schema_version: 1,
            formation: formation.clone(),
            prelude_artifact: artifact.clone(),
            prelude_payload_hash: DescriptorHash::of_bytes(payload).to_hex(),
            config: config.clone(),
            owner_topology: config.handoff.as_ref().map(|_| config.resolve_owner_topology(envelope)).transpose()?,
            first_run: original,
        };
        let record = ContextRecord {
            generation: digest(&identity)?,
            identity,
        };
        create_record(&root.join("context.json"), &record)?;
        Ok(Self {
            root: root.to_path_buf(),
            record,
        })
    }

    pub(crate) fn generation(&self) -> &str {
        &self.record.generation
    }

    pub(crate) fn prepare(
        &self,
        original: OriginalRun,
        cell_id: &str,
        runtime_tool_call_id: &str,
        enqueue_ordinal: u64,
        tool: &str,
        arguments: Value,
        route: CodeModeRoute,
    ) -> Result<PreparedCall, String> {
        self.validate_live_record()?;
        let resume_tool = self
            .record
            .identity
            .config
            .continuation
            .as_ref()
            .ok_or("missing admitted continuation config")?
            .resume_tool()
            .to_string();
        let process_owner = self.record.identity.owner_topology.as_ref().map(|topology| {
            let route = topology.routes.iter().find(|route| route.tool == tool)
                .ok_or("pending tool lost its bound process descriptor")?;
            crate::config::DeclaredToolOwner::from_descriptor(&route.process_descriptor)
        }).transpose()?;
        let identity = PendingIdentity {
            schema_version: 1,
            context: self.record.clone(),
            original_run: original,
            cell_id: cell_id.to_string(),
            runtime_tool_call_id: runtime_tool_call_id.to_string(),
            enqueue_ordinal,
            process_owner,
            tool: tool.to_string(),
            arguments,
            route,
            resume_tool,
        };
        let prepared = PreparedCall {
            digest: digest(&identity)?,
            identity,
        };
        prepared.validate()?;
        let bytes = serde_json::to_vec(&prepared).map_err(|e| e.to_string())?;
        if bytes.len() > self.record.identity.config.max_output_bytes {
            return Err("code mode pending call exceeds its configured output byte limit".into());
        }
        create_record(
            &self.root.join(format!("{}.pending.json", prepared.digest)),
            &prepared,
        )?;
        Ok(prepared)
    }

    pub(crate) fn record_emission(
        &self,
        pending: &PreparedCall,
        artifact: ArtifactRef,
        result: &Result<DeliveryVerdict, String>,
    ) -> Result<(), String> {
        let outcome = match result {
            Ok(DeliveryVerdict::Admitted { seq }) => EmissionOutcome::Admitted { seq: *seq },
            Ok(DeliveryVerdict::Refused { reason, detail }) => EmissionOutcome::Refused {
                reason: reason.as_str().to_string(),
                detail: detail.clone(),
            },
            Err(error) => EmissionOutcome::Indeterminate {
                error: error.clone(),
            },
        };
        create_record(
            &self.root.join(format!("{}.emission.json", pending.digest)),
            &EmissionRecord { artifact, outcome },
        )
    }

    pub(crate) fn accept(
        &self,
        envelope: &InterfaceInvocationEnvelope,
        formation: &WorkerEnvironmentFormation,
        source_movement_seq: u64,
    ) -> Result<AdmittedResult, String> {
        self.validate_live_record()?;
        let (pending, result) = read_pending_result(
            &self.root,
            envelope,
            ResultCarrier::FormedInvocation {
                source_movement_seq,
            },
        )?;
        if pending.identity.context != self.record || &self.record.identity.formation != formation {
            return Err("code mode continuation context or formed invocation changed".into());
        }
        validate_return(&self.root, &pending, envelope, &result)?;
        let emission: EmissionRecord =
            read_record(&self.root.join(format!("{}.emission.json", pending.digest)))?;
        let EmissionOutcome::Admitted { seq } = emission.outcome else {
            return Err(
                "code mode nested call has no admitted emission; refusing result settlement".into(),
            );
        };
        if seq <= pending.identity.original_run.source_movement_seq || source_movement_seq <= seq {
            return Err("code mode result does not follow its exact admitted emission".into());
        }
        create_record(
            &self.root.join(format!("{}.accepted.json", pending.digest)),
            &AcceptedResult {
                artifact: envelope.artifact.clone(),
                metadata_digest: digest(envelope.metadata().map_err(|e| e.to_string())?)?,
                emitted_seq: seq,
                result_seq: source_movement_seq,
            },
        )?;
        Ok(AdmittedResult { pending, result })
    }

    pub(crate) fn retire(&self) -> Result<(), String> {
        let path = self.root.join("retired.json");
        if path.try_exists().map_err(|e| e.to_string())? {
            let generation: String = read_record(&path)?;
            if generation != self.record.generation {
                return Err("code mode retirement generation changed".into());
            }
            return Ok(());
        }
        create_record(&path, &self.record.generation)
    }

    fn validate_live_record(&self) -> Result<(), String> {
        let record: ContextRecord = read_record(&self.root.join("context.json"))?;
        if record != self.record
            || self
                .root
                .join("retired.json")
                .try_exists()
                .map_err(|e| e.to_string())?
        {
            return Err("code mode context is retired or its immutable identity changed".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingIdentity {
    schema_version: u32,
    context: ContextRecord,
    original_run: OriginalRun,
    cell_id: String,
    runtime_tool_call_id: String,
    enqueue_ordinal: u64,
    process_owner: Option<crate::config::DeclaredToolOwner>,
    tool: String,
    arguments: Value,
    route: CodeModeRoute,
    resume_tool: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedCall {
    identity: PendingIdentity,
    digest: String,
}

impl PreparedCall {
    pub fn process_owner(&self) -> Option<&crate::config::DeclaredToolOwner> {
        self.identity.process_owner.as_ref()
    }

    pub fn enqueue_ordinal(&self) -> u64 {
        self.identity.enqueue_ordinal
    }

    fn validate(&self) -> Result<(), String> {
        let identity = &self.identity;
        let context = &identity.context.identity;
        let owner = context.owner_topology.as_ref().map(|topology| {
            let route = topology.routes.iter().find(|route| route.tool == identity.tool)
                .ok_or("immutable pending tool has no original process descriptor")?;
            crate::config::DeclaredToolOwner::from_descriptor(&route.process_descriptor)
        }).transpose()?;
        context.formation.validate().map_err(|e| e.to_string())?;
        context
            .config
            .validate(context.formation.door_interface.as_str())?;
        if identity.schema_version != 1
            || context.schema_version != 1
            || digest(identity)? != self.digest
            || digest(context)? != identity.context.generation
            || identity.tool != identity.route.tool
            || !context.config.routes.contains(&identity.route)
            || context
                .config
                .continuation
                .as_ref()
                .map(|c| c.resume_tool())
                != Some(identity.resume_tool.as_str())
            || !identity.arguments.is_object()
            || identity.cell_id.is_empty()
            || identity.runtime_tool_call_id.is_empty()
            || identity.enqueue_ordinal == 0
            || identity.process_owner != owner
            || identity.original_run.tool.is_empty()
            || identity.original_run.correlation_id.is_empty()
        {
            return Err("code mode immutable pending tuple is invalid".into());
        }
        Ok(())
    }

    pub fn correlation(&self) -> String {
        format!(
            "code-mode-tool:{}:{}",
            self.identity
                .context
                .identity
                .formation
                .worker_environment_id,
            self.digest
        )
    }
    pub(crate) fn cell_id(&self) -> &str {
        &self.identity.cell_id
    }
    pub(crate) fn runtime_tool_call_id(&self) -> &str {
        &self.identity.runtime_tool_call_id
    }
    pub fn tool_call(&self) -> ToolCallArtifact {
        ToolCallArtifact {
            tool: self.identity.tool.clone(),
            arguments: self.identity.arguments.clone(),
            correlation_id: Some(self.correlation()),
            label: Some("tool.call".into()),
            source_interface_evidence: BTreeMap::new(),
        }
    }
    pub(crate) fn resume_call(
        &self,
        result: &ToolResultArtifact,
    ) -> Result<ToolCallArtifact, String> {
        Ok(ToolCallArtifact {
            tool: self.identity.resume_tool.clone(),
            arguments: json!({"cell_id":self.identity.cell_id,"tool_call_id":self.identity.runtime_tool_call_id,"result":serde_json::to_value(result).map_err(|e| e.to_string())?}),
            correlation_id: Some(format!(
                "code-mode-resume:{}:{}",
                self.identity
                    .context
                    .identity
                    .formation
                    .worker_environment_id,
                self.digest
            )),
            label: Some("tool.call".into()),
            source_interface_evidence: BTreeMap::new(),
        })
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case", deny_unknown_fields)]
enum EmissionOutcome {
    Admitted { seq: u64 },
    Refused { reason: String, detail: Value },
    Indeterminate { error: String },
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EmissionRecord {
    artifact: ArtifactRef,
    outcome: EmissionOutcome,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AcceptedResult {
    artifact: ArtifactRef,
    metadata_digest: String,
    emitted_seq: u64,
    result_seq: u64,
}

pub(crate) struct AdmittedResult {
    pub pending: PreparedCall,
    pub result: ToolResultArtifact,
}

pub(crate) fn result_locator(
    envelope: &InterfaceInvocationEnvelope,
) -> Result<Option<WorkerEnvironmentId>, String> {
    if envelope
        .read_artifact_manifest()
        .map_err(|e| e.to_string())?
        .label
        == "tool.call"
    {
        return Ok(None);
    }
    let bytes = envelope
        .read_artifact_payload_bounded(RECORD_BYTES as u64)
        .map_err(|e| e.to_string())?;
    let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
        return Ok(None);
    };
    if let Some((worker, _)) = application_locator(envelope)? {
        return Ok(Some(worker));
    }
    let Some(id) = value.get("correlation_id").and_then(Value::as_str) else {
        return Ok(None);
    };
    if !id.starts_with("code-mode-tool:") {
        return Ok(None);
    }
    let (worker, _) = parse_correlation(id)?;
    Ok(Some(worker))
}

pub fn historical_continuation_call(
    ctx: &octopus_tool_door_runtime::InterfaceLiftContext,
    envelope: &InterfaceInvocationEnvelope,
    source: &Movement,
) -> Result<(ToolCallArtifact, PathBuf), String> {
    if source.kind != octopus_ledger::MovementKind::InterfaceOutbound
        || source.to != Endpoint::Interface(ctx.owner_interface().clone())
        || source.artifact.as_ref() != Some(&envelope.artifact)
        || source.invocation_metadata.as_ref().and_then(|metadata| metadata.invocation())
            != Some(envelope.metadata().map_err(|e| e.to_string())?) {
        return Err("historical continuation differs from its original admitted movement".into());
    }
    let purpose = ToolDoorArtifactDecodePurpose::HistoricalIdentity;
    if envelope.read_artifact_manifest().map_err(|e| e.to_string())?.label == "code_mode.frontier_ready" {
        let ready = read_ready(envelope)?;
        let root = octopus_tool_door_runtime::tool_door_durable_state_dir(ctx, &ready.worker_environment_id)?;
        let call = decode_ready(&root, envelope, source, purpose, None)?;
        return Ok((call, root));
    }
    let worker = result_locator(envelope)?.ok_or("native failure source is not a declared continuation")?;
    let root = octopus_tool_door_runtime::tool_door_durable_state_dir(ctx, &worker)?;
    let call = decode_result(&root, ctx.owner_interface(), envelope, purpose, None, source)?;
    Ok((call, root))
}

pub(crate) fn decode_result(
    root: &Path,
    door: &InterfaceId,
    envelope: &InterfaceInvocationEnvelope,
    purpose: ToolDoorArtifactDecodePurpose,
    current_config: Option<&CodeModeConfig>,
    source: &Movement,
) -> Result<ToolCallArtifact, String> {
    if application_locator(envelope)?.is_some() {
        return decode_application(root, envelope, source, purpose, current_config);
    }
    let (pending, result) = read_pending_result(root, envelope, ResultCarrier::OriginalAdmitted)?;
    if pending.identity.context.identity.formation.door_interface != *door {
        return Err("code mode result belongs to another door".into());
    }
    validate_return(root, &pending, envelope, &result)?;
    let Endpoint::Interface(source_interface) = &source.from;
    if source_interface.as_str() != pending.identity.route.interface {
        return Err("code mode result was emitted by another interface".into());
    }
    if purpose == ToolDoorArtifactDecodePurpose::Dispatch {
        let config = current_config.ok_or("code mode Dispatch has no current config")?;
        config.validate(door.as_str())?;
        if *config != pending.identity.context.identity.config {
            return Err("code mode continuation config changed".into());
        }
        let context: ContextRecord = read_record(&root.join("context.json"))?;
        if context != pending.identity.context
            || root
                .join("retired.json")
                .try_exists()
                .map_err(|e| e.to_string())?
        {
            return Err("code mode context identity is retired or changed".into());
        }
        if root
            .join(format!("{}.accepted.json", pending.digest))
            .try_exists()
            .map_err(|e| e.to_string())?
        {
            return Err(
                "code mode result was already accepted; duplicate settlement refused".into(),
            );
        }
        let emission_path = root.join(format!("{}.emission.json", pending.digest));
        if emission_path.try_exists().map_err(|e| e.to_string())? {
            let emission: EmissionRecord = read_record(&emission_path)?;
            if !matches!(emission.outcome, EmissionOutcome::Admitted { .. }) {
                return Err("code mode result follows a refused or indeterminate emission".into());
            }
        }
    }
    if purpose == ToolDoorArtifactDecodePurpose::Dispatch {
        let witness = result_witness(envelope, source_interface.clone(), source.seq.0)?;
        let path = root.join(format!("{}.admitted.json", pending.digest));
        if path.try_exists().map_err(|e| e.to_string())? {
            let original: AdmittedCarrier = read_record(&path)?;
            if original != witness {
                return Err(
                    "code mode result conflicts with its already admitted original carrier".into(),
                );
            }
        } else {
            create_record(&path, &witness)?;
        }
    }
    pending.resume_call(&result)
}

pub(crate) fn decode_formed_result(
    root: &Path,
    door: &InterfaceId,
    envelope: &InterfaceInvocationEnvelope,
    config: &CodeModeConfig,
    source_movement_seq: u64,
) -> Result<ToolCallArtifact, String> {
    if application_locator(envelope)?.is_some() {
        return formed_application_call(root, envelope, config, source_movement_seq);
    }
    let (pending, result) = read_pending_result(
        root,
        envelope,
        ResultCarrier::FormedInvocation {
            source_movement_seq,
        },
    )?;
    if pending.identity.context.identity.formation.door_interface != *door
        || pending.identity.context.identity.config != *config
    {
        return Err("code mode formed result changed its owner or config".into());
    }
    validate_return(root, &pending, envelope, &result)?;
    if root
        .join(format!("{}.accepted.json", pending.digest))
        .try_exists()
        .map_err(|e| e.to_string())?
    {
        return Err("code mode result was already accepted".into());
    }
    pending.resume_call(&result)
}

#[derive(PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AdmittedCarrier {
    source_interface: InterfaceId,
    source_movement_seq: u64,
    artifact: ArtifactRef,
    payload_hash: String,
    metadata_hash: String,
}

enum ResultCarrier {
    OriginalAdmitted,
    FormedInvocation { source_movement_seq: u64 },
}

fn result_witness(
    envelope: &InterfaceInvocationEnvelope,
    source_interface: InterfaceId,
    source_movement_seq: u64,
) -> Result<AdmittedCarrier, String> {
    Ok(AdmittedCarrier {
        source_interface,
        source_movement_seq,
        artifact: envelope.artifact.clone(),
        payload_hash: DescriptorHash::of_bytes(
            &envelope
                .read_artifact_payload_bounded(RECORD_BYTES as u64)
                .map_err(|e| e.to_string())?,
        )
        .to_hex(),
        metadata_hash: digest(envelope.metadata().map_err(|e| e.to_string())?)?,
    })
}

fn parse_correlation(id: &str) -> Result<(WorkerEnvironmentId, &str), String> {
    let mut parts = id.split(':');
    if parts.next() != Some("code-mode-tool") {
        return Err("unknown code mode correlation".into());
    }
    let worker = WorkerEnvironmentId::parse(
        parts
            .next()
            .ok_or("code mode correlation has no worker environment")?,
    )
    .map_err(|e| e.to_string())?;
    let hash = parts
        .next()
        .ok_or("code mode correlation has no pending digest")?;
    if DescriptorHash::from_hex(hash).is_none() || parts.next().is_some() {
        return Err("malformed code mode pending digest".into());
    }
    Ok((worker, hash))
}

fn read_pending_result(
    root: &Path,
    envelope: &InterfaceInvocationEnvelope,
    carrier: ResultCarrier,
) -> Result<(PreparedCall, ToolResultArtifact), String> {
    validate_directory(root)?;
    let captured = envelope
        .read_artifact_bounded(MANIFEST_BYTES, RECORD_BYTES as u64)
        .map_err(|e| e.to_string())?;
    if matches!(carrier, ResultCarrier::OriginalAdmitted) {
        validate_carrier(envelope, &captured.manifest_bytes, &captured.manifest.label)?;
    }
    let result: ToolResultArtifact = serde_json::from_slice(&captured.payload_bytes)
        .map_err(|e| format!("code mode incoming ToolResult is malformed: {e}"))?;
    let (worker, hash) = parse_correlation(
        result
            .correlation_id
            .as_deref()
            .ok_or("code mode result has no correlation")?,
    )?;
    let pending: PreparedCall = read_record(&root.join(format!("{hash}.pending.json")))?;
    pending.validate()?;
    if let ResultCarrier::FormedInvocation {
        source_movement_seq,
    } = carrier
    {
        let original: AdmittedCarrier = read_record(&root.join(format!("{hash}.admitted.json")))?;
        if original.source_interface.as_str() != pending.identity.route.interface
            || original
                != result_witness(
                    envelope,
                    original.source_interface.clone(),
                    source_movement_seq,
                )?
        {
            return Err("code mode formed result is not the original owner-admitted artifact body and metadata".into());
        }
    }
    if pending.digest != hash
        || pending
            .identity
            .context
            .identity
            .formation
            .worker_environment_id
            != worker
        || result.tool != pending.identity.tool
        || result.label != captured.manifest.label
        || result.correlation_id.as_deref() != Some(pending.correlation().as_str())
        || captured.payload_bytes.len() > pending.identity.context.identity.config.max_output_bytes
    {
        return Err(
            "code mode result does not match its immutable pending call or byte limit".into(),
        );
    }
    Ok((pending, result))
}

fn validate_return(
    root: &Path,
    pending: &PreparedCall,
    envelope: &InterfaceInvocationEnvelope,
    result: &ToolResultArtifact,
) -> Result<(), String> {
    let metadata = envelope.metadata().map_err(|e| e.to_string())?;
    let runtime = envelope.runtime().map_err(|e| e.to_string())?;
    let formation = &pending.identity.context.identity.formation;
    let bindings = metadata
        .checkpoint_binding_policy
        .as_ref()
        .ok_or("code mode result has no admitted binding snapshot")?;
    let admission = metadata
        .checkpoint_admission
        .as_ref()
        .ok_or("code mode result has no checkpoint admission")?;
    if runtime.run.as_str() != formation.run
        || runtime.ee.as_str() != formation.ee
        || runtime.worker != formation.worker
        || runtime.interface != formation.door_interface
        || Some(runtime.checkpoint.as_str()) != pending.identity.route.result_checkpoint.as_deref()
        || !return_pin_matches(root, pending, envelope)?
        || bindings.inbound.interface.as_str() != pending.identity.route.interface
        || metadata.outbound_interface_bindings != [formation.door_interface.clone()]
        || bindings
            .outbound
            .iter()
            .map(|binding| &binding.interface)
            .collect::<Vec<_>>()
            != [&formation.door_interface]
        || !admission.complete
        || admission.artifact_label != result.label
        || metadata.verification_failure.is_some()
        || !metadata.required_verified_artifacts.is_empty()
        || !admission.required_artifact_labels.is_empty()
        || [&pending.identity.tool, &pending.identity.resume_tool]
            .into_iter()
            .any(|tool| {
                !metadata.grant.tools.contains(tool)
                    || metadata.grant.hard_blocked_tools.contains(tool)
            })
        || [
            &pending.identity.route.interface,
            &formation.door_interface.to_string(),
        ]
        .into_iter()
        .any(|interface| !metadata.grant.interfaces.contains(interface))
    {
        return Err(
            "code mode result has a foreign, incomplete or changed admitted return boundary".into(),
        );
    }
    let config = &pending.identity.context.identity.config;
    let resolved = config.resolve_context(envelope)?;
    let ResolvedContext::Persistent {
        artifact, payload, ..
    } = resolved
    else {
        return Err("code mode result lost its bound prelude".into());
    };
    let context = &pending.identity.context.identity;
    if artifact != context.prelude_artifact
        || DescriptorHash::of_bytes(&payload).to_hex() != context.prelude_payload_hash
    {
        return Err("code mode result changed its exact prelude payload".into());
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReturnBoundary {
    pub checkpoint: String,
    pub epoch: u64,
    pub run_record_hash: String,
    pub binding: octopus_interfaces::CheckpointBindingPolicySnapshot,
}

impl ReturnBoundary {
    fn survives_checked_transition(&self, before: &[u8], after: &[u8]) -> Result<bool, String> {
        if self.run_record_hash != ContentHash::of_bytes(before).to_hex() {
            return Err("retained boundary differs from its exact pre-transition Run hash".into());
        }
        let before: Value = serde_json::from_slice(before).map_err(|e| e.to_string())?;
        let after: Value = serde_json::from_slice(after).map_err(|e| e.to_string())?;
        let checkpoints = before.get("checkpoints").and_then(Value::as_array)
            .ok_or("checked boundary source has no checkpoints")?;
        let Some(checkpoint) = checkpoints.iter().find(|row| row.get("id") == Some(&json!(self.checkpoint))) else {
            return Ok(false);
        };
        let binding: octopus_interfaces::CheckpointBindingPolicySnapshot = serde_json::from_value(json!({
            "inbound": checkpoint.get("interface_binding").ok_or("checked boundary lost its inbound binding")?,
            "outbound": checkpoint.get("outbound_interface_bindings").ok_or("checked boundary lost its outbound bindings")?,
        })).map_err(|e| e.to_string())?;
        if checkpoint.get("epoch").and_then(Value::as_u64) != Some(self.epoch) || binding != self.binding {
            return Err("retained boundary differs from its exact checkpoint epoch or binding".into());
        }
        let active = |record: &Value| -> Result<bool, String> {
            Ok(record.get("active_checkpoints").and_then(Value::as_array)
                .ok_or("checked boundary Run has no active checkpoints")?.contains(&json!(self.checkpoint)))
        };
        Ok(active(&before)? && active(&after)? && after.get("checkpoints").and_then(Value::as_array)
            .ok_or("checked boundary target has no checkpoints")?.iter()
            .find(|row| row.get("id") == Some(&json!(self.checkpoint))) == Some(checkpoint))
    }

    pub fn matches(&self, envelope: &InterfaceInvocationEnvelope) -> Result<bool, String> {
        let runtime = envelope.runtime().map_err(|e| e.to_string())?;
        Ok(runtime.checkpoint.as_str() == self.checkpoint
            && runtime.epoch == self.epoch
            && runtime.run_record_hash.as_deref() == Some(self.run_record_hash.as_str())
            && envelope
                .metadata()
                .map_err(|e| e.to_string())?
                .checkpoint_binding_policy
                .as_ref()
                == Some(&self.binding))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstalledHandoff {
    pub before_hash: String,
    pub after_hash: String,
    pub source_seq: u64,
    pub source_artifact: ArtifactRef,
    pub result_seq: u64,
    pub result_artifact: ArtifactRef,
}

pub fn record_install(root: &Path, installed: &InstalledHandoff) -> Result<(), String> {
    if installed.source_seq >= installed.result_seq
        || ContentHash::from_hex(&installed.after_hash).is_none()
        || ContentHash::from_hex(&installed.before_hash).is_none()
    {
        return Err("code mode installed handoff has invalid evidence".into());
    }
    publish_record(
        &root.join(format!("{}.installed.json", installed.after_hash)),
        installed,
        true,
    )
}

pub fn prepare_return(
    root: &Path,
    pending: &PreparedCall,
    boundary: &ReturnBoundary,
) -> Result<(), String> {
    pending.validate()?;
    if Some(boundary.checkpoint.as_str()) != pending.identity.route.result_checkpoint.as_deref()
        || boundary.binding.inbound.interface.as_str() != pending.identity.route.interface
        || boundary
            .binding
            .outbound
            .iter()
            .map(|b| &b.interface)
            .collect::<Vec<_>>()
            != [&pending.identity.context.identity.formation.door_interface]
        || ContentHash::from_hex(&boundary.run_record_hash).is_none()
    {
        return Err("code mode prospective return differs from immutable route".into());
    }
    prepare_record(
        &root.join(format!(
            "{}.{}.return.json",
            pending.digest, boundary.run_record_hash
        )),
        boundary,
    )
}

fn return_pin_matches(
    root: &Path,
    pending: &PreparedCall,
    envelope: &InterfaceInvocationEnvelope,
) -> Result<bool, String> {
    let hash = envelope
        .runtime()
        .map_err(|e| e.to_string())?
        .run_record_hash
        .as_deref()
        .ok_or("code mode return has no run snapshot")?;
    if pending.identity.context.identity.config.handoff.is_none() {
        return Ok(hash == pending.identity.original_run.run_record_hash);
    }
    let boundary: ReturnBoundary =
        read_record(&root.join(format!("{}.{}.return.json", pending.digest, hash)))?;
    let installed: InstalledHandoff = read_record(&root.join(format!("{hash}.installed.json")))?;
    Ok(installed.after_hash == hash && boundary.matches(envelope)?)
}

pub fn prepared_call_for_output(
    root: &Path,
    call: &ToolCallArtifact,
    formation: &WorkerEnvironmentFormation,
) -> Result<PreparedCall, String> {
    let (worker, key) = parse_correlation(
        call.correlation_id
            .as_deref()
            .ok_or("owner output has no immutable pending correlation")?,
    )?;
    let pending: PreparedCall = read_record(&root.join(format!("{key}.pending.json")))?;
    pending.validate()?;
    if pending.digest != key
        || pending.formation() != formation
        || pending.formation().worker_environment_id != worker
        || !pending.matches_call(call)
    {
        return Err("owner output differs from its exact immutable pending call".into());
    }
    let accepted = root.join(format!("{key}.accepted.json"));
    match std::fs::symlink_metadata(&accepted) {
        Ok(_) => {
            let _: AcceptedResult = read_record(&accepted)?;
            return Err("owner output was already accepted; re-emission refused".into());
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("owner output completion record: {error}")),
    }
    Ok(pending)
}

pub fn prepared_calls(root: &Path) -> Result<Vec<PreparedCall>, String> {
    validate_directory(root)?;
    let mut pending = Vec::new();
    for entry in std::fs::read_dir(root).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let Some(key) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.strip_suffix(".pending.json"))
            .map(str::to_string)
        else {
            continue;
        };
        if root
            .join(format!("{key}.accepted.json"))
            .try_exists()
            .map_err(|e| e.to_string())?
        {
            continue;
        }
        let call: PreparedCall = read_record(&entry.path())?;
        call.validate()?;
        if call.digest != key {
            return Err("code mode pending filename differs from its immutable digest".into());
        }
        pending.push(call);
    }
    pending.sort_by(|a, b| a.digest.cmp(&b.digest));
    Ok(pending)
}

impl PreparedCall {
    pub fn pending_digest(&self) -> &str {
        &self.digest
    }
    pub fn original(&self) -> &OriginalRun {
        &self.identity.original_run
    }
    pub fn route(&self) -> &CodeModeRoute {
        &self.identity.route
    }
    pub fn formation(&self) -> &WorkerEnvironmentFormation {
        &self.identity.context.identity.formation
    }
    pub fn matches_call(&self, call: &ToolCallArtifact) -> bool {
        call.tool == self.identity.tool
            && call.arguments == self.identity.arguments
            && call.correlation_id.as_deref() == Some(self.correlation().as_str())
            && call.source_interface_evidence.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrontierReady {
    pub schema_version: u32,
    pub worker_environment_id: WorkerEnvironmentId,
    pub context_generation: String,
    pub cell_id: String,
    pub event_id: u64,
    pub original: OriginalRun,
    pub event: Value,
    pub event_digest: String,
}

impl FrontierReady {
    pub fn identity(&self) -> Result<String, String> {
        digest(self)
    }
    pub fn file_name(&self) -> Result<String, String> {
        Ok(format!("{}.ready.json", self.identity()?))
    }
}

impl ContinuationContext {
    pub(crate) fn publish_ready(
        &self,
        cell: &str,
        original: OriginalRun,
        event_id: u64,
        event: Value,
    ) -> Result<Option<String>, String> {
        if self.record.identity.config.handoff.is_none() {
            return Ok(None);
        }
        self.validate_live_record()?;
        let ready = FrontierReady {
            schema_version: 1,
            worker_environment_id: self.record.identity.formation.worker_environment_id.clone(),
            context_generation: self.generation().to_string(),
            cell_id: cell.to_string(),
            event_id,
            original,
            event_digest: digest(&event)?,
            event,
        };
        let key = ready.identity()?;
        create_record(&self.root.join(ready.file_name()?), &ready)?;
        Ok(Some(key))
    }
    pub(crate) fn observe_ready(&self, key: &str) -> Result<(), String> {
        if DescriptorHash::from_hex(key).is_none() {
            return Err("malformed readiness identity".into());
        }
        let path = self.root.join(format!("{key}.observed.json"));
        if !path.try_exists().map_err(|e| e.to_string())? {
            create_record(&path, &self.record.generation)?;
        }
        Ok(())
    }
    pub(crate) fn accept_ready(
        &self,
        envelope: &InterfaceInvocationEnvelope,
        source_seq: u64,
    ) -> Result<FrontierReady, String> {
        self.validate_live_record()?;
        let ready = read_ready(envelope)?;
        let key = ready.identity()?;
        let admitted: AdmittedCarrier =
            read_record(&self.root.join(format!("{key}.wake-admitted.json")))?;
        if admitted
            != result_witness(
                envelope,
                self.record.identity.formation.door_interface.clone(),
                source_seq,
            )?
        {
            return Err("code mode readiness is not the original admitted carrier".into());
        }
        validate_ready(&self.root, &ready, envelope)?;
        create_record(
            &self.root.join(format!("{key}.wake-accepted.json")),
            &admitted,
        )?;
        Ok(ready)
    }
}

pub fn read_ready(envelope: &InterfaceInvocationEnvelope) -> Result<FrontierReady, String> {
    let captured = envelope
        .read_artifact_bounded(MANIFEST_BYTES, RECORD_BYTES as u64)
        .map_err(|e| e.to_string())?;
    if captured.manifest.label != "code_mode.frontier_ready" {
        return Err("not a code mode readiness artifact".into());
    }
    serde_json::from_slice(&captured.payload_bytes).map_err(|e| e.to_string())
}

pub fn ready_records(root: &Path) -> Result<Vec<FrontierReady>, String> {
    validate_directory(root)?;
    let mut records = Vec::new();
    for entry in std::fs::read_dir(root).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        if entry
            .file_name()
            .to_str()
            .is_some_and(|s| s.ends_with(".ready.json"))
        {
            let ready: FrontierReady = read_record(&entry.path())?;
            let key = ready.identity()?;
            if entry.file_name().to_str() != Some(ready.file_name()?.as_str()) {
                return Err("readiness filename differs from immutable identity".into());
            }
            if !root
                .join(format!("{key}.observed.json"))
                .try_exists()
                .map_err(|e| e.to_string())?
                && !root
                    .join(format!("{key}.wake-sent.json"))
                    .try_exists()
                    .map_err(|e| e.to_string())?
            {
                records.push(ready);
            }
        }
    }
    records.sort_by_key(|r| r.event_id);
    Ok(records)
}

fn validate_ready(
    root: &Path,
    ready: &FrontierReady,
    envelope: &InterfaceInvocationEnvelope,
) -> Result<ContextRecord, String> {
    let context: ContextRecord = read_record(&root.join("context.json"))?;
    let stored: FrontierReady = read_record(&root.join(ready.file_name()?))?;
    let metadata = envelope.metadata().map_err(|e| e.to_string())?;
    let runtime = envelope.runtime().map_err(|e| e.to_string())?;
    let handoff = context
        .identity
        .config
        .handoff
        .as_ref()
        .ok_or("readiness requires declared handoff")?;
    if stored != *ready
        || ready.schema_version != 1
        || ready.event_id == 0
        || ready.cell_id.is_empty()
        || ready.event_digest != digest(&ready.event)?
        || ready.context_generation != context.generation
        || ready.worker_environment_id != context.identity.formation.worker_environment_id
        || runtime.interface != context.identity.formation.door_interface
        || runtime.run.as_str() != context.identity.formation.run
        || runtime.ee.as_str() != context.identity.formation.ee
        || runtime.worker != context.identity.formation.worker
        || !metadata.grant.tools.contains(&handoff.wait_tool)
        || metadata
            .grant
            .hard_blocked_tools
            .contains(&handoff.wait_tool)
        || metadata.verification_failure.is_some()
        || !metadata.required_verified_artifacts.is_empty()
        || !metadata
            .checkpoint_admission
            .as_ref()
            .is_some_and(|a| a.complete && a.artifact_label == "code_mode.frontier_ready")
    {
        return Err(
            "code mode readiness differs from original context or admitted boundary".into(),
        );
    }
    let hash = runtime
        .run_record_hash
        .as_deref()
        .ok_or("readiness has no admitted run snapshot")?;
    let boundary: ReturnBoundary = read_record(&root.join(format!("{hash}.wake-pin.json")))?;
    let installed: InstalledHandoff = read_record(&root.join(format!("{hash}.installed.json")))?;
    if installed.after_hash != hash || !boundary.matches(envelope)? {
        return Err("readiness differs from installed prospective snapshot".into());
    }
    context.identity.config.resolve_context(envelope)?;
    Ok(context)
}

pub(crate) fn decode_ready(
    root: &Path,
    envelope: &InterfaceInvocationEnvelope,
    source: &Movement,
    purpose: ToolDoorArtifactDecodePurpose,
    current: Option<&CodeModeConfig>,
) -> Result<ToolCallArtifact, String> {
    let captured = envelope
        .read_artifact_bounded(MANIFEST_BYTES, RECORD_BYTES as u64)
        .map_err(|e| e.to_string())?;
    validate_carrier(envelope, &captured.manifest_bytes, &captured.manifest.label)?;
    let ready = read_ready(envelope)?;
    let context = validate_ready(root, &ready, envelope)?;
    if source.from != Endpoint::Interface(context.identity.formation.door_interface.clone()) {
        return Err("readiness came from another interface".into());
    }
    let key = ready.identity()?;
    if purpose == ToolDoorArtifactDecodePurpose::Dispatch {
        if current != Some(&context.identity.config)
            || root
                .join("retired.json")
                .try_exists()
                .map_err(|e| e.to_string())?
            || root
                .join(format!("{key}.wake-accepted.json"))
                .try_exists()
                .map_err(|e| e.to_string())?
        {
            return Err("readiness context changed, retired or already consumed".into());
        }
        let witness = result_witness(
            envelope,
            context.identity.formation.door_interface.clone(),
            source.seq.0,
        )?;
        let path = root.join(format!("{key}.wake-admitted.json"));
        if path.try_exists().map_err(|e| e.to_string())? {
            let prior: AdmittedCarrier = read_record(&path)?;
            if prior != witness {
                return Err("conflicting duplicate readiness admission".into());
            }
        } else {
            create_record(&path, &witness)?;
        }
    }
    Ok(ToolCallArtifact {
        tool: context
            .identity
            .config
            .handoff
            .as_ref()
            .ok_or("missing wake role")?
            .wait_tool
            .clone(),
        arguments: json!({"cell_id":ready.cell_id}),
        correlation_id: Some(format!(
            "code-mode-ready:{}:{key}",
            ready.worker_environment_id
        )),
        label: Some("tool.call".into()),
        source_interface_evidence: BTreeMap::new(),
    })
}

pub fn artifact_ref(
    file: &octopus_interfaces::InterfaceArtifactFile,
) -> Result<ArtifactRef, String> {
    let captured = file
        .read_manifest_bounded(MANIFEST_BYTES)
        .map_err(|e| e.to_string())?;
    let content = ContentHash::of_bytes(&captured.manifest_bytes);
    Ok(ArtifactRef {
        id: ArtifactId::new(format!("{}:{}", captured.manifest.label, content.to_hex())),
        content,
    })
}

fn validate_carrier(
    envelope: &InterfaceInvocationEnvelope,
    manifest: &[u8],
    label: &str,
) -> Result<(), String> {
    let hash = ContentHash::of_bytes(manifest);
    if envelope.artifact.content != hash
        || envelope.artifact.id.as_str() != format!("{label}:{}", hash.to_hex())
    {
        return Err(
            "code mode original artifact manifest does not match its immutable reference".into(),
        );
    }
    Ok(())
}

pub fn digest(value: &impl Serialize) -> Result<String, String> {
    Ok(
        DescriptorHash::of_canonical_json(&serde_json::to_value(value).map_err(|e| e.to_string())?)
            .to_hex(),
    )
}

pub fn validate_directory(path: &Path) -> Result<(), String> {
    let metadata = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !path.is_absolute()
        || !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || std::fs::canonicalize(path).map_err(|e| e.to_string())? != path
    {
        return Err("code mode private directory is not a canonical real directory".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o077 != 0 {
            return Err("code mode private directory is not owner-only".into());
        }
    }
    Ok(())
}

pub fn read_record<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, String> {
    serde_json::from_slice(&read_record_bytes(path)?)
        .map_err(|e| format!("code mode immutable record is malformed: {e}"))
}

fn read_record_bytes(path: &Path) -> Result<Vec<u8>, String> {
    validate_directory(
        path.parent()
            .ok_or("code mode record has no owner directory")?,
    )?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options
        .open(path)
        .map_err(|e| format!("code mode record {}: {e}", path.display()))?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.len() > RECORD_BYTES as u64 {
        return Err("code mode record is not a bounded regular file".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o777 != 0o600
            || metadata.nlink() != 1
        {
            return Err("code mode private record ownership changed".into());
        }
    }
    let mut bytes = Vec::new();
    std::io::Read::by_ref(&mut file)
        .take(RECORD_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > RECORD_BYTES {
        return Err("code mode private record exceeded byte limit".into());
    }
    Ok(bytes)
}

pub fn prepare_record(path: &Path, value: &impl Serialize) -> Result<(), String> {
    publish_record(path, value, true)
}

pub fn create_record(path: &Path, value: &impl Serialize) -> Result<(), String> {
    publish_record(path, value, false)
}

fn publish_record(path: &Path, value: &impl Serialize, exact_existing: bool) -> Result<(), String> {
    let parent = path.parent().ok_or("code mode record has no parent")?;
    validate_directory(parent)?;
    let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    if bytes.len() > RECORD_BYTES {
        return Err("code mode record exceeds byte limit".into());
    }
    let temporary = parent.join(format!(
        ".record-{}-{}",
        std::process::id(),
        NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
    ));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options.open(&temporary).map_err(|e| e.to_string())?;
    let outcome: Result<bool, String> = (|| {
        file.write_all(&bytes).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        if exact_existing {
            #[cfg(test)]
            INSTALL_PUBLICATION_BARRIER.with(|slot| {
                if let Some(barrier) = slot.borrow_mut().take() { barrier.wait(); }
            });
            return match rename_record_exclusive(&temporary, path) {
                Ok(()) => Ok(true),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
                Err(error) => Err(format!("code mode immutable record {} could not be published: {error}", path.display())),
            };
        }
        std::fs::hard_link(&temporary, path).map_err(|e| {
            format!(
                "code mode immutable record {} could not be published: {e}",
                path.display()
            )
        })?;
        Ok(false)
    })();
    let removal = if matches!(&outcome, Ok(true)) { Ok(()) } else {
        std::fs::remove_file(&temporary).map_err(|e| e.to_string())
    };
    let moved = outcome?;
    removal?;
    if exact_existing && !moved && read_record_bytes(path)? != bytes {
        return Err(format!("code mode prospective record {} conflicts with its immutable bytes", path.display()));
    }
    File::open(parent)
        .and_then(|file| file.sync_all())
        .map_err(|e| e.to_string())
}

fn rename_record_exclusive(source: &Path, target: &Path) -> std::io::Result<()> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        use std::os::unix::ffi::OsStrExt;
        let source = std::ffi::CString::new(source.as_os_str().as_bytes())?;
        let target = std::ffi::CString::new(target.as_os_str().as_bytes())?;
        #[cfg(target_os = "macos")]
        let status = unsafe { libc::renamex_np(source.as_ptr(), target.as_ptr(), libc::RENAME_EXCL) };
        #[cfg(target_os = "linux")]
        let status = unsafe { libc::syscall(libc::SYS_renameat2, libc::AT_FDCWD, source.as_ptr(), libc::AT_FDCWD, target.as_ptr(), libc::RENAME_NOREPLACE) };
        if status == 0 { Ok(()) } else { Err(std::io::Error::last_os_error()) }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (source, target);
        Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "atomic exclusive installation publication is unsupported on this platform"))
    }
}

impl ContinuationContext {
    pub(crate) fn ready_observed(&self, key: &str) -> Result<bool, String> {
        if DescriptorHash::from_hex(key).is_none() {
            return Err("malformed readiness digest".into());
        }
        let path = self.root.join(format!("{key}.observed.json"));
        if !path.try_exists().map_err(|e| e.to_string())? {
            return Ok(false);
        }
        let generation: String = read_record(&path)?;
        if generation != self.record.generation {
            return Err("observed readiness belongs to another generation".into());
        }
        Ok(true)
    }
}

pub(crate) fn formed_ready_call(
    root: &Path,
    envelope: &InterfaceInvocationEnvelope,
    config: &CodeModeConfig,
    source_seq: u64,
) -> Result<ToolCallArtifact, String> {
    let ready = read_ready(envelope)?;
    let context = validate_ready(root, &ready, envelope)?;
    let witness: AdmittedCarrier =
        read_record(&root.join(format!("{}.wake-admitted.json", ready.identity()?)))?;
    if config != &context.identity.config
        || witness
            != result_witness(
                envelope,
                context.identity.formation.door_interface.clone(),
                source_seq,
            )?
    {
        return Err("formed readiness is not its exact owner-admitted source".into());
    }
    Ok(ToolCallArtifact {
        tool: config
            .handoff
            .as_ref()
            .ok_or("missing wake role")?
            .wait_tool
            .clone(),
        arguments: json!({"cell_id":ready.cell_id}),
        correlation_id: Some(format!(
            "code-mode-ready:{}:{}",
            ready.worker_environment_id,
            ready.identity()?
        )),
        label: Some("tool.call".into()),
        source_interface_evidence: BTreeMap::new(),
    })
}

pub(crate) fn is_internal_receipt(envelope: &InterfaceInvocationEnvelope) -> Result<bool, String> {
    let result: Value = serde_json::from_slice(
        &envelope
            .read_artifact_payload_bounded(RECORD_BYTES as u64)
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let correlation = result
        .get("correlation_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    Ok([
        "code-mode-resume:",
        "code-mode-ready:",
        "code-mode-decision:",
        "code-mode-app-result:",
        "code-mode-artifact:",
    ]
    .iter()
    .any(|prefix| correlation.starts_with(prefix)))
}

pub fn record_internal_receipt(
    root: &Path,
    artifact: &ArtifactRef,
    source_seq: u64,
) -> Result<(), String> {
    prepare_record(
        &root.join(format!("{}.receipt.json", artifact.content.to_hex())),
        &json!({"artifact":artifact,"source_seq":source_seq}),
    )
}

pub(crate) fn validate_internal_receipt(
    ctx: &octopus_tool_door_runtime::InterfaceLiftContext,
    envelope: &InterfaceInvocationEnvelope,
) -> Result<(), String> {
    let source = ctx.source_movement(envelope)?;
    if source.from != Endpoint::Interface(ctx.owner_interface().clone()) {
        return Err("internal receipt came from another owner".into());
    }
    let payload: Value = serde_json::from_slice(
        &envelope
            .read_artifact_payload_bounded(RECORD_BYTES as u64)
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let correlation = payload
        .get("correlation_id")
        .and_then(Value::as_str)
        .ok_or("receipt has no correlation")?;
    let prefix = correlation
        .split(':')
        .next()
        .ok_or("receipt has no owner vocabulary")?;
    let (worker, _) = parse_owner_correlation(correlation, prefix)?;
    let root = octopus_tool_door_runtime::tool_door_durable_state_dir(ctx, &worker)?;
    let recorded: Value = read_record(&root.join(format!(
        "{}.receipt.json",
        envelope.artifact.content.to_hex()
    )))?;
    if recorded.get("artifact")
        != Some(&serde_json::to_value(&envelope.artifact).map_err(|e| e.to_string())?)
        || !recorded
            .get("source_seq")
            .and_then(Value::as_u64)
            .is_some_and(|seq| seq < source.seq.0)
    {
        return Err("internal receipt has no exact prior owner publication".into());
    }
    Ok(())
}

pub fn context_generation(root: &Path) -> Result<String, String> {
    let context: ContextRecord = read_record(&root.join("context.json"))?;
    if digest(&context.identity)? != context.generation {
        return Err("owner context identity is corrupt".into());
    }
    Ok(context.generation)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthoringResult {
    correlation: String,
    tool: String,
    label: String,
    boundary: ReturnBoundary,
    path: PathBuf,
    before: String,
    after: String,
    bytes: usize,
}

pub fn prepare_authoring_result(
    root: &Path,
    correlation: &str,
    tool: &str,
    label: &str,
    boundary: &ReturnBoundary,
    path: &Path,
    before: &str,
    after: &str,
    bytes: usize,
) -> Result<(), String> {
    let (_, key) = parse_owner_correlation(correlation, "code-mode-authoring")?;
    prepare_record(
        &root.join(format!("{key}.authoring-result.json")),
        &AuthoringResult {
            correlation: correlation.to_string(),
            tool: tool.to_string(),
            label: label.to_string(),
            boundary: boundary.clone(),
            path: path.to_path_buf(),
            before: before.to_string(),
            after: after.to_string(),
            bytes,
        },
    )
}

fn parse_owner_correlation<'a>(
    correlation: &'a str,
    prefix: &str,
) -> Result<(WorkerEnvironmentId, &'a str), String> {
    let mut parts = correlation.split(':');
    if parts.next() != Some(prefix) {
        return Err("foreign owner correlation".into());
    }
    let worker =
        WorkerEnvironmentId::parse(parts.next().ok_or("owner correlation has no formation")?)
            .map_err(|e| e.to_string())?;
    let key = parts.next().ok_or("owner correlation has no digest")?;
    if DescriptorHash::from_hex(key).is_none() || parts.next().is_some() {
        return Err("malformed owner correlation".into());
    }
    Ok((worker, key))
}

pub(crate) fn consume_authoring_result(
    ctx: &octopus_tool_door_runtime::InterfaceLiftContext,
    envelope: &InterfaceInvocationEnvelope,
) -> Result<bool, String> {
    consume_authoring_result_with_source(ctx, envelope, || ctx.source_movement(envelope).cloned())
}

pub fn validate_historical_authoring_result(
    ctx: &octopus_tool_door_runtime::InterfaceLiftContext,
    envelope: &InterfaceInvocationEnvelope,
    source: &Movement,
) -> Result<bool, String> {
    let limits = ctx.owner_io_limits()?;
    let evidence = octopus_run_authoring::CheckedRunEvidence {
        movements_path: ctx.owner_movements_path(), ledger_index_path: ctx.owner_ledger_index_path(), blobs_dir: ctx.owner_blobs_dir(),
        max_control_frame_bytes: limits.max_control_frame_bytes, max_manifest_bytes: limits.max_manifest_bytes, max_payload_bytes: limits.max_payload_bytes,
    };
    if source.kind != octopus_ledger::MovementKind::InterfaceOutbound
        || source.to != Endpoint::Interface(ctx.owner_interface().clone())
        || source.artifact.as_ref() != Some(&envelope.artifact)
        || source.invocation_metadata.as_ref().and_then(|metadata| metadata.invocation())
            != Some(envelope.metadata().map_err(|e| e.to_string())?)
        || octopus_run_authoring::generated_artifacts::read_movement(&evidence, source.seq.0)? != *source {
        return Err("historical authoring result changed its exact admitted movement".into());
    }
    consume_authoring_result_with_source(ctx, envelope, || Ok(source.clone()))
}

fn consume_authoring_result_with_source(
    ctx: &octopus_tool_door_runtime::InterfaceLiftContext,
    envelope: &InterfaceInvocationEnvelope,
    source: impl FnOnce() -> Result<Movement, String>,
) -> Result<bool, String> {
    let captured = envelope
        .read_artifact_bounded(MANIFEST_BYTES, RECORD_BYTES as u64)
        .map_err(|e| e.to_string())?;
    let payload: Value =
        serde_json::from_slice(&captured.payload_bytes).map_err(|e| e.to_string())?;
    let Some(correlation) = payload
        .get("correlation_id")
        .and_then(Value::as_str)
        .filter(|s| s.starts_with("code-mode-authoring:"))
    else {
        return Ok(false);
    };
    let (worker, key) = parse_owner_correlation(correlation, "code-mode-authoring")?;
    let key = key.to_owned();
    let root = octopus_tool_door_runtime::tool_door_durable_state_dir(ctx, &worker)?;
    let pending: AuthoringResult = read_record(&root.join(format!("{key}.authoring-result.json")))?;
    validate_carrier(envelope, &captured.manifest_bytes, &captured.manifest.label)?;
    let source = source()?;
    let result: ToolResultArtifact = serde_json::from_value(payload).map_err(|e| e.to_string())?;
    let metadata = envelope.metadata().map_err(|e| e.to_string())?;
    if !result.ok {
        if result.correlation_id.as_deref() != Some(pending.correlation.as_str())
            || result.tool != pending.tool
            || result.label != pending.label
            || result.label != captured.manifest.label
            || result.body.is_some()
            || result.error.is_none()
            || source.from != Endpoint::Interface(pending.boundary.binding.inbound.interface.clone())
            || !metadata
                .checkpoint_admission
                .as_ref()
                .is_some_and(|a| a.complete && a.artifact_label == pending.label)
            || metadata.verification_failure.is_some()
            || !metadata.required_verified_artifacts.is_empty()
        {
            return Err("failed authoring result differs from its exact owner-prepared call".into());
        }
        return Ok(true);
    }
    let mut boundary = pending.boundary.clone();
    let hash = envelope.runtime().map_err(|error| error.to_string())?.run_record_hash.as_deref()
        .ok_or("authoring result has no admitted snapshot")?;
    if hash != boundary.run_record_hash {
        let read = |hash: &str| -> Result<Vec<u8>, String> {
            let pin = ContentHash::from_hex(hash).ok_or("authoring result snapshot hash invalid")?;
            let bytes = octopus_interfaces::read_artifact_carrier_file_bounded(&ctx.owner_blobs_dir().join(hash), RECORD_BYTES as u64).map_err(|error| error.to_string())?;
            if ContentHash::of_bytes(&bytes) != pin { return Err("authoring result snapshot integrity mismatch".into()); }
            Ok(bytes)
        };
        let installed = checked_authoring_result_install(ctx, &root, &key, &pending, &source)?;
        octopus_run_authoring::validate_unchanged_admitted_checkpoint(
            &installed, &read(hash)?, &octopus_descriptors::CheckpointId::new(&boundary.checkpoint),
        )?;
        boundary.run_record_hash = hash.to_owned();
    }
    if result.correlation_id.as_deref() != Some(pending.correlation.as_str())
        || result.tool != pending.tool
        || result.label != pending.label
        || result.label != captured.manifest.label
        || !boundary.matches(envelope)?
        || source.from != Endpoint::Interface(pending.boundary.binding.inbound.interface.clone())
        || !metadata
            .checkpoint_admission
            .as_ref()
            .is_some_and(|a| a.complete && a.artifact_label == pending.label)
        || metadata.verification_failure.is_some()
        || !metadata.required_verified_artifacts.is_empty()
    {
        return Err(
            "authoring result differs from its exact owner-prepared call and admitted return"
                .into(),
        );
    }
    let expected = json!({"result":"files_written","writes":[{"path":pending.path,"before_hash":pending.before,"after_hash":pending.after,"bytes":pending.bytes}]});
    if result.error.is_some() || result.body.as_ref() != Some(&expected) {
        return Err("authoring result differs from exact checked write evidence".into());
    }
    Ok(true)
}

fn checked_authoring_result_install(
    ctx: &octopus_tool_door_runtime::InterfaceLiftContext, root: &Path, key: &str,
    pending: &AuthoringResult, result: &Movement,
) -> Result<Vec<u8>, String> {
    use octopus_run_authoring::{CheckedRunEdit, CheckedRunEvidence, CheckedRunEvidenceReader,
        CheckedRunInstall, CheckedRunWriter, ConsentEvidenceRef, FileCheckedRunReader, RunAuthoringProfile};
    let context: ContextRecord = read_record(&root.join("context.json"))?;
    if digest(&context.identity)? != context.generation || context.identity.formation.door_interface != *ctx.owner_interface() {
        return Err("authoring result changed its immutable owner context".into());
    }
    let topology = context.identity.owner_topology.as_ref().ok_or("authoring result has no retained topology")?;
    let descriptor = ctx.owner_descriptor()?;
    let profile: RunAuthoringProfile = serde_json::from_value(descriptor.interface_driver_config.as_ref()
        .and_then(|config| config.private_config.get("run_authoring_profile"))
        .cloned().ok_or("authoring result has no checked writer profile")?).map_err(|error| error.to_string())?;
    let writer = CheckedRunWriter { owner: ctx.owner_interface().clone(), source: ctx.owner_interface().clone(),
        door: topology.authoring_result.binding.inbound.interface.clone(), tool: profile.file_write_tool,
        call_label: profile.file_write_label, result_label: profile.file_write_result_label };
    if writer.tool != pending.tool || writer.result_label != pending.label
        || pending.boundary.binding != topology.authoring_result.binding {
        return Err("authoring result changed its exact retained writer".into());
    }
    let limits = ctx.owner_io_limits()?;
    let evidence = CheckedRunEvidence { movements_path:ctx.owner_movements_path(), ledger_index_path:ctx.owner_ledger_index_path(),
        blobs_dir:ctx.owner_blobs_dir(), max_control_frame_bytes:limits.max_control_frame_bytes,
        max_manifest_bytes:limits.max_manifest_bytes, max_payload_bytes:limits.max_payload_bytes };
    let mut reader = FileCheckedRunReader::open(&evidence, Some(context.identity.first_run.source_movement_seq))?;
    let mut selected = None;
    while reader.next_sequence() < result.seq.0 {
        let movement = reader.movement(reader.next_sequence())?.ok_or("authoring result has no complete admitted prefix")?;
        if movement.kind != octopus_ledger::MovementKind::InterfaceOutbound
            || movement.from != Endpoint::Interface(writer.source.clone()) || movement.to != Endpoint::Interface(writer.door.clone()) {
            continue;
        }
        let (manifest, payload) = reader.artifact(movement.artifact.as_ref().ok_or("authoring call has no exact artifact")?)?;
        if manifest.label != writer.call_label { continue; }
        let call: ToolCallArtifact = serde_json::from_slice(&payload).map_err(|error| error.to_string())?;
        if call.correlation_id.as_deref() != Some(pending.correlation.as_str()) { continue; }
        if call.tool != writer.tool || selected.replace((movement, call)).is_some() {
            return Err("authoring result has no unique exact admitted call".into());
        }
    }
    let (call, artifact) = selected.ok_or("authoring result has no matching admitted call")?;
    let hash = call.runtime().and_then(|runtime| runtime.run_record_hash.as_deref()).and_then(ContentHash::from_hex)
        .ok_or("authoring call lost its admitted preimage")?;
    let declaration = reader.snapshot(&hash)?;
    if ContentHash::of_bytes(&declaration) != hash { return Err("authoring call preimage integrity mismatch".into()); }
    let mut seeds = topology.coauthor.iter().cloned().collect::<Vec<_>>();
    seeds.push(writer);
    let transition = octopus_run_authoring::checked_declared_run_install(&evidence, &pending.path, &declaration,
        &CheckedRunInstall {
            call: ConsentEvidenceRef { movement_seq:call.seq.0, artifact:call.artifact.clone().ok_or("authoring call lost its artifact")? },
            result: ConsentEvidenceRef { movement_seq:result.seq.0, artifact:result.artifact.clone().ok_or("authoring result lost its artifact")? },
        }, seeds)?;
    let edit: CheckedRunEdit = read_record(&root.join(format!("{key}.edit.json")))?;
    let expected = CheckedRunEdit::prepare_with_format(pending.path.clone(), &transition.before,
        serde_json::from_slice(&transition.after).map_err(|error| error.to_string())?, pending.correlation.clone(), edit.json_format)?;
    if transition.call != call || transition.result != *result
        || artifact.arguments != edit.arguments()?
        || serde_json::to_value(&edit).map_err(|error| error.to_string())? != serde_json::to_value(&expected).map_err(|error| error.to_string())?
        || edit.before_hash != pending.before || edit.after_hash != pending.after
        || edit.after_hash != pending.boundary.run_record_hash || edit.content.len() != pending.bytes {
        return Err("authoring result differs from its exact prepared checked installation".into());
    }
    Ok(transition.after)
}

impl ContinuationContext {
    pub(crate) fn ready_record(&self, key: &str) -> Result<FrontierReady, String> {
        if DescriptorHash::from_hex(key).is_none() {
            return Err("malformed readiness digest".into());
        }
        read_record(&self.root.join(format!("{key}.ready.json")))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingApplication {
    pub pending_digest: String,
    pub request: octopus_app_server_interface::OwnerOperatorRequest,
    pub operator_interface: InterfaceId,
    pub request_artifact: ArtifactRef,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplicationDecision {
    pub request_id: String,
    pub call_id: String,
    pub decision: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_persistence: Option<octopus_app_server_interface::OperatorPermissionPersistence>,
    pub artifact: ArtifactRef,
    pub source_seq: u64,
    pub metadata_hash: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplicationProjection {
    pub request_id: String,
    pub pending_digest: String,
    pub boundary: ReturnBoundary,
    pub checkpoints: Vec<String>,
}

pub fn pending_application(root: &Path, key: &str) -> Result<PendingApplication, String> {
    if DescriptorHash::from_hex(key).is_none() {
        return Err("invalid pending application digest".into());
    }
    let pending: PendingApplication = read_record(&root.join(format!("{key}.application.json")))?;
    pending.request.validate()?;
    pending.request.validate_operator_interface(&pending.operator_interface)?;
    if root.join(format!("{key}.selected-consent.json")).try_exists().map_err(|e| e.to_string())? {
        return Err("selected Home consent state cannot use the ordinary nested operator continuation".into());
    }
    if pending.pending_digest != key {
        return Err("application changed its immutable pending identity".into());
    }
    Ok(pending)
}

pub fn record_application_projection(
    root: &Path,
    value: &ApplicationProjection,
) -> Result<(), String> {
    let application = pending_application(root, &value.pending_digest)?;
    let expected = vec![format!("application_{}_approved",application.pending_digest),format!("application_{}_denied",application.pending_digest)];
    if application.request.request_id != value.request_id || value.checkpoints != expected {
        return Err("application projection does not match its exact request and paired checkpoints".into());
    }
    create_record(
        &root.join(format!(
            "{}.{}.application-projected.json",
            value.pending_digest, value.boundary.run_record_hash
        )),
        value,
    )
}

impl ContinuationContext {
    pub(crate) fn suspend_application(
        &self,
        envelope: &InterfaceInvocationEnvelope,
        seq: u64,
    ) -> Result<Option<PendingApplication>, String> {
        let (pending, result) = read_pending_result(
            &self.root,
            envelope,
            ResultCarrier::FormedInvocation {
                source_movement_seq: seq,
            },
        )?;
        let Some(request) = result.body.as_ref().and_then(|v| v.get("operator_request")) else {
            return Ok(None);
        };
        let request: octopus_app_server_interface::OwnerOperatorRequest =
            serde_json::from_value(request.clone()).map_err(|e| e.to_string())?;
        request.validate()?;
        request.validate_bound_files(envelope.metadata().map_err(|e| e.to_string())?)?;
        validate_return(&self.root, &pending, envelope, &result)?;
        let topology = self
            .record
            .identity
            .config
            .resolve_owner_topology(envelope)?;
        let link = topology
            .routes
            .iter()
            .find(|r| r.tool == pending.identity.tool)
            .and_then(|r| r.app_approval.as_ref())
            .ok_or("nested application has no declared operator/resume linkage")?;
        let emitted: EmissionRecord =
            read_record(&self.root.join(format!("{}.emission.json", pending.digest)))?;
        let EmissionOutcome::Admitted { seq: emitted_seq } = emitted.outcome else {
            return Err("application request follows a non-admitted tool call".into());
        };
        if !result.ok
            || result.error.is_some()
            || result.body.as_ref().and_then(|v| v.get("effect_performed"))
                != Some(&Value::Bool(false))
            || request.original.tool != pending.identity.tool
            || request.call_id != pending.correlation()
            || request.original.arguments != pending.identity.arguments
            || request.original.source_artifact != emitted.artifact
            || request.original.source_movement_seq < emitted_seq
            || request.original.source_movement_seq >= seq
            || request.original.door_interface.as_str() != pending.identity.route.interface
            || request.resume.tool != link.resume_tool
            || request.original.run != self.record.identity.formation.run
            || request.original.ee != self.record.identity.formation.ee
            || request.original.worker != self.record.identity.formation.worker
        {
            return Err(
                "application request differs from the exact admitted nested call or declared owner"
                    .into(),
            );
        }
        request.validate_operator_interface(&InterfaceId::new(&link.operator_interface))?;
        let application = PendingApplication {
            pending_digest: pending.digest,
            request,
            operator_interface: InterfaceId::new(&link.operator_interface),
            request_artifact: envelope.artifact.clone(),
        };
        create_record(
            &self
                .root
                .join(format!("{}.application.json", application.pending_digest)),
            &application,
        )?;
        Ok(Some(application))
    }
}

fn application_locator(
    envelope: &InterfaceInvocationEnvelope,
) -> Result<Option<(WorkerEnvironmentId, String)>, String> {
    let payload: Value = serde_json::from_slice(
        &envelope
            .read_artifact_payload_bounded(RECORD_BYTES as u64)
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let call = payload
        .pointer("/operator_approval_response/call_id")
        .or_else(|| payload.pointer("/body/original_result/correlation_id"))
        .and_then(Value::as_str);
    let Some(call) = call.filter(|c| c.starts_with("code-mode-tool:")) else {
        return Ok(None);
    };
    let (worker, key) = parse_correlation(call)?;
    Ok(Some((worker, key.to_string())))
}

pub(crate) fn decode_application(
    root: &Path,
    envelope: &InterfaceInvocationEnvelope,
    source: &Movement,
    purpose: ToolDoorArtifactDecodePurpose,
    config: Option<&CodeModeConfig>,
) -> Result<ToolCallArtifact, String> {
    let (_, key) =
        application_locator(envelope)?.ok_or("no immutable application continuation identity")?;
    let application = pending_application(root, &key)?;
    let pending: PreparedCall = read_record(&root.join(format!("{key}.pending.json")))?;
    pending.validate()?;
    let captured = envelope
        .read_artifact_bounded(MANIFEST_BYTES, RECORD_BYTES as u64)
        .map_err(|e| e.to_string())?;
    validate_carrier(envelope, &captured.manifest_bytes, &captured.manifest.label)?;
    let payload: Value =
        serde_json::from_slice(&captured.payload_bytes).map_err(|e| e.to_string())?;
    let is_decision = captured.manifest.label == "operator.approval.response";
    let metadata = envelope.metadata().map_err(|e| e.to_string())?;
    let runtime = envelope.runtime().map_err(|e| e.to_string())?;
    let formation = &pending.identity.context.identity.formation;
    application.request.validate_bound_files(metadata)?;
    if runtime.run.as_str() != formation.run
        || runtime.ee.as_str() != formation.ee
        || runtime.worker != formation.worker
        || runtime.interface != formation.door_interface
        || !metadata
            .checkpoint_admission
            .as_ref()
            .is_some_and(|a| a.complete && a.artifact_label == captured.manifest.label)
        || metadata.verification_failure.is_some()
        || !metadata.grant.tools.contains(&pending.identity.resume_tool)
        || metadata
            .grant
            .hard_blocked_tools
            .contains(&pending.identity.resume_tool)
    {
        return Err("application continuation has foreign or incomplete admission".into());
    }
    if purpose == ToolDoorArtifactDecodePurpose::Dispatch
        && (config != Some(&pending.identity.context.identity.config)
            || root
                .join("retired.json")
                .try_exists()
                .map_err(|e| e.to_string())?)
    {
        return Err("application continuation context changed or retired".into());
    }
    let suffix = if is_decision {
        let response = application.request.parse_operator_response(payload.get("operator_approval_response")
            .cloned().ok_or("missing exact application response wrapper")?)?;
        use octopus_app_server_interface::OperatorPermissionDecision;
        let (decision, mark, verifier) = match response.decision {
            OperatorPermissionDecision::Accept => ("accept", "operator.approval", "operator_approval"),
            OperatorPermissionDecision::Decline => ("decline", "operator.input.provided", "operator_input"),
            OperatorPermissionDecision::Cancel => ("cancel", "operator.input.provided", "operator_input"),
        };
        let requirement = json!({"mark":mark,"artifact_label":"operator.approval.response","verifier":verifier,
            "params":application.request.verification_params()});
        let projection: ApplicationProjection = read_record(&root.join(format!(
                "{key}.{}.application-projected.json",
                runtime
                    .run_record_hash
                    .as_deref()
                    .ok_or("decision has no pinned snapshot")?
            )))?;
        let binding = metadata
            .checkpoint_binding_policy
            .as_ref()
            .ok_or("decision has no admitted bindings")?;
        if source.from != Endpoint::Interface(application.operator_interface.clone())
            || runtime.run_record_hash.as_deref()
                != Some(projection.boundary.run_record_hash.as_str())
            || !projection
                .checkpoints
                .iter()
                .any(|id| id == runtime.checkpoint.as_str())
            || binding.inbound.interface != application.operator_interface
            || binding
                .outbound
                .iter()
                .map(|b| &b.interface)
                .collect::<Vec<_>>()
                != [
                    &formation.door_interface,
                    &application.request.original.door_interface,
                ]
            || metadata.outbound_interface_bindings
                != [
                    formation.door_interface.clone(),
                    application.request.original.door_interface.clone(),
                ]
            || serde_json::to_value(&metadata.required_verified_artifacts)
                .map_err(|e| e.to_string())?
                != json!([requirement])
            || !metadata.grant.mark_outcomes.iter().any(|v| v == mark)
            || !metadata
                .checkpoint_admission
                .as_ref()
                .is_some_and(|a| a.required_artifact_labels == ["operator.approval.response"])
        {
            return Err("application decision did not cross its exact verifier/mark and paired owner/GUI checkpoint".into());
        }
        if purpose == ToolDoorArtifactDecodePurpose::Dispatch {
            create_record(
                &root.join(format!("{key}.application-decision.json")),
                &ApplicationDecision {
                    request_id: application.request.request_id.clone(),
                    call_id: application.request.call_id.clone(),
                    decision: decision.into(),
                    selected_persistence: response.selected_persistence,
                    artifact: envelope.artifact.clone(),
                    source_seq: source.seq.0,
                    metadata_hash: digest(metadata)?,
                },
            )?;
        }
        "decision"
    } else {
        let outer: ToolResultArtifact =
            serde_json::from_value(payload.clone()).map_err(|e| e.to_string())?;
        let body = outer.body.as_ref().ok_or("GUI resume result has no body")?;
        let original: ToolResultArtifact = serde_json::from_value(
            body.get("original_result")
                .cloned()
                .ok_or("GUI resume lost original result")?,
        )
        .map_err(|e| e.to_string())?;
        if source.from != Endpoint::Interface(application.request.original.door_interface.clone())
            || outer.tool != application.request.resume.tool
            || outer.correlation_id.as_deref()
                != Some(application.request.resume.correlation_id.as_str())
            || outer.label != format!("{}.result", application.request.resume.tool)
            || !outer.ok
            || outer.error.is_some()
            || body.get("request_id").and_then(Value::as_str)
                != Some(application.request.request_id.as_str())
            || original.tool != pending.identity.tool
            || original.correlation_id.as_deref() != Some(application.request.call_id.as_str())
            || original.label != format!("{}.result", pending.identity.tool)
        {
            return Err(
                "GUI resume result differs from its original pending application call".into(),
            );
        }
        validate_return(root, &pending, envelope, &outer)?;
        "app-result"
    };
    if purpose == ToolDoorArtifactDecodePurpose::Dispatch {
        let witness = result_witness(
            envelope,
            {
                let Endpoint::Interface(source_interface) = &source.from;
                source_interface.clone()
            },
            source.seq.0,
        )?;
        create_record(
            &root.join(format!("{key}.{suffix}-admitted.json")),
            &witness,
        )?;
        create_record(&root.join(format!("{key}.{suffix}-payload.json")), &payload)?;
    }
    application_call(&pending, &payload, suffix)
}

fn application_call(
    pending: &PreparedCall,
    payload: &Value,
    suffix: &str,
) -> Result<ToolCallArtifact, String> {
    Ok(ToolCallArtifact {
        tool: pending.identity.resume_tool.clone(),
        arguments: json!({"cell_id":pending.identity.cell_id,"tool_call_id":pending.identity.runtime_tool_call_id,"result":payload}),
        correlation_id: Some(format!(
            "code-mode-{suffix}:{}:{}",
            pending
                .identity
                .context
                .identity
                .formation
                .worker_environment_id,
            pending.digest
        )),
        label: Some("tool.call".into()),
        source_interface_evidence: BTreeMap::new(),
    })
}

pub(crate) fn formed_application_call(
    root: &Path,
    envelope: &InterfaceInvocationEnvelope,
    config: &CodeModeConfig,
    seq: u64,
) -> Result<ToolCallArtifact, String> {
    let (_, key) = application_locator(envelope)?.ok_or("no application identity")?;
    let pending: PreparedCall = read_record(&root.join(format!("{key}.pending.json")))?;
    let application = pending_application(root, &key)?;
    let captured = envelope.read_artifact_bounded(MANIFEST_BYTES, RECORD_BYTES as u64).map_err(|e| e.to_string())?;
    let suffix = if captured.manifest.label == "operator.approval.response" { "decision" } else { "app-result" };
    let witness: AdmittedCarrier =
        read_record(&root.join(format!("{key}.{suffix}-admitted.json")))?;
    let owner = if suffix == "decision" { application.operator_interface } else { application.request.original.door_interface };
    if witness != result_witness(envelope, owner, seq)?
        || config != &pending.identity.context.identity.config
    {
        return Err("formed application continuation is not its original admitted carrier".into());
    }
    let payload: Value = read_record(&root.join(format!("{key}.{suffix}-payload.json")))?;
    application_call(&pending, &payload, suffix)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplicationRetirement {
    pub pending_digest: String,
    pub before_hash: String,
    pub after_hash: String,
    pub decision_artifact: ArtifactRef,
    pub decision_seq: u64,
    pub paired_checkpoints: Vec<String>,
}

pub fn applications(root: &Path) -> Result<Vec<PendingApplication>, String> {
    validate_directory(root)?;
    let mut result = Vec::new();
    for entry in std::fs::read_dir(root).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        if entry
            .file_name()
            .to_str()
            .is_some_and(|s| s.ends_with(".application.json"))
        {
            let application: PendingApplication = read_record(&entry.path())?;
            application.request.validate()?;
            application.request.validate_operator_interface(&application.operator_interface)?;
            if root.join(format!("{}.selected-consent.json", application.pending_digest)).try_exists().map_err(|e| e.to_string())? {
                return Err("selected Home consent state cannot use the ordinary nested operator continuation".into());
            }
            if !root
                .join(format!(
                    "{}.application-retired.json",
                    application.pending_digest
                ))
                .try_exists()
                .map_err(|e| e.to_string())?
            {
                result.push(application);
            }
        }
    }
    Ok(result)
}

pub fn record_application_retirement(
    root: &Path,
    retirement: &ApplicationRetirement,
) -> Result<(), String> {
    let decision: ApplicationDecision = read_record(&root.join(format!(
        "{}.application-decision.json",
        retirement.pending_digest
    )))?;
    let projection: ApplicationProjection = read_record(&root.join(format!(
        "{}.{}.application-projected.json",
        retirement.pending_digest, retirement.before_hash
    )))?;
    let installed: InstalledHandoff =
        read_record(&root.join(format!("{}.installed.json", retirement.after_hash)))?;
    if decision.artifact != retirement.decision_artifact
        || decision.source_seq != retirement.decision_seq
        || projection.checkpoints != retirement.paired_checkpoints
        || installed.before_hash != retirement.before_hash
        || installed.after_hash != retirement.after_hash
        || installed.source_seq <= decision.source_seq
    {
        return Err("application retirement does not prove exact admitted decision and paired checkpoint removal".into());
    }
    create_record(
        &root.join(format!(
            "{}.application-retired.json",
            retirement.pending_digest
        )),
        retirement,
    )
}

impl ContinuationContext {
    pub(crate) fn application_input(
        &self,
        envelope: &InterfaceInvocationEnvelope,
    ) -> Result<Option<(PendingApplication, bool)>, String> {
        let Some((_, key)) = application_locator(envelope)? else {
            return Ok(None);
        };
        let decision = envelope
            .read_artifact_manifest()
            .map_err(|e| e.to_string())?
            .label
            == "operator.approval.response";
        let application = pending_application(&self.root, &key)?;
        Ok(Some((application, decision)))
    }

    pub(crate) fn finish_application(
        &self,
        application: &PendingApplication,
    ) -> Result<Option<AdmittedResult>, String> {
        self.validate_live_record()?;
        let key = &application.pending_digest;
        if !self
            .root
            .join(format!("{key}.application-retired.json"))
            .try_exists()
            .map_err(|e| e.to_string())?
            || !self
                .root
                .join(format!("{key}.app-result-admitted.json"))
                .try_exists()
                .map_err(|e| e.to_string())?
        {
            return Ok(None);
        }
        let retirement: ApplicationRetirement =
            read_record(&self.root.join(format!("{key}.application-retired.json")))?;
        let decision: ApplicationDecision =
            read_record(&self.root.join(format!("{key}.application-decision.json")))?;
        let witness: AdmittedCarrier =
            read_record(&self.root.join(format!("{key}.app-result-admitted.json")))?;
        let payload: Value =
            read_record(&self.root.join(format!("{key}.app-result-payload.json")))?;
        let pending: PreparedCall = read_record(&self.root.join(format!("{key}.pending.json")))?;
        pending.validate()?;
        if retirement.decision_artifact != decision.artifact
            || witness.source_movement_seq <= decision.source_seq
        {
            return Err("GUI resume did not follow its exact admitted operator decision".into());
        }
        let result: ToolResultArtifact = serde_json::from_value(
            payload
                .pointer("/body/original_result")
                .cloned()
                .ok_or("queued GUI resume has no original result")?,
        )
        .map_err(|e| e.to_string())?;
        let mut response = json!({"request_id":decision.request_id,"call_id":decision.call_id,"decision":decision.decision});
        if let Some(persistence) = decision.selected_persistence { response["selected_persistence"] = json!(persistence); }
        application.request.parse_operator_response(response)?;
        if decision.decision != "accept" && result.ok {
            return Err("declined application returned a successful original operation".into());
        }
        let emission: EmissionRecord =
            read_record(&self.root.join(format!("{key}.emission.json")))?;
        let EmissionOutcome::Admitted { seq } = emission.outcome else {
            return Err("GUI continuation original call was not admitted".into());
        };
        let accepted = AcceptedResult {
            artifact: witness.artifact,
            metadata_digest: witness.metadata_hash,
            emitted_seq: seq,
            result_seq: witness.source_movement_seq,
        };
        let path = self.root.join(format!("{key}.accepted.json"));
        if path.try_exists().map_err(|e| e.to_string())? {
            return Ok(None);
        }
        create_record(&path, &accepted)?;
        Ok(Some(AdmittedResult { pending, result }))
    }
}

pub fn validate_admitted_coauthor(
    ctx: &octopus_tool_door_runtime::InterfaceLiftContext,
    envelope: &InterfaceInvocationEnvelope,
    topology: &crate::config::CodeModeOwnerTopology,
) -> Result<(), String> {
    let Some(writer) = &topology.coauthor else { return Ok(()); };
    let metadata = envelope.metadata().map_err(|e| e.to_string())?;
    let hash = envelope.runtime().map_err(|e| e.to_string())?.run_record_hash.as_deref()
        .ok_or("coauthor proof has no admitted Run snapshot")?;
    let pin = ContentHash::from_hex(hash).filter(|pin| pin.to_hex() == hash)
        .ok_or("coauthor proof has an invalid Run snapshot hash")?;
    let bytes = octopus_interfaces::read_artifact_carrier_file_bounded(
        &ctx.owner_blobs_dir().join(hash), ctx.owner_io_limits()?.max_payload_bytes,
    ).map_err(|e| e.to_string())?;
    if ContentHash::of_bytes(&bytes) != pin { return Err("coauthor Run snapshot integrity mismatch".into()); }
    octopus_run_authoring::validate_admitted_run_writer(&bytes, metadata, writer)
}

pub fn checked_recovery_observation<T>(
    mut read: impl FnMut() -> Result<Vec<u8>, String>,
    mut prove: impl FnMut(&[u8]) -> Result<T, octopus_run_authoring::CheckedRunError>,
    cancelled: impl Fn() -> bool,
) -> Result<T, octopus_run_authoring::CheckedRunError> {
    loop {
        if cancelled() { return Err("shared run recovery cancelled".into()); }
        let current = read()?;
        let result = prove(&current);
        let reread = read()?;
        if reread == current { return result; }
        std::thread::yield_now();
    }
}

pub fn first_output_is_unsubmitted(
    ctx: &octopus_tool_door_runtime::InterfaceLiftContext,
    root: &Path,
    config: &CodeModeConfig,
    source: &octopus_tool_door_runtime::ToolDoorOutputSource,
    output: &octopus_interfaces::InterfaceArtifactFile,
) -> Result<bool, String> {
    use std::io::BufRead;
    let context: ContextRecord = read_record(&root.join("context.json"))?;
    let Some(topology) = context.identity.owner_topology.as_ref() else { return Ok(false); };
    let Some(writer) = topology.coauthor.as_ref() else { return Ok(false); };
    let original = OriginalRun::from_envelope(&source.envelope, source.movement.seq.0)?;
    if context.identity.config != *config || context.generation != digest(&context.identity)?
        || context.identity.first_run != original
        || source.formation.as_ref() != Some(&context.identity.formation)
        || context.identity.formation.door_interface != *ctx.owner_interface()
        || config.resolve_owner_topology(&source.envelope)? != *topology
        || root.join("retired.json").try_exists().map_err(|e| e.to_string())?
    { return Ok(false); }
    validate_admitted_coauthor(ctx, &source.envelope, topology)?;
    let ResolvedContext::Persistent { artifact, payload, .. } = config.resolve_context(&source.envelope)? else { return Ok(false); };
    if artifact != context.identity.prelude_artifact
        || DescriptorHash::of_bytes(&payload).to_hex() != context.identity.prelude_payload_hash
    { return Err("unsubmitted output changed its admitted prelude".into()); }
    let limits = ctx.owner_io_limits()?;
    let captured = output.read_bounded(limits.max_manifest_bytes, limits.max_payload_bytes).map_err(|e| e.to_string())?;
    let result: ToolResultArtifact = serde_json::from_slice(&captured.payload_bytes).map_err(|e| e.to_string())?;
    if result.tool != original.tool || result.correlation_id.as_deref() != Some(original.correlation_id.as_str())
        || result.label != captured.manifest.label || result.label != format!("{}.result", original.tool)
    { return Ok(false); }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)] {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let ledger = options.open(ctx.owner_movements_path()).map_err(|e| e.to_string())?;
    let metadata = ledger.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() { return Err("unsubmitted output ledger is not a regular file".into()); }
    let length = metadata.len();
    let mut reader = std::io::BufReader::new(ledger.take(length));
    let decoder = octopus_ledger_store::DurableMovementDecoder::for_log(ctx.owner_movements_path());
    let mut sequence = 0;
    let mut source_seen = false;
    let mut line = Vec::new();
    loop {
        line.clear();
        let count = Read::by_ref(&mut reader).take(limits.max_control_frame_bytes.saturating_add(1))
            .read_until(b'\n', &mut line).map_err(|e| e.to_string())?;
        if count == 0 { break; }
        if count as u64 > limits.max_control_frame_bytes { return Err("unsubmitted output ledger row exceeds its bound".into()); }
        let Some(bytes) = line.strip_suffix(b"\n") else { return Ok(false); };
        let row = decoder.decode(bytes, sequence).map_err(|e| e.to_string())?;
        if row.seq.0 != sequence as u64 { return Err("unsubmitted output ledger sequence changed".into()); }
        sequence += 1;
        if row.seq == source.movement.seq {
            if row != source.movement { return Err("unsubmitted output source differs from its admitted movement".into()); }
            source_seen = true;
        } else if row.seq > source.movement.seq && row.kind == octopus_ledger::MovementKind::InterfaceOutbound
            && row.from == Endpoint::Interface(ctx.owner_interface().clone()) {
            return Ok(false);
        }
    }
    if !source_seen { return Err("unsubmitted output has no exact admitted source".into()); }
    let run_root = ctx.owner_root_slot("run_records_root")?.canonicalize().map_err(|e| e.to_string())?;
    let path = run_root.join(format!("{}.json", topology.run)).canonicalize().map_err(|e| e.to_string())?;
    if path.parent() != Some(run_root.as_path()) { return Err("unsubmitted output run escapes its granted root".into()); }
    let before = octopus_interfaces::read_artifact_carrier_file_bounded(
        &ctx.owner_blobs_dir().join(&original.run_record_hash), limits.max_payload_bytes).map_err(|e| e.to_string())?;
    if ContentHash::of_bytes(&before).to_hex() != original.run_record_hash { return Err("unsubmitted output snapshot integrity mismatch".into()); }
    let evidence = octopus_run_authoring::CheckedRunEvidence {
        movements_path: ctx.owner_movements_path(), ledger_index_path: ctx.owner_ledger_index_path(), blobs_dir: ctx.owner_blobs_dir(),
        max_control_frame_bytes: limits.max_control_frame_bytes, max_manifest_bytes: limits.max_manifest_bytes, max_payload_bytes: limits.max_payload_bytes,
    };
    let cancel = ctx.owner_cancel();
    let unchanged_owner = checked_recovery_observation(
        || octopus_interfaces::read_artifact_carrier_file_bounded(&path, limits.max_payload_bytes).map_err(|e| e.to_string()),
        |current| Ok(octopus_run_authoring::checked_declared_run_advance(
            &evidence, &path, &before, current, original.source_movement_seq, vec![writer.clone()])?
            .iter().all(|transition| transition.writer == *writer)),
        || cancel.is_cancelled(),
    )?;
    let after = reader.get_ref().get_ref().metadata().map_err(|e| e.to_string())?;
    Ok(unchanged_owner && after.len() == length && after.modified().ok() == metadata.modified().ok())
}

pub enum OutputAuthority {
    Admitted(octopus_tool_door_runtime::ToolDoorOutputSource),
    Initial(octopus_interfaces::InterfaceArtifactFile),
    Pending,
}

pub fn output_authority(
    ctx: &octopus_tool_door_runtime::InterfaceLiftContext,
    root: &Path,
    config: &CodeModeConfig,
    source: &octopus_tool_door_runtime::ToolDoorOutputSource,
) -> Result<OutputAuthority, octopus_run_authoring::CheckedRunError> {
    use octopus_run_authoring::CheckedRunEvidenceReader;
    use octopus_run_record::RunRecord;
    let context: ContextRecord = read_record(&root.join("context.json"))?;
    let topology = context.identity.owner_topology.as_ref().ok_or("output authority has no declared topology")?;
    if context.identity.config != *config || context.generation != digest(&context.identity)?
        || source.formation.as_ref() != Some(&context.identity.formation)
        || topology.owner_interface != ctx.owner_interface().as_str()
        || config.resolve_owner_topology(&source.envelope)? != *topology
        || root.join("retired.json").try_exists().map_err(|e| e.to_string())?
    { return Err("output authority changed its original context".into()); }
    validate_admitted_coauthor(ctx, &source.envelope, topology)?;
    let limits = ctx.owner_io_limits()?;
    let original = &context.identity.first_run;
    let before = octopus_interfaces::read_artifact_carrier_file_bounded(&ctx.owner_blobs_dir().join(&original.run_record_hash), limits.max_payload_bytes).map_err(|e| e.to_string())?;
    if ContentHash::of_bytes(&before).to_hex() != original.run_record_hash { return Err("output authority original snapshot differs".into()); }
    let initial = RunRecord::from_bytes(&before).map_err(|e| e.to_string())?;
    let run_root = ctx.owner_root_slot("run_records_root")?.canonicalize().map_err(|e| e.to_string())?;
    let path = run_root.join(format!("{}.json", topology.run)).canonicalize().map_err(|e| e.to_string())?;
    if path.parent() != Some(run_root.as_path()) { return Err("output authority run escapes its granted root".into()); }
    let evidence = octopus_run_authoring::CheckedRunEvidence {
        movements_path: ctx.owner_movements_path(), ledger_index_path: ctx.owner_ledger_index_path(), blobs_dir: ctx.owner_blobs_dir(),
        max_control_frame_bytes: limits.max_control_frame_bytes, max_manifest_bytes: limits.max_manifest_bytes, max_payload_bytes: limits.max_payload_bytes,
    };
    let cancel = ctx.owner_cancel();
    let (current, transitions) = checked_recovery_observation(
        || octopus_interfaces::read_artifact_carrier_file_bounded(&path, limits.max_payload_bytes).map_err(|e| e.to_string()),
        |current| Ok((current.to_vec(), octopus_run_authoring::checked_declared_run_advance(
            &evidence, &path, &before, current, original.source_movement_seq, topology.coauthor.iter().cloned().collect())?)),
        || cancel.is_cancelled(),
    )?;
    let record = RunRecord::from_bytes(&current).map_err(|e| e.to_string())?;
    let reader = octopus_tool_door_runtime::LedgerTailReader::open_named(ctx, "code-mode-output-authority")?;
    let formed_source = |movement: Movement| -> Result<_, String> {
        let envelope = reader.envelope_for_movement(&movement)?;
        if config.resolve_owner_topology(&envelope)? != *topology { return Err("output authority changed its admitted topology".into()); }
        validate_admitted_coauthor(ctx, &envelope, topology)?;
        Ok(octopus_tool_door_runtime::ToolDoorOutputSource { movement, envelope, formation: source.formation.clone() })
    };
    let owned = record.interface_state.get(ctx.owner_interface()).and_then(|state| state.get("frontier_checkpoint_ids"))
        .and_then(Value::as_array).ok_or("output authority has no owner frontier")?;
    for transition in transitions.iter().rev().filter(|transition| transition.writer.owner == *ctx.owner_interface()) {
        let metadata = transition.result.invocation_metadata.as_ref().and_then(|value| value.invocation()).ok_or("output authority result has no admission")?;
        let runtime = metadata.runtime().ok_or("output authority result has no coordinate")?;
        if !owned.contains(&json!(runtime.checkpoint)) { continue; }
        let checkpoint = record.checkpoint(&runtime.checkpoint).ok_or("output authority return is absent")?;
        if !record.active_checkpoints.contains(&checkpoint.id)
            || checkpoint.interface_binding.as_ref() != Some(&topology.authoring_result.binding.inbound)
            || checkpoint.outbound_interface_bindings != topology.authoring_result.binding.outbound
            || transition.result.from != Endpoint::Interface(topology.authoring_result.binding.inbound.interface.clone())
            || transition.result.to != Endpoint::Interface(ctx.owner_interface().clone())
        { return Err("output authority has another authoring return".into()); }
        octopus_run_authoring::validate_unchanged_admitted_checkpoint(&transition.after, &current, &runtime.checkpoint)?;
        return Ok(OutputAuthority::Admitted(formed_source(transition.result.clone())?));
    }
    if transitions.iter().any(|transition| transition.writer.owner == *ctx.owner_interface()) {
        return Err("output authority lost its admitted authoring return".into());
    }
    let original_owned = initial.interface_state.get(ctx.owner_interface()).and_then(|state| state.get("frontier_checkpoint_ids"))
        .and_then(Value::as_array).ok_or("output authority has no initial owner frontier")?;
    let ownership = |record: &RunRecord| -> Result<Vec<Value>, String> {
        record.interface_state.get(ctx.owner_interface()).and_then(|state| state.get("owned_checkpoint_ids"))
            .and_then(Value::as_array).cloned().ok_or("output authority has no exact owned checkpoint set".into())
    };
    let original_ownership = ownership(&initial)?;
    let current_ownership = ownership(&record)?;
    let candidates = initial.checkpoints.iter().filter(|checkpoint| original_owned.contains(&json!(checkpoint.id))
        && initial.active_checkpoints.contains(&checkpoint.id)
        && checkpoint.interface_binding.as_ref() == Some(&topology.readiness.binding.inbound)
        && checkpoint.outbound_interface_bindings == topology.readiness.binding.outbound
        && checkpoint.required_verified_artifacts.is_empty()).collect::<Vec<_>>();
    let [checkpoint] = candidates.as_slice() else { return Err("output authority requires one original owned readiness edge".into()); };
    if topology.readiness.binding.inbound.interface != *ctx.owner_interface()
        || topology.readiness.binding.outbound.iter().map(|binding| &binding.interface).collect::<Vec<_>>() != [ctx.owner_interface()]
        || !owned.contains(&json!(checkpoint.id)) || !record.active_checkpoints.contains(&checkpoint.id)
        || !original_ownership.contains(&json!(checkpoint.id)) || !current_ownership.contains(&json!(checkpoint.id))
        || record.checkpoint(&checkpoint.id) != Some(*checkpoint)
    { return Err("output authority initial readiness edge changed".into()); }
    let payload = json!({"schema_version":1,"context_generation":context.generation,
        "worker_environment_id":context.identity.formation.worker_environment_id,
        "source_movement_seq":original.source_movement_seq,"source_artifact":original.artifact});
    prepare_record(&root.join("output-authority.json"), &payload)?;
    let artifact = octopus_interfaces::InterfaceArtifactFile::write(root.join("output-authority"), "code_mode.owner_ready",
        &serde_json::to_vec(&payload).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let reference = artifact_ref(&artifact)?;
    let mut history = octopus_run_authoring::FileCheckedRunReader::open(&evidence, None)?;
    let mut selected = None;
    let mut arrival = false;
    let mut refusal = None;
    let complete = visit_complete_ledger(ctx, |movement| {
        if movement.seq.0 <= original.source_movement_seq || movement.artifact.as_ref() != Some(&reference) { return Ok(()); }
        if movement.from != Endpoint::Interface(ctx.owner_interface().clone()) { return Err("output authority carrier has a foreign source".into()); }
        if movement.kind == octopus_ledger::MovementKind::DeliveryRefused {
            let Some(octopus_ledger::MovementMetadata::Delivery(delivery)) = movement.invocation_metadata.as_ref() else { return Err("output authority refusal has no verdict".into()); };
            refusal = Some(delivery.delivery_verdict.clone());
            return Ok(());
        }
        if movement.kind == octopus_ledger::MovementKind::InterfaceArrival { arrival = true; return Ok(()); }
        if movement.kind != octopus_ledger::MovementKind::InterfaceOutbound { return Ok(()); }
        if movement.from != Endpoint::Interface(ctx.owner_interface().clone()) || movement.to != movement.from { return Err("output authority carrier changed its owner edge".into()); }
        let metadata = movement.invocation_metadata.as_ref().and_then(|value| value.invocation()).ok_or("output authority carrier has no admission")?;
        let runtime = metadata.runtime().ok_or("output authority carrier has no coordinate")?;
        if runtime.run != record.id || runtime.ee != record.ee || runtime.worker != record.worker
            || runtime.interface != *ctx.owner_interface() || runtime.checkpoint != checkpoint.id || runtime.epoch != checkpoint.epoch.0
            || metadata.verification_failure.is_some() || metadata.checkpoint_binding_policy.as_ref() != Some(&octopus_interfaces::CheckpointBindingPolicySnapshot {
                inbound: topology.readiness.binding.inbound.clone(), outbound: topology.readiness.binding.outbound.clone(),
            })
            || !metadata.checkpoint_admission.as_ref().is_some_and(|admission| admission.complete && admission.artifact_label == "code_mode.owner_ready")
        { return Err("output authority carrier differs from its exact readiness checkpoint".into()); }
        let pinned = history.snapshot(&ContentHash::from_hex(runtime.run_record_hash.as_deref().ok_or("output authority carrier has no snapshot")?).ok_or("output authority carrier snapshot is invalid")?)?;
        octopus_run_authoring::validate_unchanged_admitted_checkpoint(&pinned, &current, &runtime.checkpoint)?;
        if selected.replace(formed_source(movement.clone())?).is_some() { return Err("output authority has duplicate admissions".into()); }
        Ok(())
    })?;
    if !complete { return Ok(OutputAuthority::Pending); }
    if let Some(source) = selected { return Ok(OutputAuthority::Admitted(source)); }
    if let Some(verdict) = refusal { return Err(format!("output authority admission was refused: {verdict:?}").into()); }
    if arrival { return Ok(OutputAuthority::Pending); }
    if root.join("output-authority-dispatch.json").try_exists().map_err(|e| e.to_string())? {
        return Err("output authority dispatch has no exact verdict; retained without retry".into());
    }
    Ok(OutputAuthority::Initial(artifact))
}

fn visit_complete_ledger(
    ctx: &octopus_tool_door_runtime::InterfaceLiftContext,
    mut visit: impl FnMut(&Movement) -> Result<(), String>,
) -> Result<bool, String> {
    use std::io::BufRead;
    let mut options = OpenOptions::new(); options.read(true);
    #[cfg(unix)] {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let ledger = options.open(ctx.owner_movements_path()).map_err(|e| e.to_string())?;
    let before = ledger.metadata().map_err(|e| e.to_string())?;
    if !before.is_file() { return Err("output authority ledger is not a regular file".into()); }
    let length = before.len();
    let mut reader = std::io::BufReader::new(ledger.take(length));
    let decoder = octopus_ledger_store::DurableMovementDecoder::for_log(ctx.owner_movements_path());
    let maximum = ctx.owner_io_limits()?.max_control_frame_bytes;
    let mut line = Vec::new(); let mut sequence = 0; let mut consumed = 0;
    loop {
        line.clear();
        let count = Read::by_ref(&mut reader).take(maximum.saturating_add(1)).read_until(b'\n', &mut line).map_err(|e| e.to_string())?;
        if count == 0 { break; }
        if count as u64 > maximum { return Err("output authority ledger row exceeds its bound".into()); }
        let Some(bytes) = line.strip_suffix(b"\n") else { return Ok(false); };
        let row = decoder.decode(bytes, sequence).map_err(|e| e.to_string())?;
        if row.seq.0 != sequence as u64 { return Err("output authority ledger sequence changed".into()); }
        visit(&row)?; sequence += 1; consumed += count as u64;
    }
    let after = reader.get_ref().get_ref().metadata().map_err(|e| e.to_string())?;
    Ok(consumed == length && after.len() == length && before.modified().ok() == after.modified().ok())
}

pub fn reconcile_checked_provider_advance(
    ctx: &octopus_tool_door_runtime::InterfaceLiftContext,
    root: &Path,
    config: &CodeModeConfig,
    input: Option<&InterfaceInvocationEnvelope>,
) -> Result<Option<InstalledHandoff>, octopus_run_authoring::CheckedRunError> {
    if !root.join("context.json").try_exists().map_err(|e| e.to_string())? { return Ok(None); }
    let context: ContextRecord = read_record(&root.join("context.json"))?;
    if context.identity.config != *config || digest(&context.identity)? != context.generation || context.identity.formation.door_interface != *ctx.owner_interface() {
        return Err("shared run recovery changed its exact owner context".into());
    }
    let Some(topology) = &context.identity.owner_topology else { return Ok(None); };
    let Some(writer) = &topology.coauthor else { return Ok(None); };
    if root.join("retired.json").try_exists().map_err(|e| e.to_string())? { return Err("shared run context is retired".into()); }
    if let Some(input) = input {
        if config.resolve_owner_topology(input)? != *topology { return Err("shared run input changed its immutable owner topology".into()); }
        validate_admitted_coauthor(ctx, input, topology)?;
        let ResolvedContext::Persistent { artifact,payload,.. } = config.resolve_context(input)? else { return Err("shared run input lost its prelude".into()); };
        let runtime = input.runtime().map_err(|e| e.to_string())?;
        if artifact != context.identity.prelude_artifact || DescriptorHash::of_bytes(&payload).to_hex() != context.identity.prelude_payload_hash
            || runtime.run.as_str() != context.identity.formation.run || runtime.ee.as_str() != context.identity.formation.ee || runtime.worker != context.identity.formation.worker || runtime.interface != *ctx.owner_interface() {
            return Err("shared run input changed its run, formation or prelude".into());
        }
    }
    let limits = ctx.owner_io_limits()?;
    let load = |hash: &str| -> Result<Vec<u8>,String> {
        let parsed = ContentHash::from_hex(hash).ok_or("shared run has no canonical snapshot hash")?;
        if parsed.to_hex() != hash { return Err("shared run snapshot hash is not canonical".into()); }
        let bytes = octopus_interfaces::read_artifact_carrier_file_bounded(&ctx.owner_blobs_dir().join(hash),limits.max_payload_bytes).map_err(|e| e.to_string())?;
        if ContentHash::of_bytes(&bytes) != parsed { return Err("shared run snapshot integrity mismatch".into()); }
        Ok(bytes)
    };
    let mut latest: Option<InstalledHandoff> = None;
    for entry in std::fs::read_dir(root).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        if entry.file_name().to_str().is_some_and(|name| name.ends_with(".installed.json")) {
            let record: InstalledHandoff = read_record(&entry.path())?;
            if latest.as_ref().is_some_and(|old| old.result_seq == record.result_seq && old != &record) { return Err("shared run has ambiguous installed authoring evidence".into()); }
            if latest.as_ref().is_none_or(|old| old.result_seq < record.result_seq) { latest = Some(record); }
        }
    }
    let run_root = ctx.owner_root_slot("run_records_root")?.canonicalize().map_err(|e| e.to_string())?;
    let path = run_root.join(format!("{}.json",topology.run)).canonicalize().map_err(|e| e.to_string())?;
    if path.parent() != Some(run_root.as_path()) { return Err("shared run file escapes its granted root".into()); }
    let evidence = octopus_run_authoring::CheckedRunEvidence { movements_path:ctx.owner_movements_path(),ledger_index_path:ctx.owner_ledger_index_path(),blobs_dir:ctx.owner_blobs_dir(),max_control_frame_bytes:limits.max_control_frame_bytes,max_manifest_bytes:limits.max_manifest_bytes,max_payload_bytes:limits.max_payload_bytes };
    let original = load(&context.identity.first_run.run_record_hash)?;
    let (before, after_seq) = match &latest {
        Some(installed) => {
            use octopus_run_authoring::{CheckedRunInstall, ConsentEvidenceRef};
            let transition = octopus_run_authoring::checked_declared_run_install(
                &evidence, &path, &original,
                &CheckedRunInstall {
                    call: ConsentEvidenceRef { movement_seq: installed.source_seq, artifact: installed.source_artifact.clone() },
                    result: ConsentEvidenceRef { movement_seq: installed.result_seq, artifact: installed.result_artifact.clone() },
                }, vec![writer.clone()],
            )?;
            if ContentHash::of_bytes(&transition.before).to_hex() != installed.before_hash
                || ContentHash::of_bytes(&transition.after).to_hex() != installed.after_hash {
                return Err("installed shared run differs from its exact checked write".into());
            }
            (transition.after, installed.source_seq)
        }
        None => (original, context.identity.first_run.source_movement_seq),
    };
    let input_checkpoint = input.map(|input| -> Result<_, String> {
        let manifest = input.read_artifact_manifest().map_err(|e| e.to_string())?;
        if manifest.label == "tool.call" { return Ok(None); }
        let runtime = input.runtime().map_err(|e| e.to_string())?;
        let pinned = load(runtime.run_record_hash.as_deref().ok_or("shared run input has no pinned snapshot")?)?;
        Ok(Some((pinned, runtime.checkpoint.clone())))
    }).transpose()?.flatten();
    let cancellation = ctx.owner_cancel();
    let transitions = checked_recovery_observation(
        || octopus_interfaces::read_artifact_carrier_file_bounded(&path,limits.max_payload_bytes).map_err(|e| e.to_string()),
        |current| {
            let transitions = octopus_run_authoring::checked_declared_run_advance(&evidence,&path,&before,current,after_seq,vec![writer.clone()])?;
            if let Some((pinned, checkpoint)) = &input_checkpoint {
                octopus_run_authoring::validate_unchanged_admitted_checkpoint(pinned,current,checkpoint)?;
            }
            Ok(transitions)
        },
        || cancellation.is_cancelled(),
    )?;
    if transitions.is_empty() {
        return Ok(latest);
    }
    let pending = prepared_calls(root)?;
    for transition in transitions {
        let before_hash = ContentHash::of_bytes(&transition.before).to_hex();
        let after_hash = ContentHash::of_bytes(&transition.after).to_hex();
        for call in &pending {
            let old_path = root.join(format!("{}.{}.return.json",call.digest,before_hash));
            if old_path.try_exists().map_err(|e| e.to_string())? {
                let mut boundary: ReturnBoundary = read_record(&old_path)?;
                if boundary.survives_checked_transition(&transition.before, &transition.after)? {
                    boundary.run_record_hash = after_hash.clone();
                    prepare_return(root,call,&boundary)?;
                }
            }
        }
        let old_wake = root.join(format!("{before_hash}.wake-pin.json"));
        if old_wake.try_exists().map_err(|e| e.to_string())? {
            let mut boundary: ReturnBoundary = read_record(&old_wake)?;
            if boundary.survives_checked_transition(&transition.before, &transition.after)? {
                boundary.run_record_hash = after_hash.clone();
                prepare_record(&root.join(format!("{after_hash}.wake-pin.json")),&boundary)?;
            }
        }
        for application in applications(root)? {
            let old_path = root.join(format!("{}.{}.application-projected.json",application.pending_digest,before_hash));
            if old_path.try_exists().map_err(|e| e.to_string())? {
                let mut projection: ApplicationProjection = read_record(&old_path)?;
                if projection.boundary.survives_checked_transition(&transition.before, &transition.after)? {
                    projection.boundary.run_record_hash = after_hash.clone();
                    record_application_projection(root,&projection)?;
                }
            }
        }
        let result_artifact = transition.result.artifact.clone().ok_or("checked provider result has no artifact")?;
        let installed = InstalledHandoff { before_hash,after_hash,source_seq:transition.call.seq.0,source_artifact:transition.call.artifact.clone().ok_or("checked installation has no actual call artifact")?,result_seq:transition.result.seq.0,result_artifact };
        record_install(root,&installed)?;
        latest = Some(installed);
    }
    Ok(latest)
}

pub fn retained_continuation_formation(
    ctx: &octopus_tool_door_runtime::InterfaceLiftContext,
    config: &CodeModeConfig,
    input: &InterfaceInvocationEnvelope,
    source: &Movement,
) -> Result<WorkerEnvironmentFormation, String> {
    let (_, root) = historical_continuation_call(ctx, input, source)?;
    let context: ContextRecord = read_record(&root.join("context.json"))?;
    let label = input.read_artifact_manifest().map_err(|e| e.to_string())?.label;
    let witness_path = if label == "code_mode.frontier_ready" {
        root.join(format!("{}.wake-admitted.json", read_ready(input)?.identity()?))
    } else {
        let (pending, suffix) = if let Some((_, key)) = application_locator(input)? {
            let pending: PreparedCall = read_record(&root.join(format!("{key}.pending.json")))?;
            pending.validate()?;
            let suffix = if label == "operator.approval.response" { "decision-admitted" } else { "app-result-admitted" };
            (pending, suffix)
        } else {
            (read_pending_result(&root, input, ResultCarrier::OriginalAdmitted)?.0, "admitted")
        };
        if pending.identity.context != context {
            return Err("retained continuation changed its immutable pending context".into());
        }
        root.join(format!("{}.{suffix}.json", pending.digest))
    };
    let Endpoint::Interface(source_interface) = &source.from;
    let witness: AdmittedCarrier = read_record(&witness_path)?;
    if witness != result_witness(input, source_interface.clone(), source.seq.0)? {
        return Err("retained continuation differs from its exact admitted carrier".into());
    }
    let formation = &context.identity.formation;
    formation.validate().map_err(|e| e.to_string())?;
    let runtime = input.runtime().map_err(|e| e.to_string())?;
    let topology = config.resolve_owner_topology(input)?;
    validate_admitted_coauthor(ctx, input, &topology)?;
    let ResolvedContext::Persistent { artifact, payload, .. } = config.resolve_context(input)? else {
        return Err("retained continuation has no admitted persistent context".into());
    };
    if context.generation != digest(&context.identity)?
        || context.identity.config != *config
        || context.identity.owner_topology.as_ref() != Some(&topology)
        || context.identity.prelude_artifact != artifact
        || context.identity.prelude_payload_hash != DescriptorHash::of_bytes(&payload).to_hex()
        || formation.door_interface != runtime.interface || formation.run != runtime.run.as_str()
        || formation.ee != runtime.ee.as_str() || formation.worker != runtime.worker
        || octopus_tool_door_runtime::tool_door_durable_state_dir(ctx, &formation.worker_environment_id)? != root {
        return Err("retained continuation differs from its immutable context and formation".into());
    }
    let mut selected = None;
    for snapshot in octopus_worker_env::WorkerEnvironmentRecordStore::new(ctx.interface_var_dir())
        .snapshots().map_err(|e| e.to_string())? {
        let record = snapshot.record;
        if record.formation.worker_environment_id != formation.worker_environment_id { continue; }
        if record.formation != *formation || source.seq.0 < record.opened_at_movement_seq {
            return Err("retained continuation differs from its recorded formation".into());
        }
        if record.lifecycle == octopus_worker_env::WorkerEnvironmentLifecycle::Released
            || root.join("retired.json").try_exists().map_err(|e| e.to_string())? {
            return Err("retained continuation context was released".into());
        }
        if selected.replace(record.formation).is_some() {
            return Err("retained continuation has ambiguous formed ownership".into());
        }
    }
    selected.ok_or_else(|| "retained continuation has no exact recorded formation".into())
}

pub fn retained_output_formation(
    ctx: &octopus_tool_door_runtime::InterfaceLiftContext,
    config: &CodeModeConfig,
    input: &InterfaceInvocationEnvelope,
    source_seq: u64,
) -> Result<WorkerEnvironmentFormation, String> {
    let runtime = input.runtime().map_err(|e| e.to_string())?;
    let topology = config.resolve_owner_topology(input)?;
    validate_admitted_coauthor(ctx, input, &topology)?;
    let ResolvedContext::Persistent { artifact, payload, .. } = config.resolve_context(input)? else {
        return Err("retained output has no admitted persistent context".into());
    };
    let mut selected = None;
    for snapshot in octopus_worker_env::WorkerEnvironmentRecordStore::new(ctx.interface_var_dir())
        .snapshots().map_err(|e| e.to_string())? {
        let record = snapshot.record;
        let formation = record.formation;
        if formation.door_interface != runtime.interface || formation.run != runtime.run.as_str()
            || formation.ee != runtime.ee.as_str() || formation.worker != runtime.worker
            || source_seq < record.opened_at_movement_seq || source_seq > record.last_activity_movement_seq {
            continue;
        }
        let root = octopus_tool_door_runtime::tool_door_durable_state_dir(ctx, &formation.worker_environment_id)?;
        if !root.join("context.json").try_exists().map_err(|e| e.to_string())? { continue; }
        let context: ContextRecord = read_record(&root.join("context.json"))?;
        if context.identity.formation != formation || context.generation != digest(&context.identity)? {
            return Err("retained output formation differs from its immutable context".into());
        }
        if context.identity.config != *config || context.identity.owner_topology.as_ref() != Some(&topology)
            || context.identity.prelude_artifact != artifact
            || context.identity.prelude_payload_hash != DescriptorHash::of_bytes(&payload).to_hex() {
            continue;
        }
        if record.lifecycle == octopus_worker_env::WorkerEnvironmentLifecycle::Released
            || root.join("retired.json").try_exists().map_err(|e| e.to_string())? {
            return Err("retained output context was released".into());
        }
        if selected.replace(formation).is_some() { return Err("retained output has ambiguous formed ownership".into()); }
    }
    selected.ok_or_else(|| "retained output has no exact recorded formation".into())
}
